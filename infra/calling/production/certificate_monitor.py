"""CF Meet TLS certificate activation and monitoring (installed by private-tls-durable).

One certbot lineage (api.cfmeet.com) is served by more than one nginx: the private
proxy container (cfmeet-core-tls) and, when it reads the managed lineage, host nginx.
For every such target this program:

1. validates the managed lineage: parseable chain with intermediates, not expired, chain
   verifies to the system roots for the hostname as a TLS server, private key matches;
2. measures the certificate actually presented: N consecutive, independent TLS
   connections (SNI, hostname and chain verified) must all present the same certificate;
3. activates it when needed (`--reload`, used by the certbot deploy hook, or `--check`
   finding a stale target): `nginx -t`, a graceful reload, then waiting until every
   accepting worker belongs to the new generation, then step 2 again. Bounded attempts
   with backoff; invalid material or a failing `nginx -t` never reloads, so the last
   valid configuration keeps serving. A stopped proxy container is started again.
4. reports one status: OK, WARN (45 or 30 days left: certbot renews at 30) or ALERT
   (14, 7 days, expired, invalid lineage, or a target not presenting the managed
   certificate), written to the status file and the journal. The scheduled GitHub
   workflow `cfmeet-certificate-monitor.yml` turns it into operator notifications.

Standard library only; no secret is read into this process (openssl reads the key).
"""
import datetime
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BEGIN, END = '-----BEGIN CERTIFICATE-----', '-----END CERTIFICATE-----'
DEFAULT_CONFIG = Path(__file__).with_suffix('.json')


