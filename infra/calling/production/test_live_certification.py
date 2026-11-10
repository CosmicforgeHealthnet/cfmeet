"""Offline tests of the read-only live-production certification (live_certification.py)
and of the `certify` / `verify` contract in coordinated_release.py."""
import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import live_certification as lc
import production as p

SHA = 'a' * 40
OLD = 'b' * 40
REPO_URL = 'https://github.com/CosmicforgeHealthnet/cfmeet'
# Docker sub-commands certification may issue. Everything else mutates the host.
READ_ONLY_DOCKER = {('docker', 'inspect'), ('docker', 'image'), ('docker', 'ps'), ('docker', 'run'), ('docker', 'exec'),
                    ('nsenter', '-t')}


def container(name, image='sha256:img', env=None, labels=None, mounts=(), restarts=0, aliases=(), ip='10.0.0.9'):
    return {'Name': '/' + name, 'Id': name + '-id', 'Image': image, 'RestartCount': restarts,
            'State': {'Running': True, 'Status': 'running', 'OOMKilled': False, 'StartedAt': '2026-11-10T00:00:00Z',
                      'Pid': 4242},
            'Config': {'Env': ['%s=%s' % kv for kv in (env or {}).items()], 'Labels': labels or {}, 'Image': image},
            'HostConfig': {'RestartPolicy': {'Name': 'unless-stopped'}, 'Memory': 0, 'ReadonlyRootfs': True},
            'Mounts': [{'Source': s, 'Destination': d} for s, d in mounts],
            'NetworkSettings': {'Networks': {p.NET: {'Aliases': list(aliases), 'IPAddress': ip}}}}


class FakeHost:
    """A production host where every catalogue service runs `revision`'s image. Records every
    docker invocation; answers probes like a healthy deployment."""

    def __init__(self, folder, revision=SHA, http=None, db_ok=True):
        self.folder = Path(folder)
        self.revision = revision
        self.http = http or {}
        self.db_ok = db_ok
        self.calls = []
        self.containers = {}
        key = self.folder / 'key.pem'
        key.write_text('not a real key')
        for name in cr.ORDER:
            s = cr.SERVICES[name]
            env = {'CF_SERVICE_AUTH_KEY_ID': name + '-production-1', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/run/secrets/k.pem',
                   'CF_PLATFORM_ACCESS_BASE_URL': 'https://api.cfmeet.com:8443'}
            mounts = [(str(key), '/run/secrets/k.pem')]
            if name != 'api-gateway':
                bundle = self.folder / (name + '.trust.json')
                bundle.write_text(json.dumps(dict(receiver=cr.service_id(name), runtime_environment='production',
                                                  trusted_issuers=[])))
                env['CF_SERVICE_TRUST_BUNDLE_FILE'] = '/run/secrets/trust.json'
                mounts.append((str(bundle), '/run/secrets/trust.json'))
            if name == 'api-gateway':
                env['API_GATEWAY_PUBLIC_ROUTING_ENABLED'] = 'true'
            if s['db'] or name == 'meeting-service':
                env['DATABASE_URL'] = 'postgres://%s:hunter2@cfmeet-core-db:5432/%s' % (s['role'] or 'meeting', s['db'] or 'cfmeet')
            self.containers[s['container']] = container(s['container'], 'sha256:' + cr.image_of(name), env,
                                                        {'org.opencontainers.image.revision': OLD}, mounts)
        # The private TLS proxy answers for api.cfmeet.com on the private network.
        self.containers['cfmeet-core-tls'] = container('cfmeet-core-tls', aliases=['api.cfmeet.com'], ip='10.0.0.2')

    def image_meta(self, image):
        return [{'Id': image, 'Created': '2026-11-09T00:00:00Z', 'RepoDigests': [],
                 'Config': {'Labels': {'org.opencontainers.image.revision': self.revision,
                                       'org.opencontainers.image.source': REPO_URL}}}]

    def run(self, *args, input=None):
        self.calls.append(args)
        if args[:2] == ('docker', 'ps'):
            return '\n'.join(self.containers)
        if args[:3] == ('docker', 'image', 'inspect'):
            return json.dumps(self.image_meta(args[3]))
        if args[:2] == ('docker', 'exec'):
            assert input.startswith(lc.READ_ONLY_SQL), 'database session is not read-only'
            if 'information_schema.tables' in input:
                return '\n'.join(t for s in cr.SERVICES.values() for t in s['history'])
            if 'SELECT version FROM' in input:
                return 'm20261101_000001_x\nm20261102_000001_y'
            return '{}'
        if args[:2] == ('docker', 'stats'):
            return ''
        raise AssertionError('unexpected command %s' % (args[:3],))

    def docker(self, args, env=None, timeout=60):
        self.calls.append(tuple(args))
        if args[:3] == ['docker', 'image', 'inspect']:
            ref = args[-1]
            if ref.startswith(cr.IMAGE_REPO):
                image = ref[len(cr.IMAGE_REPO):].split(':')[0]
                return 0, 'sha256:' + image
            return 0, 'sha256:probe'
        if args[0] == 'nsenter':
            assert args[:5] == ['nsenter', '-t', '4242', '-n', '--'] and args[5] == 'curl', args
            url = args[-1]
            if 'api.cfmeet.com' in url:
                assert any(a.startswith('api.cfmeet.com:') and a.endswith(':10.0.0.2') for a in args), args
            for fragment, answer in self.http.items():
                if fragment in url:
                    return 0, answer
            if '/internal/' in url or url.endswith(('/api/v1/users/me', '/v1/calls', '/sms-bridge/disclosure')):
                return 0, '{"error":{"code":"INTERNAL_AUTHENTICATION_FAILED"}}\n401'
            return 0, '{"status":"ok"}\n200'
        assert args[:2] == ['docker', 'run'] and '--rm' in args and 'never' in args and '--read-only' in args
        if lc.PG in args:
            assert env and env['PGOPTIONS'] == '-c default_transaction_read_only=on'
            assert 'hunter2' not in ' '.join(args), 'database URL on the command line'
            sql = args[-1]
            if 'SELECT version FROM' in sql:
                return 0, 'm20261101_000001_x'
            if 'information_schema.tables' in sql:
                return 0, 'seaql_migrations'
            if not self.db_ok:
                return 2, ''
            db = env['DATABASE_URL'].rsplit('/', 1)[1]
            return 0, '%s|role|on|160004' % db
        raise AssertionError('unexpected docker run %s' % args)

    def inspect(self, name):
        if name not in self.containers:
            raise RuntimeError('subprocess failed: docker')
        return self.containers[name]


