"""The shared scan gateway deployment: engine admission, private routing, hardening."""
import datetime
import json
from pathlib import Path
import unittest

import scan_gateway as g

NOW = datetime.datetime(2026, 11, 5, 18, 0, tzinfo=datetime.timezone.utc)
# Measured on the VPS (resource-scanners 42077986241).
MAIL = {'StreamMaxLength': '25M', 'MaxThreads': '8', 'ConcurrentDatabaseReload': 'no',
        'AlertExceedsMax': 'yes', 'TCPSocket': '3310'}
CGROUP = {'memory.max': 3072, 'memory.peak': 1846, 'memory.current': 952, 'oom_kill': 0, 'cpu_usage_s': 1.0}
VERSION = 'ClamAV 1.4.6/28173/Tue Nov  3 06:27:52 2026'


def engine(**state):
    c = {'State': {'Running': True, 'Health': {'Status': 'healthy'}, 'OOMKilled': False},
         'Config': {'Labels': {'com.docker.compose.project': 'admin-server-mail'}},
         'HostConfig': {'PortBindings': {}}}
    c['State'].update(state)
    return c


class EngineAdmission(unittest.TestCase):
    def admit(self, c=None, cgroup=None, config=None, version=VERSION, now=NOW):
        return g.engine_preconditions(c or engine(), dict(CGROUP, **(cgroup or {})), dict(MAIL, **(config or {})),
                                      g.parse_engine_version(version, now))

    def test_the_measured_production_mail_engine_is_admitted(self):
        e = self.admit()
        self.assertEqual((e['headroomMib'], e['gatewayShare'], e['maxThreads']), (1226, 2, 8))
        self.assertEqual(e['signatureVersion'], 28173)
        self.assertAlmostEqual(e['signatureAgeHours'], 59.5, places=1)

    def test_an_engine_that_would_be_harmed_or_is_unsafe_is_refused(self):
        refusals = [
            dict(c=engine(Running=False)),
            dict(c=engine(Health={'Status': 'unhealthy'})),
            dict(c=engine(OOMKilled=True)),
            # The KarryGo scanner: peak at its 1536 MiB limit.
            dict(cgroup={'memory.max': 1536, 'memory.peak': 1536}),
            dict(cgroup={'oom_kill': 1}),
            dict(cgroup={'memory.max': 'max'}),
            dict(config={'StreamMaxLength': '10M'}),
            dict(config={'MaxThreads': '4'}),
            dict(config={'AlertExceedsMax': 'no'}),
            dict(config={'ConcurrentDatabaseReload': 'yes'}),
            dict(now=NOW + datetime.timedelta(hours=13)),   # signatures past 72 h
        ]
        for kwargs in refusals:
            with self.assertRaises(AssertionError, msg=str(kwargs)):
                self.admit(**kwargs)
        other = engine(); other['Config']['Labels']['com.docker.compose.project'] = 'admin-server-staging-mail'
        with self.assertRaises(AssertionError):
            self.admit(c=other)
        published = engine(); published['HostConfig']['PortBindings'] = {'3310/tcp': [{'HostPort': '3310'}]}
        with self.assertRaises(AssertionError):
            self.admit(c=published)
        with self.assertRaises(AssertionError):
            g.parse_engine_version('ClamAV 1.4.6', NOW)

    def test_sizes_parse_like_clamd(self):
        self.assertEqual(g.parse_size('25M'), 25 * 1024 * 1024)
        self.assertEqual(g.parse_size('100'), 100)
        with self.assertRaises(AssertionError):
            g.parse_size('lots')


NGINX = 'events {}\nhttp {\n  server { listen 8443 ssl; server_name api.cfmeet.com; }\n}\n'


class PrivateRoute(unittest.TestCase):
    def test_route_exposes_only_scans_and_readiness_and_is_idempotent(self):
        once = g.tls_route(NGINX)
        self.assertEqual(g.tls_route(once), once)
        route = [l for l in once.splitlines() if 'listen 8449' in l][0]
        self.assertIn('location = /internal/v1/scans { limit_except POST', route)
        self.assertIn('location = /ready { limit_except GET', route)
        self.assertIn('location / { return 404; }', route)
        self.assertNotIn('9199', route)
        self.assertNotIn('/metrics', route)
        self.assertIn('proxy_request_buffering off', route)
        self.assertIn('client_max_body_size 26m', route)
        self.assertTrue(once.rstrip().endswith('}'))

    def test_route_removal_restores_the_original_and_ports_are_never_shared(self):
        self.assertEqual(g.remove_tls_route(g.tls_route(NGINX)).strip(), NGINX.strip())
        self.assertEqual(g.remove_tls_route(NGINX), NGINX)
        with self.assertRaises(AssertionError):
            g.tls_route(NGINX.replace('8443', '8449'))


class Hardening(unittest.TestCase):
    def test_gateway_container_is_private_unprivileged_and_bounded(self):
        args = g.run_args('cosmicforge-scan-gateway', 'img:sha', Path('/opt/cfmeet-core/scan-gateway'))
        joined = ' '.join(args)
        for flag in ('--read-only', '--cap-drop ALL', '--security-opt no-new-privileges', '--user 10002:10002',
                     '--memory 64m', '--memory-swap 64m', '--pids-limit 64', '--cpus 0.5'):
            self.assertIn(flag, joined)
        for forbidden in ('-p ', '--publish', '--privileged', 'docker.sock', 'clamav/clamav', '--network host'):
            self.assertNotIn(forbidden, joined)
        mounts = [a for a in args if a.startswith('type=bind')]
        self.assertEqual(len(mounts), 3)
        self.assertTrue(all(m.endswith(',readonly') for m in mounts))
        self.assertEqual(args[-1], 'img:sha')
        env = dict(a.split('=', 1) for i, a in enumerate(args) if args[i - 1] == '--env')
        self.assertEqual(env['CF_RUNTIME_ENVIRONMENT'], 'production')
        self.assertEqual(env['SCAN_GATEWAY_BACKEND_ADDRESS'], 'cosmicforge-scan-engine:3310')
        self.assertEqual(int(env['SCAN_GATEWAY_BACKEND_STREAM_MAX_BYTES']), 25 * 1024 * 1024)
        self.assertLessEqual(int(env['SCAN_GATEWAY_BACKEND_MAX_CONCURRENCY']), 2)

    def test_only_messaging_is_trusted_and_registered_within_the_engine_limits(self):
        jwk = {'kid': 'messaging-1', 'kty': 'OKP', 'crv': 'Ed25519', 'x': 'x'}
        b = g.bundle(jwk)
        self.assertEqual(b['receiver'], 'cfmeet.security-scan')
        self.assertEqual([(i['issuer'], i['permissions'], i['runtime_environment']) for i in b['trusted_issuers']],
                         [('cfmeet.messaging', ['security-scan.submit'], 'production')])
        r = g.registry()
        self.assertEqual(r['environment'], 'production')
        self.assertEqual([c['issuer'] for c in r['clients']], ['cfmeet.messaging'])
        for c in r['clients']:
            self.assertLessEqual(c['maxBytes'], g.STREAM_MAX_BYTES)
            self.assertLessEqual(c['maxConcurrent'], g.BACKEND_CONCURRENCY)
        json.dumps(b); json.dumps(r)


if __name__ == '__main__':
    unittest.main()
