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
import re
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
# MANAGED attachments are scanned by the CosmicForge shared security scan gateway in
# front of an EXISTING production ClamAV (scan_gateway.py). CF Meet never starts its own
# antivirus engine on the shared VPS: the 3 GiB dedicated daemon is removed from this
# configuration. Without a ready gateway the configuration runs in QUARANTINE mode:
# MANAGED attachments requiring scanning stay VERIFYING (never READY unscanned).
SCAN_KEYS_TARGET = '/run/messaging/scan-verdict-keys.json'
# The engine's StreamMaxLength (25M) bounds what can be scanned; larger MANAGED files
# are refused as TOO_LARGE_TO_SCAN rather than delivered unscanned.
MANAGED_SCAN_MAX_BYTES = 25 * 1024 * 1024
# Measured capacity rule (docs/operations/messaging-section-8-delivery.md): a
# coordinated release still needs its own 3 GiB for isolated candidates, plus a
# reserve for Calling/LiveKit media. No scanner memory is reserved any more.
RELEASE_MARGIN_MIB = 3072
MEDIA_RESERVE_MIB = 512
HEADROOM_KIB = (RELEASE_MARGIN_MIB + MEDIA_RESERVE_MIB) * 1024
# Retired names, kept only so a failed earlier run can be recognized and never reused.
RETIRED_SCANNER = 'cfmeet-messaging-clamav'


