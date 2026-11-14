"""Read-only live-production certification of the 13 CF Meet backend containers.

Dispatched as the `certify` operation of cfmeet-coordinated-release.yml and run on the VPS
by coordinated_release.py. Unlike `verify`, it does not require production to run one
coordinated release SHA: it certifies the topology that is actually deployed, service by
service, and records the image revision of each container. The runner then checks each
revision's ancestry, its source against the main head, and its expected migrations
(certification_report.py). `verify` keeps its strict same-SHA contract unchanged.

Read-only by construction:
  - no container is stopped, started, renamed, created (except ephemeral --rm,
    --pull never, read-only psql containers), removed or reconfigured;
  - HTTP probes run the host's own curl inside the service's network namespace
    (nsenter -n): no container is created for them. Names are resolved from Docker's
    own records (curl --resolve), so TLS is verified against the real hostname;
  - no file on the host is written: database URLs reach psql through the environment of
    the docker CLI process (`docker run -e NAME`), never a file or the command line;
  - every SQL session is read-only (default_transaction_read_only=on);
  - HTTP probes are credential-free GETs that the services reject in their auth layer
    before any handler runs, or health/readiness reads;
  - nothing is pulled: a missing psql image is reported, not fetched.

Output: one JSON object per line (`certifyService`, `certifyPlatform`, `certifyCollected`).
Secrets are never printed; error messages are reduced to their type.
"""
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path
from urllib.parse import urlsplit

import production as p
import coordinated_release as cr
import communications_release as release

PG = cr.PG_CLIENT
# Containers whose name or network alias a service may use to reach a dependency.
RESOLVABLE = ('cfmeet-core-tls', 'cfmeet-core-db')
SAFE_RUN = ('--rm', '--pull', 'never', '--read-only', '--cap-drop', 'ALL',
            '--security-opt', 'no-new-privileges', '--pids-limit', '64', '--memory', '128m')
READ_ONLY_SQL = 'SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY;\n'

# Readiness: what each service's own readiness route proves (services/*/src, see the
# certification document). The meeting service's /health is liveness only; its database
# is proven separately by the service-credential query below.
READINESS_PROVES_DB = {
    'platform-access': True, 'organizations': True, 'messaging': True, 'calling': True,
    'communications': True, 'numbering': True, 'privacy-relay': True, 'sms-gateway': True,
    'telephony': True, 'meeting-service': False, 'developer-platform': True,
    'developer-platform-worker': False, 'api-gateway': False,
}

# One credential-free GET per service that its authentication layer must reject before any
# handler runs (401). Placeholder IDs cannot match data.
NIL = '00000000-0000-0000-0000-000000000000'
AUTH_PROBES = {
    'platform-access': '/internal/v1/scopes',
    'organizations': '/internal/v1/admin/event-delivery',
    'messaging': '/v1/messaging/sms-bridge/disclosure',
    'calling': '/internal/v1/webhook-migration/inventory',
    'communications': '/internal/v1/communications/metrics',
    'numbering': '/internal/v1/number-resources/' + NIL,
    'privacy-relay': '/internal/v1/contexts/' + NIL,
    'sms-gateway': '/internal/v1/sms-bridge/messages/' + NIL,
    'telephony': '/internal/v1/routes/availability',
    'meeting-service': '/api/v1/users/me',
    'developer-platform': '/internal/v1/operator/activity',
    'api-gateway': '/v1/calls',
}

