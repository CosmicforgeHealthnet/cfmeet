"""Final production completion: email delivery (Resend) and meeting lifecycle operations.

Imported by coordinated_release.py (never run on its own). Every operation prints JSON
lines through `emit` and never prints a secret, an address, an email body or a code.

* `diagnose` (read-only): the Meeting Service's mail configuration (non-secret values
  and presence only), SMTP failure evidence in its logs (counts and timestamps of known
  failure signatures, never the lines), the legacy Redis email queue length, the email
  ledger state, and the conference lifecycle state (LIVE instant conferences without
  anyone, LIVE conferences of archived spaces, the reported meeting codes).
* `activate-resend-email`: switches the Meeting Service's email provider to Resend
  (`EMAIL_PROVIDER=resend`, API key mounted read-only from the VPS secret store, sender
  `CF Meet <no-reply@cfmeet.com>`), then proves delivery: a controlled acceptance email
  must be ACCEPTED by Resend (and DELIVERED when the signed webhook is configured). Any
  failure restores the previous container (SMTP stays the transport), so Gmail is only
  retired once Resend delivery is verified.
* `email-acceptance`: the same proof, repeatable, without changing anything else; with
  `confirm=otp` it also runs the public OTP journey (register → Resend → verification code
  → verify → sign in → deactivate the acceptance account).
"""

import json
import re
import subprocess
import time
import urllib.error
import urllib.request
import uuid

import production as p

MEETING_LOOPBACK = 'http://127.0.0.1:18081'
SECRET_KEY_FILE = 'resend-api-key'
SECRET_WEBHOOK_FILE = 'resend-webhook-secret'
KEY_TARGET = '/run/secrets/resend-api-key'
WEBHOOK_TARGET = '/run/secrets/resend-webhook-secret'
SENDER_ADDRESS = 'no-reply@cfmeet.com'
SENDER_NAME = 'CF Meet'
# Resend's test inbox: simulates a successful delivery (with `+label` support).
DEFAULT_ACCEPTANCE_RECIPIENT = 'delivered@resend.dev'
REPORTED_MEETING_CODES = ('swy-hayb-mzk',)
ADDRESS = re.compile(r'^[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$')
FAILURE_SIGNATURES = {
    'smtp535': re.compile(r'\b535\b'),
    'badCredentials': re.compile(r'BadCredentials|Username and Password not accepted'),
    'smtpError': re.compile(r'SMTP error'),
    'jobFailed': re.compile(r'Email job \S+ failed'),
    'droppedAfterRetries': re.compile(r'exceeded max retries'),
    'smtpSent': re.compile(r'Email sent: '),
    'providerAccepted': re.compile(r'email accepted by provider'),
    'providerTemporary': re.compile(r'email delivery failed temporarily'),
    'providerRejected': re.compile(r'email rejected permanently|email not delivered'),
}
TIMESTAMP = re.compile(r'^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)')


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True, default=str), flush=True)


def domain_of(address):
    return address.rsplit('@', 1)[-1].lower() if address and '@' in address else None


def mail_summary(env):
    """Non-secret mail configuration of the Meeting Service (presence for secrets)."""
    return {
        'emailProvider': env.get('EMAIL_PROVIDER') or ('smtp (implicit)' if env.get('SMTP_HOST') else 'none'),
        'smtpHost': env.get('SMTP_HOST') or None,
        'smtpPort': env.get('SMTP_PORT') or None,
        'smtpUsernameDomain': domain_of(env.get('SMTP_USERNAME')),
        'smtpPasswordConfigured': bool(env.get('SMTP_PASSWORD') or env.get('SMTP_PASSWORD_FILE')),
        'smtpFromDomain': domain_of(env.get('SMTP_FROM_EMAIL')),
        'emailFromAddress': env.get('EMAIL_FROM_ADDRESS') or None,
        'resendKeyMounted': env.get('RESEND_API_KEY_FILE') == KEY_TARGET,
        'resendWebhookConfigured': bool(env.get('RESEND_WEBHOOK_SECRET_FILE')),
        'notificationsEmailEnabled': env.get('NOTIFICATIONS_EMAIL_ENABLED') == 'true',
        'emailVerificationRequired': env.get('EMAIL_VERIFICATION_REQUIRED') or None,
        'emailVerificationEnforcement': env.get('EMAIL_VERIFICATION_ENFORCEMENT') or None,
    }


