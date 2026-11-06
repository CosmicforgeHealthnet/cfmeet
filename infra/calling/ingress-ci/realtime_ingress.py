"""Real-configuration test of the public Messaging realtime ingress (CI only).

Runs the deployment code itself (messaging_runtime.nginx_config, ensure_relay, verifies,
ingress_probe and production.reload_host_nginx) against:
- Ubuntu 24.04's nginx 1.24 / OpenSSL 3.0.13 (the production host build) as host nginx;
- the production private proxy's nginx image (by digest) with its 8444 server block;
- a certificate chain shaped like Let's Encrypt Generation Y (leaf, intermediate, a root
  cross-signed by a second root, and that root cross-signed by a third), whose trust
  anchor sits two intermediates above the leaf.

It proves: nginx's default verify depth reproduces the production 502; the rendered
configuration reaches the upstream with verification, SNI and hostname checks on; a valid
ticket upgrades to a working WebSocket; refusals pass through unchanged; a reload is
complete before the next request; and the route survives the private proxy changing its
Docker address without touching host nginx or the relay.
"""
import base64
import http.client
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / 'production'))
import production as p  # noqa: E402
import messaging_runtime as runtime  # noqa: E402

# The production private proxy's image (`docker image inspect` RepoDigests, 2026-11-05).
PROXY_IMAGE = 'nginx@sha256:0e52bd054126cdc040b1276c0af347aeb02a41b9cfe305d6094767ac30e97949'
STUB_IMAGE = 'python:3.12-slim'
FRONT = 18480
CONTAINERS = ('cfmeet-core-tls', 'cfmeet-messaging', runtime.RELAY, 'cfmeet-ci-address-holder')
NETWORKS = (runtime.RELAY_NET, p.NET)
VALID_TOKEN = 'ci-valid-client-token'


