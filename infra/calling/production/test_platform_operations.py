"""Offline tests: CF Meet Platform "Build" activations (platform_operations.py, ADR 0062 / 0063)."""
import contextlib
import io
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import notification_operations as notif_ops
import platform_operations as po
from test_coordinated_release import REPO, isolated_work, permission_registry

SHA = 'a' * 40
MANIFEST = json.loads((REPO / 'infra/api-gateway/route-manifest.json').read_text())


def journal(work, *operations, status='done'):
    (work / 'journal.json').write_text(json.dumps(
        [dict(id='%s:%d' % (o, i), operation=o, sha=SHA, status=status, components=[]) for i, o in enumerate(operations)]))


class Capture:
    """Stands in for cr.apply_activation: records the plan, optionally runs the post-check."""

    def __init__(self, run_post=False):
        self.calls = []
        self.run_post = run_post

    def __call__(self, operation, sha, run_id, changes, irreversible=False, post=None):
        self.calls.append(dict(operation=operation, changes=[tuple(c) for c in changes], post=post))
        if self.run_post and post:
            post()

    @property
    def plan(self):
        return {c[0]: c for c in self.calls[-1]['changes']}


def envs(**overrides):
    base = {
        'api-gateway': {'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'true', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x:8443',
                        'CF_CALLING_BASE_URL': 'https://x:8445', 'API_GATEWAY_DISABLED_DOMAINS': cr.DISABLED_DOMAINS},
        'developer-platform': {'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'true', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x'},
        'messaging': {'MESSAGING_SESSION_DEVICES_ENABLED': 'true', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x',
                      'CFMEET_USAGE_EXPORT_ENABLED': 'true', 'CF_NOTIFICATIONS_BASE_URL': 'https://x:8456'},
        'calling': {'CF_PLATFORM_ACCESS_BASE_URL': 'https://x', 'CF_NOTIFICATIONS_BASE_URL': 'https://x:8456'},
        'meeting-service': {'CF_SERVICE_TRUST_BUNDLE_FILE': '/run/trust.json', 'CF_PLATFORM_ACCESS_BASE_URL': 'https://x',
                            'CF_MESSAGING_BASE_URL': 'https://x:8444'},
        'platform-access': {}, 'notifications': {}, 'notifications-2': {},
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
            patch.object(cr, 'done', lambda op: op in done):
        yield


class Registration(unittest.TestCase):
    def test_every_operation_is_registered_with_its_prerequisites(self):
        for op in po.OPERATIONS:
            self.assertIn(op, cr.ACTIVATIONS)
            self.assertEqual(cr.ACTIVATIONS[op][0], po.REQUIRES[op])
            self.assertIn(op, cr.OPERATIONS)
            self.assertRegex(op, r'^[a-z][a-z-]{1,40}$')  # the workflow's input rule
        self.assertIn(po.ACCEPTANCE, cr.OPERATIONS)
        self.assertNotIn(po.ACCEPTANCE, cr.ACTIVATIONS)
        self.assertRegex(po.CALLING_EDGE_CONFIRM, r'^[a-z0-9,=-]{0,120}$')
        # B1 -> B2 -> (B3, B4) -> B5; B6 after B2 and B3; the calling edge after B4 only.
        self.assertEqual(po.REQUIRES[po.TOKENS], (po.USERS,))
        self.assertIn('activate-notifications-push', po.REQUIRES[po.PUSH])
        self.assertIn('activate-calling-push', po.REQUIRES[po.PUSH])
        self.assertEqual(po.REQUIRES[po.CALLING_EDGE][0], po.CALLS)
        self.assertNotIn(po.CALLING_EDGE, sum((po.REQUIRES[o] for o in po.OPERATIONS), ()))

    def test_grants_match_the_contract_and_the_permission_registry(self):
        registry = permission_registry()
        self.assertEqual(cr.validate_grants(registry, po.GRANTS), [])
        rows = {(op, receiver, issuer): set(perms) for op, receiver, issuer, perms in po.GRANTS}
        self.assertEqual(rows[(po.USERS, 'platform-access', 'developer-platform')], {'platform-end-users.delegate'})
        for issuer in ('messaging', 'calling', 'meeting-service', 'api-gateway'):
            self.assertEqual(rows[(po.TOKENS, 'platform-access', issuer)],
                             {'platform-user-token.verify', 'platform-environment-origins.read'})
        for replica in ('notifications', 'notifications-2'):
            self.assertEqual(rows[(po.PUSH, replica, 'developer-platform')], {'notifications-push-provider.manage'})
        self.assertIn('platform-principal.ensure-meeting-platform', rows[(po.MEETINGS, 'platform-access', 'meeting-service')])
        self.assertEqual(rows[(po.MEETINGS, 'messaging', 'meeting-service')], {'messaging-meeting.sync-platform'})
        self.assertEqual(rows[(po.MEETINGS, 'meeting-service', 'api-gateway')], {'meeting-request.forward'})
        for row in po.GRANTS:
            self.assertIn(row, cr.GRANTS)
        self.assertFalse([g for g in po.GRANTS if g[0] in (po.CHAT, po.CALLS, po.CALLING_EDGE)])

    def test_every_contract_flag_is_owned_by_exactly_one_operation(self):
        contract = (REPO / 'docs/platform/build-internal-contracts.md').read_text()
        named = set(re.findall(r'`((?:DEVELOPER_PLATFORM|MESSAGING|CALLING|MEETING|NOTIFICATIONS|API_GATEWAY)_[A-Z_]+_ENABLED)`',
                               contract))
        owned = {flag for _, flag, _ in po.PLATFORM_FLAGS}
        self.assertTrue(named, 'contract flags not found')
        self.assertEqual(named - owned, set())
        for service, flag, op in po.PLATFORM_FLAGS:
            self.assertIn(service, cr.SERVICES)
            self.assertIn(op, po.OPERATIONS)
            self.assertIn(flag, cr.FLAGS)

    def test_inspect_shows_boolean_switches_even_when_their_name_says_token(self):
        flags = cr.safe_flags({'PLATFORM_ACCESS_USER_TOKENS_ENABLED': 'true', po.USER_TOKEN_KEY_VAR: '/run/secrets/x.pem',
                               'API_GATEWAY_USER_TOKENS_ENABLED': 'https://user:hunter2@x'})
        self.assertEqual(flags['PLATFORM_ACCESS_USER_TOKENS_ENABLED'], 'true')
        self.assertEqual(flags[po.USER_TOKEN_KEY_VAR], 'present')
        self.assertEqual(flags['API_GATEWAY_USER_TOKENS_ENABLED'], 'present')


class SafeDefaults(unittest.TestCase):
    def test_platform_flags_are_refused_until_their_activation_is_recorded(self):
        running = {'platform-access': {'PLATFORM_ACCESS_END_USERS_ENABLED': 'true'},
                   'meeting-service': {'MEETING_PLATFORM_ACCESS_ENABLED': 'true'},
                   'notifications-2': {'NOTIFICATIONS_PARTNER_PUSH_ENABLED': 'true'}}
        with isolated_work() as work:
            self.assertEqual(sorted(cr.safe_default_violations(running)), [
                'meeting-service MEETING_PLATFORM_ACCESS_ENABLED', 'notifications-2 NOTIFICATIONS_PARTNER_PUSH_ENABLED',
                'platform-access PLATFORM_ACCESS_END_USERS_ENABLED'])
            journal(work, po.USERS, po.PUSH, po.MEETINGS)
            self.assertEqual(cr.safe_default_violations(running), [])
            running['platform-access']['PLATFORM_ACCESS_END_USERS_ENABLED'] = 'yes'
            self.assertEqual(cr.safe_default_violations(running), ['platform-access PLATFORM_ACCESS_END_USERS_ENABLED'])

    def test_a_rolled_back_activation_no_longer_widens_the_rules(self):
        with isolated_work() as work:
            journal(work, po.USERS, status='rolled-back')
            self.assertEqual(cr.safe_default_violations({'developer-platform': {'DEVELOPER_PLATFORM_USERS_API_ENABLED': 'true'}}),
                             ['developer-platform DEVELOPER_PLATFORM_USERS_API_ENABLED'])


class GatewayEdge(unittest.TestCase):
    def test_calling_edge_covers_every_rest_calling_route_and_never_the_websocket(self):
        def covered(pattern, prefixes):
            return any(pattern == x or pattern.startswith(x + '/') for x in prefixes)
        calling = [r for r in MANIFEST['routes'] if r['pattern'].startswith(('/v1/calls', '/v1/calling/'))]
        self.assertTrue(calling)
        for r in calling:
            if r['protocol'] == 'WEBSOCKET':
                self.assertFalse(covered(r['pattern'], po.CALLING_EDGE_PREFIXES), r['id'])
                self.assertIn(r['pattern'], po.CALLING_WEBSOCKETS)
            else:
                self.assertTrue(covered(r['pattern'], po.CALLING_EDGE_PREFIXES), r['id'])
        snippet = cr.gateway_snippet(cr.GATEWAY_PREFIXES + po.CALLING_EDGE_PREFIXES)
        locations = re.findall(r'location (=|\^~) (\S+) \{', snippet)
        for kind, path in locations:
            # nginx: an exact or ^~ prefix location never matches the WebSocket path.
            matches = path == '/v1/calling/signaling' if kind == '=' else '/v1/calling/signaling'.startswith(path)
            self.assertFalse(matches, path)
        self.assertNotIn('/v1/calling/signaling', [p for _, p in locations])
        self.assertFalse(set(po.CALLING_EDGE_PREFIXES) & set(cr.FAMILY_PREFIXES + cr.GATEWAY_PREFIXES))
        probed = {next(p for p in po.CALLING_EDGE_PREFIXES if path == p or path.startswith(p + '/'))
                  for _, path in po.CALLING_EDGE_PROBES}
        self.assertEqual(probed, set(po.CALLING_EDGE_PREFIXES) - {'/v1/calling/sessions'})

    def test_edge_prefixes_follow_the_journal(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families')
            self.assertEqual(cr.edge_prefixes(), cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES)
            journal(work, 'activate-gateway-families', po.USERS, po.CALLING_EDGE)
            self.assertEqual(cr.edge_prefixes(), cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                             + po.CALLING_EDGE_PREFIXES)
            journal(work, 'activate-gateway-families', po.USERS, po.MEETINGS, po.CALLING_EDGE)
            self.assertEqual(cr.edge_prefixes(), cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                             + po.CALLING_EDGE_PREFIXES + po.MEETINGS_EDGE)
        src = (REPO / 'infra/calling/production/coordinated_release.py').read_text()
        # Re-running the families step keeps the platform prefixes.
        self.assertIn('GATEWAY_PREFIXES + FAMILY_PREFIXES + platform_ops.edge_prefixes(done)', src)

    def edge_fixture(self, work):
        snippets = work / 'snippets'
        snippets.mkdir()
        snippet = snippets / 'cfmeet-api-gateway.conf'
        snippet.write_text(cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES))
        (snippets / 'cfmeet-calling-routes.conf').write_text('location ~ ^/v1/calls(?:/|$) { proxy_pass http://c; }\n'
                                                             'location ~ ^/v1/calling/ { proxy_pass http://c; }\n')
        site = work / 'site'
        site.write_text('server { %s\n    %s }' % (cr.ROUTES_ANCHOR, cr.GATEWAY_INCLUDE))
        return snippet, site

    @contextlib.contextmanager
    def edge(self, work, statuses, smoke=None, reloads=None):
        snippet, site = self.edge_fixture(work)

        def status(url, method='GET', headers=None):
            path = url[len(cr.PUBLIC):]
            text = snippet.read_text()
            routed = any(('location ^~ %s/' % p in text or 'location = %s ' % p in text)
                         and (path == p or path.startswith(p + '/'))
                         for p in po.CALLING_EDGE_PREFIXES + po.USERS_EDGE + po.MEETINGS_EDGE)
            code = statuses.get(path, 401)
            return code, ({'X-Correlation-Id': 'c'} if routed else {})

        with patch.object(cr, 'GATEWAY_SNIPPET', snippet), patch.object(cr, 'SITE', site), \
                patch.object(cr, 'Path', lambda p: work / 'snippets' if p == '/etc/nginx/snippets' else Path(p)), \
                patch.object(cr, 'http_status', status), \
                patch.object(cr, 'public_smoke', lambda: smoke or {'public_swagger': 'PASS'}), \
                patch.object(cr.p, 'reload_host_nginx', lambda: (reloads if reloads is not None else []).append(1)), \
                patch.object(cr.p, 'regression', lambda: None), \
                patch.object(cr, 'env_of', lambda n: envs()[n]), \
                patch.object(po, 'run_acceptance', lambda c, r, op: {}):
            yield snippet

    def test_calling_edge_is_an_owner_decision(self):
        with isolated_work() as work, self.edge(work, {}) as snippet:
            before = snippet.read_text()
            with self.assertRaisesRegex(RuntimeError, 'owner decision'):
                po.activate_calling_edge(cr, SHA, '5-1', '')
            self.assertEqual(snippet.read_text(), before)
            self.assertFalse((work / 'journal.json').exists())

    def test_calling_edge_moves_the_rest_routes_and_is_journaled_and_rolled_back(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-routing', 'activate-gateway-families', po.CALLS)
            reloads = []
            with self.edge(work, {'/v1/calling/signaling': 400}, reloads=reloads) as snippet:
                po.activate_calling_edge(cr, SHA, '5-1', po.CALLING_EDGE_CONFIRM)
                text = snippet.read_text()
                for prefix in po.CALLING_EDGE_PREFIXES + cr.FAMILY_PREFIXES:
                    self.assertIn('location ^~ %s/ {' % prefix, text)
                entry = json.loads((work / 'journal.json').read_text())[-1]
                self.assertEqual((entry['operation'], entry['status']), (po.CALLING_EDGE, 'done'))
                self.assertEqual(entry['platformEdge']['prefixes'], list(po.CALLING_EDGE_PREFIXES))
                self.assertTrue((work / 'calling-edge' / '5-1' / 'snippet.before').exists())
                # `rollback` picks the edge entry (no containers) and removes exactly its prefixes.
                with patch.object(cr.p, 'regression', lambda: None):
                    cr.rollback(SHA, '6-1')
                text = snippet.read_text()
                self.assertEqual(text, cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES))
                self.assertEqual(json.loads((work / 'journal.json').read_text())[-1]['status'], 'rolled-back')
            self.assertEqual(len(reloads), 2)

    def test_a_failed_calling_edge_check_restores_the_snippet(self):
        cases = [dict(statuses={'/v1/calls': 404}), dict(statuses={'/v1/calling/signaling': 200}),
                 dict(statuses={}, smoke={'public_swagger': 'FAIL'})]
        for case in cases:
            with isolated_work() as work:
                journal(work, 'activate-gateway-families', po.CALLS)
                reloads = []
                with self.edge(work, dict({'/v1/calling/signaling': 401}, **case['statuses']), case.get('smoke'),
                               reloads) as snippet:
                    before = snippet.read_text()
                    with self.assertRaisesRegex(RuntimeError, 'calling edge checks failed'):
                        po.activate_calling_edge(cr, SHA, '5-1', po.CALLING_EDGE_CONFIRM)
                    self.assertEqual(snippet.read_text(), before, case)
                entry = json.loads((work / 'journal.json').read_text())[-1]
                self.assertEqual(entry['status'], 'failed')
                self.assertEqual(len(reloads), 2)

    def test_calling_edge_refuses_an_explicit_existing_location(self):
        with isolated_work() as work, self.edge(work, {}) as snippet:
            (work / 'snippets' / 'cfmeet-calling-routes.conf').write_text('location ^~ /v1/calls/ { }')
            with self.assertRaisesRegex(RuntimeError, 'already serve: /v1/calls'):
                po.activate_calling_edge(cr, SHA, '5-1', po.CALLING_EDGE_CONFIRM)
            self.assertNotIn('/v1/calls', snippet.read_text())


class Users(unittest.TestCase):
    def test_plan_flags_caps_and_grant_receiver(self):
        apply = Capture()
        with isolated_work(), plan_env(), patch.object(cr, 'apply_activation', apply), \
                patch.object(po, 'require_gateway_edge', lambda c: None), \
                patch.object(po, 'refuse_collisions', lambda c, p: None):
            po.activate_users(cr, SHA, '1-1', '')
        call = apply.calls[0]
        self.assertEqual(call['operation'], po.USERS)
        self.assertEqual([c[0] for c in call['changes']], ['platform-access', 'developer-platform'])
        access = apply.plan['platform-access'][1]
        self.assertEqual(access['PLATFORM_ACCESS_END_USERS_ENABLED'], 'true')
        self.assertEqual(access['PLATFORM_ACCESS_MAX_END_USERS_SANDBOX'], '100')
        self.assertEqual(access['PLATFORM_ACCESS_MAX_END_USERS_TEST'], '1000')
        self.assertEqual(access['PLATFORM_ACCESS_MAX_END_USERS_PRODUCTION'], '100000')
        self.assertEqual(apply.plan['developer-platform'][1], {'DEVELOPER_PLATFORM_USERS_API_ENABLED': 'true'})

    def test_refused_before_the_developer_gateway_api(self):
        values = envs(developer_platform={})
        with plan_env(values), patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')), \
                patch.object(po, 'require_gateway_edge', lambda c: None):
            with self.assertRaisesRegex(RuntimeError, 'activate-developer-platform'):
                po.activate_users(cr, SHA, '1-1', '')

    def test_post_routes_v1_users_runs_the_acceptance_and_restores_the_edge_on_failure(self):
        for acceptance_ok in (True, False):
            with isolated_work() as work:
                journal(work, 'activate-gateway-families')
                apply = Capture(run_post=True)
                stages = []

                def acceptance(c, run_id, op):
                    stages.append(po.enabled_stages(c, op))
                    if not acceptance_ok:
                        raise RuntimeError('Platform acceptance failed')

                with GatewayEdge().edge(work, {}) as snippet, \
                        patch.object(cr, 'apply_activation', apply), patch.object(po, 'run_acceptance', acceptance), \
                        patch.object(cr, 'has_signing_identity', lambda n: None), \
                        patch.object(cr, 'tls_route_present', lambda p: None):
                    before = snippet.read_text()
                    if acceptance_ok:
                        po.activate_users(cr, SHA, '1-1', '')
                        self.assertIn('location ^~ /v1/users/ {', snippet.read_text())
                    else:
                        with self.assertRaisesRegex(RuntimeError, 'acceptance failed'):
                            po.activate_users(cr, SHA, '1-1', '')
                        self.assertEqual(snippet.read_text(), before)
                self.assertEqual(stages, [('users',)])

    def test_rollback_removes_v1_users_before_the_containers(self):
        order = []
        with isolated_work() as work:
            (work / 'journal.json').write_text(json.dumps([
                dict(id='f:0', operation='activate-gateway-families', sha=SHA, status='done', components=[]),
                dict(id='u:1', operation=po.USERS, sha=SHA, runId='1-1', status='done', components=[
                    dict(service='developer-platform', name='cfmeet-developer-platform', previous=None,
                         status='released', created=False)])]))
            snippet = work / 'gateway.conf'
            snippet.write_text(cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE))
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), \
                    patch.object(cr.p, 'reload_host_nginx', lambda: order.append('edge')), \
                    patch.object(cr, 'restore', lambda component, run_id: order.append('restore')), \
                    patch.object(cr, 'container_names', lambda: []), patch.object(cr.p, 'regression', lambda: None):
                cr.rollback(SHA, '2-1')
            self.assertEqual(snippet.read_text(), cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES))
        self.assertEqual(order, ['edge', 'restore'])


