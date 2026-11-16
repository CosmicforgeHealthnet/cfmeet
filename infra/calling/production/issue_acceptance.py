"""Production verification of the CF Meet server-bug fixes (16 November 2026) — VPS side.

Run by `.github/workflows/cfmeet-issue-acceptance.yml` on the main head under the shared
production lock. Read-only towards the running system: no container is created other than the
ephemeral probe client, nothing is restarted, reconfigured or migrated, and no application
grant changes. What it writes is bounded to the controlled acceptance accounts:

  - it reuses `secrets/calling-acceptance-accounts.json` and refuses to register accounts,
    so no email is sent;
  - it issues one Platform Access credential for CF Meet's own production identity scope
    (the one `communications_release.prod_scope()` names), valid one day, restricted to the
    controlled accounts' principals, and revokes it when the journey ends;
  - the journey itself (`issue_acceptance_client.py`) creates calls, one group and one
    attachment among those accounts only, and ends, archives or removes them.

The client runs in the existing probe image (python + livekit) on the host network against
https://api.cfmeet.com, exactly like a client. Secrets reach it only through a 0600 file in the
evidence folder, deleted afterwards; nothing secret is printed.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import production as p

ACCOUNTS = 'calling-acceptance-accounts.json'
PUBLIC = 'https://api.cfmeet.com'
MESSAGING = 'https://api.cfmeet.com:8444'
TIMEOUT_SECONDS = 900
UID = 10001
LABEL = 'CF Meet server-bug production verification'
# Personal Calling and Messaging only: no enterprise, telecom, policy-write or admin scope.
SCOPES = ('calls.read', 'calls.write', 'messages.read', 'messages.send', 'conversations.create',
          'conversations.manage', 'attachments.write')
DETAIL_KEYS = ('testPrincipals', 'c1CallId', 'c2OfflineCallId', 'c3CallId', 'groupConversationId', 'attachmentId',
               'messagingCapability', 'attachmentsCapability', 'policyBaseline', 'sharedMediaItem',
               'sharedMediaThumbnail', 'notes', 'cleanupFailures')
METRIC_PREFIXES = ('calling_media_admission_retries_total', 'calling_media_admission_unavailable_total',
                   'calling_finished_call_invitations_closed_total')


def context(run_id, accounts, credential, principals, scopes=SCOPES):
    """The only data the client receives: public URLs, the controlled accounts and one
    restricted, short-lived credential (with the scopes it actually holds)."""
    return {'base': PUBLIC, 'messaging': MESSAGING, 'runId': run_id, 'credential': credential,
            'principals': list(principals), 'scopes': list(scopes),
            'accounts': [{'email': a['email'], 'password': a['password']} for a in accounts]}


def verdict(report):
    """FAIL on any failed check (or none at all); PARTIAL when some checks could not run."""
    states = list((report.get('checks') or {}).values())
    if not states or 'FAIL' in states:
        return 'FAIL'
    return 'PASS' if all(s == 'PASS' for s in states) else 'PARTIAL'


def public_report(report):
    """Report fields safe to print: check results, non-secret ids and observations only."""
    details = report.get('details') or {}
    return {'runId': report.get('runId'), 'verdict': verdict(report), 'checks': report.get('checks') or {},
            'details': {k: details[k] for k in DETAIL_KEYS if k in details}}


def admission_metrics(text):
    """Only the admission / offer-repair counter lines of a Prometheus exposition."""
    return sorted(line for line in text.splitlines() if line.startswith(METRIC_PREFIXES))


def api(path, body=None, token=None):
    headers = {'Accept': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    data = None
    if body is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(body).encode()
    request = urllib.request.Request(PUBLIC + path, data=data, headers=headers, method='POST' if data else 'GET')
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def principal_of(account):
    """The controlled account's principal, read through the public API (then signed out)."""
    login = api('/api/v1/auth/login', {'email': account['email'], 'password': account['password']})
    try:
        return api('/api/v2/messaging/me', token=login['access_token'])['principalId']
    finally:
        try:
            api('/api/v1/auth/logout', {'refresh_token': login['refresh_token']}, login['access_token'])
        except (urllib.error.URLError, KeyError):
            pass


REQUIRED_SCOPES = ('calls.read', 'calls.write')


