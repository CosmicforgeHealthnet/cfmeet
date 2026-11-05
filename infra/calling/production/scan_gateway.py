"""CosmicForge shared security scan gateway on the shared VPS (no new antivirus engine).

Operations (existing VPS workflow, main only):

* ``scan-gateway-configure``: deploys the gateway image built from the requested SHA in
  front of the EXISTING production mail ClamAV, through a dedicated internal network that
  contains only the gateway and that engine, then runs live acceptance. Idempotent; an
  upgrade keeps the previous gateway container for rollback.
* ``scan-gateway-verify``: live acceptance + resource measurement only.
* ``scan-gateway-disable``: stops the gateway (retained), removes its private TLS route
  and disconnects the engine from the dedicated network. Messaging then fails closed
  (MANAGED attachments stay VERIFYING); nothing fails open.

Boundaries kept on another product's scanner: no restart, no configuration change, no
signature change, no published port. The only change is one extra network attachment
(reversible with ``scan-gateway-disable``), made only after its health, memory headroom,
signature freshness and stream limit were measured.
"""
import datetime
import json
import os
import re
import subprocess
import time
from pathlib import Path

import production as p
import communications_release as release

WORK = p.ROOT / 'scan-gateway'
GATEWAY = 'cosmicforge-scan-gateway'
BACKEND_NET = 'cosmicforge-scan-backend'
ENGINE = 'admin-server-mail-clamav'
ENGINE_PROJECT = 'admin-server-mail'
ENGINE_ALIAS = 'cosmicforge-scan-engine'
TLS_PORT = 8449
TLS_MARKER = '  # COSMICFORGE SCAN GATEWAY PRIVATE ROUTE'
URL = 'https://api.cfmeet.com:' + str(TLS_PORT)
VERDICT_KID = 'scan-verdict-20261105'
# The engine's own StreamMaxLength is 25M (audit 42077986241). Never exceed it.
STREAM_MAX_BYTES = 25 * 1024 * 1024
BACKEND_CONCURRENCY = 2              # of the engine's MaxThreads 8
MAX_SIGNATURE_AGE_HOURS = 72
GATEWAY_MEMORY_MIB = 64
ENGINE_HEADROOM_MIB = 512            # spare inside the engine's own cgroup limit
CLIENTS = [{'issuer': 'cfmeet.messaging', 'product': 'cfmeet', 'maxBytes': STREAM_MAX_BYTES,
            'maxConcurrent': BACKEND_CONCURRENCY, 'maxPerMinute': 120}]
LABEL = 'com.cosmicforge.scan'


def image_ref(sha):
    return 'ghcr.io/cosmicforgehealthnet/cfmeet-security-scan-gateway:' + sha


def names():
    return p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()


def parse_size(value):
    m = re.fullmatch(r'(\d+)([KMG]?)', (value or '').strip().upper())
    assert m, 'Unparseable clamd size'
    return int(m.group(1)) * {'': 1, 'K': 1024, 'M': 1024 ** 2, 'G': 1024 ** 3}[m.group(2)]


def parse_engine_version(text, now):
    m = re.search(r'ClamAV ([\d.]+)/(\d+)/(.+)$', text.strip())
    assert m, 'Engine signature version unavailable'
    built = datetime.datetime.strptime(' '.join(m.group(3).split()), '%a %b %d %H:%M:%S %Y').replace(
        tzinfo=datetime.timezone.utc)
    return {'engine': m.group(1), 'signatureVersion': int(m.group(2)),
            'signatureAgeHours': round((now - built).total_seconds() / 3600, 1)}


