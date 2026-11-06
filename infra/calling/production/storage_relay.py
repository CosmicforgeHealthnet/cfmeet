"""Durable host routes to the private object store (`cfmeet-calling-artifacts`).

Host nginx serves two signed-URL bucket routes on api.cfmeet.com: Calling artifacts
(`/cfmeet-calling-artifacts/`, GET/HEAD) and Messaging attachments
(`/cfmeet-messaging-attachments/`, GET/HEAD/PUT). Both were installed with the store's
Docker address of that moment (`proxy_pass http://172.x.y.z:9000;`). Docker assigns a
new address when the store container is recreated or the host restarts in another order,
and the routes then reach nothing, or whatever container takes that address next.

Host nginx cannot use Docker DNS. As for the realtime route, a loopback-only TCP relay
on the private network resolves the store by its Docker name on every connection and
forwards bytes unchanged: the S3 signature (computed over the public Host header, which
the routes preserve) is still verified by the store, the bucket policies are unchanged,
and the relay publishes only 127.0.0.1. Scanning is unaffected: MANAGED attachments are
released by Messaging only after a signed clean verdict, independent of this path.
"""
import ipaddress
import json
import os
import re
import secrets
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

import production as p
import storage_encryption as storage

RELAY = 'cfmeet-storage-ingress'
RELAY_NET = 'cfmeet-storage-ingress-net'
RELAY_PORT = 19000
UPSTREAM = storage.NAME + ':9000'
ADDRESS = '127.0.0.1:%d' % RELAY_PORT
WORK = p.ROOT / 'storage'
CALLING_SNIPPET = Path('/etc/nginx/snippets/cfmeet-calling-artifacts.conf')
MESSAGING_SNIPPET = Path('/etc/nginx/snippets/cfmeet-messaging.conf')
BUCKETS = {'cfmeet-calling-artifacts': CALLING_SNIPPET, 'cfmeet-messaging-attachments': MESSAGING_SNIPPET}
STORE_PASS = re.compile(r'proxy_pass http://([0-9.]+):9000;')


def relay_conf():
    return WORK / 'ingress-relay.conf'


def relay_config():
    return ('daemon off;\nworker_processes 1;\npid /tmp/nginx.pid;\nerror_log stderr warn;\n'
            'events { worker_connections 512; }\n'
            '# TCP passthrough only: requests and S3 signatures reach the store unchanged.\n'
            'stream {\n    resolver 127.0.0.11 valid=10s ipv6=off;\n'
            '    server {\n        listen 9000;\n        set $peer ' + UPSTREAM + ';\n'
            '        proxy_pass $peer;\n        proxy_connect_timeout 5s;\n        proxy_timeout 900s;\n    }\n}\n')


def relay_create_args(image, config=None):
    """Loopback-only port, read-only root, no capabilities, non-root, bounded resources."""
    return ['docker', 'create', '--name', RELAY, '--restart', 'unless-stopped',
            '--network', RELAY_NET, '--publish', '127.0.0.1:%d:9000' % RELAY_PORT,
            '--read-only', '--tmpfs', '/tmp:rw,size=1m', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges', '--user', '101:101',
            '--memory', '32m', '--pids-limit', '32', '--cpus', '0.25',
            '--log-opt', 'max-size=5m', '--log-opt', 'max-file=2',
            '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % (config or relay_conf()),
            '--entrypoint', 'nginx', image, '-e', 'stderr', '-c', '/etc/nginx/nginx.conf']


def relay_matches(container, image):
    host = container['HostConfig']
    return (container['State']['Running'] and container['Image'] == image
            and host.get('PortBindings') == {'9000/tcp': [{'HostIp': '127.0.0.1', 'HostPort': str(RELAY_PORT)}]}
            and set(container['NetworkSettings']['Networks']) == {RELAY_NET, p.NET}
            and bool(host.get('ReadonlyRootfs')) and host.get('CapDrop') == ['ALL'])


def names():
    return p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()


def ensure_relay(run_id):
    """Reuses an identical running relay; otherwise replaces it, keeping the previous
    one stopped for rollback. Uses the private proxy's own nginx image (already on the
    host; nothing is pulled)."""
    image = p.inspect('cfmeet-core-tls')['Image']
    text = relay_config()
    current = p.inspect(RELAY) if RELAY in names() else None
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
        assert relay_matches(p.inspect(RELAY), image), 'Storage ingress relay is not confined as required'
    except Exception:
        restore_relay(state)
        raise
    return state


def restore_relay(state):
    if state['relay'] == 'reused':
        return
    if RELAY in names():
        p.run('docker', 'rm', '-f', RELAY)
    if state['confBefore'] is not None:
        relay_conf().write_text(state['confBefore'])
    if state['previous']:
        p.run('docker', 'rename', state['previous'], RELAY)
        p.run('docker', 'start', RELAY)


def wait_for_relay(timeout=30, url='http://%s/minio/health/live' % ADDRESS):
    """The store's own liveness answer, through the relay (Docker DNS resolved)."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            with urllib.request.urlopen(url, timeout=5) as r:
                if r.status == 200:
                    return
        except (urllib.error.URLError, OSError):
            pass
        if time.monotonic() > deadline:
            raise RuntimeError('Storage relay does not reach the object store')
        time.sleep(1)


def store_upstream():
    """The upstream host routes must use: the relay once it runs confined, otherwise
    (first installation only) the store's current private address."""
    if RELAY in names():
        relay = p.inspect(RELAY)
        if relay_matches(relay, p.inspect('cfmeet-core-tls')['Image']):
            return ADDRESS
    ip = p.inspect(storage.NAME)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    assert ipaddress.ip_address(ip).is_private
    return ip + ':9000'