class AdminRefused(RuntimeError):
    """The Platform Access admin tool refused; carries only a fixed category, never output."""


def refusal_category(text):
    """A fixed label for an admin-tool error. The text itself never leaves the VPS."""
    text = (text or '').lower()
    if 'not allowed' in text:
        return 'SCOPE_NOT_ALLOWED'
    if 'not found' in text or 'unknown' in text:
        return 'NOT_FOUND'
    if 'principal' in text:
        return 'PRINCIPAL_RESTRICTION_REFUSED'
    return 'ADMIN_TOOL_FAILED'


def admin(*args):
    """api_acceptance.admin with the same command line, but a failure is classified."""
    image = p.inspect('cfmeet-platform-access')['Image']
    proc = subprocess.run(['docker', 'run', '--rm', '--network', p.NET, '--env-file', str(p.ROOT / 'platform-access.runtime.env'),
                           '--mount', f'type=bind,src={p.SECRETS}/platform-access-pepper,dst=/run/secrets/platform-pepper,readonly',
                           '--entrypoint', '/app/platform-access-admin', image, '--operator', 'cfmeet-production-acceptance', *args],
                          capture_output=True, text=True)
    if proc.returncode:
        raise AdminRefused(args[0] + ' ' + args[1] + ': ' + refusal_category(proc.stderr + proc.stdout))
    return json.loads(proc.stdout)


def usable_scopes(allowed):
    """The verification scopes the application may hold in production (no grant is ever added)."""
    scopes = [s for s in SCOPES if s in set(allowed)]
    missing = [s for s in REQUIRED_SCOPES if s not in scopes]
    if missing:
        raise RuntimeError('CF Meet production identity lacks required scopes: ' + ', '.join(missing))
    return scopes


def issue_credential(principals):
    import api_acceptance
    import communications_release as release
    import uuid
    scope = release.prod_scope()
    account = str(uuid.UUID(scope['serviceAccountId']))
    refs = json.loads(api_acceptance.sql(
        "SELECT json_build_object('tenant',t.slug,'application',a.slug,'account',s.name) "
        "FROM platform_service_accounts s JOIN platform_tenants t ON t.id=s.tenant_id "
        "JOIN platform_applications a ON a.id=s.application_id WHERE s.id='" + account + "'"))
    allowed = json.loads(api_acceptance.sql(
        "SELECT coalesce(json_agg(x.scope ORDER BY x.scope),'[]') FROM platform_application_scopes x "
        "JOIN platform_service_accounts s ON s.application_id=x.application_id WHERE s.id='" + account + "' "
        "AND (x.allowed_environment_kinds IS NULL OR EXISTS "
        "(SELECT 1 FROM unnest(x.allowed_environment_kinds) k WHERE lower(k)='production'))"))
    scopes = usable_scopes(allowed)
    issued = admin('credential', 'create', '--tenant', refs['tenant'], '--application', refs['application'],
                   '--environment', 'production', '--service-account', refs['account'],
                   '--expires-in-days', '1', *[v for s in scopes for v in ('--scope', s)], '--label', LABEL)
    credential_id = issued['credential']['id']
    try:
        admin('credential', 'restrict-principals', '--credential', credential_id,
              *[v for pid in principals for v in ('--principal', str(uuid.UUID(pid)))])
    except BaseException:
        revoke_credential(credential_id)
        raise
    return issued['secret'], credential_id, scopes


def revoke_credential(credential_id):
    admin('credential', 'revoke', '--credential', credential_id)


def read_metrics():
    address = p.env_of(p.inspect('cfmeet-calling')).get('CALLING_METRICS_ADDRESS')
    if not address or not re.fullmatch(r'[0-9a-zA-Z.\-]+:[0-9]{2,5}', address):
        return 'NOT_EXPOSED'
    port = address.rsplit(':', 1)[1]
    try:
        text = p.run('docker', 'run', '--rm', '--network', 'container:cfmeet-calling', 'curlimages/curl:8.12.1',
                     '-sS', '--max-time', '10', 'http://127.0.0.1:' + port + '/metrics')
    except Exception as error:  # noqa: BLE001 - observability is reported, never fatal
        return 'UNREADABLE:' + type(error).__name__
    return admission_metrics(text)


