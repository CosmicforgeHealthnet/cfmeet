"""Production operations. Secret-bearing Docker responses stay on the VPS."""
import base64
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

ROOT = Path('/opt/cfmeet-core')
SECRETS = ROOT / 'secrets'
SAFE_ENV = {
    'CALLING_NEW_CALLS_ENABLED', 'CALLING_APP_CALLING_ENABLED',
    'ENTERPRISE_CALLING_ENABLED', 'VOICEMAIL_ENABLED', 'CALL_RECORDING_ENABLED',
    'CALLING_PUBLIC_OUTBOUND_ENABLED', 'CALLING_PUBLIC_INBOUND_ENABLED',
    'CALLING_CALLBACK_RELAY_ENABLED', 'CALLING_SYNTHETIC_INTERCONNECT_ENABLED',
    'CALLING_MEDIA_PROVIDER', 'CALLING_CAPTURE_PROVIDER',
    'CALLING_ARTIFACT_STORAGE_SERVER_SIDE_ENCRYPTION',
}


def run(*args, input=None):
    p = subprocess.run(args, input=input, capture_output=True, text=True)
    if p.returncode:
        # Commands and stderr can contain credentials. Emit neither.
        raise RuntimeError('subprocess failed: ' + args[0])
    return p.stdout.strip()


def inspect(name):
    return json.loads(run('docker', 'inspect', name))[0]


def env_of(container):
    return dict(x.split('=', 1) for x in container['Config']['Env'] if '=' in x)


def secret(name):
    return (SECRETS / name).read_text().strip()


def lk_request(method, body, key, password, grants):
    b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip('=')
    now = int(time.time())
    payload = dict(iss=key, sub='cfmeet-production-acceptance', iat=now,
                   nbf=now-2, exp=now+90, video=grants)
    token = b64(b'{"alg":"HS256","typ":"JWT"}') + '.' + b64(json.dumps(payload).encode())
    token += '.' + b64(hmac.new(password.encode(), token.encode(), hashlib.sha256).digest())
    req = urllib.request.Request('http://127.0.0.1:7880/twirp/livekit.' + method,
        data=json.dumps(body).encode(), headers={'Authorization': 'Bearer '+token,
                                                'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=12) as response:
        return json.load(response)


def identity():
    for name in ('cfmeet-calling', 'cfmeet-production-backend', 'cfmeet-messaging',
                 'cfmeet-platform-access', 'lk-server', 'cfmeet-calling-redis',
                 'cfmeet-calling-artifacts'):
        assert inspect(name)['State']['Running'], 'Expected production identity absent: '+name
    assert Path('/opt/livekit/livekit.yaml').is_file()


def audit():
    identity()
    for name in ('cfmeet-calling', 'cfmeet-production-backend', 'cfmeet-messaging',
                 'cfmeet-platform-access', 'lk-server', 'cfmeet-calling-redis',
                 'cfmeet-calling-artifacts', 'cfmeet-calling-egress'):
        try:
            c = inspect(name)
        except RuntimeError:
            print(json.dumps({'service': name, 'state': 'absent'}))
            continue
        h, cfg = c['HostConfig'], c['Config']
        print(json.dumps(dict(service=name, state=c['State']['Status'], image=cfg['Image'],
            image_id=c['Image'], network_mode=h['NetworkMode'],
            networks=list(c['NetworkSettings']['Networks']), ports=h['PortBindings'],
            memory=h['Memory'], nano_cpus=h['NanoCpus'], restart=h['RestartPolicy'],
            mounts=[dict(source=m['Source'], destination=m['Destination'], rw=m['RW']) for m in c['Mounts']],
            entrypoint_present=bool(cfg['Entrypoint']), command_present=bool(cfg['Cmd']),
            healthcheck_present=bool(cfg.get('Healthcheck')),
            environment_names=sorted(env_of(c)),
            flags={k: v for k, v in env_of(c).items() if k in SAFE_ENV})))
    meeting = env_of(inspect('cfmeet-production-backend'))
    rooms = lk_request('RoomService/ListRooms', {}, meeting['LIVEKIT_API_KEY'],
        meeting['LIVEKIT_API_SECRET'], {'roomList': True}).get('rooms', [])
    print(json.dumps(dict(livekit_rooms=len(rooms), participants=sum(
        int(r.get('numParticipants', r.get('num_participants', 0))) for r in rooms))))
    print(json.dumps(dict(pyyaml_available=bool(importlib.util.find_spec('yaml')),
        config_top_level_keys=[line.split(':', 1)[0] for line in
            Path('/opt/livekit/livekit.yaml').read_text().splitlines()
            if line and not line.startswith((' ', '#')) and ':' in line],
        backup_config_files=[p.name for p in ROOT.glob('*backup*')],
        kms_env_names=[k for k in env_of(inspect('cfmeet-calling-artifacts')) if 'KMS' in k],
        disk=run('df', '-h', '/').splitlines()[-1])))


if __name__ == '__main__':
    os.umask(0o077)
    try:
        operation, sha, run_id = sys.argv[1:]
        if operation == 'inspect':
            audit()
        else:
            raise RuntimeError('Operation not yet implemented; activation requires real acceptance evidence')
    except Exception as exc:
        # Avoid exception details from network libraries leaking signed URLs.
        print('PRODUCTION_OPERATION_FAILED:', type(exc).__name__)
        sys.exit(1)
