"""Offline tests: CF Meet Apps ("Integrate") activations (integrate_operations.py, ADR 0064 / 0065)."""
import contextlib
import io
import json
import re
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import coordinated_release as cr
import integrate_operations as io_ops
import platform_acceptance
import platform_operations as po
from test_coordinated_release import REPO, isolated_work, permission_registry
import test_platform_operations as tpo
from test_platform_operations import Capture, journal

SHA = 'b' * 40
REVIEWERS = 'github-alice=11111111-2222-4333-8444-555555555555, github-bob=66666666-7777-4888-9999-000000000000'


def envs(**overrides):
    base = {
        'api-gateway': {'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'true', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x:8443',
                        'API_GATEWAY_DISABLED_DOMAINS': cr.DISABLED_DOMAINS},
        'developer-platform': {'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'true', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x',
                               'CF_MEETING_SERVICE_BASE_URL': 'https://x:8448'},
        'developer-platform-worker': {},
        'meeting-service': {'CF_SERVICE_TRUST_BUNDLE_FILE': '/run/trust.json', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x',
                            'CFMEET_COMMUNICATION_ENVIRONMENT_ID': 'e', 'MEETING_PLATFORM_ACCESS_ENABLED': 'true',
                            'MEETING_OAUTH_AUTHORIZE_ENABLED': 'true', 'CF_ORGANIZATIONS_BASE_URL': 'https://x:8450'},
        'messaging': {'CF_PLATFORM_ACCESS_BASE_URL': 'https://x', 'MESSAGING_PUBLIC_EVENTS_DESTINATION': 'developer-platform'},
        'organizations': {'CF_SERVICE_TRUST_BUNDLE_FILE': '/run/trust.json'},
        'platform-access': {},
    }
    for name, values in overrides.items():
        base[name.replace('_', '-')] = values
    return base


@contextlib.contextmanager
def plan_env(values=None, done=()):
    values = values or envs()
    with patch.object(cr, 'env_of', lambda name: values[name]), \
            patch.object(cr, 'tls_route_present', lambda port: None), \
            patch.object(cr, 'has_signing_identity', lambda name: None), \
            patch.object(cr, 'done', lambda op: op in done), \
            patch.object(po, 'require_gateway_edge', lambda c: None), \
            patch.object(po, 'refuse_collisions', lambda c, p: None), \
            contextlib.redirect_stdout(io.StringIO()) as out:
        yield out


def must_not_apply():
    return patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply'))