class UserTokens(unittest.TestCase):
    def test_signing_key_is_generated_once_owned_0400_mounted_and_never_printed(self):
        out = io.StringIO()
        with tempfile.TemporaryDirectory() as folder, isolated_work(), plan_env(), \
                patch.object(cr.p, 'SECRETS', Path(folder)), patch.object(cr.os, 'chown') as chown, \
                contextlib.redirect_stdout(out):
            apply = Capture()
            with patch.object(cr, 'apply_activation', apply):
                po.activate_user_tokens(cr, SHA, '2-1', '')
                key = Path(folder) / po.USER_TOKEN_KEY
                first = key.read_text()
                po.activate_user_tokens(cr, SHA, '2-2', '')
                self.assertEqual(key.read_text(), first)  # never regenerated over an existing key
            self.assertTrue(first.startswith('-----BEGIN PRIVATE KEY-----'))
            self.assertEqual(oct(key.stat().st_mode & 0o777), '0o400')
            chown.assert_called_with(key, cr.SERVICES['platform-access']['uid'], cr.SERVICES['platform-access']['uid'])
            import subprocess
            text = subprocess.run(['openssl', 'pkey', '-noout', '-text'], input=first, capture_output=True, text=True)
            self.assertIn('ED25519', (text.stdout + text.stderr).upper())
        body = first.strip().splitlines()[1]
        self.assertNotIn(body, out.getvalue())
        emitted = [json.loads(line) for line in out.getvalue().splitlines() if line.startswith('{')]
        self.assertEqual([e['userTokenSigningKey']['generated'] for e in emitted if 'userTokenSigningKey' in e], [True, False])
        service, overrides, extra = apply.plan['platform-access']
        self.assertEqual(overrides, {'PLATFORM_ACCESS_USER_TOKENS_ENABLED': 'true',
                                     'PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED': 'true',
                                     po.USER_TOKEN_KEY_VAR: po.USER_TOKEN_KEY_TARGET})
        self.assertEqual(extra['mounts'], [(key, po.USER_TOKEN_KEY_TARGET)])
        self.assertEqual(apply.plan['developer-platform'][1], {'DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED': 'true',
                                                               'DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED': 'true'})
        self.assertEqual(apply.plan['api-gateway'][1], {'API_GATEWAY_USER_TOKENS_ENABLED': 'true',
                                                        'API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED': 'true'})
        self.assertEqual([c[0] for c in apply.calls[0]['changes']], ['platform-access', 'developer-platform', 'api-gateway'])

    def test_bundles_recreate_platform_access_with_every_verifier_issuer(self):
        receivers = {r for op, r, _, _ in cr.GRANTS if op == po.TOKENS}
        self.assertEqual(receivers, {'platform-access'})
        issuers = {i for op, _, i, _ in cr.GRANTS if op == po.TOKENS}
        self.assertEqual(issuers, {'messaging', 'calling', 'meeting-service', 'api-gateway'})


