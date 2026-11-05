"""Public Messaging realtime ingress (Section 8 defect closed 2026-11-05).

Production 502 root cause: the live routes verified the private proxy with nginx's default
depth (1), which cannot reach the Generation Y chain's trust anchor. Every later repair was
then probed while the previous nginx worker generation still served requests, failed, and
was rolled back, so the corrected depth never stayed live. These tests pin the corrected
configuration, the relay that replaces the Docker address, deployment ordering, the reload
wait and exact rollback. infra/calling/ingress-ci/realtime_ingress.py runs the same code
against real nginx builds in CI."""
import copy
import itertools
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

import communications_release as release
import messaging_runtime as runtime
import production as p
import storage_encryption as storage

ROOT = Path(__file__).resolve().parents[3]
_ids = itertools.count(1)

# The routes that were live during the 502 (`/etc/nginx/snippets/cfmeet-messaging.conf`,
# 2026-11-05 23:11 CET; the private proxy's address as rendered then).
LIVE_FAILURE = '''location = /v1/realtime/tickets {
    limit_except POST OPTIONS { deny all; }
    access_log off;
    proxy_http_version 1.1;
    proxy_set_header Host $http_host;
    proxy_set_header X-Forwarded-Proto https;
    add_header Cache-Control "private, no-store" always;
    proxy_ssl_server_name on;
    proxy_ssl_name api.cfmeet.com;
    proxy_ssl_verify on;
    proxy_ssl_trusted_certificate /etc/ssl/certs/ca-certificates.crt;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600;
    proxy_pass https://172.23.0.4:8444;
}
'''
MEASURED_MINIMUM_DEPTH = 2


def locations(text):
    return {m.group(1): m.group(2) for m in re.finditer(r'^location (\S+ \S+) \{\n(.*?)^\}', text, re.S | re.M)}


def realtime_problems(block):
    """What makes a realtime route unable to verify the private proxy or tied to Docker."""
    problems = []
    depth = re.search(r'proxy_ssl_verify_depth (\d+);', block)
    if not depth or int(depth.group(1)) < MEASURED_MINIMUM_DEPTH:
        problems.append('verify depth below the chain anchor')
    for directive in ('proxy_ssl_verify on;', 'proxy_ssl_server_name on;', 'proxy_ssl_name api.cfmeet.com;',
                      'proxy_ssl_trusted_certificate '):
        if directive not in block:
            problems.append('missing ' + directive)
    if 'proxy_ssl_verify off' in block:
        problems.append('verification disabled')
    if set(re.findall(r'\d+\.\d+\.\d+\.\d+', block)) - {'127.0.0.1'}:
        problems.append('Docker address in host configuration')
    return problems