class Registration(unittest.TestCase):
    def test_every_operation_is_registered_with_its_prerequisites(self):
        for op in io_ops.OPERATIONS:
            self.assertIn(op, cr.ACTIVATIONS)
            self.assertEqual(cr.ACTIVATIONS[op][0], io_ops.REQUIRES[op])
            self.assertIn(op, cr.OPERATIONS)
            self.assertRegex(op, r'^[a-z][a-z-]{1,40}$')  # the workflow's input rule
            self.assertNotIn(op, po.OPERATIONS)
        self.assertRegex(io_ops.NO_REVIEWERS_CONFIRM, r'^[a-z0-9,=-]{0,120}$')
        self.assertEqual(tuple(io_ops.FEATURES[op] for op in io_ops.OPERATIONS), platform_acceptance.APP_FEATURES)
        # I1 after the console and the Build users phase; everything else after I1; delegated
        # access after the partner meetings family; installs after Organizations is authoritative.
        self.assertEqual(io_ops.REQUIRES[io_ops.OAUTH_APPS], ('activate-developer-console', po.USERS))
        for op in io_ops.OPERATIONS[1:]:
            self.assertTrue({io_ops.OAUTH_APPS, io_ops.OAUTH} & set(io_ops.REQUIRES[op]), op)
        self.assertIn(po.MEETINGS, io_ops.REQUIRES[io_ops.DELEGATED])
        self.assertIn(('org-cutover-authoritative', 'org-cutover-first-activation'), io_ops.REQUIRES[io_ops.INSTALLS])
        self.assertIn('activate-developer-webhooks', io_ops.REQUIRES[io_ops.EVENTS])
        # The Integrate operations come after the Build ones in the dispatcher (workflow order).
        ops = list(cr.OPERATIONS)
        self.assertLess(ops.index(po.MEETINGS), ops.index(io_ops.OAUTH_APPS))
        self.assertLess(ops.index(io_ops.IN_MEETING), ops.index('developer-acceptance'))

    def test_grants_match_the_contract_and_the_permission_registry(self):
        self.assertEqual(cr.validate_grants(permission_registry(), io_ops.GRANTS), [])
        rows = {(op, receiver, issuer): set(perms) for op, receiver, issuer, perms in io_ops.GRANTS}
        self.assertEqual(rows, {
            (io_ops.OAUTH_APPS, 'platform-access', 'developer-platform'):
                {'platform-oauth.manage', 'platform-oauth.token', 'platform-identity.read'},
            (io_ops.OAUTH, 'platform-access', 'meeting-service'): {'platform-oauth.authorize'},
            (io_ops.OAUTH, 'meeting-service', 'developer-platform'): {'meeting-oauth-userinfo.read'},
            (io_ops.INSTALLS, 'organizations', 'meeting-service'): {'organization-apps.evaluate'}})
        for row in io_ops.GRANTS:
            self.assertIn(row, cr.GRANTS)
        # Every permission of contract §1 is granted by exactly these operations.
        contract = (REPO / 'docs/platform/integrate-internal-contracts.md').read_text()
        section = contract[contract.index('## 1. Permissions'):contract.index('## 2.')]
        named = set(re.findall(r'\| `([a-z.-]+)` \|', section))
        self.assertEqual(named, {p for _, _, _, perms in io_ops.GRANTS for p in perms} - {'platform-identity.read'})

    def test_every_contract_flag_is_owned_by_exactly_one_operation(self):
        contract = (REPO / 'docs/platform/integrate-internal-contracts.md').read_text()
        named = set(re.findall(r'`((?:DEVELOPER_PLATFORM|PLATFORM_ACCESS|MESSAGING|MEETING|ORGANIZATIONS|API_GATEWAY)'
                               r'_[A-Z_]+_ENABLED)`', contract))
        self.assertGreaterEqual(len(named), 11)
        owners = {}
        for service, flag, op in io_ops.INTEGRATE_FLAGS:
            owners.setdefault(flag, set()).add(op)
            self.assertIn(service, cr.SERVICES)
            self.assertIn(op, io_ops.OPERATIONS)
            self.assertIn(flag, cr.FLAGS)
        self.assertEqual(named - set(owners), set())
        self.assertTrue(all(len(ops) == 1 for ops in owners.values()), owners)
        self.assertFalse(set(owners) & {flag for _, flag, _ in po.PLATFORM_FLAGS})
        for var in (io_ops.REVIEWERS_VAR, io_ops.PAIRWISE_KEY_VAR, io_ops.CONTEXT_KEY_VAR):
            self.assertIn(var, cr.FLAGS)

    def test_flag_names_are_the_ones_the_services_read(self):
        sources = {'developer-platform': 'services/developer-platform/src/config/mod.rs',
                   'developer-platform-worker': 'services/developer-platform/src/config/mod.rs',
                   'platform-access': 'services/platform-access/src/config/platform_build.rs',
                   'meeting-service': 'services/meeting-service/src/services/apps/mod.rs',
                   'organizations': 'services/organizations/src/config.rs',
                   'api-gateway': 'services/api-gateway/src/config/mod.rs',
                   'messaging': 'services/messaging/src/apps.rs'}
        for service, flag, _ in io_ops.INTEGRATE_FLAGS:
            self.assertIn('"%s"' % flag, (REPO / sources[service]).read_text(), (service, flag))
        self.assertIn('"%s"' % io_ops.PAIRWISE_KEY_VAR, (REPO / sources['platform-access']).read_text())
        self.assertIn('"%s"' % io_ops.CONTEXT_KEY_VAR, (REPO / sources['meeting-service']).read_text())
        self.assertIn('"%s"' % io_ops.REVIEWERS_VAR, (REPO / sources['developer-platform']).read_text())

    def test_inspect_never_prints_key_files_but_shows_reviewers_and_switches(self):
        flags = cr.safe_flags({io_ops.PAIRWISE_KEY_VAR: io_ops.PAIRWISE_KEY_TARGET, 'API_GATEWAY_OAUTH_ENABLED': 'true',
                               io_ops.CONTEXT_KEY_VAR: io_ops.CONTEXT_KEY_TARGET, io_ops.REVIEWERS_VAR: 'github-alice'})
        self.assertEqual(flags, {io_ops.PAIRWISE_KEY_VAR: 'present', io_ops.CONTEXT_KEY_VAR: 'present',
                                 'API_GATEWAY_OAUTH_ENABLED': 'true', io_ops.REVIEWERS_VAR: 'github-alice'})


class SafeDefaults(unittest.TestCase):
    def test_apps_flags_are_refused_until_their_activation_is_recorded(self):
        running = {'platform-access': {'PLATFORM_ACCESS_OAUTH_ENABLED': 'true'},
                   'api-gateway': {'API_GATEWAY_OAUTH_ENABLED': 'true'},
                   'developer-platform-worker': {'DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED': 'true'},
                   'organizations': {'ORGANIZATIONS_APP_INSTALLS_ENABLED': 'false'}}
        with isolated_work() as work:
            self.assertEqual(sorted(cr.safe_default_violations(running)), [
                'api-gateway API_GATEWAY_OAUTH_ENABLED', 'developer-platform-worker DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED',
                'platform-access PLATFORM_ACCESS_OAUTH_ENABLED'])
            journal(work, io_ops.OAUTH_APPS, io_ops.OAUTH, io_ops.EVENTS)
            self.assertEqual(cr.safe_default_violations(running), [])
            journal(work, io_ops.OAUTH_APPS, io_ops.OAUTH, io_ops.EVENTS, status='rolled-back')
            self.assertEqual(len(cr.safe_default_violations(running)), 3)


