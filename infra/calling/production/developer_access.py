"""Final completion (Section 6): the Developer Platform production-access review path.

Imported by coordinated_release.py. Manual approval only (automatic approval stays refused
by the safe defaults and by the Developer Platform's own configuration check).

* `activate-production-access-review`: creates the operator console identity
  (`cfmeet.operator-console`, an Ed25519 key in the VPS secret store), grants it
  `developer-platform.administer` on the Developer Platform (the only identity that
  permission may be granted to), names the reviewers
  (`DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`, from the owner's
  `CFMEET_PRODUCTION_ACCESS_REVIEWERS` variable) and only then enables customer
  submissions (`DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=true`): requests are
  never accepted while nobody can review them. Post-check: the review tool lists requests.
* `developer-access-review`: one operator command (`list`, `get`, `review`, `approve`,
  `reject`) by the dispatching reviewer (GitHub login → `github-<login>`, which must be a
  named reviewer with a CF Meet user id), through the Developer Platform image's
  `developer-platform-operator` tool. Approve / reject are audited by the Developer
  Platform (self-approval refused there); the applicant is then emailed through the
  Meeting Service's unified email delivery (Resend in production).
"""

import json
import re
import subprocess
import urllib.error
import urllib.request

import production as p

KEY_FILE = 'operator-console-service-auth.pem'
KEY_ID = 'operator-console-production-1'
KEY_TARGET = '/run/secrets/operator-console.pem'
REVIEWERS_FILE = 'production-reviewers'
REVIEW_FILE = 'developer-access-review.json'
REVIEWER = re.compile(r'^github-[a-z0-9](?:[a-z0-9-]{0,38})=[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
UUID = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
COMMANDS = ('list', 'get', 'review', 'approve', 'reject')


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True, default=str), flush=True)


def parse_reviewers(text):
    """`github-<login>=<CF Meet user id>` entries, comma or newline separated."""
    entries = [e.strip().lower() for e in re.split(r'[,\n]', text or '') if e.strip()]
    if not entries or not all(REVIEWER.fullmatch(e) for e in entries):
        raise RuntimeError('CFMEET_PRODUCTION_ACCESS_REVIEWERS must list github-<login>=<CF Meet user id> entries')
    return dict(e.split('=', 1) for e in entries)


def reviewers(cr):
    path = cr.WORK / REVIEWERS_FILE
    if not path.exists():
        raise RuntimeError('No production-access reviewers are named (set the CFMEET_PRODUCTION_ACCESS_REVIEWERS '
                           'variable of the production environment)')
    return parse_reviewers(path.read_text())


def ensure_operator_key(cr):
    """Create the operator console's Ed25519 key once (activation only)."""
    key = p.SECRETS / KEY_FILE
    if not key.exists():
        p.run('openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(key))
    key.chmod(0o400)
    return key


def operator_key(cr):
    key = p.SECRETS / KEY_FILE
    if not key.exists():
        raise RuntimeError('The operator console identity does not exist (run activate-production-access-review)')
    return key


def operator_public_key(cr):
    """Read-only (certify): never creates the key."""
    import communications_release as release
    return release.public_key(operator_key(cr), KEY_ID)


def tool(cr, args, operator=None, user_id=None):
    """Run developer-platform-operator from the running Developer Platform image."""
    dp = p.inspect(cr.SERVICES['developer-platform']['container'])
    uid = cr.SERVICES['developer-platform']['uid'] or 10001
    key = operator_key(cr)
    env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': KEY_ID,
           'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': KEY_TARGET,
           'CF_DEVELOPER_PLATFORM_BASE_URL': cr.private_url(cr.SERVICES['developer-platform']['tls']),
           'CF_OPERATOR': operator or 'github-operations'}
    if user_id:
        env['CF_OPERATOR_USER_ID'] = user_id
    path = cr.WORK / 'run' / 'developer-access.env'
    p.write_private(path, ''.join('%s=%s\n' % kv for kv in env.items()))
    copy = cr.WORK / 'run' / 'operator-console.pem'
    copy.write_bytes(key.read_bytes())
    import os
    os.chown(copy, uid, uid)
    copy.chmod(0o400)
    try:
        proc = subprocess.run(
            ['docker', 'run', '--rm', '--network', 'cfmeet-core-private', '--read-only', '--cap-drop', 'ALL',
             '--security-opt', 'no-new-privileges', '--user', '%d:%d' % (uid, uid), '--memory', '64m',
             '--env-file', str(path), '-v', '%s:%s:ro' % (copy, KEY_TARGET),
             '--entrypoint', '/app/developer-platform-operator', dp['Image'], *args],
            capture_output=True, text=True, timeout=120)
    finally:
        path.unlink(missing_ok=True)
        copy.unlink(missing_ok=True)
    if proc.returncode:
        raise RuntimeError('Operator command refused: ' + cr.scrub(proc.stderr.strip())[-300:])
    return json.loads(proc.stdout.strip().splitlines()[-1])