# External integrations and the switch that governs each. A value in `off` means the
# integration is disabled in production; anything else is reported as enabled.
INTEGRATIONS = (
    ('sms-gateway', 'SMS_GATEWAY_PROVIDER', 'SMS carrier provider', (None, '', 'none')),
    ('numbering', 'NUMBERING_PROVIDERS', 'Number inventory providers', (None, '')),
    ('numbering', 'NUMBERING_PORTING_ENABLED', 'Number porting', (None, 'false')),
    ('telephony', 'TELEPHONY_ENABLED', 'PSTN telephony', (None, 'false')),
    ('telephony', 'PUBLIC_OUTBOUND_CALLING_ENABLED', 'Public outbound PSTN calling', (None, 'false')),
    ('telephony', 'PUBLIC_INBOUND_CALLING_ENABLED', 'Public inbound PSTN calling', (None, 'false')),
    ('telephony', 'CALLBACK_RELAY_ENABLED', 'Callback relay', (None, 'false')),
    ('telephony', 'ENTERPRISE_SIP_ENABLED', 'Enterprise SIP trunking', (None, 'false')),
    ('telephony', 'TELEPHONY_SIP_EDGE', 'SIP edge', (None, 'none')),
    ('privacy-relay', 'PRIVACY_RELAY_ENABLED', 'Privacy relay', (None, 'false')),
    ('privacy-relay', 'CONTACT_VAULT_ENABLED', 'Contact vault', (None, 'false')),
    ('privacy-relay', 'RELAY_NUMBER_POOL_ENABLED', 'Relay number pool', (None, 'false')),
    ('messaging', 'MESSAGING_PUSH_PROVIDER', 'Mobile push provider', (None, '', 'none')),
    ('messaging', 'MESSAGING_E2EE_DEVICES_ENABLED', 'End-to-end encrypted devices', (None, 'false')),
    ('calling', 'CALLING_PUBLIC_OUTBOUND_ENABLED', 'Calling public outbound', (None, 'false')),
    ('calling', 'CALLING_PUBLIC_INBOUND_ENABLED', 'Calling public inbound', (None, 'false')),
    ('calling', 'CALL_RECORDING_ENABLED', 'Call recording', (None, 'false')),
    ('calling', 'VOICEMAIL_ENABLED', 'Voicemail', (None, 'false')),
    ('calling', 'ENTERPRISE_CALLING_ENABLED', 'Enterprise calling', (None, 'false')),
    ('calling', 'CALLING_MEDIA_PROVIDER', 'Media provider (LiveKit)', (None, '', 'none')),
    ('developer-platform', 'DEVELOPER_PLATFORM_WEBHOOKS_ENABLED', 'Customer webhooks', (None, 'false')),
    ('developer-platform', 'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE', 'Webhook delivery', (None, 'shadow')),
    ('developer-platform', 'DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED', 'Production access workflow', (None, 'false')),
    ('developer-platform', 'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED', 'Developer gateway API', (None, 'false')),
    ('api-gateway', 'API_GATEWAY_PUBLIC_ROUTING_ENABLED', 'API gateway public routing', (None, 'false')),
    ('api-gateway', 'API_GATEWAY_WEBSOCKETS_ENABLED', 'API gateway WebSockets', (None, 'false')),
    ('organizations', 'ORGANIZATIONS_PROVISIONING_ENABLED', 'Organization provisioning', (None, 'false')),
    ('organizations', 'ORGANIZATIONS_SERVICE_BINDINGS_ENABLED', 'Automatic organization service bindings', (None, 'false')),
)

UPSTREAM_URL = re.compile(r'^CF_[A-Z_]+_BASE_URL$')


def emit(kind, value):
    cr.emit(kind, value)


def docker(args, env=None, timeout=60):
    """(exit code, stdout). stderr is discarded: it can contain connection strings."""
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL,
                             env=dict(os.environ, **env) if env else None)
    except subprocess.TimeoutExpired:
        return 124, ''
    return out.returncode, out.stdout.strip()


def has_image(ref):
    return docker(['docker', 'image', 'inspect', '--format', '{{.Id}}', ref])[0] == 0


# --- HTTP from inside a service's own network namespace -------------------------------------

def address_of(hostname, container):
    """IPv4 address Docker's DNS gives `hostname` inside `container` (a container name or
    network alias on a network the two share), or None."""
    networks = p.inspect(container)['NetworkSettings']['Networks']
    candidates = [s['container'] for s in cr.SERVICES.values()] + list(RESOLVABLE)
    for candidate in candidates:
        try:
            other = p.inspect(candidate)
        except RuntimeError:
            continue
        for net, endpoint in (other['NetworkSettings']['Networks'] or {}).items():
            names = {candidate} | set(endpoint.get('Aliases') or []) | set(endpoint.get('DNSNames') or [])
            if net in networks and hostname in names and endpoint.get('IPAddress'):
                return endpoint['IPAddress']
    return None