class Edge(unittest.TestCase):
    def test_edge_prefixes_follow_the_journal_after_the_build_ones(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families', po.USERS, po.MEETINGS, io_ops.DIRECTORY)
            self.assertEqual(cr.edge_prefixes(), cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE + po.MEETINGS_EDGE
                             + io_ops.DIRECTORY_EDGE)
            journal(work, 'activate-gateway-families', po.USERS, io_ops.DIRECTORY, io_ops.OAUTH)
            self.assertEqual(cr.edge_prefixes(), cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                             + io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE)
        src = (REPO / 'infra/calling/production/coordinated_release.py').read_text()
        # Re-running the families step keeps the Apps prefixes too.
        self.assertIn('platform_ops.edge_prefixes(done)\n                                                   + '
                      'integrate_ops.edge_prefixes(done)', src)
        snippet = cr.gateway_snippet(cr.GATEWAY_PREFIXES + io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE)
        for prefix in io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE:
            self.assertIn('location = %s {' % prefix, snippet)
            self.assertIn('location ^~ %s/ {' % prefix, snippet)
        self.assertFalse(set(io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE)
                         & set(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE + po.MEETINGS_EDGE
                               + po.CALLING_EDGE_PREFIXES))

    def test_every_anonymous_apps_route_of_the_contract_is_under_an_edge_prefix(self):
        contract = json.loads((REPO / 'packages/contracts/platform-developer/http/v1.openapi.json').read_text())
        routes = [p for p in contract['paths'] if p.startswith(('/v1/oauth', '/v1/app-directory'))]
        self.assertTrue(routes)
        for path in routes:
            self.assertTrue(any(path == x or path.startswith(x + '/') for x in io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE),
                            path)

    def rollback(self, operation, prefixes):
        order = []
        with isolated_work() as work:
            (work / 'journal.json').write_text(json.dumps([
                dict(id='f:0', operation='activate-gateway-families', sha=SHA, status='done', components=[]),
                dict(id='u:1', operation=po.USERS, sha=SHA, status='done', components=[]),
                dict(id='o:2', operation=operation, sha=SHA, runId='9-1', status='done', components=[
                    dict(service='developer-platform', name='cfmeet-developer-platform', previous=None,
                         status='released', created=False)])]))
            snippet = work / 'gateway.conf'
            snippet.write_text(cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE + prefixes))
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), \
                    patch.object(cr.p, 'reload_host_nginx', lambda: order.append('edge')), \
                    patch.object(cr, 'restore', lambda component, run_id: order.append('restore')), \
                    patch.object(cr, 'container_names', lambda: []), patch.object(cr.p, 'regression', lambda: None), \
                    contextlib.redirect_stdout(io.StringIO()) as out:
                cr.rollback(SHA, '10-1')
            self.assertEqual(snippet.read_text(), cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE))
            self.assertEqual(json.loads((work / 'journal.json').read_text())[-1]['status'], 'rolled-back')
        self.assertEqual(order, ['edge', 'restore'])
        self.assertIn('platformEdgeRemoved', out.getvalue())

    def test_rollback_removes_v1_oauth_before_the_containers(self):
        self.rollback(io_ops.OAUTH, io_ops.OAUTH_EDGE)

    def test_rollback_removes_v1_app_directory_before_the_containers(self):
        self.rollback(io_ops.DIRECTORY, io_ops.DIRECTORY_EDGE)

    def test_rollback_of_a_containers_only_phase_leaves_the_edge(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families', io_ops.OAUTH)
            snippet = work / 'gateway.conf'
            snippet.write_text('unchanged')
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), \
                    patch.object(cr.p, 'reload_host_nginx', side_effect=AssertionError('no edge change')):
                io_ops.undo_edge(cr, {'operation': io_ops.DELEGATED})
            self.assertEqual(snippet.read_text(), 'unchanged')