def sh(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError('%s failed: %s' % (' '.join(args[:3]), r.stderr[-400:]))
    return r.stdout.strip()


def cleanup():
    for name in CONTAINERS:
        sh('docker', 'rm', '-f', name, check=False)
    for net in NETWORKS:
        sh('docker', 'network', 'rm', net, check=False)


# ---- certificate chain shaped like Let's Encrypt Generation Y -------------------------

def _cfg(folder, name, ca, san=None):
    ext = ['basicConstraints=critical,CA:%s' % ('true' if ca else 'false'),
           'keyUsage=critical,' + ('keyCertSign,cRLSign' if ca else 'digitalSignature'),
           'subjectKeyIdentifier=hash']
    if not ca:
        ext += ['extendedKeyUsage=serverAuth', 'subjectAltName=DNS:' + san]
    path = folder / (name + '.cnf')
    path.write_text('[req]\ndistinguished_name=dn\nprompt=no\n[dn]\n[v3]\n' + '\n'.join(ext) + '\n')
    return path


def key(folder, name):
    out = folder / (name + '.key')
    sh('openssl', 'genpkey', '-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256', '-out', str(out))
    return out


def self_signed(folder, name, subject, private):
    out = folder / (name + '.pem')
    sh('openssl', 'req', '-x509', '-new', '-key', str(private), '-subj', '/CN=' + subject, '-days', '2',
       '-config', str(_cfg(folder, name, True)), '-extensions', 'v3', '-out', str(out))
    return out


def signed(folder, name, subject, private, ca_cert, ca_key, ca=True, san=None):
    csr, out = folder / (name + '.csr'), folder / (name + '.pem')
    sh('openssl', 'req', '-new', '-key', str(private), '-subj', '/CN=' + subject, '-out', str(csr))
    sh('openssl', 'x509', '-req', '-in', str(csr), '-CA', str(ca_cert), '-CAkey', str(ca_key), '-set_serial',
       str(secrets.randbits(63)), '-days', '2', '-extfile', str(_cfg(folder, name, ca, san)), '-extensions', 'v3',
       '-out', str(out))
    return out


def pki(folder):
    folder.mkdir()
    x1k, x2k, yek, ye1k, leafk = (key(folder, n) for n in ('x1', 'x2', 'ye', 'ye1', 'leaf'))
    x1 = self_signed(folder, 'x1', 'CI Root X1', x1k)
    x2 = self_signed(folder, 'x2', 'CI Root X2', x2k)
    x2_cross = signed(folder, 'x2-cross', 'CI Root X2', x2k, x1, x1k)
    ye_cross = signed(folder, 'ye-cross', 'CI Root YE', yek, x2, x2k)
    ye1 = signed(folder, 'ye1', 'CI YE1', ye1k, ye_cross, yek)
    leaf = signed(folder, 'leaf', 'api.cfmeet.com', leafk, ye1, ye1k, ca=False, san='api.cfmeet.com')
    chain = folder / 'fullchain.pem'
    chain.write_text(''.join(c.read_text() for c in (leaf, ye1, ye_cross, x2_cross)))
    trust = folder / 'trust.pem'
    trust.write_text(x1.read_text() + x2.read_text())
    other = folder / 'unrelated.pem'
    other.write_text(self_signed(folder, 'unrelated', 'CI Unrelated Root', key(folder, 'unrelated')).read_text())
    for path in folder.iterdir():
        path.chmod(0o644)
    return {'chain': chain, 'key': leafk, 'trust': trust, 'unrelated': other}


# ---- the private side ----------------------------------------------------------------

PROXY_CONF = '''worker_processes 1;
error_log /dev/stderr warn;
events { worker_connections 1024; }
http {
  resolver 127.0.0.11 valid=10s ipv6=off;
  access_log off;
  ssl_protocols TLSv1.2 TLSv1.3;
  ssl_certificate /etc/nginx/tls/fullchain.pem;
  ssl_certificate_key /etc/nginx/tls/privkey.pem;
  server {
    listen 8444 ssl;
    server_name api.cfmeet.com;
    location / {
      set $peer cfmeet-messaging:8097;
      proxy_pass http://$peer$request_uri;
      proxy_http_version 1.1;
      proxy_set_header Host $host;
      proxy_set_header X-Forwarded-Proto https;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection "upgrade";
      proxy_read_timeout 3600;
    }
  }
}
'''


def start_proxy(work, certs):
    sh('docker', 'run', '-d', '--name', 'cfmeet-core-tls', '--network', p.NET, '--network-alias', 'api.cfmeet.com',
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % (work / 'proxy.conf'),
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/tls/fullchain.pem,readonly' % certs['chain'],
       '--mount', 'type=bind,src=%s,dst=/etc/nginx/tls/privkey.pem,readonly' % certs['key'], PROXY_IMAGE)


def proxy_address():
    return p.inspect('cfmeet-core-tls')['NetworkSettings']['Networks'][p.NET]['IPAddress']


# ---- host nginx (the production build, unprivileged prefix) ----------------------------

class HostNginx:
    def __init__(self, prefix):
        self.prefix = prefix
        self.conf, self.error = prefix / 'nginx.conf', prefix / 'error.log'
        self.snippet = prefix / 'cfmeet-messaging.conf'
        temp = ' '.join('%s_temp_path %s;' % (k, prefix / k) for k in ('client_body', 'proxy', 'fastcgi', 'uwsgi', 'scgi'))
        self.conf.write_text('pid %s; error_log %s info; events {} http { access_log off; %s '
                             'server { listen 127.0.0.1:%d; server_name api.cfmeet.com; include %s; } }\n'
                             % (prefix / 'nginx.pid', self.error, temp, FRONT, self.snippet))
        self.base = ('nginx', '-p', str(prefix), '-e', str(self.error), '-c', str(self.conf))

    def start(self, snippet):
        self.snippet.write_text(snippet)
        sh(*self.base, '-t')
        sh(*self.base)
        time.sleep(0.5)

    def apply(self, snippet):
        """Production's reload helper, pointed at this instance."""
        self.snippet.write_text(snippet)
        return p.reload_host_nginx(pidfile=self.prefix / 'nginx.pid', reload=self.base + ('-s', 'reload'),
                                   test=self.base + ('-t',))

    def master(self):
        return (self.prefix / 'nginx.pid').read_text().strip()

    def stop(self):
        subprocess.run(self.base + ('-s', 'quit'), capture_output=True)


def local_request(method, path, headers=None, body=None):
    connection = http.client.HTTPConnection('127.0.0.1', FRONT, timeout=15)
    try:
        connection.request(method, path, body=body, headers=dict(headers or {}, Host='api.cfmeet.com'))
        response = connection.getresponse()
        return response.status, runtime.joined_headers(response), response.read(65536)
    finally:
        connection.close()


def websocket_echo(ticket):
    """A real WebSocket session through the whole path: 101, then one framed round trip."""
    key = base64.b64encode(secrets.token_bytes(16)).decode()
    with socket.create_connection(('127.0.0.1', FRONT), timeout=15) as s:
        s.sendall(('GET /v1/realtime HTTP/1.1\r\nHost: api.cfmeet.com\r\nUpgrade: websocket\r\n'
                   'Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: %s\r\n'
                   'Sec-WebSocket-Protocol: cfmeet.messaging.v1, cfmeet.ticket.%s\r\n\r\n' % (key, ticket)).encode())
        head = b''
        while b'\r\n\r\n' not in head:
            chunk = s.recv(1)
            assert chunk, 'connection closed during the upgrade'
            head += chunk
        lines = head.decode().split('\r\n')
        assert lines[0].split()[1] == '101', 'WebSocket upgrade refused: ' + lines[0]
        headers = {l.split(':', 1)[0].lower(): l.split(':', 1)[1].strip() for l in lines[1:] if ':' in l}
        import hashlib
        expected = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        assert headers.get('sec-websocket-accept') == expected and headers.get('sec-websocket-protocol') == 'cfmeet.messaging.v1'
        payload = ('ci-' + secrets.token_hex(8)).encode()
        mask = secrets.token_bytes(4)
        s.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
        frame = b''
        while len(frame) < 2 + len(payload):
            chunk = s.recv(64)
            assert chunk, 'connection closed before the echo'
            frame += chunk
        assert frame[0] == 0x81 and frame[2:2 + frame[1]] == payload, 'WebSocket frame did not round-trip'
    return 101


def wait_status(expected, path='/v1/realtime/tickets', timeout=30):
    deadline = time.monotonic() + timeout
    while True:
        status = local_request('POST', path, {'Content-Type': 'application/json'}, b'{}')[0]
        if status == expected or time.monotonic() > deadline:
            return status
        time.sleep(0.5)


def main():
    evidence = {}
    work = Path(tempfile.mkdtemp(prefix='messaging-ingress-ci-'))
    work.chmod(0o755)
    runtime.WORK = work / 'messaging'
    certs = pki(work / 'pki')
    (work / 'proxy.conf').write_text(PROXY_CONF)
    (work / 'proxy.conf').chmod(0o644)
    (work / 'host-nginx').mkdir()
    host = HostNginx(work / 'host-nginx')
    cleanup()
    try:
        for image in (PROXY_IMAGE, STUB_IMAGE):
            sh('docker', 'pull', '-q', image)
        sh('docker', 'network', 'create', '--internal', p.NET)
        sh('docker', 'run', '-d', '--name', 'cfmeet-messaging', '--network', p.NET,
           '--mount', 'type=bind,src=%s,dst=/stub.py,readonly' % (HERE / 'stub_messaging.py'), STUB_IMAGE, 'python', '/stub.py')
        start_proxy(work, certs)

        relay = runtime.ensure_relay('ci')
        runtime.wait_for_relay()
        relay_id = p.inspect(runtime.RELAY)['Id']
        assert relay['relay'] == 'created'
        assert sh('docker', 'port', runtime.RELAY) == '8444/tcp -> 127.0.0.1:%d' % runtime.RELAY_PORT
        assert [m['Destination'] for m in p.inspect(runtime.RELAY)['Mounts']] == ['/etc/nginx/nginx.conf'], \
            'The relay must hold no certificate or key'
        assert runtime.ensure_relay('ci-again')['relay'] == 'reused'
        assert p.inspect(runtime.RELAY)['Id'] == relay_id
        evidence['relay'] = 'loopback-only, confined, reused when unchanged'

        # The chain has the production shape: OpenSSL needs depth 2, like the live chain.
        assert runtime.verifies(certs['trust']) and not runtime.verifies(certs['trust'], depth=1)
        assert not runtime.verifies(certs['unrelated'])
        evidence['chainNeedsDepth'] = 2

        good = runtime.nginx_config('172.31.255.1', certs['trust'])
        default_depth = re.sub(r'\n\s*proxy_ssl_verify_depth \d+;', '', good)
        assert 'proxy_ssl_verify_depth' not in default_depth

        # 1. The production failure: the previously live directives (nginx default depth).
        host.start(default_depth)
        status = local_request('POST', '/v1/realtime/tickets', {'Content-Type': 'application/json'}, b'{}')[0]
        assert status == 502, status
        assert 'upstream SSL certificate verify error' in host.error.read_text()
        evidence['defaultDepthReproducesProduction502'] = status

        # 2. The rendered configuration, applied with the production reload helper.
        host.apply(good)
        evidence['probe'] = runtime.ingress_probe(request=local_request)

        # 3. A valid ticket upgrades to a working WebSocket; a replay is refused.
        status, _, raw = local_request('POST', '/v1/realtime/tickets',
                                       {'Content-Type': 'application/json', 'Authorization': 'CF-Client-Token ' + VALID_TOKEN}, b'{}')
        assert status == 201, status
        ticket = json.loads(raw)['ticket']
        evidence['validTicketIssued'] = status
        evidence['websocketUpgradeAndFrameRoundTrip'] = websocket_echo(ticket)
        replay = local_request('GET', '/v1/realtime', {'Upgrade': 'websocket', 'Connection': 'Upgrade',
            'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': base64.b64encode(secrets.token_bytes(16)).decode(),
            'Sec-WebSocket-Protocol': 'cfmeet.messaging.v1, cfmeet.ticket.' + ticket})
        evidence['ticketReplayRefused'] = runtime.messaging_refusal(replay, 401, 'INVALID_TICKET')

        # 4. Every reload is complete before the next request (the masking race).
        for _ in range(3):
            host.apply(default_depth)
            assert local_request('POST', '/v1/realtime/tickets', {}, b'{}')[0] == 502
            host.apply(good)
            assert local_request('POST', '/v1/realtime/tickets', {}, b'{}')[0] == 401
        evidence['reloadCompleteBeforeNextRequest'] = 'PASS x3 each way'

        # 5. Verification stays on: a wrong name or an untrusted root is refused.
        for label, variant in (('wrongName', good.replace('proxy_ssl_name api.cfmeet.com;', 'proxy_ssl_name other.cfmeet.invalid;')),
                               ('untrustedRoot', good.replace(str(certs['trust']), str(certs['unrelated'])))):
            host.apply(variant)
            status = local_request('POST', '/v1/realtime/tickets', {}, b'{}')[0]
            assert status == 502, (label, status)
            evidence[label + 'Refused'] = status
        host.apply(good)

        # 6. The private proxy changes its Docker address: the route recovers by itself.
        master, before = host.master(), proxy_address()
        sh('docker', 'rm', '-f', 'cfmeet-core-tls')
        sh('docker', 'run', '-d', '--name', 'cfmeet-ci-address-holder', '--network', p.NET, STUB_IMAGE, 'sleep', '600')
        start_proxy(work, certs)
        after = proxy_address()
        assert after != before, 'Docker reused the address; the test would not prove anything'
        assert wait_status(401) == 401
        assert host.master() == master and p.inspect(runtime.RELAY)['Id'] == relay_id
        assert str(before) not in host.snippet.read_text() and str(after) not in host.snippet.read_text()
        evidence['proxyAddressChangeRecoveredWithoutReconfiguration'] = 'PASS'

        # 7. Rollback removes a relay this run created.
        runtime.restore_relay(relay)
        assert runtime.RELAY not in sh('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
        evidence['relayRollback'] = 'PASS'
        evidence['hostNginx'] = subprocess.run(['nginx', '-v'], capture_output=True, text=True).stderr.strip()
        print(json.dumps({'messagingRealtimeIngressCi': evidence}, indent=1))
    except Exception:
        for name in CONTAINERS:
            logs = subprocess.run(['docker', 'logs', '--tail', '20', name], capture_output=True, text=True)
            print('---', name, logs.stdout[-1500:], logs.stderr[-1500:], file=sys.stderr)
        if host.error.exists():
            print('--- host nginx', host.error.read_text()[-3000:], file=sys.stderr)
        raise
    finally:
        host.stop()
        cleanup()


if __name__ == '__main__':
    main()
