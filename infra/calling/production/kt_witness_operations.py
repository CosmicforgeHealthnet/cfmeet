"""Operator-managed KT cosigner. This is not an independent operator or assessment."""
import base64
import json
from pathlib import Path, PurePosixPath
import subprocess
import time
import urllib.request
import urllib.error

import production as p
import meeting_recording_operations as layout
import provider_operations as providers

OPERATION = 'activate-managed-kt-witness'
NAME = 'cfmeet-messaging-kt-witness'
WITNESS = 'cfmeet.operated'
PORT = 8109
NETWORK = 'cfmeet-kt-witness-ingress'
INCLUDE = 'include /etc/nginx/snippets/cfmeet-managed-kt-witness.conf;'
SNIPPET = Path('/etc/nginx/snippets/cfmeet-managed-kt-witness.conf')
PATH = '/witness/v1/add-checkpoint'
TARGET = '/run/secrets/managed-kt-witnesses.json'


def route_text():
    return ('# Operator-managed cosigner; public log signatures authorize submissions.\n'
            'location = ' + PATH + ' {\n'
            '    limit_except POST { deny all; }\n'
            '    client_max_body_size 64k;\n'
            '    access_log off;\n'
            '    proxy_pass http://127.0.0.1:' + str(PORT) + ';\n}\n')


def site_text(before):
    if before.count(INCLUDE) == 1:
        return before
    if INCLUDE in before or before.count(layout.ANCHOR) != 1:
        raise RuntimeError('Unknown TLS site layout; preserved')
    return before.replace(layout.ANCHOR, layout.ANCHOR + '\n    ' + INCLUDE)


def public_key(file):
    result = subprocess.run(['openssl', 'pkey', '-in', str(file), '-pubout', '-outform', 'DER'],
                            capture_output=True, timeout=15)
    prefix = bytes.fromhex('302a300506032b6570032100')
    if result.returncode or not result.stdout.startswith(prefix) or len(result.stdout) != 44:
        raise RuntimeError('Expected an Ed25519 signing key')
    return base64.urlsafe_b64encode(result.stdout[-32:]).decode().rstrip('=')


def ensure_network():
    # Docker does not publish host ports on the internal core network. The witness
    # needs only loopback ingress from host nginx, never access to core services.
    names = p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines()
    if NETWORK not in names:
        p.run('docker', 'network', 'create', '--driver', 'bridge', '--label',
              'cfmeet.kt.operator-managed=true', NETWORK)
    network = json.loads(p.run('docker', 'network', 'inspect', NETWORK))[0]
    if (network.get('Driver') != 'bridge' or network.get('Internal') is not False
            or (network.get('Labels') or {}).get('cfmeet.kt.operator-managed') != 'true'):
        raise RuntimeError('Unknown witness ingress network; preserved')


def ready(expected_key):
    for _ in range(30):
        try:
            with urllib.request.urlopen('http://127.0.0.1:%d/witness/v1/public-key' % PORT, timeout=2) as response:
                data = json.loads(response.read(4096))
            if (data.get('witnessId') != WITNESS or data.get('publicKey') != expected_key
                    or data.get('signatureAlgorithm') != 'Ed25519'):
                raise RuntimeError('Witness listener identity differs; preserved')
            return
        except (OSError, ValueError):
            time.sleep(1)
    raise RuntimeError('Witness loopback listener is not ready; Messaging was not changed')


def log_identity(cr):
    c = p.inspect(cr.SERVICES['messaging']['container'])
    env = p.env_of(c)
    target = env.get('MESSAGING_KT_SIGNING_KEY_FILE')
    sources = []
    for m in c.get('Mounts', []):
        destination = m.get('Destination', m.get('Target'))
        if m.get('Type') != 'bind' or not destination or not target:
            continue
        if destination == target:
            sources.append(Path(m['Source']))
        elif target.startswith(destination.rstrip('/') + '/'):
            relative = PurePosixPath(target).relative_to(destination)
            base = Path(m['Source']).resolve()
            candidate = base.joinpath(*relative.parts).resolve()
            if not candidate.is_relative_to(base):
                raise RuntimeError('Log key resolves outside its configured mount')
            sources.append(candidate)
    if len(sources) != 1 or not env.get('MESSAGING_KT_LOG_ID'):
        raise RuntimeError('Preserve and locate the existing pinned log identity first')
    return c['Image'], env['MESSAGING_KT_LOG_ID'], public_key(sources[0])


def prove_current_head(cr):
    for _ in range(60):
        count = cr.release.db_sql('cfmeet_messaging',
            "SELECT count(*) FROM messaging_transparency_cosignatures c "
            "JOIN messaging_transparency_head h ON c.tree_size=h.tree_size AND c.root_hash=h.root_hash "
            "AND c.head_timestamp_micros=(extract(epoch FROM h.updated_at)*1000000)::bigint "
            "WHERE c.witness_id='cfmeet.operated';")
        if int(count.strip()) > 0:
            cr.emit('managedWitness', dict(cosignaturesVerified=True, independentOperator=False,
                                           witnessId=WITNESS, quorum=1, durableState=True))
            return
        time.sleep(2)
    raise RuntimeError('No verified cosignature of the current head; Messaging activation rolls back')


