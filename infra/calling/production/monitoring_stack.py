"""Production monitoring stack: Prometheus, Alertmanager, blackbox and heartbeat exporters.

Backend gap audit P1 / P3 / P4. Deployed on the VPS by coordinated_release.py: after every
successful `release` (never failing it) and by the `deploy-monitoring` operation. Runs as
root on the VPS like the rest of the release; the configuration comes from the copy of
infra/monitoring that the workflow ships next to this file (`monitoring-config/`).

Containers (all on the private network cfmeet-core-private, no published port, read-only
root filesystem, no capabilities, uid 65534, bounded memory):

  cfmeet-prometheus           scrapes only listeners that exist in the running containers
                              (render_prometheus), loads infra/monitoring rules, sends to
                              cfmeet-alertmanager. TSDB in the volume cfmeet-prometheus-data
                              (15 days, at most 2 GB).
  cfmeet-alertmanager         infra/monitoring/alertmanager/alertmanager.yml with only the
                              integrations whose secret file exists (render_alertmanager).
                              Attached to the egress bridge cfmeet-monitoring-egress only
                              when a receiver is configured.
  cfmeet-blackbox             credential-free GETs of each service's own health / readiness
                              route (job health-probes).
  cfmeet-heartbeat-exporter   SQL exporter reading devplat_worker_heartbeats with the
                              read-only role cfmeet_monitor (job developer-platform-heartbeat).

Receiver secrets: the workflow writes the GitHub environment secrets
CFMEET_ALERT_WEBHOOK_URL / CFMEET_ALERT_SLACK_WEBHOOK_URL to RECEIVER_SECRETS on the VPS (or
removes them when unset). Values are never printed and never written into a config file:
Alertmanager reads them through *_file fields. Without them the stack starts with
receivers that have no integration.

Only the containers named in CONTAINERS are ever created or removed here.
"""
import hashlib
import json
import os
import re
import secrets
import shutil
import time
from pathlib import Path
from urllib.parse import quote, urlsplit

import production as p

HERE = Path(__file__).resolve().parent
CONFIG_SOURCE = HERE / 'monitoring-config'
ROOT = p.ROOT / 'monitoring'
UID = 65534
EGRESS_NET = 'cfmeet-monitoring-egress'
LABEL = 'com.cfmeet.monitoring.config'
PROBE_IMAGE = 'curlimages/curl:8.12.1'

# Pinned by digest (multi-arch index). Prometheus / Alertmanager are the images CI validates
# the configuration with (.github/workflows/service-boundaries.yml).
IMAGES = {
    'prometheus': 'prom/prometheus:v3.5.0@sha256:63805ebb8d2b3920190daf1cb14a60871b16fd38bed42b857a3182bc621f4996',
    'alertmanager': 'prom/alertmanager:v0.28.1@sha256:27c475db5fb156cab31d5c18a4251ac7ed567746a2483ff264516437a39b15ba',
    'blackbox': 'prom/blackbox-exporter:v0.27.0@sha256:a50c4c0eda297baa1678cd4dc4712a67fdea713b832d43ce7fcc5f9bea05094d',
    'heartbeat': 'burningalchemist/sql_exporter:0.24.14@sha256:9ca0661972dbe8a31b7ebe38f9e3fe11699a1e0104a3bd887701cf9fc7d89e82',
}
CONTAINERS = {
    'prometheus': 'cfmeet-prometheus',
    'alertmanager': 'cfmeet-alertmanager',
    'blackbox': 'cfmeet-blackbox',
    'heartbeat': 'cfmeet-heartbeat-exporter',
}
MEMORY_MIB = {'prometheus': 384, 'alertmanager': 64, 'blackbox': 48, 'heartbeat': 48}
VOLUMES = {'prometheus': ('cfmeet-prometheus-data', '/prometheus'),
           'alertmanager': ('cfmeet-alertmanager-data', '/alertmanager')}
SAFETY_MARGIN_MIB = 1024

# GitHub environment secret -> file under the VPS secret store (written by the workflow).
RECEIVER_SECRETS = {
    'webhook_configs': ('CFMEET_ALERT_WEBHOOK_URL', 'alertmanager-webhook-url', 'alert-webhook-url'),
    'slack_configs': ('CFMEET_ALERT_SLACK_WEBHOOK_URL', 'alertmanager-slack-webhook-url', 'alert-slack-webhook-url'),
}

