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
        if name == EGRESS:
            logs = run('docker','logs','--tail','30',EGRESS)
            print(json.dumps({'egress_user':cfg['User'],'exit_code':c['State']['ExitCode'],
                'error_classes':[needle for needle in ('permission denied','read-only file system',
                    'connection refused','no such file','failed to','error') if needle in logs.lower()]}))
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
    for p in Path('/etc/nginx/sites-enabled').glob('*'):
        if p.is_file() and 'cfmeet' in p.read_text():
            print(json.dumps({'nginx_cfmeet_site':str(p.resolve()),'routing_lines':[
                line.strip() for line in p.read_text().splitlines()
                if re.match(r'\s*(server_name|listen|location|include|proxy_pass)\s',line)]}))
    c = inspect('lk-server')
    print(json.dumps({'livekit_command': c['Config']['Cmd'],
                      'livekit_entrypoint': c['Config']['Entrypoint']}))
    processes = run('ps','-eo','etimes,comm,args').splitlines()
    operations = []
    for line in processes:
        parts = line.split(None,2)
        if len(parts)==3 and parts[1]=='docker':
            operations.append({'elapsed_seconds':int(parts[0]),
                'kind':'egress_image_pull' if parts[2].startswith('docker pull livekit/egress:') else 'docker_operation'})
    print(json.dumps({'in_progress_docker_operations':operations}))


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
    result = run('docker', 'run', '--rm', '--network', 'container:'+container,
        '--mount', f'type=bind,src={SECRETS}/calling-capture-redis-password,dst=/run/password,readonly',
        'redis:7.4-alpine', 'sh', '-ec',
        'REDISCLI_AUTH="$(cat /run/password)" redis-cli -h cfmeet-calling-redis ping')
    assert result == 'PONG', 'Redis authentication failed'


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
    evidence_dir = ROOT / 'production-evidence' / (run_id+'-'+str(time.time_ns()))
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
    run('docker', 'pull', 'livekit/egress:v1.10.0')
    pinned = json.loads(run('docker','image','inspect','livekit/egress:v1.10.0'))[0]['RepoDigests'][0]
    uid = int(run('docker','run','--rm','--network','none','--entrypoint','id',pinned,'-u'))
    gid = int(run('docker','run','--rm','--network','none','--entrypoint','id',pinned,'-g'))
    if subprocess.run(['docker','container','inspect',EGRESS], capture_output=True).returncode == 0:
        assert egress_path.exists() and yaml.safe_load(egress_path.read_text()) == egress_config, 'Existing Egress differs; preserved'
        assert inspect(EGRESS)['Config']['Image'] == pinned, 'Existing Egress image differs; preserved'
        os.chown(egress_path,uid,gid)
        run('docker','restart',EGRESS)
    else:
        write_private(egress_path, json.dumps(egress_config))
        # Resolve the pinned image's actual egress uid; base images can already own uid 1000.
        os.chown(egress_path,uid,gid)
        run('docker','run','-d','--name',EGRESS,'--restart','unless-stopped',
            '--network',NET,'--memory','2g','--cpus','2','--shm-size','256m',
            '--tmpfs',f'/tmp:rw,nosuid,size=512m,uid={uid},gid={gid}',
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
        existing = dict(line.split('=',1) for line in capture_path.read_text().splitlines() if '=' in line)
        assert all(existing.get(k)==v for k,v in capture_env.items()), 'Existing capture env differs; preserved'
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


def secure_storage_ingress(run_id):
    identity()
    store = inspect('cfmeet-calling-artifacts')
    assert not store['HostConfig']['PortBindings']
    ip = store['NetworkSettings']['Networks'][NET]['IPAddress']
    import ipaddress
    assert ipaddress.ip_address(ip).is_private
    site = Path('/etc/nginx/sites-enabled/api.cfmeet.com').resolve()
    assert site == Path('/etc/nginx/sites-available/api.cfmeet.com')
    text = site.read_text()
    existing_include = 'include /etc/nginx/snippets/cfmeet-calling-routes.conf;'
    assert text.count(existing_include) == 1 and 'listen 443 ssl' in text
    routes = Path('/etc/nginx/snippets/cfmeet-calling-routes.conf')
    previous = routes.read_text()
    snippet = Path('/etc/nginx/snippets/cfmeet-calling-artifacts.conf')
    contents = ('# Signed read-only S3 path; never log bearer query signatures.\n'
        'location ^~ /cfmeet-calling-artifacts/ {\n'
        '    limit_except GET HEAD { deny all; }\n'
        '    access_log off;\n'
        '    proxy_buffering off;\n'
        '    proxy_set_header Host $http_host;\n'
        '    proxy_set_header X-Forwarded-Proto https;\n'
        '    add_header Cache-Control "private, no-store" always;\n'
        '    proxy_pass http://'+ip+':9000;\n}\n')
    old_snippet = snippet.read_text() if snippet.exists() else None
    include = 'include /etc/nginx/snippets/cfmeet-calling-artifacts.conf;'
    backup = ROOT/'production-evidence'/('storage-ingress-'+run_id)
    backup.mkdir(parents=True,exist_ok=False,mode=0o700)
    write_private(backup/'calling-routes.before',previous)
    if old_snippet is not None:
        write_private(backup/'calling-artifacts.before',old_snippet)
    try:
        write_private(snippet,contents)
        if include not in previous:
            routes.write_text(previous.rstrip()+'\n'+include+'\n')
        run('nginx','-t')
        run('systemctl','reload','nginx')
        try:
            urllib.request.urlopen('https://api.cfmeet.com/cfmeet-calling-artifacts/unsigned-probe',timeout=15)
        except urllib.error.HTTPError as exc:
            assert exc.code == 403, 'Anonymous storage access must be denied; received HTTP '+str(exc.code)
        else:
            raise AssertionError('Unsigned storage access was permitted')
        regression()
        capture_path = ROOT/'calling-capture.production.env'
        assert capture_path.exists()
        capture_text = capture_path.read_text()
        public = 'CALLING_ARTIFACT_STORAGE_PUBLIC_ENDPOINT=https://api.cfmeet.com\n'
        if 'CALLING_ARTIFACT_STORAGE_PUBLIC_ENDPOINT=' not in capture_text:
            write_private(capture_path,capture_text+public)
        else:
            assert public in capture_text
    except Exception:
        routes.write_text(previous)
        if old_snippet is not None:
            write_private(snippet,old_snippet)
        elif snippet.exists():
            snippet.unlink()
        run('nginx','-t')
        run('systemctl','reload','nginx')
        raise
    print('STORAGE_HTTPS: read-only private-bucket route installed; anonymous GET denied; no new ports')
    configure_storage_operations()


def configure_storage_operations():
    # Retention of final recordings remains owned by Calling's database-backed worker.
    # Bucket lifecycle only sweeps abandoned temporary uploads, never final artifacts.
    store = inspect('cfmeet-calling-artifacts')
    ip = store['NetworkSettings']['Networks'][NET]['IPAddress']
    mc = ROOT/'storage'/'mc'
    assert mc.is_file(), 'Existing verified MinIO CLI missing'
    env = dict(os.environ, MC_HOST_capture='http://'+secret('minio-admin-user')+':'+
        secret('minio-admin-password')+'@'+ip+':9000')
    def command(*args, allow_no_lifecycle=False):
        response = subprocess.run([str(mc),*args],env=env,capture_output=True,text=True)
        if allow_no_lifecycle and response.returncode and any(x in response.stdout+response.stderr
                for x in ('NoSuchLifecycleConfiguration','No lifecycle configuration')):
            return ''
        assert response.returncode==0, 'MinIO lifecycle operation failed'
        return response.stdout
    rules = command('ilm','rule','ls','--json','capture/cfmeet-calling-artifacts',allow_no_lifecycle=True)
    if 'cfmeet-capture-staging-cleanup' not in rules:
        command('ilm','rule','add','--id','cfmeet-capture-staging-cleanup','--prefix','capture-staging/',
            '--expire-days','1','capture/cfmeet-calling-artifacts')
    monitor = ROOT/'storage'/'monitor-capacity.py'
    write_private(monitor, '''import json, shutil, sys\nfrom pathlib import Path\nusage=shutil.disk_usage('/var/lib/cfmeet-artifacts')\nfree_gib=round(usage.free/1024**3,2)\npercent=round(100*usage.used/usage.total,1)\nstatus='ALERT' if free_gib<25 or percent>=85 else 'OK'\nprint(json.dumps(dict(service='cfmeet-calling-artifacts',status=status,free_gib=free_gib,used_percent=percent)))\nsys.exit(1 if status=='ALERT' else 0)\n''')
    write_private(Path('/etc/systemd/system/cfmeet-capture-capacity.service'),
        '[Unit]\nDescription=CF Meet private capture capacity check\n[Service]\nType=oneshot\n'
        'ExecStart=/usr/bin/python3 '+str(monitor)+'\nNoNewPrivileges=true\nProtectSystem=strict\n'
        'ProtectHome=true\nPrivateTmp=true\nMemoryMax=64M\nCPUQuota=10%\n')
    write_private(Path('/etc/systemd/system/cfmeet-capture-capacity.timer'),
        '[Unit]\nDescription=Check CF Meet capture capacity every five minutes\n[Timer]\n'
        'OnBootSec=2min\nOnUnitActiveSec=5min\nPersistent=true\n[Install]\nWantedBy=timers.target\n')
    run('systemctl','daemon-reload')
    run('systemctl','enable','--now','cfmeet-capture-capacity.timer')
    run('systemctl','start','cfmeet-capture-capacity.service')
    print('STORAGE_OPERATIONS: staging expiry 1 day; five-minute capacity journal checks enabled')


def validate_acceptance(evidence, now, images):
    assert evidence.get('schema') == 1
    assert evidence.get('status') == 'PRODUCTION_CERTIFIED'
    assert 0 <= now-evidence['timestamp'] <= 86400, 'Acceptance evidence expired'
    assert evidence['images'] == images, 'Acceptance image mismatch'
    assert all(evidence['acceptance'].get(name) == 'PASS' for name in REQUIRED), 'Required acceptance incomplete'
    assert re.fullmatch('[a-f0-9]{40}', evidence['git_sha'])
    assert evidence.get('rollback_reference'), 'Rollback reference missing'
    receipts = evidence.get('receipts', {})
    for name in REQUIRED:
        receipt = receipts.get(name, {})
        path = Path(receipt.get('path', ''))
        assert path.is_absolute() and path.is_relative_to(ROOT/'production-evidence'), 'Acceptance receipt outside protected evidence directory'
        assert path.is_file() and not path.is_symlink(), 'Missing production acceptance receipt'
        assert hashlib.sha256(path.read_bytes()).hexdigest() == receipt.get('sha256'), 'Production acceptance receipt changed'
        result = json.loads(path.read_text())
        assert result.get('scenario') == name and result.get('result') == 'PASS'
        assert result.get('git_sha') == evidence['git_sha'] and result.get('images') == images
        assert result.get('started_at') and result.get('finished_at') and result.get('assertions'), 'Receipt lacks test execution details'


def activate():
    evidence = json.loads((ROOT/'calling-production-evidence.json').read_text())
    images = {n:inspect(n)['Image'] for n in ('lk-server',EGRESS,'cfmeet-calling','cfmeet-calling-artifacts')}
    validate_acceptance(evidence, int(time.time()), images)
    identity()
    regression()
    capture = dict(line.split('=',1) for line in (ROOT/'calling-capture.production.env').read_text().splitlines())
    capture.update({k:'true' for k in ('CALLING_NEW_CALLS_ENABLED','CALLING_APP_CALLING_ENABLED',
        'ENTERPRISE_CALLING_ENABLED','VOICEMAIL_ENABLED','CALL_RECORDING_ENABLED')})
    capture.update({k:'false' for k in ('CALLING_PUBLIC_OUTBOUND_ENABLED','CALLING_PUBLIC_INBOUND_ENABLED',
        'CALLING_CALLBACK_RELAY_ENABLED','CALLING_SYNTHETIC_INTERCONNECT_ENABLED')})
    recreate_calling(capture)


class DockerConnection(http.client.HTTPConnection):
    def __init__(self):
        super().__init__('localhost', timeout=90)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect('/var/run/docker.sock')


def docker_api(method, path, payload=None):
    conn = DockerConnection()
    try:
        conn.request(method, '/v1.45'+path,
            body=json.dumps(payload).encode() if payload is not None else None,
            headers={'Content-Type':'application/json'})
        response = conn.getresponse()
        data = response.read()
        assert response.status < 300, 'Docker transaction rejected'
        return json.loads(data) if data else None
    finally:
        conn.close()


def clone_payload(c, overrides):
    payload = copy.deepcopy(c['Config'])
    payload['Image'] = c['Image']  # immutable running image; do not resolve a moving tag
    env = env_of(c)
    env.update(overrides)
    payload['Env'] = [k+'='+v for k,v in env.items()]
    host = copy.deepcopy(c['HostConfig'])
    assert not host.get('AutoRemove'), 'Auto-removal incompatible with rollback'
    assert not host.get('PortBindings'), 'Calling must remain private'
    # Persist all original mounts, restart, resources, security and startup settings.
    destination = '/run/secrets/calling-artifact-store-secret'
    existing = [m for m in c['Mounts'] if m['Destination'] == destination]
    if not existing:
        host.setdefault('Mounts', []).append(dict(Type='bind',
            Source=str(SECRETS/'calling-artifact-store-secret'), Target=destination, ReadOnly=True))
    else:
        assert len(existing) == 1 and not existing[0]['RW']
        assert existing[0]['Source'] == str(SECRETS/'calling-artifact-store-secret')
    payload['HostConfig'] = host
    endpoints = {}
    for name, ep in c['NetworkSettings']['Networks'].items():
        assert not ep.get('IPAMConfig'), 'Static-IP topology requires explicit migration; preserved'
        endpoints[name] = {k:copy.deepcopy(ep[k]) for k in ('Aliases','Links','DriverOpts') if ep.get(k)}
        if 'Aliases' in endpoints[name]:
            endpoints[name]['Aliases'] = [a for a in endpoints[name]['Aliases'] if a not in (c['Id'],c['Id'][:12])]
    payload['NetworkingConfig'] = {'EndpointsConfig':endpoints}
    return payload


def recreate_calling(overrides, require_acceptance=True):
    c = inspect('cfmeet-calling')
    if all(env_of(c).get(k) == v for k,v in overrides.items()):
        regression()
        print('CALLING_RUNTIME: already configured')
        return
    payload = clone_payload(c, overrides)
    backup = 'cfmeet-calling-rollback-'+str(int(time.time()))
    write_private(ROOT/'calling-activation-rollback.json', json.dumps({'container':backup,'original':c}))
    # Rename before stopping; never delete a previous rollback container.
    run('docker','rename','cfmeet-calling',backup)
    try:
        run('docker','stop','--time','30',backup)
        docker_api('POST','/containers/create?name=cfmeet-calling',payload)
        docker_api('POST','/containers/cfmeet-calling/start')
        for _ in range(30):
            try:
                regression()
                break
            except Exception:
                time.sleep(2)
        else:
            raise RuntimeError('Calling activation readiness failed')
        deployed = inspect('cfmeet-calling')
        assert all(env_of(deployed).get(k) == v for k,v in overrides.items())
        assert set(deployed['NetworkSettings']['Networks']) == set(c['NetworkSettings']['Networks'])
        assert all(any(x['Source']==m['Source'] and x['Destination']==m['Destination'] and x['RW']==m['RW']
            for x in deployed['Mounts']) for m in c['Mounts'])
        assert deployed['Config']['Cmd'] == c['Config']['Cmd']
        assert deployed['Config']['Entrypoint'] == c['Config']['Entrypoint']
        assert not deployed['HostConfig']['PortBindings']
        # Re-run real acceptance after activation; failure restores original container.
        if require_acceptance:
            verify(str(int(time.time())))
            latest = json.loads((ROOT/'calling-production-evidence.json').read_text())
            validate_acceptance(latest, int(time.time()),
                {n:inspect(n)['Image'] for n in ('lk-server',EGRESS,'cfmeet-calling','cfmeet-calling-artifacts')})
    except Exception:
        rollback_calling()
        raise
    print('CALLING_RUNTIME_UPDATED: flags/mounts/networks verified; rollback retained '+backup)


def configure_capture():
    identity()
    c = inspect('cfmeet-calling')
    old_env = env_of(c)
    assert old_env.get('CALLING_NEW_CALLS_ENABLED','false') == 'false'
    assert old_env.get('VOICEMAIL_ENABLED','false') == 'false'
    assert old_env.get('CALL_RECORDING_ENABLED','false') == 'false'
    capture = dict(line.split('=',1) for line in (ROOT/'calling-capture.production.env').read_text().splitlines())
    assert set(capture).issubset({'CALLING_CAPTURE_PROVIDER','CALLING_CAPTURE_STAGING_PREFIX',
        'CALLING_ARTIFACT_STORAGE_ENDPOINT','CALLING_ARTIFACT_STORAGE_PUBLIC_ENDPOINT',
        'CALLING_ARTIFACT_STORAGE_REGION','CALLING_ARTIFACT_STORAGE_BUCKET',
        'CALLING_ARTIFACT_STORAGE_ACCESS_KEY_ID','CALLING_ARTIFACT_STORAGE_SECRET_ACCESS_KEY_FILE',
        'CALLING_ARTIFACT_STORAGE_PATH_STYLE','CALLING_ARTIFACT_STORAGE_SERVER_SIDE_ENCRYPTION'})
    # Capture settings are installed with all existing activation switches preserved.
    recreate_calling(capture,require_acceptance=False)
    deployed = env_of(inspect('cfmeet-calling'))
    assert all(deployed.get(k)==v for k,v in old_env.items() if k in SAFE_ENV and k not in capture)
    print('CAPTURE_RUNTIME_CONFIGURED: feature flags preserved; capture not production-certified')


def rollback_calling():
    record = json.loads((ROOT/'calling-activation-rollback.json').read_text())
    backup = record['container']
    assert backup.startswith('cfmeet-calling-rollback-')
    assert inspect(backup)['Id'] == record['original']['Id'], 'Rollback identity mismatch'
    # Retain failed replacement too; no deletion of original/rollback containers.
    if subprocess.run(['docker','inspect','cfmeet-calling'], capture_output=True).returncode == 0:
        run('docker','stop','--time','15','cfmeet-calling')
        run('docker','rename','cfmeet-calling','cfmeet-calling-failed-'+str(int(time.time())))
    run('docker','rename',backup,'cfmeet-calling')
    run('docker','start','cfmeet-calling')
    regression()
    print('CALLING_ROLLBACK: original container restored')


def verify(run_id):
    identity()
    regression()
    redis_ping_from('lk-server')
    redis_ping_from(EGRESS)
    out = ROOT/'production-evidence'/('verify-'+run_id)
    out.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chown(out,10001,10001)
    # Only the probe's explicit, existing service credentials enter this disposable client.
    command = ['docker','run','--rm','--name','cfmeet-production-media-probe',
        '--network',NET,'--user','10001:10001','--read-only','--memory','768m','--cpus','1',
        '--tmpfs','/tmp:rw,nosuid,size=512m,uid=10001,gid=10001',
        '--mount',f'type=bind,src={out},dst=/result',
        '--mount',f'type=bind,src={Path(__file__).with_name("media_probe.py")},dst=/probe.py,readonly']
    for name in ('calling-livekit-api-key','calling-livekit-api-secret','minio-service-access-key',
                 'calling-artifact-store-secret','minio-egress-access-key','minio-egress-secret'):
        command += ['--mount',f'type=bind,src={SECRETS/name},dst=/run/secrets/{name},readonly']
    command += ['python:3.12-slim','sh','-ec',
        'pip install --disable-pip-version-check --no-cache-dir --quiet --target /tmp/deps livekit==1.1.20 && PYTHONPATH=/tmp/deps python /probe.py']
    probe = subprocess.run(command, capture_output=True, text=True, timeout=600)
    result_path = out/'media-result.json'
    result = json.loads(result_path.read_text()) if result_path.exists() else {
        'error_type':'PROBE_DID_NOT_PRODUCE_RESULTS','checks':{}}
    if probe.returncode:
        # SDK log messages may include connection tokens; only return stable error summaries.
        print('MEDIA_PROBE: incomplete; inspect restricted evidence on VPS')
    for kind in ('track','composite'):
        audio = out/(kind+'.ogg')
        if not audio.exists():
            continue
        try:
            run('docker','run','--rm','--network','none','--user','10001:10001',
                '--memory','128m','--cpus','0.5','--entrypoint','ffmpeg',
                '--mount',f'type=bind,src={out},dst=/result',inspect(EGRESS)['Image'],
                '-v','error','-i','/result/'+kind+'.ogg','-ac','1','-ar','48000',
                '-f','s16le','/result/'+kind+'.pcm')
            import array
            pcm = array.array('h',(out/(kind+'.pcm')).read_bytes())
            assert len(pcm)>=3*48000 and sum(int(x)**2 for x in pcm)/len(pcm)>100**2
            result['checks'][kind+'_decoded_nonsilent_audio'] = 'PASS'
        except Exception:
            result['checks'][kind+'_decoded_nonsilent_audio'] = 'FAIL'
    result['git_sha'] = sys.argv[2]
    result['timestamp'] = int(time.time())
    result['backup_destination_configured'] = bool(list(ROOT.glob('*backup*.env')))
    result['activation'] = 'BLOCKED_PENDING_FULL_PRODUCTION_ACCEPTANCE'
    write_private(out/'verification.json',json.dumps(result,indent=2))
    print(json.dumps(result))
    audit()
    regression()
    assert probe.returncode==0, 'Real media infrastructure probe failed'


if __name__ == '__main__':
    os.umask(0o077)
    try:
        operation, sha, run_id = sys.argv[1:]
        if operation == 'inspect':
            audit()
        elif operation == 'complete-safe-infrastructure':
            provision(sha, run_id)
            secure_storage_ingress(run_id)
            configure_capture()
            verify(run_id)
        elif operation == 'provision':
            provision(sha, run_id)
        elif operation == 'activate':
            activate()
        elif operation == 'rollback':
            assert inspect('cfmeet-production-backend')['State']['Running']
            rollback_calling()
        elif operation == 'verify':
            verify(run_id)
        elif operation == 'storage-ingress':
            secure_storage_ingress(run_id)
        elif operation == 'configure-capture':
            configure_capture()
        else:
            raise RuntimeError('Operation not yet implemented; activation requires real acceptance evidence')
    except Exception as exc:
        # Avoid exception details from network libraries leaking signed URLs.
        print('PRODUCTION_OPERATION_FAILED:', type(exc).__name__,
              str(exc) if isinstance(exc, (AssertionError, RuntimeError)) else '')
        sys.exit(1)
