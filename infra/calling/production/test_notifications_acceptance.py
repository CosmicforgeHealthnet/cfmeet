"""Offline tests of the Notification Service production acceptance (no network)."""
import json
import re
import unittest
import uuid

import coordinated_release as cr
import notifications_acceptance as na
import organization_acceptance as oa
from test_organization_acceptance import FakeApi, accounts, SECRET_WORDS


class FakeNotifications(FakeApi):
    """The organization fake plus an in-memory Notification Service: invitations and role
    changes become inbox items of the affected account unless its preference is off."""

    def __init__(self, people, deliver=True, duplicate=False, honour_preferences=True, leak=False):
        super().__init__(people)
        self.inbox = {p['userId']: [] for p in people}
        self.prefs = {p['userId']: {'version': 0, 'organizationUpdates': True} for p in people}
        self.deliver, self.duplicate, self.honour, self.leak = deliver, duplicate, honour_preferences, leak

    def notify(self, user_id):
        if not self.deliver:
            return
        # Document 4 §7.2 rule 7: a category switched off records the in-app item silently.
        silent = self.honour and not self.prefs[user_id]['organizationUpdates']
        for _ in range(2 if self.duplicate else 1):
            self.inbox[user_id].append({'id': str(uuid.uuid4()), 'category': 'ORGANIZATION_UPDATES',
                                        'readAt': None, 'dismissedAt': None, 'silent': silent})

    def __call__(self, method, url, headers, data):
        path = url[len(oa.PUBLIC):]
        user = self.user(headers)
        a, b, c = list(self.people.values())
        body = json.loads(data) if data else {}
        if path.startswith('/api/v2/notifications'):
            self.requests.append((method, oa.template(path)))
            mine = self.inbox[user['userId']]
            if path.startswith('/api/v2/notifications?'):
                items = [i for i in mine if not i['dismissedAt']]
                if self.leak and user is c:
                    items += self.inbox[b['userId']]
                return 200, {'items': items, 'unreadCount': 0}
            if path == '/api/v2/notifications/unread-count':
                return 200, {'unreadCount': sum(1 for i in mine if not i['readAt'] and not i['dismissedAt']
                                                and not i['silent'])}
            if path in ('/api/v2/notifications/read', '/api/v2/notifications/dismiss'):
                field = 'readAt' if path.endswith('read') else 'dismissedAt'
                for i in mine:
                    if i['id'] in body['ids']:
                        i[field] = i[field] or 'now'
                        if field == 'dismissedAt':
                            i['readAt'] = i['readAt'] or 'now'
                return 200, {'updated': 1}
            if path == '/api/v2/notifications/preferences':
                p = self.prefs[user['userId']]
                if method == 'PUT':
                    assert body['expectedVersion'] == p['version']
                    p.update(body['categories'])
                    p['version'] += 1
                return 200, {'version': p['version'], 'categories': {'organizationUpdates': p['organizationUpdates']}}
        status, answer = super().__call__(method, url, headers, data)
        if path == '/api/v2/organization-invitations/accept' and status < 300:
            self.notify(a['userId'])  # invitation.accepted -> the inviter
        if re.fullmatch(r'/api/v2/organizations/([^/]+)/members/[^/]+', path) and method == 'PATCH' and status < 300:
            self.notify(b['userId'])  # membership.role_changed -> the member
            answer = {'id': str(uuid.uuid4()), 'version': 2, 'status': 'ACTIVE', 'role': body.get('role')}
        return status, answer


def run(api):
    from unittest import mock
    lines = []
    clock = [0.0]

    def sleep(seconds):
        clock[0] += seconds
    journey = na.Journey(accounts_for(api), lambda kind, value: lines.append((kind, value)), sleep=sleep)
    journey.transport = api
    with mock.patch.object(oa.time, 'monotonic', side_effect=lambda: clock[0]):
        return journey.run(), lines


def accounts_for(api):
    return list(api.people.values())


class Acceptance(unittest.TestCase):
    def setUp(self):
        # Passwordless sign-in (organization_acceptance.Journey.login): the controlled code
        # replacement needs the VPS (Redis).
        from unittest import mock
        import app_acceptance
        stub = mock.patch.object(app_acceptance, 'controlled_code', lambda key, url=None: '024680')
        stub.start()
        self.addCleanup(stub.stop)

    def test_the_full_journey_passes_and_records_no_secret(self):
        api = FakeNotifications(accounts())
        checks, lines = run(api)
        self.assertEqual(checks, {name: 'PASS' for name in na.CHECKS})
        text = json.dumps(lines)
        for word in SECRET_WORDS:
            self.assertNotIn(word, text)
        # Cleanup: both organizations archived, B's preference back on.
        self.assertEqual(sum(1 for m, r in api.requests if r == '/api/v2/organizations/{id}/archive'), 2)
        self.assertTrue(all(p['organizationUpdates'] for p in api.prefs.values()))

    def test_no_delivery_fails_the_first_delivery_check(self):
        checks, _ = run(FakeNotifications(accounts(), deliver=False))
        self.assertEqual(checks['inviter_is_told_the_invitation_was_accepted'], 'FAIL')
        self.assertEqual(checks['unread_count_counts_it'], 'NOT_RUN')

    def test_a_duplicate_notification_fails(self):
        checks, _ = run(FakeNotifications(accounts(), duplicate=True))
        self.assertEqual(checks['inviter_is_told_the_invitation_was_accepted'], 'FAIL')

    def test_a_leak_to_another_account_fails_isolation(self):
        checks, _ = run(FakeNotifications(accounts(), leak=True))
        self.assertEqual(checks['other_accounts_see_nothing_and_cannot_change_it'], 'FAIL')

    def test_an_ignored_preference_fails(self):
        checks, _ = run(FakeNotifications(accounts(), honour_preferences=False))
        self.assertEqual(checks['preference_off_suppresses_and_on_restores'], 'FAIL')
        self.assertEqual(checks['role_change_reaches_the_member_once'], 'PASS')

    def test_run_asks_app_acceptance_for_accounts_it_accepts(self):
        """The real argument checks of app_acceptance.accounts (production run 44173524918 failed
        on them: auth security checks are for the organization namespace only)."""
        import tempfile
        from unittest import mock
        import app_acceptance
        seen = []

        def accounts(namespace=None, count=3, security_checks=False, keep_sessions=False):
            with mock.patch.object(app_acceptance.p, 'SECRETS', cr.Path(tempfile.mkdtemp())), \
                    mock.patch.object(app_acceptance.p, 'write_private', lambda *a: None), \
                    mock.patch.object(app_acceptance, 'redis_url', lambda: None), \
                    mock.patch.object(app_acceptance, 'sign_in',
                                      lambda a, url: {'is_new_account': True, 'user': {'id': str(uuid.uuid4())}}):
                seen.append(namespace)
                return real(namespace, count, security_checks, keep_sessions)
        real = app_acceptance.accounts
        with mock.patch.object(app_acceptance, 'accounts', accounts), \
                mock.patch.object(na.Journey, 'run', lambda self: {}):
            na.run('44173524918-1', lambda *a: None)
        self.assertEqual(seen, ['notifications-44173524918-1'])

    def test_the_operation_is_gated_on_the_producer_and_the_routing_row(self):
        self.assertIn('notifications-acceptance', cr.OPERATIONS)
        src = (cr.Path(__file__).resolve().parent / 'coordinated_release.py').read_text()
        self.assertIn("'activate-organizations-publications'", src)
        workflow = (cr.Path(__file__).resolve().parents[3] / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('notifications-acceptance', workflow)


if __name__ == '__main__':
    unittest.main()
