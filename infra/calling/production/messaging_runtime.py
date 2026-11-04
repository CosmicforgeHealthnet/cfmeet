"""Prepare independently safe Messaging dependencies; never enable production E2EE.

Uses the existing private MinIO/KMS and Redis. Customer policies/data and existing keys
are preserved. The candidate keeps the EXACT running image; rollback retains its owner.
"""
import base64
import copy
import datetime
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import urllib.error
import urllib.request
from urllib.parse import quote
import production as p
import communications_release as release
import storage_encryption as storage

WORK = p.ROOT / 'messaging'
BUCKET = 'cfmeet-messaging-attachments'
SCANNER = 'cfmeet-messaging-clamav'
# Official Cisco Talos release, 2026-08-07. Resolve and record immutable registry digest.
SCANNER_TAG = 'clamav/clamav:1.5.4'


def protected_file(name, make):
    path = WORK / name
    recovery = p.ROOT / 'key-recovery' / 'messaging' / name
    recovery.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not path.exists() and recovery.exists():
        release.save(path, recovery.read_text(), True)
    if not path.exists():
        release.save(path, make(), True)
    assert path.is_file() and path.stat().st_mode & 0o077 == 0, 'Unsafe Messaging key permissions'
    if recovery.exists():
        assert recovery.read_bytes() == path.read_bytes(), 'Recovery material differs; refusing replacement'
    else:
        p.write_private(recovery, path.read_text())
    return path


def storage_identity():
    user = protected_file('storage-user', lambda: 'cfmeet-msg-' + secrets.token_hex(10))
    password = protected_file('storage-secret', lambda: secrets.token_urlsafe(40))
    storage.mc(storage.NAME, 'mb', '--ignore-existing', 'kms/' + BUCKET)
    storage.mc(storage.NAME, 'anonymous', 'set', 'none', 'kms/' + BUCKET)
    storage.mc(storage.NAME, 'encrypt', 'set', 'sse-s3', 'kms/' + BUCKET)
    policy = {'Version': '2012-10-17', 'Statement': [
        {'Effect': 'Allow', 'Action': ['s3:GetBucketLocation', 's3:ListBucket'],
         'Resource': ['arn:aws:s3:::' + BUCKET]},
        {'Effect': 'Allow', 'Action': ['s3:GetObject', 's3:PutObject', 's3:DeleteObject',
            's3:AbortMultipartUpload', 's3:ListMultipartUploadParts'],
         'Resource': ['arn:aws:s3:::' + BUCKET + '/*']}]}
    policy_path = WORK / 'storage-policy.json'
    p.write_private(policy_path, json.dumps(policy))
    # Never re-enable an identity an operator suspended. Only a precise no-such-user
    # response permits creation; all other failures preserve existing IAM state.
    ip = p.inspect(storage.NAME)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    admin_env = dict(os.environ, MC_HOST_kms='http://' + p.secret('minio-admin-user') + ':' + p.secret('minio-admin-password') + '@' + ip + ':9000')
    info = subprocess.run([str(p.ROOT / 'storage' / 'mc'), '--json', 'admin', 'user', 'info', 'kms', user.read_text()], env=admin_env, capture_output=True, text=True, timeout=40)
    if info.returncode:
        assert 'XMinioAdminNoSuchUser' in info.stdout + info.stderr, 'Cannot establish Messaging storage identity state'
        storage.mc(storage.NAME, 'admin', 'user', 'add', 'kms', user.read_text(), password.read_text())
    else:
        assert json.loads(info.stdout).get('userStatus', '').lower() == 'enabled', 'Existing Messaging storage identity is disabled; preserved'
    storage.mc(storage.NAME, 'admin', 'policy', 'create', 'kms', 'cfmeet-messaging-storage', str(policy_path))
    storage.mc(storage.NAME, 'admin', 'policy', 'attach', 'kms', 'cfmeet-messaging-storage', '--user', user.read_text())
    return user.read_text(), password