@contextlib.contextmanager
def host(**kwargs):
    with tempfile.TemporaryDirectory() as folder:
        work = Path(folder) / 'work'
        fake = FakeHost(folder, **kwargs)
        out = io.StringIO()
        with patch.object(cr, 'WORK', work), patch.object(cr, 'JOURNAL', work / 'journal.json'), \
                patch.object(p, 'run', side_effect=fake.run), patch.object(p, 'inspect', side_effect=fake.inspect), \
                patch.object(lc, 'docker', side_effect=fake.docker), patch.object(lc.time, 'sleep'), \
                patch.object(lc.shutil, 'which', return_value='/usr/bin/tool'), \
                patch.object(p, 'identity'), patch.object(cr, 'public_smoke', return_value={'public_swagger': 'PASS'}), \
                patch.object(cr, 'mem_available_mib', return_value=5000), \
                patch.object(cr, 'disk_free_bytes', return_value=100 * 2**30), contextlib.redirect_stdout(out):
            yield fake, work, out


def records(out, kind):
    return [json.loads(line)[kind] for line in out.getvalue().splitlines() if line.startswith('{"' + kind + '"')]


class ReadOnlyByConstruction(unittest.TestCase):
    def test_certify_issues_only_read_only_docker_commands_and_writes_nothing(self):
        with host() as (fake, work, out):
            cr.run('certify', SHA, '42-1')
            self.assertFalse(work.exists(), 'certify created the coordinated work directory')
        for call in fake.calls:
            self.assertIn(tuple(call[:2]), READ_ONLY_DOCKER, call)
            if call[:2] in (('docker', 'image'),):
                self.assertEqual(call[2], 'inspect')
            if call[0] == 'nsenter':
                self.assertEqual(call[3:6], ('-n', '--', 'curl'))  # network namespace only
                continue
            if call[:2] == ('docker', 'run'):
                for flag in ('--rm', '--read-only', 'no-new-privileges'):
                    self.assertIn(flag, call)
                self.assertEqual(call[call.index('--pull') + 1], 'never')
        self.assertNotIn('hunter2', out.getvalue())
        self.assertEqual(len(records(out, 'certifyService')), len(cr.ORDER))
        self.assertTrue(records(out, 'certifyCollected'))

    def test_certify_takes_no_confirmation_and_never_pulls(self):
        with host() as (fake, work, out):
            with self.assertRaisesRegex(RuntimeError, 'no confirmation'):
                cr.run('certify', SHA, '42-1', 'yes')
            with patch.object(lc, 'has_image', return_value=False):
                with self.assertRaisesRegex(RuntimeError, 'never pulls'):
                    cr.run('certify', SHA, '42-1')

    def test_safe_run_flags_are_constant(self):
        self.assertEqual(lc.SAFE_RUN[:3], ('--rm', '--pull', 'never'))
        self.assertIn('--read-only', lc.SAFE_RUN)
        self.assertIn('ALL', lc.SAFE_RUN)


