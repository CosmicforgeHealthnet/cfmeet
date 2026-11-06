"""Durable certificate material for the private TLS proxy (`cfmeet-core-tls`).

The proxy was created with two single-file bind mounts of numbered Let's Encrypt archive
files (`archive/api.cfmeet.com/fullchain4.pem`, `privkey4.pem`). A file bind mount pins
the inode it was given: certbot writes a renewal as new numbered files and repoints the
`live/` symlinks, so the running proxy would keep presenting the old certificate until it
expired, whatever it was told to reload.

`private-tls-durable` replaces that with read-only *directory* mounts of the managed
lineage (`live/api.cfmeet.com` and `archive/api.cfmeet.com`, at the same paths, so the
relative `live/` symlinks resolve inside the container) and points nginx at
`live/api.cfmeet.com/{fullchain,privkey}.pem`. A renewal is then visible to the
container, and a certbot deploy hook validates it and reloads nginx (graceful: the old
workers finish their connections). A daily timer checks expiry and that the presented
certificate is the managed one, and repairs a missed reload.

The one-time switch starts the replacement container next to the running one (both
answer the `api.cfmeet.com` alias with the same routes), proves every private route on
the replacement, then stops the previous proxy gracefully and keeps it, stopped, for
`private-tls-rollback`. LiveKit media does not pass through this proxy.
"""
import copy
import datetime
import hashlib
import json
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

import production as p

CONTAINER = 'cfmeet-core-tls'
HOSTNAME = 'api.cfmeet.com'
LETSENCRYPT = Path('/etc/letsencrypt')
LIVE = LETSENCRYPT / 'live' / HOSTNAME
ARCHIVE = LETSENCRYPT / 'archive' / HOSTNAME
WORK = p.ROOT / 'tls-proxy'
CONF = WORK / 'nginx.conf'
STATE = WORK / 'durable-rollback.json'
HOOK = LETSENCRYPT / 'renewal-hooks' / 'deploy' / 'cfmeet-core-tls'
MONITOR = WORK / 'certificate-monitor.py'
MONITOR_CONFIG = WORK / 'certificate-monitor.json'
STATUS = WORK / 'certificate-status.json'
UNIT = 'cfmeet-core-tls-certificate'
SYSTEMD = Path('/etc/systemd/system')
CA_BUNDLE = '/etc/ssl/certs/ca-certificates.crt'
CURL = 'curlimages/curl:8.12.1'
PINNED = {'/etc/nginx/tls/fullchain.pem', '/etc/nginx/tls/privkey.pem'}
OLD_DIRECTIVES = ('ssl_certificate /etc/nginx/tls/fullchain.pem;',
                  'ssl_certificate_key /etc/nginx/tls/privkey.pem;')
NEW_DIRECTIVES = ('ssl_certificate %s/fullchain.pem;' % LIVE,
                  'ssl_certificate_key %s/privkey.pem;' % LIVE)
# Refuse to switch to material this close to expiry; alert well before certbot's own
# 30-day renewal window closes.
MIN_REMAINING_DAYS = 7
# Operator bands: 45 and 30 days warn (certbot renews at 30), 14 and 7 alert.
ALERT_THRESHOLDS = (45, 30, 14, 7)
STABLE_HANDSHAKES = 5
RELOAD_ATTEMPTS = 3
# Production private routes (port, path). Stage routes are not checked: their
# candidates are stopped between stage runs by design.
ROUTES = ((8443, '/ready'), (8444, '/ready'), (8445, '/health'), (8446, '/'),
          (8447, '/ready'), (8448, '/health'), (8449, '/ready'), (443, '/health'))


def sh(*args, input=None, timeout=60):
    r = subprocess.run(args, input=input, capture_output=True, text=True, timeout=timeout)
    return r.returncode, r.stdout, r.stderr


# ---- certificate material ---------------------------------------------------------