class Chat(unittest.TestCase):
    def test_needs_session_devices_and_switches_only_messaging(self):
        values = envs(messaging={'CF_PLATFORM_ACCESS_BASE_URL': 'https://x'})
        with plan_env(values), patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'MESSAGING_SESSION_DEVICES_ENABLED'):
                po.activate_chat(cr, SHA, '3-1', '')
        apply = Capture()
        with plan_env(), patch.object(cr, 'apply_activation', apply):
            po.activate_chat(cr, SHA, '3-1', '')
        self.assertEqual(apply.calls[0]['changes'], [('messaging', {'MESSAGING_PARTNER_SESSION_DEVICES_ENABLED': 'true',
                                                                    'MESSAGING_MESSAGE_SENT_EXPORT_ENABLED': 'true'})])

    def test_message_sent_export_only_with_the_usage_export_and_base_url_only_when_missing(self):
        values = envs(messaging={'MESSAGING_SESSION_DEVICES_ENABLED': 'true'})
        apply = Capture()
        with plan_env(values), patch.object(cr, 'apply_activation', apply), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            po.activate_chat(cr, SHA, '3-1', '')
        overrides = apply.plan['messaging'][1]
        self.assertNotIn('MESSAGING_MESSAGE_SENT_EXPORT_ENABLED', overrides)
        self.assertEqual(overrides['CF_PLATFORM_ACCESS_BASE_URL'], cr.private_url(8443))
        self.assertIn('messageSentExport', out.getvalue())


