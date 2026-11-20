"""Offline tests: the CF Meet Platform "Build" two-tenant acceptance (platform_acceptance.py)."""
import base64
import contextlib
import hashlib
import io
import json
import re
import unittest
import urllib.parse
from unittest.mock import patch

import developer_acceptance as da
import platform_acceptance as pa

ALL = pa.STAGES
UUID = re.compile(r'[0-9a-f-]{36}')


class FakePlatform:
    """Just enough of api.cfmeet.com (console, gateway, Developer Console, Messaging, Calling,
    Meeting Service) for the acceptance: tenant-scoped state, 404 for foreign resources, flags."""

    def __init__(self, stages=ALL, broken=(), apps=pa.APP_FEATURES):
        self.stages = set(stages)
        # CF Meet Apps features served (only while the apps stage is on).
        self.apps_on = set(apps) if 'apps' in self.stages else set()
        # Behaviours to break: 'isolation-users', 'chat-receive', 'cors-credential-route',
        # 'refresh-reuse', 'cross-client-code', 'apps-isolation'.
        self.broken = set(broken)
        self.n = 0
        self.calls = []
        self.users, self.tokens, self.client_tokens, self.signaling = {}, {}, {}, {}
        self.not_before = {}
        self.origins, self.conversations, self.messages = {}, {}, {}
        self.calls_, self.invitations, self.spaces, self.conferences = {}, {}, {}, {}
        self.participants, self.participant_tokens, self.push = {}, {}, {}
        self.revoked = set()
        self.scopes = {}
        self.apps, self.codes, self.grants, self.oauth_tokens, self.app_spaces = {}, {}, {}, {}, {}
        self.issued_codes = []

    def id(self, prefix='id'):
        self.n += 1
        return '%s%d' % (prefix, self.n)

    # The patched Client._call
    def __call__(self, client, method, path, body, expected, token, idempotent, headers=None):
        bearer = token or client.token
        headers = dict(client.headers, **(headers or {}))
        self.calls.append((method, path.split('?')[0], client.base, bearer))
        client.last_headers = {'cache-control': 'no-store'}
        status, value = self.route(client, method, path, body, bearer, headers)
        if status not in expected:
            raise da.Failed('%s %s answered %d' % (method, path.split('?')[0], status))
        return status, value

    # --- helpers --------------------------------------------------------------------------

    def on(self, stage):
        return stage in self.stages

    def machine_env(self, bearer):
        m = re.fullmatch(r'cfmk_sbx_(\w+)_s', bearer or '')
        return m.group(1) if m and bearer not in self.revoked else None

    def session(self, bearer):
        m = re.fullmatch(r'(\w+)-tok', bearer or '')
        return m.group(1) if m else None

    def user_token(self, bearer):
        t = self.tokens.get(bearer)
        if not t or t['iat'] < self.not_before.get(t['user'], 0):
            return None
        return t

    @staticmethod
    def nf():
        return 404, {'error': {'code': 'NOT_FOUND'}}

    def mint(self, env, user, grants):
        scopes = self.scopes[env]
        need = {'msg': 'messages.read', 'call': 'calls.write', 'mtg': 'meetings.write'}
        if any(need[k] not in scopes for k in grants):
            return 403, {'error': {'code': 'INSUFFICIENT_SCOPE'}}  # the Developer Platform's code
        token = 'cfut_sbx_%s.payload.SIG%s' % (self.id('t'), 'x')
        self.n += 1
        self.tokens[token] = dict(env=env, user=user, grants=grants, iat=self.n)
        return 201, {'token': token, 'tokenId': self.id('tid'), 'expiresAt': '2026-11-18T13:00:00Z'}

    def new_user(self, env, body):
        if not re.fullmatch(r'[!-~]{1,128}', body['externalId']) or re.fullmatch(r'\+?[0-9]{6,15}', body['externalId']):
            return 400, {'error': {'code': 'VALIDATION_ERROR'}}
        for uid, u in self.users.items():
            if u['env'] == env and u['externalId'] == body['externalId']:
                return 200, dict(u, id=uid)
        uid = self.id('u')
        self.users[uid] = dict(env=env, externalId=body['externalId'], displayName=body.get('displayName'), status='ACTIVE')
        # Contract: POST /v1/users answers the EndUser itself (not wrapped).
        return 201, dict(self.users[uid], id=uid)

    def visible_user(self, env, uid):
        u = self.users.get(uid)
        if not u or (u['env'] != env and 'isolation-users' not in self.broken):
            return None
        return u

    # --- routing --------------------------------------------------------------------------

    def route(self, client, method, path, body, bearer, headers):
        bare = path.split('?')[0]
        if client.base == pa.RELAY:
            return self.relay(method, bare, body, bearer, headers)
        if path == '/api/v1/auth/otp/request':
            return 200, {}
        if path == '/api/v1/auth/otp/verify':
            return 200, {'access_token': body['email'].split('@')[0] + '-tok'}
        if method == 'OPTIONS':
            return self.preflight(client, bare, headers)
        if bare.startswith(('/v1/oauth/', '/v1/app-directory')) or bare == '/api/v2/app-context-keys':
            return self.apps_public(method, bare, body, bearer, headers)
        if bearer and bearer.startswith('cfoa_'):
            return self.apps_token(method, bare, bearer)
        owner = self.session(bearer)
        if owner and bare.startswith('/api/v2/'):
            return self.apps_session(owner, method, path, bare, body)
        if owner:
            return self.console(owner, method, bare, body)
        env = self.machine_env(bearer)
        if env:
            return self.machine(env, method, path, bare, body, headers)
        scheme, _, credential = headers.get('Authorization', '').partition(' ')
        if scheme == 'CF-Client-Token':
            ct = self.client_tokens.get(credential)
            return self.device_client(ct, method, bare, body) if ct else (401, {'error': {'code': 'AUTHENTICATION_FAILED'}})
        if bearer and bearer.startswith('cfut_'):
            return self.device_user_token(method, bare, body, bearer)
        if bearer in self.client_tokens:
            return 401, {'error': {'code': 'AUTHENTICATION_FAILED'}}  # client tokens never ride Bearer
        if bearer in self.signaling:
            return self.device_signaling(self.signaling[bearer], method, bare)
        if headers.get('X-Participant-Token') in self.participant_tokens:
            conf = self.participant_tokens[headers['X-Participant-Token']]
            if bare == '/api/v2/conferences/%s/messages' % conf:
                return 200, {'success': True, 'data': {'items': []}}
        return 401, {}

    def preflight(self, client, path, headers):
        origin = headers.get('Origin')
        registered = any(origin in o for o in self.origins.values())
        device_route = path == '/v1/conversations'
        if origin == pa.WEB_ORIGIN or (self.on('tokens') and registered
                                       and (device_route or 'cors-credential-route' in self.broken)):
            client.last_headers['access-control-allow-origin'] = origin
        return 204, {}

    def console(self, owner, method, path, body):
        env = owner + 'env'
        if method == 'POST' and path == '/v1/organizations':
            return 201, {'id': owner + 'org', 'myRole': 'OWNER', 'state': 'ACTIVE'}
        if re.fullmatch(r'/v1/organizations/\w+/projects', path):
            return 201, {'id': owner + 'proj'}
        m = re.fullmatch(r'/v1/projects/(\w+)/environments', path)
        if m:
            return 201, {'id': env, 'kind': 'SANDBOX', 'projectId': m.group(1)}
        m = re.fullmatch(r'/v1/projects/(\w+)/scopes', path)
        if m:
            self.scopes.setdefault(env, set()).add(body['scope'])
            return 200, {}
        if path.endswith('/service-accounts'):
            return 201, {'id': self.id('sa')}
        if path.endswith('/credentials') and method == 'POST':
            return 201, {'secret': 'cfmk_sbx_%s_s' % env, 'credential': {'id': self.id('cr'), 'scopes': body['scopes']}}
        m = re.fullmatch(r'/v1/environments/(\w+)/credentials/\w+/revoke', path)
        if m:
            self.revoked.add('cfmk_sbx_%s_s' % m.group(1))
            return 200, {}
        if re.fullmatch(r'/v1/(projects/\w+/archive|organizations/\w+/close)', path):
            return 200, {}
        m = re.fullmatch(r'/v1/projects/(\w+)/app(/secret|/test-accounts)?', path)
        if m:
            return self.console_app(owner, method, m.group(1), m.group(2), body)
        m = re.fullmatch(r'/v1/environments/(\w+)/(users|allowed-origins|test-user-tokens|push-credentials)', path)
        if m:
            target, feature = m.groups()
            stage = {'users': 'users', 'allowed-origins': 'tokens', 'test-user-tokens': 'tokens',
                     'push-credentials': 'push'}[feature]
            if not self.on(stage) or target != env:
                return self.nf()
            if feature == 'users':
                return 200, {'items': [dict(u, id=i) for i, u in self.users.items() if u['env'] == env], 'nextCursor': None}
            if feature == 'allowed-origins':
                if method == 'PUT':
                    if any('*' in o for o in body['origins']):
                        return 400, {'error': {'code': 'VALIDATION_ERROR'}}
                    self.origins[env] = list(body['origins'])
                return 200, {'environmentId': env, 'origins': self.origins.get(env, [])}
            if feature == 'test-user-tokens':
                status, created = self.new_user(env, body)
                _, minted = self.mint(env, created['id'], body['grants'])
                return 201, dict(minted, user=created)
            if method == 'POST':
                # A well-shaped key with the PRODUCTION APNs host in a SANDBOX environment.
                if body.get('apnsHost') == 'PRODUCTION' and str(body.get('p8', '')).startswith('-----BEGIN PRIVATE KEY-----'):
                    return 422, {'error': {'code': 'POLICY_VIOLATION'}}
                return 400, {'error': {'code': 'VALIDATION_ERROR'}}
            return 200, {'items': self.push.get(env, [])}
        return 404, {}

    def machine(self, env, method, path, bare, body, headers):
        if bare.startswith('/v1/users'):
            if not self.on('users'):
                return self.nf()
            if bare == '/v1/users' and method == 'POST':
                return self.new_user(env, body)
            if bare == '/v1/users' and method == 'GET':
                ext = path.split('externalId=')[1]
                return 200, {'items': [dict(u, id=i) for i, u in self.users.items()
                                       if u['env'] == env and u['externalId'] == ext], 'nextCursor': None}
            if bare == '/v1/users/tokens':
                if not self.on('tokens'):
                    return self.nf()
                _, created = self.new_user(env, body)
                status, minted = self.mint(env, created['id'], body['grants'])
                return status, dict(minted, user=created)
            m = re.fullmatch(r'/v1/users/(\w+)(/tokens|/revoke-sessions)?', bare)
            u = self.visible_user(env, m.group(1))
            if u is None:
                return self.nf()
            if m.group(2) == '/tokens':
                return self.mint(env, m.group(1), body['grants']) if self.on('tokens') else self.nf()
            if m.group(2) == '/revoke-sessions':
                self.n += 1
                self.not_before[m.group(1)] = self.n
                return 200, {'tokensNotBefore': '2026-11-18T12:00:00Z'}
            if method == 'PATCH':
                u.update(body)
            if method == 'DELETE':
                del self.users[m.group(1)]
                return 204, {}
            return 200, dict(u, id=m.group(1))
        if bare == '/v1/conversations' and method == 'POST':
            conv = self.id('conv')
            self.conversations[conv] = dict(env=env, members={headers['CF-Acting-Principal'], body['counterpartPrincipalId']})
            return 201, {'conversationId': conv, 'securityProfile': body['securityProfile']}
        m = re.fullmatch(r'/v1/conversations/(\w+)', bare)
        if m:
            c = self.conversations.get(m.group(1))
            return (200, {}) if c and c['env'] == env else self.nf()
        if bare == '/v1/calls' and method == 'POST':
            call = self.id('call')
            self.calls_[call] = dict(env=env)
            target = body['targetPrincipalIds'][0]
            invitation = self.id('inv')
            self.invitations[invitation] = dict(call=call, user=target)
            return 201, {'call': {'callId': call, 'state': 'RINGING'}, 'liveInvitationCount': 1}
        m = re.fullmatch(r'/v1/calls/(\w+)(/cancel)?', bare)
        if m:
            c = self.calls_.get(m.group(1))
            return (200, {}) if c and c['env'] == env else self.nf()
        return 404, {}

    def device_user_token(self, method, path, body, bearer):
        t = self.user_token(bearer)
        if path == '/v1/messaging/user-sessions':
            if not self.on('chat'):
                # The route is always mounted; off it answers 503 FEATURE_DISABLED (a valid token).
                return (503, {'error': {'code': 'FEATURE_DISABLED'}}) if t else (401, {})
            if not t:
                return 401, {'error': {'code': 'USER_TOKEN_INVALID'}}
            ct = 'cfct_' + self.id()
            self.client_tokens[ct] = dict(env=t['env'], user=t['user'], installation=body['installationId'])
            return 201, {'principalId': t['user'], 'sessionDeviceId': self.id('sd'), 'clientToken': ct}
        if path == '/v1/calling/user-sessions':
            if not self.on('calls'):
                return self.nf()
            if not t:
                return 401, {}
            sig = 'cfsig_' + self.id()
            self.signaling[sig] = dict(env=t['env'], user=t['user'])
            return 201, {'principalId': t['user'], 'endpointId': self.id('ep'), 'signalingCredential': sig}
        m = re.fullmatch(r'/v1/messaging/user-push-registrations/([\w.-]+)', path)
        if m:
            if not t:
                return 401, {}
            if method == 'PUT':
                return 200, {'installationId': m.group(1), 'platform': body['platform'], 'status': 'ACTIVE'}
            return 204, {}
        m = re.fullmatch(r'/api/v2/conferences/(\w+)/join', path)
        if m:
            if not self.on('meetings'):
                return 401, {}
            if not t:
                return 401, {}
            conf = self.conferences.get(m.group(1))
            if not conf or conf['env'] != t['env']:
                return self.nf()
            if not any(g['c'] == m.group(1) for g in t['grants'].get('mtg', [])):
                return 403, {'error': {'code': 'FORBIDDEN'}}
            key = (m.group(1), t['user'])
            pid = self.participants.setdefault(key, self.id('p'))
            pt = 'pt_' + self.id()
            self.participant_tokens[pt] = m.group(1)
            return 200, {'participant': {'id': pid, 'role': t['grants']['mtg'][0]['r'],
                                         'displayName': self.users[t['user']]['displayName']},
                         'participantToken': pt}
        return 404, {}

    def device_client(self, ct, method, path, body):
        if path == '/v1/realtime/tickets':
            return 201, {'ticket': 'rt_x'}
        if re.fullmatch(r'/v1/messaging/user-push-registrations/([\w.-]+)', path):
            return 401, {}  # these take the user token, never a client token
        m = re.fullmatch(r'/v1/conversations/(\w+)/(messages|mls/state)', path)
        if m:
            c = self.conversations.get(m.group(1))
            if not c or c['env'] != ct['env'] or ct['user'] not in c['members']:
                return self.nf()
            if m.group(2) == 'mls/state':
                return 401, {'error': {'code': 'CLIENT_TOKENS_NOT_ENABLED'}}  # not a partner device route
            log = self.messages.setdefault(m.group(1), [])
            if method == 'POST':
                log.append(dict(body))
                return 201, {}
            return 200, {'messages': [] if 'chat-receive' in self.broken else list(log)}
        return 404, {}

    def device_signaling(self, sig, method, path):
        if path == '/v1/calling/invitations':
            return 200, {'endpointId': 'ep', 'data': [dict(invitationId=i, callId=v['call']) for i, v in self.invitations.items()
                                                      if v['user'] == sig['user']]}
        m = re.fullmatch(r'/v1/calling/invitations/(\w+)/accept', path)
        if m:
            v = self.invitations.get(m.group(1))
            return (200, {}) if v and v['user'] == sig['user'] else self.nf()
        return 404, {}


    # --- CF Meet Apps -----------------------------------------------------------------------

    def app_on(self, feature):
        return feature in self.apps_on

    def console_app(self, owner, method, project, suffix, body):
        if not self.app_on('register') or (project != owner + 'proj' and 'apps-isolation' not in self.broken):
            return self.nf()
        app = self.apps.get(project)
        if suffix is None and method == 'PUT':
            created = app is None
            app = self.apps.setdefault(project, dict(owner=owner, secret=None, tests=[]))
            app['manifest'] = dict(body)
            return (201 if created else 200), self.app_view(project)
        if app is None:
            return self.nf()
        if suffix == '/secret':
            app['secret'] = 'cfcs_sbx_%s_%s' % (self.id('k'), 's' * 8)
            return 201, {'clientId': project, 'clientSecret': app['secret'], 'secretId': self.id('sec')}
        if suffix == '/test-accounts':
            app['tests'] = list(body['accountIds'])
            return 200, {'testAccountIds': app['tests']}
        return 200, self.app_view(project)

    def app_view(self, project):
        app = self.apps[project]
        return {'appId': project, 'clientId': project, 'status': 'DRAFT', 'manifest': app['manifest'],
                'testAccountIds': app['tests']}

    def authorization(self, owner, q):
        """(status, error code, app) of an authorization request of `owner`'s account."""
        app = self.apps.get(q.get('clientId'))
        if not self.app_on('oauth'):
            return 404, 'FEATURE_DISABLED', None
        if not app or q.get('redirectUri') not in app['manifest']['redirectUris']:
            return 400, 'OAUTH_REQUEST_INVALID', None
        if q.get('codeChallengeMethod') != 'S256' or not q.get('codeChallenge'):
            return 400, 'OAUTH_REQUEST_INVALID', None
        if 'acct-' + owner not in app['tests']:
            return 400, 'OAUTH_REQUEST_INVALID', None
        return 200, None, app

    def apps_session(self, owner, method, path, bare, body):
        account = 'acct-' + owner
        if bare == '/api/v2/oauth/authorize/context':
            q = {k: v[0] for k, v in urllib.parse.parse_qs(path.split('?', 1)[1]).items()}
            status, error, _ = self.authorization(owner, {'clientId': q.get('client_id'), 'redirectUri': q.get('redirect_uri'),
                                                         'codeChallenge': q.get('code_challenge'),
                                                         'codeChallengeMethod': q.get('code_challenge_method')})
            if status != 200:
                return status, {'error': {'code': error}}
            out = {'app': {'clientId': q['client_id'], 'status': 'DRAFT', 'name': 'x'}, 'target': 'USER',
                   'redirectUri': q['redirect_uri'], 'state': q.get('state')}
            if q.get('install') == 'organization':
                if not self.app_on('installs'):
                    return 404, {'error': {'code': 'FEATURE_DISABLED'}}
                out.update(target='ORGANIZATION', organizationInstall={'allowed': False, 'reason': 'NOT_A_MEMBER'})
            return 200, out
        if bare == '/api/v2/oauth/authorize/approve':
            status, error, app = self.authorization(owner, body)
            if status != 200:
                return status, {'error': {'code': error}}
            grant = next((g for g, v in self.grants.items() if v['client'] == body['clientId'] and v['account'] == account
                          and v['active']), None) or self.id('grant')
            self.grants.setdefault(grant, dict(client=body['clientId'], account=account, active=True))
            code_ = 'code_' + self.id()
            self.issued_codes.append(code_)
            self.codes[code_] = dict(client=body['clientId'], redirect=body['redirectUri'], grant=grant,
                                     challenge=body['codeChallenge'])
            return 200, {'redirectTo': '%s?%s' % (body['redirectUri'], urllib.parse.urlencode(
                {'code': code_, 'state': body['state']}))}
        if bare == '/api/v2/me/apps':
            return 200, {'items': [{'grantId': g, 'appId': v['client']} for g, v in self.grants.items()
                                   if v['account'] == account and v['active']]}
        m = re.fullmatch(r'/api/v2/me/apps/(\w+)', bare)
        if m and method == 'DELETE':
            g = self.grants.get(m.group(1))
            if not g or g['account'] != account:
                return self.nf()
            g['active'] = False
            return 204, {}
        return 404, {}

    def client_auth(self, headers):
        value = headers.get('Authorization', '')
        if not value.startswith('Basic '):
            return None
        client_id, secret = (urllib.parse.unquote(x) for x in base64.b64decode(value[6:]).decode().split(':', 1))
        app = self.apps.get(client_id)
        return client_id if app and app['secret'] == secret else None

    def issue(self, client, grant):
        access, refresh = 'cfoa_sbx_%s_a' % self.id(), 'cfor_sbx_%s_r' % self.id()
        self.oauth_tokens[access] = dict(kind='access', grant=grant, active=True)
        self.oauth_tokens[refresh] = dict(kind='refresh', grant=grant, active=True)
        return 200, {'access_token': access, 'token_type': 'Bearer', 'expires_in': 3600, 'refresh_token': refresh,
                     'scope': ' '.join(pa.APP_USER_SCOPES), 'installation': {'id': grant, 'target': 'USER'}}

    def live(self, token, kind):
        t = self.oauth_tokens.get(token)
        return t if t and t['kind'] == kind and t['active'] and self.grants[t['grant']]['active'] else None

    def apps_public(self, method, path, body, bearer, headers):
        oauth_off = not self.app_on('oauth')
        if path == '/v1/oauth/metadata':
            return self.nf() if oauth_off else (200, {'token_endpoint': pa.PUBLIC + '/v1/oauth/token'})
        if path in ('/v1/oauth/token', '/v1/oauth/revoke'):
            if oauth_off:
                return self.nf()
            assert headers.get('Content-Type') == pa.FORM and isinstance(body, str)
            client = self.client_auth(headers)
            if not client:
                return 401, {'error': 'invalid_client'}
            form = dict(urllib.parse.parse_qsl(body))
            if path == '/v1/oauth/revoke':
                if form.get('token') in self.oauth_tokens:
                    self.oauth_tokens[form['token']]['active'] = False
                return 200, {}
            if form['grant_type'] == 'authorization_code':
                c = self.codes.pop(form['code'], None)
                if not c or (c['client'] != client and 'cross-client-code' not in self.broken) \
                        or c['redirect'] != form['redirect_uri']:
                    return 400, {'error': 'invalid_grant'}
                challenge = base64.urlsafe_b64encode(hashlib.sha256(form['code_verifier'].encode()).digest()).rstrip(b'=')
                if challenge.decode() != c['challenge']:
                    return 400, {'error': 'invalid_grant'}
                return self.issue(client, c['grant'])
            t = self.oauth_tokens.get(form['refresh_token'])
            if t and not t['active'] and 'refresh-reuse' not in self.broken:
                self.grants[t['grant']]['active'] = False  # reuse of a superseded refresh token
            if not t or not t['active'] or not self.grants[t['grant']]['active']:
                return 400, {'error': 'invalid_grant'}
            t['active'] = False
            return self.issue(client, t['grant'])
        if path == '/v1/oauth/userinfo':
            if oauth_off:
                return self.nf()
            t = self.live(bearer, 'access')
            if not t:
                return 401, {'error': 'invalid_token'}
            return 200, {'sub': 'pairwise-%s' % hashlib.sha256(t['grant'].encode()).hexdigest()[:16], 'name': 'Acceptance',
                         'picture': None, 'email': 'x@example.invalid'}
        if path.startswith('/v1/app-directory'):
            if not self.app_on('directory'):
                return self.nf()
            return (200, {'items': [], 'nextCursor': None}) if path == '/v1/app-directory' else self.nf()
        if path == '/api/v2/app-context-keys':
            if not self.app_on('in-meeting'):
                return 404, {'error': {'code': 'FEATURE_DISABLED'}}
            return 200, {'keys': [{'kty': 'OKP', 'crv': 'Ed25519', 'x': 'AAAA', 'kid': 'k1', 'use': 'sig', 'alg': 'EdDSA'}]}
        return 404, {}

    def apps_token(self, method, path, bearer):
        t = self.live(bearer, 'access')
        if not t:
            return 401, {}
        if path == '/v1/meetings/meeting-spaces' and method == 'POST':
            if not self.app_on('delegated'):
                return 401, {}
            space = self.id('aspace')
            self.app_spaces[space] = 'ACTIVE'
            return 201, {'id': space}
        m = re.fullmatch(r'/v1/meetings/meeting-spaces/(\w+)/archive', path)
        if m and self.app_on('delegated') and m.group(1) in self.app_spaces:
            self.app_spaces[m.group(1)] = 'ARCHIVED'
            return 200, {}
        if path.startswith('/v1/users'):
            return 403, {'error': {'code': 'FORBIDDEN'}}
        return 404, {}

    def relay(self, method, path, body, bearer, headers):
        assert headers.get('Host') == 'api.cfmeet.com' and headers.get('X-Forwarded-Proto') == 'https'
        env = self.machine_env(bearer)
        if not env or not self.on('meetings'):
            return 401, {}
        if path == '/api/v2/meeting-spaces':
            space = self.id('space')
            self.spaces[space] = env
            return 201, {'success': True, 'data': {'id': space}}
        m = re.fullmatch(r'/api/v2/meeting-spaces/(\w+)/(conferences|archive)', path)
        if m:
            if self.spaces.get(m.group(1)) != env:
                return self.nf()
            if m.group(2) == 'archive':
                return 200, {}
            conf = self.id('conf')
            self.conferences[conf] = dict(env=env, space=m.group(1))
            return 201, {'id': conf, 'meetingSpaceId': m.group(1)}
        m = re.fullmatch(r'/api/v2/conferences/(\w+)(/end)?', path)
        if m:
            c = self.conferences.get(m.group(1))
            return (200, {}) if c and c['env'] == env else self.nf()
        return 404, {}


