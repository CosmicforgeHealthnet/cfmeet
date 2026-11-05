"""Read-only audit of every existing ClamAV engine on the shared VPS.

Selects whether an existing *production* engine can safely carry CF Meet's
additional scan traffic through the shared scan gateway. Nothing here changes
another product's container: no restart, no network attach, no config write.

Allowed actions on other products' scanners: ``docker inspect``, cgroup
counters, a whitelisted read of clamd configuration keys, the signature
database file list (names, sizes, mtimes) and one ``clamdscan --version`` /
benign stdin scan through ``docker exec`` (latency only; the bytes are a fixed
harmless string, never customer data). Output carries no environment, logs,
filenames of customer content or credentials.
"""
import json
import re
import statistics
import subprocess
import time
from pathlib import Path

CONFIG_KEYS = ('TCPSocket', 'TCPAddr', 'LocalSocket', 'MaxThreads', 'MaxQueue', 'StreamMaxLength',
               'MaxScanSize', 'MaxFileSize', 'ConcurrentDatabaseReload', 'ReadTimeout',
               'CommandReadTimeout', 'IdleTimeout', 'SelfCheck', 'MaxConnectionQueueLength')
CONFIG_PATHS = ('/etc/clamav/clamd.conf', '/etc/clamd.conf', '/usr/local/etc/clamd.conf', '/etc/clamd.d/scan.conf')
BENIGN = b'CosmicForge shared scanner capacity audit: harmless fixed bytes\n' * 64


def sh(*args, timeout=60, input=None):
    r = subprocess.run(args, capture_output=True, timeout=timeout, input=input)
    return r.returncode, r.stdout.decode(errors='replace'), r.stderr.decode(errors='replace')


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True), flush=True)


def is_scanner(c):
    n = c['Name'].lstrip('/')
    image = (c['Config'].get('Image') or '').lower()
    return 'clamav' in n or 'clamd' in n or 'scanner' in n or 'clamav' in image or 'clamd' in image


def classify(n, labels):
    project = labels.get('com.docker.compose.project', '')
    owner = ('cfmeet' if n.startswith('cfmeet-') else 'karrygo' if n.startswith('karrygo-') or 'karrygo' in project
             else 'admin-server-mail' if 'mail' in n else 'unknown')
    stage = 'staging' if 'staging' in n or 'staging' in project or 'stage' in n.split('-') else 'production'
    return owner, stage, project


def cgroup(cid):
    for base in (Path('/sys/fs/cgroup/system.slice') / ('docker-' + cid + '.scope'), Path('/sys/fs/cgroup/docker') / cid):
        if base.exists():
            out = {}
            for f in ('memory.current', 'memory.peak', 'memory.max', 'memory.swap.current'):
                p = base / f
                if p.exists():
                    v = p.read_text().strip()
                    out[f] = v if v == 'max' else round(int(v) / 2**20)
            ev = base / 'memory.events'
            if ev.exists():
                out['memory.events'] = {k: int(v) for k, v in (l.split() for l in ev.read_text().splitlines())}
            cpu = base / 'cpu.stat'
            if cpu.exists():
                stats = dict(l.split() for l in cpu.read_text().splitlines())
                out['cpu_usage_s'] = round(int(stats.get('usage_usec', 0)) / 1e6)
                out['cpu_throttled_s'] = round(int(stats.get('throttled_usec', 0)) / 1e6)
            return out
    return {'cgroup': 'not found'}


def clamd_config(n):
    found = {}
    for path in CONFIG_PATHS:
        rc, out, _ = sh('docker', 'exec', n, 'cat', path, timeout=20)
        if rc == 0:
            found['path'] = path
            for line in out.splitlines():
                parts = line.strip().split(None, 1)
                if len(parts) == 2 and parts[0] in CONFIG_KEYS:
                    found[parts[0]] = re.sub(r'[^A-Za-z0-9 ./:_-]', '', parts[1])[:60]
            break
    return found or {'path': 'unreadable'}


def signatures(n):
    rc, out, _ = sh('docker', 'exec', n, 'sh', '-c', 'ls -l --time-style=+%s /var/lib/clamav 2>/dev/null', timeout=20)
    files, now = [], time.time()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 7 and re.match(r'^(main|daily|bytecode)\.(cvd|cld)$', parts[-1]):
            files.append({'file': parts[-1], 'mib': round(int(parts[4]) / 2**20, 1), 'age_h': round((now - int(parts[5])) / 3600, 1)})
    rc, version, _ = sh('docker', 'exec', n, 'clamdscan', '--version', timeout=30)
    stamp = None
    if rc == 0:
        m = re.search(r'ClamAV ([\d.]+)/(\d+)/(.+)$', version.strip())
        if m:
            stamp = {'engine': m.group(1), 'daily': int(m.group(2)), 'built': m.group(3).strip()[:40]}
            try:
                t = time.mktime(time.strptime(m.group(3).strip(), '%a %b %d %H:%M:%S %Y'))
                stamp['age_h'] = round((now - t) / 3600, 1)
            except ValueError:
                pass
    rc_ps, ps, _ = sh('docker', 'exec', n, 'sh', '-c', 'ps -o comm= 2>/dev/null || ps', timeout=20)
    procs = sorted({w for w in re.findall(r'(clamd|freshclam|supervisord|crond)', ps)})
    return {'database_files': files, 'daemon_version': stamp or 'unavailable', 'processes': procs}