# Separate metrics listeners (service -> environment variable of its running container).
METRICS_ENV = {
    'platform-access': 'PLATFORM_ACCESS_METRICS_ADDRESS',
    'messaging': 'MESSAGING_METRICS_ADDRESS',
    'calling': 'CALLING_METRICS_ADDRESS',
    'numbering': 'NUMBERING_METRICS_ADDRESS',
    'privacy-relay': 'PRIVACY_RELAY_METRICS_ADDRESS',
    'sms-gateway': 'SMS_GATEWAY_METRICS_ADDRESS',
    'telephony': 'TELEPHONY_METRICS_ADDRESS',
}
SCAN_GATEWAY = ('security-scan-gateway', 'cosmicforge-scan-gateway', 'SCAN_GATEWAY_METRICS_ADDRESS')
# Services that serve GET /metrics on their main listener (unauthenticated, private network).
MAIN_LISTENER_METRICS = ('notifications', 'notifications-2')

MONITOR_ROLE = 'cfmeet_monitor'
MONITOR_DB = 'cfmeet_developer_platform'
HEARTBEAT_TABLE = 'devplat_worker_heartbeats'
MONITOR_SQL = """SELECT format('CREATE ROLE %I LOGIN NOINHERIT', '{role}') WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{role}') \\gexec
ALTER ROLE {role} PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 2;
ALTER ROLE {role} SET default_transaction_read_only = on;
ALTER ROLE {role} SET statement_timeout = '5s';
GRANT CONNECT ON DATABASE {db} TO {role};
"""
MONITOR_GRANT_SQL = """GRANT USAGE ON SCHEMA public TO {role};
GRANT SELECT ON TABLE {table} TO {role};
"""


# --- rendering (pure; unit-tested) -----------------------------------------------------------

def drop_integration(text, key):
    """Removes every `key:` block (the line and its deeper-indented children) from YAML text."""
    out, skip_indent = [], None
    for line in text.splitlines():
        stripped = line.strip()
        indent = len(line) - len(line.lstrip(' '))
        if skip_indent is not None:
            if not stripped or indent > skip_indent or (indent == skip_indent and stripped.startswith('- ')):
                continue
            skip_indent = None
        if stripped.startswith(key + ':') and not stripped.startswith('#'):
            skip_indent = indent
            continue
        out.append(line)
    return '\n'.join(out) + '\n'


def render_alertmanager(text, present):
    """The committed routing with only the integrations whose secret file is present."""
    for key in RECEIVER_SECRETS:
        if key not in present:
            text = drop_integration(text, key)
    return text


def listener_target(container, address):
    """`container:port` for a listener bound to a non-loopback address, else None."""
    if not address:
        return None
    parsed = urlsplit('//' + address.strip())
    try:
        port = parsed.port
    except ValueError:
        return None
    if not port or parsed.hostname in ('127.0.0.1', 'localhost', '::1'):
        return None
    return '%s:%d' % (container, port)


def scrape_jobs(services, envs, meeting_token_file=None, heartbeat=False):
    """Prometheus scrape jobs for what is running.

    services: coordinated_release.SERVICES; envs: {service or container name: env} of the
    running containers (absent = not running)."""
    jobs = []
    for name, var in METRICS_ENV.items():
        env = envs.get(name)
        target = listener_target(services[name]['container'], (env or {}).get(var))
        if target:
            jobs.append(dict(job_name=name, metrics_path='/metrics', static_configs=[dict(targets=[target])]))
    service, container, var = SCAN_GATEWAY
    target = listener_target(container, (envs.get(container) or {}).get(var))
    if target:
        jobs.append(dict(job_name=service, metrics_path='/metrics', static_configs=[dict(targets=[target])]))
    targets = []
    for name in MAIN_LISTENER_METRICS:
        if name in envs:
            targets.append('%s:%d' % (services[name]['container'], services[name]['port']))
    if targets:
        jobs.append(dict(job_name='notifications', metrics_path='/metrics', static_configs=[dict(targets=targets)]))
    if meeting_token_file and 'meeting-service' in envs:
        s = services['meeting-service']
        jobs.append(dict(job_name='meeting-service', metrics_path='/internal/metrics',
                         http_headers={'X-Internal-Token': {'files': [meeting_token_file]}},
                         static_configs=[dict(targets=['%s:%d' % (s['container'], s['port'])])]))
    probes = []
    for name, s in services.items():
        if name in envs and s['port'] and s['health']:
            probes.append(dict(targets=['http://%s:%d%s' % (s['container'], s['port'], s['health'])],
                               labels=dict(service=name)))
    if probes:
        jobs.append(dict(job_name='health-probes', metrics_path='/probe', params=dict(module=['http_ready']),
                         static_configs=probes,
                         relabel_configs=[
                             dict(source_labels=['__address__'], target_label='__param_target'),
                             dict(source_labels=['__param_target'], target_label='instance'),
                             dict(target_label='__address__', replacement=CONTAINERS['blackbox'] + ':9115')]))
    if heartbeat:
        jobs.append(dict(job_name='developer-platform-heartbeat', metrics_path='/metrics',
                         static_configs=[dict(targets=[CONTAINERS['heartbeat'] + ':9399'])]))
    return jobs