class PerServiceChecks(unittest.TestCase):
    def test_healthy_mixed_topology_passes_every_service_check(self):
        with host() as (fake, work, out):
            cr.run('certify', SHA, '42-1')
        for result in records(out, 'certifyService'):
            for key in ('container_state', 'image', 'readiness', 'database', 'migrations', 'authentication',
                        'service_auth', 'upstreams', 'restart_stability'):
                self.assertIn(result[key]['result'], ('PASS', 'N/A'), (result['service'], key, result[key]))
            self.assertEqual(result['image']['imageRevision'], SHA)
            self.assertTrue(result['image']['containerLabelStale'])  # cloned label from an older image

    def test_failed_readiness_auth_and_database_are_reported_per_service(self):
        http = {':8094/ready': '{}\n503', ':8443/ready': '{}\n503', ':8101/internal/v1/admin/event-delivery': '{"ok":1}\n200'}
        with host(http=http, db_ok=False) as (fake, work, out):
            cr.run('certify', SHA, '42-1')
        by = {r['service']: r for r in records(out, 'certifyService')}
        self.assertEqual(by['platform-access']['readiness']['result'], 'FAIL')
        self.assertEqual(by['organizations']['authentication']['result'], 'FAIL')
        self.assertEqual(by['calling']['database']['result'], 'FAIL')
        self.assertEqual(by['api-gateway']['database']['result'], 'N/A')
        # An upstream that is not ready fails its callers' service-to-service check.
        self.assertEqual(by['numbering']['upstreams']['result'], 'FAIL')

    def test_unknown_revision_or_foreign_source_fails_image_identity(self):
        with host(revision=None) as (fake, work, out):
            cr.run('certify', SHA, '42-1')
        self.assertTrue(all(r['image']['result'] == 'FAIL' for r in records(out, 'certifyService')))

    def test_gateway_expects_deny_all_while_public_routing_is_off(self):
        with host(http={':8099/v1/calls': '{}\n404'}) as (fake, work, out):
            gateway_env = fake.containers['cfmeet-api-gateway']['Config']['Env']
            gateway_env.remove('API_GATEWAY_PUBLIC_ROUTING_ENABLED=true')
            cr.run('certify', SHA, '42-1')
        gateway = next(r for r in records(out, 'certifyService') if r['service'] == 'api-gateway')
        self.assertEqual(gateway['authentication']['expected'], [404])
        self.assertEqual(gateway['authentication']['result'], 'PASS')

    def test_every_listening_service_has_an_auth_probe_and_readiness_fact(self):
        for name in cr.ORDER:
            self.assertIn(name, lc.READINESS_PROVES_DB)
            if cr.SERVICES[name]['port']:
                self.assertIn(name, lc.AUTH_PROBES)
                self.assertTrue(lc.AUTH_PROBES[name].startswith('/'))

    def test_upstream_owner_resolves_private_tls_and_container_routes(self):
        from urllib.parse import urlsplit
        self.assertEqual(lc.upstream_owner(urlsplit('https://api.cfmeet.com:8443')), 'platform-access')
        with host() as (fake, work, out):
            self.assertEqual(lc.address_of('api.cfmeet.com', 'cfmeet-calling'), '10.0.0.2')
            self.assertIsNone(lc.address_of('elsewhere.example', 'cfmeet-calling'))
        self.assertEqual(lc.upstream_owner(urlsplit('https://api.cfmeet.com:8448')), 'meeting-service')
        self.assertEqual(lc.upstream_owner(urlsplit('http://cfmeet-developer-platform:8098')), 'developer-platform')
        self.assertIsNone(lc.upstream_owner(urlsplit('https://example.com:8443')))


