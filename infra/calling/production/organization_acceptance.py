"""First-party organization production acceptance (authenticated, public API only).

Run on the VPS by `coordinated_release.py organization-acceptance` (production lock, main head)
once Organizations is authoritative, automatic service bindings are active and per-organization
Enterprise Calling is on. Every request goes to https://api.cfmeet.com exactly as a web or mobile
client's would (real deployed services, no mocks, no database access, no operator route). It
creates three fresh, email-verified acceptance accounts (app_acceptance; secrets stay in 0600
files on the VPS):

  A  owner of org1            B  invited member of org1          C  owner of org2 / outsider of org1

and proves, as named checks, the journey registration/login -> organization creation ->
automatic provisioning (no manual binding anywhere) -> owner membership -> invitation and
acceptance -> teams and workspaces -> organization Messaging (REST, unread counts, attachment
authorization) -> realtime -> organization Enterprise Calling and permissions -> audit ->
membership revocation -> suspension / reactivation -> archival, with cross-organization isolation
and personal-account preservation checked along the way.

The journey stops at the first failed check (later steps depend on earlier ones); the failing
step is recorded. Cleanup (archiving both organizations) always runs. Output: one JSON line per
check (`organizationAcceptance`) and per request (`organizationAcceptanceRequest`: method, route
template, status, error code). Never a token, email, password, name or message body.
"""
import base64
import hashlib
import json
import os
import re
import socket
import ssl
import time
import urllib.error
import urllib.request
import uuid

PUBLIC = 'https://api.cfmeet.com'
PROVISIONING_WAIT_SECONDS = 180
RECOVERY_WAIT_SECONDS = 180
UUID_RE = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', re.I)
# A member who lost access, an outsider, or an organization that is not ACTIVE: the relay refuses
# with one of these (Organizations decides on every request; never forwarded).
REFUSED = (403, 404, 409)

CHECKS = (
    'accounts_login_and_personal_baseline',
    'organizations_created_by_their_owners',
    'retry_create_is_idempotent',
    'automatic_provisioning_ready_without_operator',
    'owner_membership_workspace_and_capabilities',
    'invitation_accepted_only_by_the_invitee',
    'teams_and_member_workspace',
    'organization_messaging_conversation_members_and_messages',
    'organization_messaging_unread_and_read_cursor',
    'organization_realtime_session_and_events',
    'organization_attachment_authorization',
    'cross_organization_messaging_isolation',
    'personal_messaging_unaffected',
    'organization_calling_enterprise_permissions_and_isolation',
    'organization_audit_events',
    'membership_revocation_is_immediate',
    'suspension_fails_closed_and_reactivation_restores',
    'archival_of_both_organizations',
)


class Failed(AssertionError):
    pass


def template(path):
    """The route without ids or query: safe to record."""
    return UUID_RE.sub('{id}', path.split('?')[0])


class Transport:
    """urllib against the public ingress. Returns (status, json-or-{})."""

    def __call__(self, method, url, headers, data):
        request = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, raw = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read()
        try:
            return status, (json.loads(raw) if raw else {})
        except ValueError:
            return status, {}


class Person:
    def __init__(self, label, account):
        self.label = label
        self.account = account
        self.token = None

    @property
    def user_id(self):
        return self.account['userId']


