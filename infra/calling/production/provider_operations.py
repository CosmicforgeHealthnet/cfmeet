"""Credential-gated integrations; no credential value is emitted or journaled."""
import base64
import json
import os
import re
import stat
import urllib.request

import production as p
import notification_operations as notifications

PHONE_OPERATION = 'activate-phone-accounts'
NATIVE_OPERATION = 'activate-native-push'
PHONE_TOKEN_TARGET = '/run/secrets/phone-verify-auth-token'
APNS_TARGET = '/run/secrets/native-push-apns.p8'
FCM_TARGET = '/run/secrets/native-push-fcm.json'


def root_file(name):
    path = p.SECRETS / name
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise RuntimeError('%s must be a regular root-owned 0400/0600 file' % name)
    if info.st_size > 65536:
        raise RuntimeError('%s is too large' % name)
    return path, path.read_text().strip()


def provider_get(host, path, authorization):
    """Fixed provider hosts, no proxies or redirects, bounded response, no error bodies."""
    if not ((host == 'verify.twilio.com' and re.fullmatch(r'/v2/Services/VA[0-9a-fA-F]{32}', path))
            or (host == 'api.resend.com' and (path == '/domains' or re.fullmatch(r'/domains/[0-9a-f-]{36}', path)))):
        raise RuntimeError('Unapproved provider endpoint')

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    request = urllib.request.Request('https://' + host + path, headers={'Authorization': authorization})
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(request, timeout=15) as response:
            data = response.read(65537)
            if len(data) > 65536:
                raise ValueError('oversized')
            return json.loads(data)
    except Exception:
        raise RuntimeError('Provider configuration could not be verified') from None


def activate_phone(cr, sha, run_id, confirm):
    if confirm != 'twilio-verify-ready':
        raise RuntimeError('confirm must be twilio-verify-ready')
    cr.require_release(sha)
    _, text = root_file('phone-verify.json')
    cfg = json.loads(text)
    if set(cfg) != {'accountSid', 'serviceSid'} or not re.fullmatch(r'AC[0-9a-fA-F]{32}', cfg['accountSid']) \
            or not re.fullmatch(r'VA[0-9a-fA-F]{32}', cfg['serviceSid']):
        raise RuntimeError('Invalid phone-verify.json accountSid/serviceSid')
    token, secret = root_file('phone-verify-auth-token')
    if not re.fullmatch(r'[A-Za-z0-9]{16,256}', secret):
        raise RuntimeError('Invalid phone verification token file')
    authorization = 'Basic ' + base64.b64encode((cfg['accountSid'] + ':' + secret).encode()).decode()
    result = provider_get('verify.twilio.com', '/v2/Services/' + cfg['serviceSid'], authorization)
    if result.get('sid') != cfg['serviceSid'] or result.get('account_sid') != cfg['accountSid']:
        raise RuntimeError('Twilio Verify service does not belong to the configured account')
    # GET verifies configuration without sending an SMS or creating an account.
    cr.apply_activation(PHONE_OPERATION, sha, run_id, [('meeting-service', {
        'PHONE_ACCOUNTS_ENABLED': 'true', 'PHONE_VERIFY_ACCOUNT_SID': cfg['accountSid'],
        'PHONE_VERIFY_SERVICE_SID': cfg['serviceSid'], 'PHONE_VERIFY_AUTH_TOKEN_FILE': PHONE_TOKEN_TARGET,
    }, {'mounts': [(token, PHONE_TOKEN_TARGET)]})])