def engine_preconditions(c, cgroup, config, version):
    """Decides whether the existing engine may carry the gateway's bounded traffic."""
    labels = c['Config'].get('Labels') or {}
    assert c['State']['Running'] and (c['State'].get('Health') or {}).get('Status') == 'healthy', 'Engine not healthy'
    assert labels.get('com.docker.compose.project') == ENGINE_PROJECT, 'Unexpected engine owner'
    assert not c['HostConfig'].get('PortBindings'), 'Engine must not publish ports'
    assert not c['State'].get('OOMKilled'), 'Engine was OOM killed'
    limit, peak = cgroup['memory.max'], cgroup['memory.peak']
    assert isinstance(limit, int) and limit - peak >= ENGINE_HEADROOM_MIB, 'Engine memory headroom insufficient'
    assert cgroup.get('oom_kill', 0) == 0, 'Engine has OOM kill events'
    assert config.get('ConcurrentDatabaseReload', '').lower() == 'no' or limit >= 2 * peak, \
        'Engine reload peak could exceed its limit'
    assert parse_size(config.get('StreamMaxLength', '0')) >= STREAM_MAX_BYTES, 'Engine stream limit below gateway limit'
    assert int(config.get('MaxThreads', '0')) >= 4 * BACKEND_CONCURRENCY, 'Engine has too few threads to share'
    assert config.get('AlertExceedsMax', '').lower() == 'yes', 'Oversized archive members would scan clean'
    assert 0 <= version['signatureAgeHours'] <= MAX_SIGNATURE_AGE_HOURS, 'Engine signatures stale'
    return {'engine': ENGINE, 'memoryLimitMib': limit, 'memoryPeakMib': peak, 'headroomMib': limit - peak,
            'maxThreads': int(config['MaxThreads']), 'gatewayShare': BACKEND_CONCURRENCY,
            'streamMaxLength': config['StreamMaxLength'], **version}


def cgroup_of(cid):
    base = Path('/sys/fs/cgroup/system.slice') / ('docker-' + cid + '.scope')
    def mib(name):
        v = (base / name).read_text().strip()
        return v if v == 'max' else int(v) // 2 ** 20
    events = dict(line.split() for line in (base / 'memory.events').read_text().splitlines())
    cpu = dict(line.split() for line in (base / 'cpu.stat').read_text().splitlines())
    return {'memory.max': mib('memory.max'), 'memory.peak': mib('memory.peak'), 'memory.current': mib('memory.current'),
            'oom_kill': int(events.get('oom_kill', 0)), 'cpu_usage_s': int(cpu.get('usage_usec', 0)) / 1e6}


def engine_config():
    out = p.run('docker', 'exec', ENGINE, 'cat', '/etc/clamav/clamd.conf')
    keys = ('StreamMaxLength', 'MaxThreads', 'ConcurrentDatabaseReload', 'AlertExceedsMax', 'TCPSocket')
    found = {}
    for line in out.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and parts[0] in keys:
            found[parts[0]] = parts[1].strip()
    assert found.get('TCPSocket') == '3310', 'Engine TCP socket unexpected'
    return found


def check_engine():
    c = p.inspect(ENGINE)
    now = datetime.datetime.now(datetime.timezone.utc)
    version = parse_engine_version(p.run('docker', 'exec', ENGINE, 'clamdscan', '--version'), now)
    return engine_preconditions(c, cgroup_of(c['Id']), engine_config(), version)


def bundle(messaging_jwk):
    receiver = 'cfmeet.security-scan'
    return {'bundle_version': 1, 'receiver': receiver, 'runtime_environment': 'production',
            'trusted_issuers': [{'issuer': 'cfmeet.messaging', 'audience': receiver,
                                 'runtime_environment': 'production', 'keys': [messaging_jwk],
                                 'permissions': ['security-scan.submit']}]}


def registry():
    return {'version': 1, 'environment': 'production', 'clients': CLIENTS}


def gateway_env():
    return {
        'CF_RUNTIME_ENVIRONMENT': 'production',
        'CF_SERVICE_ID': 'cfmeet.security-scan',
        'CF_SERVICE_TRUST_BUNDLE_FILE': '/run/scan-gateway/trust.json',
        'SCAN_GATEWAY_CLIENTS_FILE': '/run/scan-gateway/clients.json',
        'SCAN_GATEWAY_VERDICT_KEY_FILE': '/run/scan-gateway/verdict.pem',
        'SCAN_GATEWAY_VERDICT_KEY_ID': VERDICT_KID,
        'SCAN_GATEWAY_BACKEND_ADDRESS': ENGINE_ALIAS + ':3310',
        'SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY': str(BACKEND_CONCURRENCY),
        'SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES': str(STREAM_MAX_BYTES),
        'SCAN_GATEWAY_MAX_SIGNATURE_AGE_HOURS': str(MAX_SIGNATURE_AGE_HOURS),
        'SCAN_GATEWAY_SCAN_TIMEOUT_MS': '60000',
        'SCAN_GATEWAY_QUEUE_TIMEOUT_MS': '2000',
        'SCAN_GATEWAY_BIND_ADDRESS': '0.0.0.0:8099',
        'SCAN_GATEWAY_METRICS_ADDRESS': '0.0.0.0:9199',
        'RUST_LOG': 'info',
    }