def nginx_config(tls_ip, store_ip):
    assert ipaddress.ip_address(tls_ip).is_private and ipaddress.ip_address(store_ip).is_private
    common = ('    access_log off;\n    proxy_http_version 1.1;\n'
        '    proxy_set_header Host $http_host;\n    proxy_set_header X-Forwarded-Proto https;\n'
        '    add_header Cache-Control "private, no-store" always;\n')
    tls = (common + '    proxy_ssl_server_name on;\n    proxy_ssl_name api.cfmeet.com;\n'
        '    proxy_ssl_verify on;\n    proxy_ssl_trusted_certificate /etc/ssl/certs/ca-certificates.crt;\n'
        '    proxy_set_header Upgrade $http_upgrade;\n    proxy_set_header Connection "upgrade";\n'
        '    proxy_read_timeout 3600;\n    proxy_pass https://' + tls_ip + ':8444;\n')
    return ('# Section 8: device-only realtime ingress; Account owns native REST authentication.\n'
        'location = /v1/realtime {\n    limit_except GET { deny all; }\n' + tls + '}\n'
        'location = /v1/realtime/tickets {\n    limit_except POST OPTIONS { deny all; }\n' + tls + '}\n'
        'location ^~ /' + BUCKET + '/ {\n    limit_except GET HEAD PUT OPTIONS { deny all; }\n'
        '    client_max_body_size 64m;\n    proxy_buffering off;\n    proxy_request_buffering off;\n'
        + common + '    proxy_pass http://' + store_ip + ':9000;\n}\n')


def install_ingress(run_id):
    tls = p.inspect('cfmeet-core-tls')['NetworkSettings']['Networks'][p.NET]['IPAddress']
    store = p.inspect(storage.NAME)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    site = Path('/etc/nginx/sites-enabled/api.cfmeet.com').resolve()
    assert site == Path('/etc/nginx/sites-available/api.cfmeet.com')
    before = site.read_text()
    include = 'include /etc/nginx/snippets/cfmeet-messaging.conf;'
    anchor = 'include /etc/nginx/snippets/cfmeet-calling-routes.conf;'
    assert before.count(anchor) == 1 and before.count(include) <= 1
    snippet = Path('/etc/nginx/snippets/cfmeet-messaging.conf')
    old = snippet.read_text() if snippet.exists() else None
    p.write_private(WORK / ('nginx-site-before-' + run_id), before)
    if old is not None: p.write_private(WORK / ('nginx-snippet-before-' + run_id), old)
    try:
        snippet.write_text(nginx_config(tls, store))
        if include not in before: site.write_text(before.replace(anchor, anchor + '\n    ' + include))
        p.run('nginx', '-t')
        p.run('systemctl', 'reload', 'nginx')
    except Exception:
        site.write_text(before)
        if old is not None: snippet.write_text(old)
        else: snippet.write_text('# previous configuration had no Messaging ingress\n')
        p.run('nginx', '-t'); p.run('systemctl', 'reload', 'nginx')
        raise