def available_kib():
    return int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:')))


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
    # Depth 5: the private proxy presents Let's Encrypt's Generation Y chain
    # (leaf, YE1, Root YE cross-signed by ISRG Root X2, X2 cross-signed by X1), measured
    # by messaging-ingress 42109430743. nginx's default depth 1, and 3, cannot reach a
    # trusted root and answer 502; verification against the system roots is unchanged.
    tls = (common + '    proxy_ssl_server_name on;\n    proxy_ssl_name api.cfmeet.com;\n'
        '    proxy_ssl_verify on;\n    proxy_ssl_verify_depth 5;\n'
        '    proxy_ssl_trusted_certificate /etc/ssl/certs/ca-certificates.crt;\n'
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


def scanner_mode():
    """`gateway` when the shared scan gateway answers ready over its private TLS route;
    otherwise `quarantine` (no scanner variables: MANAGED scans stay pending)."""
    import scan_gateway
    keys = scan_gateway.WORK / 'verdict-keys.json'
    if scan_gateway.GATEWAY not in p.run('docker', 'ps', '--format', '{{.Names}}').splitlines() or not keys.is_file():
        return 'quarantine', {'reason': 'scan gateway not deployed'}
    try:
        ready = scan_gateway.readiness(scan_gateway.URL)
    except (RuntimeError, ValueError):
        return 'quarantine', {'reason': 'scan gateway unreachable'}
    if ready.get('status') != 'ready':
        return 'quarantine', {'reason': 'scan gateway not ready', 'ready': ready}
    return 'gateway', {'url': scan_gateway.URL, 'ready': ready, 'verdictKeys': str(keys)}


def runtime_overrides(env, user, redis_password, scan='gateway'):
    assert env.get('CF_RUNTIME_ENVIRONMENT') == 'production'
    assert env.get('MESSAGING_E2EE_DEVICES_ENABLED', 'false') == 'false', 'Production cryptographic guard required'
    assert not env.get('MESSAGING_MANAGED_KEYRING_FILE') or env['MESSAGING_MANAGED_KEYRING_FILE'] == '/run/messaging/managed.ring'
    assert scan in ('gateway', 'quarantine')
    scanner = {'MESSAGING_SCAN_GATEWAY_URL': 'https://api.cfmeet.com:8449',
               'MESSAGING_SCAN_GATEWAY_VERDICT_KEYS_FILE': SCAN_KEYS_TARGET,
               'MESSAGING_SCAN_GATEWAY_TIMEOUT_MS': '75000'} if scan == 'gateway' else {}
    return {**scanner,
        'MESSAGING_MANAGED_SCAN_MAX_BYTES': str(MANAGED_SCAN_MAX_BYTES),
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
        'MESSAGING_METRICS_ADDRESS': '0.0.0.0:9197',
        'MESSAGING_KT_CHECKPOINT_FILE': '/var/lib/messaging-kt/checkpoint.json',
    }


def runtime_config(old, env, user, password, managed, checkpoint, redis_password, scan_keys=None):
    """The configured production container: the EXACT running image plus dependencies."""
    scan = 'gateway' if scan_keys else 'quarantine'
    cfg = release.clone(old, 'cfmeet-messaging', old['Image'], runtime_overrides(env, user, redis_password, scan), False)
    # Never two scanners, and never a scanner from a previous mode.
    values = dict(item.split('=', 1) for item in cfg['Env'])
    values.pop('MESSAGING_CLAMAV_ADDRESS', None)
    if scan == 'quarantine':
        for key in [k for k in values if k.startswith('MESSAGING_SCAN_GATEWAY_')]:
            del values[key]
    cfg['Env'] = [k + '=' + v for k, v in values.items()]
    cfg['HostConfig']['Mounts'] = [m for m in cfg['HostConfig'].get('Mounts') or [] if m.get('Target') != SCAN_KEYS_TARGET]
    if scan_keys:
        release.add_mount(cfg, scan_keys, SCAN_KEYS_TARGET)
    release.add_mount(cfg, managed, '/run/messaging/managed.ring')
    release.add_mount(cfg, password, '/run/messaging/storage.secret')
    mounts = cfg['HostConfig'].setdefault('Mounts', [])
    if not any(m.get('Target') == '/var/lib/messaging-kt' for m in mounts):
        mounts.append({'Type': 'bind', 'Source': str(checkpoint), 'Target': '/var/lib/messaging-kt', 'ReadOnly': False})
    cfg['HostConfig']['Memory'] = old['HostConfig'].get('Memory') or 1024 * 1024 * 1024
    cfg['HostConfig']['NanoCpus'] = old['HostConfig'].get('NanoCpus') or 2000000000
    cfg['HostConfig']['LogConfig'] = {'Type': 'json-file', 'Config': {'max-size': '20m', 'max-file': '5'}}
    return cfg


def signed_grants(shared, key):
    """The signed download grants in `mc share download --json` output. Its `url` field is
    the plain (unsigned) object URL; only `share` is a grant, and it must be signed."""
    prefix = 'https://api.cfmeet.com/' + BUCKET + '/' + key + '?'
    grants = []
    for line in shared.splitlines():
        if not line.strip():
            continue
        value = json.loads(line).get('share')
        if isinstance(value, str) and value.startswith(prefix) and 'X-Amz-Signature=' in value:
            grants.append(value)
    return grants


def ingress_probe():
    """Public realtime routes reach Messaging and refuse unauthenticated use."""
    request = urllib.request.Request('https://api.cfmeet.com/v1/realtime/tickets', method='POST',
                                     data=b'{}', headers={'Content-Type': 'application/json'})
    try:
        urllib.request.urlopen(request, timeout=15)
    except urllib.error.HTTPError as error:
        assert error.code == 401, 'Realtime ticket route did not reach Messaging: ' + str(error.code)
    else:
        raise AssertionError('Unauthenticated realtime ticket issued')
    return {'realtimeTicketRouteUnauthenticated': 401}


def ingress_diagnostics():
    """Where the public realtime route fails: redacted host nginx errors and the private
    hops tested separately (status codes only)."""
    import media_diagnostics
    out = {}
    log = Path('/var/log/nginx/error.log')
    if log.exists():
        lines = [l for l in log.read_text(errors='replace').splitlines()[-400:] if 'upstream' in l or 'SSL' in l]
        out['hostNginxErrors'] = [media_diagnostics.template(l) for l in lines[-12:]]
    tls = p.inspect('cfmeet-core-tls')['NetworkSettings']['Networks'][p.NET]['IPAddress']
    def code(*args):
        r = subprocess.run(['curl', '-sS', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', *args],
                           capture_output=True, text=True)
        return r.stdout.strip() or ('error:' + re.sub(r'[^A-Za-z0-9 :().-]', '', r.stderr)[:120])
    out['hostToPrivateProxy'] = code('-X', 'POST', '--resolve', 'api.cfmeet.com:8444:' + tls,
                                     'https://api.cfmeet.com:8444/v1/realtime/tickets')
    out['hostToPrivateProxyNoVerify'] = code('-k', '-X', 'POST', 'https://' + tls + ':8444/v1/realtime/tickets')
    inside = subprocess.run(['docker', 'run', '--rm', '--network', p.NET, 'curlimages/curl:8.12.1', '-sS', '-o', '/dev/null',
                             '-w', '%{http_code}', '--max-time', '10', '-X', 'POST', 'https://api.cfmeet.com:8444/v1/realtime/tickets'],
                            capture_output=True, text=True)
    out['privateNetworkToPrivateProxy'] = inside.stdout.strip()
    # The presented chain (public certificate names only) and how each trust source verifies it.
    for label, trust in (('bundle', ['-CAfile', '/etc/ssl/certs/ca-certificates.crt']), ('directory', ['-CApath', '/etc/ssl/certs'])):
        r = subprocess.run(['openssl', 's_client', '-connect', tls + ':8444', '-servername', 'api.cfmeet.com', '-showcerts', *trust],
                           input='', capture_output=True, text=True, timeout=20)
        names = [re.sub(r'[^A-Za-z0-9 =,.:-]', '', l.strip())[:120] for l in r.stdout.splitlines()
                 if re.match(r'^\s*\d+ s:|^\s+i:', l)]
        verify = [l.strip()[:80] for l in r.stdout.splitlines() if 'Verify return code' in l]
        out['chain_' + label] = {'certificates': names, 'verify': verify[-1:] }
    r = subprocess.run(['nginx', '-V'], capture_output=True, text=True)
    out['hostNginx'] = [l for l in (r.stderr + r.stdout).splitlines() if 'version' in l or 'OpenSSL' in l][:2]
    return out


def reinstall_ingress(run_id):
    """Re-applies only the public Messaging ingress (rolled back if its probe fails)."""
    p.identity()
    install_ingress(run_id)
    try:
        evidence = ingress_probe()
    except Exception:
        print(json.dumps({'messagingIngressDiagnostics': ingress_diagnostics()}), flush=True)
        before = WORK / ('nginx-site-before-' + run_id)
        Path('/etc/nginx/sites-available/api.cfmeet.com').write_text(before.read_text())
        previous = WORK / ('nginx-snippet-before-' + run_id)
        Path('/etc/nginx/snippets/cfmeet-messaging.conf').write_text(previous.read_text() if previous.exists() else '# Messaging ingress rolled back\n')
        p.run('nginx', '-t'); p.run('systemctl', 'reload', 'nginx')
        raise
    p.regression()
    print(json.dumps({'operation': 'messaging-ingress', 'runId': run_id, **evidence}))


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
        grants = signed_grants(shared, key)
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


def dependency_probe(container):
    p.redis_ping_from(container)
    def get(port, path):
        return p.run('docker', 'run', '--rm', '--network', p.NET, 'curlimages/curl:8.12.1',
            '-fsS', '--max-time', '15', 'http://' + container + ':' + str(port) + path)
    ready = json.loads(get(8097, '/ready'))
    assert ready['realtime']['broker'] == 'redis' and ready['realtime']['presence'] == 'redis'
    assert ready['e2eeDevices'] == 'disabled'
    assert 'messaging_db_pool_connections' in get(9197, '/metrics')
    assert not p.inspect(container)['HostConfig']['PortBindings'], 'Messaging must stay private'
    return {'authenticatedRedis': 'PASS', 'redisBrokerAndPresence': 'PASS',
            'privateMetrics': 'PASS', 'privateServicePorts': 'PASS'}


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
    # No antivirus engine is started: only the coordinated-release margin and the media
    # reserve must remain available.
    assert available_kib() >= HEADROOM_KIB, 'Insufficient memory for coordinated-release margin and media reserve'
    assert RETIRED_SCANNER not in p.run('docker', 'ps', '--format', '{{.Names}}').splitlines(), \
        'A dedicated CF Meet scanner is running; it is retired by the shared gateway'
    mode, scan_evidence = scanner_mode()
    managed = protected_file('managed.ring', lambda: 'messaging-20261105 ' + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip('=') + '\n')
    user, password = storage_identity()
    return _configure(sha, run_id, old, env, managed, user, password, mode, scan_evidence)


def scanner_in_use(container):
    """Which scanner the running binary actually configured (its own startup line)."""
    logs = subprocess.run(['docker', 'logs', '--tail', '400', container], capture_output=True, text=True, timeout=30)
    text = logs.stdout + logs.stderr
    if 'scanned through the shared security scan gateway' in text:
        return 'gateway'
    if 'no attachment scanner is configured' in text:
        return 'quarantine'
    return 'unknown'


def _configure(sha, run_id, old, env, managed, user, password, mode='quarantine', scan_evidence=None):
    import messaging_release
    import scan_gateway
    scanner_evidence = {'mode': mode, **(scan_evidence or {})}
    checkpoint = WORK / 'kt-checkpoints'
    checkpoint.mkdir(exist_ok=True, mode=0o700); os.chown(checkpoint, 10001, 10001)
    scan_keys = None
    if mode == 'gateway':
        scan_keys = WORK / 'scan-verdict-keys.json'
        release.save(scan_keys, (scan_gateway.WORK / 'verdict-keys.json').read_text(), False)
    cfg = runtime_config(old, env, user, password, managed, checkpoint, p.secret('calling-capture-redis-password'), scan_keys)
    candidate = 'cfmeet-messaging-config-candidate-' + run_id
    test = copy.deepcopy(cfg)
    test['HostConfig']['PortBindings'] = {}
    test['NetworkingConfig'] = {'EndpointsConfig': {p.NET: {'Aliases': [candidate]}}}
    release.start(candidate, test)
    previous = 'cfmeet-messaging-runtime-rollback-' + run_id
    replaced = False
    try:
        release.wait(candidate, 'messaging')
        dependency_probe(candidate)
        active = scanner_in_use(candidate)
        # An image without gateway support would silently run unscanned-pending; refuse
        # to call that "gateway". Quarantine is truthful either way.
        assert active == mode, 'Messaging image did not configure the expected scanner: ' + active
        scanner_evidence['activeInCandidate'] = active
        p.run('docker', 'stop', '--time', '20', candidate)
        p.write_private(WORK / ('runtime-rollback-' + run_id + '.private.json'), json.dumps({'original': old, 'previous': previous,
            'workflowSha': Path(__file__).resolve().parent.name, 'requestedImageSha': sha}))
        p.run('docker', 'rename', 'cfmeet-messaging', previous)
        p.run('docker', 'stop', '--time', '30', previous)
        replaced = True
        release.start('cfmeet-messaging', cfg)
        release.wait('cfmeet-messaging', 'messaging')
        dependency_evidence = dependency_probe('cfmeet-messaging')
        assert scanner_in_use('cfmeet-messaging') == mode, 'Production Messaging scanner mode changed'
        install_ingress(run_id)
        storage_evidence = storage_probe(run_id)
        storage_evidence.update(ingress_probe())
        p.regression()
        assert p.inspect('cfmeet-messaging')['Image'] == old['Image']
        print(json.dumps({'operation': 'messaging-configure', 'runId': run_id,
            'imageIdPreserved': old['Image'], 'productionE2eeEnabled': False,
            'managedPersonalPolicyChanged': False, 'scanner': scanner_evidence, 'storage': storage_evidence,
            'dependencies': dependency_evidence,
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