def benign_latency(n):
    """Three fixed harmless stdin scans through the owner's own clamdscan."""
    samples = []
    for _ in range(3):
        t = time.monotonic()
        rc, out, _ = sh('docker', 'exec', '-i', n, 'clamdscan', '--no-summary', '-', timeout=60, input=BENIGN)
        samples.append({'ms': round((time.monotonic() - t) * 1000), 'clean': rc == 0 and out.strip().endswith('OK')})
    return samples


def host():
    info = {l.split(':')[0]: int(l.split()[1]) for l in Path('/proc/meminfo').read_text().splitlines()}
    psi = {}
    for k in ('memory', 'cpu', 'io'):
        p = Path('/proc/pressure') / k
        if p.exists():
            psi[k] = {l.split()[0]: {kv.split('=')[0]: kv.split('=')[1] for kv in l.split()[1:4]} for l in p.read_text().splitlines()}
    _, nproc, _ = sh('nproc')
    return {'mem_total_mib': info['MemTotal'] // 1024, 'mem_available_mib': info['MemAvailable'] // 1024,
            'swap_used_mib': (info['SwapTotal'] - info['SwapFree']) // 1024, 'cpus': int(nproc.strip() or 0),
            'loadavg': Path('/proc/loadavg').read_text().split()[:3], 'pressure': psi}


def sample(names, seconds=120, interval=10):
    """Repeated real usage of scanners and the host under the current workload."""
    series = {n: [] for n in names}
    hostmem, units = [], {'B': 1, 'KiB': 2**10, 'MiB': 2**20, 'GiB': 2**30}
    for _ in range(max(1, seconds // interval)):
        rc, out, _ = sh('docker', 'stats', '--no-stream', '--format', '{{.Name}}\t{{.MemUsage}}\t{{.CPUPerc}}', *names, timeout=60)
        for line in out.splitlines():
            n, mem, cpu = line.split('\t')
            m = re.match(r'([\d.]+)\s*([KMG]?i?B)', mem.split('/')[0].strip())
            series[n].append((round(float(m.group(1)) * units.get(m.group(2), 1) / 2**20) if m else 0, float(cpu.strip('%') or 0)))
        hostmem.append(host()['mem_available_mib'])
        time.sleep(interval)
    def stats(xs):
        return {'min': min(xs), 'median': statistics.median(xs), 'max': max(xs)} if xs else {}
    return ({n: {'mem_mib': stats([m for m, _ in s]), 'cpu_pct': stats([c for _, c in s])} for n, s in series.items()},
            stats(hostmem))


def calling_usage():
    rc, out, _ = sh('docker', 'stats', '--no-stream', '--format', '{{.Name}}\t{{.MemUsage}}\t{{.CPUPerc}}',
                    'lk-server', 'cfmeet-calling', 'cfmeet-messaging', timeout=60)
    return [line.split('\t') for line in out.splitlines()]


def audit():
    ids = sh('docker', 'ps', '-aq')[1].split()
    all_c = json.loads(sh('docker', 'inspect', *ids)[1]) if ids else []
    scanners = [c for c in all_c if is_scanner(c)]
    emit('host', host())
    report = []
    for c in scanners:
        n = c['Name'].lstrip('/')
        labels = c['Config'].get('Labels') or {}
        owner, stage, project = classify(n, labels)
        hc = c['HostConfig']
        image = json.loads(sh('docker', 'image', 'inspect', c['Image'])[1] or '[{}]')[0]
        entry = {
            'name': n, 'owner': owner, 'classification': stage, 'compose_project': project,
            'running': c['State']['Running'], 'health': (c['State'].get('Health') or {}).get('Status', 'none'),
            'started': c['State'].get('StartedAt', '')[:19], 'restart_count': c.get('RestartCount', 0),
            'oom_killed': c['State'].get('OOMKilled'),
            'image': c['Config'].get('Image'), 'image_digests': image.get('RepoDigests', []),
            'restart_policy': (hc.get('RestartPolicy') or {}).get('Name'),
            'memory_limit_mib': (hc.get('Memory') or 0) // 2**20, 'nano_cpus': hc.get('NanoCpus') or 0,
            'cpu_shares': hc.get('CpuShares') or 0,
            'networks': sorted((c['NetworkSettings'].get('Networks') or {}).keys()),
            'published_ports': {k: [b.get('HostIp', '') + ':' + b.get('HostPort', '') for b in (v or [])]
                                for k, v in (hc.get('PortBindings') or {}).items()},
            'exposed': sorted((c['Config'].get('ExposedPorts') or {}).keys()),
            'mounts': [{'type': m.get('Type'), 'dst': m.get('Destination'), 'rw': m.get('RW')} for m in c.get('Mounts', [])],
            'cgroup': cgroup(c['Id']),
        }
        if c['State']['Running']:
            entry['clamd'] = clamd_config(n)
            entry['signatures'] = signatures(n)
            entry['benign_scan_latency'] = benign_latency(n)
        report.append(entry)
        emit('scanner', entry)
    running = [e['name'] for e in report if e['running']]
    usage, hostmem = sample(running)
    emit('scannerUsage', usage)
    emit('hostMemAvailableMibDuringSample', hostmem)
    emit('callingUsage', calling_usage())
    nets = {}
    for e in report:
        for net in e['networks']:
            rc, out, _ = sh('docker', 'network', 'inspect', net, timeout=20)
            if rc == 0:
                d = json.loads(out)[0]
                nets[net] = {'internal': d.get('Internal'), 'driver': d.get('Driver'),
                             'members': len(d.get('Containers') or {})}
    emit('scannerNetworks', nets)