def run_flow(api, stages=ALL, apps=None):
    accounts = [{'email': 'pa@example.invalid', 'userId': 'acct-pa'}, {'email': 'pb@example.invalid', 'userId': 'acct-pb'}]
    out = io.StringIO()
    emitted = []

    def emit(kind, value):
        emitted.append((kind, value))
        print(json.dumps({kind: value}, default=str))

    with patch.object(pa.app_acceptance, 'accounts', lambda ns, count: accounts), \
            patch.object(pa.app_acceptance, 'controlled_code', lambda key, url=None: '024680'), \
            patch.object(pa.Client, '_call', lambda self, *a: api(self, *a)), \
            patch.object(pa.time, 'sleep', lambda s: None), contextlib.redirect_stdout(out), \
            patch.object(pa.urllib.request, 'urlopen', side_effect=AssertionError('no network in tests')):
        checks = pa.run('77-1', stages, emit, apps=apps)
    return checks, out.getvalue(), emitted


class Journey(unittest.TestCase):
    def test_every_stage_passes_isolation_holds_and_everything_is_cleaned_up(self):
        api = FakePlatform()
        checks, printed, _ = run_flow(api)
        failed = {k: v for k, v in checks.items() if v != 'PASS'}
        self.assertEqual(failed, {})
        self.assertGreaterEqual(len(checks), 60)
        names = ' | '.join(checks)
        for stage in ('users A', 'tokens B', 'chat A', 'calls B', 'push A', 'meetings B', 'isolation users',
                      'isolation chat', 'isolation calls', 'isolation meetings', 'cors:', 'revocation:'):
            self.assertIn(stage, names)
        self.assertNotIn('flag off', names)
        # Cleanup: users deleted, origins cleared, calls cancelled, conferences ended before their
        # spaces are archived, credentials revoked before projects archived and workspaces closed.
        self.assertEqual(api.users, {})
        self.assertEqual(api.origins, {'paenv': [], 'pbenv': []})
        order = [(m, p) for m, p, _, _ in api.calls]
        for env in ('pa', 'pb'):
            close = order.index(('POST', '/v1/organizations/%sorg/close' % env))
            self.assertLess(order.index(('POST', '/v1/projects/%sproj/archive' % env)), close)
        self.assertEqual(len(api.conferences), 2)
        for conf, value in api.conferences.items():
            self.assertLess(order.index(('POST', '/api/v2/conferences/%s/end' % conf)),
                            order.index(('POST', '/api/v2/meeting-spaces/%s/archive' % value['space'])))
        self.assertTrue(any(p.endswith('/cancel') for _, p in order))
        # Machine meeting calls go through the gateway relay, device joins through the public host.
        relayed = {p for _, p, base, _ in api.calls if base == pa.RELAY}
        self.assertTrue(relayed and all(p.startswith('/api/v2/') for p in relayed))
        self.assertTrue(all(base == pa.PUBLIC for _, p, base, _ in api.calls if p.endswith('/join')))

    def test_output_is_json_and_never_carries_a_token_key_or_secret(self):
        api = FakePlatform()
        _, printed, _ = run_flow(api)
        for line in printed.splitlines():
            json.loads(line)
        secrets = (list(api.tokens) + list(api.client_tokens) + list(api.signaling) + list(api.participant_tokens)
                   + ['cfmk_sbx_paenv_s', 'cfmk_sbx_pbenv_s', 'pa-tok', 'pb-tok'])
        self.assertGreater(len(secrets), 20)
        for secret in secrets:
            self.assertNotIn(secret, printed)
            self.assertNotIn(secret.split('.')[0], printed)
        self.assertIn('platformAcceptanceVerdict', printed)
        self.assertIn('platformAcceptanceCleanup', printed)

    def test_scopes_and_grants_follow_the_enabled_stages(self):
        api = FakePlatform(stages=('users', 'tokens'))
        checks, _, _ = run_flow(api, ('users', 'tokens'))
        self.assertEqual({k: v for k, v in checks.items() if v != 'PASS'}, {})
        self.assertEqual(api.scopes['paenv'], {'principals.read', 'principals.write', 'user_tokens.issue'})
        self.assertTrue(all(t['grants'] == {} or set(t['grants']) == {'mtg'} for t in api.tokens.values()))
        self.assertIn("tokens A: grant without its scope refused (403 INSUFFICIENT_SCOPE)", checks)
        # Every phase that is not on is proven off.
        for stage in ('chat', 'calls', 'push', 'meetings'):
            self.assertTrue(any(k.startswith('flag off %s' % stage) for k in checks), stage)

    def test_users_only_proves_users_and_every_other_feature_off(self):
        api = FakePlatform(stages=('users',))
        checks, _, _ = run_flow(api, ('users',))
        self.assertEqual({k: v for k, v in checks.items() if v != 'PASS'}, {})
        self.assertEqual(sorted(k.split(':')[0] for k in checks if k.startswith('flag off')),
                         ['flag off apps', 'flag off calls', 'flag off chat', 'flag off meetings', 'flag off push',
                          'flag off tokens'])
        self.assertFalse(any('revocation' in k for k in checks))

    def test_a_feature_left_on_while_its_phase_is_off_fails_the_flag_off_check(self):
        api = FakePlatform(stages=('users', 'tokens', 'push'))
        checks, _, _ = run_flow(api, ('users', 'tokens'))
        self.assertEqual([k for k, v in checks.items() if v != 'PASS'], ['flag off push: refused (200)'])

    def test_cross_tenant_leaks_fail_and_the_journey_continues(self):
        api = FakePlatform(broken=('isolation-users',))
        checks, _, _ = run_flow(api)
        failed = [k for k, v in checks.items() if v != 'PASS']
        self.assertTrue(failed and all('isolation' in k for k in failed), failed)
        self.assertTrue(any(k.startswith('meetings B') and v == 'PASS' for k, v in checks.items()))
        self.assertEqual(api.users, {})

    def test_partner_website_on_a_credential_route_fails(self):
        checks, _, _ = run_flow(FakePlatform(broken=('cors-credential-route',)))
        self.assertEqual([k for k, v in checks.items() if v != 'PASS'],
                         ['cors: partner website never allowed on a credential-only route (/v1/users)'])

    def test_an_undelivered_message_fails_only_chat_receive(self):
        checks, _, _ = run_flow(FakePlatform(broken=('chat-receive',)))
        self.assertEqual(sorted(k for k, v in checks.items() if v != 'PASS'),
                         ["chat A: rider's device sends, customer's device receives",
                          "chat B: rider's device sends, customer's device receives"])

    def test_a_failed_setup_stops_early_and_still_cleans_up(self):
        api = FakePlatform()
        original = api.console

        def console(owner, method, path, body):
            if path.endswith('/credentials') and owner == 'pb':
                return 201, {'secret': 'nope', 'credential': {'id': 'x', 'scopes': []}}
            return original(owner, method, path, body)
        api.console = console
        checks, printed, _ = run_flow(api)
        self.assertEqual(checks['setup B: platform credential with the stage scopes'], 'FAIL')
        self.assertFalse(any(k.startswith('users') for k in checks))
        self.assertTrue(any(p == '/v1/organizations/pborg/close' for _, p, _, _ in api.calls))
        self.assertIn('"passed"', printed)

    def test_unknown_stages_are_refused(self):
        with self.assertRaises(AssertionError):
            pa.Journey('1-1', ('users', 'telecom'), lambda k, v: None)
        with self.assertRaises(AssertionError):
            pa.Journey('1-1', ('users', 'apps'), lambda k, v: None, apps=('oauth',))  # register carries the rest
        with self.assertRaises(AssertionError):
            pa.Journey('1-1', ('users', 'apps'), lambda k, v: None, apps=('register', 'telecom'))
        self.assertEqual(pa.Journey('1-1', ('users',), lambda k, v: None, apps=('register',)).apps, ())

    def test_data_unwraps_meeting_envelopes_only(self):
        self.assertEqual(pa.data({'success': True, 'data': {'id': 'x'}}), {'id': 'x'})
        self.assertEqual(pa.data({'id': 'y'}), {'id': 'y'})
        self.assertEqual(pa.data([]), {})
        self.assertEqual(pa.tampered('abcA')[-1], 'B')
        self.assertEqual(pa.tampered('abcd')[-1], 'A')


