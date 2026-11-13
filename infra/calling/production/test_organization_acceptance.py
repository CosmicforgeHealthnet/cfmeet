"""Offline tests of the organization production acceptance (no network)."""
import json
import re
import unittest
import uuid
from unittest.mock import patch

import coordinated_release as cr
import organization_acceptance as oa

SHA = 'a' * 40
SECRET_WORDS = ('access_token', 'tok-', 'invitation-secret', 'password', 'Aa1!', 'email', '@example.invalid', 'Bearer', 'acceptance one')


def accounts():
    return [dict(username='u' + x, email='u%s@example.invalid' % x, password='Aa1!secret' + x,
                 userId=str(uuid.uuid4())) for x in 'abc']


class FakeApi:
    """Answers every request of the journey like the real public API would, recording only the
    requests (method, path) so the test can check what reaches the transport."""

    def __init__(self, people, fail_route=None):
        self.people = {p['email']: p for p in people}
        self.fail_route = fail_route
        self.requests = []
        self.tokens = {}
        self.orgs = {}
        self.conv = str(uuid.uuid4())
        self.dept = str(uuid.uuid4())

    def user(self, headers):
        token = headers.get('Authorization', '')[len('Bearer '):]
        return self.tokens.get(token)

    def __call__(self, method, url, headers, data):
        path = url[len(oa.PUBLIC):]
        route = oa.template(path)
        self.requests.append((method, route))
        body = json.loads(data) if data else {}
        if self.fail_route and route == self.fail_route[1] and method == self.fail_route[0]:
            return 500, {'error': {'code': 'INTERNAL_ERROR'}}
        user = self.user(headers)
        a, b, c = list(self.people.values())
        if path == '/api/v1/auth/login':
            token = 'tok-' + uuid.uuid4().hex
            self.tokens[token] = self.people[body['email']]
            return 200, {'access_token': token}
        if path == '/v1/realtime/tickets':
            return 201, {'ticket': 't'}
        if path == '/api/v2/me/profile':
            return 200, {}
        if path == '/api/v2/messaging/me':
            return 200, {'principalId': str(uuid.UUID(int=int(user['userId'].replace('-', ''), 16) ^ 1))}
        if path == '/api/v2/messaging/relay/v1/conversations':
            return 200, {'conversations': []}
        if path == '/api/v2/messaging/session-tokens':
            return 201, {'token': 'personal'}
        if path == '/api/v2/organizations' and method == 'POST':
            owner = user['userId']
            org = next((o for o, v in self.orgs.items() if v['slug'] == body['slug']), None)
            if not org:
                org = str(uuid.uuid4())
                self.orgs[org] = dict(slug=body['slug'], owner=owner, status='ACTIVE', members={owner: 'OWNER'},
                                      tenant=str(uuid.uuid4()))
            return 201, {'id': org, 'lifecycleStatus': 'ACTIVE', 'creationSource': 'SELF_SERVICE'}
        if path == '/api/v2/me/organizations':
            return 200, [{'organization': {'id': o}} for o, v in self.orgs.items() if user['userId'] in v['members']]
        if path in ('/api/v2/me/workspaces',):
            items = [{'organizationId': o, 'role': v['members'][user['userId']], 'provisioningState': 'READY'}
                     for o, v in self.orgs.items() if user['userId'] in v['members']]
            return 200, {'items': items}
        m = re.fullmatch(r'/api/v2/me/workspaces/([^/]+)/capabilities', path)
        if m:
            role = self.orgs[m.group(1)]['members'][user['userId']]
            return 200, {'features': {'messaging': True, 'calling': True}, 'capabilities': [
                {'capability': 'calling.enterprise.manage', 'granted': role == 'OWNER'}]}
        if path == '/api/v2/organization-invitations/accept':
            if user is not b:
                return 403, {'error': {'code': 'PERMISSION_DENIED'}}
            org = next(iter(self.orgs))
            self.orgs[org]['members'][b['userId']] = 'MEMBER'
            return 200, {'id': str(uuid.uuid4()), 'version': 1, 'status': 'ACTIVE', 'userId': b['userId'],
                         'role': 'MEMBER'}
        m = re.fullmatch(r'/api/v2/organizations/([^/]+)(/.*)?', path)
        org_id, rest = m.group(1), (m.group(2) or '')
        org = self.orgs[org_id]
        member = user['userId'] in org['members'] and org['status'] == 'ACTIVE'
        if rest.startswith('/messaging/relay/') or rest.startswith('/calling/') or rest in (
                '/members', '/teams', '/audit-events', '/messaging/session-tokens') or rest.startswith('/audit'):
            if not member:
                return 403, {'error': {'code': 'MEMBERSHIP_REQUIRED'}}
        if rest == '/provisioning':
            return 200, {'provisioningEnabled': True, 'provisioningState': 'READY',
                         'scopes': [{'mappingState': 'READY', 'tenantId': org['tenant']}]}
        if rest == '/members':
            return 200, [{'id': str(uuid.uuid4()), 'userId': u, 'status': 'ACTIVE', 'role': r}
                         for u, r in org['members'].items()]
        if rest == '/invitations':
            return 201, {'token': 'invitation-secret', 'status': 'PENDING'}
        if rest == '/teams':
            return 201, {'id': str(uuid.uuid4())}
        if rest.endswith('/members') and '/teams/' in rest:
            if method == 'GET':
                return 200, {'items': [{'userId': b['userId'], 'status': 'ACTIVE'}]}
            return 201, {}
        if rest == '/messaging/members/resolve':
            return 200, {'people': [{'userId': u, 'principalId': str(uuid.uuid4())}
                                    for u in body['userIds'] if u in org['members']]}
        if rest == '/messaging/session-tokens':
            return 201, {'token': 'org'}
        if rest.startswith('/messaging/relay/'):
            r = rest[len('/messaging/relay/'):].split('?')[0]
            if r == 'v1/conversations' and method == 'POST':
                return 201, {'conversationId': self.conv}
            if r == 'v1/conversations':
                convs = [{'conversationId': self.conv, 'inbox': {'unreadCount': 0 if getattr(self, 'read', 0) else 1}}]
                return 200, {'conversations': convs if org_id == next(iter(self.orgs)) else []}
            if r.endswith('/messages') and method == 'GET':
                if org_id != next(iter(self.orgs)):
                    return 404, {}
                return 200, {'messages': [{'sequence': 1, 'managed': {'text': 'acceptance one'}}]}
            if r.endswith('/read-cursor'):
                self.read = 1
                return 200, {}
            if r.endswith('/attachments'):
                return 201, {'attachmentId': str(uuid.uuid4())}
            return 201, {}
        if rest.startswith('/calling/'):
            if org['members'][user['userId']] != 'OWNER':
                return 403, {'error': {'code': 'PERMISSION_DENIED'}}
            r = rest[len('/calling/'):]
            mine = org_id == next(iter(self.orgs))
            if r == 'v1/enterprise/departments' and method == 'POST':
                return 201, {'departmentId': self.dept}
            if r == 'v1/enterprise/departments':
                return 200, {'departments': [{'departmentId': self.dept}] if mine else []}
            if r.startswith('v1/enterprise/departments/'):
                return (200, {}) if mine else (404, {})
            if r.startswith('v1/enterprise/audit'):
                return 200, {'records': [{'resourceId': self.dept, 'actorPrincipalId': str(uuid.uuid4())}]}
        if rest == '/audit-events?limit=100':
            return 200, {'items': [{'action': x, 'organizationId': org_id} for x in (
                'organization.created', 'invitation.created', 'team.created', 'team.member_added')]}
        m2 = re.fullmatch(r'/members/([^/]+)', rest)
        if m2 and method == 'PATCH':
            org['members'].pop(b['userId'], None)
            return 200, {}
        if rest in ('/suspend', '/reactivate', '/archive'):
            if org['owner'] != user['userId']:
                return 403, {}
            org['status'] = {'/suspend': 'SUSPENDED', '/reactivate': 'ACTIVE', '/archive': 'ARCHIVED'}[rest]
            return 200, {'lifecycleStatus': org['status']}
        raise AssertionError('unexpected request %s %s' % (method, route))


