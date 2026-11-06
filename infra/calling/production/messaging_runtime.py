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


UPSTREAM_TRUST = Path('/etc/nginx/cfmeet-messaging-upstream-ca.pem')
SYSTEM_BUNDLE = Path('/etc/ssl/certs/ca-certificates.crt')
SITE = Path('/etc/nginx/sites-available/api.cfmeet.com')
SITE_LINK = Path('/etc/nginx/sites-enabled/api.cfmeet.com')
SNIPPET = Path('/etc/nginx/snippets/cfmeet-messaging.conf')
NO_INGRESS = '# previous configuration had no Messaging ingress\n'
# The private proxy presents Let's Encrypt's Generation Y chain: leaf, YE1, Root YE
# cross-signed by ISRG Root X2, and X2 cross-signed by X1. The trust anchor (X2 in the
# system store) sits two intermediates above the leaf. nginx's default depth of 1 refuses
# this chain with "unable to get local issuer certificate", the exact live 502. Depth 2 is
# the measured minimum (host nginx 1.24, OpenSSL 3.0.13, 2026-11-05); 5 leaves margin.
UPSTREAM_VERIFY_DEPTH = 5
UPSTREAM_NAME = 'api.cfmeet.com'
# Host nginx reaches the private TLS proxy through a loopback-only TCP relay. The relay
# resolves the proxy by its Docker name on every connection and never terminates TLS:
# host nginx still verifies the proxy's own chain and name. No Docker address is written
# into the host configuration, so the route survives a proxy restart or a host reboot.
RELAY = 'cfmeet-messaging-ingress'
RELAY_NET = 'cfmeet-messaging-ingress-net'
RELAY_PORT = 18444
RELAY_UPSTREAM = 'cfmeet-core-tls:8444'


def relay_conf():
    return WORK / 'ingress-relay.conf'


def relay_config():
    return ('daemon off;\nworker_processes 1;\npid /tmp/nginx.pid;\nerror_log stderr warn;\n'
        'events { worker_connections 512; }\n'
        '# TCP passthrough only: TLS runs end to end between host nginx and the private proxy.\n'
        'stream {\n    resolver 127.0.0.11 valid=10s ipv6=off;\n'
        '    server {\n        listen 8444;\n        set $peer ' + RELAY_UPSTREAM + ';\n'
        '        proxy_pass $peer;\n        proxy_connect_timeout 5s;\n        proxy_timeout 3600s;\n    }\n}\n')


def relay_create_args(image, config=None):
    """The relay container: loopback-only port, read-only, no capabilities, non-root,
    bounded memory, CPU, processes and logs. `image` is the private proxy's own nginx."""
    return ['docker', 'create', '--name', RELAY, '--restart', 'unless-stopped',
        '--network', RELAY_NET, '--publish', '127.0.0.1:%d:8444' % RELAY_PORT,
        '--read-only', '--tmpfs', '/tmp:rw,size=1m', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges', '--user', '101:101',
        '--memory', '32m', '--pids-limit', '32', '--cpus', '0.25',
        '--log-opt', 'max-size=5m', '--log-opt', 'max-file=2',
        '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % (config or relay_conf()),
        '--entrypoint', 'nginx', image, '-e', 'stderr', '-c', '/etc/nginx/nginx.conf']


def relay_matches(container, image):
    host = container['HostConfig']
    return (container['State']['Running'] and container['Image'] == image
        and host.get('PortBindings') == {'8444/tcp': [{'HostIp': '127.0.0.1', 'HostPort': str(RELAY_PORT)}]}
        and set(container['NetworkSettings']['Networks']) == {RELAY_NET, p.NET}
        and bool(host.get('ReadonlyRootfs')) and host.get('CapDrop') == ['ALL'])


def ensure_relay(run_id):
    """Reuses an identical running relay (no interruption); otherwise replaces it and
    keeps the previous one stopped for rollback."""
    image = p.inspect('cfmeet-core-tls')['Image']
    text = relay_config()
    names = p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    current = p.inspect(RELAY) if RELAY in names else None
    conf_before = relay_conf().read_text() if relay_conf().is_file() else None
    if current and conf_before == text and relay_matches(current, image):
        return {'relay': 'reused', 'previous': None, 'confBefore': conf_before}
    previous = None
    if current:
        previous = RELAY + '-previous-' + run_id
        p.run('docker', 'rename', RELAY, previous)
        p.run('docker', 'stop', '--time', '5', previous)
    state = {'relay': 'created', 'previous': previous, 'confBefore': conf_before}
    try:
        relay_conf().parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        relay_conf().write_text(text); relay_conf().chmod(0o644)
        if RELAY_NET not in p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
            p.run('docker', 'network', 'create', RELAY_NET)
        p.run(*relay_create_args(image))
        p.run('docker', 'network', 'connect', p.NET, RELAY)
        p.run('docker', 'start', RELAY)
        assert relay_matches(p.inspect(RELAY), image), 'Messaging ingress relay is not confined as required'
    except Exception:
        restore_relay(state)
        raise
    return state


