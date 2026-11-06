"""Real-container test of the storage relay (CI only).

Runs storage_relay.ensure_relay / wait_for_relay / store_upstream themselves against the
production proxy image and an object-store stand-in named like the production store. It
proves that the old route shape (the store's Docker address written into host nginx)
breaks when the store is recreated and another container takes its address, while the
relay route keeps reaching the store, by name, with no configuration change; that the
relay is published on loopback only; and that rollback restores the previous relay.
"""
import json
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / 'production'))
import production as p  # noqa: E402
import storage_relay as relay  # noqa: E402

PROXY_IMAGE = 'nginx@sha256:0e52bd054126cdc040b1276c0af347aeb02a41b9cfe305d6094767ac30e97949'
STORE = relay.storage.NAME
HOLDER = 'cfmeet-ci-address-holder'
CONTAINERS = (STORE, HOLDER, 'cfmeet-core-tls', relay.RELAY)
# The stand-in answers MinIO's liveness path and identifies its generation.
STUB = ('server { listen 9000; location = /minio/health/live { return 200 "live"; } '
        'location / { add_header X-Generation "%s" always; return 403; } }\n')


def sh(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError('%s failed: %s' % (' '.join(args[:3]), r.stderr[-400:]))
    return r.stdout.strip()


def cleanup():
    for name in sh('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines():
        if name in CONTAINERS or name.startswith(relay.RELAY + '-'):
            sh('docker', 'rm', '-f', name, check=False)
    for net in (relay.RELAY_NET, p.NET):
        sh('docker', 'network', 'rm', net, check=False)


def store(folder, generation):
    conf = folder / ('store-%s.conf' % generation)
    conf.write_text(STUB % generation); conf.chmod(0o644)
    sh('docker', 'run', '-d', '--name', STORE, '--network', p.NET,
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/conf.d/default.conf,readonly' % conf, PROXY_IMAGE)
    return p.inspect(STORE)['NetworkSettings']['Networks'][p.NET]['IPAddress']


def generation(base):
    try:
        urllib.request.urlopen('http://%s/object' % base, timeout=5)
    except urllib.error.HTTPError as e:
        return e.headers.get('X-Generation')
    except (urllib.error.URLError, OSError):
        return None


def main():
    cleanup()
    with tempfile.TemporaryDirectory() as tmp:
        folder = Path(tmp)
        relay.WORK = folder / 'storage'
        if subprocess.run(['docker', 'image', 'inspect', PROXY_IMAGE], capture_output=True).returncode:
            sh('docker', 'pull', '-q', PROXY_IMAGE)  # pinned by digest; pulled only when absent
        sh('docker', 'network', 'create', p.NET)
        try:
            sh('docker', 'run', '-d', '--name', 'cfmeet-core-tls', '--network', p.NET, PROXY_IMAGE)
            first_ip = store(folder, 'first')
            assert relay.store_upstream() == first_ip + ':9000', 'before the relay, routes use the private address'
            state = relay.ensure_relay('ci')
            relay.wait_for_relay()
            assert relay.store_upstream() == relay.ADDRESS
            assert relay.ensure_relay('ci-again')['relay'] == 'reused'
            bindings = p.inspect(relay.RELAY)['HostConfig']['PortBindings']
            assert bindings == {'9000/tcp': [{'HostIp': '127.0.0.1', 'HostPort': str(relay.RELAY_PORT)}]}, bindings
            assert generation(relay.ADDRESS) == generation(first_ip + ':9000') == 'first'
            print('STORAGE_RELAY: reaches the store by name on 127.0.0.1:%d only' % relay.RELAY_PORT)

            # Recreate the store; another container may take its previous address.
            sh('docker', 'rm', '-f', STORE)
            sh('docker', 'run', '-d', '--name', HOLDER, '--network', p.NET, PROXY_IMAGE)
            second_ip = store(folder, 'second')
            assert second_ip != first_ip
            assert generation(first_ip + ':9000') != 'second', 'a pinned address must not reach the recreated store'
            print('STORAGE_PINNED_ADDRESS_DEFECT_REPRODUCED: the old address no longer reaches the store')
            deadline = time.monotonic() + 30
            while generation(relay.ADDRESS) != 'second':
                assert time.monotonic() < deadline, 'relay did not follow the recreated store'
                time.sleep(1)
            relay.wait_for_relay()
            print('STORAGE_RELAY_SURVIVES_RECREATION: same relay, same host route, new store address')

            # A changed relay is replaced and the previous one kept; rollback restores it.
            relay.relay_conf().write_text('# outdated\n')
            replaced = relay.ensure_relay('ci-replace')
            assert replaced['relay'] == 'created' and replaced['previous']
            relay.restore_relay(replaced)
            assert p.inspect(relay.RELAY)['State']['Running'] and relay.relay_conf().read_text() == '# outdated\n'
            print('STORAGE_RELAY_ROLLBACK: previous relay and configuration restored')
            del state
        finally:
            for name in CONTAINERS:
                subprocess.run(['docker', 'logs', '--tail', '10', name], check=False)
            cleanup()
    print('STORAGE_RELAY_CI: PASS')


if __name__ == '__main__':
    main()