def http_probe(container, url, timeout=8):
    """(status, body excerpt). Status 0: no HTTP answer (connection, TLS or name failure)."""
    target = urlsplit(url)
    pid = p.inspect(container)['State'].get('Pid')
    if not pid:
        return 0, 'container has no process'
    args = ['nsenter', '-t', str(pid), '-n', '--', 'curl', '-sS', '--noproxy', '*', '--proto', '=http,https',
            '--max-time', str(timeout), '-o', '/dev/stdout', '-w', '\n%{http_code}']
    if target.hostname not in ('127.0.0.1', 'localhost'):
        address = address_of(target.hostname, container)
        if not address:
            return 0, 'name not resolvable on the service networks'
        port = target.port or (443 if target.scheme == 'https' else 80)
        args += ['--resolve', '%s:%d:%s' % (target.hostname, port, address)]
    code, out = docker(args + [url], timeout=timeout + 30)
    body, _, status = out.rpartition('\n')
    status = int(status) if status.isdigit() else 0
    return status, cr.scrub(body)[:300]


def readiness(name, container):
    s = cr.SERVICES[name]
    if not s['port']:
        before = p.inspect(container)
        time.sleep(15)
        after = p.inspect(container)
        ok = after['State']['Running'] and after['RestartCount'] == before['RestartCount']
        return dict(kind='stays-running-15s', result='PASS' if ok else 'FAIL')
    status, body = http_probe(container, 'http://127.0.0.1:%d%s' % (s['port'], s['health']))
    return dict(kind=s['health'], status=status, body=body,
                provesDatabase=READINESS_PROVES_DB[name], result='PASS' if status == 200 else 'FAIL')


def authentication(name, container):
    path = AUTH_PROBES.get(name)
    if not path:
        return dict(result='N/A', reason='no listener (worker)')
    status, body = http_probe(container, 'http://127.0.0.1:%d%s' % (cr.SERVICES[name]['port'], path))
    code = re.search(r'"code"\s*:\s*"([A-Z_]+)"', body)
    expected = (401,)
    if name == 'api-gateway' and p.env_of(p.inspect(container)).get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true':
        expected = (404,)  # deny-all table while public routing is off
    return dict(probe='GET ' + path, status=status, errorCode=code.group(1) if code else None,
                expected=list(expected), result='PASS' if status in expected else 'FAIL')


# --- Database --------------------------------------------------------------------------------

def service_sql(container, url, sql):
    """psql as the service itself (its own URL, its own network namespace), read-only."""
    code, out = docker(['docker', 'run', *SAFE_RUN, '--network', 'container:' + container,
                        '-e', 'DATABASE_URL', '-e', 'PGOPTIONS', '-e', 'PGCONNECT_TIMEOUT',
                        '--entrypoint', 'sh', PG, '-c', 'exec psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 -c "$0"', sql],
                       env={'DATABASE_URL': url, 'PGOPTIONS': '-c default_transaction_read_only=on',
                            'PGCONNECT_TIMEOUT': '8'})
    if code:
        raise RuntimeError('service database query failed')
    return out


def database(name, container, env):
    s = cr.SERVICES[name]
    url = env.get('DATABASE_URL')
    if not url:
        return dict(result='N/A', reason='service has no database')
    target = urlsplit(url)
    expected_db = s['db']
    actual_db = target.path.lstrip('/')
    try:
        row = service_sql(container, url, "SELECT current_database() || '|' || current_user || '|' || "
                                          "current_setting('transaction_read_only') || '|' || "
                                          "current_setting('server_version_num')")
        db, user, read_only, version = row.split('|')
    except (RuntimeError, ValueError):
        return dict(result='FAIL', host=target.hostname, database=actual_db, reason='connection or query failed')
    matches = expected_db is None or db == expected_db
    return dict(result='PASS' if matches and read_only == 'on' else 'FAIL', host=target.hostname,
                database=db, catalogueDatabase=expected_db, role=user, sessionReadOnly=read_only == 'on',
                serverVersion=version, matchesCatalogue=matches)