class Journey:
    def __init__(self, accounts, emit, transport=None, sleep=time.sleep, websocket=None):
        assert len(accounts) == 3
        self.a, self.b, self.c = (Person(l, x) for l, x in zip('ABC', accounts))
        self.emit = emit
        self.transport = transport or Transport()
        self.sleep = sleep
        self.websocket = websocket or Realtime
        self.checks = {}
        self.step = None
        self.run_tag = uuid.uuid4().hex[:8]
        self.org1 = self.org2 = None
        self.archived = set()

    # --- plumbing ---------------------------------------------------------------------------

    def call(self, person, method, path, body=None, expected=(200,), key=None, retry_auth=True):
        headers = {'Accept': 'application/json'}
        if person is not None:
            if person.token is None:
                self.login(person)
            headers['Authorization'] = 'Bearer ' + person.token
        data = None
        if body is not None:
            headers['Content-Type'] = 'application/json'
            data = json.dumps(body).encode()
        elif method in ('POST', 'PUT', 'PATCH'):
            headers['Content-Type'] = 'application/json'
            data = b'{}'
        if method in ('POST', 'PUT', 'PATCH', 'DELETE'):
            headers['Idempotency-Key'] = key or str(uuid.uuid4())
        status, value = self.transport(method, PUBLIC + path, headers, data)
        code = (value.get('error') or {}).get('code') if isinstance(value, dict) and isinstance(
            value.get('error'), dict) else None
        self.emit('organizationAcceptanceRequest', {'step': self.step, 'method': method, 'route': template(path),
                                                    'status': status, 'code': code})
        if status == 401 and 401 not in expected and person is not None and retry_auth:
            # The 15-minute access token expired during the run: sign in again once.
            self.login(person)
            return self.call(person, method, path, body, expected, key, retry_auth=False)
        if status not in expected:
            raise Failed('%s %s answered %d %s' % (method, template(path), status, code or ''))
        return status, value

    def login(self, person):
        _, value = self.call(None, 'POST', '/api/v1/auth/login',
                             {'email': person.account['email'], 'password': person.account['password']})
        if not value.get('access_token'):
            raise Failed('login returned no access token')
        person.token = value['access_token']

    def org_path(self, org, rest=''):
        return '/api/v2/organizations/%s%s' % (org, rest)

    def msg(self, person, org, method, rest, body=None, expected=(200,)):
        return self.call(person, method, self.org_path(org, '/messaging/relay/' + rest), body, expected)

    def calling(self, person, org, method, rest, body=None, expected=(200,)):
        return self.call(person, method, self.org_path(org, '/calling/' + rest), body, expected)

    def check(self, name, ok=True):
        assert name in CHECKS and name not in self.checks, name
        self.checks[name] = 'PASS' if ok else 'FAIL'
        self.emit('organizationAcceptance', {'check': name, 'result': self.checks[name]})
        if not ok:
            raise Failed(name)

    def require(self, condition, what):
        if not condition:
            raise Failed(what)

    def until(self, probe, seconds, what):
        deadline = time.monotonic() + seconds
        while True:
            value = probe()
            if value:
                return value
            if time.monotonic() >= deadline:
                raise Failed(what)
            self.sleep(5)

    # --- journey ----------------------------------------------------------------------------

    def run(self):
        try:
            for step in (self.accounts, self.create, self.provisioning, self.owner, self.invitation, self.teams,
                         self.messaging, self.enterprise_calling, self.audit, self.revocation, self.suspension,
                         self.archival):
                self.step = step.__name__
                step()
        except Exception as error:
            # The first check of the journey that has not passed is the one that failed.
            failed = next(n for n in CHECKS if n not in self.checks)
            self.checks[failed] = 'FAIL'
            self.emit('organizationAcceptance', {'check': failed, 'result': 'FAIL',
                                                 'failure': type(error).__name__,
                                                 'detail': str(error)[:200] if isinstance(error, Failed) else ''})
        finally:
            self.step = 'cleanup'
            for person, org in ((self.a, self.org1), (self.c, self.org2)):
                if org and org not in self.archived:
                    try:
                        self.call(person, 'POST', self.org_path(org, '/archive'), {}, expected=(200, 409))
                        self.archived.add(org)
                    except Exception as error:  # recorded, never raised over the real failure
                        self.emit('organizationAcceptance', {'check': 'cleanup', 'result': 'FAIL',
                                                             'failure': type(error).__name__})
        for name in CHECKS:
            self.checks.setdefault(name, 'NOT_RUN')
        return self.checks

    # 1 ----------------------------------------------------------------------------------------
    def accounts(self):
        for p in (self.a, self.b, self.c):
            self.login(p)
            self.call(p, 'GET', '/api/v2/me/profile')
            _, me = self.call(p, 'GET', '/api/v2/messaging/me')
            self.require(UUID_RE.fullmatch(str(me.get('principalId', ''))), 'personal messaging identity')
            p.personal_principal = me['principalId']
            _, listing = self.call(p, 'GET', '/api/v2/messaging/relay/v1/conversations')
            self.require(isinstance(listing.get('conversations'), list), 'personal conversation list')
        self.require(len({p.personal_principal for p in (self.a, self.b, self.c)}) == 3, 'distinct principals')
        self.check('accounts_login_and_personal_baseline')

    # 2 + 14 -----------------------------------------------------------------------------------
    def create(self):
        body1 = {'name': 'CF Meet acceptance ' + self.run_tag + ' one', 'slug': 'cfmeet-accept-%s-1' % self.run_tag}
        key = str(uuid.uuid4())
        _, org1 = self.call(self.a, 'POST', '/api/v2/organizations', body1, expected=(201,), key=key)
        self.org1 = org1['id']
        self.require(org1.get('lifecycleStatus') == 'ACTIVE' and org1.get('creationSource') == 'SELF_SERVICE',
                     'org1 active, self-service')
        body2 = {'name': 'CF Meet acceptance ' + self.run_tag + ' two', 'slug': 'cfmeet-accept-%s-2' % self.run_tag}
        _, org2 = self.call(self.c, 'POST', '/api/v2/organizations', body2, expected=(201,))
        self.org2 = org2['id']
        self.require(self.org1 != self.org2, 'distinct organizations')
        self.check('organizations_created_by_their_owners')
        _, again = self.call(self.a, 'POST', '/api/v2/organizations', body1, expected=(200, 201), key=key)
        self.require(again.get('id') == self.org1, 'the retried create returns the same organization')
        _, mine = self.call(self.a, 'GET', '/api/v2/me/organizations')
        ids = [m['organization']['id'] for m in mine]
        self.require(ids.count(self.org1) == 1, 'no duplicate organization after the retry')
        self.check('retry_create_is_idempotent')

    # 3 ----------------------------------------------------------------------------------------
    def provisioning(self):
        for person, org in ((self.a, self.org1), (self.c, self.org2)):
            def ready(person=person, org=org):
                _, doc = self.call(person, 'GET', self.org_path(org, '/provisioning'))
                self.require(doc.get('provisioningEnabled') is True, 'provisioning enabled')
                self.require(doc.get('provisioningState') != 'FAILED', 'provisioning FAILED')
                return doc if doc.get('provisioningState') == 'READY' else None
            doc = self.until(ready, PROVISIONING_WAIT_SECONDS, 'provisioning did not reach READY')
            scopes = doc.get('scopes') or []
            self.require(len(scopes) == 1 and scopes[0].get('mappingState') == 'READY'
                         and scopes[0].get('tenantId'), 'one READY scope mapping')
        self.require(self.scope_tenant(self.a, self.org1) != self.scope_tenant(self.c, self.org2),
                     'each organization has its own tenant')
        # READY now includes both service bindings (automatic): the organization's own Messaging
        # scope answers without any operator step.
        for person, org in ((self.a, self.org1), (self.c, self.org2)):
            self.msg(person, org, 'GET', 'v1/conversations')
        self.check('automatic_provisioning_ready_without_operator')

    def scope_tenant(self, person, org):
        _, doc = self.call(person, 'GET', self.org_path(org, '/provisioning'))
        return doc['scopes'][0]['tenantId']

    # 4 ----------------------------------------------------------------------------------------
    def owner(self):
        _, members = self.call(self.a, 'GET', self.org_path(self.org1, '/members'))
        owner = [m for m in members if m['userId'] == self.a.user_id]
        self.require(len(members) == 1 and owner and owner[0]['status'] == 'ACTIVE'
                     and (owner[0].get('organizationRole') or owner[0]['role']) == 'OWNER', 'A is the only OWNER')
        _, page = self.call(self.a, 'GET', '/api/v2/me/workspaces')
        ws = [w for w in page['items'] if w.get('organizationId') == self.org1]
        self.require(ws and ws[0]['role'] == 'OWNER' and ws[0].get('provisioningState') == 'READY', 'workspace')
        self.require(not [w for w in page['items'] if w.get('organizationId') == self.org2], 'no foreign workspace')
        _, caps = self.call(self.a, 'GET', '/api/v2/me/workspaces/%s/capabilities' % self.org1)
        granted = {c['capability'] for c in caps['capabilities'] if c['granted']}
        self.require(caps['features'].get('messaging') is True and caps['features'].get('calling') is True,
                     'messaging and calling features')
        self.require('calling.enterprise.manage' in granted, 'calling.enterprise.manage for the owner')
        self.check('owner_membership_workspace_and_capabilities')

    # 5 ----------------------------------------------------------------------------------------
    def invitation(self):
        _, inv = self.call(self.a, 'POST', self.org_path(self.org1, '/invitations'),
                           {'email': self.b.account['email'], 'role': 'MEMBER'}, expected=(201,))
        token = inv.get('token')
        self.require(token and inv.get('status') == 'PENDING', 'one-time token for the inviter')
        # Another signed-in account cannot use the invitation.
        self.call(self.c, 'POST', '/api/v2/organization-invitations/accept', {'token': token}, expected=(403, 404))
        _, membership = self.call(self.b, 'POST', '/api/v2/organization-invitations/accept', {'token': token})
        self.require(membership.get('status') == 'ACTIVE' and membership.get('userId') == self.b.user_id
                     and membership.get('role') == 'MEMBER', 'B is an ACTIVE MEMBER')
        self.b.membership = membership
        _, members = self.call(self.a, 'GET', self.org_path(self.org1, '/members'))
        users = {m['userId']: m['status'] for m in members}
        self.require(users.get(self.b.user_id) == 'ACTIVE' and self.c.user_id not in users, 'membership list')
        self.call(self.c, 'GET', self.org_path(self.org1, '/members'), expected=(403, 404))
        self.check('invitation_accepted_only_by_the_invitee')

    # 6 ----------------------------------------------------------------------------------------
    def teams(self):
        _, team = self.call(self.a, 'POST', self.org_path(self.org1, '/teams'),
                            {'name': 'Acceptance ' + self.run_tag}, expected=(201,))
        self.call(self.a, 'POST', self.org_path(self.org1, '/teams/%s/members' % team['id']),
                  {'userId': self.b.user_id, 'role': 'TEAM_MEMBER'}, expected=(200, 201))
        _, page = self.call(self.a, 'GET', self.org_path(self.org1, '/teams/%s/members' % team['id']))
        self.require([m for m in page['items'] if m['userId'] == self.b.user_id and m['status'] == 'ACTIVE'],
                     'B is a team member')
        # C (outsider) sees neither the team nor the organization.
        self.call(self.c, 'GET', self.org_path(self.org1, '/teams'), expected=(403, 404))
        _, ws = self.call(self.b, 'GET', '/api/v2/me/workspaces')
        mine = [w for w in ws['items'] if w.get('organizationId') == self.org1]
        self.require(mine and mine[0]['role'] == 'MEMBER', 'org1 in B\'s workspaces')
        _, caps = self.call(self.b, 'GET', '/api/v2/me/workspaces/%s/capabilities' % self.org1)
        granted = {c['capability'] for c in caps['capabilities'] if c['granted']}
        self.require('calling.enterprise.manage' not in granted, 'a MEMBER cannot manage Enterprise Calling')
        self.check('teams_and_member_workspace')

    # 7 + 8 ------------------------------------------------------------------------------------
    def messaging(self):
        _, resolved = self.call(self.a, 'POST', self.org_path(self.org1, '/messaging/members/resolve'),
                                {'userIds': [self.b.user_id, self.c.user_id]})
        people = resolved.get('people') or []
        self.require(len(people) == 1 and people[0]['userId'] == self.b.user_id, 'only active members resolve')
        b_org = people[0]['principalId']
        self.require(b_org != self.b.personal_principal, 'organization principal differs from the personal one')
        _, conv = self.msg(self.a, self.org1, 'POST', 'v1/conversations',
                           {'contextClass': 'GROUP', 'securityProfile': 'MANAGED', 'title': 'Acceptance'},
                           expected=(201,))
        self.conversation = conv['conversationId']
        self.msg(self.a, self.org1, 'POST', 'v1/conversations/%s/members' % self.conversation,
                 {'principalId': b_org}, expected=(200, 201))
        self.msg(self.a, self.org1, 'POST', 'v1/conversations/%s/messages' % self.conversation,
                 {'clientMessageId': str(uuid.uuid4()), 'text': 'acceptance one'}, expected=(200, 201))
        _, page = self.msg(self.b, self.org1, 'GET', 'v1/conversations/%s/messages' % self.conversation)
        texts = [m.get('managed', {}).get('text') for m in page.get('messages', [])]
        self.require('acceptance one' in texts, 'B reads the organization message')
        self.check('organization_messaging_conversation_members_and_messages')

        row = self.inbox_row(self.b)
        self.require(row['inbox']['unreadCount'] >= 1, 'unread count for B')
        sequence = max(m['sequence'] for m in page['messages'])
        self.msg(self.b, self.org1, 'PUT', 'v1/conversations/%s/read-cursor' % self.conversation,
                 {'sequence': sequence})
        self.require(self.inbox_row(self.b)['inbox']['unreadCount'] == 0, 'read cursor clears unread')
        self.check('organization_messaging_unread_and_read_cursor')

        self.realtime()
        self.check('organization_realtime_session_and_events')

        content = b'CF Meet organization acceptance attachment\n'
        intent = {'kind': 'DOCUMENT', 'size': len(content), 'sha256': hashlib.sha256(content).hexdigest(),
                  'mediaType': 'text/plain', 'fileName': 'acceptance.txt'}
        _, upload = self.msg(self.b, self.org1, 'POST', 'v1/conversations/%s/attachments' % self.conversation,
                             intent, expected=(200, 201))
        self.require(UUID_RE.fullmatch(str(upload.get('attachmentId', ''))), 'upload intent for a member')
        self.msg(self.c, self.org1, 'POST', 'v1/conversations/%s/attachments' % self.conversation, intent,
                 expected=REFUSED)
        self.check('organization_attachment_authorization')

        # C: not through org1's relay, not through its own org2 scope.
        self.msg(self.c, self.org1, 'GET', 'v1/conversations/%s/messages' % self.conversation, expected=REFUSED)
        self.msg(self.c, self.org2, 'GET', 'v1/conversations/%s/messages' % self.conversation, expected=(403, 404))
        _, other = self.msg(self.c, self.org2, 'GET', 'v1/conversations')
        self.require(not [x for x in other['conversations'] if x['conversationId'] == self.conversation],
                     'org1 conversation absent from org2')
        self.check('cross_organization_messaging_isolation')

        for p in (self.a, self.b):
            _, personal = self.call(p, 'GET', '/api/v2/messaging/relay/v1/conversations')
            self.require(not [x for x in personal['conversations'] if x['conversationId'] == self.conversation],
                         'organization conversation never in a personal list')
            _, me = self.call(p, 'GET', '/api/v2/messaging/me')
            self.require(me['principalId'] == p.personal_principal, 'personal identity unchanged')
        self.call(self.b, 'POST', '/api/v2/messaging/session-tokens', {}, expected=(201,))
        self.check('personal_messaging_unaffected')

    def inbox_row(self, person):
        _, listing = self.msg(person, self.org1, 'GET', 'v1/conversations?orderBy=lastActivity&limit=50')
        rows = [x for x in listing['conversations'] if x['conversationId'] == self.conversation]
        self.require(rows and isinstance(rows[0].get('inbox'), dict), 'conversation in the member inbox')
        return rows[0]

    def ticket(self, token):
        status, value = self.transport('POST', PUBLIC + '/v1/realtime/tickets',
                                       {'Authorization': 'CF-Client-Token ' + token,
                                        'Content-Type': 'application/json'}, b'{}')
        self.emit('organizationAcceptanceRequest', {'step': self.step, 'method': 'POST',
                                                    'route': '/v1/realtime/tickets', 'status': status, 'code': None})
        self.require(status in (200, 201) and value.get('ticket'), 'realtime ticket')
        return value['ticket']

    def realtime(self):
        _, session = self.call(self.b, 'POST', self.org_path(self.org1, '/messaging/session-tokens'), {},
                               expected=(201,))
        self.require(session.get('token'), 'organization session token')
        socket_ = self.websocket(self.ticket(session['token']))
        try:
            self.require(socket_.next_of('welcome'), 'welcome')
            socket_.send({'type': 'subscribe', 'conversationId': self.conversation, 'requestId': 's1'})
            self.require(socket_.next_of('subscribed'), 'subscribed to the organization conversation')
            self.msg(self.a, self.org1, 'POST', 'v1/conversations/%s/messages' % self.conversation,
                     {'clientMessageId': str(uuid.uuid4()), 'text': 'acceptance realtime'}, expected=(200, 201))
            # Server frame shape (services/messaging/src/realtime/connection.rs): {"type": "event",
            # "event": {"conversationId": ..., ...}}. A replayed earlier event of the same
            # conversation is also proof of delivery; another conversation's event is not.
            frame = socket_.next_of('event')
            event = (frame or {}).get('event') or {}
            self.require(event.get('conversationId') == self.conversation, 'realtime event delivered')
        finally:
            socket_.close()
        # Scoped to the organization: B's PERSONAL realtime session cannot subscribe to it.
        _, personal = self.call(self.b, 'POST', '/api/v2/messaging/session-tokens', {}, expected=(201,))
        socket_ = self.websocket(self.ticket(personal['token']))
        try:
            self.require(socket_.next_of('welcome'), 'personal welcome')
            socket_.send({'type': 'subscribe', 'conversationId': self.conversation, 'requestId': 's2'})
            reply = socket_.next_frame()
            self.require(reply is None or reply.get('type') != 'subscribed', 'personal session refused')
        finally:
            socket_.close()

    # 9 ----------------------------------------------------------------------------------------
    def enterprise_calling(self):
        self.calling(self.a, self.org1, 'GET', 'v1/enterprise/departments')
        _, dept = self.calling(self.a, self.org1, 'POST', 'v1/enterprise/departments',
                               {'name': 'Acceptance ' + self.run_tag}, expected=(201,))
        dept_id = dept['departmentId']
        _, listing = self.calling(self.a, self.org1, 'GET', 'v1/enterprise/departments')
        self.require([d for d in listing['departments'] if d['departmentId'] == dept_id], 'department in org1')
        _, foreign = self.calling(self.c, self.org2, 'GET', 'v1/enterprise/departments')
        self.require(not [d for d in foreign['departments'] if d['departmentId'] == dept_id], 'absent in org2')
        self.calling(self.c, self.org2, 'GET', 'v1/enterprise/departments/' + dept_id, expected=(404,))
        self.calling(self.b, self.org1, 'POST', 'v1/enterprise/departments', {'name': 'Denied'}, expected=(403,))
        self.calling(self.b, self.org1, 'GET', 'v1/enterprise/departments', expected=(403,))
        self.calling(self.c, self.org1, 'GET', 'v1/enterprise/departments', expected=(403, 404))
        _, audit = self.calling(self.a, self.org1, 'GET', 'v1/enterprise/audit?resourceId=' + dept_id)
        self.require([r for r in audit['records'] if r['resourceId'] == dept_id and r.get('actorPrincipalId')],
                      'enterprise audit names the acting member principal')
        self.check('organization_calling_enterprise_permissions_and_isolation')

    # 10 ---------------------------------------------------------------------------------------
    def audit(self):
        _, page = self.call(self.a, 'GET', self.org_path(self.org1, '/audit-events?limit=100'))
        actions = {e['action'] for e in page['items']}
        missing = {'organization.created', 'invitation.created', 'team.created', 'team.member_added'} - actions
        self.require(not missing, 'audit actions present')
        self.require(all(e.get('organizationId') in (None, self.org1) for e in page['items']), 'own audit only')
        self.call(self.c, 'GET', self.org_path(self.org1, '/audit-events'), expected=(403, 404))
        self.check('organization_audit_events')

    # 11 ---------------------------------------------------------------------------------------
    def revocation(self):
        m = self.b.membership
        self.call(self.a, 'PATCH', self.org_path(self.org1, '/members/' + m['id']),
                  {'expectedVersion': m['version'], 'status': 'REMOVED'})
        self.msg(self.b, self.org1, 'GET', 'v1/conversations/%s/messages' % self.conversation, expected=REFUSED)
        self.call(self.b, 'POST', self.org_path(self.org1, '/messaging/session-tokens'), {}, expected=REFUSED)
        _, ws = self.call(self.b, 'GET', '/api/v2/me/workspaces')
        self.require(not [w for w in ws['items'] if w.get('organizationId') == self.org1], 'workspace gone')
        _, me = self.call(self.b, 'GET', '/api/v2/messaging/me')
        self.require(me['principalId'] == self.b.personal_principal, 'B keeps personal Messaging')
        self.call(self.b, 'GET', '/api/v2/messaging/relay/v1/conversations')
        self.check('membership_revocation_is_immediate')

    # 12 ---------------------------------------------------------------------------------------
    def suspension(self):
        _, org = self.call(self.a, 'POST', self.org_path(self.org1, '/suspend'), {})
        self.require(org.get('lifecycleStatus') == 'SUSPENDED', 'suspended')
        self.msg(self.a, self.org1, 'GET', 'v1/conversations', expected=REFUSED)
        self.calling(self.a, self.org1, 'GET', 'v1/enterprise/departments', expected=REFUSED)
        _, org = self.call(self.a, 'POST', self.org_path(self.org1, '/reactivate'), {})
        self.require(org.get('lifecycleStatus') == 'ACTIVE', 'reactivated')

        # The tenant resume reaches Platform Access asynchronously (retried sync step).
        def usable():
            status, _ = self.msg(self.a, self.org1, 'GET', 'v1/conversations', expected=(200,) + REFUSED + (503,))
            if status != 200:
                return False
            status, _ = self.calling(self.a, self.org1, 'GET', 'v1/enterprise/departments',
                                     expected=(200,) + REFUSED + (503,))
            return status == 200
        self.until(usable, RECOVERY_WAIT_SECONDS, 'relays did not recover after reactivation')
        self.check('suspension_fails_closed_and_reactivation_restores')

    # 13 ---------------------------------------------------------------------------------------
    def archival(self):
        for person, org in ((self.a, self.org1), (self.c, self.org2)):
            _, doc = self.call(person, 'POST', self.org_path(org, '/archive'), {})
            self.require(doc.get('lifecycleStatus') == 'ARCHIVED', 'archived')
            self.archived.add(org)
            self.msg(person, org, 'GET', 'v1/conversations', expected=REFUSED)
        self.check('archival_of_both_organizations')


