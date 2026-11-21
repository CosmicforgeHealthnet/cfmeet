"""CF Meet Platform "Build" production acceptance (ADR 0062 / 0063), two fresh tenants.

Run on the VPS by `coordinated_release.py platform-acceptance` and as the post-check of every
platform activation (platform_operations.py). Every request goes to https://api.cfmeet.com as a
partner's server (platform credential), a console user (developer session) or a partner's
device (user token, then the device credential it is exchanged for) would send it; nothing
reads or writes a database. It extends developer_acceptance: two fresh controlled accounts, each
with its own developer workspace, project and SANDBOX environment (A and B), then per stage:

  users     create, idempotent upsert, read, update, list by externalId; phone-number id
            refused; console list; B gets 404 on A's user (read, update, mint)
  tokens    mint (cfut_sbx_) and upsert-with-token; ungranted scope refused; console SANDBOX
            token generator; allowed websites (wildcard refused); gateway CORS: preflight for
            A's website on a device route only, never on a credential-only route, never for an
            unregistered origin; B gets 404 on A's environment; revoke-sessions ends tokens
  chat      user-token exchange for a partner session device (tampered token refused); the
            partner's server opens a MANAGED conversation; the rider's device sends, the
            customer's device receives; realtime ticket; MLS refused; B's device and B's server
            get 404/403 on A's conversation
  calls     exchange for an endpoint + cfsig_; the partner's server places rider -> customer;
            the customer's device sees the invitation; B gets 404/403 on A's call / invitation
  push      push credentials list; invalid upload refused and nothing stored; device push
            registration and removal; B gets 404 on A's credentials. Real ringing needs the
            partner's own APNs / FCM keys and a device: not claimed here
  meetings  the partner's server creates a space and a conference (machine family through
            the gateway relay: the edge does not route /api/v2 machine traffic); a user-token
            join with the grant's role and the profile's name; re-join keeps the participant;
            no grant -> 403; B's token on A's conference -> refused; meeting chat readable
  apps      CF Meet Apps (ADR 0064 / 0065; integrate_operations.py), per feature on (`apps`):
            register   A and B each register a DRAFT app on their project (manifest, client
                       secret shown once, A's own account as the test account); B gets 404 on
                       A's app
            oauth      the DRAFT test-account flow exactly as the consent page and the app's
                       server send it: consent context (PKCE S256 required; B's account, not a
                       test account, refused), approve -> code, token exchange (client_secret_basic,
                       form), userinfo (pairwise sub), Connected apps lists the grant, an OAuth
                       token refused on a non-OAuth route (403), refresh rotation, revoke,
                       reuse of a superseded refresh token revokes the grant, B's client cannot
                       redeem A's code
            delegated  the access token creates (and archives) a meeting space through /v1/meetings
            installs   a workspace install into an organization the account does not belong to
                       is never allowed
            directory  anonymous list (A's DRAFT app never listed; unknown app 404)
            in-meeting the public app-context JWKS (one Ed25519 key, no private part)
            Features that are off get flag-off checks. Install event delivery and E2EE
            invisibility need fixtures acceptance accounts do not have: noted, not claimed.

Stages that are not on get flag-off checks instead (the feature answers like an unknown route).
Cleanup always runs: users deleted, origins cleared, calls cancelled, conferences ended, spaces
archived, app grants revoked, credentials revoked, projects archived, workspaces closed. Output:
one JSON object per check (`platformAcceptance`), ids and statuses only: never a token, code,
key or secret.
"""
import base64
import datetime
import hashlib
import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import app_acceptance
import developer_acceptance as da

PUBLIC = 'https://api.cfmeet.com'
# The gateway's loopback relay (coordinated_release.GATEWAY_RELAY_PORT, asserted by the tests):
# what the edge would forward to if the machine meeting family were routed there.
RELAY = 'http://127.0.0.1:18110'
RELAY_HEADERS = {'Host': 'api.cfmeet.com', 'X-Forwarded-Proto': 'https'}
BUILD_STAGES = ('users', 'tokens', 'chat', 'calls', 'push', 'meetings')
STAGES = BUILD_STAGES + ('apps',)
SCOPES = {'users': ('principals.read', 'principals.write'), 'tokens': ('user_tokens.issue',),
          'chat': ('conversations.create', 'conversations.manage', 'messages.read', 'messages.send'),
          'calls': ('calls.read', 'calls.write'), 'push': (), 'meetings': ('meetings.read', 'meetings.write'),
          'apps': ()}
# CF Meet Apps features (integrate_operations.FEATURES, same order); `register` carries the rest.
APP_FEATURES = ('register', 'oauth', 'delegated', 'installs', 'events', 'directory', 'in-meeting')
APP_USER_SCOPES = ('profile:read', 'profile:email', 'meetings:read', 'meetings:write')
FORM = 'application/x-www-form-urlencoded'
WEB_ORIGIN = 'https://cfmeet.com'
UNREGISTERED_ORIGIN = 'https://unregistered.acceptance.example.com'
PHONE_LIKE_ID = '+14155550123'
REFUSED = (401, 403, 404, 405)
# A PEM-shaped placeholder (no real key): passes the console's shape check, so the SANDBOX
# "production APNs host" rule is what refuses it; nothing is sealed or stored before that rule.
PEM_PLACEHOLDER = '-----BEGIN PRIVATE KEY-----\n' + 'A' * 64 + '\n-----END PRIVATE KEY-----'


def off_status(answer):
    """A switched-off feature's status: 503 counts as off only with FEATURE_DISABLED."""
    status, value = answer
    return status if status != 503 or error_code(value) == 'FEATURE_DISABLED' else 599
CORS_WAIT = (12, 5)        # attempts, seconds: the gateway polls the origins feed every <= 30 s
REVOCATION_WAIT = (12, 5)  # verify results are cached for <= 30 s


class Client(da.Client):
    """developer_acceptance.Client with a base URL, fixed headers and per-call headers."""

    def __init__(self, token=None, account=None, base=PUBLIC, headers=None):
        super().__init__(token, account)
        self.base = base
        self.headers = dict(headers or {})

    def call(self, method, path, body=None, expected=(200,), token=None, idempotent=False, headers=None):
        try:
            return self._call(method, path, body, expected, token, idempotent, headers)
        except da.Expired:
            self.token = login(self.account)
            return self._call(method, path, body, expected, token, idempotent, headers)

    def _call(self, method, path, body, expected, token, idempotent, headers=None):
        sent = {'Accept': 'application/json'}
        sent.update(self.headers)
        sent.update(headers or {})
        bearer = token or self.token
        if bearer:
            sent['Authorization'] = 'Bearer ' + bearer
        data = None
        if isinstance(body, str):  # a pre-encoded body (OAuth form); its Content-Type is in `headers`
            data = body.encode()
        elif body is not None:
            sent['Content-Type'] = 'application/json'
            data = json.dumps(body).encode()
        elif method in ('POST', 'PUT', 'PATCH'):
            data = b''
        if idempotent:
            sent['Idempotency-Key'] = str(uuid.uuid4())
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=sent)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, raw, self.last_headers = response.status, response.read(), da.lower(response.headers)
        except urllib.error.HTTPError as error:
            status, raw, self.last_headers = error.code, error.read(), da.lower(error.headers)
        try:
            value = json.loads(raw) if raw else {}
        except ValueError:
            value = {}
        if status == 401 and 401 not in expected and self.account and not token:
            raise da.Expired()
        if status not in expected:
            code = error_code(value)
            raise da.Failed('%s %s answered %d %s' % (method, path.split('?')[0], status, code or ''))
        return status, value