def run(*args, input=None, timeout=60):
    try:
        r = subprocess.run(args, input=input, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return 1, '', 'unavailable'
    return r.returncode, r.stdout, r.stderr


def blocks(text):
    return [BEGIN + b.split(END)[0] + END + '\n' for b in text.split(BEGIN)[1:] if END in b]


def certificate_facts(pem):
    """(sha256 fingerprint, notAfter) of one PEM certificate, or (None, None)."""
    code, out, _ = run('openssl', 'x509', '-noout', '-fingerprint', '-sha256', '-enddate', input=pem)
    fp = end = None
    for line in out.splitlines() if code == 0 else []:
        if line.lower().startswith('sha256 fingerprint='):
            fp = line.split('=', 1)[1].replace(':', '').lower()
        if line.startswith('notAfter='):
            end = datetime.datetime.strptime(line.split('=', 1)[1].replace('  ', ' '),
                                             '%b %d %H:%M:%S %Y %Z').replace(tzinfo=datetime.timezone.utc)
    return fp, end


def managed(cfg, now=None):
    """The lineage's current certificate and the first reason it must not be loaded."""
    now = now or datetime.datetime.now(datetime.timezone.utc)
    live = Path(cfg['lineage'])
    result = {'fingerprint': None, 'expiresAt': None, 'remainingDays': None, 'problem': None}
    try:
        chain = blocks((live / 'fullchain.pem').read_text())
    except OSError:
        chain = []
    if len(chain) < 2:
        result['problem'] = 'chain incomplete'
        return result
    fp, end = certificate_facts(chain[0])
    if not fp or not end:
        result['problem'] = 'certificate unreadable'
        return result
    result.update(fingerprint=fp, expiresAt=end.isoformat(), remainingDays=(end - now).days)
    if end <= now:
        result['problem'] = 'certificate expired'
        return result
    with tempfile.TemporaryDirectory() as tmp:
        leaf, rest = Path(tmp) / 'leaf.pem', Path(tmp) / 'rest.pem'
        leaf.write_text(chain[0]); rest.write_text(''.join(chain[1:]))
        if run('openssl', 'verify', '-CAfile', cfg['ca'], '-untrusted', str(rest), '-purpose', 'sslserver',
               '-verify_hostname', cfg['hostname'], str(leaf))[0]:
            result['problem'] = 'chain does not verify for ' + cfg['hostname']
            return result
    cert_pub = run('openssl', 'x509', '-noout', '-pubkey', input=chain[0])[1].strip()
    code, key_pub, _ = run('openssl', 'pkey', '-pubout', '-in', str(live / 'privkey.pem'))
    if code or not cert_pub or cert_pub != key_pub.strip():
        result['problem'] = 'key does not match'
        return result
    try:
        mode = (live / 'privkey.pem').resolve().stat().st_mode
    except OSError:
        mode = 0o777
    if mode & 0o077:
        result['problem'] = 'private key readable by group or others'
    return result


def handshake(cfg, address, port):
    """Fingerprint presented by one fresh TLS connection, or None when the connection
    or its verification (chain, hostname, SNI) fails."""
    code, out, _ = run('openssl', 's_client', '-connect', '%s:%d' % (address, port), '-servername', cfg['hostname'],
                       '-verify_hostname', cfg['hostname'], '-verify_return_error', '-CAfile', cfg['ca'],
                       input='', timeout=20)
    shown = blocks(out)
    if code or 'Verify return code: 0 (ok)' not in out or not shown:
        return None
    return certificate_facts(shown[0])[0]


def stable(cfg, address, port, count=None, pause=0.2):
    """The fingerprint only if `count` consecutive independent connections all verify
    and present it; otherwise None. Returns (fingerprint, observed)."""
    count = count or cfg.get('stableHandshakes', 5)
    observed = []
    for i in range(count):
        observed.append(handshake(cfg, address, port))
        if i + 1 < count:
            time.sleep(pause)
    first = observed[0]
    return (first if first and all(x == first for x in observed) else None), observed


def accepting_workers(ps_output):
    """PIDs of nginx workers still accepting connections. An old-generation worker
    retitles itself 'nginx: worker process is shutting down' once it stops accepting."""
    pids = set()
    for line in ps_output.splitlines():
        pid, _, title = line.strip().partition(' ')
        if pid.isdigit() and title.strip() == 'nginx: worker process':
            pids.add(pid)
    return pids


class Target:
    def __init__(self, spec):
        self.spec = spec
        self.name = spec['name']
        self.port = spec['port']

    def address(self):
        if self.spec['kind'] == 'container':
            code, out, _ = run('docker', 'inspect', '-f',
                               '{{(index .NetworkSettings.Networks "%s").IPAddress}}' % self.spec['network'],
                               self.spec['container'])
            return out.strip() if code == 0 and out.strip() else None
        return self.spec['address']

    def ensure_running(self):
        """Recovery for a stopped proxy container: start it again (its restart policy
        normally does this). Host nginx is left to systemd."""
        if self.spec['kind'] != 'container':
            return None
        code, out, _ = run('docker', 'inspect', '-f', '{{.State.Running}}', self.spec['container'])
        if code == 0 and out.strip() == 'false':
            run('docker', 'start', self.spec['container'])
            for _ in range(30):
                if self.address():
                    break
                time.sleep(1)
            return 'started'
        return None

    def workers(self):
        if self.spec['kind'] == 'container':
            code, out, _ = run('docker', 'exec', self.spec['container'], 'ps', '-o', 'pid,args')
        else:
            try:
                master = Path(self.spec.get('pidfile', '/run/nginx.pid')).read_text().strip()
            except OSError:
                return None
            code, out, _ = run('ps', '--ppid', master, '-o', 'pid=,args=')
        return accepting_workers(out) if code == 0 else None

    def test_config(self):
        if self.spec['kind'] == 'container':
            return run('docker', 'exec', self.spec['container'], 'nginx', '-t')[0] == 0
        return run('nginx', '-t')[0] == 0

    def signal_reload(self):
        if self.spec['kind'] == 'container':
            return run('docker', 'exec', self.spec['container'], 'nginx', '-s', 'reload')[0] == 0
        return run('systemctl', 'reload', 'nginx')[0] == 0

    def reload(self, timeout=30):
        """nginx -t, a graceful reload, then wait until every accepting worker is new."""
        before = self.workers()
        if before is None or not self.test_config() or not self.signal_reload():
            return False
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            now = self.workers()
            if now and not now & before:
                return True
            time.sleep(0.2)
        return False


def activate(cfg, target, fingerprint, sleep=time.sleep):
    """Bounded attempts to make `target` present `fingerprint` stably. Returns
    (presented fingerprint or None, attempts, observed)."""
    attempts, observed = 0, []
    for attempt in range(cfg.get('reloadAttempts', 3)):
        attempts = attempt + 1
        if target.reload():
            address = target.address()
            shown, observed = stable(cfg, address, target.port) if address else (None, [])
            if shown == fingerprint:
                return shown, attempts, observed
        sleep(2 ** attempt)
    address = target.address()
    shown, observed = stable(cfg, address, target.port) if address else (None, [])
    return shown, attempts, observed


def band(remaining, thresholds):
    """The alert band: 'EXPIRED', the smallest threshold not above the days left, or None."""
    if remaining is None or remaining < 0:
        return 'EXPIRED'
    hits = [t for t in thresholds if remaining <= t]
    return min(hits) if hits else None


def classify(lineage, targets, current_band, thresholds):
    alert_bands = set(sorted(thresholds)[:2]) | {'EXPIRED'}
    if lineage['problem'] or not all(t['matchesManaged'] for t in targets) or current_band in alert_bands:
        return 'ALERT'
    return 'WARN' if current_band is not None else 'OK'


def check(cfg, mode='--check', now=None):
    lineage = managed(cfg, now)
    fp = lineage['fingerprint']
    targets = []
    for spec in cfg['targets']:
        target = Target(spec)
        recovered = target.ensure_running()
        address = target.address()
        shown, observed = stable(cfg, address, target.port) if address else (None, [])
        attempts = 0
        if lineage['problem'] is None and fp and (mode == '--reload' or shown != fp):
            shown, attempts, observed = activate(cfg, target, fp)
        targets.append({'name': target.name, 'recovered': recovered, 'reloadAttempts': attempts,
                        'reloaded': attempts > 0 and shown == fp, 'presentedFingerprint': shown,
                        'handshakes': len(observed), 'stable': shown is not None,
                        'matchesManaged': fp is not None and shown == fp})
    current_band = band(lineage['remainingDays'] if lineage['problem'] != 'certificate expired' else -1,
                        cfg['thresholds'])
    status = classify(lineage, targets, current_band, cfg['thresholds'])
    checked = (now or datetime.datetime.now(datetime.timezone.utc)).isoformat()
    return {'checkedAt': checked, 'mode': mode.strip('-'), 'hostname': cfg['hostname'],
            'problem': lineage['problem'], 'managedFingerprint': fp, 'expiresAt': lineage['expiresAt'],
            'remainingDays': lineage['remainingDays'], 'band': current_band, 'thresholds': cfg['thresholds'],
            'targets': targets, 'presentedMatchesManaged': bool(targets) and all(t['matchesManaged'] for t in targets),
            'reloaded': bool(targets) and all(t['reloaded'] for t in targets), 'status': status}


def main(argv):
    mode = '--reload' if '--reload' in argv else '--check'
    config = Path(argv[argv.index('--config') + 1]) if '--config' in argv else DEFAULT_CONFIG
    cfg = json.loads(config.read_text())
    status = check(cfg, mode)
    path = Path(cfg['status'])
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(status)); os.chmod(tmp, 0o600); tmp.replace(path)
    level = {'OK': 'info', 'WARN': 'warning', 'ALERT': 'err'}[status['status']]
    run('logger', '-t', 'cfmeet-certificate', '-p', 'user.' + level, json.dumps(status))
    print(json.dumps(status))
    return 1 if status['status'] == 'ALERT' else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