def split_pem(text):
    blocks, current = [], []
    for line in text.splitlines():
        if line.startswith('-----BEGIN CERTIFICATE-----'):
            current = [line]
        elif current:
            current.append(line)
            if line.startswith('-----END CERTIFICATE-----'):
                blocks.append('\n'.join(current) + '\n')
                current = []
    return blocks


def certificate_facts(pem):
    """Fingerprint, expiry and DNS names of the first certificate in `pem`."""
    code, out, _ = sh('openssl', 'x509', '-noout', '-fingerprint', '-sha256', '-enddate',
                      '-ext', 'subjectAltName', input=pem)
    assert code == 0, 'Certificate is not parseable'
    fingerprint = expires = None
    names = []
    for line in out.splitlines():
        line = line.strip()
        if line.lower().startswith('sha256 fingerprint='):
            fingerprint = line.split('=', 1)[1].replace(':', '').lower()
        elif line.startswith('notAfter='):
            expires = datetime.datetime.strptime(line.split('=', 1)[1].replace('  ', ' '),
                                                 '%b %d %H:%M:%S %Y %Z').replace(tzinfo=datetime.timezone.utc)
        elif 'DNS:' in line:
            names += [x.strip().removeprefix('DNS:') for x in line.split(',') if 'DNS:' in x]
    assert fingerprint and expires, 'Certificate facts unavailable'
    return {'fingerprint': fingerprint, 'expiresAt': expires.isoformat().replace('+00:00', 'Z'),
            'names': names, '_expires': expires}


def validate_material(folder=LIVE, now=None, min_days=MIN_REMAINING_DAYS, ca=CA_BUNDLE):
    """The lineage's current chain verifies to a public root for HOSTNAME as a TLS server,
    the private key matches it, and it has at least `min_days` left. Returns its facts."""
    now = now or datetime.datetime.now(datetime.timezone.utc)
    chain_text = (folder / 'fullchain.pem').read_text()
    blocks = split_pem(chain_text)
    assert len(blocks) >= 2, 'Certificate chain must include its intermediates'
    facts = certificate_facts(blocks[0])
    assert HOSTNAME in facts['names'], 'Certificate does not name ' + HOSTNAME
    with tempfile.TemporaryDirectory() as tmp:
        leaf, rest = Path(tmp) / 'leaf.pem', Path(tmp) / 'chain.pem'
        leaf.write_text(blocks[0]); rest.write_text(''.join(blocks[1:]))
        code, _, _ = sh('openssl', 'verify', '-CAfile', ca, '-untrusted', str(rest),
                        '-purpose', 'sslserver', '-verify_hostname', HOSTNAME, str(leaf))
        assert code == 0, 'Certificate chain does not verify for ' + HOSTNAME
    _, cert_key, _ = sh('openssl', 'x509', '-noout', '-pubkey', input=blocks[0])
    # The key is read by openssl from its path; its bytes never enter this process.
    code, key_pub, _ = sh('openssl', 'pkey', '-pubout', '-in', str(folder / 'privkey.pem'))
    assert code == 0 and cert_key.strip() and cert_key.strip() == key_pub.strip(), 'Private key does not match the certificate'
    remaining = facts['_expires'] - now
    assert remaining >= datetime.timedelta(days=min_days), 'Certificate expires too soon to switch to'
    facts['remainingDays'] = remaining.days
    facts['keyMode'] = oct((folder / 'privkey.pem').resolve().stat().st_mode & 0o777)
    assert int(facts['keyMode'], 8) & 0o077 == 0, 'Private key must not be readable by group or others'
    return facts


def handshake_failure(out, err):
    """A bounded, secret-free class of an openssl s_client failure for diagnostics."""
    text = out + err
    for marker in ('Connection refused', 'connect:errno', 'No route to host', 'timed out'):
        if marker in text:
            return 'connect: ' + marker
    for line in text.splitlines():
        line = line.strip()
        if line.startswith('verify error:') or line.startswith('Verify return code:'):
            return line[:120]
    return 'handshake failed'