def login(account):
    """Passwordless sign-in of a controlled account (code replaced in Redis, VPS only)."""
    client = Client()
    return app_acceptance.sign_in_via(lambda path, body: client.call('POST', path, body)[1], account)['access_token']


def data(value):
    """The Meeting Service wraps some answers in {success, data}."""
    if isinstance(value, dict) and 'success' in value and isinstance(value.get('data'), dict):
        return value['data']
    return value if isinstance(value, dict) else {}


def error_code(value):
    """The stable error code: {"error": {"code": ...}} (CF Meet APIs) or {"error": "..."} (OAuth)."""
    err = value.get('error') if isinstance(value, dict) else None
    return err.get('code') if isinstance(err, dict) else err


def code(value):
    return error_code(value)


def device(client_token):
    """A partner session device: client tokens use the CF-Client-Token scheme, never Bearer."""
    return Client(headers={'Authorization': 'CF-Client-Token ' + client_token})


def retry(fn, attempts, delay):
    """fn() until it is truthy (at most `attempts` times); the last answer."""
    value = None
    for attempt in range(attempts):
        value = fn()
        if value:
            return value
        if attempt + 1 < attempts:
            time.sleep(delay)
    return value


def tampered(token):
    """The same token with its last signature character changed (still well-formed)."""
    return token[:-1] + ('A' if token[-1] != 'A' else 'B')


def pkce():
    """(verifier, S256 challenge) of RFC 7636."""
    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
    return verifier, challenge


def basic(client_id, secret):
    """client_secret_basic (RFC 6749 §2.3.1: both parts form-encoded before Base64)."""
    raw = '%s:%s' % (urllib.parse.quote(client_id, safe=''), urllib.parse.quote(secret, safe=''))
    return {'Authorization': 'Basic ' + base64.b64encode(raw.encode()).decode(), 'Content-Type': FORM}


def query(url):
    """The query parameters of a redirect URL ({name: first value})."""
    return {k: v[0] for k, v in urllib.parse.parse_qs(urllib.parse.urlsplit(url or '').query).items()}


