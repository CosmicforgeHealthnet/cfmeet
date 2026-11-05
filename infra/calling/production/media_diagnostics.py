"""Read-only diagnosis of intermittent media-connection timeouts and VPS headroom.

Nothing here writes production state: it reads /proc, Docker metadata and logs,
kernel/sysstat history, a count-only database query and credential-less probes.
Logs never leave the VPS raw. Only redacted message templates, counts, timings and
resource figures are printed: no URLs, query strings, tokens, keys, addresses,
identities or account data.
"""
import collections
import datetime
import json
import os
import re
import shutil
import socket
import ssl
import subprocess
import time
from pathlib import Path

# Failed and control windows (UTC), taken from the public GitHub job logs.
WINDOWS = [
    # run id, image, outcome, start, end, failing stage reported by the job
    ('41794968776', '787bb9a', 'FAIL', '2026-11-04T07:51:30', '2026-11-04T07:55:30', 'communications connect TimeoutError'),
    ('41795985590', '787bb9a', 'PASS', '2026-11-04T07:59:30', '2026-11-04T08:09:00', 'control'),
    ('41797479652', '787bb9a', 'FAIL', '2026-11-04T08:11:30', '2026-11-04T08:12:30', 'native call did not become CONNECTED'),
    ('41798211419', '787bb9a', 'FAIL', '2026-11-04T08:17:00', '2026-11-04T08:18:30', 'public-id rotation 429'),
    ('41798927157', '787bb9a', 'PASS', '2026-11-04T08:22:30', '2026-11-04T08:24:00', 'control'),
    ('41972295095', '0a5d9dd', 'PASS', '2026-11-05T05:10:30', '2026-11-05T05:14:30', 'isolated staging control'),
    ('41972828392', '0a5d9dd', 'FAIL', '2026-11-05T05:19:00', '2026-11-05T05:27:40', 'enterprise ivr_routing URLError'),
    ('41974711025', '0a5d9dd', 'FAIL', '2026-11-05T05:33:50', '2026-11-05T05:39:00', 'public-id rotation 429'),
    ('41975667093', '0a5d9dd', 'FAIL', '2026-11-05T05:42:50', '2026-11-05T05:55:00', 'enterprise extensions TimeoutError'),
    ('41977529784', '0a5d9dd', 'FAIL', '2026-11-05T06:01:20', '2026-11-05T06:06:30', 'communications video connect TimeoutError'),
    ('41979206960', '787bb9a', 'PASS', '2026-11-05T06:15:30', '2026-11-05T06:22:30', 'enterprise control'),
    ('41980193898', '0a5d9dd', 'FAIL', '2026-11-05T06:25:20', '2026-11-05T06:33:50', 'enterprise transfer media'),
    ('41981794294', '0a5d9dd', 'FAIL', '2026-11-05T06:39:30', '2026-11-05T06:44:30', 'enterprise reconnect'),
    ('41992435685', '0a5d9dd', 'FAIL', '2026-11-05T08:11:20', '2026-11-05T08:15:30', 'communications voice connect TimeoutError'),
]