def scanner():
    names = p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    if SCANNER not in names:
        p.run('docker', 'pull', SCANNER_TAG)
        image = json.loads(p.run('docker', 'image', 'inspect', SCANNER_TAG))[0]
        digest = next(x for x in image['RepoDigests'] if x.startswith('clamav/clamav@sha256:'))
        p.run('docker', 'volume', 'create', 'cfmeet-messaging-clamav-db')
        p.run('docker', 'run', '-d', '--name', SCANNER, '--network', p.NET,
              '--restart', 'unless-stopped', '--memory', '1536m', '--cpus', '1.5',
              '--label', 'com.cfmeet.messaging.scanner=section8',
              '--log-opt', 'max-size=20m', '--log-opt', 'max-file=3',
              '--mount', 'type=volume,src=cfmeet-messaging-clamav-db,dst=/var/lib/clamav', digest)
    c = p.inspect(SCANNER)
    assert c['Config'].get('Labels', {}).get('com.cfmeet.messaging.scanner') == 'section8'
    assert not c['HostConfig']['PortBindings'], 'Scanner must stay private'
    for _ in range(120):
        result = subprocess.run(['docker', 'exec', SCANNER, 'clamdscan', '--ping=1'], capture_output=True)
        if result.returncode == 0: break
        time.sleep(2)
    else: raise RuntimeError('ClamAV readiness failed; no scan result is certified')
    version = p.run('docker', 'exec', SCANNER, 'clamdscan', '--version')
    # Bundled engine readiness alone is insufficient: require recent signature data.
    date = version.rsplit('/', 1)[-1].strip()
    stamp = datetime.datetime.strptime(date, '%a %b %d %H:%M:%S %Y').replace(tzinfo=datetime.timezone.utc)
    age = (datetime.datetime.now(datetime.timezone.utc) - stamp).total_seconds()
    assert 0 <= age <= 72 * 3600, 'ClamAV signatures need refresh; managed scanning remains unaccepted'
    # Exercise the actual private TCP service, including its malware refusal. EICAR
    # is the standard harmless antivirus test string; no customer attachment is used.
    probe = r'''
import socket, struct
def scan(data):
    with socket.create_connection(('cfmeet-messaging-clamav', 3310), timeout=10) as s:
        s.sendall(b'zINSTREAM\0' + struct.pack('!I', len(data)) + data + b'\0\0\0\0')
        result = b''
        while b'\0' not in result:
            chunk = s.recv(4096)
            if not chunk: break
            result += chunk
        return result
assert b': OK' in scan(b'CF Meet isolated scanner acceptance')
assert b'FOUND' in scan(b'X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*')
'''
    checked = subprocess.run(['docker', 'run', '--rm', '--network', p.NET,
        '--memory', '128m', '--cpus', '1', 'python:3.12-slim', 'python', '-c', probe],
        capture_output=True, timeout=40)
    assert checked.returncode == 0, 'Live ClamAV clean/EICAR acceptance failed'
    return {'imageId': c['Image'], 'registryDigests': json.loads(p.run('docker', 'image', 'inspect', c['Image']))[0].get('RepoDigests', []),
            'signatureAgeSeconds': int(age), 'privateTcpCleanScan': 'PASS', 'eicarDenied': 'PASS'}


def runtime_overrides(env, user, redis_password):
    assert env.get('CF_RUNTIME_ENVIRONMENT') == 'production'
    assert env.get('MESSAGING_E2EE_DEVICES_ENABLED', 'false') == 'false', 'Production cryptographic guard required'
    assert not env.get('MESSAGING_MANAGED_KEYRING_FILE') or env['MESSAGING_MANAGED_KEYRING_FILE'] == '/run/messaging/managed.ring'
    return {
        'MESSAGING_E2EE_DEVICES_ENABLED': 'false',
        'MESSAGING_MANAGED_KEYRING_FILE': '/run/messaging/managed.ring',
        'MESSAGING_ATTACHMENTS_S3_ENDPOINT': 'http://cfmeet-calling-artifacts:9000',
        'MESSAGING_ATTACHMENTS_S3_PUBLIC_ENDPOINT': 'https://api.cfmeet.com',
        'MESSAGING_ATTACHMENTS_S3_REGION': 'us-east-1', 'MESSAGING_ATTACHMENTS_S3_BUCKET': BUCKET,
        'MESSAGING_ATTACHMENTS_S3_ACCESS_KEY_ID': user,
        'MESSAGING_ATTACHMENTS_S3_SECRET_ACCESS_KEY_FILE': '/run/messaging/storage.secret',
        'MESSAGING_ATTACHMENTS_S3_PATH_STYLE': 'true',
        'MESSAGING_ATTACHMENTS_S3_SERVER_SIDE_ENCRYPTION': 'AES256',
        'MESSAGING_REALTIME_REDIS_URL': 'redis://:' + quote(redis_password, safe='') + '@cfmeet-calling-redis:6379/0',
        'MESSAGING_CLAMAV_ADDRESS': SCANNER + ':3310',
        'MESSAGING_METRICS_ADDRESS': '0.0.0.0:9197',
        'MESSAGING_KT_CHECKPOINT_FILE': '/var/lib/messaging-kt/checkpoint.json',
    }