class OAuthApps(unittest.TestCase):
    def run_op(self, reviewers=None, confirm='', values=None):
        apply = Capture()
        with tempfile.TemporaryDirectory() as folder, isolated_work() as work, plan_env(values) as out, \
                patch.object(cr.p, 'SECRETS', Path(folder)), patch.object(cr.os, 'chown') as chown, \
                patch.object(cr, 'apply_activation', apply):
            if reviewers is not None:
                (work / io_ops.REVIEWERS_FILE).write_text(reviewers)
            io_ops.activate_oauth_apps(cr, SHA, '11-1', confirm)
            key = Path(folder) / io_ops.PAIRWISE_KEY
            first = key.read_text()
            mode = oct(key.stat().st_mode & 0o777)
            io_ops.activate_oauth_apps(cr, SHA, '11-2', confirm)
            self.assertEqual(key.read_text(), first)  # never regenerated over an existing key
        return apply, first, mode, chown, out.getvalue(), key

    def test_plan_reviewers_pairwise_key_and_grant_receiver(self):
        apply, key_text, mode, chown, out, key = self.run_op(REVIEWERS)
        self.assertEqual(apply.calls[0]['operation'], io_ops.OAUTH_APPS)
        self.assertEqual([c[0] for c in apply.calls[0]['changes']], ['platform-access', 'developer-platform'])
        service, access, extra = apply.plan['platform-access']
        self.assertEqual(access, {'PLATFORM_ACCESS_OAUTH_ENABLED': 'true', io_ops.PAIRWISE_KEY_VAR: io_ops.PAIRWISE_KEY_TARGET})
        self.assertEqual(extra['mounts'], [(key, io_ops.PAIRWISE_KEY_TARGET)])
        self.assertEqual(apply.plan['developer-platform'][1], {'DEVELOPER_PLATFORM_APPS_ENABLED': 'true',
                                                               io_ops.REVIEWERS_VAR: 'github-alice,github-bob'})
        self.assertEqual(mode, '0o400')
        chown.assert_called_with(key, cr.SERVICES['platform-access']['uid'], cr.SERVICES['platform-access']['uid'])
        # The key passes Platform Access's pepper rules and is never printed.
        value = key_text.strip()
        self.assertGreaterEqual(len(value), 32)
        self.assertGreaterEqual(len(set(value)), 12)
        self.assertFalse(any(f in value.lower() for f in io_ops.PLACEHOLDER_FRAGMENTS))
        self.assertNotIn(value, out)
        self.assertNotIn('11111111-2222', out)  # reviewer user ids stay on the VPS
        emitted = [json.loads(line) for line in out.splitlines() if line.startswith('{')]
        self.assertEqual([e['oauthPairwiseKey']['generated'] for e in emitted if 'oauthPairwiseKey' in e], [True, False])
        self.assertEqual([e['appReview'] for e in emitted if 'appReview' in e][0],
                         {'reviewers': ['github-alice', 'github-bob'], 'reviewDecisionsPossible': True})
        self.assertEqual({r for op, r, _, _ in cr.GRANTS if op == io_ops.OAUTH_APPS}, {'platform-access'})

    def test_refused_without_reviewers_unless_the_owner_confirms_none_yet(self):
        for reviewers in (None, '', ' \n'):
            with tempfile.TemporaryDirectory() as folder, isolated_work() as work, plan_env(), must_not_apply(), \
                    patch.object(cr.p, 'SECRETS', Path(folder)):
                if reviewers is not None:
                    (work / io_ops.REVIEWERS_FILE).write_text(reviewers)
                with self.assertRaisesRegex(RuntimeError, 'CFMEET_APP_REVIEWERS.*no-reviewers-yet'):
                    io_ops.activate_oauth_apps(cr, SHA, '11-1', '')
                self.assertEqual(list(Path(folder).iterdir()), [])  # nothing generated before the refusal
        apply, _, _, _, out, _ = self.run_op(None, io_ops.NO_REVIEWERS_CONFIRM)
        self.assertEqual(apply.plan['developer-platform'][1], {'DEVELOPER_PLATFORM_APPS_ENABLED': 'true'})
        self.assertIn('"reviewDecisionsPossible": false', out)
        # Reviewers named anyway: the confirmation changes nothing.
        apply, _, _, _, _, _ = self.run_op(REVIEWERS, io_ops.NO_REVIEWERS_CONFIRM)
        self.assertIn(io_ops.REVIEWERS_VAR, apply.plan['developer-platform'][1])

    def test_invalid_reviewers_or_confirmations_are_refused(self):
        for reviewers, confirm, message in (('github-alice', '', 'github-<login>=<CF Meet user id>'),
                                            ('alice=11111111-2222-4333-8444-555555555555', '', 'CFMEET_APP_REVIEWERS'),
                                            (REVIEWERS, 'owner-approved-calling-edge', 'takes only')):
            with tempfile.TemporaryDirectory() as folder, isolated_work() as work, plan_env(), must_not_apply(), \
                    patch.object(cr.p, 'SECRETS', Path(folder)):
                (work / io_ops.REVIEWERS_FILE).write_text(reviewers)
                with self.assertRaisesRegex(RuntimeError, message):
                    io_ops.activate_oauth_apps(cr, SHA, '11-1', confirm)

    def test_refused_before_the_developer_gateway_api(self):
        with isolated_work(), plan_env(envs(developer_platform={})), must_not_apply():
            with self.assertRaisesRegex(RuntimeError, 'activate-developer-platform'):
                io_ops.activate_oauth_apps(cr, SHA, '11-1', io_ops.NO_REVIEWERS_CONFIRM)

    def test_parse_reviewers(self):
        self.assertEqual(io_ops.parse_reviewers(REVIEWERS.upper().replace('GITHUB', 'github')),
                         {'github-alice': '11111111-2222-4333-8444-555555555555',
                          'github-bob': '66666666-7777-4888-9999-000000000000'})
        self.assertEqual(io_ops.parse_reviewers(''), {})


