"""CF Meet resource inventory (read-only) and scoped recovery on the shared VPS.

The VPS also hosts other products. Only CF Meet-owned objects are ever changed:
containers/images whose name, repository or source label is CF Meet's. Running,
rollback, failed-release (evidence) and current/candidate release images are kept.
Output is names, sizes and counts only: no environment, logs or secrets.
"""
import collections
import json
import os
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path('/opt/cfmeet-core')
STAGE_PREFIX = 'cfmeet-comm-stage-'
CANDIDATE_LABEL = 'com.cfmeet.communications.candidate'
CFMEET_REPO = re.compile(r'^(ghcr\.io/cosmicforgehealthnet/cfmeet-|cfmeet-)')
SOURCE_LABEL = 'org.opencontainers.image.source'
LOGROTATE = Path('/etc/logrotate.d/cfmeet-docker-json')


def sh(*args, timeout=120):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError('command failed: ' + ' '.join(args[:3]))
    return result.stdout


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True), flush=True)


def containers():
    ids = sh('docker', 'ps', '-aq').split()
    return json.loads(sh('docker', 'inspect', *ids)) if ids else []


def images():
    ids = sorted(set(sh('docker', 'image', 'ls', '-aq', '--no-trunc').split()))
    return json.loads(sh('docker', 'image', 'inspect', *ids)) if ids else []


def name(c):
    return c['Name'].lstrip('/')


def cfmeet_container(c):
    return name(c).startswith('cfmeet-') or name(c) == 'lk-server'


def cfmeet_image(image):
    tags = image.get('RepoTags') or []
    digests = [d.split('@')[0] for d in image.get('RepoDigests') or []]
    source = ((image.get('Config') or {}).get('Labels') or {}).get(SOURCE_LABEL, '')
    return any(CFMEET_REPO.match(t) for t in tags + digests) or source.lower().endswith('cosmicforgehealthnet/cfmeet')


def gib(n):
    return round(n / 2**30, 2)


def memory():
    info = {line.split(':')[0]: int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines()}
    usage = shutil.disk_usage('/')
    return {'mem_available_gib': gib(info['MemAvailable'] * 1024), 'mem_total_gib': gib(info['MemTotal'] * 1024),
            'swap_used_gib': gib((info['SwapTotal'] - info['SwapFree']) * 1024),
            'loadavg': Path('/proc/loadavg').read_text().split()[:3],
            'disk_free_gib': gib(usage.free), 'disk_used_percent': round(100 * usage.used / usage.total, 1)}


def stage_db_activity():
    """Non-terminal calls and client sessions in the isolated stage databases."""
    out = {}
    query = ("SELECT count(*) FROM calls WHERE current_state NOT IN "
             "('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')")
    try:
        out['stage_active_calls'] = int(sh('docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'psql', '-XAtq', '-U', 'postgres',
                                           '-d', 'cfmeet_comm_stage_calling', '-c', query).strip() or 0)
    except RuntimeError:
        out['stage_active_calls'] = 'UNREADABLE'
    try:
        out['stage_db_client_connections'] = int(sh('docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'psql', '-XAtq', '-U', 'postgres',
            '-c', "SELECT count(*) FROM pg_stat_activity WHERE datname LIKE 'cfmeet_comm_stage_%' AND state <> 'idle'").strip() or 0)
    except RuntimeError:
        out['stage_db_client_connections'] = 'UNREADABLE'
    return out


