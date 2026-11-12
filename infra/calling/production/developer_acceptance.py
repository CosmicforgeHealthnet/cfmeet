"""Developer Platform and API Gateway production acceptance (authenticated, public API only).

Run on the VPS by `coordinated_release.py developer-acceptance` (production lock, main head).
Every request goes to https://api.cfmeet.com exactly as a customer's would; nothing reads or
writes a database. It creates two fresh, isolated acceptance accounts (app_acceptance), and
for each a developer organization, project and SANDBOX environment, then proves:

  console      DEVELOPER_SESSION through the gateway and the Meeting session bridge
  scopes       self-service STANDARD scope grants; a credential limited to them
  gateway      platform credential → Calling (/v1/limits, /v1/usage), Messaging
               (/v1/conversations) and central webhooks (/v1/webhooks) through the gateway,
               receiver grants included; scope enforcement (403 INSUFFICIENT_SCOPE); quota headers
  isolation    developer B gets 404 for A's project, environment and webhooks; B's credential
               cannot see A's webhook
  webhooks     a console subscription, a real event (credential.expiring_soon from the worker's
               lifecycle sweep), a signed delivery to the HTTPS receiver that SUCCEEDS with the
               attempt recorded
  revocation   a revoked credential is refused at the gateway (401)

Cleanup always runs: webhook deleted, credentials revoked, projects archived, organizations
closed. Output: one JSON object per check (`developerAcceptance`), no secrets, no tokens.
"""
import datetime
import json
import time
import urllib.error
import urllib.request
import uuid

import app_acceptance
import coordinated_release as cr

PUBLIC = cr.PUBLIC
SCOPES = ('usage.read', 'conversations.manage', 'webhooks.read')
EVENT_TYPES = ['credential.expiring_soon', 'credential.expired']
DELIVERY_WAIT_SECONDS = 900


class Failed(AssertionError):
    pass


class Expired(Exception):
    pass