def restore_relay(state):
    if state['relay'] == 'reused':
        return
    if RELAY in p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines():
        p.run('docker', 'rm', '-f', RELAY)
    if state['confBefore'] is not None:
        relay_conf().write_text(state['confBefore'])
    if state['previous']:
        p.run('docker', 'rename', state['previous'], RELAY)
        p.run('docker', 'start', RELAY)


def wait_for_relay(timeout=30):
    """The relay answers with the private proxy's TLS handshake (Docker DNS resolved)."""
    deadline = time.monotonic() + timeout
    while True:
        r = subprocess.run(['openssl', 's_client', '-connect', '127.0.0.1:%d' % RELAY_PORT, '-servername', UPSTREAM_NAME],
                           input='', capture_output=True, text=True, timeout=20)
        if 'BEGIN CERTIFICATE' in r.stdout or 'Server certificate' in r.stdout:
            return
        if time.monotonic() > deadline:
            raise RuntimeError('Messaging ingress relay did not reach the private proxy')
        time.sleep(1)


def verifies(ca_file, depth=UPSTREAM_VERIFY_DEPTH):
    """Verifies the private proxy through the relay exactly as host nginx will: the same
    name, SNI, depth and trust file."""
    r = subprocess.run(['openssl', 's_client', '-connect', '127.0.0.1:%d' % RELAY_PORT, '-servername', UPSTREAM_NAME,
                        '-verify_hostname', UPSTREAM_NAME, '-verify_depth', str(depth),
                        '-verify_return_error', '-no-CApath', '-CAfile', str(ca_file)],
                       input='', capture_output=True, text=True, timeout=20)
    return r.returncode == 0 and 'Verify return code: 0 (ok)' in r.stdout


def upstream_trust():
    """The system roots nginx verifies the private proxy against: the system bundle when it
    alone verifies the presented chain, else every system certificate in /etc/ssl/certs
    (where a newer root may exist outside the bundle). Never a private or pinned key."""
    bundle = SYSTEM_BUNDLE
    if verifies(bundle):
        return bundle, 'system-bundle'
    seen, pems = set(), []
    for item in sorted(Path('/etc/ssl/certs').glob('*.pem')):
        target = item.resolve()
        if target in seen or not target.is_file():
            continue
        seen.add(target)
        text = target.read_text(errors='replace')
        if 'BEGIN CERTIFICATE' in text and 'PRIVATE KEY' not in text:
            pems.append(text.strip() + '\n')
    candidate = UPSTREAM_TRUST.with_suffix('.candidate')
    candidate.write_text(''.join(pems)); candidate.chmod(0o644)
    assert verifies(candidate), 'System certificates do not verify the private proxy chain'
    candidate.replace(UPSTREAM_TRUST)
    return UPSTREAM_TRUST, 'system-directory'


def nginx_config(store, trust=SYSTEM_BUNDLE):
    """`store` is the loopback storage relay (`storage_relay.ADDRESS`) or, before that
    relay exists, a private store address."""
    host, _, port = store.partition(':')
    assert ipaddress.ip_address(host).is_private and port in ('', '9000', '19000')
    store = store if port else store + ':9000'
    common = ('    access_log off;\n    proxy_http_version 1.1;\n'
        '    proxy_set_header Host $http_host;\n    proxy_set_header X-Forwarded-Proto https;\n'
        '    add_header Cache-Control "private, no-store" always;\n')
    tls = (common + '    proxy_ssl_server_name on;\n    proxy_ssl_name ' + UPSTREAM_NAME + ';\n'
        '    proxy_ssl_verify on;\n    proxy_ssl_verify_depth ' + str(UPSTREAM_VERIFY_DEPTH) + ';\n'
        '    proxy_ssl_trusted_certificate ' + str(trust) + ';\n'
        '    proxy_set_header Upgrade $http_upgrade;\n    proxy_set_header Connection "upgrade";\n'
        '    proxy_read_timeout 3600;\n    proxy_pass https://127.0.0.1:' + str(RELAY_PORT) + ';\n')
    return ('# Section 8: device-only realtime ingress; Account owns native REST authentication.\n'
        'location = /v1/realtime {\n    limit_except GET { deny all; }\n' + tls + '}\n'
        'location = /v1/realtime/tickets {\n    limit_except POST OPTIONS { deny all; }\n' + tls + '}\n'
        'location ^~ /' + BUCKET + '/ {\n    limit_except GET HEAD PUT OPTIONS { deny all; }\n'
        '    client_max_body_size 64m;\n    proxy_buffering off;\n    proxy_request_buffering off;\n'
        + common + '    proxy_pass http://' + store + ';\n}\n')