def storage_probe(run_id):
    ip = p.inspect(storage.NAME)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    user, password = (WORK / 'storage-user').read_text(), (WORK / 'storage-secret').read_text()
    env = dict(os.environ, MC_HOST_msg='http://' + user + ':' + password + '@' + ip + ':9000',
        MC_HOST_msgpublic='https://' + user + ':' + password + '@api.cfmeet.com')
    client = str(p.ROOT / 'storage' / 'mc')
    key = 'section8-probes/' + run_id
    data = secrets.token_urlsafe(48)
    def command(*args, data=None, success=True):
        r = subprocess.run([client, *args], env=env, input=data, capture_output=True, text=True, timeout=40)
        if success: assert r.returncode == 0, 'Messaging storage probe failed: ' + args[0]
        return r
    try:
        command('pipe', 'msg/' + BUCKET + '/' + key, data=data)
        assert 'AES256' in command('stat', '--json', 'msg/' + BUCKET + '/' + key).stdout
        assert command('cat', 'msg/' + BUCKET + '/' + key).stdout == data
        assert command('ls', 'msg/cfmeet-calling-artifacts', success=False).returncode != 0, 'Cross-service storage scope leak'
        unsigned = 'https://api.cfmeet.com/' + BUCKET + '/' + key
        try: urllib.request.urlopen(unsigned, timeout=15)
        except urllib.error.HTTPError as error: assert error.code == 403
        else: raise AssertionError('Unsigned Messaging object was exposed')
        assert command('cat', 'msgpublic/' + BUCKET + '/' + key).stdout == data
        shared = command('share', 'download', '--json', '--expire', '4s', 'msgpublic/' + BUCKET + '/' + key).stdout
        def urls(value):
            if isinstance(value, str) and value.startswith('https://api.cfmeet.com/' + BUCKET + '/'): yield value
            elif isinstance(value, dict):
                for item in value.values(): yield from urls(item)
            elif isinstance(value, list):
                for item in value: yield from urls(item)
        grants = [url for line in shared.splitlines() for url in urls(json.loads(line))]
        assert len(grants) == 1, 'Expected one bounded object grant'
        with urllib.request.urlopen(grants[0], timeout=15) as response:
            assert response.read().decode() == data
        time.sleep(5)
        try: urllib.request.urlopen(grants[0], timeout=15)
        except urllib.error.HTTPError as error: assert error.code == 403
        else: raise AssertionError('Expired Messaging storage grant remained valid')
    finally:
        command('rm', 'msg/' + BUCKET + '/' + key)
    assert command('stat', 'msg/' + BUCKET + '/' + key, success=False).returncode != 0
    return {'encryptedUploadDownload': 'PASS', 'privateBucket': 'PASS', 'signedHttps': 'PASS',
            'expiredGrantDenied': 'PASS', 'callingBucketDenied': 'PASS', 'physicalProbeDeletion': 'PASS'}