class Journey:
    def __init__(self, run_id, stages, emit, apps=None):
        unknown = set(stages) - set(STAGES)
        assert not unknown, 'Unknown acceptance stages: %s' % sorted(unknown)
        self.run_id = run_id
        self.stages = tuple(s for s in STAGES if s in stages)
        if 'apps' in self.stages:
            apps = APP_FEATURES if apps is None else apps
            unknown = set(apps) - set(APP_FEATURES)
            assert not unknown and 'register' in apps, 'Invalid app features: %s' % sorted(apps)
            self.apps = tuple(f for f in APP_FEATURES if f in apps)
        else:
            self.apps = ()
        self.emit = emit
        self.tag = run_id.replace('-', 'x').lower()
        self.checks = {}
        self.cleanup = []

    def on(self, stage):
        return stage in self.stages

    def app(self, feature):
        return feature in self.apps

    def check(self, name, ok, fatal=False, **detail):
        self.checks[name] = 'PASS' if ok else 'FAIL'
        self.emit('platformAcceptance', dict(check=name, result=self.checks[name], **detail))
        if not ok and fatal:
            raise da.Failed(name)

    def step(self, name, fn, *args):
        """One independent group of checks: an unexpected answer, a missing result of an
        earlier failed group (KeyError) or a network error fails only this group."""
        try:
            return fn(*args)
        except da.Failed as error:
            self.check(name + ': ' + str(error), False)
        except KeyError as error:
            self.check('%s: skipped, an earlier step did not produce %s' % (name, error), False)
        except (OSError, ValueError) as error:
            self.check('%s: %s' % (name, type(error).__name__), False)

    def undo(self, client, method, path):
        self.cleanup.insert(0, (method, path, client))

    # --- setup -----------------------------------------------------------------------------

    def scopes(self):
        return tuple(s for stage in self.stages for s in SCOPES[stage])

    def grants(self):
        out = {}
        if self.on('chat'):
            out['msg'] = ['read', 'send']
        if self.on('calls'):
            out['call'] = ['receive']
        return out

    def tenant(self, label, account):
        console = Client(login(account), account)
        _, org = console.call('POST', '/v1/organizations', {'displayName': 'Platform acceptance %s %s' % (label, self.run_id)},
                              expected=(201,), idempotent=True)
        self.cleanup.append(('POST', '/v1/organizations/%s/close' % org['id'], console))
        _, project = console.call('POST', '/v1/organizations/%s/projects' % org['id'],
                                  {'displayName': 'platform-acceptance-%s' % label.lower()}, expected=(201,), idempotent=True)
        self.undo(console, 'POST', '/v1/projects/%s/archive' % project['id'])
        _, env = console.call('POST', '/v1/projects/%s/environments' % project['id'],
                              {'kind': 'SANDBOX', 'displayName': 'platform-acceptance'}, expected=(201,), idempotent=True)
        self.check('setup %s: workspace, project and SANDBOX environment' % label,
                   env.get('kind') == 'SANDBOX' and env.get('projectId') == project['id'], fatal=True)
        for scope in self.scopes():
            console.call('POST', '/v1/projects/%s/scopes' % project['id'], {'scope': scope})
        _, account_ = console.call('POST', '/v1/environments/%s/service-accounts' % env['id'],
                                   {'name': 'platform-acceptance-%s' % label.lower()}, expected=(201,), idempotent=True)
        expires = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%SZ')
        _, issued = console.call('POST', '/v1/environments/%s/service-accounts/%s/credentials' % (env['id'], account_['id']),
                                 {'scopes': list(self.scopes()), 'label': 'platform-acceptance', 'expiresAt': expires},
                                 expected=(201,))
        credential = issued['credential']
        self.undo(console, 'POST', '/v1/environments/%s/credentials/%s/revoke' % (env['id'], credential['id']))
        self.check('setup %s: platform credential with the stage scopes' % label,
                   str(issued.get('secret', '')).startswith('cfmk_')
                   and sorted(credential.get('scopes') or []) == sorted(self.scopes()), fatal=True)
        machine = Client(issued['secret'])
        return dict(label=label, console=console, org=org, project=project, env=env, machine=machine,
                    relay=Client(issued['secret'], base=RELAY, headers=RELAY_HEADERS), accountId=account.get('userId'),
                    origin='https://%s-%s.acceptance.example.com' % (label.lower(), self.tag))

    # --- users (B1) --------------------------------------------------------------------------

    def create_user(self, t, external_id, display_name=None):
        body = {'externalId': external_id}
        if display_name:
            body['displayName'] = display_name
        status, value = t['machine'].call('POST', '/v1/users', body, expected=(200, 201))
        # Contract: the Users API answers the EndUser itself (Platform Access's internal
        # router wraps it in {"user": ...}; the Developer Platform unwraps it).
        user = value if isinstance(value, dict) else {}
        if status == 201 and user.get('id'):
            self.undo(t['machine'], 'DELETE', '/v1/users/%s' % user['id'])
        return status, user

    def users(self, t):
        label, machine = t['label'], t['machine']
        status, rider = self.create_user(t, 'rider-%s-%s' % (t['label'].lower(), self.tag), 'Acceptance rider')
        self.check('users %s: user created (201, ACTIVE)' % label, status == 201 and rider.get('status') == 'ACTIVE'
                   and rider.get('externalId') == 'rider-%s-%s' % (t['label'].lower(), self.tag), fatal=True,
                   userId=rider.get('id'))
        again, same = self.create_user(t, rider['externalId'], 'Acceptance rider')
        self.check('users %s: create is idempotent on externalId (200, same id)' % label,
                   again == 200 and same.get('id') == rider['id'])
        _, customer = self.create_user(t, 'customer-%s-%s' % (t['label'].lower(), self.tag), 'Acceptance customer')
        self.check('users %s: second user created' % label, bool(customer.get('id')), fatal=True)
        _, got = machine.call('GET', '/v1/users/%s' % rider['id'])
        self.check('users %s: read by id' % label, got.get('id') == rider['id'])
        _, listed = machine.call('GET', '/v1/users?externalId=%s' % rider['externalId'])
        self.check('users %s: list by externalId' % label, [u.get('id') for u in listed.get('items', [])] == [rider['id']])
        _, patched = machine.call('PATCH', '/v1/users/%s' % rider['id'], {'displayName': 'Acceptance rider %s' % label})
        self.check('users %s: profile updated' % label,
                   patched.get('displayName') == 'Acceptance rider %s' % label)
        status, value = machine.call('POST', '/v1/users', {'externalId': PHONE_LIKE_ID}, expected=(201, 400, 422))
        self.check('users %s: phone-number externalId refused (400 VALIDATION_ERROR)' % label,
                   status == 400 and code(value) == 'VALIDATION_ERROR')
        if status == 201:
            self.undo(machine, 'DELETE', '/v1/users/%s' % value.get('id'))
        _, console = t['console'].call('GET', '/v1/environments/%s/users' % t['env']['id'])
        self.check('users %s: console lists the environment\'s users' % label,
                   rider['id'] in [u.get('id') for u in console.get('items', [])])
        t.update(rider=rider, customer=customer)

    def users_isolation(self, a, b):
        rider = a['rider']
        status, value = b['machine'].call('GET', '/v1/users/%s' % rider['id'], expected=(403, 404))
        self.check('isolation users: B reads A\'s user -> 404', status == 404 and code(value) == 'NOT_FOUND')
        status, _ = b['machine'].call('PATCH', '/v1/users/%s' % rider['id'], {'displayName': 'x'}, expected=(403, 404))
        self.check('isolation users: B updates A\'s user -> 404', status == 404)
        _, listed = b['machine'].call('GET', '/v1/users?externalId=%s' % rider['externalId'])
        self.check('isolation users: B lists none of A\'s users', listed.get('items') == [])
        status, _ = b['console'].call('GET', '/v1/environments/%s/users' % a['env']['id'], expected=(403, 404))
        self.check('isolation users: B\'s console reads A\'s user list -> 404', status == 404)

    # --- tokens and allowed websites (B2) ----------------------------------------------------

    def mint(self, t, user, grants, expected=(201,)):
        status, value = t['machine'].call('POST', '/v1/users/%s/tokens' % user['id'], {'grants': grants, 'ttlSeconds': 900},
                                          expected=expected)
        return status, value

    def tokens(self, t):
        label = t['label']
        _, minted = self.mint(t, t['rider'], self.grants())
        token = str(minted.get('token', ''))
        self.check('tokens %s: user token minted (cfut_sbx_, <= 2048 bytes)' % label,
                   token.startswith('cfut_sbx_') and len(token) <= 2048 and bool(minted.get('expiresAt')), fatal=True,
                   tokenId=minted.get('tokenId'))
        _, upserted = t['machine'].call('POST', '/v1/users/tokens', {'externalId': t['customer']['externalId'],
                                                                     'grants': self.grants()}, expected=(201,))
        self.check('tokens %s: upsert-and-mint returns the existing user' % label,
                   (upserted.get('user') or {}).get('id') == t['customer']['id']
                   and str(upserted.get('token', '')).startswith('cfut_sbx_'), fatal=True)
        t.update(riderToken=token, customerToken=upserted['token'])
        if not self.on('meetings'):
            status, value = self.mint(t, t['rider'], {'mtg': [{'c': str(uuid.uuid4()), 'r': 'PARTICIPANT'}]},
                                      expected=(201, 403))
            self.check('tokens %s: grant without its scope refused (403 INSUFFICIENT_SCOPE)' % label,
                       status == 403 and code(value) == 'INSUFFICIENT_SCOPE')
        status, generated = t['console'].call('POST', '/v1/environments/%s/test-user-tokens' % t['env']['id'],
                                              {'externalId': 'console-%s-%s' % (label.lower(), self.tag), 'grants': {}},
                                              expected=(201,))
        console_user = (generated.get('user') or {}).get('id')
        if console_user:
            self.undo(t['machine'], 'DELETE', '/v1/users/%s' % console_user)
        self.check('tokens %s: console SANDBOX token generator' % label,
                   str(generated.get('token', '')).startswith('cfut_sbx_'))
        path = '/v1/environments/%s/allowed-origins' % t['env']['id']
        t['console'].call('PUT', path, {'origins': [t['origin']]})
        self.undo(t['console'], 'PUT', path)
        _, origins = t['console'].call('GET', path)
        self.check('tokens %s: allowed website stored' % label, origins.get('origins') == [t['origin']])
        status, value = t['console'].call('PUT', path, {'origins': ['https://*.acceptance.example.com']},
                                          expected=(200, 400, 422))
        self.check('tokens %s: wildcard origin refused (400 VALIDATION_ERROR)' % label,
                   status == 400 and code(value) == 'VALIDATION_ERROR')

    def preflight(self, path, origin, method='GET'):
        status, headers = 0, {}
        client = Client()
        try:
            client.call('OPTIONS', path, expected=(200, 204, 400, 401, 403, 404, 405),
                        headers={'Origin': origin, 'Access-Control-Request-Method': method,
                                 'Access-Control-Request-Headers': 'authorization,content-type'})
            headers = client.last_headers
        except da.Failed:
            pass
        return headers.get('access-control-allow-origin')

    def cors(self, a):
        origin = a['origin']
        allowed = retry(lambda: self.preflight('/v1/conversations', origin) == origin, *CORS_WAIT)
        self.check('cors: partner website allowed on a device route (preflight)', bool(allowed))
        self.check('cors: partner website never allowed on a credential-only route (/v1/users)',
                   self.preflight('/v1/users', origin, 'POST') != origin)
        self.check('cors: an unregistered website is refused',
                   self.preflight('/v1/conversations', UNREGISTERED_ORIGIN) != UNREGISTERED_ORIGIN)
        self.check('cors: the CF Meet web origin is unchanged',
                   self.preflight('/v1/conversations', WEB_ORIGIN) == WEB_ORIGIN)

    def tokens_isolation(self, a, b):
        status, _ = self.mint(b, a['rider'], {}, expected=(201, 403, 404))
        self.check('isolation tokens: B mints for A\'s user -> 404', status == 404)
        status, _ = b['console'].call('GET', '/v1/environments/%s/allowed-origins' % a['env']['id'], expected=(200, 403, 404))
        self.check('isolation tokens: B\'s console reads A\'s allowed websites -> 404', status == 404)
        status, _ = b['console'].call('POST', '/v1/environments/%s/test-user-tokens' % a['env']['id'],
                                      {'externalId': 'intruder-%s' % self.tag, 'grants': {}}, expected=(201, 403, 404))
        self.check('isolation tokens: B\'s console generates a token in A\'s environment -> 404', status == 404)

    # --- chat (B3) ---------------------------------------------------------------------------

    def exchange_chat(self, t, who):
        status, value = Client().call('POST', '/v1/messaging/user-sessions',
                                      {'installationId': 'acc-%s-%s' % (who, self.tag)},
                                      token=t[who + 'Token'], expected=(200, 201))
        return value

    def chat(self, t):
        label = t['label']
        rider, customer = self.exchange_chat(t, 'rider'), self.exchange_chat(t, 'customer')
        self.check('chat %s: user tokens exchanged for partner session devices' % label,
                   rider.get('principalId') == t['rider']['id'] and customer.get('principalId') == t['customer']['id']
                   and bool(rider.get('clientToken')) and bool(customer.get('clientToken')), fatal=True)
        status, _ = Client().call('POST', '/v1/messaging/user-sessions', {'installationId': 'acc-forged-%s' % self.tag},
                                  token=tampered(t['riderToken']), expected=(200, 201, 401))
        self.check('chat %s: tampered user token refused (401)' % label, status == 401)
        _, conversation = t['machine'].call('POST', '/v1/conversations', {
            'contextClass': 'DIRECT', 'securityProfile': 'MANAGED', 'counterpartPrincipalId': t['customer']['id'],
            'managedDisclosureAcknowledged': True}, expected=(200, 201), idempotent=True,
            headers={'CF-Acting-Principal': t['rider']['id']})
        self.check('chat %s: partner server opens a MANAGED conversation' % label,
                   conversation.get('securityProfile') == 'MANAGED' and bool(conversation.get('conversationId')), fatal=True,
                   conversationId=conversation.get('conversationId'))
        conv = conversation['conversationId']
        message_id = str(uuid.uuid4())
        device(rider['clientToken']).call('POST', '/v1/conversations/%s/messages' % conv,
                                          {'clientMessageId': message_id, 'text': 'acceptance %s' % self.tag},
                                          expected=(200, 201), idempotent=True)
        reader = device(customer['clientToken'])

        def received():
            _, page = reader.call('GET', '/v1/conversations/%s/messages' % conv)
            return any(m.get('clientMessageId') == message_id for m in page.get('messages', []))
        self.check('chat %s: rider\'s device sends, customer\'s device receives' % label, bool(retry(received, 6, 2)))
        status, ticket = reader.call('POST', '/v1/realtime/tickets', {}, expected=(200, 201))
        self.check('chat %s: realtime ticket for the partner session device' % label, bool(ticket.get('ticket')))
        status, value = reader.call('GET', '/v1/conversations/%s/mls/state' % conv, expected=(200, 401, 403, 404))
        self.check('chat %s: MLS refused to a partner session device' % label,
                   status in (403, 404) or (status == 401 and code(value) == 'CLIENT_TOKENS_NOT_ENABLED'))
        t.update(conversation=conv, riderClient=rider['clientToken'], customerClient=customer['clientToken'])

    def chat_isolation(self, a, b):
        path = '/v1/conversations/%s/messages' % a['conversation']
        status, _ = device(b['customerClient']).call('GET', path, expected=(200, 403, 404))
        self.check('isolation chat: B\'s device reads A\'s conversation -> 404/403', status in (403, 404))
        status, _ = device(b['riderClient']).call('POST', path, {'clientMessageId': str(uuid.uuid4()), 'text': 'x'},
                                                  expected=(200, 201, 403, 404), idempotent=True)
        self.check('isolation chat: B\'s device sends into A\'s conversation -> 404/403', status in (403, 404))
        status, _ = b['machine'].call('GET', '/v1/conversations/%s' % a['conversation'], expected=(200, 403, 404))
        self.check('isolation chat: B\'s server reads A\'s conversation -> 404', status == 404)

    # --- calls (B4) --------------------------------------------------------------------------

    def exchange_call(self, t, who):
        _, value = Client().call('POST', '/v1/calling/user-sessions',
                                 {'installationId': 'acc-%s-%s' % (who, self.tag), 'kind': 'MOBILE'},
                                 token=t[who + 'Token'], expected=(200, 201))
        return value

    def calls(self, t):
        label = t['label']
        rider, customer = self.exchange_call(t, 'rider'), self.exchange_call(t, 'customer')
        self.check('calls %s: user tokens exchanged for endpoints and cfsig_ credentials' % label,
                   rider.get('principalId') == t['rider']['id'] and bool(rider.get('endpointId'))
                   and str(rider.get('signalingCredential', '')).startswith('cfsig_')
                   and str(customer.get('signalingCredential', '')).startswith('cfsig_'), fatal=True)
        status, _ = Client().call('POST', '/v1/calling/user-sessions', {'installationId': 'acc-forged-%s' % self.tag,
                                                                         'kind': 'MOBILE'},
                                  token=tampered(t['riderToken']), expected=(200, 201, 401))
        self.check('calls %s: tampered user token refused (401)' % label, status == 401)
        _, placed = t['machine'].call('POST', '/v1/calls', {
            'kind': 'VOICE', 'initiatorPrincipalId': t['rider']['id'], 'initiatorEndpointId': rider['endpointId'],
            'targetPrincipalIds': [t['customer']['id']]}, expected=(201,), idempotent=True)
        call = placed.get('call') or {}  # calling.common.v1.CallSummary: the id is `callId`
        if call.get('callId'):
            self.undo(t['machine'], 'POST', '/v1/calls/%s/cancel' % call['callId'])
        self.check('calls %s: partner server places rider -> customer' % label,
                   bool(call.get('callId')) and (placed.get('liveInvitationCount') or 0) >= 1, fatal=True,
                   callId=call.get('callId'))
        device = Client(customer['signalingCredential'])

        def invitation():
            _, page = device.call('GET', '/v1/calling/invitations')
            return next((i for i in page.get('data', []) if i.get('callId') == call['callId']), None)
        offer = retry(invitation, 6, 2)
        self.check('calls %s: customer\'s device sees the invitation' % label, bool(offer),
                   invitationId=(offer or {}).get('invitationId'))
        t.update(call=call['callId'], invitation=(offer or {}).get('invitationId'),
                 invitationVersion=(offer or {}).get('callVersion'),
                 customerSignaling=customer['signalingCredential'])

    def calls_isolation(self, a, b):
        status, _ = b['machine'].call('GET', '/v1/calls/%s' % a['call'], expected=(200, 403, 404))
        self.check('isolation calls: B\'s server reads A\'s call -> 404', status == 404)
        if a.get('invitation'):
            status, _ = Client(b['customerSignaling']).call(
                'POST', '/v1/calling/invitations/%s/accept' % a['invitation'],
                {'expectedVersion': a['invitationVersion']}, expected=(200, 403, 404, 409))
            self.check('isolation calls: B\'s device accepts A\'s invitation -> 404/403', status in (403, 404))

    # --- push (B5) ---------------------------------------------------------------------------

    def push(self, t):
        label = t['label']
        path = '/v1/environments/%s/push-credentials' % t['env']['id']
        _, before = t['console'].call('GET', path)
        self.check('push %s: push credentials listed (feature on)' % label, isinstance(before, dict))
        status, value = t['console'].call('POST', path, {
            'provider': 'APNS', 'teamId': 'ACCEPT0001', 'keyId': 'ACCEPT0001', 'bundleId': 'com.example.acceptance',
            'voip': True, 'apnsHost': 'PRODUCTION', 'p8': PEM_PLACEHOLDER}, expected=(201, 400, 422))
        _, after = t['console'].call('GET', path)
        self.check('push %s: production APNs host refused in SANDBOX (422 POLICY_VIOLATION), nothing stored' % label,
                   status == 422 and code(value) == 'POLICY_VIOLATION' and after == before)
        if self.on('chat') and t.get('riderToken'):  # the msg grant is minted only with chat on
            installation = 'acc-rider-%s' % self.tag
            partner = Client(t['riderToken'])  # contract: user-push-registrations take the user token
            _, registered = partner.call('PUT', '/v1/messaging/user-push-registrations/%s' % installation,
                                        {'platform': 'FCM', 'handle': 'acceptance-%s' % uuid.uuid4().hex}, expected=(200, 201))
            status, _ = partner.call('DELETE', '/v1/messaging/user-push-registrations/%s?platform=FCM' % installation,
                                    expected=(204,))
            self.check('push %s: device push registration stored and removed' % label,
                       registered.get('installationId') == installation and status == 204)
        self.emit('platformAcceptanceNote', {'push': 'real ringing needs the partner\'s own APNs/FCM keys and a device; '
                                                     'not claimed by this acceptance'})

    def push_isolation(self, a, b):
        status, _ = b['console'].call('GET', '/v1/environments/%s/push-credentials' % a['env']['id'], expected=(200, 403, 404))
        self.check('isolation push: B\'s console reads A\'s push credentials -> 404', status == 404)

    # --- meetings (B6) -----------------------------------------------------------------------

    def meetings(self, t):
        label, relay = t['label'], t['relay']
        _, space = relay.call('POST', '/api/v2/meeting-spaces', {'title': 'Platform acceptance %s' % label},
                              expected=(201,), idempotent=True)
        space = data(space)
        self.undo(relay, 'POST', '/api/v2/meeting-spaces/%s/archive' % space.get('id'))
        _, conference = relay.call('POST', '/api/v2/meeting-spaces/%s/conferences' % space.get('id'), {},
                                   expected=(201,), idempotent=True)
        conference = data(conference)
        # A conference that never goes live is cancelled at cleanup (/end answers CONFERENCE_NOT_LIVE
        # for it); a live one is ended first, after which the cancel is a tolerated 409 no-op.
        self.undo(relay, 'POST', '/api/v2/conferences/%s/cancel' % conference.get('id'))
        self.check('meetings %s: partner server creates a space and a conference (gateway)' % label,
                   bool(space.get('id')) and bool(conference.get('id')), fatal=True, conferenceId=conference.get('id'))
        conf = conference['id']
        # Joins are admitted only while the conference is LIVE (409 CONFERENCE_NOT_LIVE otherwise), so the
        # partner's server starts it (machine family, meetings.write) before any user token joins.
        _, started = relay.call('POST', '/api/v2/conferences/%s/start' % conf, {}, expected=(200,))
        self.undo(relay, 'POST', '/api/v2/conferences/%s/end' % conf)
        self.check('meetings %s: partner server starts the conference (gateway, LIVE)' % label,
                   (data(started).get('conference') or {}).get('status') == 'LIVE', fatal=True)
        grants = dict(self.grants(), mtg=[{'c': conf, 'r': 'PARTICIPANT'}])
        _, minted = self.mint(t, t['rider'], grants)
        token = minted.get('token')

        def join(bearer, expected=(200, 201)):
            return Client().call('POST', '/api/v2/conferences/%s/join' % conf, {}, token=bearer, expected=expected)
        _, joined = join(token)
        joined = data(joined)
        participant = joined.get('participant') or {}
        self.check('meetings %s: user-token join with the grant\'s role and the profile\'s name' % label,
                   participant.get('role') == 'PARTICIPANT'
                   and participant.get('displayName') == 'Acceptance rider %s' % label
                   and bool(joined.get('participantToken')), participantId=participant.get('id'))
        _, again = join(token)
        self.check('meetings %s: re-join keeps the participant' % label,
                   (data(again).get('participant') or {}).get('id') == participant.get('id'))
        status, _ = join(t['customerToken'], expected=(200, 201, 403))
        self.check('meetings %s: token without a grant for the conference -> 403' % label, status == 403)
        if joined.get('participantToken'):
            status, value = Client().call('GET', '/api/v2/conferences/%s/messages' % conf, expected=(200, 403, 409, 503),
                                          headers={'X-Participant-Token': joined['participantToken']})
            self.check('meetings %s: partner meeting chat available' % label,
                       status == 200 and code(value) != 'CHAT_UNAVAILABLE')
        t.update(conference=conf)

    def meetings_isolation(self, a, b):
        status, value = self.mint(b, b['rider'], dict(mtg=[{'c': a['conference'], 'r': 'HOST'}]),
                                  expected=(201, 403, 404, 422))
        if status == 201:
            status, _ = Client().call('POST', '/api/v2/conferences/%s/join' % a['conference'], {},
                                      token=value.get('token'), expected=(200, 201, 403, 404))
            self.check('isolation meetings: B\'s token on A\'s conference -> 404', status == 404)
        else:
            self.check('isolation meetings: B cannot mint a grant for A\'s conference', status in (403, 404, 422))
        status, _ = b['relay'].call('GET', '/api/v2/conferences/%s' % a['conference'], expected=(200, 403, 404))
        self.check('isolation meetings: B\'s server reads A\'s conference -> 404', status == 404)

    # --- CF Meet Apps (ADR 0064 / 0065) --------------------------------------------------------

    def app_path(self, t, suffix=''):
        return '/v1/projects/%s/app%s' % (t['project']['id'], suffix)

    def apps_register(self, t):
        label, console = t['label'], t['console']
        redirect = t['origin'] + '/oauth/callback'
        status, app = console.call('PUT', self.app_path(t), {
            'name': 'Platform acceptance app %s %s' % (label, self.run_id), 'redirectUris': [redirect],
            'userScopes': list(APP_USER_SCOPES), 'distribution': 'PRIVATE'}, expected=(200, 201))
        self.check('apps %s: DRAFT app registered on the project' % label,
                   app.get('status') == 'DRAFT' and app.get('clientId') == t['project']['id'], fatal=True,
                   clientId=app.get('clientId'))
        _, issued = console.call('POST', self.app_path(t, '/secret'), expected=(201,), idempotent=True)
        self.check('apps %s: client secret issued once (cfcs_, no-store)' % label,
                   str(issued.get('clientSecret', '')).startswith('cfcs_') and issued.get('clientId') == app['clientId']
                   and 'no-store' in console.last_headers.get('cache-control', ''), fatal=True,
                   secretId=issued.get('secretId'))
        if not t.get('accountId'):
            raise da.Failed('apps %s: the acceptance account has no recorded CF Meet user id' % label)
        _, accounts = console.call('PUT', self.app_path(t, '/test-accounts'), {'accountIds': [t['accountId']]})
        self.check('apps %s: the developer\'s own account is the DRAFT test account' % label,
                   t['accountId'] in [str(a) for a in accounts.get('testAccountIds') or []])
        _, got = console.call('GET', self.app_path(t))
        self.check('apps %s: manifest read back (exact redirect URI, requested scopes)' % label,
                   (got.get('manifest') or {}).get('redirectUris') == [redirect]
                   and sorted((got.get('manifest') or {}).get('userScopes') or []) == sorted(APP_USER_SCOPES))
        t.update(clientId=app['clientId'], clientSecret=issued['clientSecret'], redirect=redirect)

    def apps_isolation(self, a, b):
        status, _ = b['console'].call('GET', self.app_path(a), expected=(200, 403, 404))
        self.check('isolation apps: B\'s console reads A\'s app -> 404', status == 404)
        status, _ = b['console'].call('POST', self.app_path(a, '/secret'), expected=(201, 403, 404), idempotent=True)
        self.check('isolation apps: B\'s console rotates A\'s client secret -> 404', status == 404)

    def authorize(self, t, session, challenge, state, expected=(200,), method='S256'):
        params = urllib.parse.urlencode({'client_id': t['clientId'], 'redirect_uri': t['redirect'],
                                         'scope': ' '.join(APP_USER_SCOPES), 'state': state, 'code_challenge': challenge,
                                         'code_challenge_method': method})
        status, value = session.call('GET', '/api/v2/oauth/authorize/context?' + params, expected=expected)
        return status, data(value)

    def approve(self, t, session, challenge, state):
        """APPROVE as the signed-in account; every grant it creates is revoked at cleanup."""
        try:
            _, value = session.call('POST', '/api/v2/oauth/authorize/approve', {
                'clientId': t['clientId'], 'redirectUri': t['redirect'], 'scope': ' '.join(APP_USER_SCOPES),
                'state': state, 'codeChallenge': challenge, 'codeChallengeMethod': 'S256', 'decision': 'APPROVE'})
        finally:
            self.track_grants(t, session)
        redirect = str(data(value).get('redirectTo', ''))
        got = query(redirect)
        return redirect.startswith(t['redirect'] + '?') and got.get('state') == state, got.get('code')

    def track_grants(self, t, session):
        """Cleanup revokes every grant of `t`'s app the account holds (Connected apps)."""
        try:
            _, value = session.call('GET', '/api/v2/me/apps')
        except (da.Failed, OSError):
            return
        known = {path for _, path, _ in self.cleanup}
        for item in data(value).get('items') or []:
            path = '/api/v2/me/apps/%s' % item.get('grantId')
            if item.get('appId') == t['clientId'] and item.get('grantId') and path not in known:
                self.undo(session, 'DELETE', path)

    def token(self, client, form, expected=(200,)):
        http = Client()
        answer = http.call('POST', '/v1/oauth/token', urllib.parse.urlencode(form), expected=expected,
                           headers=basic(client['clientId'], client['clientSecret']))
        self.oauth_headers = http.last_headers
        return answer

    def userinfo(self, access, expected=(200, 401)):
        return Client().call('GET', '/v1/oauth/userinfo', token=access, expected=expected)

    def apps_oauth(self, a, b):
        session = a['console']
        verifier, challenge = pkce()
        state = uuid.uuid4().hex
        _, context = self.authorize(a, session, challenge, state)
        self.check('apps oauth: consent context for the DRAFT test account',
                   (context.get('app') or {}).get('clientId') == a['clientId']
                   and (context.get('app') or {}).get('status') == 'DRAFT' and context.get('target') == 'USER'
                   and context.get('redirectUri') == a['redirect'])
        status, _ = self.authorize(a, session, challenge, state, expected=(200, 400), method='plain')
        self.check('apps oauth: PKCE S256 required (plain refused, 400)', status == 400)
        status, _ = self.authorize(a, b['console'], challenge, state, expected=(200, 400, 403))
        self.check('apps oauth: an account that is not a DRAFT test account is refused', status in (400, 403))
        ok, code_ = self.approve(a, session, challenge, state)
        self.check('apps oauth: approve redirects to the exact redirect URI with a code and the state',
                   ok and bool(code_), fatal=True)
        _, issued = self.token(a, {'grant_type': 'authorization_code', 'code': code_, 'redirect_uri': a['redirect'],
                                   'code_verifier': verifier})
        access, refresh = str(issued.get('access_token', '')), str(issued.get('refresh_token', ''))
        grant = (issued.get('installation') or {}).get('id')
        self.check('apps oauth: code exchanged (cfoa_ access, cfor_ refresh, USER installation, no-store)',
                   access.startswith('cfoa_') and refresh.startswith('cfor_') and issued.get('token_type') == 'Bearer'
                   and (issued.get('installation') or {}).get('target') == 'USER' and 'bot_token' not in issued
                   and 'no-store' in self.oauth_headers.get('cache-control', ''), fatal=True, grantId=grant)
        _, info = self.userinfo(access, expected=(200,))
        self.check('apps oauth: userinfo answers a pairwise sub (never the account id), name and email',
                   bool(info.get('sub')) and info.get('sub') != a['accountId'] and a['accountId'] not in str(info)
                   and 'name' in info and 'email' in info)
        _, apps = session.call('GET', '/api/v2/me/apps')
        self.check('apps oauth: Connected apps lists the grant',
                   grant in [x.get('grantId') for x in data(apps).get('items') or []])
        status, _ = Client().call('GET', '/v1/users', token=access, expected=(200, 401, 403, 404))
        self.check('apps oauth: an OAuth token on a route without the OAuth scheme -> 403', status == 403)
        self.apps_delegated(a, access)
        _, rotated = self.token(a, {'grant_type': 'refresh_token', 'refresh_token': refresh})
        access2, refresh2 = str(rotated.get('access_token', '')), str(rotated.get('refresh_token', ''))
        self.check('apps oauth: refresh rotates both tokens',
                   access2.startswith('cfoa_') and refresh2.startswith('cfor_') and access2 != access
                   and refresh2 != refresh, fatal=True)
        status, _ = self.userinfo(access2)
        self.check('apps oauth: the rotated access token works', status == 200)
        status, _ = Client().call('POST', '/v1/oauth/revoke', urllib.parse.urlencode({'token': access2}),
                                  expected=(200, 400, 401), headers=basic(a['clientId'], a['clientSecret']))
        refused = retry(lambda: self.userinfo(access2)[0] == 401, *REVOCATION_WAIT)
        self.check('apps oauth: revoke answers 200 and the token is refused within 60 s', status == 200 and bool(refused))
        status, value = self.token(a, {'grant_type': 'refresh_token', 'refresh_token': refresh}, expected=(200, 400, 401))
        self.check('apps oauth: a superseded refresh token is refused (invalid_grant)',
                   status == 400 and value.get('error') == 'invalid_grant')
        status, value = self.token(a, {'grant_type': 'refresh_token', 'refresh_token': refresh2}, expected=(200, 400, 401))
        self.check('apps oauth: its reuse revoked the whole grant (the current refresh token is refused)',
                   status == 400 and value.get('error') == 'invalid_grant')
        verifier2, challenge2 = pkce()
        state2 = uuid.uuid4().hex
        ok, code2 = self.approve(a, session, challenge2, state2)
        if ok and code2:
            status, value = self.token(b, {'grant_type': 'authorization_code', 'code': code2, 'redirect_uri': a['redirect'],
                                           'code_verifier': verifier2}, expected=(200, 400, 401))
            self.check('isolation apps: B\'s client cannot redeem A\'s code',
                       status in (400, 401) and value.get('error') in ('invalid_grant', 'invalid_client'))
        else:
            self.check('isolation apps: a second code for the cross-client check', False)
        self.emit('platformAcceptanceNote', {'apps': 'E2EE invisibility needs a first-party PRIVATE_E2EE messaging '
                                                     'fixture the acceptance accounts do not have: skipped, not claimed'})

    def apps_delegated(self, a, access):
        """With `delegated` on, the access token creates (then archives) a space as the user
        through the partner meetings family; off, the token is refused there."""
        status, value = Client().call('POST', '/v1/meetings/meeting-spaces', {'title': 'App acceptance %s' % self.run_id},
                                      token=access, expected=(200, 201, 401, 403, 404), idempotent=True)
        space = data(value).get('id') if status in (200, 201) else None
        archived = None
        if space:
            path = '/v1/meetings/meeting-spaces/%s/archive' % space
            archived, _ = Client().call('POST', path, token=access, expected=(200, 204, 401, 403, 404))
            if archived not in (200, 204):
                self.undo(Client(access), 'POST', path)  # best effort; the grant is revoked at cleanup
        if not self.app('delegated'):
            self.check('flag off delegated: an OAuth token is refused on /v1/meetings (%d)' % status,
                       status in REFUSED and not space)
            return
        self.check('apps delegated: the access token creates a meeting space as the user (/v1/meetings)',
                   status == 201 and bool(space), spaceId=space)
        if space:
            self.check('apps delegated: the user\'s own space archived with the token', archived in (200, 204))

    def apps_installs(self, a):
        session = a['console']
        _, challenge = pkce()
        params = urllib.parse.urlencode({'client_id': a['clientId'], 'redirect_uri': a['redirect'],
                                         'scope': 'profile:read', 'state': 'install', 'code_challenge': challenge,
                                         'code_challenge_method': 'S256', 'install': 'organization',
                                         'organization_id': str(uuid.uuid4())})
        status, value = session.call('GET', '/api/v2/oauth/authorize/context?' + params,
                                     expected=(200, 400, 403, 404))
        decision = data(value).get('organizationInstall') or {}
        self.check('apps installs: a workspace install into an organization the account does not belong to is '
                   'never allowed', status in (400, 403, 404) or (status == 200 and decision.get('allowed') is False))
        self.emit('platformAcceptanceNote', {'apps installs': 'an allowed workspace install needs a business '
                                                              'organization fixture: not claimed'})

    def apps_directory(self, a):
        status, listed = Client().call('GET', '/v1/app-directory', expected=(200,) + REFUSED)
        if not self.app('directory'):
            self.check('flag off directory: refused (%d)' % status, status in REFUSED)
            return
        items = listed.get('items')
        self.check('apps directory: anonymous list (200)', status == 200 and isinstance(items, list))
        self.check('apps directory: a DRAFT app is never listed',
                   a.get('clientId') not in [x.get('appId') or x.get('clientId') for x in items or []])
        status, _ = Client().call('GET', '/v1/app-directory/%s' % a['clientId'], expected=(200, 404))
        self.check('apps directory: a DRAFT app is not found (404)', status == 404)

    def apps_in_meeting(self, a):
        status, jwks = Client().call('GET', '/api/v2/app-context-keys', expected=(200,) + REFUSED)
        if not self.app('in-meeting'):
            self.check('flag off in-meeting: refused (%d)' % status, status in REFUSED)
            return
        keys = jwks.get('keys') if isinstance(jwks.get('keys'), list) else []
        self.check('apps in-meeting: public app-context JWKS (one Ed25519 key, no private part)',
                   status == 200 and len(keys) == 1 and keys[0].get('kty') == 'OKP' and keys[0].get('crv') == 'Ed25519'
                   and 'd' not in keys[0])

    def apps_oauth_off(self, a):
        status, _ = Client().call('GET', '/v1/oauth/metadata', expected=(200,) + REFUSED)
        self.check('flag off oauth: refused (%d)' % status, status in REFUSED)
        self.emit('platformAcceptanceNote', {'apps': 'OAuth is off: delegated access, installs and in-meeting app '
                                                     'checks need an authorized app and are not run'})

    def apps_events(self, a):
        self.emit('platformAcceptanceNote', {'apps events': 'install event delivery needs an installed app with a '
                                                            'reachable HTTPS webhook and a meeting event: not claimed'})

    # --- flag-off and revocation ---------------------------------------------------------------

    def flag_off(self, t):
        """Stages that are not on: their feature must answer like an unknown / refused route."""
        machine, console, env = t['machine'], t['console'], t['env']['id']
        token = t.get('riderToken') or 'cfut_sbx_acceptance.invalid.invalid'
        session = (200, 403, 404, 405)  # a console 401 is an expired session (re-signed), never "off"
        probes = {
            'users': lambda: machine.call('POST', '/v1/users', {'externalId': 'off-%s' % self.tag},
                                          expected=(201,) + REFUSED)[0],
            'tokens': lambda: console.call('GET', '/v1/environments/%s/allowed-origins' % env, expected=session)[0],
            # Messaging always mounts the route; off it answers 503 FEATURE_DISABLED (contract).
            'chat': lambda: off_status(Client().call('POST', '/v1/messaging/user-sessions',
                                                     {'installationId': 'off-%s' % self.tag},
                                                     token=token, expected=(200, 201, 503) + REFUSED)),
            'calls': lambda: Client().call('POST', '/v1/calling/user-sessions', {'installationId': 'off-%s' % self.tag,
                                                                                 'kind': 'MOBILE'},
                                           token=token, expected=(200, 201) + REFUSED)[0],
            'push': lambda: console.call('GET', '/v1/environments/%s/push-credentials' % env, expected=session)[0],
            'meetings': lambda: Client().call('POST', '/api/v2/conferences/%s/join' % uuid.uuid4(), {}, token=token,
                                              expected=(200, 201) + REFUSED)[0],
            'apps': lambda: console.call('GET', '/v1/projects/%s/app' % t['project']['id'], expected=session)[0],
        }
        for stage in STAGES:
            if not self.on(stage):
                status = probes[stage]()
                self.check('flag off %s: refused (%d)' % (stage, status), status in REFUSED + (503,))

    def revocation(self, t):
        _, value = t['machine'].call('POST', '/v1/users/%s/revoke-sessions' % t['rider']['id'])
        self.check('revocation: revoke-sessions answers tokensNotBefore', bool(value.get('tokensNotBefore')))
        installation = 'acc-revoked-%s' % self.tag
        if self.on('chat'):
            product, body = '/v1/messaging/user-sessions', {'installationId': installation}
        elif self.on('calls'):
            product, body = '/v1/calling/user-sessions', {'installationId': installation, 'kind': 'MOBILE'}
        else:
            return

        def refused():
            status, _ = Client().call('POST', product, body, token=t['riderToken'], expected=(200, 201, 401))
            return status == 401
        self.check('revocation: a token minted before revoke-sessions is refused within 60 s',
                   bool(retry(refused, *REVOCATION_WAIT)))

    def close(self):
        problems = []
        for method, path, client in self.cleanup:
            try:
                body = {'origins': []} if method == 'PUT' else None
                if method == 'POST' and path.startswith('/v1/calls/') and path.endswith('/cancel'):
                    # Calling control routes require the current version; an empty
                    # request is rejected before authorization or state handling.
                    code, view = client.call('GET', path[:-7], expected=(200, 404))
                    if code == 404:
                        continue
                    if view.get('state') in ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED'):
                        continue
                    body = {'expectedVersion': view['version']}
                client.call(method, path, body, expected=(200, 204, 404, 409))
            except (da.Failed, OSError) as error:
                problems.append(str(error))
        self.emit('platformAcceptanceCleanup', {'steps': len(self.cleanup), 'problems': problems})


