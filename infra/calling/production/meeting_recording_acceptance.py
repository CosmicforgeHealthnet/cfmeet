"""Launch a disposable real Meeting API / RTC client with controlled account tokens."""
import hashlib
import json
from pathlib import Path
import subprocess
import urllib.request
import uuid

import app_acceptance
import audit_operations
import production as p


def run(cr, run_id):
    folder = p.ROOT / 'meeting-recording' / 'acceptance' / run_id
    folder.mkdir(parents=True, exist_ok=False, mode=0o700)
    accounts = app_acceptance.accounts(namespace='meeting-recording', count=2, keep_sessions=True)
    tokens = []
    for account in accounts:
        if account.get('refreshToken'):
            result = app_acceptance.api('refresh', {'refresh_token': account['refreshToken']})
        else:
            result = app_acceptance.sign_in(account)
        tokens.append(result['access_token'])
    credentials = folder / 'credentials.private.json'
    p.run('chown', '10001:10001', str(folder))
    # Reuse the controlled probe's pinned SDK manifest, isolated from the private
    # runtime network during installation. Never mount a server API key in this client.
    python = json.loads(p.run('docker', 'image', 'inspect', 'python:3.12-slim'))[0]['RepoDigests'][0]
    dockerfile = Path(__file__).with_name('Probe.Dockerfile')
    image_hash = hashlib.sha256(dockerfile.read_bytes() + python.encode()).hexdigest()[:20]
    image = 'cfmeet-calling-media-probe:' + image_hash
    if subprocess.run(['docker', 'image', 'inspect', image], capture_output=True).returncode:
        p.run('docker', 'build', '--network', 'default', '--build-arg', 'PYTHON_IMAGE=' + python,
              '-f', str(dockerfile), '-t', image, str(dockerfile.parent))
    name = 'cfmeet-meeting-recording-probe-' + run_id
    try:
        p.write_private(credentials, json.dumps(dict(owner=tokens[0], outsider=tokens[1])))
        p.run('chmod', '0400', str(credentials))
        p.run('chown', '10001:10001', str(credentials))
        # Match the existing real RTC acceptance: avoid Docker NAT/hairpin for
        # the public WSS/ICE path. Only this non-root, scoped fixture client uses it.
        result = subprocess.run(['docker', 'run', '--rm', '--name', name, '--network', 'host',
            '--user', '10001:10001', '--read-only', '--log-driver', 'none', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges', '--memory', '768m', '--cpus', '1', '--pids-limit', '96',
            '--tmpfs', '/tmp:rw,nosuid,size=64m,uid=10001,gid=10001',
            '--mount', 'type=bind,src=%s,dst=/result' % folder,
            '--mount', 'type=bind,src=%s,dst=/probe.py,readonly' % Path(__file__).with_name('meeting_recording_probe.py'),
            image, 'python', '/probe.py'], capture_output=True, text=True, timeout=600)
        report_path = folder / 'report.json'
        report = json.loads(report_path.read_text()) if report_path.is_file() else {}
        if result.returncode or report.get('result') != 'PASS':
            cr.emit('meetingRecordingFailure', audit_operations.recording_receipt(report, run_id))
            raise RuntimeError('Meeting recording acceptance failed; protected receipt retained on VPS')
        return report
    except subprocess.TimeoutExpired:
        p.run('docker', 'stop', '--time', '10', name)
        raise RuntimeError('Meeting recording acceptance timed out; recording remains disabled after rollback') from None
    finally:
        try:
            progress = folder / 'cleanup.json'
            if progress.is_file():
                ids = json.loads(progress.read_text())
                # Only UUIDs written by this disposable client, with this controlled owner.
                cid = str(uuid.UUID(ids['conference']))
                sid = str(uuid.UUID(ids['space']))
                paths = []
                if ids.get('recording'):
                    rid = str(uuid.UUID(ids['recording']))
                    paths += [('POST', '/conferences/' + cid + '/recordings/' + rid + '/stop'),
                              ('DELETE', '/conferences/' + cid + '/artifacts/' + rid)]
                paths += [('POST', '/conferences/' + cid + '/end'), ('POST', '/meeting-spaces/' + sid + '/archive')]
                for method, path in paths:
                    try:
                        request = urllib.request.Request('https://api.cfmeet.com/api/v2' + path,
                            data=b'{}' if method == 'POST' else None, method=method,
                            headers={'Authorization': 'Bearer ' + tokens[0], 'Content-Type': 'application/json'})
                        with urllib.request.urlopen(request, timeout=15):
                            pass
                    except Exception:
                        # The probe already attempted the same idempotent cleanup. Keep its
                        # ids in the protected receipt for operator follow-up on an outage.
                        pass
        except Exception:
            pass
        finally:
            credentials.unlink(missing_ok=True)