_SUBS = [
    (re.compile(r'eyJ[\w-]+\.[\w-]+\.[\w-]+'), '<jwt>'),
    (re.compile(r'\b[a-z][a-z0-9+.-]*://\S+', re.I), '<url>'),
    (re.compile(r'(?i)\b(token|secret|password|passwd|key|authorization|bearer|signature|credential)s?\b(["\']?\s*[:=]\s*|\s+)\S+'), r'\1=<redacted>'),
    (re.compile(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b', re.I), '<id>'),
    (re.compile(r'\b(?:PA|TR|RM|SR|EG|IN|AG)_[A-Za-z0-9]+\b'), '<sid>'),
    (re.compile(r'\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b'), '<ip>'),
    (re.compile(r'(?<![\w:])[0-9a-f]{1,4}(?::[0-9a-f]{0,4}){2,7}(?![\w:])', re.I), '<ip6>'),
    (re.compile(r'[A-Za-z0-9+/_=-]{24,}'), '<tok>'),
    (re.compile(r'(?<![<\w])\d+(?:\.\d+)?(?:ms|s|µs|us|ns|m|h)?\b'), '<n>'),
]
_STAMP = re.compile(r'^\s*(\d{4}[-/]\d\d[-/]\d\d[T ]\d\d:\d\d:\d\d(?:[.,]\d+)?(?:Z|[+-]\d\d:?\d\d)?)\s*')


def redact(text, limit=160):
    """Reduce one log line to a stable, secret-free template."""
    for pattern, replacement in _SUBS:
        text = pattern.sub(replacement, text)
    return ' '.join(text.split())[:limit]


def stamp(line):
    match = _STAMP.match(line)
    if not match:
        return None, line
    raw = match.group(1).replace('/', '-').replace(',', '.').replace(' ', 'T').rstrip('Z')
    raw = re.sub(r'([+-]\d\d):?(\d\d)$', '', raw)
    try:
        when = datetime.datetime.fromisoformat(raw[:26])
    except ValueError:
        return None, line
    return when, line[match.end():]


def template(line):
    """Message template of a JSON, tab-separated (zap) or plain log line."""
    _, body = stamp(line)
    _, body = stamp(body)
    body = body.strip()
    if body.startswith('{'):
        try:
            value = json.loads(body)
        except ValueError:
            value = None
        if isinstance(value, dict):
            fields = value.get('fields') if isinstance(value.get('fields'), dict) else {}
            message = value.get('msg') or value.get('message') or fields.get('message') or ''
            level = value.get('level') or value.get('lvl') or ''
            target = value.get('target') or value.get('logger') or ''
            return redact(f'{level} {target} {message}')
    parts = body.split('\t')
    if len(parts) >= 5:
        # zap console: level, logger, caller, message, structured fields
        return redact(' '.join(parts[:2] + [parts[3]]))
    if ', client:' in body:
        # nginx error log: drop client/request/upstream (request URIs carry tokens)
        body = body.split(', client:', 1)[0]
        body = re.sub(r'^\[(\w+)\] \d+#\d+: \*\d+ ', r'[\1] ', body)
    return redact(body)


def histogram(lines, top=40):
    counts = collections.Counter(template(line) for line in lines if line.strip())
    return [[name, count] for name, count in counts.most_common(top)]


def sh(*args, timeout=60):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return result.stdout + result.stderr if result.returncode == 0 else None


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True), flush=True)


def proc_fields(path, names):
    values = {}
    for line in Path(path).read_text().splitlines():
        key = line.split(':', 1)[0]
        if key in names:
            values[key] = int(line.split()[1])
    return values


def host_resources():
    meminfo = proc_fields('/proc/meminfo', {'MemTotal', 'MemAvailable', 'MemFree', 'Cached', 'SwapTotal', 'SwapFree', 'Committed_AS', 'CommitLimit'})
    pressure = {}
    for kind in ('cpu', 'memory', 'io'):
        path = Path('/proc/pressure') / kind
        if path.exists():
            pressure[kind] = {line.split()[0]: {k: float(v) for k, v in (x.split('=') for x in line.split()[1:])}
                              for line in path.read_text().splitlines()}
    disks = {}
    for label, path in (('root', '/'), ('docker', '/var/lib/docker'), ('core', '/opt/cfmeet-core')):
        if Path(path).exists():
            usage = shutil.disk_usage(path)
            disks[label] = {'total_gib': round(usage.total / 2**30, 1), 'free_gib': round(usage.free / 2**30, 1),
                            'used_percent': round(100 * usage.used / usage.total, 1)}
    swappiness = Path('/proc/sys/vm/swappiness').read_text().strip()
    emit('hostResources', {'cpus': os.cpu_count(), 'loadavg': Path('/proc/loadavg').read_text().split()[:3],
        'uptime_hours': round(float(Path('/proc/uptime').read_text().split()[0]) / 3600, 1),
        'meminfo_kib': meminfo, 'swappiness': swappiness, 'pressure': pressure, 'disks': disks,
        'steal_and_iowait': cpu_split()})


def cpu_split(seconds=5):
    def read():
        return [int(x) for x in Path('/proc/stat').read_text().splitlines()[0].split()[1:9]]
    first = read(); time.sleep(seconds); second = read()
    delta = [b - a for a, b in zip(first, second)]
    total = sum(delta) or 1
    names = ('user', 'nice', 'system', 'idle', 'iowait', 'irq', 'softirq', 'steal')
    return {name: round(100 * value / total, 1) for name, value in zip(names, delta)}