def relayed(text):
    """The route text with every store address replaced by the relay. Only private
    addresses are rewritten, and each snippet must have exactly one store upstream."""
    found = STORE_PASS.findall(text)
    if 'proxy_pass http://' + ADDRESS + ';' in text and not found:
        return text
    assert len(found) == 1, 'Expected exactly one object store upstream'
    assert ipaddress.ip_address(found[0]).is_private, 'Unexpected object store upstream'
    return STORE_PASS.sub('proxy_pass http://' + ADDRESS + ';', text)


def anonymous_status(bucket):
    """A signed-URL route refuses an unsigned request (the store answers 403)."""
    url = 'https://api.cfmeet.com/%s/route-check-%s' % (bucket, secrets.token_hex(8))
    try:
        urllib.request.urlopen(url, timeout=15)
        return 200
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, TimeoutError):
        return 0


def signed_round_trip(bucket, user, password, prefix, run_id):
    """Write through the relay with the bucket's own scoped identity, read back through
    the public HTTPS route with an S3 signature, then delete. No secret is printed."""
    mc = str(p.ROOT / 'storage' / 'mc')
    env = dict(os.environ, MC_HOST_relay='http://%s:%s@%s' % (user, password, ADDRESS),
               MC_HOST_public='https://%s:%s@api.cfmeet.com' % (user, password))
    key = '%s/route-relay-%s-%s' % (prefix, run_id, secrets.token_hex(4))
    data = secrets.token_urlsafe(24)
    def mcrun(*args, input=None):
        return subprocess.run([mc, *args], env=env, input=input, capture_output=True, text=True, timeout=40)
    put = mcrun('pipe', 'relay/%s/%s' % (bucket, key), input=data)
    assert put.returncode == 0, 'Relay write failed for ' + bucket
    try:
        read = mcrun('cat', 'public/%s/%s' % (bucket, key))
        assert read.returncode == 0 and read.stdout == data, 'Signed public read failed for ' + bucket
    finally:
        assert mcrun('rm', 'relay/%s/%s' % (bucket, key)).returncode == 0, 'Probe cleanup failed for ' + bucket
    return 'PASS'


def verify_routes(run_id):
    checks = {}
    for bucket in BUCKETS:
        checks[bucket + ':anonymous'] = anonymous_status(bucket)
    checks['cfmeet-calling-artifacts:signed'] = signed_round_trip(
        'cfmeet-calling-artifacts', p.secret('minio-service-access-key'), p.secret('calling-artifact-store-secret'),
        'capture-staging', run_id)
    msg = p.ROOT / 'messaging'
    checks['cfmeet-messaging-attachments:signed'] = signed_round_trip(
        'cfmeet-messaging-attachments', (msg / 'storage-user').read_text(), (msg / 'storage-secret').read_text(),
        'section8-probes', run_id)
    assert all(checks[b + ':anonymous'] == 403 for b in BUCKETS), 'Unsigned storage access was not refused: ' + json.dumps(checks)
    return checks


def inspect_state():
    routes = {}
    for bucket, path in BUCKETS.items():
        text = path.read_text() if path.exists() else ''
        upstreams = re.findall(r'proxy_pass http://([0-9.:]+);', text)
        routes[bucket] = {'upstreams': upstreams,
                          'dockerAddressPinned': any(not u.startswith('127.') for u in upstreams)}
    store = p.inspect(storage.NAME)
    state = {'storageRoutes': routes, 'relay': RELAY in names() and p.inspect(RELAY)['State']['Running'],
             'storePublishedPorts': store['HostConfig']['PortBindings'] or {},
             'storeAddressNow': store['NetworkSettings']['Networks'][p.NET]['IPAddress']}
    print(json.dumps(state), flush=True)
    return state


def make_durable(run_id):
    p.identity()
    store = p.inspect(storage.NAME)
    assert store['State']['Running'] and not store['HostConfig']['PortBindings'], 'Object store must stay private'
    before = {b: path.read_text() for b, path in BUCKETS.items()}
    evidence = p.ROOT / 'production-evidence' / ('storage-routes-' + run_id)
    for bucket, text in before.items():
        p.write_private(evidence / (bucket + '.conf.before'), text)
    p.write_private(WORK / 'routes-rollback.json', json.dumps({'runId': run_id, 'evidence': str(evidence)}))
    relay = ensure_relay(run_id)
    try:
        wait_for_relay()
        for bucket, path in BUCKETS.items():
            path.write_text(relayed(before[bucket]))
        p.reload_host_nginx()
        checks = verify_routes(run_id)
    except Exception:
        for bucket, path in BUCKETS.items():
            path.write_text(before[bucket])
        p.reload_host_nginx()
        restore_relay(relay)
        raise
    p.regression()
    result = {'operation': 'storage-routes', 'runId': run_id, 'relay': relay['relay'], 'address': ADDRESS, 'checks': checks}
    print(json.dumps(result), flush=True)
    return result


def rollback():
    record = json.loads((WORK / 'routes-rollback.json').read_text())
    evidence = Path(record['evidence'])
    for bucket, path in BUCKETS.items():
        path.write_text((evidence / (bucket + '.conf.before')).read_text())
    p.reload_host_nginx()
    print(json.dumps({'storageRoutesRollback': record['runId']}), flush=True)


def run(operation, sha, run_id):
    if operation == 'storage-routes-inspect':
        inspect_state()
    elif operation == 'storage-routes':
        make_durable(run_id)
        inspect_state()
    elif operation == 'storage-routes-rollback':
        rollback()
        inspect_state()
    else:
        raise RuntimeError('Unknown storage route operation')
