import re
import unittest
from unittest.mock import patch

import developer_acceptance as da


class FakeApi:
    """Just enough of the public API for the acceptance flow (paths, statuses, fields)."""

    def __init__(self, delivery_status='SUCCEEDED'):
        self.n = 0
        self.calls = []
        self.revoked = set()
        self.delivery_status = delivery_status
        self.hooks = {}

    def id(self):
        self.n += 1
        return 'id%d' % self.n

    def __call__(self, client, method, path, body, expected, token, idempotent):
        bearer = token or client.token
        self.calls.append((method, path.split('?')[0], bearer))
        client.last_headers = {'cache-control': 'no-store', 'x-request-id': 'r'}
        status, value = self.route(method, path, body, bearer)
        if status not in expected:
            raise da.Failed('%s %s %d' % (method, path, status))
        return status, value

    def route(self, method, path, body, bearer):
        owner = (bearer or '').split('-')[0]
        if path == '/api/v1/auth/login':
            return 200, {'access_token': body['email'].split('@')[0] + '-tok'}
        if bearer and bearer.startswith('cfmk_'):
            if bearer in self.revoked or 'invalid' in bearer:
                return 401, {}
            if method == 'POST' and path == '/v1/webhooks':
                return 403, {'error': {'code': 'INSUFFICIENT_SCOPE'}}
            if path == '/v1/webhooks':
                return 200, {'data': [{'webhookId': h} for h, env in self.hooks.items() if env == bearer.split('_')[2]]}
            return 200, {}
        if method == 'POST' and path == '/v1/organizations':
            return 201, {'id': owner + 'org', 'myRole': 'OWNER', 'state': 'ACTIVE'}
        m = re.fullmatch(r'/v1/organizations/(\w+)/projects', path)
        if m:
            return 201, {'id': owner + 'proj'}
        m = re.fullmatch(r'/v1/projects/(\w+)/environments', path)
        if m:
            if body['kind'] == 'PRODUCTION':
                return 422, {}
            return 201, {'id': owner + 'env', 'kind': 'SANDBOX', 'projectId': m.group(1)}
        if re.fullmatch(r'/v1/projects/\w+/scopes', path):
            return 200, {}
        if path.endswith('/service-accounts'):
            if not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,62}', body['name']):
                return 400, {'error': {'code': 'VALIDATION_ERROR'}}
            return 201, {'id': self.id()}
        if path.endswith('/credentials') and method == 'POST':
            env = path.split('/')[3]
            return 201, {'secret': 'cfmk_sbx_%s_s' % env, 'credential': {'id': self.id(), 'scopes': body['scopes']}}
        if path.endswith('/revoke'):
            env = path.split('/')[3]
            self.revoked.add('cfmk_sbx_%s_s' % env)
            return 200, {}
        m = re.fullmatch(r'/v1/environments/(\w+)/webhooks', path)
        if m and method == 'POST':
            hook = self.id()
            self.hooks[{'webhookId': hook}['webhookId']] = m.group(1)
            return 201, {'webhook': {'webhookId': hook, 'version': 1, 'status': 'ACTIVE'}, 'signingSecret': 'x'}
        if m:
            return (404, {'error': {'code': 'NOT_FOUND'}}) if not m.group(1).startswith(owner) else (200, {})
        if re.fullmatch(r'/v1/(projects|environments)/\w+', path.split('?')[0]):
            return (404, {'error': {'code': 'NOT_FOUND'}}) if owner not in path else (200, {})
        if path.split('?')[0].endswith('/deliveries'):
            return 200, {'items': [{'deliveryId': 'd1', 'eventType': 'credential.expiring_soon',
                                    'status': self.delivery_status, 'lastHttpStatus': 204, 'attemptCount': 1}]}
        if path.endswith('/attempts'):
            return 200, {'items': [{'httpStatus': 204}]}
        if method in ('POST', 'DELETE'):
            return 200, {}
        return 404, {}


class AcceptanceFlow(unittest.TestCase):
    def run_flow(self, api):
        accounts = [{'email': 'devA@example.invalid', 'password': 'p'}, {'email': 'devB@example.invalid', 'password': 'p'}]
        with patch.object(da.app_acceptance, 'accounts', lambda ns, count: accounts), \
                patch.object(da.Client, '_call', lambda self, *a: api(self, *a)), \
                patch.object(da.time, 'sleep', lambda s: None):
            return da.run('77-1', 'https://receiver.example/204')

    def test_full_journey_passes_and_cleans_up(self):
        api = FakeApi()
        checks = self.run_flow(api)
        self.assertTrue(checks and all(v == 'PASS' for v in checks.values()), checks)
        self.assertGreaterEqual(len(checks), 20)
        closing = [c for c in api.calls if c[1].endswith(('/close', '/archive'))]
        self.assertEqual(len(closing), 4)  # both organizations closed, both projects archived
        self.assertTrue(any(c[0] == 'DELETE' and '/webhooks/' in c[1] for c in api.calls))
        # the webhook exists before tenant A's credential (no missed lifecycle event)
        order = [c[1] for c in api.calls]
        self.assertLess(order.index('/v1/environments/devAenv/webhooks'),
                        next(i for i, p in enumerate(order) if p.endswith('/credentials')))

    def test_failed_delivery_is_recorded_other_checks_still_run_and_cleanup_happens(self):
        api = FakeApi(delivery_status='FAILED')
        checks = self.run_flow(api)
        failed = [k for k, v in checks.items() if v == 'FAIL']
        self.assertEqual(len(failed), 1, failed)
        self.assertIn('SUCCEEDED', failed[0])
        self.assertTrue(any(k.startswith('revocation') and v == 'PASS' for k, v in checks.items()))
        self.assertTrue(any(c[1].endswith('/close') for c in api.calls))


if __name__ == '__main__':
    unittest.main()