def presented(address, port=8443, ca=None):
    """Facts of the certificate the proxy at `address` presents for HOSTNAME, verified
    against the public roots with SNI and the hostname checked."""
    code, out, err = sh('openssl', 's_client', '-connect', '%s:%d' % (address, port), '-servername', HOSTNAME,
                        '-verify_hostname', HOSTNAME, '-verify_return_error', '-CAfile', ca or CA_BUNDLE, input='', timeout=20)
    assert code == 0 and 'Verify return code: 0 (ok)' in out, \
        'Presented certificate does not verify (%s)' % handshake_failure(out, err)
    blocks = split_pem(out)
    assert blocks, 'No certificate presented'
    return certificate_facts(blocks[0])


def presented_eventually(address, port=8443, timeout=30, probe=None, sleep=time.sleep, clock=time.monotonic):
    """`presented`, allowing a freshly started or reloaded proxy up to `timeout`
    seconds to accept TLS; the last failure is raised unchanged."""
    probe = probe or presented
    deadline = clock() + timeout
    while True:
        try:
            return probe(address, port)
        except AssertionError:
            if clock() >= deadline:
                raise
            sleep(1)


def public(facts):
    return {k: v for k, v in facts.items() if not k.startswith('_')}


# ---- container shape ---------------------------------------------------------------

def address_of(container):
    return container['NetworkSettings']['Networks'][p.NET]['IPAddress']


def is_durable(container, conf_text):
    mounts = {m['Destination']: m for m in container['Mounts']}
    return (all(str(d) in mounts and mounts[str(d)]['Source'] == str(d) and not mounts[str(d)]['RW']
                for d in (LIVE, ARCHIVE))
            and not PINNED & set(mounts)
            and all(conf_text.count(x) == 1 for x in NEW_DIRECTIVES)
            and not any(x in conf_text for x in OLD_DIRECTIVES))


def durable_conf(text):
    for old in OLD_DIRECTIVES:
        assert text.count(old) == 1, 'Unexpected private proxy TLS directives'
    for old, new in zip(OLD_DIRECTIVES, NEW_DIRECTIVES):
        text = text.replace(old, new)
    return text


def durable_payload(container):
    """Create payload for the replacement: the running image by id and every setting of
    the current proxy, except that the two pinned certificate files become read-only
    mounts of the managed lineage directories."""
    payload = copy.deepcopy(container['Config'])
    payload['Image'] = container['Image']
    payload.pop('Hostname', None)
    host = copy.deepcopy(container['HostConfig'])
    assert not host.get('AutoRemove'), 'Auto-removal incompatible with rollback'
    assert not host.get('PortBindings'), 'The private TLS proxy must not publish ports'
    assert not host.get('Binds'), 'Unexpected legacy bind configuration'
    kept = [m for m in host.get('Mounts') or [] if m['Target'] not in PINNED]
    assert len(kept) == len(host.get('Mounts') or []) - 2, 'Expected exactly the two pinned certificate mounts'
    host['Mounts'] = kept + [dict(Type='bind', Source=str(d), Target=str(d), ReadOnly=True) for d in (LIVE, ARCHIVE)]
    payload['HostConfig'] = host
    endpoints = {}
    for name, ep in container['NetworkSettings']['Networks'].items():
        assert not ep.get('IPAMConfig'), 'Static addresses are not used by this proxy'
        aliases = [a for a in ep.get('Aliases') or [] if a not in (container['Id'], container['Id'][:12])]
        # The proxy name is an alias too, so it resolves to the replacement while the
        # previous proxy still runs under that name.
        endpoints[name] = {'Aliases': sorted(set(aliases) | ({HOSTNAME, CONTAINER} if name == p.NET else set()))}
    payload['NetworkingConfig'] = {'EndpointsConfig': endpoints}
    return payload


# ---- route checks --------------------------------------------------------------------