def history_sql(name, container, env):
    if name == 'meeting-service':
        return lambda q: service_sql(container, env['DATABASE_URL'], q)
    db = cr.SERVICES[name]['db']
    return lambda q: release.db_sql(db, READ_ONLY_SQL + q)


def migrations(name, container, env):
    s = cr.SERVICES[name]
    if not s['history'] or s['identity']:
        return dict(result='N/A', reason='shares the database of ' + (s['identity'] or 'nothing'))
    sql = history_sql(name, container, env)
    try:
        tables = cr.history_tables(sql)
        applied = {t: [v for v in sql('SELECT version FROM "%s" ORDER BY 1;' % t).splitlines() if v] for t in tables}
    except RuntimeError:
        return dict(result='FAIL', reason='history tables unreadable')
    missing = [t for t in s['history'] if not applied.get(t)]
    return dict(result='FAIL' if missing else 'PASS', tables=sorted(applied), applied=applied,
                missingHistoryTables=missing)


# --- Service-to-service ------------------------------------------------------------------------

def upstream_owner(url):
    """Catalogue service behind a configured URL: the private TLS route (api.cfmeet.com:<tls
    port>) or a direct container address (<container>:<port>)."""
    for name, s in cr.SERVICES.items():
        if url.scheme == 'https' and url.hostname == 'api.cfmeet.com' and s['tls'] and url.port == s['tls']:
            return name
        if url.hostname == s['container'] and s['port'] and (url.port or 80) == s['port']:
            return name
    return None


def upstreams(name, container, env):
    """Every private HTTPS dependency the service is configured with, reached from the
    service's own network namespace with full TLS verification."""
    checks = []
    for var in sorted(k for k in env if UPSTREAM_URL.match(k)):
        url = urlsplit(env[var])
        owner = upstream_owner(url)
        if not owner:
            checks.append(dict(variable=var, result='WARN', reason='not a private service route'))
            continue
        path = cr.SERVICES[owner]['health']
        status, _ = http_probe(container, '%s://%s:%d%s' % (url.scheme, url.hostname, url.port or 80, path))
        checks.append(dict(variable=var, upstream=owner, path=path, status=status, tls=url.scheme == 'https',
                           result='PASS' if status == 200 else 'FAIL'))
    failed = [c for c in checks if c['result'] == 'FAIL']
    return dict(result='N/A' if not checks else ('FAIL' if failed else 'PASS'), checks=checks)


def trust(name, c, env, journal_ops):
    """Trust bundle and signing identity. Grants of completed activations must be present
    with the issuer's current public key."""
    out = dict(signingKeyId=env.get('CF_SERVICE_AUTH_KEY_ID'))
    problems = []
    key_target = env.get('CF_SERVICE_AUTH_PRIVATE_KEY_FILE')
    if name != 'api-gateway' or key_target:
        try:
            out['signingKeyMounted'] = Path(cr.mount_source(c, key_target)).is_file() if key_target else False
        except RuntimeError:
            out['signingKeyMounted'] = False
        if not out['signingKeyMounted']:
            problems.append('signing key not mounted')
    if env.get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        try:
            _, _, bundle = cr.current_bundle(c)
        except (RuntimeError, OSError, ValueError):
            return dict(out, result='FAIL', problems=problems + ['trust bundle unreadable'])
        out['receiver'] = bundle.get('receiver')
        out['runtimeEnvironment'] = bundle.get('runtime_environment')
        out['issuers'] = {i['issuer']: dict(kids=[k.get('kid') for k in i['keys']], permissions=sorted(i['permissions']))
                          for i in bundle.get('trusted_issuers', [])}
        if bundle.get('receiver') != cr.service_id(name):
            problems.append('bundle receiver %s' % bundle.get('receiver'))
        if bundle.get('runtime_environment') != 'production':
            problems.append('bundle is not a production bundle')
        grants = []
        for op, receiver, issuer, permissions in cr.GRANTS:
            if receiver != name or op not in journal_ops:
                continue
            entry = next((i for i in bundle['trusted_issuers'] if i['issuer'] == cr.service_id(issuer)), None)
            try:
                key = cr.issuer_key(issuer)
            except (RuntimeError, AssertionError, OSError):
                key = None
            present = bool(entry) and set(permissions) <= set(entry['permissions'])
            key_ok = bool(entry and key) and any(k.get('kid') == key['kid'] and k.get('x') == key['x'] for k in entry['keys'])
            grants.append(dict(activation=op, issuer=issuer, permissions=list(permissions),
                               present=present, issuerKeyCurrent=key_ok))
            if not (present and key_ok):
                problems.append('grant %s from %s' % (op, issuer))
        out['activatedGrants'] = grants
    elif name not in ('api-gateway', 'meeting-service'):
        problems.append('no trust bundle configured')
    return dict(out, result='FAIL' if problems else 'PASS', problems=problems)