def kernel_events():
    events = []
    output = sh('journalctl', '-k', '--since', '2026-11-01', '-o', 'short-iso', '--no-pager', timeout=90) or ''
    if not output:
        output = sh('dmesg', '-T') or ''
    patterns = {'oom_kill': re.compile(r'Out of memory|oom-kill|Killed process'),
                'conntrack_full': re.compile(r'nf_conntrack: table full'),
                'syn_flood': re.compile(r'Possible SYN flooding'),
                'hung_task': re.compile(r'blocked for more than'),
                'soft_lockup': re.compile(r'soft lockup')}
    victims = collections.Counter()
    for line in output.splitlines():
        for name, pattern in patterns.items():
            if pattern.search(line):
                when = line.split(' ', 1)[0]
                victim = re.search(r'Killed process \d+ \(([\w.-]+)\)', line)
                if victim:
                    victims[victim.group(1)] += 1
                events.append([name, when[:19]])
    emit('kernelEvents', {'source': 'journalctl' if output else 'unavailable', 'count': len(events),
        'by_type': dict(collections.Counter(e[0] for e in events)), 'oom_victims': dict(victims),
        'recent': events[-20:]})


def sysstat_history():
    if not shutil.which('sar'):
        emit('sysstat', 'NOT_INSTALLED')
        return
    rows = {}
    for day in sorted({w[3][:10] for w in WINDOWS}):
        files = [Path('/var/log/sysstat') / ('sa' + day[8:10]), Path('/var/log/sa') / ('sa' + day[8:10])]
        source = next((f for f in files if f.exists()), None)
        if not source:
            rows[day] = 'NO_DATA_FILE'
            continue
        rows[day] = {}
        for flag in ('-u', '-r', '-q', '-W'):
            output = sh('sar', flag, '-f', str(source))
            rows[day][flag] = [line.split() for line in (output or '').splitlines()
                               if re.match(r'^\d\d:\d\d:\d\d', line)][-200:]
    emit('sysstat', rows)


def container_resources():
    stats = sh('docker', 'stats', '--no-stream', '--format', '{{json .}}', timeout=90) or ''
    current = []
    for line in stats.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        current.append({k: value.get(k) for k in ('Name', 'CPUPerc', 'MemUsage', 'MemPerc', 'PIDs')})
    names = (sh('docker', 'ps', '-a', '--format', '{{.Names}}\t{{.State}}') or '').splitlines()
    limits = []
    for row in names:
        name, state = (row.split('\t') + [''])[:2]
        if state != 'running':
            continue
        c = json.loads(sh('docker', 'inspect', name) or '[{}]')[0]
        host = c.get('HostConfig', {})
        limits.append({'name': name, 'memory_limit_mib': (host.get('Memory') or 0) // 2**20,
            'memory_reservation_mib': (host.get('MemoryReservation') or 0) // 2**20,
            'cpus': (host.get('NanoCpus') or 0) / 1e9, 'cpu_shares': host.get('CpuShares'),
            'network_mode': host.get('NetworkMode'), 'restart_count': c.get('RestartCount'),
            'oom_killed': c.get('State', {}).get('OOMKilled'), 'started': c.get('State', {}).get('StartedAt', '')[:19],
            'log_driver': host.get('LogConfig', {}).get('Type'),
            'log_rotation': bool(host.get('LogConfig', {}).get('Config'))})
    exited = [row.split('\t')[0] for row in names if row.endswith('\texited')]
    emit('containerResources', {'running': current, 'limits': limits,
        'exited_count': len(exited), 'exited_failed_or_rollback': sorted(n for n in exited if 'failed' in n or 'rollback' in n)})
    info = json.loads(sh('docker', 'info', '--format', '{{json .}}') or '{}')
    emit('dockerEngine', {k: info.get(k) for k in ('NCPU', 'MemTotal', 'LoggingDriver', 'CgroupVersion', 'ServerVersion', 'Containers', 'Images')})
    emit('dockerDiskUsage', [line.split() for line in (sh('docker', 'system', 'df') or '').splitlines()])