def activate(cr, sha, run_id, confirm):
    if confirm != 'operator-managed':
        raise RuntimeError('confirm must be operator-managed')
    cr.require_release(sha)
    image, log_id, log_key = log_identity(cr)
    before = layout.SITE.read_text()
    after = site_text(before)
    old = SNIPPET.read_text() if SNIPPET.exists() else None
    if old is not None and old != route_text():
        raise RuntimeError('Unknown witness route; preserved')
    p.run('docker', 'run', '--rm', '--entrypoint', '/bin/test', image,
          '-x', '/app/messaging-kt-witness')
    root = p.ROOT / 'managed-kt-witness'
    state = root / 'state'
    root.mkdir(parents=True, exist_ok=True, mode=0o755)
    state.mkdir(exist_ok=True, mode=0o700)
    p.run('chown', '10001:10001', str(state))
    key = p.SECRETS / 'managed-kt-witness.pk8.pem'
    recovery = p.ROOT / 'key-recovery' / 'managed-kt-witness.pk8.pem'
    if not key.exists():
        if not recovery.exists() and int(cr.release.db_sql('cfmeet_messaging',
                "SELECT count(*) FROM messaging_transparency_witness_state WHERE witness_id='cfmeet.operated';").strip()):
            raise RuntimeError('Witness identity already used; recover its existing key')
        if (state / 'checkpoints.json').exists() and not recovery.exists():
            raise RuntimeError('Witness state exists without its key; never reset or rotate implicitly')
        p.write_private(key, recovery.read_text() if recovery.exists() else
                        p.run('openssl', 'genpkey', '-algorithm', 'ED25519'))
    if recovery.exists() and recovery.read_text() != key.read_text():
        raise RuntimeError('Witness recovery key differs; preserved')
    providers.root_file('managed-kt-witness.pk8.pem')
    if not recovery.exists():
        p.write_private(recovery, key.read_text())
    mounted_key = root / 'key.pem'
    if mounted_key.exists() and public_key(mounted_key) != public_key(key):
        raise RuntimeError('Mounted witness identity differs; never rotate implicitly')
    p.write_private(mounted_key, key.read_text())
    p.run('chown', '10001:10001', str(mounted_key))
    p.run('chmod', '0400', str(mounted_key))
    logs = root / 'logs.json'
    logs.write_text(json.dumps([dict(logId=log_id, publicKey=log_key)]))
    logs.chmod(0o444)
    config = root / 'witnesses.json'
    config.write_text(json.dumps([dict(witnessId=WITNESS, url='https://api.cfmeet.com',
                                       publicKey=public_key(key))]))
    config.chmod(0o444)
    ensure_network()
    existing = NAME in p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    if existing:
        c = p.inspect(NAME)
        if ((c['Config'].get('Labels') or {}).get('cfmeet.kt.operator-managed') != 'true'
                or c['Image'] != image or set(c['NetworkSettings']['Networks']) != {NETWORK}):
            raise RuntimeError('Existing witness differs; preserve it and its monotonic state')
        if not c['State']['Running']:
            p.run('docker', 'start', NAME)
    else:
        p.run('docker', 'run', '-d', '--name', NAME, '--restart', 'unless-stopped',
              '--network', NETWORK, '--user', '10001:10001', '--read-only', '--cap-drop', 'ALL',
              '--security-opt', 'no-new-privileges', '--memory', '128m', '--cpus', '.25', '--pids-limit', '64',
              '--label', 'cfmeet.kt.operator-managed=true', '-p', '127.0.0.1:%d:8099' % PORT,
              '--mount', 'type=bind,src=%s,dst=/state' % state,
              '--mount', 'type=bind,src=%s,dst=/key.pem,readonly' % mounted_key,
              '--mount', 'type=bind,src=%s,dst=/logs.json,readonly' % logs,
              '-e', 'KT_WITNESS_ID=' + WITNESS, '-e', 'KT_WITNESS_SIGNING_KEY_FILE=/key.pem',
              '-e', 'KT_WITNESS_LOGS_FILE=/logs.json', '-e', 'KT_WITNESS_STATE_FILE=/state/checkpoints.json',
              '-e', 'KT_WITNESS_BIND_ADDRESS=0.0.0.0:8099', '--entrypoint', '/app/messaging-kt-witness', image)
    try:
        ready(public_key(key))
        SNIPPET.write_text(route_text())
        layout.SITE.write_text(after)
        p.reload_host_nginx()

        def post():
            prove_current_head(cr)

        cr.apply_activation(OPERATION, sha, run_id, [('messaging', {
            'MESSAGING_KT_WITNESSES_FILE': TARGET, 'MESSAGING_KT_WITNESS_QUORUM': '1',
            'MESSAGING_KT_WITNESS_INTERVAL_SECONDS': '2'}, {'mounts': [(config, TARGET)]})], post=post)
    except Exception:
        layout.SITE.write_text(before)
        if old is None:
            SNIPPET.unlink(missing_ok=True)
        else:
            SNIPPET.write_text(old)
        p.reload_host_nginx()
        if not existing:
            p.run('docker', 'rm', '-f', NAME)
        raise
