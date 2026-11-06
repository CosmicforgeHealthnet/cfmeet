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
ALERT_REMAINING_DAYS = 21
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


def presented(address, port=8443, ca=None):
    """Facts of the certificate the proxy at `address` presents for HOSTNAME, verified
    against the public roots with SNI and the hostname checked."""
    code, out, _ = sh('openssl', 's_client', '-connect', '%s:%d' % (address, port), '-servername', HOSTNAME,
                      '-verify_hostname', HOSTNAME, '-verify_return_error', '-CAfile', ca or CA_BUNDLE, input='', timeout=20)
    assert code == 0 and 'Verify return code: 0 (ok)' in out, 'Presented certificate does not verify'
    blocks = split_pem(out)
    assert blocks, 'No certificate presented'
    return certificate_facts(blocks[0])


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

MONITOR_SOURCE = r'''"""CF Meet private TLS proxy certificate check (installed by private-tls-durable).

--check : status OK only when the managed %(name)s certificate verifies, matches its key,
          has at least %(alert)d days left and is the one the proxy presents; when the
          proxy presents another certificate, reload once (after nginx -t) and re-check.
--reload: certbot deploy hook: the same validation, then nginx -t and a graceful reload.
A failed validation never reloads, so the running certificate is kept.
"""
import datetime, json, subprocess, sys, tempfile, time
from pathlib import Path
LIVE = Path(%(live)r)
STATUS = Path(%(status)r)
NAME = %(name)r
CONTAINER = %(container)r
NET = %(net)r
CA = %(ca)r
ALERT_DAYS = %(alert)d
BEGIN, END = '-----BEGIN CERTIFICATE-----', '-----END CERTIFICATE-----'

def sh(*a, input=None):
    r = subprocess.run(a, input=input, capture_output=True, text=True, timeout=60)
    return r.returncode, r.stdout

def blocks(text):
    return [BEGIN + b.split(END)[0] + END + '\n' for b in text.split(BEGIN)[1:] if END in b]

def facts(pem):
    code, out = sh('openssl', 'x509', '-noout', '-fingerprint', '-sha256', '-enddate', input=pem)
    fp = end = None
    for line in out.splitlines() if code == 0 else []:
        if line.lower().startswith('sha256 fingerprint='): fp = line.split('=', 1)[1].replace(':', '').lower()
        if line.startswith('notAfter='):
            end = datetime.datetime.strptime(line.split('=', 1)[1].replace('  ', ' '), '%%b %%d %%H:%%M:%%S %%Y %%Z').replace(tzinfo=datetime.timezone.utc)
    return fp, end

def managed():
    chain = blocks((LIVE / 'fullchain.pem').read_text())
    if len(chain) < 2: return None, None, 'chain incomplete'
    with tempfile.TemporaryDirectory() as tmp:
        leaf, rest = Path(tmp) / 'leaf.pem', Path(tmp) / 'rest.pem'
        leaf.write_text(chain[0]); rest.write_text(''.join(chain[1:]))
        if sh('openssl', 'verify', '-CAfile', CA, '-untrusted', str(rest), '-purpose', 'sslserver', '-verify_hostname', NAME, str(leaf))[0]:
            return None, None, 'chain does not verify'
    cert_pub = sh('openssl', 'x509', '-noout', '-pubkey', input=chain[0])[1].strip()
    code, key_pub = sh('openssl', 'pkey', '-pubout', '-in', str(LIVE / 'privkey.pem'))
    if code or not cert_pub or cert_pub != key_pub.strip(): return None, None, 'key does not match'
    fp, end = facts(chain[0])
    return fp, end, None

def presented():
    code, ip = sh('docker', 'inspect', '-f', '{{(index .NetworkSettings.Networks "' + NET + '").IPAddress}}', CONTAINER)
    if code or not ip.strip(): return None
    code, out = sh('openssl', 's_client', '-connect', ip.strip() + ':8443', '-servername', NAME, '-verify_hostname', NAME,
                   '-verify_return_error', '-CAfile', CA, input='')
    shown = blocks(out)
    if code or 'Verify return code: 0 (ok)' not in out or not shown: return None
    return facts(shown[0])[0]

def reload():
    return sh('docker', 'exec', CONTAINER, 'nginx', '-t')[0] == 0 and sh('docker', 'exec', CONTAINER, 'nginx', '-s', 'reload')[0] == 0

def main(mode):
    fp, end, problem = managed()
    current, reloaded = presented(), False
    if problem is None and (mode == '--reload' or current != fp):
        reloaded = reload()
        for _ in range(15):
            current = presented()
            if current == fp: break
            time.sleep(1)
    now = datetime.datetime.now(datetime.timezone.utc)
    left = (end - now).days if end else -1
    ok = problem is None and current == fp and left >= ALERT_DAYS
    status = dict(checkedAt=now.isoformat(), mode=mode.strip('-'), problem=problem, managedFingerprint=fp,
                  presentedMatchesManaged=fp is not None and current == fp, expiresAt=end.isoformat() if end else None,
                  remainingDays=left, reloaded=reloaded, status='OK' if ok else 'ALERT')
    STATUS.write_text(json.dumps(status)); STATUS.chmod(0o600)
    subprocess.run(['logger', '-t', CONTAINER, '-p', 'user.' + ('info' if ok else 'err'), json.dumps(status)])
    print(json.dumps(status))
    return 0 if ok else 1

if __name__ == '__main__':
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else '--check'))
'''