def render_prometheus(jobs):
    """prometheus.yml (JSON is YAML) with the repository rules and the local Alertmanager."""
    return json.dumps({
        'global': dict(scrape_interval='30s', evaluation_interval='30s', scrape_timeout='10s',
                       external_labels=dict(environment='production')),
        'rule_files': ['/etc/prometheus/rules/*.rules.yml'],
        'alerting': dict(alertmanagers=[dict(static_configs=[dict(targets=[CONTAINERS['alertmanager'] + ':9093'])])]),
        'scrape_configs': jobs,
    }, indent=1, sort_keys=True) + '\n'


def render_sql_exporter(dsn):
    return json.dumps({
        'global': dict(scrape_timeout='10s', scrape_timeout_offset='500ms', min_interval='0s',
                       max_connections=1, max_idle_connections=1),
        'target': dict(data_source_name=dsn, collectors=['developer_platform_worker']),
        'collector_files': ['/etc/sql_exporter/*.collector.yml'],
    }, indent=1, sort_keys=True) + '\n'


def tree_hash(folder):
    digest = hashlib.sha256()
    for path in sorted(x for x in Path(folder).rglob('*') if x.is_file()):
        digest.update(path.relative_to(folder).as_posix().encode() + b'\0' + path.read_bytes() + b'\0')
    return digest.hexdigest()[:32]


def create_args(component, config_hash, config_dir):
    """`docker create` for one monitoring container: private, bounded, read-only, no caps."""
    name, memory = CONTAINERS[component], '%dm' % MEMORY_MIB[component]
    target = {'prometheus': '/etc/prometheus', 'alertmanager': '/etc/alertmanager',
              'blackbox': '/etc/blackbox_exporter', 'heartbeat': '/etc/sql_exporter'}[component]
    args = ['docker', 'create', '--name', name, '--restart', 'unless-stopped',
            '--network', p.NET, '--network-alias', name, '--user', '%d:%d' % (UID, UID),
            '--memory', memory, '--memory-swap', memory, '--pids-limit', '128', '--read-only',
            '--tmpfs', '/tmp:rw,noexec,nosuid,size=16m', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
            '--log-opt', 'max-size=10m', '--log-opt', 'max-file=3',
            '--label', LABEL + '=' + config_hash, '--label', 'com.cfmeet.release.managed-by=monitoring-stack',
            '--mount', 'type=bind,src=%s,dst=%s,readonly' % (config_dir, target)]
    if component in VOLUMES:
        volume, path = VOLUMES[component]
        args += ['--mount', 'type=volume,src=%s,dst=%s' % (volume, path)]
    args.append(IMAGES[component])
    if component == 'prometheus':
        args += ['--config.file=/etc/prometheus/prometheus.yml', '--storage.tsdb.path=/prometheus',
                 '--storage.tsdb.retention.time=15d', '--storage.tsdb.retention.size=2GB',
                 '--web.listen-address=:9090']
    elif component == 'alertmanager':
        args += ['--config.file=/etc/alertmanager/alertmanager.yml', '--storage.path=/alertmanager',
                 '--cluster.listen-address=', '--web.listen-address=:9093']
    elif component == 'blackbox':
        args += ['--config.file=/etc/blackbox_exporter/blackbox.yml', '--web.listen-address=:9115']
    else:
        args += ['-config.file=/etc/sql_exporter/sql_exporter.yml', '-web.listen-address=:9399']
    return args


# --- VPS side ----------------------------------------------------------------------------------

def _own(name):
    if name not in CONTAINERS.values():
        raise RuntimeError('Refusing to operate on a container the monitoring stack does not own: ' + name)


def container_names():
    return p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()


def write_file(path, text, mode=0o400):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    tmp = path.with_name(path.name + '.tmp')
    tmp.write_text(text)
    os.chown(tmp, UID, UID)
    os.chmod(tmp, mode)
    os.replace(tmp, path)