def native_config(cfg):
    if not isinstance(cfg, dict) or not cfg or set(cfg) - {'apns', 'fcm'}:
        raise RuntimeError('Native push metadata must contain apns and/or fcm')
    env, files, platforms = {}, [], []
    if 'apns' in cfg:
        a = cfg['apns']
        if set(a) != {'keyId', 'teamId', 'topic'} or not all(re.fullmatch(r'[A-Z0-9]{10}', a[k]) for k in ('keyId', 'teamId')) \
                or not re.fullmatch(r'[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+', a['topic']):
            raise RuntimeError('Invalid APNs keyId/teamId/topic')
        env.update(NOTIFICATIONS_PUSH_APNS_KEY_ID=a['keyId'], NOTIFICATIONS_PUSH_APNS_TEAM_ID=a['teamId'],
                   NOTIFICATIONS_PUSH_APNS_TOPIC=a['topic'], NOTIFICATIONS_PUSH_APNS_KEY_FILE=APNS_TARGET)
        files.append(('native-push-apns.p8', APNS_TARGET))
        platforms.append('APNS')
    if 'fcm' in cfg:
        if set(cfg['fcm']) != {'projectId'} or not re.fullmatch(r'[a-z][a-z0-9-]{4,61}[a-z0-9]', cfg['fcm']['projectId']):
            raise RuntimeError('Invalid FCM projectId')
        env['NOTIFICATIONS_PUSH_FCM_SERVICE_ACCOUNT_FILE'] = FCM_TARGET
        files.append(('native-push-fcm.json', FCM_TARGET))
        platforms.append('FCM')
    return env, files, platforms


def activate_native_push(cr, sha, run_id, confirm):
    if confirm != 'native-push-credentials-ready':
        raise RuntimeError('confirm must be native-push-credentials-ready')
    cr.require_release(sha)
    if not cr.done('activate-notifications-push'):
        raise RuntimeError('Native push needs activate-notifications-push first')
    _, metadata = root_file('native-push.json')
    cfg = json.loads(metadata)
    env, files, platforms = native_config(cfg)
    mounts = []
    for name, target in files:
        source, content = root_file(name)
        if name.endswith('.json'):
            key = json.loads(content)
            if key.get('type') != 'service_account' or key.get('project_id') != cfg['fcm']['projectId'] \
                    or key.get('token_uri') != 'https://oauth2.googleapis.com/token' \
                    or not str(key.get('client_email', '')).endswith('.iam.gserviceaccount.com') \
                    or not str(key.get('private_key', '')).startswith('-----BEGIN PRIVATE KEY-----'):
                raise RuntimeError('FCM key does not match the configured service-account project')
        elif not content.startswith('-----BEGIN PRIVATE KEY-----'):
            raise RuntimeError('APNs key must be PKCS#8 PEM')
        # Service-readable copy. The root original and values never enter the journal.
        copy = p.SECRETS / ('notifications-' + name)
        p.write_private(copy, content)
        uid = cr.SERVICES['notifications']['uid']
        os.chown(copy, uid, uid)
        os.chmod(copy, 0o400)
        mounts.append((copy, target))

    def post():
        for replica in notifications.REPLICAS:
            supported = notifications.notifications_ready(cr, replica).get('pushPlatforms', [])
            if not set(platforms).issubset(supported):
                raise RuntimeError('Native adapter did not become ready; restoring previous containers')
        cr.emit('nativePush', {'platforms': platforms, 'deviceDelivery': 'REQUIRES_REAL_DEVICE_ACCEPTANCE'})

    cr.apply_activation(NATIVE_OPERATION, sha, run_id,
                        [(r, dict(env), {'mounts': mounts}) for r in notifications.REPLICAS], post=post)


def resend_domain_facts(cr):
    """Read-only DNS verification evidence; no addresses, message bodies, keys or raw errors."""
    try:
        _, key = root_file('resend-api-key')
        result = provider_get('api.resend.com', '/domains', 'Bearer ' + key)
        domain = next((d for d in result.get('data', []) if d.get('name') == 'cfmeet.com'), None)
        if not domain or not re.fullmatch(r'[0-9a-f-]{36}', domain.get('id', '')):
            facts = {'domain': 'cfmeet.com', 'status': 'NOT_FOUND'}
        else:
            details = provider_get('api.resend.com', '/domains/' + domain['id'], 'Bearer ' + key)
            # Only known enum-like statuses are retained. No provider-controlled error text.
            clean = lambda s: s if isinstance(s, str) and re.fullmatch(r'[A-Za-z_]{1,32}', s) else 'unknown'
            facts = {'domain': 'cfmeet.com', 'status': clean(details.get('status')),
                     'records': [{'record': clean(r.get('record')), 'status': clean(r.get('status'))}
                                 for r in details.get('records', [])[:32]]}
    except Exception:
        facts = {'domain': 'cfmeet.com', 'status': 'UNAVAILABLE_OR_SEND_ONLY_KEY'}
    cr.emit('resendDomainVerification', facts)
    return facts