def scan(text, hours):
    """Counts (and first/last timestamps) of mail failure signatures in log text."""
    out = {}
    lines = text.splitlines()
    for name, pattern in FAILURE_SIGNATURES.items():
        stamps = []
        count = 0
        for line in lines:
            if pattern.search(line):
                count += 1
                m = TIMESTAMP.match(line)
                if m:
                    stamps.append(m.group(1))
        out[name] = {'count': count, 'first': stamps[0] if stamps else None,
                     'last': stamps[-1] if stamps else None}
    out['windowHours'] = hours
    return out


def log_evidence(container, hours=168):
    """Lines never leave the VPS: only counts and timestamps are reported."""
    proc = subprocess.run(['docker', 'logs', '--timestamps', '--since', '%dh' % hours, container],
                          capture_output=True, text=True)
    return scan((proc.stdout or '') + '\n' + (proc.stderr or ''), hours)


def redis_command(cr, env, command, extra=None):
    url = env.get('REDIS_URL')
    if not url:
        return None
    path = cr.WORK / 'run' / 'meeting-redis.env'
    values = 'REDIS_URL=%s\n' % url + ''.join('%s=%s\n' % kv for kv in (extra or {}).items())
    p.write_private(path, values)
    try:
        return p.run('docker', 'run', '--rm', '--network', 'container:' + cr.MEETING, '--env-file', str(path),
                     'redis:7.4-alpine', 'sh', '-c', 'exec redis-cli -u "$REDIS_URL" --no-auth-warning ' + command)
    except RuntimeError:
        return None
    finally:
        path.unlink(missing_ok=True)


def redis_queue_length(cr, env):
    out = redis_command(cr, env, 'LLEN email:queue')
    try:
        return int(out.strip().splitlines()[-1]) if out and out.strip() else None
    except ValueError:
        return None


LIFECYCLE_SQL = """
SELECT json_build_object(
  'liveByKind', (SELECT coalesce(json_object_agg(kind, n), '{}') FROM
      (SELECT kind, count(*) n FROM conference_sessions WHERE status='LIVE' AND origin='V2' GROUP BY kind) k),
  'endingConferences', (SELECT count(*) FROM conference_sessions WHERE status='ENDING'),
  'liveInstantWithoutActiveSessionOver10Min', (SELECT count(*) FROM conference_sessions c
      WHERE c.status='LIVE' AND c.origin='V2' AND c.kind='INSTANT'
        AND COALESCE(c.started_at, c.created_at) < now() - interval '10 minutes'
        AND NOT EXISTS (SELECT 1 FROM participant_sessions ps WHERE ps.conference_id=c.id
                        AND ps.state IN ('PENDING','CONNECTED') AND (ps.expires_at IS NULL OR ps.expires_at > now()))),
  'liveInArchivedSpaces', (SELECT count(*) FROM conference_sessions c JOIN meeting_spaces s ON s.id=c.meeting_space_id
      WHERE c.status='LIVE' AND s.status='ARCHIVED'),
  'emptyTimeoutEnded24h', (SELECT count(*) FROM conference_sessions
      WHERE termination_reason='EMPTY_TIMEOUT' AND ending_requested_at > now() - interval '24 hours'),
  'reported', (SELECT coalesce(json_agg(json_build_object(
        'meetingCode', s.meeting_code, 'spaceStatus', s.status, 'archivedAt', s.archived_at,
        'conferenceStatus', c.status, 'kind', c.kind, 'createdAt', c.created_at, 'startedAt', c.started_at,
        'endingRequestedAt', c.ending_requested_at, 'endedAt', c.ended_at,
        'terminationReason', c.termination_reason,
        'sessionsTotal', (SELECT count(*) FROM participant_sessions ps WHERE ps.conference_id=c.id),
        'sessionsActive', (SELECT count(*) FROM participant_sessions ps WHERE ps.conference_id=c.id
                           AND ps.state IN ('PENDING','CONNECTED'))) ORDER BY c.created_at), '[]')
      FROM meeting_spaces s JOIN conference_sessions c ON c.meeting_space_id=s.id
      WHERE s.meeting_code = ANY(%s))
);
"""

