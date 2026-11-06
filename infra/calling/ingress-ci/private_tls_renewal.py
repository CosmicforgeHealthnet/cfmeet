"""Real-container test of the private TLS proxy renewal path (CI only).

Uses the production proxy image (by digest), a certbot-shaped lineage (numbered archive
files, relative live/ symlinks repointed atomically) under a CI root, and the deployment
code itself (private_tls.durable_payload / durable_conf, the generated renewal hook and
monitor, rollback). It proves:

1. the defect: a proxy with single-file mounts of archive/fullchain1.pem keeps presenting
   the old certificate after a renewal and a reload;
2. the replacement starts beside the running proxy and presents the managed certificate
   before the switch;
3. after a renewal, the deploy hook (validate, nginx -t, graceful reload) makes the proxy
   present the renewed certificate;
4. a renewal whose hook never ran is repaired by the daily check;
5. invalid renewed material is refused and the running certificate kept;
6. rollback restores the previous proxy and configuration exactly.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / 'production'))
import production as p  # noqa: E402
import private_tls as tls  # noqa: E402
from test_private_tls import issue, make_ca  # noqa: E402

PROXY_IMAGE = 'nginx@sha256:0e52bd054126cdc040b1276c0af347aeb02a41b9cfe305d6094767ac30e97949'
PINNED = 'cfmeet-ci-pinned-proxy'
CONTAINERS = (tls.CONTAINER, PINNED)


def sh(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError('%s failed: %s' % (' '.join(args[:3]), r.stderr[-400:]))
    return r.stdout.strip()


def cleanup():
    for name in sh('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines():
        if name in CONTAINERS or name.startswith(tls.CONTAINER + '-'):
            sh('docker', 'rm', '-f', name, check=False)
    sh('docker', 'network', 'rm', p.NET, check=False)


CONF = ('worker_processes 1;\nerror_log /dev/stderr warn;\nevents { worker_connections 64; }\nhttp {\n'
        '  access_log off;\n  ssl_protocols TLSv1.2 TLSv1.3;\n'
        '  ssl_certificate /etc/nginx/tls/fullchain.pem;\n  ssl_certificate_key /etc/nginx/tls/privkey.pem;\n'
        '  server { listen 8443 ssl; server_name api.cfmeet.com; location / { return 200 "ok"; } }\n}\n')


def pinned_container(name, conf, lineage, n):
    """Exactly how cfmeet-core-private-tls.yml created the production proxy."""
    archive = lineage / 'archive' / tls.HOSTNAME
    sh('docker', 'run', '-d', '--name', name, '--restart', 'unless-stopped', '--network', p.NET,
       *(['--network-alias', tls.HOSTNAME] if name == tls.CONTAINER else []),
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % conf,
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/tls/fullchain.pem,readonly' % (archive / ('fullchain%d.pem' % n)),
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/tls/privkey.pem,readonly' % (archive / ('privkey%d.pem' % n)),
       PROXY_IMAGE)


def shown(name):
    deadline = time.monotonic() + 20
    while True:
        try:
            return tls.presented(tls.address_of(p.inspect(name)))['fingerprint']
        except Exception:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.5)


def managed(lineage):
    return tls.validate_material(lineage / 'live' / tls.HOSTNAME, ca=tls.CA_BUNDLE)['fingerprint']


def monitor(mode):
    r = subprocess.run(['python3', str(tls.MONITOR), mode], capture_output=True, text=True)
    return r.returncode, json.loads(r.stdout.strip().splitlines()[-1])


def main():
    cleanup()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        os.chmod(root, 0o755)
        ca = str(make_ca(root))
        lineage = root  # issue() lays out live/ and archive/ beside the CI CA
        # Point the deployment module at the CI lineage, proxy configuration and CA.
        tls.LIVE, tls.ARCHIVE = lineage / 'live' / tls.HOSTNAME, lineage / 'archive' / tls.HOSTNAME
        tls.NEW_DIRECTIVES = ('ssl_certificate %s/fullchain.pem;' % tls.LIVE, 'ssl_certificate_key %s/privkey.pem;' % tls.LIVE)
        tls.WORK = root / 'tls-proxy'; tls.WORK.mkdir()
        tls.CONF, tls.STATE = tls.WORK / 'nginx.conf', tls.WORK / 'durable-rollback.json'
        tls.MONITOR, tls.STATUS = tls.WORK / 'certificate-monitor.py', tls.WORK / 'certificate-status.json'
        tls.CA_BUNDLE = ca
        tls.route_codes = lambda address=None: {}
        # Bind mount sources must be readable by the Docker daemon; keys stay 0600.
        issue(root, 1)
        for path in (lineage, lineage / 'live', lineage / 'archive', tls.LIVE, tls.ARCHIVE):
            path.chmod(0o755)
        tls.CONF.write_text(CONF); tls.CONF.chmod(0o644)
        pinned_conf = root / 'pinned.conf'; pinned_conf.write_text(CONF); pinned_conf.chmod(0o644)
        sh('docker', 'pull', '-q', PROXY_IMAGE)
        sh('docker', 'network', 'create', p.NET)
        try:
            pinned_container(tls.CONTAINER, tls.CONF, lineage, 1)
            pinned_container(PINNED, pinned_conf, lineage, 1)
            first = managed(lineage)
            assert shown(tls.CONTAINER) == first == shown(PINNED)

            # The one-time switch, with the deployment's own payload and configuration.
            current = p.inspect(tls.CONTAINER)
            payload = tls.durable_payload(current)
            state = {'runId': 'ci', 'previous': tls.CONTAINER + '-previous-ci', 'next': tls.CONTAINER + '-next-ci',
                     'conf': str(root / 'conf.before'), 'stage': 'prepared', 'restart': 'unless-stopped'}
            Path(state['conf']).write_text(CONF)
            tls.CONF.write_text(tls.durable_conf(CONF))
            p.docker_api('POST', '/containers/create?name=' + state['next'], payload)
            p.docker_api('POST', '/containers/%s/start' % state['next'])
            sh('docker', 'exec', state['next'], 'nginx', '-t')
            assert shown(state['next']) == first, 'replacement must present the managed certificate before the switch'
            sh('docker', 'rename', tls.CONTAINER, state['previous'])
            sh('docker', 'update', '--restart', 'no', state['previous'])
            sh('docker', 'stop', '--time', '10', state['previous'])
            sh('docker', 'rename', state['next'], tls.CONTAINER)
            assert tls.is_durable(p.inspect(tls.CONTAINER), tls.CONF.read_text())
            print('PRIVATE_TLS_SWITCH: replacement verified before the switch; previous retained stopped')

            # Renewal: certbot writes fullchain2/privkey2 and repoints live/.
            tls.install_renewal = lambda: None
            tls.MONITOR.write_text(tls.monitor_source())
            issue(root, 2)
            second = managed(lineage)
            assert second != first
            sh('docker', 'exec', PINNED, 'nginx', '-s', 'reload')
            time.sleep(1)
            assert shown(PINNED) == first, 'the pinned layout was expected to keep the old certificate'
            print('PRIVATE_TLS_DEFECT_REPRODUCED: single-file archive mounts keep presenting the old certificate after renewal and reload')
            hook = tls.WORK / 'hook'
            hook.write_text(tls.hook_source()); hook.chmod(0o700)
            r = subprocess.run([str(hook)], env=dict(os.environ, RENEWED_DOMAINS='api.cfmeet.com'), capture_output=True, text=True)
            status = json.loads(r.stdout.strip().splitlines()[-1])
            assert r.returncode == 0 and status['reloaded'] and status['presentedMatchesManaged'], status
            assert shown(tls.CONTAINER) == second
            print('PRIVATE_TLS_RENEWAL_HOOK: renewed certificate validated, tested and presented after a graceful reload')

            # A renewal whose hook did not run is repaired by the daily check.
            issue(root, 3)
            third = managed(lineage)
            assert shown(tls.CONTAINER) == second
            code, status = monitor('--check')
            assert code == 0 and status['reloaded'] and shown(tls.CONTAINER) == third, status
            print('PRIVATE_TLS_MONITOR: missed reload detected and repaired; expiry recorded', status['expiresAt'])

            # Invalid renewed material is never loaded.
            live = tls.LIVE
            (live / 'privkey.pem').unlink()
            (live / 'privkey.pem').symlink_to('../../archive/%s/privkey1.pem' % tls.HOSTNAME)
            code, status = monitor('--reload')
            assert code == 1 and status['problem'] == 'key does not match' and not status['reloaded'], status
            assert shown(tls.CONTAINER) == third
            print('PRIVATE_TLS_INVALID_RENEWAL_REFUSED: mismatched key not loaded; running certificate kept')

            # Rollback restores the previous proxy and configuration exactly.
            tls.rollback(state)
            assert tls.CONF.read_text() == CONF
            assert p.inspect(tls.CONTAINER)['Id'] == current['Id'] and shown(tls.CONTAINER) == first
            assert p.inspect(tls.CONTAINER + '-failed-ci')['State']['Running'] is False
            print('PRIVATE_TLS_ROLLBACK: previous proxy and configuration restored')
        finally:
            for name in CONTAINERS:
                subprocess.run(['docker', 'logs', '--tail', '20', name], check=False)
            cleanup()
    print('PRIVATE_TLS_RENEWAL_CI: PASS')


if __name__ == '__main__':
    main()