class Calls(unittest.TestCase):
    def test_exchange_flag_only_and_no_edge_change(self):
        apply = Capture()
        with plan_env(), patch.object(cr, 'apply_activation', apply):
            po.activate_calls(cr, SHA, '4-1', '')
        self.assertEqual(apply.calls[0]['changes'], [('calling', {'CALLING_USER_TOKEN_EXCHANGE_ENABLED': 'true'})])
        self.assertNotIn(po.CALLS, po.EDGE)


class Push(unittest.TestCase):
    def run_push(self, values, private_key_mounted):
        apply = Capture(run_post=True)
        egress = []
        with plan_env(values), patch.object(cr, 'apply_activation', apply), \
                patch.object(notif_ops, 'sealing_key', lambda c: ('notif-hpke-2026-11-1', Path('/k'), 'notif-hpke-2026-11-1:PUB')), \
                patch.object(notif_ops, 'require_egress', lambda c, urls, kind: egress.append((urls, kind))), \
                patch.object(po, 'run_acceptance', lambda c, r, op: egress.append(op)), \
                contextlib.redirect_stdout(io.StringIO()):
            po.activate_push(cr, SHA, '5-1', '')
        return apply, egress

    def test_plan_seals_to_the_service_key_and_checks_provider_egress(self):
        values = envs(notifications={}, notifications_2={'NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE': notif_ops.HPKE_TARGET})
        apply, egress = self.run_push(values, False)
        plan = apply.plan
        self.assertEqual(list(plan), ['notifications', 'notifications-2', 'developer-platform', 'messaging', 'calling'])
        self.assertEqual(plan['notifications'][1]['NOTIFICATIONS_PARTNER_PUSH_ENABLED'], 'true')
        self.assertEqual(plan['notifications'][2]['mounts'], [(Path('/k'), notif_ops.HPKE_TARGET)])
        self.assertEqual(plan['notifications-2'][1], {'NOTIFICATIONS_PARTNER_PUSH_ENABLED': 'true'})
        self.assertEqual(plan['notifications-2'][2], {})
        console = plan['developer-platform'][1]
        self.assertEqual(console['DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED'], 'true')
        self.assertEqual(console['CF_NOTIFICATIONS_SEALING_KEY'], 'notif-hpke-2026-11-1:PUB')
        self.assertEqual(console['CF_NOTIFICATIONS_BASE_URL'], cr.private_url(cr.SERVICES['notifications']['tls']))
        self.assertEqual(plan['messaging'][1], {'MESSAGING_PARTNER_PUSH_ENABLED': 'true'})
        self.assertEqual(plan['calling'][1], {'CALLING_PARTNER_PUSH_ENABLED': 'true'})
        self.assertEqual(egress, [(po.PUSH_PROVIDER_URLS, 'partner push'), po.PUSH])
        self.assertIn('https://api.push.apple.com/', po.PUSH_PROVIDER_URLS)

    def test_refused_while_a_producer_has_no_notification_service_target(self):
        values = envs(calling={'CF_PLATFORM_ACCESS_BASE_URL': 'https://x'})
        with plan_env(values), patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'calling has no CF_NOTIFICATIONS_BASE_URL'):
                po.activate_push(cr, SHA, '5-1', '')