LEDGER_SQL = """
SELECT CASE WHEN to_regclass('public.email_deliveries') IS NULL THEN 'null'::json ELSE
  (SELECT json_build_object(
     'byStatus', (SELECT coalesce(json_object_agg(status, n), '{}') FROM
        (SELECT status, count(*) n FROM email_deliveries GROUP BY status) s),
     'byCategoryLast24h', (SELECT coalesce(json_object_agg(category || ':' || status, n), '{}') FROM
        (SELECT category, status, count(*) n FROM email_deliveries
          WHERE created_at > now() - interval '24 hours' GROUP BY category, status) c),
     'lastErrorClasses24h', (SELECT coalesce(json_object_agg(last_error_class, n), '{}') FROM
        (SELECT last_error_class, count(*) n FROM email_deliveries
          WHERE last_error_class IS NOT NULL AND updated_at > now() - interval '24 hours'
          GROUP BY last_error_class) e),
     'providerEvents24h', (SELECT coalesce(json_object_agg(event_type, n), '{}') FROM
        (SELECT event_type, count(*) n FROM email_delivery_events
          WHERE received_at > now() - interval '24 hours' GROUP BY event_type) v)))
END;
"""


def sql_array(values):
    return "ARRAY[%s]::text[]" % ','.join("'%s'" % v for v in values if re.fullmatch(r'[a-z0-9-]{3,40}', v))


def diagnose(cr, sha, run_id):
    meeting = cr.MEETING
    env = p.env_of(p.inspect(meeting))
    flags = {k: env.get(k) for k in (
        'FEATURE_MEETING_V2_ENABLED', 'FEATURE_RTC_PROVIDER_CONTROL_V2', 'FEATURE_CONFERENCE_REALTIME_ENABLED',
        'FEATURE_RECORDING_V2_ENABLED', 'FEATURE_TRANSCRIPTION_V2_ENABLED', 'FEATURE_INTELLIGENCE_V2_ENABLED',
        'FEATURE_HEALTHCARE_PROFILE_ENABLED', 'CONFERENCE_EMPTY_TIMEOUT_SECS', 'APP_URL')}
    emit('meetingFlags', flags)
    emit('meetingMail', mail_summary(env))
    emit('meetingMailLogEvidence', log_evidence(meeting))
    emit('legacyRedisEmailQueueLength', redis_queue_length(cr, env))
    emit('emailLedger', json.loads(cr.meeting_psql(LEDGER_SQL) or 'null'))
    emit('conferenceLifecycle', json.loads(cr.meeting_psql(LIFECYCLE_SQL % sql_array(REPORTED_MEETING_CODES))))
    emit('meetingHistories', cr.history_state(cr.meeting_psql))


# --- Resend activation and acceptance --------------------------------------------------------