BUILD = pa.BUILD_STAGES


class Apps(unittest.TestCase):
    """The CF Meet Apps stage (ADR 0064 / 0065)."""

    def test_every_feature_passes_and_grants_are_revoked_at_cleanup(self):
        api = FakePlatform()
        checks, printed, _ = run_flow(api)
        self.assertEqual({k: v for k, v in checks.items() if v != 'PASS'}, {})
        names = ' | '.join(checks)
        for name in ('apps A: DRAFT app registered', 'apps B: client secret issued once', 'isolation apps: B\'s console reads',
                     'apps oauth: consent context for the DRAFT test account', 'apps oauth: PKCE S256 required',
                     'apps oauth: an account that is not a DRAFT test account is refused', 'apps oauth: code exchanged',
                     'apps oauth: userinfo answers a pairwise sub', 'apps oauth: Connected apps lists the grant',
                     'without the OAuth scheme -> 403', 'apps oauth: refresh rotates both tokens',
                     'apps oauth: revoke answers 200', 'a superseded refresh token is refused',
                     'its reuse revoked the whole grant', "isolation apps: B's client cannot redeem A's code",
                     'apps delegated: the access token creates a meeting space', 'apps delegated: the user\'s own space archived',
                     'apps installs: a workspace install', 'apps directory: anonymous list', 'apps directory: a DRAFT app is never listed',
                     'apps in-meeting: public app-context JWKS'):
            self.assertIn(name, names)
        self.assertNotIn('flag off', names)
        self.assertTrue(api.grants and not any(g['active'] for g in api.grants.values()))
        self.assertEqual(set(api.app_spaces.values()), {'ARCHIVED'})
        self.assertIn('E2EE invisibility', printed)
        self.assertIn('install event delivery', printed)
        # The app's OAuth traffic is exactly what a client sends: form token requests through the
        # public host, consent on the first-party Meeting Service routes with the account session.
        token_calls = [c for c in api.calls if c[1] == '/v1/oauth/token']
        self.assertTrue(token_calls and all(base == pa.PUBLIC and bearer is None for _, _, base, bearer in token_calls))
        self.assertTrue(all(bearer == 'pa-tok' for _, p, _, bearer in api.calls if p == '/api/v2/oauth/authorize/approve'))

    def test_output_never_carries_a_client_secret_code_or_app_token(self):
        api = FakePlatform()
        _, printed, _ = run_flow(api)
        secrets = [a['secret'] for a in api.apps.values()] + list(api.oauth_tokens) + api.issued_codes
        self.assertGreaterEqual(len(secrets), 8)
        for secret in secrets:
            self.assertNotIn(secret, printed)
        self.assertNotIn('code_', printed)
        for line in printed.splitlines():
            json.loads(line)

    def test_register_only_proves_registration_and_oauth_off(self):
        api = FakePlatform(apps=('register',))
        checks, _, _ = run_flow(api, apps=('register',))
        self.assertEqual({k: v for k, v in checks.items() if v != 'PASS'}, {})
        self.assertIn('flag off oauth: refused (404)', checks)
        self.assertIn('flag off directory: refused (404)', checks)
        self.assertFalse(any(k.startswith('apps oauth') for k in checks))
        self.assertTrue(any(k.startswith('apps A: DRAFT app registered') for k in checks))

    def test_features_left_on_while_their_phase_is_off_fail_their_flag_off_checks(self):
        api = FakePlatform()  # everything served
        checks, _, _ = run_flow(api, apps=('register', 'oauth'))
        self.assertEqual(sorted(k for k, v in checks.items() if v != 'PASS'),
                         ['flag off delegated: an OAuth token is refused on /v1/meetings (201)',
                          'flag off directory: refused (200)', 'flag off in-meeting: refused (200)'])

    def test_refresh_reuse_that_does_not_revoke_the_grant_fails(self):
        checks, _, _ = run_flow(FakePlatform(broken=('refresh-reuse',)))
        self.assertEqual([k for k, v in checks.items() if v != 'PASS'],
                         ['apps oauth: its reuse revoked the whole grant (the current refresh token is refused)'])

    def test_another_clients_code_redemption_fails_the_isolation_check(self):
        checks, _, _ = run_flow(FakePlatform(broken=('cross-client-code',)))
        self.assertEqual([k for k, v in checks.items() if v != 'PASS'], ["isolation apps: B's client cannot redeem A's code"])

    def test_a_readable_foreign_app_fails_isolation_and_the_journey_continues(self):
        checks, _, _ = run_flow(FakePlatform(broken=('apps-isolation',)))
        self.assertEqual(checks["isolation apps: B's console reads A's app -> 404"], 'FAIL')
        self.assertEqual(checks["isolation apps: B's console rotates A's client secret -> 404"], 'FAIL')
        self.assertEqual(checks['apps directory: anonymous list (200)'], 'PASS')
        self.assertEqual(checks['apps in-meeting: public app-context JWKS (one Ed25519 key, no private part)'], 'PASS')

    def test_an_account_without_a_recorded_user_id_fails_registration_only(self):
        api = FakePlatform(stages=BUILD[:2] + ('apps',), apps=('register',))
        out = io.StringIO()
        with patch.object(pa.app_acceptance, 'accounts', lambda ns, count: [{'email': 'pa@x'}, {'email': 'pb@x'}]), \
                patch.object(pa.app_acceptance, 'controlled_code', lambda key, url=None: '024680'), \
                patch.object(pa.Client, '_call', lambda self, *a: api(self, *a)), \
                patch.object(pa.time, 'sleep', lambda s: None), contextlib.redirect_stdout(out):
            checks = pa.run('77-2', BUILD[:2] + ('apps',), lambda k, v: print(json.dumps({k: v})), apps=('register',))
        failed = sorted(k for k, v in checks.items() if v != 'PASS')
        self.assertEqual(len(failed), 2, failed)
        self.assertTrue(all('no recorded CF Meet user id' in k for k in failed), failed)
        self.assertEqual(checks['users A: user created (201, ACTIVE)'], 'PASS')

    def test_pkce_and_basic_auth_follow_the_rfcs(self):
        verifier, challenge = pa.pkce()
        self.assertTrue(43 <= len(verifier) <= 128)
        self.assertEqual(challenge, base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode())
        header = pa.basic('id', 'cfcs_sbx_a:b')
        self.assertEqual(base64.b64decode(header['Authorization'][6:]).decode(), 'id:cfcs_sbx_a%3Ab')
        self.assertEqual(header['Content-Type'], pa.FORM)
        self.assertEqual(pa.query('https://x/cb?code=c1&state=s'), {'code': 'c1', 'state': 's'})


if __name__ == '__main__':
    unittest.main()