def staging_dir(component):
    """An empty staging directory for one component's configuration."""
    path = ROOT / ('.staging-' + component)
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, mode=0o700)
    return path


def publish(component, staging):
    """Moves a rendered configuration to its content-addressed directory and seals it.

    A running container keeps its bind mount of the previous directory (its files are
    read at scrape / notification time), so a configuration directory is never changed in
    place: a new content gets a new directory, and old ones are removed only after the
    container that used them was replaced (prune)."""
    final = ROOT / ('%s-%s' % (component, tree_hash(staging)))
    if final.exists():
        shutil.rmtree(staging)
        return final
    for folder in [staging] + [x for x in staging.rglob('*') if x.is_dir()]:
        os.chown(folder, UID, UID)
        os.chmod(folder, 0o500)
    os.replace(staging, final)
    return final


def prune(component, keep):
    for folder in ROOT.glob(component + '-*'):
        if folder.is_dir() and folder != keep and re.fullmatch(re.escape(component) + r'-[0-9a-f]{32}', folder.name):
            shutil.rmtree(folder)


def running_envs(cr):
    names = container_names()
    envs = {}
    for name, s in cr.SERVICES.items():
        if s['container'] in names:
            c = p.inspect(s['container'])
            if c['State']['Running']:
                envs[name] = p.env_of(c)
    if SCAN_GATEWAY[1] in names:
        c = p.inspect(SCAN_GATEWAY[1])
        if c['State']['Running']:
            envs[SCAN_GATEWAY[1]] = p.env_of(c)
    return envs


def provision_monitor_role(cr):
    """Read-only role for the heartbeat query; None when the table does not exist yet."""
    if cr.SERVICES['developer-platform']['container'] not in container_names():
        return None
    exists = cr.release.db_sql(MONITOR_DB, "SELECT to_regclass('public.%s') IS NOT NULL;" % HEARTBEAT_TABLE)
    if exists.strip() != 't':
        return None
    path = p.SECRETS / 'cfmeet-monitor.password'
    if not path.exists() or not path.read_text().strip():
        p.write_private(path, secrets.token_hex(32))
    os.chmod(path, 0o400)
    password = path.read_text().strip()
    assert re.fullmatch(r'[0-9a-f]{64}', password), 'Unexpected monitor password file format'
    cr.release.db_sql('postgres', MONITOR_SQL.format(role=MONITOR_ROLE, password=password, db=MONITOR_DB))
    cr.release.db_sql(MONITOR_DB, MONITOR_GRANT_SQL.format(role=MONITOR_ROLE, table=HEARTBEAT_TABLE))
    return 'postgres://%s:%s@cfmeet-core-db:5432/%s?sslmode=disable' % (MONITOR_ROLE, quote(password, safe=''), MONITOR_DB)


def receivers_present():
    present = set()
    for key, (_secret, filename, _target) in RECEIVER_SECRETS.items():
        path = p.SECRETS / filename
        if path.is_file() and not path.is_symlink() and path.read_text().strip():
            present.add(key)
    return present


def render_all(cr, emit):
    """Renders every configuration directory under ROOT. Returns {component: config dir}."""
    source = CONFIG_SOURCE
    if not (source / 'prometheus' / 'rules').is_dir():
        raise RuntimeError('monitoring-config missing next to the release module')
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    dirs = {}

    envs = running_envs(cr)
    prom = staging_dir('prometheus')
    for rules in sorted((source / 'prometheus' / 'rules').glob('*.rules.yml')):
        write_file(prom / 'rules' / rules.name, rules.read_text())
    token_file = None
    token = (envs.get('meeting-service') or {}).get('INTERNAL_API_TOKEN', '')
    if token.strip():
        write_file(prom / 'secrets' / 'meeting-service-internal-api', token.strip())
        token_file = '/etc/prometheus/secrets/meeting-service-internal-api'
    dsn = None
    try:
        dsn = provision_monitor_role(cr)
    except Exception as error:  # the heartbeat exporter is optional; the rest still deploys
        emit('monitoringHeartbeatRole', {'status': 'FAILED', 'error': type(error).__name__})
    jobs = scrape_jobs(cr.SERVICES, envs, token_file, heartbeat=bool(dsn))
    write_file(prom / 'prometheus.yml', render_prometheus(jobs))
    dirs['prometheus'] = prom

    am = staging_dir('alertmanager')
    present = receivers_present()
    write_file(am / 'alertmanager.yml',
               render_alertmanager((source / 'alertmanager' / 'alertmanager.yml').read_text(), present))
    for key in present:
        _secret, filename, target = RECEIVER_SECRETS[key]
        write_file(am / 'secrets' / target, (p.SECRETS / filename).read_text().strip())
    dirs['alertmanager'] = am

    bb = staging_dir('blackbox')
    write_file(bb / 'blackbox.yml', (source / 'blackbox' / 'blackbox.yml').read_text())
    dirs['blackbox'] = bb

    if dsn:
        hb = staging_dir('heartbeat')
        for collector in sorted((source / 'sql-exporter').glob('*.collector.yml')):
            write_file(hb / collector.name, collector.read_text())
        write_file(hb / 'sql_exporter.yml', render_sql_exporter(dsn))
        dirs['heartbeat'] = hb
    dirs = {component: publish(component, staging) for component, staging in dirs.items()}
    emit('monitoringTargets', {'jobs': sorted(j['job_name'] for j in jobs),
                               'probes': sorted(t['labels']['service'] for j in jobs if j['job_name'] == 'health-probes'
                                                for t in j['static_configs']),
                               'receivers': sorted(present) or 'none (alerts are not delivered; set '
                                                                'CFMEET_ALERT_WEBHOOK_URL)'})
    return dirs, bool(present)