class Client:
    def __init__(self, token=None, account=None):
        self.token = token
        self.account = account  # a session client re-signs in when its 15-minute token expires
        self.last_headers = {}

    def call(self, method, path, body=None, expected=(200,), token=None, idempotent=False):
        try:
            return self._call(method, path, body, expected, token, idempotent)
        except Expired:
            self.token = login(self.account)
            return self._call(method, path, body, expected, token, idempotent)

    def _call(self, method, path, body, expected, token, idempotent):
        headers = {'Accept': 'application/json'}
        bearer = token or self.token
        if bearer:
            headers['Authorization'] = 'Bearer ' + bearer
        data = None
        if body is not None:
            headers['Content-Type'] = 'application/json'
            data = json.dumps(body).encode()
        elif method in ('POST', 'PUT', 'PATCH'):
            data = b''
        if idempotent:
            headers['Idempotency-Key'] = str(uuid.uuid4())
        request = urllib.request.Request(PUBLIC + path, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, raw, self.last_headers = response.status, response.read(), dict(response.headers)
        except urllib.error.HTTPError as error:
            status, raw, self.last_headers = error.code, error.read(), dict(error.headers)
        try:
            value = json.loads(raw) if raw else {}
        except ValueError:
            value = {}
        if status == 401 and 401 not in expected and self.account and not token:
            raise Expired()
        if status not in expected:
            code = (value.get('error') or {}).get('code') if isinstance(value, dict) else None
            route = path.split('?')[0]
            raise Failed('%s %s answered %d %s' % (method, route, status, code or ''))
        return status, value


def login(account):
    _, value = Client().call('POST', '/api/v1/auth/login', {'email': account['email'], 'password': account['password']})
    return value['access_token']


class Journey:
    def __init__(self, run_id, receiver):
        self.run_id = run_id
        self.receiver = receiver
        self.checks = {}
        self.cleanup = []

    def check(self, name, ok, **detail):
        self.checks[name] = 'PASS' if ok else 'FAIL'
        cr.emit('developerAcceptance', dict(check=name, result=self.checks[name], **detail))
        if not ok:
            raise Failed(name)

    def tenant(self, label, account, before_credential=None):
        console = Client(login(account), account)
        _, org = console.call('POST', '/v1/organizations', {'displayName': 'Acceptance %s %s' % (label, self.run_id)},
                              expected=(201,), idempotent=True)
        self.cleanup.append(('POST', '/v1/organizations/%s/close' % org['id'], console))
        self.check('console %s: organization created through the gateway (DEVELOPER_SESSION)' % label,
                   org.get('myRole') == 'OWNER' and org.get('state') == 'ACTIVE')
        _, project = console.call('POST', '/v1/organizations/%s/projects' % org['id'],
                                  {'displayName': 'acceptance-%s' % label}, expected=(201,), idempotent=True)
        self.cleanup.insert(0, ('POST', '/v1/projects/%s/archive' % project['id'], console))
        _, env = console.call('POST', '/v1/projects/%s/environments' % project['id'],
                              {'kind': 'SANDBOX', 'displayName': 'acceptance'}, expected=(201,), idempotent=True)
        self.check('console %s: project and SANDBOX environment' % label,
                   env.get('kind') == 'SANDBOX' and env.get('projectId') == project['id'])
        status, _ = console.call('POST', '/v1/projects/%s/environments' % project['id'], {'kind': 'PRODUCTION'},
                                 expected=(422,))
        self.check('console %s: PRODUCTION environment needs approval (422)' % label, status == 422)
        for scope in SCOPES:
            console.call('POST', '/v1/projects/%s/scopes' % project['id'], {'scope': scope})
        _, service_account = console.call('POST', '/v1/environments/%s/service-accounts' % env['id'],
                                          {'name': 'acceptance-%s' % label}, expected=(201,), idempotent=True)
        tenant = dict(console=console, org=org, project=project, env=env)
        if before_credential:
            tenant['hook'] = before_credential(tenant)
        expires = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ')
        _, issued = console.call('POST', '/v1/environments/%s/service-accounts/%s/credentials' % (env['id'], service_account['id']),
                                 {'scopes': list(SCOPES), 'label': 'acceptance', 'expiresAt': expires}, expected=(201,))
        credential = issued['credential']
        self.cleanup.insert(0, ('POST', '/v1/environments/%s/credentials/%s/revoke' % (env['id'], credential['id']), console))
        self.check('console %s: credential issued once with only the granted scopes' % label,
                   issued['secret'].startswith('cfmk_sbx_') and sorted(credential['scopes']) == sorted(SCOPES)
                   and console.last_headers.get('Cache-Control', '').find('no-store') >= 0)
        tenant.update(credential=credential, machine=Client(issued['secret']))
        return tenant

    def gateway(self, label, t):
        machine = t['machine']
        for path, family in (('/v1/limits', 'Calling'), ('/v1/usage', 'Calling'), ('/v1/conversations', 'Messaging'),
                             ('/v1/webhooks', 'Developer Platform (central webhooks)')):
            status, _ = machine.call('GET', path)
            quota = {k.lower(): v for k, v in machine.last_headers.items() if k.lower().startswith('x-cf-quota')}
            self.check('gateway %s: %s %s with a platform credential' % (label, family, path), status == 200,
                       requestId=machine.last_headers.get('x-request-id'), quota=quota or None)
        status, value = machine.call('POST', '/v1/webhooks', {'url': self.receiver, 'eventTypes': EVENT_TYPES},
                                     expected=(403,), idempotent=True)
        self.check('gateway %s: scope enforced (webhooks.manage missing → 403)' % label,
                   (value.get('error') or {}).get('code') == 'INSUFFICIENT_SCOPE')
        status, _ = Client('cfmk_sbx_invalid_invalid').call('GET', '/v1/limits', expected=(401,))
        self.check('gateway: forged platform credential refused (401)', status == 401)

    def webhook(self, t):
        console, env = t['console'], t['env']
        _, created = console.call('POST', '/v1/environments/%s/webhooks' % env['id'],
                                  {'url': self.receiver, 'eventTypes': EVENT_TYPES, 'description': 'acceptance'},
                                  expected=(201,), idempotent=True)
        hook = created['webhook']
        self.cleanup.insert(0, ('DELETE', '/v1/environments/%s/webhooks/%s?expectedVersion=%d'
                                % (env['id'], hook['webhookId'], hook['version']), console))
        self.check('webhooks: subscription created with a signing secret',
                   hook['status'] == 'ACTIVE' and bool(created.get('signingSecret')), webhookId=hook['webhookId'])
        return hook

    def webhook_visible(self, t):
        _, listed = t['machine'].call('GET', '/v1/webhooks')
        self.check('webhooks: visible to the environment credential through the gateway',
                   any(w['webhookId'] == t['hook']['webhookId'] for w in listed.get('data', [])))

    def delivery(self, t, hook):
        console, env = t['console'], t['env']
        deadline = time.time() + DELIVERY_WAIT_SECONDS
        seen = None
        while time.time() < deadline:
            _, page = console.call('GET', '/v1/environments/%s/webhooks/%s/deliveries?limit=20' % (env['id'], hook['webhookId']))
            items = page.get('items', [])
            done = [d for d in items if d['status'] in ('SUCCEEDED', 'FAILED')]
            if items:
                seen = items[0]
            if done:
                d = done[0]
                _, attempts = console.call('GET', '/v1/environments/%s/webhook-deliveries/%s/attempts'
                                           % (env['id'], d['deliveryId']))
                first = (attempts.get('items') or [{}])[0]
                self.check('webhooks: event delivered, signed, attempt recorded (SUCCEEDED)', d['status'] == 'SUCCEEDED',
                           eventType=d['eventType'], status=d['status'], lastHttpStatus=d.get('lastHttpStatus'),
                           attemptCount=d.get('attemptCount'), attemptHttpStatus=first.get('httpStatus'),
                           failureCategory=d.get('failureCategory'))
                return
            time.sleep(20)
        self.check('webhooks: event delivered within %d s' % DELIVERY_WAIT_SECONDS, False,
                   lastSeen={k: seen.get(k) for k in ('eventType', 'status', 'failureCategory')} if seen else None)

    def isolation(self, a, b):
        console = b['console']
        for path in ('/v1/projects/%s' % a['project']['id'], '/v1/environments/%s' % a['env']['id'],
                     '/v1/environments/%s/webhooks' % a['env']['id']):
            status, value = console.call('GET', path, expected=(404, 403))
            self.check('isolation: developer B reading %s → 404' % path.split('/')[2],
                       status == 404 and (value.get('error') or {}).get('code') == 'NOT_FOUND')
        _, listed = b['machine'].call('GET', '/v1/webhooks')
        self.check('isolation: credential of tenant B lists none of tenant A\'s webhooks', listed.get('data') == [])

    def revocation(self, t):
        console, env = t['console'], t['env']
        console.call('POST', '/v1/environments/%s/credentials/%s/revoke' % (env['id'], t['credential']['id']))
        for _ in range(12):
            try:
                t['machine'].call('GET', '/v1/limits', expected=(401,))
                self.check('revocation: revoked credential refused at the gateway (401)', True)
                return
            except Failed:
                time.sleep(5)
        self.check('revocation: revoked credential refused at the gateway (401)', False)

    def close(self):
        problems = []
        for method, path, client in self.cleanup:
            try:
                client.call(method, path, expected=(200, 404, 409))
            except (Failed, OSError) as error:
                problems.append(str(error))
        cr.emit('developerAcceptanceCleanup', {'steps': len(self.cleanup), 'problems': problems})


def run(run_id, receiver):
    accounts = app_acceptance.accounts('developer-' + run_id.replace('-', 'x'), count=2)
    journey = Journey(run_id, receiver)
    try:
        a = journey.tenant('A', accounts[0], before_credential=journey.webhook)
        b = journey.tenant('B', accounts[1])
        journey.gateway('A', a)
        journey.gateway('B', b)
        journey.webhook_visible(a)
        journey.isolation(a, b)
        journey.delivery(a, a['hook'])
        journey.revocation(b)
    finally:
        journey.close()
    cr.emit('developerAcceptanceVerdict', {'passed': sum(v == 'PASS' for v in journey.checks.values()),
                                           'checks': len(journey.checks)})
    return journey.checks