class FakeHttp:
    """integrate_operations.http_json: answers by (method, path); the gateway stamps its routes."""

    def __init__(self, answers, snippet=None):
        self.answers = answers
        self.snippet = snippet
        self.calls = []

    def __call__(self, c, method, path, body=None, headers=None):
        self.calls.append((method, path, body, dict(headers or {})))
        status, doc = self.answers.get((method, path), (404, {}))
        routed = self.snippet is None or any(
            ('location ^~ %s/' % p in self.snippet.read_text()) and (path == p or path.startswith(p + '/'))
            for p in io_ops.OAUTH_EDGE + io_ops.DIRECTORY_EDGE)
        headers_out = {'content-type': 'application/json'}
        if routed:
            headers_out['x-correlation-id'] = 'c'
        return status, headers_out, doc


GOOD_OAUTH = {('GET', io_ops.OAUTH_METADATA_PATH): (200, {'token_endpoint': cr.PUBLIC + io_ops.OAUTH_TOKEN_PATH,
                                                          'code_challenge_methods_supported': ['S256']}),
              ('POST', io_ops.OAUTH_TOKEN_PATH): (401, {'error': 'invalid_client'})}
GOOD_DIRECTORY = {('GET', io_ops.DIRECTORY_PATH): (200, {'items': [], 'nextCursor': None})}


@contextlib.contextmanager
def edge_post(work, answers, done=(), acceptance_ok=True, smoke=None):
    """A real gateway snippet fixture; apply_activation runs the post-check immediately."""
    snippet, site = tpo.GatewayEdge().edge_fixture(work)
    reloads, stages = [], []

    def acceptance(c, run_id, op):
        stages.append(op)
        if not acceptance_ok:
            raise RuntimeError('Platform acceptance failed')
    http = FakeHttp(answers, snippet)
    with patch.object(cr, 'GATEWAY_SNIPPET', snippet), patch.object(cr, 'SITE', site), \
            patch.object(cr, 'Path', lambda p: work / 'snippets' if p == '/etc/nginx/snippets' else Path(p)), \
            patch.object(cr, 'public_smoke', lambda: smoke or {'public_swagger': 'PASS'}), \
            patch.object(cr.p, 'reload_host_nginx', lambda: reloads.append(1)), \
            patch.object(cr, 'env_of', lambda n: envs()[n]), \
            patch.object(cr, 'has_signing_identity', lambda n: None), \
            patch.object(cr, 'tls_route_present', lambda p: None), \
            patch.object(cr, 'apply_activation', Capture(run_post=True)), \
            patch.object(po, 'run_acceptance', acceptance), patch.object(io_ops, 'http_json', http), \
            contextlib.redirect_stdout(io.StringIO()) as out:
        yield SimpleNamespace(snippet=snippet, reloads=reloads, stages=stages, http=http, out=out)