def mem_available_mib():
    for line in Path('/proc/meminfo').read_text().splitlines():
        if line.startswith('MemAvailable:'):
            return int(line.split()[1]) // 1024
    raise RuntimeError('MemAvailable missing from /proc/meminfo')


def converge(component, config_dir, egress):
    """Recreates the container only when its configuration or image changed."""
    name = CONTAINERS[component]
    _own(name)
    config_hash = hashlib.sha256((tree_hash(config_dir) + IMAGES[component] + repr(egress)).encode()).hexdigest()[:32]
    if name in container_names():
        c = p.inspect(name)
        if ((c['Config'].get('Labels') or {}).get(LABEL) == config_hash) and c['State']['Running']:
            return 'unchanged'
        p.run('docker', 'rm', '-f', name)
        status = 'replaced'
    else:
        status = 'created'
    p.run(*create_args(component, config_hash, config_dir))
    if egress:
        p.run('docker', 'network', 'connect', EGRESS_NET, name)
    p.run('docker', 'start', name)
    return status


def egress_network():
    """Non-internal bridge without inter-container traffic, for Alertmanager's notifications."""
    if EGRESS_NET not in p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
        p.run('docker', 'network', 'create', '--driver', 'bridge', '--opt',
              'com.docker.network.bridge.enable_icc=false', EGRESS_NET)
    return EGRESS_NET


def ready(url, attempts=30):
    for _ in range(attempts):
        try:
            p.run('docker', 'run', '--rm', '--network', p.NET, PROBE_IMAGE, '-fsS', '--max-time', '4', '-o', '/dev/null', url)
            return True
        except RuntimeError:
            time.sleep(2)
    return False


def target_health():
    """{job: {up, down}} from Prometheus' own target list (read-only)."""
    out = p.run('docker', 'run', '--rm', '--network', p.NET, PROBE_IMAGE, '-fsS', '--max-time', '10',
                'http://%s:9090/api/v1/targets?state=active' % CONTAINERS['prometheus'])
    health = {}
    for t in json.loads(out)['data']['activeTargets']:
        job = t['labels'].get('job', '?')
        bucket = health.setdefault(job, {'up': 0, 'down': 0})
        bucket['up' if t['health'] == 'up' else 'down'] += 1
    return health


