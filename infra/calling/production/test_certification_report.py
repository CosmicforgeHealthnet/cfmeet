"""Offline tests of public_certification.py and certification_report.py."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import certification_report as report
import coordinated_release as cr
import public_certification as pc

REPO = Path(__file__).resolve().parents[3]


class PublicSurface(unittest.TestCase):
    def test_gateway_families_match_the_activated_routing(self):
        self.assertEqual(pc.GATEWAY_PREFIXES, cr.GATEWAY_PREFIXES)

    def test_only_read_only_methods_can_be_sent(self):
        for method in ('POST', 'PUT', 'PATCH', 'DELETE'):
            with self.assertRaises(ValueError):
                pc.request('/health', method)

    def test_placeholders_replace_every_path_parameter(self):
        self.assertEqual(pc.concrete('/v1/projects/{projectId}/scopes/{id}'),
                         '/v1/projects/%s/scopes/%s' % (pc.PLACEHOLDER, pc.PLACEHOLDER))

    def test_public_route_judgement(self):
        authed = {'pattern': '/v1/calls', 'auth': ['PLATFORM_CREDENTIAL']}
        family = {'pattern': '/v1/projects/x', 'auth': ['DEVELOPER_SESSION']}
        self.assertEqual(pc.judge_public(authed, 401)[0], 'PASS')
        self.assertEqual(pc.judge_public(authed, 404)[0], 'PASS')  # not routed publicly yet
        self.assertEqual(pc.judge_public(authed, 200)[0], 'FAIL')  # auth bypass
        self.assertEqual(pc.judge_public(authed, 502)[0], 'FAIL')
        self.assertEqual(pc.judge_public(authed, 0)[0], 'FAIL')
        self.assertEqual(pc.judge_public(family, 404)[0], 'FAIL')  # activated family must route
        self.assertEqual(pc.judge_public(family, 403)[0], 'PASS')
        self.assertEqual(pc.judge_internal(200)[0], 'FAIL')
        self.assertEqual(pc.judge_internal(404)[0], 'PASS')

    def test_route_inventories_come_from_the_repository(self):
        public = pc.public_routes(REPO)
        internal = pc.internal_routes(REPO)
        self.assertGreater(len(public), 100)
        self.assertTrue(all(r['method'] == 'GET' for r in public))
        self.assertGreater(len(internal), 5)
        self.assertTrue(all(r['cls'] in pc.INTERNAL_CLASSES for r in internal))

    def test_certify_reports_failures(self):
        answers = {'/health': (200, {}, '{"status":"healthy"}'), '/api-docs/swagger-config.json': (200, {}, '{"urls":[]}'),
                   '/api/v2/me/profile': (401, {}, '')}

        def fake(path, method='GET', headers=None, attempts=4, limit=400):
            if method == 'OPTIONS':
                return 204, {'access-control-allow-origin': headers['Origin']}, ''
            return answers.get(path, (200 if path.startswith('/internal') else 401, {}, ''))

        with patch.object(pc, 'request', side_effect=fake), patch.object(pc, 'certificate_days', return_value=60):
            result = pc.certify(REPO, pace=0)
        self.assertTrue(result['failed'])
        self.assertTrue(all(f.startswith('internal ') for f in result['failed']))

    def test_gateway_must_answer_cors_for_the_www_origin(self):
        def fake(path, method='GET', headers=None, attempts=4, limit=400):
            if method == 'OPTIONS':
                # The gateway still allows only the apex origin.
                return (204, {'access-control-allow-origin': pc.WEB_ORIGIN}, '') if headers['Origin'] == pc.WEB_ORIGIN \
                    else (403, {}, '')
            if path == '/api-docs/swagger-config.json':
                return 200, {}, '{"urls":[]}'
            return (404 if path.startswith('/internal') else 401), {}, ''

        with patch.object(pc, 'request', side_effect=fake), patch.object(pc, 'certificate_days', return_value=60):
            result = pc.certify(REPO, pace=0)
        self.assertIn('gateway_cors_preflight_from_www', ' '.join(result['failed']))


def sh(cwd, *args):
    subprocess.run(args, cwd=cwd, check=True, capture_output=True)


class Report(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        repo = self.repo = Path(self.tmp.name)
        sh(repo, 'git', 'init', '-q', '-b', 'main')
        sh(repo, 'git', 'config', 'user.email', 't@example.com')
        sh(repo, 'git', 'config', 'user.name', 't')
        for name in ('numbering', 'api-gateway'):
            folder = repo / 'services' / name
            (folder / 'migration' / 'src').mkdir(parents=True)
            (folder / 'Dockerfile').write_text('FROM x AS builder\nCOPY packages/shared packages/shared\n'
                                              'COPY services/%s services/%s\nCOPY --from=builder /a /b\n' % (name, name))
        (repo / 'packages/shared').mkdir(parents=True)
        (repo / 'packages/shared/lib.rs').write_text('1')
        (repo / 'services/numbering/migration/src/m20261101_000001_first.rs').write_text('')
        sh(repo, 'git', 'add', '-A')
        sh(repo, 'git', 'commit', '-qm', 'one')
        self.first = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=repo, capture_output=True, text=True).stdout.strip()
        (repo / 'docs.md').write_text('docs only')
        sh(repo, 'git', 'add', '-A')
        sh(repo, 'git', 'commit', '-qm', 'docs')
        self.docs = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=repo, capture_output=True, text=True).stdout.strip()

    def tearDown(self):
        self.tmp.cleanup()

    def head(self):
        return subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=self.repo, capture_output=True, text=True).stdout.strip()

    def service(self, name, revision, applied):
        ok = {'result': 'PASS'}
        return dict(service=name, container=cr.SERVICES[name]['container'], verdict=None,
                    container_state=ok, restart_stability=ok, readiness=ok, database=ok, authentication=ok,
                    service_auth=ok, upstreams=ok, image=dict(ok, imageRevision=revision),
                    migrations=dict(ok, applied={'numbering_seaql_migrations': applied}))

    def test_revision_lineage_source_equivalence_and_migrations(self):
        s = self.service('numbering', self.first, ['m20261101_000001_first'])
        checks = report.repository_checks(self.repo, self.docs, s)
        self.assertEqual(checks['revision_on_main']['result'], 'PASS')
        self.assertEqual(checks['source_matches_main']['result'], 'PASS')  # docs-only change
        self.assertEqual(checks['migrations_complete']['result'], 'PASS')
        (self.repo / 'packages/shared/lib.rs').write_text('2')
        (self.repo / 'services/numbering/migration/src/m20261102_000002_second.rs').write_text('')
        sh(self.repo, 'git', 'add', '-A')
        sh(self.repo, 'git', 'commit', '-qm', 'code')
        checks = report.repository_checks(self.repo, self.head(), s)
        self.assertEqual(checks['source_matches_main']['result'], 'FAIL')
        self.assertIn('packages/shared/lib.rs', checks['source_matches_main']['changedFiles'])
        # Migrations are judged against the running revision, not the head.
        self.assertEqual(checks['migrations_complete']['result'], 'PASS')
        s_missing = self.service('numbering', self.first, [])
        self.assertEqual(report.repository_checks(self.repo, self.docs, s_missing)['migrations_complete']['missing'],
                         ['m20261101_000001_first'])

    def test_unknown_or_off_main_revisions_fail(self):
        s = self.service('numbering', 'f' * 40, [])
        self.assertTrue(all(c['result'] == 'FAIL' for c in report.repository_checks(self.repo, self.docs, s).values()))
        sh(self.repo, 'git', 'checkout', '-qb', 'side', self.first)
        (self.repo / 'x').write_text('x')
        sh(self.repo, 'git', 'add', '-A')
        sh(self.repo, 'git', 'commit', '-qm', 'side')
        side = self.head()
        s = self.service('numbering', side, ['m20261101_000001_first'])
        self.assertEqual(report.repository_checks(self.repo, self.docs, s)['revision_on_main']['result'], 'FAIL')

    def write_log(self, services, platform, collected=True, failure=None):
        path = self.repo / 'log'
        lines = [json.dumps({'certifyStarted': {'referenceSha': self.docs}})]
        lines += [json.dumps({'certifyService': s}) for s in services]
        if platform is not None:
            lines.append(json.dumps({'certifyPlatform': platform}))
        if collected:
            lines.append(json.dumps({'certifyCollected': {}}))
        if failure:
            lines.append(failure)
        path.write_text('\n'.join(lines) + '\n')
        return path

    def platform(self):
        ok = {'result': 'PASS'}
        return dict(providerSafeDefaults=ok, organizationsEventDelivery=ok, coreIdentity=ok,
                    publicSmokeFromVps={'public_swagger': 'PASS'}, integrations=[
                        dict(service='telephony', variable='TELEPHONY_ENABLED', integration='PSTN', value='false', state='disabled')])

    def test_verdicts(self):
        api = self.service('api-gateway', self.first, [])
        api['migrations'] = {'result': 'N/A', 'reason': 'no db'}
        services = []
        for name in cr.ORDER:
            s = api if name == 'api-gateway' else self.service('numbering', self.first, ['m20261101_000001_first'])
            s = dict(s, service=name, container=cr.SERVICES[name]['container'])
            services.append(s)
        with patch.object(cr, 'image_of', return_value='numbering'):
            ok = report.evaluate(self.repo, self.docs, self.write_log(services, self.platform()), {'failed': []})
            self.assertEqual(ok['verdict'], 'CERTIFIED_READ_ONLY', ok['failures'])
            self.assertTrue(ok['outstanding'])  # authenticated journeys are always listed
            md = report.render(ok, 'https://example.invalid/run')
            self.assertIn('Verdict: **CERTIFIED_READ_ONLY**', md)
            self.assertIn('| telephony | PSTN | `TELEPHONY_ENABLED`=false | disabled |', md)
            bad = report.evaluate(self.repo, self.docs, self.write_log(services, self.platform()),
                                  {'failed': ['public /v1/x answered']})
            self.assertEqual(bad['verdict'], 'NOT_CERTIFIED')
            short = report.evaluate(self.repo, self.docs, self.write_log(services[:-1], self.platform()), {'failed': []})
            self.assertEqual(short['verdict'], 'INCOMPLETE')
            broken = report.evaluate(self.repo, self.docs, self.write_log(
                services[:2], None, collected=False, failure='COORDINATED_RELEASE_FAILED: RuntimeError x'), None)
            self.assertEqual(broken['verdict'], 'INCOMPLETE')
            self.assertIn('public certification did not run', broken['failures'])
            services[0]['readiness'] = {'result': 'FAIL', 'status': 503}
            failed = report.evaluate(self.repo, self.docs, self.write_log(services, self.platform()), {'failed': []})
            self.assertEqual(failed['verdict'], 'NOT_CERTIFIED')
            self.assertIn('platform-access: readiness', failed['failures'])

    def test_build_inputs_ignore_multi_stage_copies(self):
        inputs = report.build_inputs(self.repo, 'numbering', self.docs)
        self.assertIn('packages/shared', inputs)
        self.assertIn('services/numbering', inputs)
        self.assertNotIn('/a', inputs)

    def test_real_meeting_service_inputs_and_migrations(self):
        head = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=REPO, capture_output=True, text=True).stdout.strip()
        inputs = report.build_inputs(REPO, 'meeting-service', head)
        self.assertIn('services/meeting-service', inputs)
        self.assertIn('packages/service-auth', inputs)
        names = report.expected_migrations(REPO, 'communications', head)
        self.assertIn('m20261103_000001_foundation', names)


if __name__ == '__main__':
    unittest.main()