def run_args(name, image, work):
    """The gateway container: private, unprivileged, read-only, bounded."""
    args = ['docker', 'create', '--name', name, '--network', p.NET, '--network-alias', GATEWAY,
            '--restart', 'unless-stopped', '--memory', str(GATEWAY_MEMORY_MIB) + 'm',
            '--memory-swap', str(GATEWAY_MEMORY_MIB) + 'm', '--cpus', '0.5', '--cpu-shares', '256',
            '--pids-limit', '64', '--read-only', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges', '--user', '10002:10002',
            '--log-opt', 'max-size=20m', '--log-opt', 'max-file=3',
            '--label', LABEL + '=gateway', '--label', 'com.cfmeet.owner=cfmeet']
    for source, target in (('trust.json', 'trust.json'), ('clients.json', 'clients.json'), ('verdict.pem', 'verdict.pem')):
        args += ['--mount', f'type=bind,src={work / source},dst=/run/scan-gateway/{target},readonly']
    for key, value in gateway_env().items():
        args += ['--env', key + '=' + value]
    return args + [image]


def tls_route(text):
    """The private TLS route Messaging uses (scan + readiness only; never metrics)."""
    assert text.count(TLS_MARKER) <= 1
    if TLS_MARKER in text:
        return text
    assert f'listen {TLS_PORT} ' not in text and f'listen {TLS_PORT};' not in text, 'TLS port already used'
    common = (f'set $peer {GATEWAY}:8099; proxy_pass http://$peer$request_uri; proxy_http_version 1.1; '
              'proxy_set_header Host $host; proxy_set_header X-Forwarded-Proto https; ')
    route = (f'  server {{ listen {TLS_PORT} ssl; server_name api.cfmeet.com; client_max_body_size 26m; '
             'access_log off; '
             f'location = /internal/v1/scans {{ limit_except POST {{ deny all; }} {common}'
             'proxy_request_buffering off; proxy_buffering off; proxy_read_timeout 120s; proxy_send_timeout 120s; } '
             f'location = /ready {{ limit_except GET {{ deny all; }} {common}}} '
             'location / { return 404; } }')
    body = text.rstrip()
    assert body.endswith('}'), 'Unexpected TLS proxy configuration'
    return body[:-1].rstrip() + '\n' + TLS_MARKER + '\n' + route + '\n}\n'


def remove_tls_route(text):
    lines = text.splitlines()
    if TLS_MARKER not in lines:
        return text
    i = lines.index(TLS_MARKER)
    assert lines[i + 1].lstrip().startswith(f'server {{ listen {TLS_PORT} ssl;')
    return '\n'.join(lines[:i] + lines[i + 2:]) + '\n'


def apply_tls(transform):
    path = p.ROOT / 'tls-proxy' / 'nginx.conf'
    text = path.read_text()
    new = transform(text)
    if new == text:
        return False
    p.write_private(WORK / ('tls-before-' + str(int(time.time())) + '.conf'), text)
    path.write_text(new)  # existing read-only bind mount requires the same inode
    try:
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-t')
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-s', 'reload')
    except Exception:
        path.write_text(text)
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-s', 'reload')
        raise
    assert not p.inspect('cfmeet-core-tls')['HostConfig']['PortBindings']
    return True


def messaging_signer():
    c = p.inspect('cfmeet-messaging')
    e = p.env_of(c)
    path = Path(release.mount_source(c, e['CF_SERVICE_AUTH_PRIVATE_KEY_FILE']))
    return path, e['CF_SERVICE_AUTH_KEY_ID']