def activate(cr, sha, run_id, confirm):
    names = reviewers(cr)
    if cr.env_of('developer-platform').get('DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL') == 'true':
        raise RuntimeError('Automatic production approval is configured; refusing')
    if not image_has_tool(cr):
        raise RuntimeError('The running Developer Platform predates the review tool; release first')
    ensure_operator_key(cr)
    flags = {'DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS': ','.join(sorted(names)),
             'DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED': 'true'}
    emit('productionAccessReview', {'reviewers': sorted(names), 'automaticApproval': False})
    cr.apply_activation('activate-production-access-review', sha, run_id,
                        [('developer-platform', flags), ('developer-platform-worker', flags)],
                        post=lambda: emit('reviewQueue', summarize(tool(cr, ['list']))))


def image_has_tool(cr):
    """The running Developer Platform image ships the review tool (it prints its usage)."""
    dp = p.inspect(cr.SERVICES['developer-platform']['container'])
    proc = subprocess.run(['docker', 'run', '--rm', '--network', 'none', '--entrypoint',
                           '/app/developer-platform-operator', dp['Image']], capture_output=True, text=True)
    return 'usage: developer-platform-operator' in proc.stderr


def summarize(answer):
    result = answer.get('result') or {}
    items = result.get('items') if isinstance(result, dict) else None
    if items is None and isinstance(result, list):
        items = result
    if items is not None:
        return {'count': len(items), 'requests': [
            {k: i.get(k) for k in ('id', 'state', 'requestedCapabilities', 'createdAt')} for i in items[:50]]}
    return {k: result.get(k) for k in ('id', 'state', 'requestedCapabilities', 'approvedCapabilities',
                                        'decisionReasonCode', 'reviewerRef', 'decidedAt', 'requestedBy')}


def notify_applicant(cr, decision, request, reason=None):
    env = p.env_of(p.inspect(cr.MEETING))
    token = env.get('INTERNAL_API_TOKEN')
    applicant = request.get('requestedBy')
    if not token or not applicant or not UUID.fullmatch(str(applicant)):
        return 'not sent (no applicant or internal token)'
    body = {'userId': applicant, 'requestId': request['id'], 'decision': decision}
    if decision == 'REJECTED':
        body['reasonCode'] = reason or request.get('decisionReasonCode') or 'NOT_ELIGIBLE'
    req = urllib.request.Request('http://127.0.0.1:18081/internal/operations/email/developer-access-decision',
                                 data=json.dumps(body).encode(), method='POST',
                                 headers={'X-Internal-Token': token, 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return 'queued' if r.status == 202 else 'HTTP %d' % r.status
    except urllib.error.HTTPError as e:
        return 'HTTP %d' % e.code


def review(cr, sha, run_id, confirm):
    if not cr.done('activate-production-access-review'):
        raise RuntimeError('Run activate-production-access-review first')
    path = cr.WORK / REVIEW_FILE
    if not path.exists():
        raise RuntimeError('No review command was provided by the workflow')
    doc = json.loads(path.read_text())
    path.unlink(missing_ok=True)
    command, operator = doc.get('command'), doc.get('operator', '')
    if command not in COMMANDS:
        raise RuntimeError('Unknown review command')
    names = reviewers(cr)
    if operator not in names:
        raise RuntimeError('The dispatching GitHub account is not a named production-access reviewer')
    args = [command]
    if command != 'list':
        if not UUID.fullmatch(doc.get('requestId', '')):
            raise RuntimeError('A production-access request id is required')
        args.append(doc['requestId'])
    if command == 'reject':
        reason = doc.get('reasonCode', '')
        if not re.fullmatch(r'[A-Z][A-Z0-9_]{1,63}', reason):
            raise RuntimeError('A rejection needs a reason code (upper snake case)')
        args.append(reason)
    answer = tool(cr, args, operator, names[operator])
    summary = summarize(answer)
    emit('developerAccessReview', {'command': command, 'operator': operator, 'result': summary})
    if command in ('approve', 'reject'):
        request = answer.get('result') or {}
        emit('applicantNotification', notify_applicant(cr, 'APPROVED' if command == 'approve' else 'REJECTED',
                                                       request, doc.get('reasonCode')))