# --- Image and container identity --------------------------------------------------------------

def image_identity(name, c, journal_registry):
    meta = json.loads(p.run('docker', 'image', 'inspect', c['Image']))[0]
    labels = (meta.get('Config') or {}).get('Labels') or {}
    container_labels = c['Config'].get('Labels') or {}
    revision = labels.get('org.opencontainers.image.revision')
    out = dict(imageId=c['Image'], imageRevision=revision,
               imageSource=labels.get('org.opencontainers.image.source'),
               imageCreated=meta.get('Created'), repoDigests=meta.get('RepoDigests') or [],
               containerLabelRevision=container_labels.get('org.opencontainers.image.revision'),
               releaseShaLabel=container_labels.get('com.cfmeet.release.sha'))
    out['containerLabelStale'] = bool(out['containerLabelRevision']) and out['containerLabelRevision'] != revision
    image = cr.image_of(name)
    if revision and cr.SHA_RE.fullmatch(revision):
        ref = cr.image_ref(image, revision)
        code, tagged = docker(['docker', 'image', 'inspect', '--format', '{{.Id}}', ref])
        out['registryTag'] = ref
        out['registryTagMatches'] = code == 0 and tagged == c['Image']
    else:
        out['registryTagMatches'] = False
    if journal_registry is not None:
        out['matchesJournaledRelease'] = journal_registry.get(image, {}).get('imageId') == c['Image']
    ok = bool(revision and cr.SHA_RE.fullmatch(revision)) and out['registryTagMatches'] \
        and out['imageSource'] == 'https://github.com/CosmicforgeHealthnet/cfmeet'
    return dict(out, result='PASS' if ok else 'FAIL')


