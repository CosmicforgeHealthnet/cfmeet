"""Production operations. Secret-bearing Docker responses stay on the VPS."""
import base64
import hashlib
import copy
import http.client
import hmac
import importlib.util
import json
import os
import shutil
import socket
import re
from pathlib import Path
import subprocess
import sys
import time
import urllib.request
import urllib.error

ROOT = Path('/opt/cfmeet-core')
SECRETS = ROOT / 'secrets'
NET = 'cfmeet-core-private'
EGRESS = 'cfmeet-calling-egress'
REQUIRED = ('voice', 'video', 'invitations_accept_decline_cancel', 'termination_cleanup',
    'reconnection', 'history_missed', 'extensions', 'ring_groups', 'queues',
    'transfers_hold_resume', 'ivr', 'tenant_isolation', 'voicemail_lifecycle',
    'recording_consent_authorization', 'recording_lifecycle', 'https_retrieval_isolation',
    'retention_deletion', 'staging_cleanup', 'encryption_at_rest',
    'offserver_encrypted_backup_restore', 'meetings_media', 'messaging', 'public_swagger')
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
    store_env = env_of(inspect('cfmeet-calling-artifacts'))
    for key in sorted(k for k in store_env if 'KMS' in k):
        value = store_env[key]
        print(json.dumps({'kms_setting': key, 'configured': bool(value),
                          'mounted_secret': any(m['Destination'] == value for m in inspect('cfmeet-calling-artifacts')['Mounts'])}))
    print(json.dumps({'nginx_sites': [str(p) for p in Path('/etc/nginx/sites-enabled').glob('*')],
                      'cfmeet_backup_files': [str(p) for p in ROOT.glob('**/*backup*.env')],
                      'ffprobe_available': bool(shutil.which('ffprobe'))}))
    c = inspect('lk-server')
    print(json.dumps({'livekit_command': c['Config']['Cmd'],
                      'livekit_entrypoint': c['Config']['Entrypoint']}))