def internal(env, method, path, body=None):
    token = env.get('INTERNAL_API_TOKEN')
    if not token:
        raise RuntimeError('The Meeting Service has no INTERNAL_API_TOKEN; internal operations are unavailable')
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(MEETING_LOOPBACK + path, data=data, method=method,
                                     headers={'X-Internal-Token': token, 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, json.loads(response.read() or b'null')
    except urllib.error.HTTPError as error:
        return error.code, None


def acceptance_recipient(cr):
    path = cr.WORK / 'email-acceptance-recipient'
    value = path.read_text().strip() if path.exists() else ''
    if value and not ADDRESS.fullmatch(value):
        raise RuntimeError('The configured acceptance recipient is not a valid address')
    return value or DEFAULT_ACCEPTANCE_RECIPIENT


def labelled(address, label):
    local, domain = address.split('@', 1)
    return '%s+%s@%s' % (local.split('+', 1)[0], label, domain)


def wait_for(env, delivery_id, wanted, seconds, interval=3):
    deadline = time.time() + seconds
    last = None
    while time.time() < deadline:
        status, body = internal(env, 'GET', '/internal/operations/email/deliveries/%s' % delivery_id)
        if status == 200 and body:
            last = {k: body.get(k) for k in ('status', 'provider', 'attempts', 'lastErrorClass', 'lastErrorStatus',
                                             'acceptedAt', 'deliveredAt')}
            if last['status'] in wanted or last['status'] in ('FAILED', 'EXPIRED', 'BOUNCED', 'COMPLAINED'):
                return last
        time.sleep(interval)
    return last


def email_acceptance_check(cr, run_id, require_delivered):
    env = p.env_of(p.inspect(cr.MEETING))
    recipient = acceptance_recipient(cr)
    reference = 'acc-' + re.sub(r'[^0-9a-z-]', '-', run_id.lower())[:40]
    status, body = internal(env, 'POST', '/internal/operations/email/acceptance',
                            {'to': recipient, 'reference': reference})
    if status != 202 or not body or not body.get('deliveryId'):
        raise RuntimeError('The acceptance email could not be queued (HTTP %s)' % status)
    accepted = wait_for(env, body['deliveryId'], ('ACCEPTED', 'DELIVERED'), 90)
    result = {'reference': reference, 'recipientDomain': domain_of(recipient), 'deliveryId': body['deliveryId'],
              'accepted': accepted}
    if not accepted or accepted['status'] not in ('ACCEPTED', 'DELIVERED') or accepted['provider'] != 'resend':
        emit('emailAcceptance', result)
        raise RuntimeError('Resend did not accept the acceptance email: %s' % json.dumps(accepted))
    if require_delivered:
        delivered = wait_for(env, body['deliveryId'], ('DELIVERED',), 180)
        result['delivered'] = delivered
        if not delivered or delivered['status'] != 'DELIVERED':
            emit('emailAcceptance', result)
            raise RuntimeError('No signed delivery event arrived for the acceptance email: %s' % json.dumps(delivered))
    emit('emailAcceptance', result)
    return result


def resend_changes(cr):
    key = p.SECRETS / SECRET_KEY_FILE
    if not key.exists() or not key.read_text().strip().startswith('re_'):
        raise RuntimeError('No Resend API key on the VPS: set the CFMEET_RESEND_API_KEY secret of the production '
                           'environment and dispatch again')
    for path in (p.SECRETS / SECRET_KEY_FILE, p.SECRETS / SECRET_WEBHOOK_FILE):
        if path.exists():
            path.chmod(0o400)
    overrides = {'EMAIL_PROVIDER': 'resend', 'RESEND_API_KEY_FILE': KEY_TARGET,
                 'EMAIL_FROM_ADDRESS': SENDER_ADDRESS, 'EMAIL_FROM_NAME': SENDER_NAME}
    mounts = [(key, KEY_TARGET)]
    webhook = p.SECRETS / SECRET_WEBHOOK_FILE
    has_webhook = webhook.exists() and webhook.read_text().strip().startswith('whsec_')
    if has_webhook:
        overrides['RESEND_WEBHOOK_SECRET_FILE'] = WEBHOOK_TARGET
        mounts.append((webhook, WEBHOOK_TARGET))
    return overrides, mounts, has_webhook


def require_ledger(cr):
    if cr.history_state(cr.meeting_psql).get('lifecycle_delivery_migrations', {}).get('applied', 0) < 1:
        raise RuntimeError('The running Meeting Service predates the durable email ledger; release first')


def activate_resend_email(cr, sha, run_id, confirm):
    require_ledger(cr)
    overrides, mounts, has_webhook = resend_changes(cr)
    emit('resendActivation', {'webhookSigningConfigured': has_webhook,
                              'sender': SENDER_NAME + ' <' + SENDER_ADDRESS + '>'})
    cr.apply_activation('activate-resend-email', sha, run_id,
                        [('meeting-service', overrides, {'mounts': mounts})],
                        post=lambda: email_acceptance_check(cr, run_id, has_webhook))


def public(method, path, body=None, token=None):
    headers = {'Content-Type': 'application/json', 'Origin': 'https://cfmeet.com'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request('https://api.cfmeet.com' + path, method=method, headers=headers,
                                     data=json.dumps(body).encode() if body is not None else None)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.loads(response.read() or b'null')
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b'null')
        except ValueError:
            return error.code, None


def otp_from_redis(cr, env, address):
    """The current verification code of the acceptance account (read on the VPS only)."""
    out = redis_command(cr, env, 'GET "$KEY"', {'KEY': 'otp:verify:' + address.lower()})
    code = out.strip().splitlines()[-1] if out and out.strip() else ''
    return code if re.fullmatch(r'\d{6}', code) else None


def otp_journey(cr, run_id):
    """Register → Resend → verify → sign in → deactivate, through the public API."""
    env = p.env_of(p.inspect(cr.MEETING))
    label = 'cfmeetotp' + uuid.uuid4().hex[:10]
    address = labelled(acceptance_recipient(cr), label)
    password = 'Acc!' + uuid.uuid4().hex + 'Z9'
    checks = {}
    status, _ = public('POST', '/api/v1/auth/register', {
        'username': label, 'email': address, 'password': password, 'confirm_password': password})
    checks['register'] = 'PASS' if status == 201 else 'FAIL %s' % status
    if status != 201:
        return checks
    time.sleep(2)
    rows = cr.meeting_psql(
        "SELECT id || '|' || status FROM email_deliveries WHERE category='AUTH_VERIFICATION' "
        "AND recipient_hash = encode(sha256(convert_to(lower('%s'),'UTF8')),'hex') "
        "ORDER BY created_at DESC LIMIT 1;" % address.replace("'", ''))
    delivery_id = rows.split('|')[0] if rows else None
    checks['verification_email_recorded'] = 'PASS' if delivery_id else 'FAIL'
    if delivery_id:
        accepted = wait_for(env, delivery_id, ('ACCEPTED', 'DELIVERED'), 90)
        checks['verification_email_accepted_by_resend'] = (
            'PASS' if accepted and accepted['status'] in ('ACCEPTED', 'DELIVERED') and accepted['provider'] == 'resend'
            else 'FAIL %s' % json.dumps(accepted))
    code = otp_from_redis(cr, env, address)
    checks['code_issued'] = 'PASS' if code else 'FAIL'
    wrong = '000000' if code != '000000' else '111111'
    status, _ = public('POST', '/api/v1/auth/verify-email', {'email': address, 'token': wrong})
    checks['invalid_code_refused'] = 'PASS' if status == 400 else 'FAIL %s' % status
    status, _ = public('POST', '/api/v1/auth/verify-email', {'email': address, 'token': code or '999999'})
    checks['valid_code_verifies'] = 'PASS' if status == 200 else 'FAIL %s' % status
    status, _ = public('POST', '/api/v1/auth/verify-email', {'email': address, 'token': code or '999999'})
    checks['used_code_refused'] = 'PASS' if status == 400 else 'FAIL %s' % status
    status, body = public('POST', '/api/v1/auth/login', {'email': address, 'password': password})
    token = (body or {}).get('access_token')
    checks['sign_in_after_verification'] = 'PASS' if status == 200 and token else 'FAIL %s' % status
    if token:
        status, _ = public('GET', '/api/v2/me/profile', token=token)
        checks['authenticated_session'] = 'PASS' if status == 200 else 'FAIL %s' % status
        status, _ = public('POST', '/api/v1/users/me/deactivate', {'current_password': password}, token=token)
        checks['cleanup_deactivated'] = 'PASS' if status in (200, 204) else 'MANUAL %s' % status
    return checks


def email_acceptance(cr, sha, run_id, confirm):
    require_ledger(cr)
    env = p.env_of(p.inspect(cr.MEETING))
    if env.get('EMAIL_PROVIDER') != 'resend':
        raise RuntimeError('Resend is not the active provider (run activate-resend-email)')
    email_acceptance_check(cr, run_id, bool(env.get('RESEND_WEBHOOK_SECRET_FILE')))
    if 'otp' in (confirm or '').split(','):
        checks = otp_journey(cr, run_id)
        emit('otpJourney', checks)
        if any(not v.startswith(('PASS', 'MANUAL')) for v in checks.values()):
            raise RuntimeError('OTP journey failed')