def deploy(cr, emit):
    """Renders and converges the stack. Returns a summary; raises on failure."""
    names = container_names()
    new = [c for c in CONTAINERS if CONTAINERS[c] not in names]
    required = sum(MEMORY_MIB[c] for c in new)
    available = mem_available_mib()
    if new and available - required < SAFETY_MARGIN_MIB:
        raise RuntimeError('Monitoring resource gate: %d MiB available, %d MiB required, margin %d MiB'
                           % (available, required, SAFETY_MARGIN_MIB))
    dirs, has_receiver = render_all(cr, emit)
    if has_receiver:
        egress_network()
    result = {}
    for component in ('blackbox', 'heartbeat', 'alertmanager', 'prometheus'):
        if component not in dirs:
            if CONTAINERS[component] in container_names():
                _own(CONTAINERS[component])
                p.run('docker', 'rm', '-f', CONTAINERS[component])
                result[component] = 'removed (no heartbeat source)'
            prune(component, None)
            continue
        result[component] = converge(component, dirs[component], has_receiver and component == 'alertmanager')
        prune(component, dirs[component])
    checks = {
        'prometheus': 'http://%s:9090/-/ready' % CONTAINERS['prometheus'],
        'alertmanager': 'http://%s:9093/-/ready' % CONTAINERS['alertmanager'],
        'blackbox': 'http://%s:9115/-/healthy' % CONTAINERS['blackbox'],
    }
    if 'heartbeat' in dirs:
        checks['heartbeat'] = 'http://%s:9399/metrics' % CONTAINERS['heartbeat']
    readiness = {component: 'PASS' if ready(url) else 'FAIL' for component, url in checks.items()}
    summary = dict(containers=result, ready=readiness)
    try:
        time.sleep(35)  # one scrape interval
        summary['targets'] = target_health()
    except (RuntimeError, ValueError, KeyError) as error:
        summary['targets'] = 'unavailable (%s)' % type(error).__name__
    emit('monitoringStack', summary)
    if any(v != 'PASS' for v in readiness.values()):
        raise RuntimeError('Monitoring stack readiness failed: ' + ', '.join(k for k, v in readiness.items() if v != 'PASS'))
    return summary


def render_example(services, source, out):
    """CI: every configuration this module can deploy, with every optional part present
    (all metrics listeners, the Meeting token, both receivers, the heartbeat exporter) and
    the Alertmanager variant without receivers. No secret is involved; the DSN names no
    password. Checked with promtool / amtool / blackbox / sql_exporter in
    .github/workflows/service-boundaries.yml."""
    out = Path(out)
    envs = {name: {} for name in services}
    for name, var in METRICS_ENV.items():
        envs[name] = {var: '0.0.0.0:9%03d' % (100 + sorted(METRICS_ENV).index(name))}
    envs[SCAN_GATEWAY[1]] = {SCAN_GATEWAY[2]: '0.0.0.0:9199'}
    jobs = scrape_jobs(services, envs, '/etc/prometheus/secrets/meeting-service-internal-api', heartbeat=True)
    files = {
        'prometheus/prometheus.yml': render_prometheus(jobs),
        'alertmanager/alertmanager.yml': render_alertmanager(
            (source / 'alertmanager' / 'alertmanager.yml').read_text(), set(RECEIVER_SECRETS)),
        'alertmanager-none/alertmanager.yml': render_alertmanager(
            (source / 'alertmanager' / 'alertmanager.yml').read_text(), set()),
        'sql-exporter/sql_exporter.yml': render_sql_exporter(
            'postgres://%s@127.0.0.1:5432/%s?sslmode=disable' % (MONITOR_ROLE, MONITOR_DB)),
    }
    for collector in sorted((source / 'sql-exporter').glob('*.collector.yml')):
        files['sql-exporter/' + collector.name] = collector.read_text()
    for rules in sorted((source / 'prometheus' / 'rules').glob('*.rules.yml')):
        files['prometheus/rules/' + rules.name] = rules.read_text()
    files['blackbox/blackbox.yml'] = (source / 'blackbox' / 'blackbox.yml').read_text()
    # Empty placeholders where production mounts secret files (no secret exists in CI).
    files['prometheus/secrets/meeting-service-internal-api'] = ''
    for _secret, _filename, target in RECEIVER_SECRETS.values():
        files['alertmanager/secrets/' + target] = ''
    for rel, text in files.items():
        (out / rel).parent.mkdir(parents=True, exist_ok=True)
        (out / rel).write_text(text)
    return sorted(j['job_name'] for j in jobs)


def deploy_after_release(cr, emit):
    """Never fails the release: a monitoring failure is reported, the release stands."""
    try:
        return deploy(cr, emit)
    except Exception as error:
        message = str(error) if isinstance(error, (RuntimeError, AssertionError)) else ''
        emit('monitoringStack', {'status': 'NOT_UPDATED', 'error': type(error).__name__, 'detail': cr.scrub(message)})
        return None


if __name__ == '__main__':
    import sys
    if len(sys.argv) != 3 or sys.argv[1] != '--render-example':
        sys.exit('usage: monitoring_stack.py --render-example <output dir>')
    import coordinated_release
    repo = HERE.parents[2]
    print(json.dumps(render_example(coordinated_release.SERVICES, repo / 'infra' / 'monitoring', sys.argv[2])))