class FakeSocket:
    def __init__(self, ticket):
        self.ticket = ticket

    def next_of(self, kind):
        if kind == 'event':
            return {'type': 'event', 'conversationId': FakeSocket.conversation}
        return {'type': kind}

    def send(self, value):
        FakeSocket.sent.append(value['requestId'])

    def next_frame(self):
        return {'type': 'error'}

    def close(self):
        pass


FakeSocket.sent = []


class OrganizationAcceptanceTest(unittest.TestCase):
    def journey(self, api):
        events = []
        FakeSocket.conversation = api.conv
        journey = oa.Journey(list(api.people.values()), lambda k, v: events.append((k, v)), transport=api,
                             sleep=lambda s: None, websocket=FakeSocket)
        return journey, events

    def test_check_names_are_unique_and_cover_the_directive(self):
        self.assertEqual(len(oa.CHECKS), len(set(oa.CHECKS)))
        self.assertEqual(len(oa.CHECKS), 18)

    def test_full_journey_passes_against_a_conforming_api_and_reports_no_secret(self):
        api = FakeApi(accounts())
        journey, events = self.journey(api)
        checks = journey.run()
        self.assertEqual(set(checks.values()), {'PASS'}, checks)
        dump = json.dumps(events)
        for word in SECRET_WORDS:
            self.assertNotIn(word, dump)
        for p in api.people.values():
            self.assertNotIn(p['userId'], dump)
        # Ids never appear in recorded routes.
        self.assertFalse([e for e in events if oa.UUID_RE.search(json.dumps(e))])
        # Both organizations were archived; no operator or internal route was used.
        self.assertTrue(all(not r.startswith('/internal') for _, r in api.requests))
        self.assertEqual(sum(1 for m, r in api.requests if r == '/api/v2/organizations/{id}/archive'), 2)

    def test_first_failure_stops_the_journey_names_the_check_and_still_cleans_up(self):
        api = FakeApi(accounts(), fail_route=('POST', '/api/v2/organizations/{id}/teams'))
        journey, events = self.journey(api)
        checks = journey.run()
        self.assertEqual(checks['teams_and_member_workspace'], 'FAIL')
        self.assertEqual(checks['invitation_accepted_only_by_the_invitee'], 'PASS')
        self.assertEqual(checks['organization_messaging_conversation_members_and_messages'], 'NOT_RUN')
        archives = [r for m, r in api.requests if r == '/api/v2/organizations/{id}/archive']
        self.assertEqual(len(archives), 2, 'cleanup archives both organizations')
        self.assertNotIn('Bearer', json.dumps(events))

    def test_isolation_violation_fails(self):
        api = FakeApi(accounts())
        original = api.__call__

        def leaky(method, url, headers, data):
            status, value = original(method, url, headers, data)
            if url.endswith('/messaging/members/resolve'):
                value = {'people': [{'userId': u, 'principalId': str(uuid.uuid4())}
                                    for u in json.loads(data)['userIds']]}
            return status, value
        journey, _ = self.journey(api)
        journey.transport = leaky
        checks = journey.run()
        self.assertEqual(checks['organization_messaging_conversation_members_and_messages'], 'FAIL')

    def test_template_strips_ids_and_query(self):
        u = str(uuid.uuid4())
        self.assertEqual(oa.template('/api/v2/organizations/%s/calling/v1/enterprise/audit?resourceId=%s' % (u, u)),
                         '/api/v2/organizations/{id}/calling/v1/enterprise/audit')