class Meetings(unittest.TestCase):
    def test_plan_flags_grants_upstream_and_the_partner_edge(self):
        values = envs(meeting_service={'CF_SERVICE_TRUST_BUNDLE_FILE': '/t'})
        apply = Capture()
        refused = []
        with plan_env(values), patch.object(cr, 'apply_activation', apply), \
                patch.object(po, 'require_gateway_edge', lambda c: None), \
                patch.object(po, 'refuse_collisions', lambda c, p: refused.append(p)):
            po.activate_meetings(cr, SHA, '6-1', '')
        plan = apply.plan
        self.assertEqual(plan['platform-access'][1], {'PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED': 'true'})
        meeting = plan['meeting-service'][1]
        for flag in ('MEETING_PLATFORM_ACCESS_ENABLED', 'MEETING_PLATFORM_USER_JOIN_ENABLED', 'MEETING_PLATFORM_CHAT_ENABLED'):
            self.assertEqual(meeting[flag], 'true')
        self.assertEqual(meeting['CF_PLATFORM_ACCESS_BASE_URL'], cr.private_url(8443))
        self.assertEqual(meeting['CF_MESSAGING_BASE_URL'], cr.private_url(8444))
        self.assertEqual(plan['api-gateway'][1], {'CF_MEETING_SERVICE_BASE_URL': cr.private_url(8448)})
        self.assertEqual(po.EDGE[po.MEETINGS], ('/v1/meetings',))
        self.assertEqual(refused, [po.MEETINGS_EDGE])

    def test_refused_without_a_receiver_trust_bundle(self):
        with plan_env(envs(meeting_service={})), \
                patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'trust bundle'):
                po.activate_meetings(cr, SHA, '6-1', '')

    def test_refused_before_the_gateway_edge(self):
        with isolated_work() as work, plan_env(), \
                patch.object(cr, 'GATEWAY_SNIPPET', work / 'missing.conf'), \
                patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'activate-gateway-routing'):
                po.activate_meetings(cr, SHA, '6-1', '')

    def test_the_edge_moves_only_the_partner_family_never_first_party_api_v2(self):
        snippet = cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.MEETINGS_EDGE)
        locations = [p for _, p in re.findall(r'location (=|\^~) (\S+) \{', snippet)]
        self.assertIn('/v1/meetings', locations)
        self.assertIn('/v1/meetings/', locations)
        self.assertFalse([p for p in locations if p.startswith('/api/')])
        self.assertFalse(set(po.MEETINGS_EDGE) & set(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                                                     + po.CALLING_EDGE_PREFIXES))
        # The probes: the partner route and its first-party twin (a platform-credential
        # Meeting Service route); every gateway route under /v1/meetings is such an alias.
        method, path = po.MEETINGS_DIRECT_PROBE
        twin = next(r for r in MANIFEST['routes'] if (r['method'], r['pattern']) == (method, path))
        self.assertIn('PLATFORM_CREDENTIAL', twin['auth'])
        self.assertEqual(po.MEETINGS_EDGE_PROBE, (method, '/v1/meetings' + path[len('/api/v2'):]))
        for r in MANIFEST['routes']:
            if r['pattern'] == '/v1/meetings' or r['pattern'].startswith('/v1/meetings/'):
                self.assertEqual((r['upstream'], r['auth']), ('cfmeet.meeting-service', ['PLATFORM_CREDENTIAL']), r['id'])
                self.assertEqual(r['upstreamPath'], '/api/v2' + r['pattern'][len('/v1/meetings'):], r['id'])

    def run_post(self, work, statuses, acceptance_ok=True, smoke=None):
        apply = Capture(run_post=True)
        stages = []

        def acceptance(c, run_id, op):
            stages.append(po.enabled_stages(c, op))
            if not acceptance_ok:
                raise RuntimeError('Platform acceptance failed')

        reloads = []
        with GatewayEdge().edge(work, statuses, smoke, reloads) as snippet, \
                patch.object(cr, 'apply_activation', apply), patch.object(po, 'run_acceptance', acceptance), \
                patch.object(cr, 'has_signing_identity', lambda n: None), \
                patch.object(cr, 'tls_route_present', lambda p: None), \
                patch.object(cr, 'env_of', lambda n: envs()[n]), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            before = snippet.read_text()
            error = None
            try:
                po.activate_meetings(cr, SHA, '6-1', '')
            except RuntimeError as e:
                error = e
            after = snippet.read_text()
        return before, after, error, stages, reloads, out.getvalue()

    def test_post_routes_v1_meetings_proves_api_v2_is_direct_and_runs_the_acceptance(self):
        with isolated_work() as work:
            journal(work, 'activate-gateway-families', po.USERS, po.TOKENS, po.CHAT)
            before, after, error, stages, reloads, out = self.run_post(work, {})
            self.assertIsNone(error)
            self.assertNotIn('/v1/meetings', before)
            self.assertIn('location ^~ /v1/meetings/ {', after)
            self.assertIn('location = /v1/meetings {', after)
            self.assertNotIn('/api/v2', after)
            self.assertEqual(after, cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                                                       + po.MEETINGS_EDGE))
            self.assertEqual(stages, [('users', 'tokens', 'chat', 'meetings')])
            self.assertEqual(len(reloads), 1)
            self.assertIn('platformMeetingsEdge', out)
            self.assertIn('answered by the gateway', out)
            self.assertIn('still direct', out)

    def test_a_failed_meetings_edge_check_or_acceptance_restores_the_snippet(self):
        cases = [dict(statuses={'/v1/meetings/meeting-spaces': 404}),
                 dict(statuses={'/v1/meetings/meeting-spaces': 200}),
                 dict(statuses={'/api/v2/meeting-spaces': 404}),
                 dict(statuses={}, smoke={'public_swagger': 'FAIL'}),
                 dict(statuses={}, acceptance_ok=False)]
        for case in cases:
            with isolated_work() as work:
                journal(work, 'activate-gateway-families', po.USERS, po.TOKENS, po.CHAT)
                before, after, error, stages, reloads, _ = self.run_post(
                    work, case['statuses'], case.get('acceptance_ok', True), case.get('smoke'))
                self.assertIsNotNone(error, case)
                self.assertRegex(str(error), 'edge checks failed' if case.get('acceptance_ok', True)
                                 else 'acceptance failed')
                self.assertEqual(after, before, case)
                self.assertEqual(len(reloads), 2, case)

    def test_a_first_party_api_v2_answer_from_the_gateway_fails_the_check(self):
        calls = []

        def served(c, method, path):
            calls.append((method, path))
            return 401, True  # both answered by the gateway: /api/v2 moved by mistake
        with patch.object(po, 'served_by_gateway', served), patch.object(cr, 'public_smoke', lambda: {}):
            checks = po.meetings_edge_checks(cr)
        self.assertEqual(calls, [po.MEETINGS_EDGE_PROBE, po.MEETINGS_DIRECT_PROBE])
        self.assertEqual(sorted(checks.values()), ['FAIL 401 (gateway)', 'PASS'])

    def test_refuses_an_explicit_existing_v1_meetings_location(self):
        with isolated_work() as work, GatewayEdge().edge(work, {}) as snippet, \
                patch.object(cr, 'has_signing_identity', lambda n: None), \
                patch.object(cr, 'tls_route_present', lambda p: None), \
                patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            (work / 'snippets' / 'cfmeet-calling-routes.conf').write_text('location ^~ /v1/meetings/ { }')
            with self.assertRaisesRegex(RuntimeError, 'already serve: /v1/meetings'):
                po.activate_meetings(cr, SHA, '6-1', '')
            self.assertNotIn('/v1/meetings', snippet.read_text())

    def test_rollback_removes_v1_meetings_before_the_containers(self):
        order = []
        with isolated_work() as work:
            (work / 'journal.json').write_text(json.dumps([
                dict(id='f:0', operation='activate-gateway-families', sha=SHA, status='done', components=[]),
                dict(id='u:1', operation=po.USERS, sha=SHA, status='done', components=[]),
                dict(id='m:2', operation=po.MEETINGS, sha=SHA, runId='6-1', status='done', components=[
                    dict(service='meeting-service', name='cfmeet-meeting-service', previous=None,
                         status='released', created=False)])]))
            snippet = work / 'gateway.conf'
            snippet.write_text(cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE
                                                  + po.MEETINGS_EDGE))
            with patch.object(cr, 'GATEWAY_SNIPPET', snippet), \
                    patch.object(cr.p, 'reload_host_nginx', lambda: order.append('edge')), \
                    patch.object(cr, 'restore', lambda component, run_id: order.append('restore')), \
                    patch.object(cr, 'container_names', lambda: []), patch.object(cr.p, 'regression', lambda: None):
                cr.rollback(SHA, '7-1')
            self.assertEqual(snippet.read_text(),
                             cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES + po.USERS_EDGE))
        self.assertEqual(order, ['edge', 'restore'])