class RealtimeConfiguration(unittest.TestCase):
    def test_the_live_failure_configuration_is_recognized(self):
        problems = realtime_problems(locations(LIVE_FAILURE)['= /v1/realtime/tickets'])
        self.assertIn('verify depth below the chain anchor', problems)
        self.assertIn('Docker address in host configuration', problems)

    def test_rendered_realtime_routes_verify_beyond_the_anchor_through_the_relay(self):
        blocks = locations(runtime.nginx_config('172.23.0.5'))
        for path in ('= /v1/realtime', '= /v1/realtime/tickets'):
            block = blocks[path]
            self.assertEqual(realtime_problems(block), [], path)
            self.assertIn('proxy_pass https://127.0.0.1:%d;' % runtime.RELAY_PORT, block)
            self.assertIn('proxy_ssl_trusted_certificate %s;' % runtime.SYSTEM_BUNDLE, block)
            self.assertIn('proxy_set_header Upgrade $http_upgrade;', block)
            self.assertIn('proxy_set_header Connection "upgrade";', block)
            self.assertIn('add_header Cache-Control "private, no-store" always;', block)
        self.assertIn('limit_except GET { deny all; }', blocks['= /v1/realtime'])
        self.assertIn('limit_except POST OPTIONS { deny all; }', blocks['= /v1/realtime/tickets'])
        self.assertGreaterEqual(runtime.UPSTREAM_VERIFY_DEPTH, MEASURED_MINIMUM_DEPTH)
        self.assertEqual(runtime.SYSTEM_BUNDLE.as_posix(), '/etc/ssl/certs/ca-certificates.crt')

    def test_attachment_route_is_unchanged(self):
        block = locations(runtime.nginx_config('172.23.0.5'))['^~ /cfmeet-messaging-attachments/']
        self.assertIn('proxy_pass http://172.23.0.5:9000;', block)
        self.assertNotIn('proxy_ssl', block)

    def test_relay_is_tcp_passthrough_resolving_the_proxy_by_name(self):
        text = runtime.relay_config()
        self.assertIn('stream {', text)
        self.assertNotIn('http {', text)
        self.assertNotIn('ssl_certificate', text)
        self.assertNotIn('proxy_ssl', text)
        self.assertIn('resolver 127.0.0.11 valid=10s ipv6=off;', text)
        self.assertIn('set $peer cfmeet-core-tls:8444;', text)
        self.assertIn('proxy_pass $peer;', text)
        self.assertEqual(set(re.findall(r'\d+\.\d+\.\d+\.\d+', text)), {'127.0.0.11'})

    def test_relay_container_is_loopback_only_and_confined(self):
        args = runtime.relay_create_args('sha256:proxy-nginx', '/w/ingress-relay.conf')
        published = [args[i + 1] for i, a in enumerate(args) if a in ('--publish', '-p')]
        self.assertEqual(published, ['127.0.0.1:18444:8444'])
        for flag in ('--read-only', 'no-new-privileges'):
            self.assertIn(flag, args)
        self.assertEqual(args[args.index('--cap-drop') + 1], 'ALL')
        self.assertEqual(args[args.index('--user') + 1], '101:101')
        self.assertEqual(args[args.index('--restart') + 1], 'unless-stopped')
        self.assertEqual(args[args.index('--memory') + 1], '32m')
        self.assertIn('max-size=5m', args)
        self.assertIn('type=bind,src=/w/ingress-relay.conf,dst=/etc/nginx/nginx.conf,readonly', args)
        self.assertEqual(args[args.index('--entrypoint') + 2], 'sha256:proxy-nginx')
        for forbidden in ('--privileged', 'host', '--cap-add', '0.0.0.0'):
            self.assertFalse(any(forbidden in a for a in args), forbidden)
        self.assertEqual(sum(1 for a in args if a.startswith('type=bind')), 1, 'no certificate or key in the relay')

    def test_relay_is_registered_as_long_lived_infrastructure(self):
        import resource_recovery
        self.assertIn(runtime.RELAY, resource_recovery.LONG_LIVED)


def container_from(args, confined=True):
    publish = args[args.index('--publish') + 1].split(':')
    return {'Id': 'r%d' % next(_ids), 'Image': args[args.index('--entrypoint') + 2], 'State': {'Running': False},
            'HostConfig': {'PortBindings': {publish[2] + '/tcp': [{'HostIp': publish[0], 'HostPort': publish[1]}]},
                           'ReadonlyRootfs': confined and '--read-only' in args,
                           'CapDrop': [args[args.index('--cap-drop') + 1]] if confined else None},
            'NetworkSettings': {'Networks': {args[args.index('--network') + 1]: {}}}}


class RelayDocker:
    """Docker as the relay lifecycle sees it; containers tracked by identity."""

    def __init__(self, confined=True):
        self.containers = {'cfmeet-core-tls': {'Image': 'sha256:proxy-nginx', 'NetworkSettings': {'Networks': {p.NET: {'IPAddress': '172.23.0.4'}}}},
                           storage.NAME: {'Image': 'sha256:minio', 'NetworkSettings': {'Networks': {p.NET: {'IPAddress': '172.23.0.5'}}}}}
        self.networks = {p.NET}
        self.commands = []
        self.fail = None
        self.confined = confined

    def run(self, *args, input=None):
        self.commands.append(args)
        if self.fail and args[:len(self.fail)] == self.fail:
            raise RuntimeError('subprocess failed: docker')
        if args[:3] == ('docker', 'ps', '-a'):
            return '\n'.join(self.containers)
        if args[:3] == ('docker', 'network', 'ls'):
            return '\n'.join(sorted(self.networks))
        if args[:3] == ('docker', 'network', 'create'):
            self.networks.add(args[3])
        elif args[:3] == ('docker', 'network', 'connect'):
            self.containers[args[4]]['NetworkSettings']['Networks'][args[3]] = {}
        elif args[:2] == ('docker', 'create'):
            self.containers[args[args.index('--name') + 1]] = container_from(args, self.confined)
        elif args[:2] == ('docker', 'start'):
            self.containers[args[2]]['State']['Running'] = True
        elif args[:2] == ('docker', 'stop'):
            self.containers[args[-1]]['State']['Running'] = False
        elif args[:2] == ('docker', 'rename'):
            self.containers[args[3]] = self.containers.pop(args[2])
        elif args[:3] == ('docker', 'rm', '-f'):
            self.containers.pop(args[3])
        return ''

    def inspect(self, name):
        return self.containers[name]

    def mutations(self):
        return [c for c in self.commands if c[1] in ('create', 'start', 'stop', 'rename', 'rm')]