def route_codes(address=None):
    """HTTP status of every production private route through the proxy (by Docker DNS,
    or pinned to one proxy `address`). A TLS or connection failure is code 0."""
    codes = {}
    for port, path in ROUTES:
        url = 'https://%s:%d%s' % (HOSTNAME, port, path)
        pin = ['--resolve', '%s:%d:%s' % (HOSTNAME, port, address)] if address else []
        code, out, _ = sh('docker', 'run', '--rm', '--network', p.NET, CURL, '-sS', '-o', '/dev/null',
                          '-w', '%{http_code}', '--max-time', '15', *pin, url, timeout=60)
        codes['%d%s' % (port, path)] = int(out) if code == 0 and out.strip().isdigit() else 0
    return codes


def wait_ready(address, expected, timeout=60):
    deadline = time.monotonic() + timeout
    while True:
        try:
            codes = route_codes(address)
            if codes == expected:
                return codes
        except Exception:
            codes = None
        if time.monotonic() > deadline:
            raise RuntimeError('Replacement private proxy routes differ: %s' % json.dumps(codes))
        time.sleep(2)


def public_realtime_refusal():
    """The public realtime ticket route reaches Messaging through host nginx, the
    loopback relay and this proxy: Messaging's own 401, not an nginx 502."""
    req = urllib.request.Request('https://%s/v1/realtime/tickets' % HOSTNAME, data=b'{}', method='POST',
                                 headers={'Content-Type': 'application/json'})
    try:
        urllib.request.urlopen(req, timeout=15)
        status = 200
    except urllib.error.HTTPError as e:
        status = e.code
    assert status == 401, 'Public realtime route answered %d' % status
    return status


# ---- renewal hook and monitor --------------------------------------------------------

def host_nginx_uses_lineage():
    """True when host nginx serves this hostname from the managed live/ lineage (then it
    is an activation target too). Paths only; no key material is read."""
    code, out, _ = sh('nginx', '-T', timeout=30)
    if code:
        return False
    return any(line.strip().startswith('ssl_certificate ') and str(LIVE) + '/' in line
               for line in out.splitlines())


def monitor_config(include_host):
    targets = [{'name': 'private-proxy', 'kind': 'container', 'container': CONTAINER, 'network': p.NET, 'port': 8443}]
    if include_host:
        targets.append({'name': 'host-nginx', 'kind': 'host-nginx', 'address': '127.0.0.1', 'port': 443,
                        'pidfile': '/run/nginx.pid'})
    return {'lineage': str(LIVE), 'hostname': HOSTNAME, 'ca': CA_BUNDLE, 'status': str(STATUS),
            'thresholds': list(ALERT_THRESHOLDS), 'stableHandshakes': STABLE_HANDSHAKES,
            'reloadAttempts': RELOAD_ATTEMPTS, 'targets': targets}


def install_monitor_files(include_host):
    """The monitor program (certificate_monitor.py, this release's copy) and its config."""
    p.write_private(MONITOR, (Path(__file__).resolve().parent / 'certificate_monitor.py').read_text())
    p.write_private(MONITOR_CONFIG, json.dumps(monitor_config(include_host), indent=2))


def hook_source():
    return ('#!/bin/sh\n# CF Meet: reload the private TLS proxy after api.cfmeet.com renews (private-tls-durable).\n'
            '# Validates the renewed material and nginx -t first; a failure keeps the running certificate.\n'
            'case " ${RENEWED_DOMAINS:-} " in *" %s "*) ;; *) exit 0 ;; esac\n'
            'exec /usr/bin/python3 %s --reload --config %s\n') % (HOSTNAME, MONITOR, MONITOR_CONFIG)


