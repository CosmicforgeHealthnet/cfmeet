"""Server-audit operations. Host state and credentials never leave the VPS."""
import time
import datetime
import json
from pathlib import Path
import shutil

import production as p

LOG_TARGET = '/var/log/cfmeet'
CAPACITY_OPERATION = 'activate-database-capacity'
CAPACITY_CONFIRM = 'restart-core-database'
LOGS_OPERATION = 'activate-service-log-collector'
LOG_UNIT = Path('/etc/systemd/system/cfmeet-service-logs.service')
MAX_CONNECTIONS = 256
BACKEND_MEMORY_MIB = 16
MEMORY_RESERVE_MIB = 2048
LOG_DEPENDENCIES = frozenset((
    'cfmeet-core-db', 'cfmeet-core-redis', 'cfmeet-core-minio', 'cfmeet-minio',
    'cfmeet-postgres', 'cfmeet-redis', 'cfmeet-calling-redis', 'cfmeet-calling-artifacts',
    'cfmeet-calling-egress', 'cfmeet-storage-ingress', 'cfmeet-messaging-ingress',
    'cfmeet-gateway-ingress', 'cfmeet-prometheus', 'cfmeet-alertmanager',
    'cfmeet-blackbox', 'cfmeet-heartbeat-exporter', 'cfmeet-messaging-kt-witness',
))


def log_containers(catalogue, running):
    return sorted(({spec['container'] for spec in catalogue.values()} | LOG_DEPENDENCIES) & set(running))


def persistent_meeting_logs(cfg):
    """Keep logs outside every replacement container, including activation clones."""
    source = p.ROOT / 'logs' / 'meeting-service'
    root = p.ROOT.resolve()
    if not source.resolve().is_relative_to(root) or source.is_symlink():
        raise RuntimeError('Unexpected persistent log directory')
    host = cfg.setdefault('HostConfig', {})
    for bind in host.get('Binds') or []:
        parts = bind.split(':')
        if len(parts) > 1 and parts[1] == LOG_TARGET:
            if parts[0] != str(source) or (len(parts) > 2 and 'ro' in parts[2].split(',')):
                raise RuntimeError('Conflicting meeting log mount')
            break
    else:
        mounts = host.setdefault('Mounts', [])
        mounted = [m for m in mounts if m.get('Target') == LOG_TARGET]
        if mounted and any(m.get('Source') != str(source) or m.get('ReadOnly') for m in mounted):
            raise RuntimeError('Conflicting meeting log mount')
        if not mounted:
            mounts.append(dict(Type='bind', Source=str(source), Target=LOG_TARGET, ReadOnly=False))
    # The Meeting image currently runs as root. Respect a numeric non-root deployment.
    user = (cfg.get('User') or '0').split(':')
    if not all(part.isdigit() for part in user):
        raise RuntimeError('Meeting log ownership requires a numeric container user')
    p.run('install', '-d', '-m', '0750', '-o', user[0], '-g', user[-1], str(source))
    env = dict(item.split('=', 1) for item in cfg.get('Env', []))
    env['CFMEET_LOG_DIRECTORY'] = LOG_TARGET
    cfg['Env'] = [key + '=' + value for key, value in env.items()]