def install_ingress(run_id):
    """Installs the relay and the host routes. Returns only when every accepting host
    nginx worker serves the new routes; restores everything itself on failure. The
    returned relay state lets later checks roll the installation back."""
    import storage_relay
    store = storage_relay.store_upstream()
    assert SITE_LINK.resolve() == SITE
    before = SITE.read_text()
    include = 'include /etc/nginx/snippets/cfmeet-messaging.conf;'
    anchor = 'include /etc/nginx/snippets/cfmeet-calling-routes.conf;'
    assert before.count(anchor) == 1 and before.count(include) <= 1
    old = SNIPPET.read_text() if SNIPPET.exists() else None
    p.write_private(WORK / ('nginx-site-before-' + run_id), before)
    if old is not None: p.write_private(WORK / ('nginx-snippet-before-' + run_id), old)
    relay = ensure_relay(run_id)
    try:
        wait_for_relay()
        trust, source = upstream_trust()
        print(json.dumps({'messagingIngressUpstreamTrust': source, 'relay': relay['relay']}), flush=True)
        SNIPPET.write_text(nginx_config(store, trust))
        if include not in before: SITE.write_text(before.replace(anchor, anchor + '\n    ' + include))
        p.reload_host_nginx()
    except Exception:
        restore_ingress(run_id, relay)
        raise
    return relay


def restore_ingress(run_id, relay):
    """The exact previous host routes, then the previous relay."""
    SITE.write_text((WORK / ('nginx-site-before-' + run_id)).read_text())
    previous = WORK / ('nginx-snippet-before-' + run_id)
    SNIPPET.write_text(previous.read_text() if previous.exists() else NO_INGRESS)
    p.reload_host_nginx()
    restore_relay(relay)


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


def joined_headers(response):
    """Lower-cased response headers; a repeated header keeps every value, comma-joined."""
    headers = {}
    for name, value in response.getheaders():
        name = name.lower()
        headers[name] = headers[name] + ', ' + value if name in headers else value
    return headers


def public_request(method, path, headers=None, body=None, host=UPSTREAM_NAME, port=443):
    """One request over verified public HTTPS (HTTP/1.1): status, lower-cased headers, body."""
    import http.client
    import ssl
    connection = http.client.HTTPSConnection(host, port, timeout=15, context=ssl.create_default_context())
    try:
        connection.request(method, path, body=body, headers=headers or {})
        response = connection.getresponse()
        return response.status, joined_headers(response), response.read(65536)
    finally:
        connection.close()


def messaging_refusal(answer, status, reason=None, code='AUTHENTICATION_FAILED'):
    """`answer` is Messaging's own refusal (its error envelope) served through the realtime
    location, not an nginx error page. Messaging sends `no-store`; only the realtime
    location adds `private, no-store`."""
    got, headers, raw = answer
    assert got == status, 'Realtime route answered %d, expected %d from Messaging' % (got, status)
    assert 'private' in headers.get('cache-control', ''), 'Response did not pass the realtime location'
    error = json.loads(raw)['error']
    assert error['code'] == code, 'Unexpected Messaging error code'
    if reason: assert error.get('reason') == reason, 'Unexpected Messaging refusal reason'
    if status == 401: assert 'cf-meet-messaging' in headers.get('www-authenticate', '')
    return status