def configure(sha, run_id):
    import messaging_release
    assert any((p.ROOT / 'communications').glob('messaging-baseline-*.json')), 'Published baseline required'
    p.identity()
    WORK.mkdir(parents=True, exist_ok=True, mode=0o700)
    old = p.inspect('cfmeet-messaging'); env = p.env_of(old)
    assert env.get('CF_RUNTIME_ENVIRONMENT') == 'production' and env.get('MESSAGING_E2EE_DEVICES_ENABLED', 'false') == 'false'
    assert not old['HostConfig']['PortBindings']
    baseline = messaging_release.database_summary()
    assert (WORK / 'managed.ring').exists() or baseline['conversations'] == 0, 'Cannot invent a keyring for existing content'
    available = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:')))
    assert available >= 3 * 1024 * 1024, 'Insufficient memory for bounded scanner/candidate acceptance'
    managed = protected_file('managed.ring', lambda: 'messaging-20261105 ' + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip('=') + '\n')
    user, password = storage_identity()
    scanner_evidence = scanner()
    checkpoint = WORK / 'kt-checkpoints'
    checkpoint.mkdir(exist_ok=True, mode=0o700); os.chown(checkpoint, 10001, 10001)
    overrides = runtime_overrides(env, user, p.secret('calling-capture-redis-password'))
    cfg = release.clone(old, 'cfmeet-messaging', old['Image'], overrides, False)
    release.add_mount(cfg, managed, '/run/messaging/managed.ring')
    release.add_mount(cfg, password, '/run/messaging/storage.secret')
    mounts = cfg['HostConfig'].setdefault('Mounts', [])
    if not any(m.get('Target') == '/var/lib/messaging-kt' for m in mounts):
        mounts.append({'Type': 'bind', 'Source': str(checkpoint), 'Target': '/var/lib/messaging-kt', 'ReadOnly': False})
    cfg['HostConfig']['Memory'] = old['HostConfig'].get('Memory') or 1024 * 1024 * 1024
    cfg['HostConfig']['NanoCpus'] = old['HostConfig'].get('NanoCpus') or 2000000000
    cfg['HostConfig']['LogConfig'] = {'Type': 'json-file', 'Config': {'max-size': '20m', 'max-file': '5'}}
    candidate = 'cfmeet-messaging-config-candidate-' + run_id
    test = copy.deepcopy(cfg)
    test['HostConfig']['PortBindings'] = {}
    test['NetworkingConfig'] = {'EndpointsConfig': {p.NET: {'Aliases': [candidate]}}}
    release.start(candidate, test)
    previous = 'cfmeet-messaging-runtime-rollback-' + run_id
    replaced = False
    try:
        release.wait(candidate, 'messaging')
        p.run('docker', 'stop', '--time', '20', candidate)
        p.write_private(WORK / ('runtime-rollback-' + run_id + '.private.json'), json.dumps({'original': old, 'previous': previous, 'workflowSha': sha}))
        p.run('docker', 'rename', 'cfmeet-messaging', previous)
        p.run('docker', 'stop', '--time', '30', previous)
        replaced = True
        release.start('cfmeet-messaging', cfg)
        release.wait('cfmeet-messaging', 'messaging')
        install_ingress(run_id)
        storage_evidence = storage_probe(run_id)
        p.regression()
        assert p.inspect('cfmeet-messaging')['Image'] == old['Image']
        print(json.dumps({'operation': 'messaging-configure', 'runId': run_id,
            'imageIdPreserved': old['Image'], 'productionE2eeEnabled': False,
            'managedPersonalPolicyChanged': False, 'scanner': scanner_evidence, 'storage': storage_evidence,
            'offServerRecovery': 'DEFERRED_BY_OWNER', 'rollbackContainer': previous}))
        messaging_release.inspect_runtime(sha, run_id)
    except Exception:
        if replaced:
            if 'cfmeet-messaging' in p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines():
                p.run('docker', 'stop', '--time', '15', 'cfmeet-messaging')
                p.run('docker', 'rename', 'cfmeet-messaging', 'cfmeet-messaging-runtime-failed-' + run_id)
            p.run('docker', 'rename', previous, 'cfmeet-messaging'); p.run('docker', 'start', 'cfmeet-messaging')
            release.wait('cfmeet-messaging', 'messaging')
        before = WORK / ('nginx-site-before-' + run_id)
        if before.exists():
            Path('/etc/nginx/sites-available/api.cfmeet.com').write_text(before.read_text())
            previous_snippet = WORK / ('nginx-snippet-before-' + run_id)
            Path('/etc/nginx/snippets/cfmeet-messaging.conf').write_text(previous_snippet.read_text() if previous_snippet.exists() else '# Messaging ingress rolled back\n')
            p.run('nginx', '-t'); p.run('systemctl', 'reload', 'nginx')
        raise