def apps_plan(journey, a, b):
    """The `apps` stage groups of the features on (register first; every other feature needs it)."""
    plan = [('apps A', journey.apps_register, a), ('apps B', journey.apps_register, b),
            ('apps isolation', journey.apps_isolation, a, b)]
    if journey.app('oauth'):
        plan.append(('apps oauth', journey.apps_oauth, a, b))
        if journey.app('installs'):
            plan.append(('apps installs', journey.apps_installs, a))
        plan.append(('apps in-meeting', journey.apps_in_meeting, a))
    else:
        plan.append(('apps oauth off', journey.apps_oauth_off, a))
    plan.append(('apps directory', journey.apps_directory, a))
    if journey.app('events'):
        plan.append(('apps events', journey.apps_events, a))
    return plan


def run(run_id, stages, emit, apps=None):
    """{check: PASS|FAIL}. `stages` ⊆ STAGES, the phases that are on; `apps` ⊆ APP_FEATURES,
    the CF Meet Apps features on (with the `apps` stage; default: all)."""
    journey = Journey(run_id, stages, emit, apps)
    accounts = app_acceptance.accounts('platform-' + journey.tag, count=2)
    try:
        a = journey.tenant('A', accounts[0])
        b = journey.tenant('B', accounts[1])
        pair = (a, b)
        plan = []
        if journey.on('users'):
            plan += [('users A', journey.users, a), ('users B', journey.users, b),
                     ('users isolation', journey.users_isolation, a, b)]
        if journey.on('tokens'):
            plan += [('tokens A', journey.tokens, a), ('tokens B', journey.tokens, b), ('cors', journey.cors, a),
                     ('tokens isolation', journey.tokens_isolation, a, b)]
        for stage in ('chat', 'calls', 'push', 'meetings'):
            if journey.on(stage):
                plan += [('%s %s' % (stage, t['label']), getattr(journey, stage), t) for t in pair]
                plan.append(('%s isolation' % stage, getattr(journey, stage + '_isolation'), a, b))
        if journey.on('apps'):
            plan += apps_plan(journey, a, b)
        plan.append(('flag off', journey.flag_off, a))
        if journey.on('tokens'):
            plan.append(('revocation', journey.revocation, a))
        for name, fn, *args in plan:
            journey.step(name, fn, *args)
    except da.Failed as error:
        if str(error) not in journey.checks:  # a fatal check is already recorded
            journey.check('setup: %s' % error, False)
    except (OSError, KeyError, ValueError) as error:
        journey.check('setup: %s' % type(error).__name__, False)
    finally:
        journey.close()
    emit('platformAcceptanceVerdict', {'stages': list(journey.stages), 'apps': list(journey.apps),
                                       'passed': sum(v == 'PASS' for v in journey.checks.values()),
                                       'checks': len(journey.checks)})
    return journey.checks