def ingress_probe(request=public_request):
    """The public realtime routes reach Messaging over verified TLS and refuse every
    unauthenticated or forged use. A valid ticket needs an enrolled device, which the
    production E2EE guard refuses (issue 13), so issuance is not attempted here."""
    json_post = {'Content-Type': 'application/json'}
    forged = 'CF-Client-Token ' + secrets.token_urlsafe(32)
    evidence = {
        'ticketUnauthenticated': messaging_refusal(request('POST', '/v1/realtime/tickets', json_post, b'{}'), 401),
        'ticketForgedClientToken': messaging_refusal(request('POST', '/v1/realtime/tickets',
            dict(json_post, Authorization=forged), b'{}'), 401),
        'ticketRefusedOrigin': messaging_refusal(request('POST', '/v1/realtime/tickets',
            dict(json_post, Authorization=forged, Origin='https://origin-refused.invalid'), b'{}'), 401, 'ORIGIN_NOT_ALLOWED'),
    }
    status, headers, _ = request('OPTIONS', '/v1/realtime/tickets',
        {'Origin': 'https://origin-refused.invalid', 'Access-Control-Request-Method': 'POST'})
    assert status == 403 and 'access-control-allow-origin' not in headers, 'Refused origin preflight was allowed'
    evidence['ticketPreflightRefusedOrigin'] = status
    status, _, _ = request('DELETE', '/v1/realtime/tickets')
    assert status == 403, 'Realtime ticket route accepted another method'
    evidence['ticketOtherMethodDenied'] = status
    def upgrade(protocols):
        headers = {'Upgrade': 'websocket', 'Connection': 'Upgrade', 'Sec-WebSocket-Version': '13',
                   'Sec-WebSocket-Key': base64.b64encode(secrets.token_bytes(16)).decode()}
        if protocols: headers['Sec-WebSocket-Protocol'] = protocols
        answer = request('GET', '/v1/realtime', headers)
        assert answer[0] != 101, 'Realtime WebSocket upgraded without a valid ticket'
        return answer
    evidence['websocketNoTicket'] = messaging_refusal(upgrade('cfmeet.messaging.v1'), 401, 'TICKET_REQUIRED')
    # A well-formed (64 hex) unknown ticket is refused by Messaging's ticket store lookup.
    evidence['websocketForgedTicket'] = messaging_refusal(
        upgrade('cfmeet.messaging.v1, cfmeet.ticket.' + secrets.token_hex(32)), 401, 'INVALID_TICKET')
    evidence['websocketNoSubprotocol'] = messaging_refusal(upgrade(None), 400, 'SUBPROTOCOL_REQUIRED', 'VALIDATION_ERROR')
    evidence['validTicketIssuance'] = 'NOT_RUN_PRODUCTION_E2EE_DEVICE_ENROLLMENT_DISABLED'
    return evidence


def ingress_diagnostics():
    """Where the public realtime route fails (status codes and redacted log templates)."""
    import media_diagnostics
    out = {}
    log = Path('/var/log/nginx/error.log')
    if log.exists():
        lines = [l for l in log.read_text(errors='replace').splitlines()[-2000:] if '/v1/realtime' in l]
        out['realtimeErrors'] = [media_diagnostics.template(l) for l in lines[-6:]]
    r = subprocess.run(['curl', '-sS', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', '-X', 'POST',
                        '--resolve', '%s:%d:127.0.0.1' % (UPSTREAM_NAME, RELAY_PORT),
                        'https://%s:%d/v1/realtime/tickets' % (UPSTREAM_NAME, RELAY_PORT)], capture_output=True, text=True)
    out['hostToRelayVerified'] = r.stdout.strip() or 'error'
    out['relayVerifiesAtNginxDepth'] = verifies(SYSTEM_BUNDLE)
    names = p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    out['relayRunning'] = RELAY in names and p.inspect(RELAY)['State']['Running']
    snippet = SNIPPET.read_text() if SNIPPET.exists() else ''
    out['liveRealtimeDirectives'] = sorted({re.sub(r'\d+\.\d+\.\d+\.\d+', '<ip>', l.strip()) for l in snippet.splitlines()
                                           if 'proxy_ssl' in l or 'proxy_pass https' in l})
    out['acceptingWorkers'] = len(p.accepting_nginx_workers())
    return out


def reinstall_ingress(run_id):
    """Re-applies only the public Messaging ingress; restores the exact previous routes
    and relay if any probe or the regression fails."""
    p.identity()
    relay = install_ingress(run_id)
    try:
        evidence = ingress_probe()
        p.regression()
    except Exception:
        print(json.dumps({'messagingIngressDiagnostics': ingress_diagnostics()}), flush=True)
        restore_ingress(run_id, relay)
        raise
    print(json.dumps({'operation': 'messaging-ingress', 'runId': run_id, 'relay': relay['relay'],
                      'upstreamVerifyDepth': UPSTREAM_VERIFY_DEPTH, **evidence}))


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
    relay = None
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
        relay = install_ingress(run_id)
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
        # install_ingress restores its own failures; later failures restore it here.
        if relay is not None:
            restore_ingress(run_id, relay)
        raise