def verdict_key(image):
    """The gateway's verdict key: created once, never replaced, recoverable."""
    WORK.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = WORK / 'verdict.pem'
    recovery = p.ROOT / 'key-recovery' / 'scan-gateway' / 'verdict.pem'
    recovery.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not path.exists() and recovery.exists():
        p.write_private(path, recovery.read_text())
    keygen = WORK / 'keygen'
    keygen.mkdir(exist_ok=True, mode=0o700)
    if path.exists():
        p.write_private(keygen / 'verdict.pem', path.read_text())
    os.chown(keygen, 10002, 10002)
    if (keygen / 'verdict.pem').exists():
        os.chown(keygen / 'verdict.pem', 10002, 10002)
    public = p.run('docker', 'run', '--rm', '--network', 'none', '--user', '10002:10002',
                   '--mount', f'type=bind,src={keygen},dst=/keys', '--entrypoint', '/app/security-scan-gateway-keygen',
                   image, VERDICT_KID, '/keys/verdict.pem')
    created = (keygen / 'verdict.pem').read_text()
    if path.exists():
        assert path.read_text() == created, 'Verdict key changed; refusing replacement'
    else:
        p.write_private(path, created)
    if recovery.exists():
        assert recovery.read_text() == created, 'Recovery verdict key differs; refusing replacement'
    else:
        p.write_private(recovery, created)
    (keygen / 'verdict.pem').unlink()
    os.chown(path, 10002, 10002)
    os.chmod(path, 0o400)
    keys = json.loads(public)
    assert [k['kid'] for k in keys['keys']] == [VERDICT_KID]
    release.save(WORK / 'verdict-keys.json', json.dumps(keys), False)
    return keys


def connect_engine(run_id):
    nets = p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines()
    if BACKEND_NET not in nets:
        p.run('docker', 'network', 'create', '--driver', 'bridge', '--internal',
              '--label', LABEL + '=backend', BACKEND_NET)
    info = json.loads(p.run('docker', 'network', 'inspect', BACKEND_NET))[0]
    assert info['Internal'] and (info.get('Labels') or {}).get(LABEL) == 'backend', 'Unexpected backend network'
    attached = (p.inspect(ENGINE)['NetworkSettings'].get('Networks') or {})
    if BACKEND_NET not in attached:
        p.write_private(WORK / ('engine-attached-' + run_id + '.json'),
                        json.dumps({'engine': ENGINE, 'network': BACKEND_NET, 'runId': run_id}))
        p.run('docker', 'network', 'connect', '--alias', ENGINE_ALIAS, BACKEND_NET, ENGINE)
    members = sorted(m['Name'] for m in json.loads(p.run('docker', 'network', 'inspect', BACKEND_NET))[0]['Containers'].values())
    allowed = {ENGINE} | {n for n in members if n == GATEWAY or n.startswith(GATEWAY + '-')}
    assert set(members) <= allowed, 'Backend network has unexpected members'
    return members


def readiness(url):
    out = p.run('docker', 'run', '--rm', '--network', p.NET, 'curlimages/curl:8.12.1', '-sS', '--max-time', '15', url + '/ready')
    return json.loads(out)


def wait_ready(name):
    for attempt in range(40):
        try:
            ready = readiness('http://' + name + ':8099')
            if ready.get('status') == 'ready':
                return ready
        except (RuntimeError, ValueError):
            pass
        time.sleep(3)
    raise RuntimeError('Scan gateway readiness failed')