class Fixture(unittest.TestCase):
    SITE = 'server {\n    server_name api.cfmeet.com;\n    include /etc/nginx/snippets/cfmeet-calling-routes.conf;\n}\n'

    def setUp(self):
        folder = tempfile.TemporaryDirectory(); self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        self.work, self.site, self.snippet = root / 'messaging', root / 'api.cfmeet.com', root / 'cfmeet-messaging.conf'
        self.site.write_text(self.SITE)
        self.snippet.write_text(LIVE_FAILURE)
        self.docker = RelayDocker()
        self.reloads = []
        def reload():
            self.reloads.append(self.snippet.read_text())
            return [1]
        self.reload = reload
        for target, name, value in ((runtime, 'WORK', self.work), (runtime, 'SITE', self.site), (runtime, 'SITE_LINK', self.site),
                                    (runtime, 'SNIPPET', self.snippet)):
            patcher = patch.object(target, name, value); patcher.start(); self.addCleanup(patcher.stop)
        for name, value in (('run', self.docker.run), ('inspect', self.docker.inspect)):
            patcher = patch.object(p, name, side_effect=value); patcher.start(); self.addCleanup(patcher.stop)
        for target, name, kwargs in ((runtime, 'wait_for_relay', {}), (runtime, 'verifies', {'return_value': True}),
                                     (p, 'identity', {}), (p, 'regression', {})):
            patcher = patch.object(target, name, **kwargs); patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(p, 'reload_host_nginx', side_effect=lambda: self.reload()); patcher.start(); self.addCleanup(patcher.stop)


class RelayLifecycle(Fixture):
    def test_first_install_creates_a_confined_relay_on_both_networks(self):
        state = runtime.ensure_relay('1')
        relay = self.docker.containers[runtime.RELAY]
        self.assertEqual(state['relay'], 'created')
        self.assertTrue(relay['State']['Running'])
        self.assertEqual(relay['Image'], 'sha256:proxy-nginx', 'the private proxy image; nothing is pulled')
        self.assertEqual(set(relay['NetworkSettings']['Networks']), {runtime.RELAY_NET, p.NET})
        self.assertEqual(runtime.relay_conf().read_text(), runtime.relay_config())
        self.assertEqual(runtime.relay_conf().stat().st_mode & 0o777 & 0o444, 0o444)
        order = [c[:2] for c in self.docker.mutations()]
        self.assertEqual(order, [('docker', 'create'), ('docker', 'start')])
        connect = self.docker.commands.index(('docker', 'network', 'connect', p.NET, runtime.RELAY))
        self.assertLess(connect, self.docker.commands.index(('docker', 'start', runtime.RELAY)),
                        'the relay accepts connections only once it can reach the proxy')

    def test_identical_running_relay_is_reused_without_interruption(self):
        runtime.ensure_relay('1')
        relay = self.docker.containers[runtime.RELAY]
        self.docker.commands.clear()
        state = runtime.ensure_relay('2')
        self.assertEqual(state['relay'], 'reused')
        self.assertIs(self.docker.containers[runtime.RELAY], relay)
        self.assertEqual(self.docker.mutations(), [])
        runtime.restore_relay(state)
        self.assertIs(self.docker.containers[runtime.RELAY], relay)

    def test_changed_relay_is_replaced_and_restored_exactly(self):
        runtime.ensure_relay('1')
        old = self.docker.containers[runtime.RELAY]
        old['Image'] = 'sha256:older-nginx'
        runtime.relay_conf().write_text('# older relay\n')
        state = runtime.ensure_relay('2')
        self.assertEqual(state['previous'], runtime.RELAY + '-previous-2')
        self.assertIsNot(self.docker.containers[runtime.RELAY], old)
        self.assertFalse(old['State']['Running'])
        runtime.restore_relay(state)
        self.assertIs(self.docker.containers[runtime.RELAY], old)
        self.assertTrue(old['State']['Running'])
        self.assertNotIn(runtime.RELAY + '-previous-2', self.docker.containers)
        self.assertEqual(runtime.relay_conf().read_text(), '# older relay\n')

    def test_failed_creation_leaves_no_new_relay(self):
        self.docker.fail = ('docker', 'network', 'connect')
        with self.assertRaises(RuntimeError):
            runtime.ensure_relay('1')
        self.assertNotIn(runtime.RELAY, self.docker.containers)

    def test_unconfined_relay_is_refused_and_removed(self):
        self.docker.confined = False
        with self.assertRaises(AssertionError):
            runtime.ensure_relay('1')
        self.assertNotIn(runtime.RELAY, self.docker.containers)