class TrustAndIntegrations(unittest.TestCase):
    def test_activated_grants_must_be_in_the_receiver_bundle_with_the_current_key(self):
        key = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'organizations-production-1', 'x': 'AA'}
        with tempfile.TemporaryDirectory() as folder:
            bundle = Path(folder) / 'b.json'
            pem = Path(folder) / 'k.pem'
            pem.write_text('x')
            good = cr.merge_grant(cr.empty_bundle('platform-access'), 'cfmeet.organizations', key,
                                  ['platform-identity.provision-first-party'])
            bundle.write_text(json.dumps(good))
            c = container('cfmeet-platform-access', env={'CF_SERVICE_TRUST_BUNDLE_FILE': '/t', 'CF_SERVICE_AUTH_KEY_ID': 'k',
                                                          'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/k'},
                          mounts=[(str(bundle), '/t'), (str(pem), '/k')])
            env = p.env_of(c)
            with patch.object(cr, 'issuer_key', return_value=key):
                ok = lc.trust('platform-access', c, env, {'activate-provisioning'})
                self.assertEqual(ok['result'], 'PASS', ok)
            with patch.object(cr, 'issuer_key', return_value=dict(key, x='rotated')):
                stale = lc.trust('platform-access', c, env, {'activate-provisioning'})
                self.assertEqual(stale['result'], 'FAIL')
            bundle.write_text(json.dumps(cr.empty_bundle('platform-access')))
            with patch.object(cr, 'issuer_key', return_value=key):
                self.assertEqual(lc.trust('platform-access', c, env, {'activate-provisioning'})['result'], 'FAIL')
                # A grant whose activation never ran is not required.
                self.assertEqual(lc.trust('platform-access', c, env, set())['result'], 'PASS')

    def test_disabled_integrations_are_reported_as_disabled_and_secrets_never_shown(self):
        envs = {'sms-gateway': {'SMS_GATEWAY_PROVIDER': 'none'}, 'telephony': {'TELEPHONY_ENABLED': 'false'},
                'messaging': {'MESSAGING_PUSH_PROVIDER': 'fcm'},
                'meeting-service': {'SMTP_HOST': 'smtp', 'SMTP_USERNAME': 'u', 'SMTP_PASSWORD': 'hunter2'}}
        report = {(r['service'], r['variable']): r for r in lc.integrations(envs)}
        self.assertEqual(report[('sms-gateway', 'SMS_GATEWAY_PROVIDER')]['state'], 'disabled')
        self.assertEqual(report[('telephony', 'TELEPHONY_ENABLED')]['state'], 'disabled')
        self.assertEqual(report[('telephony', 'PUBLIC_INBOUND_CALLING_ENABLED')]['state'], 'disabled')  # absent = off
        self.assertEqual(report[('messaging', 'MESSAGING_PUSH_PROVIDER')]['state'], 'enabled')
        self.assertEqual(report[('meeting-service', 'SMTP_*')]['state'], 'configured')
        self.assertNotIn('hunter2', json.dumps(list(report.values())))


class VerifyStaysStrict(unittest.TestCase):
    def test_verify_still_requires_a_completed_release_of_exactly_this_sha(self):
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder)
            with patch.object(cr, 'WORK', work), patch.object(cr, 'JOURNAL', work / 'journal.json'):
                cr.commit(dict(id='release:1', operation='release', sha=OLD, runId='1', status='done', components=[]))
                with self.assertRaisesRegex(RuntimeError, 'No completed coordinated release of this SHA.*%s.*certify' % OLD):
                    cr.verify(SHA, '2')

    def test_workflow_gate_keeps_main_head_and_skips_ci_wait_only_for_read_only_operations(self):
        text = (Path(__file__).resolve().parents[3] / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('[[ "$GITHUB_REF" == refs/heads/main ]]', text)
        self.assertIn('"$OPERATION" != inspect && "$OPERATION" != rollback && "$RELEASE_SHA" != "$GITHUB_SHA"', text)
        self.assertIn("inputs.operation != 'inspect' && inputs.operation != 'rollback' && inputs.operation != 'certify'", text)
        self.assertIn('group: deploy-backend-production', text)


class ReleaseLabels(unittest.TestCase):
    def test_a_swapped_container_reports_the_release_image_revision(self):
        cloned = {'Labels': {'org.opencontainers.image.revision': OLD, 'org.opencontainers.image.source': 'x',
                             'com.example.keep': '1'}}
        running = {'State': {'Running': True}, 'Image': 'sha256:old'}
        captured = {}
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(cr, 'WORK', Path(folder)), patch.object(cr, 'JOURNAL', Path(folder) / 'j.json'), \
                patch.object(cr, 'container_names', return_value=['cfmeet-numbering']), \
                patch.object(p, 'inspect', return_value=running), patch.object(cr.release, 'clone', return_value=cloned), \
                patch.object(cr, 'swap', side_effect=lambda n, cfg, r, e: captured.update(cfg) or dict(status='swapping')), \
                patch.object(cr, 'checked'):
            entry = cr.begin('release', SHA, '7')
            cr.deploy('numbering', 'sha256:new', SHA, '7', entry)
        self.assertEqual(captured['Labels'], {'com.example.keep': '1', 'com.cfmeet.release.sha': SHA})

    def test_inspect_summary_reports_the_image_revision_not_the_stale_container_label(self):
        c = container('cfmeet-numbering', labels={'org.opencontainers.image.revision': OLD})
        meta = json.dumps([{'Config': {'Labels': {'org.opencontainers.image.revision': SHA}}}])
        with patch.object(p, 'run', return_value=meta):
            summary = cr.summarize(c)
        self.assertEqual(summary['revision'], SHA)
        self.assertEqual(summary['containerLabelRevision'], OLD)


if __name__ == '__main__':
    unittest.main()