def container_state(c):
    state = c['State']
    ok = state.get('Running') and not state.get('Restarting') and not state.get('OOMKilled')
    return dict(result='PASS' if ok else 'FAIL', status=state.get('Status'), startedAt=state.get('StartedAt'),
                restartCount=c.get('RestartCount'), oomKilled=state.get('OOMKilled'),
                restartPolicy=(c['HostConfig'].get('RestartPolicy') or {}).get('Name'),
                readOnlyRootfs=c['HostConfig'].get('ReadonlyRootfs'),
                memoryMiB=(c['HostConfig'].get('Memory') or 0) // 1048576)


def integrations(envs):
    report = []
    for service, var, label, off in INTEGRATIONS:
        env = envs.get(service)
        if env is None:
            continue
        value = env.get(var)
        shown = value if value is not None and not cr.SECRET_HINT.search(var) else (None if value is None else 'set')
        report.append(dict(service=service, variable=var, integration=label, value=shown,
                           state='disabled' if value in off else 'enabled'))
    meeting = envs.get('meeting-service') or {}
    mail = cr.mail_presence(meeting) if meeting else {}
    provider = 'resend' if mail.get('resendConfigured') else ('smtp' if mail.get('smtpHostConfigured') else 'none')
    report.append(dict(service='meeting-service', variable='EMAIL_PROVIDER', integration='Transactional email',
                       value=provider, state='configured' if meeting and cr.mail_available(meeting) else 'not configured'))
    report.append(dict(service='meeting-service', variable='GOOGLE_CLIENT_ID', integration='Google sign-in',
                       value=None, state='configured' if meeting.get('GOOGLE_CLIENT_ID') else 'not configured'))
    return report


# --- Orchestration ---------------------------------------------------------------------------

def journal_state():
    journal = cr.load_journal()
    done_ops = {e['operation'] for e in journal if e['status'] == 'done'}
    latest = cr.last_release()
    registry = None
    if latest:
        path = cr.WORK / ('registry-' + latest['sha'] + '.json')
        registry = json.loads(path.read_text()) if path.exists() else None
    return journal, done_ops, latest, registry


def certify_service(name, names, envs, done_ops, registry):
    s = cr.SERVICES[name]
    container = s['container']
    result = dict(service=name, container=container)
    if container not in names:
        result['container_state'] = dict(result='FAIL', status='absent')
        return result
    c = p.inspect(container)
    env = envs[name]
    steps = (('container_state', lambda: container_state(c)),
             ('image', lambda: image_identity(name, c, registry)),
             ('readiness', lambda: readiness(name, container)),
             ('database', lambda: database(name, container, env)),
             ('migrations', lambda: migrations(name, container, env)),
             ('authentication', lambda: authentication(name, container)),
             ('service_auth', lambda: trust(name, c, env, done_ops)),
             ('upstreams', lambda: upstreams(name, container, env)))
    for key, step in steps:
        try:
            result[key] = step()
        except Exception as error:  # one failing check must not hide the others
            result[key] = dict(result='FAIL', error=type(error).__name__)
    result['restartCountAtStart'] = c.get('RestartCount')
    return result


def platform(envs, latest, journal):
    out = dict(journal=[{k: e.get(k) for k in ('operation', 'sha', 'runId', 'status')} for e in journal[-12:]],
               latestRelease=latest and {k: latest[k] for k in ('sha', 'runId', 'status')})
    violations = cr.safe_default_violations(envs)
    out['providerSafeDefaults'] = dict(result='PASS' if not violations else 'FAIL', violations=violations)
    out['integrations'] = integrations(envs)
    out['featureFlags'] = {name: cr.safe_flags(env) for name, env in envs.items()}
    try:
        delivery = cr.event_delivery()
        out['organizationsEventDelivery'] = dict(states=delivery, result='PASS' if not delivery.get('DEAD') else 'FAIL')
    except Exception as error:
        out['organizationsEventDelivery'] = dict(result='FAIL', error=type(error).__name__)
    try:
        out['publicSmokeFromVps'] = cr.public_smoke()
    except Exception as error:
        out['publicSmokeFromVps'] = {'error ' + type(error).__name__: 'FAIL'}
    try:
        p.identity()
        out['coreIdentity'] = dict(result='PASS')
    except Exception as error:
        out['coreIdentity'] = dict(result='FAIL', error=type(error).__name__)
    out['resources'] = dict(memAvailableMiB=cr.mem_available_mib(), diskFreeGiB=round(cr.disk_free_bytes('/') / 2**30, 1))
    return out


def certify(sha, run_id):
    emit('certifyStarted', dict(referenceSha=sha, runId=run_id, at=cr.now(), services=list(cr.ORDER)))
    missing = [ref for ref in (PG,) if not has_image(ref)]
    if missing:
        raise RuntimeError('Probe images are not present on the host (certify never pulls): ' + ', '.join(missing))
    tools = [tool for tool in ('nsenter', 'curl') if not shutil.which(tool)]
    if tools:
        raise RuntimeError('Host tools missing: ' + ', '.join(tools))
    names = cr.container_names()
    envs = cr.running_envs()
    journal, done_ops, latest, registry = journal_state()
    results = [certify_service(name, names, envs, done_ops, registry) for name in cr.ORDER]
    for result in results:
        container = result['container']
        if container in cr.container_names():
            end = p.inspect(container)
            result['restart_stability'] = dict(
                result='PASS' if end.get('RestartCount') == result.get('restartCountAtStart') and end['State']['Running'] else 'FAIL',
                restartCountAtEnd=end.get('RestartCount'))
        emit('certifyService', result)
    emit('certifyPlatform', platform(envs, latest, journal))
    emit('certifyCollected', dict(at=cr.now(), services=len(results)))