def install_renewal():
    install_monitor_files(host_nginx_uses_lineage())
    HOOK.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    HOOK.write_text(hook_source()); HOOK.chmod(0o700)
    p.write_private(SYSTEMD / (UNIT + '.service'),
        '[Unit]\nDescription=CF Meet private TLS proxy certificate check\nAfter=docker.service\n[Service]\nType=oneshot\n'
        # ProtectSystem=full keeps /usr, /boot and /etc read-only; host `nginx -t` must
        # still open its logs under /var for the self-repair reload.
        'ExecStart=/usr/bin/python3 %s --check --config %s\nNoNewPrivileges=true\nProtectSystem=full\nProtectHome=read-only\n'
        'PrivateTmp=true\nMemoryMax=64M\nCPUQuota=10%%\n' % (MONITOR, MONITOR_CONFIG))
    p.write_private(SYSTEMD / (UNIT + '.timer'),
        '[Unit]\nDescription=Check the CF Meet private TLS certificate daily\n[Timer]\nOnBootSec=10min\n'
        'OnCalendar=daily\nRandomizedDelaySec=1h\nPersistent=true\n[Install]\nWantedBy=timers.target\n')
    p.run('systemctl', 'daemon-reload')
    p.run('systemctl', 'enable', '--now', UNIT + '.timer')


def certbot_dry_run():
    """Certbot's own renewal simulation for this lineage (Let's Encrypt staging; the live
    certificate is not replaced). The result is reported, not hidden."""
    try:
        code, out, err = sh('certbot', 'renew', '--dry-run', '--cert-name', HOSTNAME, '--no-random-sleep-on-renew', timeout=300)
    except (FileNotFoundError, subprocess.TimeoutExpired) as e:
        return {'exitCode': None, 'summary': [type(e).__name__]}
    tail = (out + err).strip().splitlines()[-3:]
    return {'exitCode': code, 'summary': [x for x in tail if 'token' not in x.lower()]}


# ---- operations ----------------------------------------------------------------------

def certificate_consumers():
    """Every CF Meet consumer of letsencrypt material on this host: containers with a
    letsencrypt bind mount, host nginx certificate paths for this hostname, and certbot's
    renewal method for the lineage. Paths and setting names only."""
    containers = []
    for name in p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines():
        c = p.inspect(name)
        mounts = [m['Destination'] for m in c['Mounts'] if m['Source'].startswith(str(LETSENCRYPT))]
        if mounts:
            containers.append({'name': name, 'running': c['State']['Running'], 'mounts': mounts})
    code, out, _ = sh('nginx', '-T', timeout=30)
    host = sorted({line.strip().rstrip(';') for line in out.splitlines()
                   if line.strip().startswith(('ssl_certificate ', 'ssl_certificate_key ')) and HOSTNAME in line}) if code == 0 else []
    renewal = {}
    conf = LETSENCRYPT / 'renewal' / (HOSTNAME + '.conf')
    if conf.is_file():
        for line in conf.read_text().splitlines():
            key, _, value = line.partition('=')
            if key.strip() in ('authenticator', 'installer', 'renew_hook', 'post_hook', 'pre_hook', 'deploy_hook'):
                renewal[key.strip()] = value.strip()
    timers = sh('systemctl', 'list-timers', '--all', '--no-legend')[1]
    return {'containers': containers, 'hostNginx': host, 'hostNginxUsesManagedLineage': host_nginx_uses_lineage(),
            'certbotRenewal': renewal, 'deployHooks': sorted(x.name for x in HOOK.parent.iterdir()) if HOOK.parent.is_dir() else [],
            'certbotTimer': any('certbot' in line or 'snap.certbot' in line for line in timers.splitlines())}