class AcceptanceWiring(unittest.TestCase):
    def test_post_check_runs_every_recorded_stage_plus_the_current_one_and_fails_closed(self):
        import platform_acceptance
        seen = []
        with isolated_work() as work:
            journal(work, po.USERS, po.TOKENS, po.CALLS)

            def run(run_id, stages, emit):
                seen.append(stages)
                return {'a': 'PASS', 'b': 'FAIL'} if 'meetings' in stages else {'a': 'PASS'}
            with patch.object(platform_acceptance, 'run', run):
                po.run_acceptance(cr, '7-1', po.CHAT)
                with self.assertRaisesRegex(RuntimeError, 'acceptance failed after activate-platform-meetings'):
                    po.run_acceptance(cr, '7-1', po.MEETINGS)
        self.assertEqual(seen, [('users', 'tokens', 'chat', 'calls'), ('users', 'tokens', 'calls', 'meetings')])
        with patch.object(platform_acceptance, 'run', lambda r, s, e: {}):
            with self.assertRaisesRegex(RuntimeError, 'acceptance failed'):
                po.run_acceptance(cr, '7-1', po.USERS)  # no checks is never a pass

    def test_platform_acceptance_operation_requires_users_and_reports(self):
        import platform_acceptance
        with isolated_work() as work:
            with self.assertRaisesRegex(RuntimeError, 'Acceptance needs activate-platform-users'):
                cr.run(po.ACCEPTANCE, SHA, '8-1')
            journal(work, po.USERS, po.TOKENS)
            with patch.object(platform_acceptance, 'run', lambda r, s, e: {'x': 'PASS'}) as _, \
                    contextlib.redirect_stdout(io.StringIO()) as out:
                cr.run(po.ACCEPTANCE, SHA, '8-1')
            self.assertIn('platformAcceptanceSummary', out.getvalue())
            with patch.object(platform_acceptance, 'run', lambda r, s, e: {'x': 'FAIL'}), \
                    contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, 'Platform acceptance failed'):
                    cr.run(po.ACCEPTANCE, SHA, '8-1')
            with self.assertRaisesRegex(RuntimeError, 'no confirmation'):
                cr.run(po.ACCEPTANCE, SHA, '8-1', 'x')

    def test_activations_refuse_to_run_out_of_order(self):
        with isolated_work() as work:
            journal(work, 'activate-developer-console', 'activate-gateway-families')
            with patch.object(cr.p, 'identity', side_effect=AssertionError('must not start')):
                for op in (po.TOKENS, po.CHAT, po.CALLS, po.PUSH, po.MEETINGS, po.CALLING_EDGE):
                    with self.assertRaisesRegex(RuntimeError, 'Activation order'):
                        cr.run(op, SHA, '9-1')

    def test_relay_port_matches_the_gateway_relay(self):
        import platform_acceptance
        self.assertEqual(platform_acceptance.RELAY, 'http://127.0.0.1:%d' % cr.GATEWAY_RELAY_PORT)
        self.assertEqual(platform_acceptance.PUBLIC, cr.PUBLIC)
        self.assertEqual(platform_acceptance.STAGES, tuple(po.STAGES[op] for op in po.STAGE_ORDER))


if __name__ == '__main__':
    unittest.main()