def monitor_source():
    return MONITOR_SOURCE % dict(live=str(LIVE), status=str(STATUS), name=HOSTNAME, container=CONTAINER,
                                 net=p.NET, alert=ALERT_REMAINING_DAYS, ca=CA_BUNDLE)


def hook_source():
    return ('#!/bin/sh\n# CF Meet: reload the private TLS proxy after api.cfmeet.com renews (private-tls-durable).\n'
            '# Validates the renewed material and nginx -t first; a failure keeps the running certificate.\n'
            'case " ${RENEWED_DOMAINS:-} " in *" %s "*) ;; *) exit 0 ;; esac\n'
            'exec /usr/bin/python3 %s --reload\n') % (HOSTNAME, MONITOR)


def install_renewal():
    p.write_private(MONITOR, monitor_source())
    HOOK.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    HOOK.write_text(hook_source()); HOOK.chmod(0o700)
    p.write_private(SYSTEMD / (UNIT + '.service'),
        '[Unit]\nDescription=CF Meet private TLS proxy certificate check\nAfter=docker.service\n[Service]\nType=oneshot\n'
        'ExecStart=/usr/bin/python3 %s --check\nNoNewPrivileges=true\nProtectSystem=strict\nProtectHome=true\n'
        'ReadWritePaths=%s\nPrivateTmp=true\nMemoryMax=64M\nCPUQuota=10%%\n' % (MONITOR, WORK))
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
        shown = presented(address_of(candidate))
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
    shown = presented(address_of(deployed))
    assert shown['fingerprint'] == live['fingerprint']
    relay = presented('127.0.0.1', 18444)
    assert relay['fingerprint'] == live['fingerprint'], 'Messaging ingress relay does not reach the proxy'
    realtime = public_realtime_refusal()
    # Renewal path: the deploy hook validates, tests and reloads; the presented
    # certificate must still be the managed one afterwards.
    code, out, _ = sh('/usr/bin/env', 'RENEWED_DOMAINS=' + HOSTNAME, 'RENEWED_LINEAGE=' + str(LIVE), str(HOOK), timeout=120)
    hook = json.loads(out.strip().splitlines()[-1]) if code == 0 and out.strip() else None
    assert hook and hook['presentedMatchesManaged'] and hook['reloaded'], 'Renewal hook did not reload the managed certificate'
    after_hook = wait_ready(None, route_codes())
    dry = certbot_dry_run()
    result = {'operation': 'private-tls-durable', 'runId': run_id, 'result': how,
              'presented': public(shown), 'relayPresented': relay['fingerprint'] == live['fingerprint'],
              'publicRealtimeTickets': realtime, 'renewalHook': hook, 'routesAfterHook': after_hook,
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