class Deployment(Fixture):
    def test_install_writes_routes_then_returns_only_after_the_reload(self):
        state = runtime.install_ingress('5')
        self.assertEqual(state['relay'], 'created')
        self.assertEqual(len(self.reloads), 1)
        self.assertEqual(self.reloads[0], runtime.nginx_config('172.23.0.5'), 'reload saw the new routes')
        self.assertEqual(self.site.read_text().count('include /etc/nginx/snippets/cfmeet-messaging.conf;'), 1)
        self.assertEqual((self.work / 'nginx-snippet-before-5').read_text(), LIVE_FAILURE)

    def test_failed_reload_restores_routes_and_relay(self):
        outcomes = iter([RuntimeError('Host nginx did not complete the reload'), None])
        def reload():
            self.reloads.append(self.snippet.read_text())
            outcome = next(outcomes)
            if outcome: raise outcome
        self.reload = reload
        with self.assertRaises(RuntimeError):
            runtime.install_ingress('6')
        self.assertEqual(self.site.read_text(), self.SITE)
        self.assertEqual(self.snippet.read_text(), LIVE_FAILURE)
        self.assertEqual(self.reloads[-1], LIVE_FAILURE, 'restore reloads the previous routes')
        self.assertNotIn(runtime.RELAY, self.docker.containers)

    def test_unverifiable_upstream_never_reaches_host_nginx(self):
        with patch.object(runtime, 'verifies', return_value=False), patch.object(runtime.Path, 'glob', return_value=[]), \
             patch.object(runtime, 'UPSTREAM_TRUST', self.work / 'trust.pem'):
            self.work.mkdir(exist_ok=True)
            with self.assertRaises(AssertionError):
                runtime.install_ingress('7')
        self.assertEqual(self.snippet.read_text(), LIVE_FAILURE)
        self.assertEqual(self.site.read_text(), self.SITE)
        self.assertNotIn(runtime.RELAY, self.docker.containers)

    def test_failed_probe_restores_the_exact_previous_routes_and_relay(self):
        with patch.object(runtime, 'ingress_probe', side_effect=AssertionError('Realtime route answered 502')), \
             patch.object(runtime, 'ingress_diagnostics', return_value={}), self.assertRaises(AssertionError):
            runtime.reinstall_ingress('8')
        self.assertEqual(self.site.read_text(), self.SITE)
        self.assertEqual(self.snippet.read_text(), LIVE_FAILURE)
        self.assertEqual(len(self.reloads), 2)
        self.assertNotIn(runtime.RELAY, self.docker.containers)

    def test_failed_regression_after_the_probe_also_restores(self):
        with patch.object(runtime, 'ingress_probe', return_value={}), patch.object(p, 'regression', side_effect=RuntimeError('x')), \
             patch.object(runtime, 'ingress_diagnostics', return_value={}), self.assertRaises(RuntimeError):
            runtime.reinstall_ingress('9')
        self.assertEqual(self.snippet.read_text(), LIVE_FAILURE)
        self.assertNotIn(runtime.RELAY, self.docker.containers)

    def test_repeated_operations_persist_without_drift_or_relay_restart(self):
        with patch.object(runtime, 'ingress_probe', return_value={'ticketUnauthenticated': 401}):
            runtime.reinstall_ingress('10')
            first = (self.site.read_text(), self.snippet.read_text(), self.docker.containers[runtime.RELAY])
            self.docker.commands.clear()
            runtime.reinstall_ingress('11')
        self.assertEqual(self.site.read_text(), first[0])
        self.assertEqual(self.snippet.read_text(), first[1])
        self.assertIs(self.docker.containers[runtime.RELAY], first[2])
        self.assertEqual(self.docker.mutations(), [])
        self.assertEqual(realtime_problems(locations(self.snippet.read_text())['= /v1/realtime/tickets']), [])


