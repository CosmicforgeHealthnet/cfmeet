"""Offline tests: developer console, customer webhooks, gateway families, safe defaults,
the off-host backup record (activation steps added for the all-features activation)."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import offsite_backup as ob
from test_coordinated_release import REPO, isolated_work

MANIFEST = json.loads((REPO / 'infra/api-gateway/route-manifest.json').read_text())
EXCLUDED = ('/v1/numbers', '/v1/sms', '/v1/privacy-relay')
DIRECT = ('/v1/calls', '/v1/calling', '/v1/enterprise', '/v1/realtime')


def journal(work, *operations):
    (work / 'journal.json').write_text(json.dumps(
        [dict(id='%s:%d' % (o, i), operation=o, sha='a' * 40, status='done', components=[]) for i, o in enumerate(operations)]))


class SafeDefaults(unittest.TestCase):
    def test_active_webhook_delivery_only_after_its_activation_and_on_the_worker_too(self):
        envs = {'developer-platform': {'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': 'active'},
                'developer-platform-worker': {'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': 'active'}}
        with isolated_work() as work:
            self.assertEqual(sorted(cr.safe_default_violations(envs)),
                             ['developer-platform DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE',
                              'developer-platform-worker DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'])
            journal(work, 'activate-developer-webhooks')
            self.assertEqual(cr.safe_default_violations(envs), [])

    def test_auto_approval_and_providers_stay_fixed_after_every_activation(self):
        with isolated_work() as work:
            journal(work, *cr.ACTIVATIONS)
            envs = {'developer-platform-worker': {'DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL': 'true'},
                    'messaging': {'MESSAGING_E2EE_DEVICES_ENABLED': 'true', 'MESSAGING_PUSH_PROVIDER': 'fcm'},
                    'sms-gateway': {'SMS_GATEWAY_PROVIDER': 'africastalking'},
                    'telephony': {'TELEPHONY_ENABLED': 'true'}, 'privacy-relay': {'PRIVACY_RELAY_ENABLED': 'true'},
                    'numbering': {'NUMBERING_PROVIDERS': 'x'}}
            self.assertEqual(len(cr.safe_default_violations(envs)), 7)


    def test_push_adapter_credentials_are_refused_until_push_is_approved(self):
        with isolated_work():
            envs = {'messaging': {'MESSAGING_PUSH_PROVIDER': 'none', 'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': '/k',
                                  'MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE': '/f', 'MESSAGING_PUSH_APNS_TOPIC': ''}}
            self.assertEqual(cr.safe_default_violations(envs),
                             ['messaging MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE',
                              'messaging MESSAGING_PUSH_WEB_VAPID_KEY_FILE'])


class GatewayFamilies(unittest.TestCase):
    def test_every_family_is_a_routed_public_customer_rest_family_of_calling_messaging_or_developer_platform(self):
        upstreams = {'cfmeet.calling', 'cfmeet.messaging', 'cfmeet.developer-platform'}
        for prefix in cr.FAMILY_PREFIXES:
            routes = [r for r in MANIFEST['routes'] if r['pattern'] == prefix or r['pattern'].startswith(prefix + '/')]
            self.assertTrue(routes, prefix)
            for r in routes:
                self.assertEqual(r['protocol'], 'REST', r['id'])
                self.assertIn(r['upstream'], upstreams, r['id'])

    def test_families_never_cover_excluded_internal_or_already_direct_routes(self):
        for prefix in cr.FAMILY_PREFIXES:
            for other in EXCLUDED + DIRECT:
                self.assertFalse(prefix == other or prefix.startswith(other + '/') or other.startswith(prefix + '/'))
            for word in ('internal', 'admin', 'operator', 'callback', 'metrics'):
                self.assertNotIn(word, prefix)
        self.assertFalse(set(cr.FAMILY_PREFIXES) & set(cr.GATEWAY_PREFIXES))
        self.assertEqual(set(cr.DISABLED_DOMAINS.split(',')), {'numbering', 'sms', 'privacy-relay'})

    def test_every_family_has_a_probe_and_the_snippet_routes_both_sets(self):
        probed = {next(p for p in cr.FAMILY_PREFIXES if path == p or path.startswith(p + '/')) for _, path in cr.FAMILY_PROBES}
        self.assertEqual(probed, set(cr.FAMILY_PREFIXES))
        text = cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES)
        for prefix in cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES:
            self.assertIn('location ^~ %s/ {' % prefix, text)
        self.assertFalse([l for l in text.splitlines() if l.startswith('location') and '/internal' in l])
        self.assertEqual(cr.gateway_snippet(), cr.gateway_snippet(cr.GATEWAY_PREFIXES))

    def test_existing_routes_for_a_family_are_refused(self):
        site = 'location ^~ /v1/conversations/ { proxy_pass http://x; }\nlocation ~ ^/v1/calls(?:/|$) {}'
        self.assertEqual(cr.route_collisions([site], cr.FAMILY_PREFIXES), ['/v1/conversations'])

    def test_gateway_families_require_webhooks_and_routing(self):
        self.assertEqual(set(cr.ACTIVATIONS['activate-gateway-families'][0]),
                         {'activate-gateway-routing', 'activate-developer-webhooks'})
        self.assertEqual(cr.ACTIVATIONS['activate-developer-webhooks'][0], ('activate-developer-console',))


class GatewayDocs(unittest.TestCase):
    """activate-gateway-docs: the gateway's public documentation at the api.cfmeet.com edge."""

    DISCOVERY = {'service': 'cfmeet.api-gateway', 'documentation': {'aggregate': '/openapi.json', 'reference': '/docs'},
                 'families': [{'domain': 'calling', 'enabled': True}, {'domain': 'messaging', 'enabled': True},
                              {'domain': 'numbering', 'enabled': False}, {'domain': 'sms', 'enabled': False},
                              {'domain': 'privacy-relay', 'enabled': False}]}

    def test_paths_are_exactly_what_the_gateway_serves_publicly(self):
        source = (REPO / 'services/api-gateway/src/http/mod.rs').read_text()
        for path in cr.DOCS_EXACT:
            self.assertIn('.route("%s", get(' % path, source)
        self.assertIn('.route("/openapi/:file", get(', source)
        self.assertEqual(cr.DOCS_PREFIX, '/openapi')
        # Never the gateway's health / readiness or any /v1, internal or operator path.
        for path in cr.DOCS_EXACT + (cr.DOCS_PREFIX,):
            self.assertFalse(path.startswith('/v1') or path in ('/ready', '/health'))
            for word in ('internal', 'admin', 'operator', 'metrics'):
                self.assertNotIn(word, path)

    def test_snippet_adds_read_only_documentation_locations_and_keeps_every_family(self):
        prefixes = cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES
        with_docs, without = cr.gateway_snippet(prefixes, docs=True), cr.gateway_snippet(prefixes)
        self.assertTrue(with_docs.startswith(without.rstrip('\n')))
        for path in cr.DOCS_EXACT:
            self.assertIn('location = %s {' % path, with_docs)
            self.assertNotIn('location = %s {' % path, without)
        self.assertIn('location ^~ /openapi/ {', with_docs)
        docs_part = with_docs[len(without):]
        self.assertEqual(docs_part.count('limit_except GET OPTIONS { deny all; }'), 4)
        self.assertNotIn('/ready', with_docs)
        self.assertNotIn('/health', with_docs)

    def test_existing_documentation_locations_are_refused(self):
        paths = cr.DOCS_EXACT + (cr.DOCS_PREFIX,)
        self.assertEqual(cr.route_collisions(['location /api-docs/ { }', 'location = /docsx { }',
                                              'location /openapi.jsonx { }'], paths), [])
        self.assertEqual(cr.route_collisions(['location = /docs {', 'location ^~ /openapi/ {'], paths),
                         ['/docs', '/openapi'])

    def test_requires_gateway_routing_and_is_listed_in_the_workflow(self):
        self.assertEqual(cr.ACTIVATIONS['activate-gateway-docs'][0], ('activate-gateway-routing',))
        self.assertIn('activate-gateway-docs', cr.OPERATIONS)

    def run_activation(self, work, discovery=None, statuses=None, cors=cr.WEB_ORIGINS):
        snippets = work / 'snippets'
        snippets.mkdir()
        (snippets / 'cfmeet-api-gateway.conf').write_text(cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES))
        site = work / 'site'
        site.write_text('server { %s }' % cr.GATEWAY_INCLUDE)
        statuses = dict({'/v1/developer-platform/context': 401, '/ready': 404, '/health': 404, 'POST /discovery': 403},
                        **(statuses or {}))
        discovery = self.DISCOVERY if discovery is None else discovery

        def document(url, headers=None, limit=0):
            path = url[len(cr.PUBLIC):]
            origin = (headers or {}).get('Origin')
            h = {'Access-Control-Allow-Origin': origin} if origin in cors else {}
            if path == '/discovery':
                return 200, h, json.dumps(discovery).encode()
            if path == '/openapi.json':
                return 200, h, json.dumps({'openapi': '3.1.0', 'paths': {'/v1/calls': {}}}).encode()
            if path == '/docs':
                return 200, {'Content-Type': 'text/html; charset=utf-8'}, b'<div id="swagger-ui"></div>'
            if path.startswith('/openapi/'):
                return statuses.get(path, 200), h, b'{}'
            return 404, {}, b''

        def status(url, method='GET', headers=None):
            path = url[len(cr.PUBLIC):]
            return statuses.get(('POST ' + path) if method == 'POST' else path, 200), {}

        reloads = []
        with patch.object(cr, 'GATEWAY_SNIPPET', snippets / 'cfmeet-api-gateway.conf'), patch.object(cr, 'SITE', site), \
                patch.object(cr, 'Path', lambda p: snippets if p == '/etc/nginx/snippets' else Path(p)), \
                patch.object(cr, 'env_of', lambda n: {'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'true'}), \
                patch.object(cr, 'http_document', document), patch.object(cr, 'http_status', status), \
                patch.object(cr, 'public_smoke', lambda: {'public_swagger': 'PASS'}), \
                patch.object(cr.p, 'reload_host_nginx', lambda: reloads.append(1)), \
                patch.object(cr.p, 'regression', lambda: None):
            try:
                cr.activate_gateway_docs('a' * 40, '9-1', '')
                error = None
            except RuntimeError as e:
                error = e
            text = (snippets / 'cfmeet-api-gateway.conf').read_text()
        return error, text, json.loads((work / 'journal.json').read_text())[-1], reloads

    def test_activation_routes_documentation_keeps_families_and_journals_the_edge_step(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-routing', 'activate-gateway-families')
            error, text, entry, reloads = self.run_activation(work)
        self.assertIsNone(error)
        self.assertEqual(text, cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES, docs=True))
        self.assertEqual((entry['operation'], entry['status'], entry['components']), ('activate-gateway-docs', 'done', []))
        self.assertTrue(entry['edge']['docs'])
        self.assertEqual(len(reloads), 1)

    def test_any_failed_check_restores_the_previous_snippet(self):
        before = cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES)
        cases = [dict(discovery=dict(self.DISCOVERY, families=[{'domain': 'sms', 'enabled': True}])),
                 dict(statuses={'/ready': 200}), dict(statuses={'POST /discovery': 200}),
                 dict(statuses={'/v1/developer-platform/context': 404}),
                 dict(cors=cr.WEB_ORIGINS + ('https://evil.example',)), dict(cors=('https://cfmeet.com',)),
                 dict(statuses={'/openapi/calling.json': 404})]
        for case in cases:
            with isolated_work() as work:
                journal(work, 'activate-gateway-routing', 'activate-gateway-families')
                error, text, entry, reloads = self.run_activation(work, **case)
            self.assertRegex(str(error), 'documentation checks failed', case)
            self.assertEqual(text, before, case)
            self.assertEqual(entry['status'], 'failed', case)
            self.assertEqual(len(reloads), 2, case)

    def test_refuses_to_shadow_an_existing_route_before_changing_anything(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-routing')
            (work / 'snippets').mkdir()
            (work / 'snippets' / 'cfmeet-calling-routes.conf').write_text('location = /docs { return 200; }')
            snippet = work / 'snippets' / 'cfmeet-api-gateway.conf'
            snippet.write_text(cr.gateway_snippet())
            site = work / 'site'
            site.write_text('server { %s }' % cr.GATEWAY_INCLUDE)
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), patch.object(cr, 'SITE', site), \
                    patch.object(cr, 'Path', lambda p: work / 'snippets' if p == '/etc/nginx/snippets' else Path(p)), \
                    patch.object(cr, 'env_of', lambda n: {'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'true'}):
                with self.assertRaisesRegex(RuntimeError, 'already serve: /docs'):
                    cr.activate_gateway_docs('a' * 40, '9-1', '')
            self.assertEqual(snippet.read_text(), cr.gateway_snippet())

    def test_families_rollback_keeps_the_documentation_and_docs_rollback_keeps_the_families(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-routing', 'activate-gateway-families', 'activate-gateway-docs')
            snippet = work / 'gateway.conf'
            snippet.write_text('x')
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), patch.object(cr.p, 'reload_host_nginx', lambda: None):
                cr.undo_gateway_docs({'docs': True})
                self.assertEqual(snippet.read_text(), cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES))
        src = (REPO / 'infra/calling/production/coordinated_release.py').read_text()
        self.assertIn("gateway_snippet(GATEWAY_PREFIXES, docs=done('activate-gateway-docs'))", src)


class DeveloperWebhooks(unittest.TestCase):
    def plan(self, calling_state='0,0', migration='0'):
        captured = {}
        sql = {'cfmeet_calling': calling_state, 'cfmeet_developer_platform': migration}
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(cr.p, 'SECRETS', Path(folder)), \
                patch.object(cr, 'env_of', lambda n: {'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'true'}), \
                patch.object(cr, 'tls_route_present', lambda port: None), \
                patch.object(cr.release, 'db_sql', lambda db, q: sql[db]), \
                patch.object(cr, 'webhook_egress_network', lambda: cr.WEBHOOK_EGRESS_NET), \
                patch.object(cr.os, 'chown', lambda *a: None), \
                patch.object(cr, 'apply_activation', lambda op, sha, run, changes, **k: captured.update(op=op, changes=changes)):
            cr.activate_developer_webhooks('a' * 40, '1-1', '')
            key = (Path(folder) / cr.WEBHOOK_KEY)
            captured['key'] = key.read_text().strip()
            captured['mode'] = oct(key.stat().st_mode & 0o777)
        return captured

    def test_plan_signs_delivers_from_the_worker_only_and_retires_calling(self):
        out = self.plan()
        import base64
        self.assertEqual(len(base64.b64decode(out['key'])), 32)
        self.assertEqual(out['mode'], '0o400')
        changes = {c[0]: c for c in out['changes']}
        self.assertEqual(list(changes), ['developer-platform', 'developer-platform-worker', 'calling', 'messaging',
                                         'meeting-service'])
        for name in ('developer-platform', 'developer-platform-worker'):
            env = changes[name][1]
            self.assertEqual(env['DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'], 'active')
            self.assertEqual(env['DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE'], cr.WEBHOOK_KEY_TARGET)
            self.assertNotIn('DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL', env)
            self.assertNotIn('DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS', env)
        self.assertEqual(changes['developer-platform-worker'][2]['networks'], [cr.WEBHOOK_EGRESS_NET])
        self.assertNotIn('networks', changes['developer-platform'][2])
        self.assertEqual(changes['calling'][1]['CALLING_WEBHOOK_AUTHORITY'], 'retired')
        self.assertEqual(changes['calling'][1]['CALLING_WEBHOOK_DELIVERY_ENABLED'], 'false')
        for name in ('calling', 'messaging', 'meeting-service'):
            self.assertTrue(changes[name][1]['CF_DEVELOPER_PLATFORM_BASE_URL'].startswith('https://'))

    def test_calling_customer_state_or_a_started_migration_refuses(self):
        with self.assertRaisesRegex(RuntimeError, 'migration runbook'):
            self.plan(calling_state='2,0')
        with self.assertRaisesRegex(RuntimeError, 'migration runbook'):
            self.plan(calling_state='0,1')
        with self.assertRaisesRegex(RuntimeError, 'migration'):
            self.plan(migration='1')

    def test_recreate_adds_mounts_and_networks(self):
        c = {'Config': {'Env': ['A=1']}, 'Image': 'img', 'HostConfig': {'Mounts': []}, 'Id': 'x' * 64,
             'NetworkSettings': {'Networks': {'cfmeet-core-private': {'Aliases': ['w']}}}}
        seen = {}
        with patch.object(cr.p, 'inspect', lambda n: c), patch.object(cr.p, 'env_of', lambda c_: {'A': '1'}), \
                patch.object(cr, 'swap', lambda name, cfg, run, entry: seen.update(cfg=cfg) or 'component'), \
                patch.object(cr, 'checked', lambda *a: None):
            cr.recreate('developer-platform-worker', {'B': '2'}, None, '1-1', {},
                        {'mounts': [(Path('/k'), '/run/secrets/k')], 'networks': ['egress']})
        cfg = seen['cfg']
        self.assertIn({'Type': 'bind', 'Source': '/k', 'Target': '/run/secrets/k', 'ReadOnly': True},
                      cfg['HostConfig']['Mounts'])
        self.assertEqual(set(cfg['NetworkingConfig']['EndpointsConfig']), {'cfmeet-core-private', 'egress'})
        self.assertIn('B=2', cfg['Env'])


class UsageExport(unittest.TestCase):
    def test_usage_export_switches_only_the_three_producers_and_needs_their_targets(self):
        captured = {}
        envs = {n: {'CF_DEVELOPER_PLATFORM_BASE_URL': 'https://x'} for n in ('calling', 'messaging', 'meeting-service')}
        with patch.object(cr, 'env_of', lambda n: envs[n]), \
                patch.object(cr, 'apply_activation', lambda op, sha, run, changes, **k: captured.update(changes=changes)):
            cr.activate_usage_export('a' * 40, '1-1', '')
        self.assertEqual(captured['changes'], [(n, {'CFMEET_USAGE_EXPORT_ENABLED': 'true'})
                                               for n in ('calling', 'messaging', 'meeting-service')])
        envs['messaging'] = {}
        with patch.object(cr, 'env_of', lambda n: envs[n]), self.assertRaisesRegex(RuntimeError, 'messaging'):
            cr.activate_usage_export('a' * 40, '1-1', '')
        self.assertEqual(cr.ACTIVATIONS['activate-usage-export'][0], ('activate-developer-webhooks',))


class OffsiteBackup(unittest.TestCase):
    RECIPIENT = 'age1' + 'q' * 58

    def test_recipients_refuse_private_keys_and_garbage(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'r.txt'
            path.write_text('# owner\n%s\n' % self.RECIPIENT)
            self.assertEqual(ob.recipients(path), [self.RECIPIENT])
            for bad in ('AGE-SECRET-KEY-1QQQ\n', '-----BEGIN PRIVATE KEY-----\n', 'ssh-ed25519 AAAA\n', ''):
                path.write_text(bad)
                with self.assertRaises(RuntimeError):
                    ob.recipients(path)

    def setup_run(self, work):
        folder = work / 'backups' / '7-1' / 'offsite'
        folder.mkdir(parents=True)
        files = []
        for label in ('meeting-service', 'organizations', 'platform-access'):
            f = folder / (label + '.dump.age')
            f.write_bytes(b'age-encryption.org/v1\n' + label.encode())
            files.append(dict(label=label, file=f.name, sha256=cr.sha256(f), histories={'h': 3}))
        (folder / 'offsite-manifest.json').write_text(json.dumps({'files': files}))
        return files

    def evidence(self, work, files, **over):
        doc = dict(runId='7-1', artifactId=99, runUrl='https://github.com/x/actions/runs/1', retentionDays=90,
                   offHostFiles=[{'file': f['file'], 'sha256': f['sha256']} for f in files],
                   drills=[{'label': f['label'], 'result': 'PASS', 'histories': {'h': 3}, 'tables': 5} for f in files])
        doc.update(over)
        path = work / 'evidence.json'
        path.write_text(json.dumps(doc))
        return path

    def test_record_requires_matching_hashes_and_passing_drills(self):
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            files = self.setup_run(work)
            self.assertIsNone(ob.verified('7-1'))
            bad_hash = [{'file': f['file'], 'sha256': '0' * 64} for f in files]
            failing = [{'label': f['label'], 'result': 'FAIL'} for f in files]
            drifted = [{'label': f['label'], 'result': 'PASS', 'histories': {'h': 2}} for f in files]
            for over in (dict(offHostFiles=bad_hash), dict(drills=failing), dict(drills=drifted),
                         dict(runId='8-1'), dict(artifactId=None)):
                with self.assertRaises(RuntimeError):
                    ob.record('7-1', self.evidence(work, files, **over))
            self.assertIsNone(ob.verified('7-1'))
            ob.record('7-1', self.evidence(work, files))
            doc = ob.verified('7-1')
            self.assertEqual(doc['status'], ob.OFFSITE_STATUS)
            self.assertEqual(doc['artifactId'], 99)

    def test_prune_touches_only_old_offsite_runs_beyond_the_newest_seven(self):
        import os
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            base = 1_000_000_000
            for i in range(10):
                d = work / 'backups' / ('%d-1' % i) / 'offsite'
                d.mkdir(parents=True)
                (d / 'offsite-manifest.json').write_text('{}')
                os.utime(d.parent, (base + i * 86400, base + i * 86400))
            release = work / 'backups' / 'release-1'
            release.mkdir()
            os.utime(release, (base, base))
            removed = ob.prune(now=base + 30 * 86400)
            self.assertEqual(sorted(removed), ['0-1', '1-1', '2-1'])
            self.assertTrue(release.exists())
            self.assertEqual(ob.prune(now=base + 30 * 86400), [])

    def test_import_gate_requires_the_offsite_record(self):
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            files = self.setup_run(work)
            with self.assertRaisesRegex(RuntimeError, 'off-host'):
                cr.require_offsite({'runId': '7-1'})
            ob.record('7-1', self.evidence(work, files))
            self.assertEqual(cr.require_offsite({'runId': '7-1'})['artifactId'], 99)



class WebOrigins(unittest.TestCase):
    def preflight(self, allowed):
        def fake(url, method='GET', headers=None):
            origin = headers['Origin']
            return (204, {'Access-Control-Allow-Origin': origin}) if origin in allowed else (403, {})
        return fake

    def test_gateway_allows_both_production_web_origins_and_never_a_wildcard(self):
        self.assertEqual(cr.WEB_ORIGINS, ('https://cfmeet.com', 'https://www.cfmeet.com'))
        self.assertEqual(cr.SERVICES['api-gateway']['env']['API_GATEWAY_CORS_ALLOWED_ORIGINS'],
                         'https://cfmeet.com,https://www.cfmeet.com')
        self.assertEqual(cr.ACTIVATIONS['activate-web-origins'][0], ('activate-gateway-routing',))

    def test_activation_recreates_only_the_gateway_and_verifies_cors(self):
        applied = []

        def apply(operation, sha, run_id, changes, irreversible=False, post=None):
            applied.append((operation, changes, irreversible))
            post()

        with patch.object(cr, 'apply_activation', apply), \
                patch.object(cr, 'http_status', self.preflight(cr.WEB_ORIGINS)):
            cr.activate_web_origins('a' * 40, '3-1', '')
        self.assertEqual(applied, [('activate-web-origins', [('api-gateway', {
            'API_GATEWAY_CORS_ALLOWED_ORIGINS': 'https://cfmeet.com,https://www.cfmeet.com'})], False)])

    def test_post_check_fails_when_www_is_refused_or_a_foreign_origin_is_allowed(self):
        def apply(operation, sha, run_id, changes, irreversible=False, post=None):
            post()

        for allowed in (('https://cfmeet.com',), cr.WEB_ORIGINS + ('https://evil.example',)):
            with patch.object(cr, 'apply_activation', apply), patch.object(cr, 'http_status', self.preflight(allowed)):
                with self.assertRaisesRegex(RuntimeError, 'CORS'):
                    cr.activate_web_origins('a' * 40, '3-1', '')



class OrganizationBindingsActivationTest(unittest.TestCase):
    """ADR 0058 Amendment A2: automatic Calling / Messaging bindings."""

    def test_grant_is_the_narrow_binding_permission_for_organizations_only(self):
        rows = [g for g in cr.GRANTS if g[0] == 'activate-organization-bindings']
        self.assertEqual(rows, [('activate-organization-bindings', 'platform-access', 'organizations',
                                 ('platform-identity.bind-organization-services',))])
        # Never operator administration for a service.
        self.assertFalse([g for g in cr.GRANTS if g[2] == 'organizations' and 'platform-identity.administer' in g[3]])

    def _env(self, provisioning='true'):
        return lambda name: {'ORGANIZATIONS_PROVISIONING_ENABLED': provisioning}

    def test_activation_switches_only_the_organizations_flag_and_checks_failed_workflows(self):
        applied = []

        def apply(operation, sha, run_id, changes, irreversible=False, post=None):
            applied.append((operation, changes, irreversible))
            post()

        answers = iter(['1', '{"failed": 0, "automaticBindings": 4}'])
        with patch.object(cr, 'has_signing_identity', lambda name: None), \
                patch.object(cr, 'env_of', self._env()), \
                patch.object(cr.release, 'db_sql', lambda db, sql: next(answers)), \
                patch.object(cr.time, 'sleep', lambda s: None), \
                patch.object(cr, 'apply_activation', apply):
            cr.activate_organization_bindings('a' * 40, '3-1', '')
        self.assertEqual(applied, [('activate-organization-bindings', [
            ('organizations', {'ORGANIZATIONS_SERVICE_BINDINGS_ENABLED': 'true'})], False)])

    def test_refused_without_provisioning_or_the_migration_and_failed_after_failed_workflows(self):
        def apply(operation, sha, run_id, changes, irreversible=False, post=None):
            post()

        with patch.object(cr, 'has_signing_identity', lambda name: None), \
                patch.object(cr, 'env_of', self._env('false')):
            with self.assertRaisesRegex(RuntimeError, 'provisioning'):
                cr.activate_organization_bindings('a' * 40, '3-1', '')
        with patch.object(cr, 'has_signing_identity', lambda name: None), \
                patch.object(cr, 'env_of', self._env()), \
                patch.object(cr.release, 'db_sql', lambda db, sql: '0'):
            with self.assertRaisesRegex(RuntimeError, 'migration'):
                cr.activate_organization_bindings('a' * 40, '3-1', '')
        answers = iter(['1', '{"failed": 1, "automaticBindings": 0}'])
        with patch.object(cr, 'has_signing_identity', lambda name: None), \
                patch.object(cr, 'env_of', self._env()), \
                patch.object(cr.release, 'db_sql', lambda db, sql: next(answers)), \
                patch.object(cr.time, 'sleep', lambda s: None), \
                patch.object(cr, 'apply_activation', apply):
            with self.assertRaisesRegex(RuntimeError, 'FAILED'):
                cr.activate_organization_bindings('a' * 40, '3-1', '')


if __name__ == '__main__':
    unittest.main()