def probe(image):
    """Live acceptance from the caller's network, signed with Messaging's own key."""
    key, kid = messaging_signer()
    env = {'PROBE_SIGNER_KEY_FILE': '/run/probe/signer.pem', 'PROBE_SIGNER_KID': kid,
           'PROBE_ISSUER': 'cfmeet.messaging', 'PROBE_ENVIRONMENT': 'production',
           'PROBE_VERDICT_KEYS_FILE': '/run/probe/verdict-keys.json',
           'PROBE_TARGETS': 'http://' + GATEWAY + ':8099,' + URL,
           'PROBE_FORBIDDEN_ADDRESSES': ENGINE + ':3310,' + ENGINE_ALIAS + ':3310',
           'PROBE_MAX_BYTES': str(STREAM_MAX_BYTES), 'PROBE_BURST': '6'}
    owner = key.stat().st_uid
    args = ['docker', 'run', '--rm', '--network', p.NET, '--memory', '256m', '--cpus', '1', '--user', f'{owner}:{owner}',
            '--mount', f'type=bind,src={key},dst=/run/probe/signer.pem,readonly',
            '--mount', f'type=bind,src={WORK / "verdict-keys.json"},dst=/run/probe/verdict-keys.json,readonly',
            '--entrypoint', '/app/security-scan-gateway-probe']
    for k, v in env.items():
        args += ['--env', k + '=' + v]
    result = subprocess.run(args + [image], capture_output=True, text=True, timeout=900)
    try:
        evidence = json.loads(result.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        # The probe prints no secret; keep only a short, character-filtered tail.
        tail = re.sub(r'[^A-Za-z0-9 _.:/=()-]', '', result.stderr[-300:])
        evidence = {'pass': False, 'failures': ['probe produced no evidence'], 'exitCode': result.returncode,
                    'stderrTail': tail}
    return evidence, result.returncode == 0 and evidence.get('pass') is True


def stats(name):
    out = p.run('docker', 'stats', '--no-stream', '--format', '{{.MemUsage}}\t{{.CPUPerc}}', name)
    mem, cpu = out.split('\t')
    return {'memory': mem.split('/')[0].strip(), 'cpu': cpu.strip()}


def host_memory():
    info = {line.split(':')[0]: int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines()}
    return {'memAvailableMib': info['MemAvailable'] // 1024, 'swapUsedMib': (info['SwapTotal'] - info['SwapFree']) // 1024,
            'loadavg': Path('/proc/loadavg').read_text().split()[:3]}


def verify(run_id, image=None):
    image = image or p.inspect(GATEWAY)['Config']['Image']
    engine_before = cgroup_of(p.inspect(ENGINE)['Id'])
    gateway_before = cgroup_of(p.inspect(GATEWAY)['Id'])
    lk_before = cgroup_of(p.inspect('lk-server')['Id'])
    host_before = host_memory()
    started = time.time()
    evidence, ok = probe(image)
    elapsed = time.time() - started
    engine_after = cgroup_of(p.inspect(ENGINE)['Id'])
    gateway_after = cgroup_of(p.inspect(GATEWAY)['Id'])
    lk_after = cgroup_of(p.inspect('lk-server')['Id'])
    resources = {
        'probeSeconds': round(elapsed, 1),
        'hostBefore': host_before, 'hostAfter': host_memory(),
        'gateway': {'memoryCurrentMib': gateway_after['memory.current'], 'memoryPeakMib': gateway_after['memory.peak'],
                    'memoryLimitMib': gateway_after['memory.max'],
                    'cpuSecondsDuringProbe': round(gateway_after['cpu_usage_s'] - gateway_before['cpu_usage_s'], 2)},
        'engine': {'memoryBeforeMib': engine_before['memory.current'], 'memoryAfterMib': engine_after['memory.current'],
                   'memoryPeakMib': engine_after['memory.peak'], 'memoryLimitMib': engine_after['memory.max'],
                   'oomKills': engine_after['oom_kill'],
                   'cpuSecondsDuringProbe': round(engine_after['cpu_usage_s'] - engine_before['cpu_usage_s'], 2)},
        'livekitCpuSecondsDuringProbe': round(lk_after['cpu_usage_s'] - lk_before['cpu_usage_s'], 2),
    }
    engine_health = p.inspect(ENGINE)['State'].get('Health', {}).get('Status')
    receipt = {'operation': 'scan-gateway-verify', 'runId': run_id, 'image': image,
               'imageId': p.inspect(GATEWAY)['Image'], 'ready': readiness(URL), 'acceptance': evidence,
               'resources': resources, 'engineHealthAfter': engine_health,
               'backendNetworkMembers': sorted(m['Name'] for m in json.loads(
                   p.run('docker', 'network', 'inspect', BACKEND_NET))[0]['Containers'].values()),
               'gatewayPublishedPorts': p.inspect(GATEWAY)['HostConfig']['PortBindings']}
    p.write_private(WORK / ('verify-' + run_id + '.json'), json.dumps(receipt, sort_keys=True))
    print(json.dumps({'scanGateway': receipt}, sort_keys=True))
    assert ok, 'Scan gateway acceptance failed'
    assert engine_health == 'healthy' and engine_after['oom_kill'] == engine_before['oom_kill'], 'Engine owner affected'
    assert not receipt['gatewayPublishedPorts'], 'Gateway must stay private'
    p.regression()
    return receipt


def configure(sha, run_id):
    p.identity()
    WORK.mkdir(parents=True, exist_ok=True, mode=0o700)
    image = image_ref(sha)
    labels = json.loads(p.run('docker', 'image', 'inspect', image))[0]['Config'].get('Labels') or {}
    assert labels.get('org.opencontainers.image.revision') == sha, 'Gateway image revision mismatch'
    engine = check_engine()
    host = host_memory()
    assert host['memAvailableMib'] >= GATEWAY_MEMORY_MIB + 512 + 3072, 'Insufficient host memory for gateway and release margin'
    print(json.dumps({'scanGatewayPreconditions': {'engine': engine, 'host': host}}, sort_keys=True), flush=True)
    keys = verdict_key(image)
    key, kid = messaging_signer()
    jwk = release.public_key(key, kid)
    release.save(WORK / 'trust.json', json.dumps(bundle(jwk)), False)
    release.save(WORK / 'clients.json', json.dumps(registry()), False)
    engine_was_attached = BACKEND_NET in (p.inspect(ENGINE)['NetworkSettings'].get('Networks') or {})
    route_existed = TLS_MARKER in (p.ROOT / 'tls-proxy' / 'nginx.conf').read_text()
    members = connect_engine(run_id)
    previous = GATEWAY + '-rollback-' + run_id
    existed = GATEWAY in names()
    if existed:
        p.run('docker', 'rename', GATEWAY, previous)
        p.run('docker', 'stop', '--time', '10', previous)
    try:
        p.run(*run_args(GATEWAY, image, WORK))
        p.run('docker', 'network', 'connect', BACKEND_NET, GATEWAY)
        p.run('docker', 'start', GATEWAY)
        ready = wait_ready(GATEWAY)
        route_added = apply_tls(tls_route)
        receipt = verify(run_id, image)
    except Exception:
        if GATEWAY in names():
            subprocess.run(['docker', 'stop', '--time', '10', GATEWAY], capture_output=True)
            subprocess.run(['docker', 'rename', GATEWAY, GATEWAY + '-failed-' + run_id], capture_output=True)
        if existed:
            p.run('docker', 'rename', previous, GATEWAY)
            p.run('docker', 'start', GATEWAY)
        else:
            # First deployment failed: leave nothing behind (route, engine attachment).
            if not route_existed:
                apply_tls(remove_tls_route)
            if not engine_was_attached and BACKEND_NET in (p.inspect(ENGINE)['NetworkSettings'].get('Networks') or {}):
                subprocess.run(['docker', 'network', 'disconnect', BACKEND_NET, ENGINE], capture_output=True)
        raise
    print(json.dumps({'operation': 'scan-gateway-configure', 'runId': run_id, 'image': image,
                      'verdictKeys': keys, 'ready': ready, 'tlsRouteAdded': route_added,
                      'backendNetworkMembers': members, 'rollbackContainer': previous if existed else None,
                      'dedicatedAntivirusEngineStarted': False}, sort_keys=True))
    return receipt


def disable(run_id):
    """Fail closed: stop the gateway and undo the engine's extra network attachment."""
    if GATEWAY in names():
        p.run('docker', 'stop', '--time', '10', GATEWAY)
        p.run('docker', 'rename', GATEWAY, GATEWAY + '-disabled-' + run_id)
    apply_tls(remove_tls_route)
    if BACKEND_NET in (p.inspect(ENGINE)['NetworkSettings'].get('Networks') or {}):
        p.run('docker', 'network', 'disconnect', BACKEND_NET, ENGINE)
    assert BACKEND_NET not in (p.inspect(ENGINE)['NetworkSettings'].get('Networks') or {})
    assert p.inspect(ENGINE)['State']['Running'], 'Engine owner affected'
    print(json.dumps({'operation': 'scan-gateway-disable', 'runId': run_id,
                      'engineNetworks': sorted(p.inspect(ENGINE)['NetworkSettings']['Networks'])}))
    p.regression()


def run(operation, sha, run_id):
    if operation == 'scan-gateway-configure':
        configure(sha, run_id)
    elif operation == 'scan-gateway-verify':
        verify(run_id)
    elif operation == 'scan-gateway-disable':
        disable(run_id)
    else:
        raise AssertionError('Unsupported scan gateway operation')