class Verification(unittest.TestCase):
    def test_verification_matches_nginx_depth_name_and_relay(self):
        seen = []
        class Done:
            returncode, stdout = 0, 'Verify return code: 0 (ok)'
        with patch.object(runtime.subprocess, 'run', side_effect=lambda args, **kw: seen.append(args) or Done()):
            self.assertTrue(runtime.verifies(runtime.SYSTEM_BUNDLE))
        args = seen[0]
        self.assertIn('127.0.0.1:18444', args)
        self.assertEqual(args[args.index('-verify_depth') + 1], str(runtime.UPSTREAM_VERIFY_DEPTH))
        self.assertEqual(args[args.index('-verify_hostname') + 1], 'api.cfmeet.com')
        self.assertIn('-verify_return_error', args)


class ConfigureRollback(unittest.TestCase):
    def test_failure_after_ingress_restores_it_with_the_container(self):
        import test_messaging_config_persistence as persistence
        folder = tempfile.TemporaryDirectory(); self.addCleanup(folder.cleanup)
        work = Path(folder.name)
        for name in ('managed.ring', 'storage-secret'):
            (work / name).write_text('fixture'); (work / name).chmod(0o600)
        old = persistence.running_messaging()
        docker = persistence.FakeDocker({'cfmeet-messaging': old})
        relay = {'relay': 'created', 'previous': None, 'confBefore': None}
        with patch.object(runtime, 'WORK', work), patch.object(runtime.os, 'chown'), \
             patch.object(release.p, 'run', side_effect=docker.run), patch.object(release.p, 'inspect', side_effect=docker.inspect), \
             patch.object(release.p, 'docker_api', side_effect=docker.api), patch.object(release, 'wait'), \
             patch.object(runtime, 'scanner_in_use', return_value='quarantine'), patch.object(runtime, 'dependency_probe', return_value={}), \
             patch.object(release.p, 'secret', return_value='fixture'), patch.object(runtime, 'install_ingress', return_value=relay), \
             patch.object(runtime, 'storage_probe', side_effect=RuntimeError('storage probe failed')), \
             patch.object(runtime, 'restore_ingress') as restore, self.assertRaises(RuntimeError):
            runtime._configure('0' * 40, '77', old, release.p.env_of(old), work / 'managed.ring', 'user', work / 'storage-secret')
        restore.assert_called_once_with('77', relay)
        self.assertIs(docker.by_name['cfmeet-messaging'], old)