class OAuth(unittest.TestCase):
    def test_plan_flags_grant_receivers_and_meeting_base_url(self):
        apply = Capture()
        values = envs(meeting_service=dict(envs()['meeting-service'], CF_PLATFORM_ACCESS_BASE_URL=''))
        with plan_env(values), patch.object(cr, 'apply_activation', apply):
            io_ops.activate_oauth(cr, SHA, '12-1', '')
        plan = apply.plan
        self.assertEqual(list(plan), ['meeting-service', 'developer-platform', 'api-gateway'])
        self.assertEqual(plan['meeting-service'][1], {'MEETING_OAUTH_AUTHORIZE_ENABLED': 'true',
                                                      'CF_PLATFORM_ACCESS_BASE_URL': cr.private_url(8443)})
        self.assertEqual(plan['developer-platform'][1], {'DEVELOPER_PLATFORM_OAUTH_ENABLED': 'true'})
        self.assertEqual(plan['api-gateway'][1], {'API_GATEWAY_OAUTH_ENABLED': 'true'})
        # Platform Access (authorize grant) is recreated with its bundle by apply_activation.
        self.assertEqual({r for op, r, _, _ in cr.GRANTS if op == io_ops.OAUTH}, {'platform-access', 'meeting-service'})

    def test_preconditions(self):
        cases = ((dict(meeting_service={'CF_SERVICE_TRUST_BUNDLE_FILE': '/t'}), 'communications scope'),
                 (dict(meeting_service={'CFMEET_COMMUNICATION_ENVIRONMENT_ID': 'e'}), 'trust bundle'),
                 (dict(developer_platform={}), 'activate-developer-console'),
                 (dict(api_gateway={}), 'Platform Access target'))
        for overrides, message in cases:
            with plan_env(envs(**overrides)), must_not_apply():
                with self.assertRaisesRegex(RuntimeError, message):
                    io_ops.activate_oauth(cr, SHA, '12-1', '')

    def test_post_routes_v1_oauth_proves_metadata_and_invalid_client_and_runs_the_acceptance(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families', po.USERS, io_ops.OAUTH_APPS)
            with edge_post(work, GOOD_OAUTH) as run:
                before = run.snippet.read_text()
                io_ops.activate_oauth(cr, SHA, '12-1', '')
                after = run.snippet.read_text()
            self.assertNotIn('/v1/oauth', before)
            self.assertEqual(after, cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                                                       + io_ops.OAUTH_EDGE))
            self.assertEqual(run.stages, [io_ops.OAUTH])
            self.assertEqual(len(run.reloads), 1)
            method, path, body, headers = run.http.calls[1]
            self.assertEqual((method, path, body), ('POST', io_ops.OAUTH_TOKEN_PATH, b'grant_type=authorization_code'))
            self.assertEqual(headers['Content-Type'], 'application/x-www-form-urlencoded')
            self.assertNotIn('Authorization', headers)
            self.assertIn('oauthEdge', run.out.getvalue())

    def test_a_failed_oauth_check_or_acceptance_restores_the_snippet(self):
        bad_metadata = {**GOOD_OAUTH, ('GET', io_ops.OAUTH_METADATA_PATH): (200, {'token_endpoint': 'https://evil'})}
        plain = {**GOOD_OAUTH, ('GET', io_ops.OAUTH_METADATA_PATH):
                 (200, {'token_endpoint': cr.PUBLIC + io_ops.OAUTH_TOKEN_PATH,
                        'code_challenge_methods_supported': ['plain', 'S256']})}
        open_token = {**GOOD_OAUTH, ('POST', io_ops.OAUTH_TOKEN_PATH): (400, {'error': 'invalid_request'})}
        cases = [dict(answers={}), dict(answers=bad_metadata), dict(answers=plain), dict(answers=open_token),
                 dict(answers=GOOD_OAUTH, smoke={'public_swagger': 'FAIL'}),
                 dict(answers=GOOD_OAUTH, acceptance_ok=False)]
        for case in cases:
            with isolated_work() as work:
                journal(work, 'activate-gateway-families', io_ops.OAUTH_APPS)
                with edge_post(work, case['answers'], acceptance_ok=case.get('acceptance_ok', True),
                               smoke=case.get('smoke')) as run:
                    before = run.snippet.read_text()
                    with self.assertRaisesRegex(RuntimeError, 'edge checks failed' if case.get('acceptance_ok', True)
                                                else 'acceptance failed'):
                        io_ops.activate_oauth(cr, SHA, '12-1', '')
                    self.assertEqual(run.snippet.read_text(), before, case)
                    self.assertEqual(len(run.reloads), 2, case)

    def test_an_answer_not_stamped_by_the_gateway_fails(self):
        http = FakeHttp(GOOD_OAUTH)

        def direct(c, method, path, body=None, headers=None):
            status, _, doc = http(c, method, path, body, headers)
            return status, {'content-type': 'application/json'}, doc  # no x-correlation-id
        with patch.object(io_ops, 'http_json', direct):
            checks = io_ops.oauth_edge_checks(cr)
        self.assertEqual(sorted(checks.values()), ['FAIL 200', 'FAIL 401 invalid_client'])

    def test_refuses_an_explicit_existing_v1_oauth_location(self):
        with isolated_work() as work, edge_post(work, GOOD_OAUTH) as run, must_not_apply():
            (work / 'snippets' / 'cfmeet-calling-routes.conf').write_text('location ^~ /v1/oauth/ { }')
            with self.assertRaisesRegex(RuntimeError, 'already serve: /v1/oauth'):
                io_ops.activate_oauth(cr, SHA, '12-1', '')
            self.assertNotIn('/v1/oauth', run.snippet.read_text())


class Delegated(unittest.TestCase):
    def test_plan_and_meeting_platform_precondition(self):
        apply = Capture(run_post=True)
        ran = []
        with plan_env(), patch.object(cr, 'apply_activation', apply), \
                patch.object(po, 'run_acceptance', lambda c, r, op: ran.append(op)):
            io_ops.activate_delegated(cr, SHA, '13-1', '')
        self.assertEqual(apply.calls[0]['changes'], [('meeting-service', {'MEETING_DELEGATED_APPS_ENABLED': 'true'}),
                                                     ('messaging', {'MESSAGING_APPS_ENABLED': 'true'})])
        self.assertEqual(ran, [io_ops.DELEGATED])
        self.assertFalse([g for g in io_ops.GRANTS if g[0] == io_ops.DELEGATED])
        meeting = dict(envs()['meeting-service'], MEETING_PLATFORM_ACCESS_ENABLED='false')
        with plan_env(envs(meeting_service=meeting)), must_not_apply():
            with self.assertRaisesRegex(RuntimeError, 'MEETING_PLATFORM_ACCESS_ENABLED'):
                io_ops.activate_delegated(cr, SHA, '13-1', '')
        gateway = dict(envs()['api-gateway'], API_GATEWAY_DISABLED_DOMAINS='meetings')
        with plan_env(envs(api_gateway=gateway)), must_not_apply():
            with self.assertRaisesRegex(RuntimeError, 'meetings domain'):
                io_ops.activate_delegated(cr, SHA, '13-1', '')