def inspect_state():
    c = p.inspect(CONTAINER)
    text = CONF.read_text()
    live = public(validate_material(min_days=0))
    shown = public(presented(address_of(c)))
    state = {'privateTls': {
        'durable': is_durable(c, text),
        'mounts': [dict(source=m['Source'], target=m['Destination'], readonly=not m['RW']) for m in c['Mounts']],
        'managed': live, 'presented': shown, 'presentedIsManaged': shown['fingerprint'] == live['fingerprint'],
        'hookInstalled': HOOK.is_file(), 'monitorTimer': sh('systemctl', 'is-enabled', UNIT + '.timer')[1].strip() or 'absent',
        'consumers': certificate_consumers(),
        'monitorConfig': json.loads(MONITOR_CONFIG.read_text()) if MONITOR_CONFIG.is_file() else None,
        'lastCheck': json.loads(STATUS.read_text()) if STATUS.is_file() else None}}
    print(json.dumps(state), flush=True)
    return state


def make_durable(run_id):
    p.identity()
    current = p.inspect(CONTAINER)
    assert current['State']['Running'] and not current['HostConfig']['PortBindings']
    live = validate_material()
    print(json.dumps({'privateTlsManaged': public(live)}), flush=True)
    text = CONF.read_text()
    if is_durable(current, text):
        install_renewal()
        result = finish(run_id, live, 'already-durable')
        return result
    before = route_codes()
    assert all(before.values()), 'A private route is already failing; not switching: %s' % json.dumps(before)
    print(json.dumps({'privateRoutesBefore': before}), flush=True)
    evidence = p.ROOT / 'production-evidence' / ('private-tls-' + run_id)
    p.write_private(evidence / 'nginx.conf.before', text)
    p.write_private(evidence / 'container.before.json', json.dumps(current))
    nxt, previous = CONTAINER + '-next-' + run_id, CONTAINER + '-previous-' + run_id
    payload = durable_payload(current)
    state = {'runId': run_id, 'previous': previous, 'next': nxt, 'conf': str(evidence / 'nginx.conf.before'), 'stage': 'prepared',
             'restart': current['HostConfig']['RestartPolicy'].get('Name') or 'unless-stopped'}
    p.write_private(STATE, json.dumps(state))
    try:
        CONF.write_text(durable_conf(text))  # same inode: the running proxy's mount follows it
        p.docker_api('POST', '/containers/create?name=' + nxt, payload)
        p.docker_api('POST', '/containers/%s/start' % nxt)
        state['stage'] = 'started'; p.write_private(STATE, json.dumps(state))
        p.run('docker', 'exec', nxt, 'nginx', '-t')
        candidate = p.inspect(nxt)
        shown = presented_eventually(address_of(candidate))
        assert shown['fingerprint'] == live['fingerprint'], 'Replacement does not present the managed certificate'
        wait_ready(address_of(candidate), before)
        # Switch: the previous proxy stops gracefully (nginx finishes open requests) and
        # is retained; the replacement takes the name.
        p.run('docker', 'rename', CONTAINER, previous)
        state['stage'] = 'renamed'; p.write_private(STATE, json.dumps(state))
        p.run('docker', 'update', '--restart', 'no', previous)
        p.run('docker', 'stop', '--time', '30', previous)
        p.run('docker', 'rename', nxt, CONTAINER)
        state['stage'] = 'switched'; p.write_private(STATE, json.dumps(state))
        deployed = p.inspect(CONTAINER)
        assert deployed['Id'] == candidate['Id'] and is_durable(deployed, CONF.read_text())
        assert deployed['HostConfig']['RestartPolicy'] == current['HostConfig']['RestartPolicy']
        assert set(deployed['NetworkSettings']['Networks']) == set(current['NetworkSettings']['Networks'])
        assert not deployed['HostConfig']['PortBindings']
        after = wait_ready(None, before)
        print(json.dumps({'privateRoutesAfter': after}), flush=True)
        install_renewal()
        result = finish(run_id, live, 'switched')
    except Exception:
        rollback(state)
        raise
    state['stage'] = 'complete'; p.write_private(STATE, json.dumps(state))
    return result