class DispatchTest(unittest.TestCase):
    def run_with(self, journal):
        with patch.object(cr, 'load_journal', lambda: journal), patch.object(cr.Path, 'mkdir', lambda *a, **k: None):
            cr.run('organization-acceptance', SHA, '7-1')

    def test_operation_is_listed_and_refused_without_its_activations(self):
        self.assertIn('organization-acceptance', cr.OPERATIONS)
        with self.assertRaisesRegex(RuntimeError, 'org-cutover-authoritative or org-cutover-first-activation'):
            self.run_with([])
        done = [{'operation': o, 'status': 'done'} for o in ('org-cutover-first-activation',)]
        with self.assertRaisesRegex(RuntimeError, 'activate-organization-bindings, activate-enterprise-calling'):
            self.run_with(done)

    def test_any_non_pass_fails_the_operation(self):
        done = [{'operation': o, 'status': 'done'} for o in (
            'org-cutover-first-activation', 'activate-organization-bindings', 'activate-enterprise-calling')]
        for result, ok in (({n: 'PASS' for n in oa.CHECKS}, True),
                           (dict({n: 'PASS' for n in oa.CHECKS}, archival_of_both_organizations='NOT_RUN'), False)):
            with patch.object(oa, 'run', lambda run_id, emit, r=result: r):
                if ok:
                    self.run_with(done)
                else:
                    with self.assertRaisesRegex(RuntimeError, 'Organization acceptance failed'):
                        self.run_with(done)


if __name__ == '__main__':
    unittest.main()