class Realtime:
    """A minimal RFC 6455 client for wss://api.cfmeet.com/v1/realtime (text frames only)."""

    def __init__(self, ticket, host='api.cfmeet.com', timeout=15):
        raw = socket.create_connection((host, 443), timeout=timeout)
        self.sock = ssl.create_default_context().wrap_socket(raw, server_hostname=host)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(('GET /v1/realtime HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                           'Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n'
                           'Sec-WebSocket-Protocol: cfmeet.messaging.v1, cfmeet.ticket.%s\r\n\r\n'
                           % (host, key, ticket)).encode())
        head = b''
        while b'\r\n\r\n' not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise Failed('realtime handshake closed')
            head += chunk
        head, self.buffer = head.split(b'\r\n\r\n', 1)
        if not head.startswith(b'HTTP/1.1 101'):
            raise Failed('realtime handshake refused')

    def _read(self, n):
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self.buffer += chunk
        out, self.buffer = self.buffer[:n], self.buffer[n:]
        return out

    def next_frame(self):
        """The next JSON text frame; None when the server closed or nothing arrived in time."""
        try:
            while True:
                b0, b1 = self._read(2)
                length = b1 & 0x7f
                if length == 126:
                    length = int.from_bytes(self._read(2), 'big')
                elif length == 127:
                    length = int.from_bytes(self._read(8), 'big')
                payload = self._read(length)
                opcode = b0 & 0x0f
                if opcode == 8:
                    return None
                if opcode == 9:
                    self._send(0xA, payload)
                    continue
                if opcode == 1:
                    return json.loads(payload)
        except (EOFError, socket.timeout, OSError, ValueError):
            return None

    def next_of(self, kind, limit=20):
        for _ in range(limit):
            frame = self.next_frame()
            if frame is None:
                return None
            if frame.get('type') == kind:
                return frame
        return None

    def _send(self, opcode, payload):
        mask = os.urandom(4)
        n = len(payload)
        header = bytes([0x80 | opcode]) + (bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + n.to_bytes(2, 'big'))
        self.sock.sendall(header + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(payload)))

    def send(self, value):
        self._send(1, json.dumps(value).encode())

    def close(self):
        try:
            self._send(8, b'')
            self.sock.close()
        except OSError:
            pass


def run(run_id, emit):
    """Creates the three acceptance accounts (app_acceptance) and runs the journey."""
    import app_acceptance
    accounts = app_acceptance.accounts('organizations-' + run_id.replace('_', '-').lower()[:50], count=3)
    return Journey(accounts, emit).run()
