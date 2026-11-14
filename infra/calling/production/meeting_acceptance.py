"""Meetings production acceptance (Q&A restore, LiveKit participant identity) — VPS side.

Run by `.github/workflows/cfmeet-meeting-acceptance.yml` on the main head under the shared
production lock. It reuses the existing controlled acceptance account
(`secrets/calling-acceptance-accounts.json`) and refuses to register a new one, so no email is
sent. The journey itself (`meeting_acceptance_client.py`) runs in the existing probe image
(python + livekit) on the host network against https://api.cfmeet.com and wss LiveKit, exactly
like a client. The account password reaches the client only through a 0600 file inside the
evidence folder, which is deleted afterwards; nothing secret is printed.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

import production as p

ACCOUNTS = 'calling-acceptance-accounts.json'
PUBLIC = 'https://api.cfmeet.com'
TIMEOUT_SECONDS = 600
UID = 10001


def context(run_id, accounts):
    """The only data the client receives: the public base URL and one controlled account."""
    account = accounts[0]
    return {'base': PUBLIC, 'runId': run_id,
            'account': {'email': account['email'], 'password': account['password']}}


def verdict(report):
    checks = report.get('checks') or {}
    return 'PASS' if checks and all(v == 'PASS' for v in checks.values()) else 'FAIL'


def public_report(report):
    """Report fields safe to print: check results and non-secret ids only."""
    return {'runId': report.get('runId'), 'verdict': verdict(report), 'checks': report.get('checks') or {},
            'details': {k: v for k, v in (report.get('details') or {}).items()
                        if k in ('meetingSpaceId', 'conferenceId', 'participantIds')}}


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
        raise RuntimeError('Controlled acceptance account missing; refusing to register a new one')
    accounts = json.loads(accounts_file.read_text())
    root = p.ROOT / 'production-evidence' / ('meeting-' + run_id)
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chown(root, UID, UID)
    private = root / 'context.private.json'
    source = Path(__file__).resolve().parent
    name = 'cfmeet-meeting-acceptance-' + run_id
    try:
        p.write_private(private, json.dumps(context(run_id, accounts)))
        os.chown(private, UID, UID)
        tag = probe_image(source)
        command = ['docker', 'run', '--rm', '--name', name, '--network', 'host', '--memory', '768m', '--cpus', '1',
                   '--tmpfs', '/tmp:rw,nosuid,size=256m,uid=10001,gid=10001',
                   '--mount', f'type=bind,src={root},dst=/result',
                   '--mount', f'type=bind,src={source}/meeting_acceptance_client.py,dst=/client.py,readonly',
                   tag, 'python', '-u', '/client.py']
        with (root / 'execution.private.log').open('w') as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + TIMEOUT_SECONDS
            progress = time.monotonic() + 30
            try:
                while process.poll() is None:
                    if time.monotonic() > deadline:
                        raise RuntimeError('Meetings acceptance timed out')
                    if time.monotonic() >= progress:
                        print('MEETING_ACCEPTANCE_RUNNING: Q&A restore and LiveKit identity assertions', flush=True)
                        progress = time.monotonic() + 30
                    time.sleep(.25)
            finally:
                if process.poll() is None:
                    subprocess.run(['docker', 'stop', '--time', '10', name], capture_output=True, timeout=25)
                    process.wait(timeout=15)
    finally:
        private.unlink(missing_ok=True)
    report_file = root / 'report.json'
    report = json.loads(report_file.read_text()) if report_file.exists() else {
        'runId': run_id, 'checks': {'client produced a report': 'FAIL'}}
    result = public_report(report)
    result['meetingServiceImage'] = p.inspect('cfmeet-production-backend')['Image']
    print(json.dumps(result, indent=2), flush=True)
    if result['verdict'] != 'PASS':
        raise SystemExit('MEETING_ACCEPTANCE_FAILED')
    print('MEETING_ACCEPTANCE: PASS', flush=True)


if __name__ == '__main__':
    run(sys.argv[1])