def probe_image(source):
    p.run('docker', 'pull', 'python:3.12-slim')
    base = json.loads(p.run('docker', 'image', 'inspect', 'python:3.12-slim'))[0]['RepoDigests'][0]
    tag = 'cfmeet-calling-api-probe:' + hashlib.sha256((source / 'Probe.Dockerfile').read_bytes()
                                                       + base.encode()).hexdigest()[:20]
    if subprocess.run(['docker', 'image', 'inspect', tag], capture_output=True).returncode:
        p.run('docker', 'build', '--network', 'default', '--build-arg', 'PYTHON_IMAGE=' + base,
              '-f', str(source / 'Probe.Dockerfile'), '-t', tag, str(source))
    return tag


def run(run_id):
    if not re.fullmatch(r'[0-9]{1,20}', run_id):
        raise ValueError('run id must be the numeric workflow run id')
    accounts_file = p.SECRETS / ACCOUNTS
    if not accounts_file.exists():
        raise RuntimeError('Controlled acceptance accounts missing; refusing to register new ones')
    accounts = json.loads(accounts_file.read_text())[:3]
    if len(accounts) < 2:
        raise RuntimeError('At least two controlled acceptance accounts are required')
    source = Path(__file__).resolve().parent
    if not (source / 'rtc_client.py').exists():
        raise RuntimeError('rtc_client.py was not shipped with the tooling')
    principals = [principal_of(a) for a in accounts]
    before = read_metrics()
    root = p.ROOT / 'production-evidence' / ('issues-' + run_id)
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chown(root, UID, UID)
    private = root / 'context.private.json'
    name = 'cfmeet-issue-acceptance-' + run_id
    secret, credential_id, scopes = issue_credential(principals)
    try:
        p.write_private(private, json.dumps(context(run_id, accounts, secret, principals, scopes)))
        os.chown(private, UID, UID)
        tag = probe_image(source)
        command = ['docker', 'run', '--rm', '--name', name, '--network', 'host', '--memory', '768m', '--cpus', '1',
                   '--tmpfs', '/tmp:rw,nosuid,size=256m,uid=10001,gid=10001',
                   '--mount', f'type=bind,src={root},dst=/result',
                   '--mount', f'type=bind,src={source}/issue_acceptance_client.py,dst=/client.py,readonly',
                   '--mount', f'type=bind,src={source}/communications_acceptance.py,dst=/communications_acceptance.py,readonly',
                   '--mount', f'type=bind,src={source}/rtc_client.py,dst=/rtc_client.py,readonly',
                   tag, 'python', '-u', '/client.py']
        with (root / 'execution.private.log').open('w') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + TIMEOUT_SECONDS
            progress = time.monotonic() + 30
            try:
                while process.poll() is None:
                    if time.monotonic() > deadline:
                        raise RuntimeError('Issue verification timed out')
                    if time.monotonic() >= progress:
                        print('ISSUE_ACCEPTANCE_RUNNING: calling, media and chat assertions', flush=True)
                        progress = time.monotonic() + 30
                    time.sleep(.25)
            finally:
                if process.poll() is None:
                    subprocess.run(['docker', 'stop', '--time', '10', name], capture_output=True, timeout=25)
                    process.wait(timeout=15)
    finally:
        private.unlink(missing_ok=True)
        revoke_credential(credential_id)
    report_file = root / 'report.json'
    report = json.loads(report_file.read_text()) if report_file.exists() else {
        'runId': run_id, 'checks': {'client produced a report': 'FAIL'}}
    result = public_report(report)
    result['admissionMetrics'] = {'before': before, 'after': read_metrics()}
    result['images'] = {n: p.inspect(n)['Image'] for n in ('cfmeet-calling', 'cfmeet-messaging')}
    result['verificationCredential'] = 'REVOKED'
    result['credentialScopes'] = scopes
    result['scopesNotAllowedForApplication'] = [s for s in SCOPES if s not in scopes]
    print(json.dumps(result, indent=2), flush=True)
    if result['verdict'] == 'FAIL':
        raise SystemExit('ISSUE_ACCEPTANCE_FAILED')
    print('ISSUE_ACCEPTANCE: ' + result['verdict'], flush=True)


if __name__ == '__main__':
    run(sys.argv[1])