class ReloadWait(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory(); self.addCleanup(folder.cleanup)
        self.pidfile = Path(folder.name) / 'nginx.pid'
        self.pidfile.write_text('100\n')
        self.commands = []

    def run_reload(self, listings, timeout=5):
        listings = iter(listings)
        class Listing:
            def __init__(self, text): self.stdout = text
        def ps(args, **kw):
            self.assertEqual(args[:3], ['ps', '--ppid', '100'])
            return Listing(next(listings))
        with patch.object(p.subprocess, 'run', side_effect=ps), patch.object(p, 'run', side_effect=lambda *a, **k: self.commands.append(a)), \
             patch.object(p.time, 'sleep'):
            return p.reload_host_nginx(timeout=timeout, pidfile=self.pidfile)

    def test_returns_only_when_no_previous_worker_still_accepts(self):
        old = '201 nginx: worker process\n202 nginx: worker process\n203 nginx: cache manager process\n'
        workers = self.run_reload([
            old,
            old,                                                     # master still parsing
            old + '301 nginx: worker process\n',                     # new generation, old still accepting
            '201 nginx: worker process is shutting down\n202 nginx: worker process\n301 nginx: worker process\n302 nginx: worker process\n',
            '201 nginx: worker process is shutting down\n301 nginx: worker process\n302 nginx: worker process\n',
        ])
        self.assertEqual(workers, [301, 302])
        self.assertEqual(self.commands, [('nginx', '-t'), ('systemctl', 'reload', 'nginx')])

    def test_reload_that_never_completes_fails(self):
        old = '201 nginx: worker process\n'
        with self.assertRaises(RuntimeError):
            self.run_reload(itertools.repeat(old), timeout=0)

    def test_invalid_configuration_is_never_reloaded(self):
        with patch.object(p, 'run', side_effect=RuntimeError('subprocess failed: nginx')), self.assertRaises(RuntimeError):
            p.reload_host_nginx(pidfile=self.pidfile)

    def test_no_accepting_workers_refuses_to_reload(self):
        with self.assertRaises(AssertionError):
            self.run_reload(['201 nginx: worker process is shutting down\n'])
        self.assertEqual(self.commands, [('nginx', '-t')])


def messaging(status, reason=None, code='AUTHENTICATION_FAILED', cache='no-store, private, no-store'):
    error = {'code': code, 'message': 'refused'}
    if reason: error['reason'] = reason
    headers = {'cache-control': cache, 'content-type': 'application/json'}
    if status == 401: headers['www-authenticate'] = 'Bearer realm="cf-meet-messaging"'
    return status, headers, json.dumps({'error': error}).encode()


class Probe(unittest.TestCase):
    def answers(self, overrides=None):
        sent = []
        def request(method, path, headers=None, body=None):
            headers = headers or {}
            sent.append((method, path, dict(headers)))
            for match, answer in (overrides or {}).items():
                if match(method, path, headers):
                    return answer
            if method == 'OPTIONS': return 403, {}, b''
            if method == 'DELETE': return 403, {'content-type': 'text/html'}, b'<html>403</html>'
            if method == 'POST':
                return messaging(401, 'ORIGIN_NOT_ALLOWED' if 'Origin' in headers else None)
            protocols = headers.get('Sec-WebSocket-Protocol')
            if not protocols: return messaging(400, 'SUBPROTOCOL_REQUIRED', 'VALIDATION_ERROR')
            return messaging(401, 'INVALID_TICKET' if 'cfmeet.ticket.' in protocols else 'TICKET_REQUIRED')
        return request, sent

    def test_messaging_refusals_on_every_path(self):
        request, sent = self.answers()
        evidence = runtime.ingress_probe(request)
        self.assertEqual(evidence['ticketUnauthenticated'], 401)
        self.assertEqual(evidence['websocketForgedTicket'], 401)
        self.assertEqual(evidence['websocketNoSubprotocol'], 400)
        self.assertEqual(evidence['validTicketIssuance'], 'NOT_RUN_PRODUCTION_E2EE_DEVICE_ENROLLMENT_DISABLED')
        tokens = [h['Authorization'] for _, _, h in sent if 'Authorization' in h]
        self.assertTrue(tokens and all(t.startswith('CF-Client-Token ') for t in tokens))
        forged = [h['Sec-WebSocket-Protocol'].split('cfmeet.ticket.')[1] for _, _, h in sent if 'cfmeet.ticket.' in h.get('Sec-WebSocket-Protocol', '')]
        self.assertTrue(re.fullmatch(r'[0-9a-f]{64}', forged[0]), 'well-formed, so the ticket store is consulted')

    def test_failures_that_must_not_pass(self):
        ticket_post = lambda m, path, h: m == 'POST' and 'Authorization' not in h
        websocket = lambda m, path, h: m == 'GET' and h.get('Sec-WebSocket-Protocol') == 'cfmeet.messaging.v1'
        cases = {
            'bad gateway': (ticket_post, (502, {'content-type': 'text/html'}, b'<html>502 Bad Gateway</html>')),
            'not through the realtime location': (ticket_post, messaging(401, cache='no-store')),
            'not Messaging': (ticket_post, (401, {'cache-control': 'private, no-store'}, b'{"error": {"code": "OTHER"}}')),
            'ticketless upgrade': (websocket, (101, {'cache-control': 'private, no-store'}, b'')),
            'preflight allowed': (lambda m, path, h: m == 'OPTIONS', (204, {'access-control-allow-origin': 'https://x'}, b'')),
            'other method allowed': (lambda m, path, h: m == 'DELETE', messaging(401)),
        }
        for label, (match, answer) in cases.items():
            request, _ = self.answers({match: answer})
            with self.assertRaises((AssertionError, ValueError, KeyError), msg=label):
                runtime.ingress_probe(request)


class Workflows(unittest.TestCase):
    def test_ingress_operations_run_the_real_nginx_test_first(self):
        text = (ROOT / '.github/workflows/cfmeet-calling-production.yml').read_text()
        step = text.index('python3 infra/calling/ingress-ci/realtime_ingress.py')
        self.assertLess(step, text.index('Execute versioned production operation'))
        self.assertIn("inputs.operation == 'messaging-ingress' || inputs.operation == 'messaging-configure'", text)

    def test_ci_runs_the_real_nginx_test_on_changes(self):
        text = (ROOT / '.github/workflows/messaging-ingress.yml').read_text()
        self.assertIn('infra/calling/**', text)
        self.assertIn('python3 infra/calling/ingress-ci/realtime_ingress.py', text)
        self.assertIn('runs-on: ubuntu-24.04', text)


if __name__ == '__main__':
    unittest.main()