def network_state():
    snmp = Path('/proc/net/snmp').read_text().splitlines()
    netstat = Path('/proc/net/netstat').read_text().splitlines()

    def pairs(lines, prefix):
        rows = [line.split()[1:] for line in lines if line.startswith(prefix)]
        return dict(zip(rows[0], map(int, rows[1]))) if len(rows) == 2 else {}
    udp = pairs(snmp, 'Udp:'); tcp = pairs(snmp, 'Tcp:'); ext = pairs(netstat, 'TcpExt:')
    sysctl = {}
    for name in ('net/core/rmem_max', 'net/core/wmem_max', 'net/core/somaxconn', 'net/core/netdev_max_backlog',
                 'net/netfilter/nf_conntrack_count', 'net/netfilter/nf_conntrack_max', 'net/ipv4/ip_local_port_range'):
        path = Path('/proc/sys') / name
        sysctl[name] = path.read_text().strip() if path.exists() else None
    firewall = []
    if shutil.which('ufw'):
        status = sh('ufw', 'status') or ''
        firewall = [redact(line) for line in status.splitlines() if re.search(r'udp|7880|7881|7882|3478|5349|50\d\d\d|60\d\d\d', line)]
    emit('networkState', {'udp': {k: udp.get(k) for k in ('InDatagrams', 'InErrors', 'RcvbufErrors', 'SndbufErrors', 'NoPorts')},
        'tcp': {k: tcp.get(k) for k in ('CurrEstab', 'RetransSegs', 'AttemptFails', 'EstabResets')},
        'tcp_ext': {k: ext.get(k) for k in ('ListenOverflows', 'ListenDrops', 'TCPTimeouts', 'TCPAbortOnTimeout')},
        'socket_summary': [redact(x) for x in (sh('ss', '-s') or '').splitlines()[:6]],
        'sysctl': sysctl, 'firewall_media_rules': firewall})


def livekit_config(public_ip):
    import yaml
    config = yaml.safe_load(Path('/opt/livekit/livekit.yaml').read_text()) or {}
    rtc = config.get('rtc') or {}
    turn = config.get('turn') or {}
    room = config.get('room') or {}
    safe_rtc = {k: rtc.get(k) for k in ('tcp_port', 'udp_port', 'port_range_start', 'port_range_end', 'use_external_ip',
        'use_ice_lite', 'enable_loopback_candidate', 'allow_tcp_fallback', 'packet_buffer_size', 'congestion_control')}
    safe_rtc['node_ip_configured'] = bool(rtc.get('node_ip'))
    safe_rtc['node_ip_is_public_dns_address'] = rtc.get('node_ip') == public_ip if rtc.get('node_ip') else None
    safe_rtc['stun_server_count'] = len(rtc.get('stun_servers') or [])
    safe_rtc['interfaces_configured'] = bool(rtc.get('interfaces'))
    safe_rtc['ips_configured'] = bool(rtc.get('ips'))
    container = json.loads(sh('docker', 'inspect', 'lk-server') or '[{}]')[0]
    host = container.get('HostConfig', {})
    emit('livekitConfig', {'rtc': safe_rtc, 'turn': {k: turn.get(k) for k in ('enabled', 'udp_port', 'tls_port', 'external_tls')},
        'room': {k: room.get(k) for k in ('auto_create', 'empty_timeout', 'departure_timeout', 'max_participants')},
        'limit': config.get('limit'), 'logging_level': (config.get('logging') or {}).get('level'),
        'keys_count': len(config.get('keys') or {}), 'redis_configured': bool(config.get('redis')),
        'network_mode': host.get('NetworkMode'), 'published': sorted((host.get('PortBindings') or {}).keys()),
        'memory_limit_mib': (host.get('Memory') or 0) // 2**20, 'cpus': (host.get('NanoCpus') or 0) / 1e9,
        'image': container.get('Image'), 'started': container.get('State', {}).get('StartedAt', '')[:19],
        'restart_count': container.get('RestartCount')})
    startup = (sh('docker', 'logs', '--tail', '4000', 'lk-server', timeout=90) or '').splitlines()
    facts = {}
    for line in startup:
        tail = line[line.find('{'):] if '{' in line else ''
        try:
            fields = json.loads(tail) if tail else {}
        except ValueError:
            continue
        for key in ('nodeIP', 'portICERange', 'rtc.portTCP', 'rtc.portUDP', 'portUDP', 'portTCP', 'bindAddresses', 'version'):
            if key in fields:
                value = fields[key]
                facts[key] = (value == public_ip) if key == 'nodeIP' else redact(json.dumps(value))
    if 'nodeIP' in facts:
        facts['nodeIP_equals_public_dns_address'] = facts.pop('nodeIP')
    emit('livekitStartupFacts', facts)