class Installs(unittest.TestCase):
    def test_plan_grant_receiver_and_preconditions(self):
        apply = Capture()
        with plan_env(), patch.object(cr, 'apply_activation', apply):
            io_ops.activate_installs(cr, SHA, '14-1', '')
        self.assertEqual(apply.calls[0]['changes'], [('organizations', {'ORGANIZATIONS_APP_INSTALLS_ENABLED': 'true'})])
        self.assertEqual({r for op, r, _, _ in cr.GRANTS if op == io_ops.INSTALLS}, {'organizations'})
        meeting = {k: v for k, v in envs()['meeting-service'].items() if k != 'CF_ORGANIZATIONS_BASE_URL'}
        for values, message in ((envs(meeting_service=meeting), 'Organizations target'),
                                (envs(organizations={}), 'trust bundle')):
            with plan_env(values), must_not_apply():
                with self.assertRaisesRegex(RuntimeError, message):
                    io_ops.activate_installs(cr, SHA, '14-1', '')


class Events(unittest.TestCase):
    def test_needs_messaging_to_publish_to_the_developer_platform_and_never_changes_it(self):
        for destination in (None, '', 'calling'):
            messaging = {'CF_PLATFORM_ACCESS_BASE_URL': 'https://x'}
            if destination is not None:
                messaging['MESSAGING_PUBLIC_EVENTS_DESTINATION'] = destination
            with plan_env(envs(messaging=messaging)), must_not_apply():
                with self.assertRaisesRegex(RuntimeError, 'never changed here'):
                    io_ops.activate_events(cr, SHA, '15-1', '')
        for destination in io_ops.EVENT_DESTINATIONS:
            apply = Capture()
            messaging = {'MESSAGING_PUBLIC_EVENTS_DESTINATION': destination}
            with plan_env(envs(messaging=messaging)), patch.object(cr, 'apply_activation', apply):
                io_ops.activate_events(cr, SHA, '15-1', '')
            plan = apply.plan
            self.assertEqual(list(plan), ['developer-platform', 'developer-platform-worker'])
            self.assertEqual(plan['developer-platform'][1], {'DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED': 'true'})
            self.assertEqual(plan['developer-platform-worker'][1], {'DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED': 'true',
                                                                    'CF_PLATFORM_ACCESS_BASE_URL': cr.private_url(8443)})
            self.assertNotIn('messaging', plan)


class Directory(unittest.TestCase):
    def test_post_routes_v1_app_directory_and_proves_an_anonymous_list(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families', io_ops.OAUTH_APPS, io_ops.OAUTH)
            with edge_post(work, GOOD_DIRECTORY) as run:
                io_ops.activate_directory(cr, SHA, '16-1', '')
                after = run.snippet.read_text()
            self.assertEqual(after, cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + io_ops.OAUTH_EDGE
                                                       + io_ops.DIRECTORY_EDGE))
            self.assertEqual(run.stages, [io_ops.DIRECTORY])
            self.assertIn('appDirectoryEdge', run.out.getvalue())

    def test_a_failed_directory_check_restores_the_snippet(self):
        for answers in ({}, {('GET', io_ops.DIRECTORY_PATH): (200, {'apps': []})},
                        {**GOOD_DIRECTORY, ('GET', io_ops.DIRECTORY_PATH + '/00000000-0000-4000-8000-000000000000'):
                         (200, {})}):
            with isolated_work() as work:
                journal(work, 'activate-gateway-families', io_ops.OAUTH_APPS)
                with edge_post(work, answers) as run:
                    before = run.snippet.read_text()
                    with self.assertRaisesRegex(RuntimeError, 'edge checks failed'):
                        io_ops.activate_directory(cr, SHA, '16-1', '')
                    self.assertEqual(run.snippet.read_text(), before)
                    self.assertEqual(run.stages, [])