def stage_candidates(all_containers):
    rows = []
    for c in all_containers:
        if not name(c).startswith(STAGE_PREFIX):
            continue
        labels = c['Config'].get('Labels') or {}
        rows.append({'name': name(c), 'running': c['State']['Running'], 'candidate_label': labels.get(CANDIDATE_LABEL) == 'true',
                     'published_ports': bool(c['HostConfig'].get('PortBindings')), 'started': c['State']['StartedAt'][:19],
                     'memory_limit_mib': (c['HostConfig'].get('Memory') or 0) // 2**20})
    return rows


def referenced_text():
    """Every rollback journal, evidence record and env file CF Meet keeps under ROOT."""
    chunks = []
    for path in ROOT.rglob('*'):
        if path.is_file() and path.suffix in ('.json', '.env', '.conf', '.yaml', '.yml') and path.stat().st_size < 5 * 2**20:
            chunks.append(path.read_text(errors='replace'))
    return '\n'.join(chunks)


def protected_shas(all_containers):
    """Image source SHAs that must never be removed: running, rollback/evidence, releases."""
    shas = set()
    for c in all_containers:
        tag = c['Config'].get('Image', '')
        if ':' in tag:
            shas.add(tag.rsplit(':', 1)[1])
    shas.update(re.findall(r'\b[0-9a-f]{40}\b', referenced_text()))
    return shas


def inventory():
    all_containers = containers()
    all_images = images()
    used = {c['Image'] for c in all_containers}
    emit('capacity', memory())
    emit('stageCandidates', {'containers': stage_candidates(all_containers), **stage_db_activity()})
    exited = [c for c in all_containers if not c['State']['Running']]
    by_kind = collections.Counter()
    for c in exited:
        n = name(c)
        kind = ('cfmeet-failed' if 'failed' in n else 'cfmeet-rollback' if 'rollback' in n else 'cfmeet-retired' if 'retired' in n
                else 'cfmeet-other') if cfmeet_container(c) else 'other-product'
        by_kind[kind] += 1
    emit('exitedContainers', dict(by_kind))
    shas = protected_shas(all_containers)
    summary = collections.defaultdict(lambda: {'count': 0, 'bytes': 0})
    removable = []
    for image in all_images:
        owner = 'cfmeet' if cfmeet_image(image) else 'other'
        state = 'in_use' if image['Id'] in used else ('dangling' if not image.get('RepoTags') else 'unused_tagged')
        key = owner + ':' + state
        summary[key]['count'] += 1
        summary[key]['bytes'] += image.get('Size', 0)
        if owner == 'cfmeet' and state != 'in_use':
            tags = image.get('RepoTags') or []
            sha_tags = {t.rsplit(':', 1)[1] for t in tags if ':' in t}
            if not sha_tags & shas:
                removable.append({'tags': tags[:3], 'size_gib': gib(image.get('Size', 0)), 'created': image.get('Created', '')[:10]})
    emit('images', {k: {'count': v['count'], 'gib': gib(v['bytes'])} for k, v in sorted(summary.items())})
    emit('cfmeetRemovableImages', {'count': len(removable), 'sum_unshared_upper_bound_gib': round(sum(r['size_gib'] for r in removable), 2),
                                   'examples': sorted(removable, key=lambda r: r['created'])[:12],
                                   'protected_sha_count': len(shas)})
    logs = []
    for c in all_containers:
        path = c.get('LogPath') or ''
        size = os.path.getsize(path) if path and os.path.exists(path) else 0
        logs.append((size, name(c), cfmeet_container(c), c['State']['Running'],
                     bool((c['HostConfig'].get('LogConfig') or {}).get('Config'))))
    logs.sort(reverse=True)
    emit('containerLogs', {'cfmeet_total_gib': gib(sum(s for s, _, own, _, _ in logs if own)),
        'other_total_gib': gib(sum(s for s, _, own, _, _ in logs if not own)),
        'largest_cfmeet': [[n, gib(s), running, rotated] for s, n, own, running, rotated in logs if own][:15]})
    journal = subprocess.run(['journalctl', '--disk-usage'], capture_output=True, text=True).stdout.strip()
    emit('hostLogs', {'journal': re.sub(r'[^A-Za-z0-9 .:,()-]', '', journal)[:120],
                      'var_log_gib': gib(sum(f.stat().st_size for f in Path('/var/log').rglob('*') if f.is_file())),
                      'logrotate_cfmeet_installed': LOGROTATE.exists()})
    emit('dockerDiskUsage', [line.split() for line in sh('docker', 'system', 'df').splitlines()])


# Long-lived CF Meet infrastructure containers that releases never recreate.
LONG_LIVED = ('lk-server', 'cfmeet-core-db', 'cfmeet-core-tls', 'cfmeet-calling-loopback', 'cfmeet-calling-redis',
              'cfmeet-calling-artifacts', 'cfmeet-calling-egress')


def removable_images(all_containers, all_images):
    """CF Meet-owned, unused, not referenced anywhere CF Meet keeps evidence or rollback
    journals, never another product's image. The newest Meetings build is kept."""
    used = {c['Image'] for c in all_containers}
    shas = protected_shas(all_containers)
    text = referenced_text()
    meetings = sorted((i for i in all_images if any(t.startswith('cfmeet-meeting-service:prod-') for t in i.get('RepoTags') or [])),
                      key=lambda i: i.get('Created', ''))
    keep = {meetings[-1]['Id']} if meetings else set()
    tags = []
    for image in all_images:
        if not cfmeet_image(image) or image['Id'] in used or image['Id'] in keep:
            continue
        for tag in image.get('RepoTags') or []:
            suffix = tag.rsplit(':', 1)[1].removeprefix('prod-')
            if suffix in ('latest', 'prod-latest') or suffix in shas or (len(suffix) >= 7 and suffix in text):
                break
        else:
            tags.extend(image.get('RepoTags') or [])
    return tags


def logrotate_config(all_containers):
    paths = sorted(c['LogPath'] for c in all_containers
                   if name(c) in LONG_LIVED and c['State']['Running'] and c.get('LogPath')
                   and not (c['HostConfig'].get('LogConfig') or {}).get('Config'))
    if not paths:
        return None
    # copytruncate: Docker keeps writing the same file; no container restart.
    return ('# CF Meet long-lived container logs (generated by resource-recover; Docker json-file).\n'
            + '\n'.join(paths) + ' {\n    size 100M\n    rotate 5\n    compress\n    delaycompress\n'
            '    copytruncate\n    missingok\n    notifempty\n}\n')


def recover(run_id):
    before = memory()
    all_containers = containers()
    stage = stage_candidates(all_containers)
    activity = stage_db_activity()
    running = [c for c in stage if c['running']]
    # Every stop condition is re-verified at execution time, not assumed.
    assert activity == {'stage_active_calls': 0, 'stage_db_client_connections': 0}, 'Isolated stage is active; nothing stopped'
    assert all(c['candidate_label'] and not c['published_ports'] and c['name'].startswith(STAGE_PREFIX) for c in running)
    stopped = []
    for c in running:
        sh('docker', 'stop', '--time', '20', c['name'])  # retained, never removed
        stopped.append(c['name'])
    removed, refused = [], []
    for tag in removable_images(all_containers, images()):
        result = subprocess.run(['docker', 'image', 'rm', tag], capture_output=True, text=True)  # never forced
        (removed if result.returncode == 0 else refused).append(tag)
    config = logrotate_config(all_containers)
    if config:
        candidate = LOGROTATE.with_suffix('.candidate')
        candidate.write_text(config)
        check = subprocess.run(['logrotate', '-d', str(candidate)], capture_output=True, text=True)
        candidate.unlink()
        assert check.returncode == 0, 'logrotate rejected the generated configuration; nothing installed'
        LOGROTATE.write_text(config)
        LOGROTATE.chmod(0o644)
    after = memory()
    emit('resourceRecovery', {'runId': run_id, 'stoppedStageCandidates': stopped, 'removedImageTags': removed,
        'refusedImageTags': refused, 'logrotateInstalledFor': [n for n in LONG_LIVED if config and n in
            {name(c) for c in all_containers if c.get('LogPath') in config}],
        'before': before, 'after': after,
        'preserved': 'running production, rollback/failed/retired containers, evidence, volumes, other products, secrets'})
    inventory()


def run(operation, sha, run_id):
    if operation == 'resource-inventory':
        inventory()
    elif operation == 'resource-recover':
        recover(run_id)
    else:
        raise RuntimeError('Unsupported resource operation')