def write_private(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(data)
    path.chmod(0o600)


def current_rooms():
    e = env_of(inspect('cfmeet-production-backend'))
    return lk_request('RoomService/ListRooms', {}, e['LIVEKIT_API_KEY'],
                      e['LIVEKIT_API_SECRET'], {'roomList': True}).get('rooms', [])


def regression():
    identity()
    run('curl', '-fsS', '--max-time', '10', 'http://127.0.0.1:18081/health')
    for port, path in ((8443, '/ready'), (8444, '/ready'), (8445, '/health'), (8446, '/')):
        run('docker', 'run', '--rm', '--network', NET, 'curlimages/curl:8.12.1',
            '-fsS', '--max-time', '10', f'https://api.cfmeet.com:{port}{path}')
    current_rooms()
    lk_request('RoomService/ListRooms', {}, secret('calling-livekit-api-key'),
               secret('calling-livekit-api-secret'), {'roomList': True})
    print('REGRESSION: Meetings health, Messaging/Calling/Platform TLS readiness, both LiveKit keys PASS')


def redis_ping_from(container):
    c = inspect(container)
    assert NET in c['NetworkSettings']['Networks']
    # Redis CLI runs in the exact target network namespace, not merely another bridge member.
    run('docker', 'run', '--rm', '--network', 'container:'+container,
        '--mount', f'type=bind,src={SECRETS}/calling-capture-redis-password,dst=/run/password,readonly',
        'redis:7.4-alpine', 'sh', '-ec',
        'REDISCLI_AUTH="$(cat /run/password)" redis-cli -h cfmeet-calling-redis ping')


def provision(sha, run_id):
    import yaml
    identity()
    regression()
    assert shutil.disk_usage('/').free >= 25 * 1024**3, 'Disk reserve insufficient'
    available = int(re.search(r'MemAvailable:\s+(\d+)', Path('/proc/meminfo').read_text())[1])
    assert available >= 3 * 1024**2, 'Memory reserve insufficient'
    for n in ('calling-capture-redis-password', 'minio-egress-access-key', 'minio-egress-secret',
              'minio-service-access-key', 'calling-artifact-store-secret',
              'calling-livekit-api-key', 'calling-livekit-api-secret'):
        assert secret(n), 'Required existing credential missing'
    evidence_dir = ROOT / 'production-evidence' / run_id
    evidence_dir.mkdir(parents=True, exist_ok=False, mode=0o700)
    for n in ('cfmeet-calling', 'lk-server', 'cfmeet-calling-redis', 'cfmeet-calling-artifacts'):
        write_private(evidence_dir / (n+'.inspect.json'), json.dumps(inspect(n)))
    config_path = Path('/opt/livekit/livekit.yaml')
    original = config_path.read_text()
    before = yaml.safe_load(original)
    assert env_of(inspect('lk-server')).keys().isdisjoint({'LIVEKIT_CONFIG', 'LIVEKIT_KEYS', 'REDIS_HOST', 'REDIS_PASSWORD'})
    assert '/etc/livekit.yaml' in inspect('lk-server')['Config']['Cmd']
    assert any(m['Source'] == str(config_path) and m['Destination'] == '/etc/livekit.yaml'
               for m in inspect('lk-server')['Mounts'])
    desired = {'address': 'cfmeet-calling-redis:6379', 'password': secret('calling-capture-redis-password')}
    if before.get('redis') != desired:
        assert 'redis' not in before, 'Existing Redis config differs; refusing overwrite'
        assert not any(int(r.get('numParticipants', r.get('num_participants', 0))) for r in current_rooms()), 'Active participants: restart deferred'
        write_private(evidence_dir / 'livekit.yaml.before', original)
        changed = original.rstrip() + '\nredis:\n  address: cfmeet-calling-redis:6379\n  password: ' + json.dumps(desired['password']) + '\n'
        after = yaml.safe_load(changed)
        assert {k:v for k,v in after.items() if k != 'redis'} == before
        try:
            # Write through the existing inode: LiveKit bind-mounts this individual file.
            config_path.write_text(changed)
            run('docker', 'restart', 'lk-server')
            for attempt in range(30):
                try:
                    current_rooms()
                    break
                except Exception:
                    time.sleep(2)
            else:
                raise RuntimeError('LiveKit failed restart validation')
            redis_ping_from('lk-server')
            regression()
        except Exception:
            config_path.write_text(original)
            run('docker', 'restart', 'lk-server')
            print('LIVEKIT_ROLLBACK: previous configuration restored')
            raise
    else:
        redis_ping_from('lk-server')
    # Never alter API keys, media ports, certificates, or original Docker settings.
    print('LIVEKIT_REDIS: authenticated connectivity PASS; previous keys/config preserved')
    egress_config = dict(api_key=secret('calling-livekit-api-key'),
        api_secret=secret('calling-livekit-api-secret'), ws_url='wss://api.cfmeet.com:8446',
        health_port=9091, prometheus_port=9092, logging={'level':'info'}, redis=desired,
        storage={'s3':dict(access_key=secret('minio-egress-access-key'), secret=secret('minio-egress-secret'),
            region='us-east-1', endpoint='http://cfmeet-calling-artifacts:9000',
            bucket='cfmeet-calling-artifacts', force_path_style=True)})
    egress_path = ROOT / 'egress' / 'production.yaml'
    if subprocess.run(['docker','container','inspect',EGRESS], capture_output=True).returncode == 0:
        assert egress_path.exists() and yaml.safe_load(egress_path.read_text()) == egress_config, 'Existing Egress differs; preserved'
        assert inspect(EGRESS)['State']['Running']
    else:
        write_private(egress_path, json.dumps(egress_config))
        # The image runs as egress (uid 1000). Configuration remains owner-only.
        egress_path.chown(1000, 1000) if hasattr(egress_path, 'chown') else os.chown(egress_path, 1000, 1000)
        run('docker', 'pull', 'livekit/egress:v1.10.0')
        pinned = json.loads(run('docker','image','inspect','livekit/egress:v1.10.0'))[0]['RepoDigests'][0]
        run('docker','run','-d','--name',EGRESS,'--restart','unless-stopped',
            '--network',NET,'--memory','2g','--cpus','2','--shm-size','256m',
            '--tmpfs','/tmp:rw,nosuid,size=512m,uid=1000,gid=1000',
            '--log-opt','max-size=10m','--log-opt','max-file=3',
            '--health-cmd','curl -fsS --max-time 5 http://127.0.0.1:9091/ || exit 1',
            '--health-interval','30s','--health-timeout','8s','--health-retries','3',
            '-e','EGRESS_CONFIG_FILE=/etc/egress/production.yaml',
            '--mount',f'type=bind,src={egress_path},dst=/etc/egress/production.yaml,readonly',pinned)
    for _ in range(45):
        try:
            assert inspect(EGRESS)['State']['Running']
            run('docker','run','--rm','--network',NET,'curlimages/curl:8.12.1',
                '-fsS','--max-time','5','http://'+EGRESS+':9091/')
            break
        except Exception:
            time.sleep(2)
    else:
        raise RuntimeError('Egress readiness failed; existing Calling flags preserved')
    assert not inspect(EGRESS)['HostConfig']['PortBindings']
    redis_ping_from(EGRESS)
    capture_env = {
        'CALLING_CAPTURE_PROVIDER':'livekit-egress', 'CALLING_CAPTURE_STAGING_PREFIX':'capture-staging',
        'CALLING_ARTIFACT_STORAGE_ENDPOINT':'http://cfmeet-calling-artifacts:9000',
        'CALLING_ARTIFACT_STORAGE_REGION':'us-east-1',
        'CALLING_ARTIFACT_STORAGE_BUCKET':'cfmeet-calling-artifacts',
        'CALLING_ARTIFACT_STORAGE_ACCESS_KEY_ID':secret('minio-service-access-key'),
        'CALLING_ARTIFACT_STORAGE_SECRET_ACCESS_KEY_FILE':'/run/secrets/calling-artifact-store-secret',
        'CALLING_ARTIFACT_STORAGE_PATH_STYLE':'true',
        # AES256 intentionally fails closed until MinIO has a functioning KMS.
        'CALLING_ARTIFACT_STORAGE_SERVER_SIDE_ENCRYPTION':'AES256',
    }
    capture_path = ROOT / 'calling-capture.production.env'
    if capture_path.exists():
        assert capture_path.read_text() == ''.join(k+'='+v+'\n' for k,v in capture_env.items()), 'Existing capture env differs; preserved'
    else:
        write_private(capture_path, ''.join(k+'='+v+'\n' for k,v in capture_env.items()))
    # Environment file is a deployment input; its presence is never treated as runtime proof.
    record = dict(schema=1, git_sha=sha, run_id=run_id, timestamp=int(time.time()),
        status='INFRASTRUCTURE_ONLY_NOT_CERTIFIED', rollback_reference=str(evidence_dir),
        livekit_redis=True, egress_ready=True, capture_env_created=True,
        images={n:inspect(n)['Image'] for n in ('lk-server',EGRESS,'cfmeet-calling','cfmeet-calling-artifacts')},
        acceptance={name:'NOT_RUN' for name in REQUIRED})
    write_private(ROOT / 'calling-production-evidence.json', json.dumps(record, indent=2))
    regression()
    print(json.dumps(record))


def validate_acceptance(evidence, now, images):
    assert evidence.get('schema') == 1
    assert evidence.get('status') == 'PRODUCTION_CERTIFIED'
    assert 0 <= now-evidence['timestamp'] <= 86400, 'Acceptance evidence expired'
    assert evidence['images'] == images, 'Acceptance image mismatch'
    assert all(evidence['acceptance'].get(name) == 'PASS' for name in REQUIRED), 'Required acceptance incomplete'
    assert re.fullmatch('[a-f0-9]{40}', evidence['git_sha'])
    assert evidence.get('rollback_reference'), 'Rollback reference missing'


def activate():
    evidence = json.loads((ROOT/'calling-production-evidence.json').read_text())
    images = {n:inspect(n)['Image'] for n in ('lk-server',EGRESS,'cfmeet-calling','cfmeet-calling-artifacts')}
    validate_acceptance(evidence, int(time.time()), images)
    raise RuntimeError('Runtime activation requires completed client acceptance and reviewed clone transaction')


if __name__ == '__main__':
    os.umask(0o077)
    try:
        operation, sha, run_id = sys.argv[1:]
        if operation == 'inspect':
            audit()
        elif operation == 'provision':
            provision(sha, run_id)
        elif operation == 'activate':
            activate()
        else:
            raise RuntimeError('Operation not yet implemented; activation requires real acceptance evidence')
    except Exception as exc:
        # Avoid exception details from network libraries leaking signed URLs.
        print('PRODUCTION_OPERATION_FAILED:', type(exc).__name__)
        sys.exit(1)