class InMeeting(unittest.TestCase):
    def run_op(self, keys_answer, folder):
        apply = Capture(run_post=True)
        ran = []
        uid_file = Path(folder) / 'meeting-service-auth.pem'
        uid_file.write_text('x')
        container = {'Mounts': [{'Source': str(uid_file), 'Destination': '/run/secrets/service-auth.pem'}],
                     'Name': '/' + cr.MEETING}
        with plan_env() as out, patch.object(cr.p, 'SECRETS', Path(folder)), patch.object(cr.os, 'chown') as chown, \
                patch.object(cr.p, 'inspect', lambda name: container), \
                patch.object(cr.p, 'env_of', lambda c: {'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/run/secrets/service-auth.pem'}), \
                patch.object(cr, 'apply_activation', apply), \
                patch.object(po, 'run_acceptance', lambda c, r, op: ran.append(op)), \
                patch.object(io_ops, 'http_json', FakeHttp({('GET', io_ops.CONTEXT_KEYS_PATH): keys_answer})):
            error = None
            try:
                io_ops.activate_in_meeting(cr, SHA, '17-1', '')
            except RuntimeError as e:
                error = e
        return apply, ran, chown, out.getvalue(), error, uid_file

    def test_key_generated_once_owned_like_the_meeting_key_mounted_and_never_printed(self):
        good = (200, {'keys': [{'kty': 'OKP', 'crv': 'Ed25519', 'x': 'AAAA', 'kid': 'k', 'use': 'sig', 'alg': 'EdDSA'}]})
        with tempfile.TemporaryDirectory() as folder:
            apply, ran, chown, out, error, uid_file = self.run_op(good, folder)
            self.assertIsNone(error)
            key = Path(folder) / io_ops.CONTEXT_KEY
            first = key.read_text()
            self.run_op(good, folder)
            self.assertEqual(key.read_text(), first)
            self.assertEqual(oct(key.stat().st_mode & 0o777), '0o400')
            owner = uid_file.stat().st_uid
            chown.assert_called_with(key, owner, owner)
        text = subprocess.run(['openssl', 'pkey', '-noout', '-text'], input=first, capture_output=True, text=True)
        self.assertIn('ED25519', (text.stdout + text.stderr).upper())
        self.assertNotIn(first.strip().splitlines()[1], out)
        service, overrides, extra = apply.plan['meeting-service']
        self.assertEqual(overrides, {'MEETING_IN_MEETING_APPS_ENABLED': 'true', io_ops.CONTEXT_KEY_VAR: io_ops.CONTEXT_KEY_TARGET})
        self.assertEqual(extra['mounts'], [(key, io_ops.CONTEXT_KEY_TARGET)])
        self.assertEqual(ran, [io_ops.IN_MEETING])
        self.assertIn('appContextKeys', out)

    def test_the_jwks_must_hold_exactly_one_public_ed25519_key(self):
        key = {'kty': 'OKP', 'crv': 'Ed25519', 'x': 'AAAA'}
        for answer in ((404, {}), (200, {'keys': []}), (200, {'keys': [key, dict(key, x='BBBB')]}),
                       (200, {'keys': [dict(key, d='private')]}), (200, {'keys': [dict(key, crv='P-256', kty='EC')]})):
            with tempfile.TemporaryDirectory() as folder:
                _, ran, _, _, error, _ = self.run_op(answer, folder)
            self.assertRegex(str(error), 'key set check failed', answer)
            self.assertEqual(ran, [])

    def test_refused_before_the_consent_relay(self):
        meeting = dict(envs()['meeting-service'], MEETING_OAUTH_AUTHORIZE_ENABLED='false')
        with plan_env(envs(meeting_service=meeting)), must_not_apply():
            with self.assertRaisesRegex(RuntimeError, 'activate-oauth'):
                io_ops.activate_in_meeting(cr, SHA, '17-1', '')


class AcceptanceWiring(unittest.TestCase):
    def test_apps_features_follow_the_journal_and_ride_on_every_acceptance(self):
        seen = []

        def run(run_id, stages, emit, apps=()):
            seen.append((stages, apps))
            return {'a': 'PASS'}
        with isolated_work() as work, patch.object(platform_acceptance, 'run', run), \
                contextlib.redirect_stdout(io.StringIO()):
            journal(work, po.USERS, po.TOKENS, po.MEETINGS, io_ops.OAUTH_APPS, io_ops.DIRECTORY)
            po.run_acceptance(cr, '18-1', io_ops.OAUTH)
            po.run_acceptance(cr, '18-1', po.CHAT)  # a Build re-activation keeps the apps stage
            cr.run(po.ACCEPTANCE, SHA, '18-2')
            journal(work, po.USERS, io_ops.DIRECTORY)  # registration rolled back: no apps stage
            po.run_acceptance(cr, '18-1', io_ops.OAUTH)
        self.assertEqual(seen, [
            (('users', 'tokens', 'meetings', 'apps'), ('register', 'oauth', 'directory')),
            (('users', 'tokens', 'chat', 'meetings', 'apps'), ('register', 'directory')),
            (('users', 'tokens', 'meetings', 'apps'), ('register', 'directory')),
            (('users',), ())])

    def test_activations_refuse_to_run_out_of_order(self):
        with isolated_work() as work:
            journal(work, 'activate-developer-console', 'activate-gateway-families', po.USERS)
            with patch.object(cr.p, 'identity', side_effect=AssertionError('must not start')):
                for op in io_ops.OPERATIONS[1:]:
                    with self.assertRaisesRegex(RuntimeError, 'Activation order'):
                        cr.run(op, SHA, '19-1')
            journal(work, 'activate-developer-console')
            with patch.object(cr.p, 'identity', side_effect=AssertionError('must not start')):
                with self.assertRaisesRegex(RuntimeError, 'activate-platform-users'):
                    cr.run(io_ops.OAUTH_APPS, SHA, '19-1')


if __name__ == '__main__':
    unittest.main()