def capacity_target(plan, current, reserved, available_mib, headroom):
    """Smallest rounded capacity that covers every pool, within a bounded RAM budget."""
    required = sum(plan.values()) + reserved + headroom
    target = max(current, ((required + 31) // 32) * 32)
    if target > MAX_CONNECTIONS:
        raise RuntimeError('Connection plan exceeds the reviewed 256-connection ceiling')
    needed = (target - current) * BACKEND_MEMORY_MIB + MEMORY_RESERVE_MIB
    if target > current and available_mib < needed:
        raise RuntimeError('Insufficient available memory for the database capacity increase')
    return target


def wait_database(cr):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        try:
            return int(cr.release.db_sql('postgres', 'SHOW max_connections;').strip())
        except (RuntimeError, ValueError):
            time.sleep(1)
    raise RuntimeError('Core database did not become ready within 90 seconds')


def set_capacity(cr, capacity):
    if not isinstance(capacity, int) or not 10 <= capacity <= MAX_CONNECTIONS:
        raise RuntimeError('Invalid reviewed database capacity')
    cr.release.db_sql('postgres', 'ALTER SYSTEM SET max_connections = %d;' % capacity)
    # Only the CF Meet-owned core database. No other postgres container is touched.
    p.run('docker', 'restart', '--time', '60', 'cfmeet-core-db')
    if wait_database(cr) != capacity:
        raise RuntimeError('Core database did not apply its connection capacity')


def activate_database_capacity(cr, sha, run_id, confirm):
    """Briefly restart core Postgres before increasing any application pool. Roll back
    the setting if health/regression fails. Pool activation stays a separate operation."""
    if confirm != CAPACITY_CONFIRM:
        raise RuntimeError('Database capacity needs confirm=restart-core-database (brief connection interruption)')
    cr.require_release(sha)
    current = int(cr.release.db_sql('postgres', 'SHOW max_connections;').strip())
    reserved = int(cr.release.db_sql('postgres', 'SHOW superuser_reserved_connections;').strip())
    overrides = {n: {cr.POOL_SETTINGS[n][0]: v} for n, v in cr.DATABASE_POOLS.items()}
    plan = cr.pool_plan(cr.running_envs(), overrides)
    target = capacity_target(plan, current, reserved, cr.mem_available_mib(), cr.POOL_HEADROOM)
    entry = cr.begin(CAPACITY_OPERATION, sha, run_id, False)
    entry['databaseCapacity'] = dict(before=current, after=target)
    cr.commit(entry)
    try:
        if target != current:
            set_capacity(cr, target)
        p.regression()
    except Exception:
        try:
            if target != current:
                set_capacity(cr, current)
        finally:
            entry['status'] = 'failed'
            cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('databaseCapacity', dict(before=current, after=target, poolConnections=sum(plan.values())))


def restore_capacity(cr, record):
    """Refuse to reduce capacity while application pools still need it. Roll pools back first."""
    before = record['before']
    reserved = int(cr.release.db_sql('postgres', 'SHOW superuser_reserved_connections;').strip())
    if cr.pool_budget_errors(cr.pool_plan(cr.running_envs(), {}), before, reserved):
        raise RuntimeError('Roll back application pools before reducing database capacity')
    set_capacity(cr, before)


def log_unit(root):
    return '''# CF Meet owned service log collector
[Unit]
Description=CF Meet persistent service logs (14 complete days)
After=docker.service
Requires=docker.service
[Service]
Type=simple
ExecStart=/usr/bin/python3 {root}/log-collector/collector.py {root}/log-collector/config.json
Restart=always
RestartSec=3
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths={root}/logs/services
RestrictAddressFamilies=AF_UNIX
CapabilityBoundingSet=
MemoryMax=512M
TasksMax=256
[Install]
WantedBy=multi-user.target
'''.format(root=root)


def restore_service_logs(record):
    # Restore configuration, never remove the archived service logs.
    allowed = {str(LOG_UNIT), str(p.ROOT.resolve() / 'log-collector' / 'config.json'),
               str(p.ROOT.resolve() / 'log-collector' / 'collector.py')}
    if set(record['files']) != allowed or any(Path(name).is_symlink() for name in allowed):
        raise RuntimeError('Refusing an unexpected log collector rollback path')
    if any(not Path(name).resolve().is_relative_to(p.ROOT.resolve()) for name in allowed if name != str(LOG_UNIT)):
        raise RuntimeError('Refusing a log collector rollback outside the owned root')
    p.run('systemctl', 'stop', LOG_UNIT.name)
    for name, previous in record['files'].items():
        path = Path(name)
        if previous is None:
            path.unlink(missing_ok=True)
        else:
            p.write_private(path, previous)
    p.run('systemctl', 'daemon-reload')
    if record['wasActive']:
        p.run('systemctl', 'start', LOG_UNIT.name)
    else:
        p.run('systemctl', 'disable', LOG_UNIT.name)


def activate_service_logs(cr, sha, run_id, confirm):
    if confirm != 'retain-14-days':
        raise RuntimeError('confirm must be retain-14-days')
    cr.require_release(sha)
    root = p.ROOT.resolve()
    if any(c.isspace() for c in str(root)) or not str(root).startswith('/opt/cfmeet'):
        raise RuntimeError('Unexpected production archive root')
    if shutil.disk_usage(root).free < 2 * 1024**3:
        raise RuntimeError('At least 2 GiB free disk is required before starting log archival')
    # The catalogue is authoritative; unrelated/staging/previous containers are excluded.
    running = set(p.run('docker', 'ps', '--format', '{{.Names}}').splitlines())
    names = log_containers(cr.SERVICES, running)
    if not names:
        raise RuntimeError('No running CF Meet services to archive')
    directory = root / 'logs' / 'services'
    config = root / 'log-collector' / 'config.json'
    program = root / 'log-collector' / 'collector.py'
    for path in (directory, config, program, LOG_UNIT):
        if path.is_symlink():
            raise RuntimeError('Log collector paths cannot be symlinks')
        if path != LOG_UNIT and not path.resolve().is_relative_to(root):
            raise RuntimeError('Log collector paths must stay inside the owned root')
    if LOG_UNIT.exists() and '# CF Meet owned service log collector' not in LOG_UNIT.read_text():
        raise RuntimeError('Refusing to replace an unowned systemd unit')
    files = {str(path): path.read_text() if path.exists() else None for path in (config, program, LOG_UNIT)}
    was_active = p.run('systemctl', 'show', LOG_UNIT.name, '--property=ActiveState', '--value') == 'active'
    entry = cr.begin(LOGS_OPERATION, sha, run_id, False)
    entry['serviceLogs'] = dict(files=files, wasActive=was_active)
    cr.commit(entry)
    try:
        p.run('install', '-d', '-m', '0700', str(directory))
        p.write_private(program, Path(__file__).with_name('service_log_collector.py').read_text())
        p.write_private(config, json.dumps({'directory': str(directory), 'containers': names}))
        p.write_private(LOG_UNIT, log_unit(root))
        p.run('systemctl', 'daemon-reload')
        p.run('systemctl', 'enable', LOG_UNIT.name)
        started = datetime.datetime.now(datetime.timezone.utc)
        p.run('systemctl', 'restart', LOG_UNIT.name)
        for _ in range(35):
            health_file = directory / 'health.json'
            if health_file.exists():
                health = json.loads(health_file.read_text())
                age = datetime.datetime.now(datetime.timezone.utc) - datetime.datetime.fromisoformat(health['at'])
                observed = datetime.datetime.fromisoformat(health['at'])
                if observed >= started and 0 <= age.total_seconds() < 30 and health['threadsAlive'] == len(names):
                    break
            time.sleep(1)
        else:
            raise RuntimeError('Log collector did not become healthy')
    except Exception:
        restore_service_logs(entry['serviceLogs'])
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('serviceLogs', {'containers': names, 'minimumRetentionDays': 14,
                           'archiveStartedNow': True, 'historicalRecovery': 'AVAILABLE_DOCKER_LOGS_ONLY'})