def finish(run_id, live, how):
    deployed = p.inspect(CONTAINER)
    shown = presented_eventually(address_of(deployed))
    assert shown['fingerprint'] == live['fingerprint']
    relay = presented_eventually('127.0.0.1', 18444)
    assert relay['fingerprint'] == live['fingerprint'], 'Messaging ingress relay does not reach the proxy'
    realtime = public_realtime_refusal()
    # Renewal path: the deploy hook validates, tests and reloads; the presented
    # certificate must still be the managed one afterwards.
    code, out, _ = sh('/usr/bin/env', 'RENEWED_DOMAINS=' + HOSTNAME, 'RENEWED_LINEAGE=' + str(LIVE), str(HOOK), timeout=300)
    hook = json.loads(out.strip().splitlines()[-1]) if code == 0 and out.strip() else None
    assert hook and hook['problem'] is None and hook['presentedMatchesManaged'] and hook['reloaded'], \
        'Renewal hook did not activate the managed certificate on every target'
    assert all(t['stable'] and t['handshakes'] >= STABLE_HANDSHAKES for t in hook['targets'])
    # The daily check right after: no reload needed, every target stable on the lineage.
    code, out, _ = sh('/usr/bin/python3', str(MONITOR), '--check', '--config', str(MONITOR_CONFIG), timeout=180)
    daily = json.loads(out.strip().splitlines()[-1]) if out.strip() else None
    assert daily and daily['presentedMatchesManaged'] and not any(t['reloadAttempts'] for t in daily['targets']), \
        'Daily check does not confirm the active certificate'
    after_hook = wait_ready(None, route_codes())
    dry = certbot_dry_run()
    result = {'operation': 'private-tls-durable', 'runId': run_id, 'result': how,
              'presented': public(shown), 'relayPresented': relay['fingerprint'] == live['fingerprint'],
              'publicRealtimeTickets': realtime, 'renewalHook': hook, 'dailyCheck': daily, 'routesAfterHook': after_hook,
              'certbotDryRun': dry, 'monitorTimer': sh('systemctl', 'is-enabled', UNIT + '.timer')[1].strip(),
              'rollbackContainer': (json.loads(STATE.read_text()).get('previous') if STATE.is_file() else None)}
    print(json.dumps(result), flush=True)
    return result


def rollback(state=None):
    """Restore the previous proxy and configuration exactly; the replacement is kept,
    stopped, for diagnosis."""
    state = state or json.loads(STATE.read_text())
    names = p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    previous, nxt = state['previous'], state['next']
    CONF.write_text(Path(state['conf']).read_text())
    if previous in names:
        failed = CONTAINER + '-failed-' + state['runId']
        if CONTAINER in names:
            p.run('docker', 'rename', CONTAINER, failed)
            p.run('docker', 'update', '--restart', 'no', failed)
            p.run('docker', 'stop', '--time', '30', failed)
        p.run('docker', 'rename', previous, CONTAINER)
        p.run('docker', 'update', '--restart', state.get('restart', 'unless-stopped'), CONTAINER)
        p.run('docker', 'start', CONTAINER)
    elif nxt in names:
        # Not yet switched: the original kept serving; only the candidate goes.
        p.run('docker', 'update', '--restart', 'no', nxt)
        p.run('docker', 'stop', '--time', '30', nxt)
        p.run('docker', 'rename', nxt, CONTAINER + '-failed-' + state['runId'])
    p.run('docker', 'exec', CONTAINER, 'nginx', '-t')
    p.run('docker', 'exec', CONTAINER, 'nginx', '-s', 'reload')
    state['stage'] = 'rolled-back'
    p.write_private(STATE, json.dumps(state))
    print(json.dumps({'privateTlsRollback': state['stage'], 'routes': route_codes()}), flush=True)


def run(operation, sha, run_id):
    if operation == 'private-tls-inspect':
        inspect_state()
    elif operation == 'private-tls-durable':
        make_durable(run_id)
        inspect_state()
    elif operation == 'private-tls-rollback':
        rollback()
        inspect_state()
    else:
        raise RuntimeError('Unknown private TLS operation')