def active_media():
    query = "SELECT current_state, count(*) FROM calls WHERE current_state NOT IN ('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED') GROUP BY 1;"
    output = sh('docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'psql', '-XAtq', '-U', 'postgres', '-d', 'cfmeet_calling', '-c', query)
    emit('activeCalls', [line.split('|') for line in (output or '').splitlines()] if output is not None else 'UNREADABLE')


def probe_latency(public_ip):
    """Credential-less timings for each hop. Every request is a read."""
    def timed(fn, attempts=15):
        results = []
        for _ in range(attempts):
            before = time.monotonic()
            try:
                status = fn()
                results.append((round(time.monotonic() - before, 3), status))
            except Exception as error:
                results.append((round(time.monotonic() - before, 3), type(error).__name__))
            time.sleep(0.2)
        seconds = sorted(r[0] for r in results)
        return {'p50': seconds[len(seconds) // 2], 'max': seconds[-1],
                'outcomes': dict(collections.Counter(str(r[1]) for r in results))}

    def https(path, upgrade=False, address=None, port=443):
        with socket.create_connection((address or 'api.cfmeet.com', port), timeout=10) as raw:
            with ssl.create_default_context().wrap_socket(raw, server_hostname='api.cfmeet.com') as tls:
                tls.settimeout(10)
                headers = 'Host: api.cfmeet.com\r\nConnection: close\r\n'
                if upgrade:
                    headers = ('Host: api.cfmeet.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n'
                               'Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n')
                tls.sendall(f'GET {path} HTTP/1.1\r\n{headers}\r\n'.encode())
                return int(tls.recv(256).split(b'\r\n', 1)[0].split()[1])

    def plain(port, path):
        with socket.create_connection(('127.0.0.1', port), timeout=10) as s:
            s.settimeout(10)
            s.sendall(f'GET {path} HTTP/1.1\r\nHost: api.cfmeet.com\r\nConnection: close\r\n\r\n'.encode())
            return int(s.recv(256).split(b'\r\n', 1)[0].split()[1])
    emit('hopLatencySeconds', {
        'public_tls_health': timed(lambda: https('/health')),
        'public_media_gateway_upgrade_without_credential': timed(lambda: https('/v1/calling/media/rtc', upgrade=True)),
        'host_loopback_calling_health': timed(lambda: plain(18090, '/health')),
        'livekit_direct_root': timed(lambda: plain(7880, '/')),
    })


def window_logs():
    names = (sh('docker', 'ps', '-a', '--format', '{{.Names}}') or '').splitlines()
    interesting = [n for n in names if n in ('lk-server', 'cfmeet-calling', 'cfmeet-calling-loopback', 'cfmeet-core-tls')
                   or re.match(r'^cfmeet-calling(-communications-failed-\d+|-failed-\d+)', n)]
    spans = {}
    for name in interesting:
        state = json.loads(sh('docker', 'inspect', name) or '[{}]')[0].get('State', {})
        spans[name] = (state.get('StartedAt', '')[:19], state.get('FinishedAt', '')[:19] if not state.get('Running') else '9999')
    access = [Path('/var/log/nginx/access.log'), Path('/var/log/nginx/access.log.1')]
    errors = [Path('/var/log/nginx/error.log'), Path('/var/log/nginx/error.log.1')]
    access_lines = [l for f in access if f.exists() for l in f.read_text(errors='replace').splitlines()]
    error_lines = [l for f in errors if f.exists() for l in f.read_text(errors='replace').splitlines()]
    month = {m: i + 1 for i, m in enumerate(('Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'))}
    access_re = re.compile(r'\[(\d\d)/(\w{3})/(\d{4}):(\d\d:\d\d:\d\d) ([+-]\d{4})\] "(\w+) (\S+)[^"]*" (\d{3})')
    local_offset = datetime.datetime.now().astimezone().utcoffset()
    parsed_access = []
    for line in access_lines:
        m = access_re.search(line)
        if m:
            offset = datetime.timedelta(hours=int(m.group(5)[:3]), minutes=int(m.group(5)[0] + m.group(5)[3:]))
            when = datetime.datetime.fromisoformat(f'{m.group(3)}-{month.get(m.group(2), 1):02d}-{m.group(1)}T{m.group(4)}') - offset
            path = m.group(7).split('?', 1)[0]
            parsed_access.append((when, '/'.join(path.split('/')[:4]), m.group(8)))
    for run, image, outcome, start, end, stage in WINDOWS:
        since, until = datetime.datetime.fromisoformat(start), datetime.datetime.fromisoformat(end)
        report = {'run': run, 'image': image, 'outcome': outcome, 'reported_stage': stage, 'containers': {}}
        for name in interesting:
            began, finished = spans[name]
            if began and finished and (finished < start or began > end):
                continue
            output = sh('docker', 'logs', '--timestamps', '--since', start + 'Z', '--until', end + 'Z', name, timeout=120) or ''
            lines = output.splitlines()
            if not lines:
                continue
            warn = [l for l in lines if re.search(r'(?i)\b(warn|warning|error|fatal|panic)\b', l)]
            first_warn = {}
            for line in warn:
                when, _ = stamp(line)
                key = template(line)
                if key not in first_warn and when:
                    first_warn[key] = when.isoformat()[:23]
            report['containers'][name] = {'lines': len(lines), 'templates': histogram(lines, 30),
                'warn_error_first_seen': sorted(first_warn.items(), key=lambda x: x[1])[:25]}
        media = [(w, p, s) for w, p, s in parsed_access if since <= w <= until and p.startswith('/v1/calling')]
        report['nginx_calling_requests'] = {f'{p} {s}': c for (p, s), c in collections.Counter((p, s) for _, p, s in media).most_common(25)}
        window_errors = []
        for line in error_lines:
            m = re.match(r'(\d{4})/(\d\d)/(\d\d) (\d\d:\d\d:\d\d)', line)
            if m and since <= datetime.datetime.fromisoformat(f'{m.group(1)}-{m.group(2)}-{m.group(3)}T{m.group(4)}') - local_offset <= until:
                window_errors.append(line[20:])
        report['nginx_error_templates'] = histogram(window_errors, 15)
        kernel = sh('journalctl', '-k', '--since', start.replace('T', ' ') + ' UTC', '--until', end.replace('T', ' ') + ' UTC', '-o', 'cat', '--no-pager') or ''
        report['kernel_templates'] = histogram(kernel.splitlines(), 10)
        emit('failureWindow', report)


def client_logs():
    root = Path('/opt/cfmeet-core')
    for run, image, outcome, start, end, stage in WINDOWS:
        folders = [root / 'communications' / ('production-test-' + run), root / 'communications' / ('staging-test-' + run)]
        folders += sorted((root / 'production-evidence').glob('api-' + run + '*'))
        for folder in folders:
            files = sorted(folder.glob('rtc-*.private.log'), key=lambda f: f.stat().st_mtime) if folder.is_dir() else []
            if not files:
                continue
            summary = []
            flagged = [f for f in files if re.search(r'(?i)timed out|error|failed|warn', f.read_text(errors='replace'))]
            for file in (flagged[-4:] + [f for f in files[-2:] if f not in flagged])[:6]:
                sequence = []
                base = None
                for line in file.read_text(errors='replace').splitlines():
                    when, _ = stamp(line)
                    if when is None:
                        continue
                    base = base or when
                    key = template(line)
                    if not sequence or sequence[-1][1] != key:
                        sequence.append([round((when - base).total_seconds(), 2), key])
                summary.append({'flagged': file in flagged, 'lines': len(sequence),
                                'started': base.isoformat()[:23] if base else None, 'sequence': sequence[:70]})
            emit('clientSdkLog', {'run': run, 'image': image, 'outcome': outcome, 'folder_kind': folder.name.split('-')[0],
                                  'files': len(files), 'flagged_files': len(flagged), 'samples': summary})


_SAFE_VALUE = re.compile(r'^[A-Za-z_]{1,24}$')
_FIELD_KEYS = ('connectionType', 'reason', 'protocol', 'candidateType', 'iceConnectionType', 'transport', 'isTCP', 'selectedCandidatePair')


def safe_fields(line):
    """Whitelisted enum-like LiveKit fields only (e.g. connectionType=udp)."""
    tail = line[line.find('{'):] if '{' in line else ''
    try:
        fields = json.loads(tail) if tail else {}
    except ValueError:
        return {}
    out = {}
    for key in _FIELD_KEYS:
        value = fields.get(key)
        if isinstance(value, bool):
            out[key] = value
        elif isinstance(value, str):
            out[key] = value if _SAFE_VALUE.match(value) else '<v>'
    return out


def stun_probe(address, port):
    """One STUN Binding Request: a read-only reachability check. No allocation."""
    request = bytes.fromhex('0001000021124a42') + os.urandom(12)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(2)
        try:
            s.sendto(request, (address, port))
            data = s.recv(512)
            return 'STUN_RESPONSE' if data[:2] in (bytes.fromhex('0101'), bytes.fromhex('0111')) else 'OTHER_RESPONSE'
        except ConnectionRefusedError:
            return 'ICMP_PORT_UNREACHABLE'
        except socket.timeout:
            return 'NO_RESPONSE'


def media_path_facts(public_ip):
    container = json.loads(sh('docker', 'inspect', 'lk-server') or '[{}]')[0]
    bindings = {port: sorted({(b.get('HostIp') or '*') == '*' or b.get('HostIp') in ('0.0.0.0', '::') for b in (v or [])})
                for port, v in (container.get('HostConfig', {}).get('PortBindings') or {}).items()}
    pid = container.get('State', {}).get('Pid')
    listeners = {}
    if pid and shutil.which('nsenter'):
        for proto, flag in (('udp', '-Hulpn'), ('tcp', '-Htlpn')):
            output = sh('nsenter', '-t', str(pid), '-n', 'ss', flag) or ''
            ports = sorted({int(line.split()[3].rsplit(':', 1)[1]) for line in output.splitlines()
                            if len(line.split()) > 3 and line.split()[3].rsplit(':', 1)[-1].isdigit()})
            listeners[proto] = {'count': len(ports), 'min': ports[0] if ports else None, 'max': ports[-1] if ports else None,
                                'well_known': [x for x in ports if x < 10000]}
    daemon = Path('/etc/docker/daemon.json')
    daemon_cfg = json.loads(daemon.read_text()) if daemon.exists() else {}
    nat = sh('iptables', '-t', 'nat', '-S', 'DOCKER') or ''
    rules = sorted({(m.group(1), m.group(2)) for m in re.finditer(r'-p (udp|tcp) .*?--dport (\S+)', nat)})
    import yaml
    config = yaml.safe_load(Path('/opt/livekit/livekit.yaml').read_text()) or {}
    turn = config.get('turn') or {}
    rtc = config.get('rtc') or {}
    probes = {}
    for label, port in (('rtc_range_start', rtc.get('port_range_start') or 50000), ('turn_udp', turn.get('udp_port') or 3478),
                        ('rtc_udp_mux_7882', 7882)):
        probes[label] = stun_probe(public_ip, int(port))
    emit('mediaPathFacts', {'livekit_published_ports_all_interfaces': bindings,
        'livekit_listeners_in_container': listeners,
        'docker_userland_proxy': daemon_cfg.get('userland-proxy', 'default(true)'),
        'docker_iptables': daemon_cfg.get('iptables', 'default(true)'),
        'docker_nat_dnat_rules': [list(r) for r in rules],
        'turn': {'relay_range_start': turn.get('relay_range_start'), 'relay_range_end': turn.get('relay_range_end'),
                 'domain_configured': bool(turn.get('domain')), 'tls_cert_configured': bool(turn.get('cert_file'))},
        'host_stun_probe_to_public_address': probes})
    ufw = sh('ufw', 'status', 'verbose') or ''
    emit('firewallPolicy', [redact(line) for line in ufw.splitlines() if line.startswith(('Status', 'Default', 'Logging'))])
    kernel = sh('journalctl', '-k', '--since', '2026-11-04', '-o', 'cat', '--no-pager', '-g', 'UFW BLOCK', timeout=90) or ''
    blocks = collections.Counter()
    for line in kernel.splitlines():
        f = dict(x.split('=', 1) for x in line.split() if '=' in x)
        source = f.get('SRC', '')
        try:
            import ipaddress
            private = ipaddress.ip_address(source).is_private
        except ValueError:
            private = None
        dport = f.get('DPT', '-')
        bucket = dport if not dport.isdigit() or int(dport) < 10000 else ('50000-60000' if 50000 <= int(dport) <= 60000 else 'ephemeral>=10000')
        blocks[(f.get('IN', '').rstrip('0123456789') or '-', f.get('PROTO', '-'), bucket,
                'private_src' if private else 'public_src', 'to_self_public' if f.get('DST') == public_ip else 'other_dst')] += 1
    emit('ufwBlocksSince20261104', [list(k) + [v] for k, v in blocks.most_common(25)])
    connection = collections.Counter(); never = collections.Counter()
    for run, image, outcome, start, end, stage in WINDOWS:
        output = sh('docker', 'logs', '--since', start + 'Z', '--until', end + 'Z', 'lk-server', timeout=120) or ''
        types = collections.Counter()
        for line in output.splitlines():
            if 'participant active' in line:
                types[json.dumps(safe_fields(line), sort_keys=True)] += 1
            if 'short ice connection' in line or 'TURN allocation quota' in line or 'without connection' in line:
                types[template(line) + ' ' + json.dumps(safe_fields(line), sort_keys=True)] += 1
        emit('livekitConnectionTypes', {'run': run, 'image': image, 'outcome': outcome, 'counts': dict(types)})


_NUMERIC_KEYS = re.compile(r'(?i)duration|rtt|attempt|count|elapsed|ms$|seconds$|protocol$|version$')
TIMELINE_RUNS = ('41794968776', '41977529784', '41992435685', '41981794294', '41798927157')


def timeline_fields(fields):
    """Enum-like strings, booleans and named numeric timings only."""
    out = {}
    for key, value in fields.items():
        if key in ('room', 'roomID', 'participant', 'pID', 'participantID', 'trackID', 'remote_addr', 'remote', 'nodeIP'):
            continue
        if isinstance(value, bool):
            out[key] = value
        elif isinstance(value, (int, float)) and _NUMERIC_KEYS.search(key):
            out[key] = value
        elif isinstance(value, str) and _SAFE_VALUE.match(value):
            out[key] = value
    return out


def livekit_timelines():
    for run, image, outcome, start, end, stage in WINDOWS:
        if run not in TIMELINE_RUNS:
            continue
        output = sh('docker', 'logs', '--timestamps', '--since', start + 'Z', '--until', end + 'Z', 'lk-server', timeout=120) or ''
        people, rooms, events, base = {}, {}, [], None
        for line in output.splitlines():
            if '.api' in line.split('{', 1)[0] or 'stats worker' in line:
                continue
            when, _ = stamp(line)
            tail = line[line.find('{'):] if '{' in line else ''
            try:
                fields = json.loads(tail) if tail else {}
            except ValueError:
                fields = {}
            participant = fields.get('pID') or fields.get('participant')
            if not participant:
                continue
            label = people.setdefault(participant, 'p%d' % len(people))
            room = rooms.setdefault(fields.get('roomID') or fields.get('room') or '-', 'r%d' % len(rooms))
            base = base or when
            events.append([round((when - base).total_seconds(), 2) if when and base else None, room, label,
                           template(line), timeline_fields(fields)])
        emit('livekitTimeline', {'run': run, 'image': image, 'outcome': outcome, 'stage': stage,
                                 'participants': len(people), 'events': events[:160]})


def run():
    public_ip = socket.gethostbyname('api.cfmeet.com')
    for step in (livekit_timelines, lambda: media_path_facts(public_ip), host_resources, kernel_events, sysstat_history, container_resources,
                 network_state, lambda: livekit_config(public_ip), active_media, lambda: probe_latency(public_ip), window_logs, client_logs):
        try:
            step()
        except Exception as error:
            emit('diagnosticStepFailed', {'step': getattr(step, '__name__', 'lambda'), 'error': type(error).__name__})
    print('MEDIA_DIAGNOSIS: read-only collection complete; no production state changed', flush=True)
