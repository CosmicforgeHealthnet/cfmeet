"""CF Meet Apps ("Integrate") activations (ADR 0064 / 0065, docs/platform/integrate-internal-contracts.md).

Imported by coordinated_release.py (``cr``); the same shape as the Build activations
(platform_operations.py, ``po``). Every feature is default-off in its service; each operation
switches one phase on through ``cr.apply_activation`` (journaled; receivers of new grants are
recreated with a merged trust bundle that never drops an issuer, key or grant; any failed
post-check restores every recreated container, and ``rollback`` restores the latest entry). The
post-check of every phase runs the two-tenant platform acceptance (platform_acceptance.py) with
its ``apps`` stage for every Integrate phase already on (``features``), plus every Build phase.

* ``activate-oauth-apps`` (I1): Developer Console ``DEVELOPER_PLATFORM_APPS_ENABLED`` and the named
  app reviewers ``DEVELOPER_PLATFORM_APP_REVIEWERS`` (the owner's ``CFMEET_APP_REVIEWERS`` variable,
  ``github-<login>=<CF Meet user id>`` entries; refused while empty unless
  ``confirm=no-reviewers-yet``: apps can then be registered and tested as DRAFT, never reviewed);
  Platform Access ``PLATFORM_ACCESS_OAUTH_ENABLED`` with a pairwise-``sub`` key generated once on the
  VPS (``PLATFORM_ACCESS_OAUTH_PAIRWISE_KEY_FILE``, 0400, never printed); ``platform-oauth.manage``,
  ``platform-oauth.token`` and ``platform-identity.read`` (Developer Platform -> Platform Access).
* ``activate-oauth`` (I2): the anonymous OAuth endpoints (``DEVELOPER_PLATFORM_OAUTH_ENABLED``), the
  Meeting Service consent relay (``MEETING_OAUTH_AUTHORIZE_ENABLED``), the gateway's
  ``OAUTH_ACCESS_TOKEN`` mode (``API_GATEWAY_OAUTH_ENABLED``), ``platform-oauth.authorize``
  (Meeting Service -> Platform Access), ``meeting-oauth-userinfo.read`` (Developer Platform ->
  Meeting Service) and ``/v1/oauth`` at the api.cfmeet.com edge (gateway snippet; restored on any
  failed check). Post-check: metadata 200 JSON and an unauthenticated token request 401
  ``invalid_client``, both answered by the gateway.
* ``activate-delegated-apps`` (I3): OAuth access tokens act as the user on the Meeting Service's
  machine operations (``MEETING_DELEGATED_APPS_ENABLED``, needs activate-platform-meetings) and on
  Messaging's MANAGED conversations (``MESSAGING_APPS_ENABLED``).
* ``activate-app-installs`` (I4): the Organizations install policy
  (``ORGANIZATIONS_APP_INSTALLS_ENABLED``) and ``organization-apps.evaluate`` (Meeting Service ->
  Organizations); needs Organizations authoritative.
* ``activate-app-events`` (I5): install-event routing and the ``APP_GRANT`` projection
  (``DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED`` on the console and its worker). Messaging must
  already relay public events to the Developer Platform (``MESSAGING_PUBLIC_EVENTS_DESTINATION``
  ``developer-platform`` / ``both``); this operation never changes that setting.
* ``activate-app-directory`` (I6): the anonymous directory
  (``DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED``) and ``/v1/app-directory`` at the edge. Post-check:
  an anonymous list 200 from the gateway.
* ``activate-in-meeting-apps`` (I7): ``MEETING_IN_MEETING_APPS_ENABLED`` with an Ed25519 app-context
  signing key generated once on the VPS (``MEETING_APP_CONTEXT_SIGNING_KEY_FILE``, PKCS#8 PEM, 0400,
  owned like the Meeting Service's own signing key, never printed). Post-check:
  ``GET /api/v2/app-context-keys`` is a JWKS with exactly one public Ed25519 key.

Rollback: containers (and with them the previous trust bundles and flags) come back through the
journal; the edge prefixes of an operation leave the gateway snippet first (``undo_edge``).
Generated keys are kept on the VPS (never deleted, never rotated over).
"""
import json
import re
import secrets
import urllib.error
import urllib.request

import platform_operations as po

OAUTH_APPS = 'activate-oauth-apps'
OAUTH = 'activate-oauth'
DELEGATED = 'activate-delegated-apps'
INSTALLS = 'activate-app-installs'
EVENTS = 'activate-app-events'
DIRECTORY = 'activate-app-directory'
IN_MEETING = 'activate-in-meeting-apps'

OPERATIONS = (OAUTH_APPS, OAUTH, DELEGATED, INSTALLS, EVENTS, DIRECTORY, IN_MEETING)

# The acceptance feature each phase proves (platform_acceptance.APP_FEATURES, same order).
FEATURES = {OAUTH_APPS: 'register', OAUTH: 'oauth', DELEGATED: 'delegated', INSTALLS: 'installs',
            EVENTS: 'events', DIRECTORY: 'directory', IN_MEETING: 'in-meeting'}

# Journal prerequisites (coordinated_release.run checks them before anything changes). A tuple
# inside lists alternatives. The acceptance harness needs the Build users phase (its tenants'
# platform credentials), so app registration follows it.
REQUIRES = {
    OAUTH_APPS: ('activate-developer-console', po.USERS),
    OAUTH: (OAUTH_APPS, 'activate-gateway-routing'),
    DELEGATED: (OAUTH, po.MEETINGS),
    INSTALLS: (OAUTH, ('org-cutover-authoritative', 'org-cutover-first-activation')),
    EVENTS: (OAUTH_APPS, 'activate-developer-webhooks'),
    DIRECTORY: (OAUTH_APPS,),
    IN_MEETING: (OAUTH,),
}

# (service, flag, operation that switches it on). Every flag is refused as anything but off
# (unset / false) until its operation is recorded (coordinated_release.safe_defaults).
INTEGRATE_FLAGS = (
    ('developer-platform', 'DEVELOPER_PLATFORM_APPS_ENABLED', OAUTH_APPS),
    ('platform-access', 'PLATFORM_ACCESS_OAUTH_ENABLED', OAUTH_APPS),
    ('developer-platform', 'DEVELOPER_PLATFORM_OAUTH_ENABLED', OAUTH),
    ('meeting-service', 'MEETING_OAUTH_AUTHORIZE_ENABLED', OAUTH),
    ('api-gateway', 'API_GATEWAY_OAUTH_ENABLED', OAUTH),
    ('meeting-service', 'MEETING_DELEGATED_APPS_ENABLED', DELEGATED),
    ('messaging', 'MESSAGING_APPS_ENABLED', DELEGATED),
    ('organizations', 'ORGANIZATIONS_APP_INSTALLS_ENABLED', INSTALLS),
    ('developer-platform', 'DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED', EVENTS),
    ('developer-platform-worker', 'DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED', EVENTS),
    ('developer-platform', 'DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED', DIRECTORY),
    ('meeting-service', 'MEETING_IN_MEETING_APPS_ENABLED', IN_MEETING),
)

# App reviewers: the owner's list, written by the workflow (like the production-access reviewers).
REVIEWERS_FILE = 'app-reviewers'
REVIEWER = re.compile(r'^github-[a-z0-9](?:[a-z0-9-]{0,38})=[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
NO_REVIEWERS_CONFIRM = 'no-reviewers-yet'
REVIEWERS_VAR = 'DEVELOPER_PLATFORM_APP_REVIEWERS'

PAIRWISE_KEY = 'platform-access-oauth-pairwise.key'
PAIRWISE_KEY_TARGET = '/run/secrets/platform-oauth-pairwise.key'
PAIRWISE_KEY_VAR = 'PLATFORM_ACCESS_OAUTH_PAIRWISE_KEY_FILE'
# Platform Access refuses short, low-variety and placeholder-looking keys (CredentialPepper rules).
PAIRWISE_MIN_LEN = 32
PLACEHOLDER_FRAGMENTS = ('changeme', 'change-me', 'change_me', 'change-this', 'changethis', 'secret123',
                         'development-secret', 'dev-secret', 'your-', 'your_', 'placeholder', 'example',
                         'replace-me', 'replace_me', 'password', 'default')

CONTEXT_KEY = 'meeting-app-context-signing.pem'
CONTEXT_KEY_TARGET = '/run/secrets/meeting-app-context-signing.pem'
CONTEXT_KEY_VAR = 'MEETING_APP_CONTEXT_SIGNING_KEY_FILE'

FLAGS = tuple(dict.fromkeys([flag for _, flag, _ in INTEGRATE_FLAGS] + [REVIEWERS_VAR, PAIRWISE_KEY_VAR,
                                                                          CONTEXT_KEY_VAR]))

# Gateway edge prefixes owned by an operation (gateway snippet on api.cfmeet.com). Every route
# under them is ANONYMOUS at the gateway (the client secret or the app token is the credential).
OAUTH_EDGE = ('/v1/oauth',)
DIRECTORY_EDGE = ('/v1/app-directory',)
EDGE = {OAUTH: OAUTH_EDGE, DIRECTORY: DIRECTORY_EDGE}

# Public events must reach the Developer Platform before installs can receive them.
EVENT_DESTINATIONS = ('developer-platform', 'both')
CONTEXT_KEYS_PATH = '/api/v2/app-context-keys'
OAUTH_METADATA_PATH = '/v1/oauth/metadata'
OAUTH_TOKEN_PATH = '/v1/oauth/token'
DIRECTORY_PATH = '/v1/app-directory'

GRANTS = (
    (OAUTH_APPS, 'platform-access', 'developer-platform',
     ('platform-oauth.manage', 'platform-oauth.token', 'platform-identity.read')),
    (OAUTH, 'platform-access', 'meeting-service', ('platform-oauth.authorize',)),
    (OAUTH, 'meeting-service', 'developer-platform', ('meeting-oauth-userinfo.read',)),
    (INSTALLS, 'organizations', 'meeting-service', ('organization-apps.evaluate',)),
)


def flags_for(operation):
    """{service: {flag: 'true'}} of one operation, in INTEGRATE_FLAGS order."""
    out = {}
    for service, flag, op in INTEGRATE_FLAGS:
        if op == operation:
            out.setdefault(service, {})[flag] = 'true'
    return out


def safe_default_rules(done):
    """{service: {flag: allowed values}}: off until the owning operation is recorded."""
    rules = {}
    for service, flag, operation in INTEGRATE_FLAGS:
        rules.setdefault(service, {})[flag] = (None, 'false', 'true') if done(operation) else (None, 'false')
    return rules


def edge_prefixes(done):
    """The edge prefixes recorded Integrate operations route to the gateway, in a fixed order."""
    return tuple(p for op in (OAUTH, DIRECTORY) if done(op) for p in EDGE[op])


def features(cr, current=None):
    """Acceptance features of the recorded Integrate phases plus `current`; () until app
    registration is on (every other feature builds on it)."""
    on = tuple(FEATURES[op] for op in OPERATIONS if op == current or cr.done(op))
    return on if FEATURES[OAUTH_APPS] in on else ()


def undo_edge(cr, entry):
    """Rollback: only the operation's own prefixes leave the edge (before its containers)."""
    remove = EDGE.get(entry['operation'], ())
    if not remove or not cr.GATEWAY_SNIPPET.exists():
        return
    keep = tuple(p for p in cr.edge_prefixes() if p not in remove)
    cr.GATEWAY_SNIPPET.write_text(cr.gateway_snippet(keep, docs=cr.done('activate-gateway-docs')))
    cr.p.reload_host_nginx()
    cr.emit('platformEdgeRemoved', {'operation': entry['operation'], 'prefixes': list(remove)})


def http_json(cr, method, path, body=None, headers=None):
    """(status, lower-case headers, JSON body or {}) of a public api.cfmeet.com request. `body`
    is sent as given (bytes), so a form request is exactly what an OAuth client sends."""
    request = urllib.request.Request(cr.PUBLIC + path, data=body, method=method, headers=dict(headers or {}))
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            status, raw, got = response.status, response.read(1 << 20), dict(response.headers)
    except urllib.error.HTTPError as error:
        status, raw, got = error.code, error.read(1 << 20), dict(error.headers)
    try:
        value = json.loads(raw) if raw else {}
    except ValueError:
        value = {}
    return status, {k.lower(): v for k, v in got.items()}, value if isinstance(value, dict) else {}


def with_edge(cr, prefixes, checks, operation, run_id, label):
    """The post-check of an edge operation: route `prefixes` to the gateway, run `checks()`
    and the acceptance; any failure restores the previous snippet."""
    old = po.write_edge(cr, prefixes)
    try:
        cr.p.reload_host_nginx()  # nginx -t first
        results = checks()
        results.update(cr.public_smoke())
        cr.emit(label, results)
        if any(v != 'PASS' for v in results.values()):
            raise RuntimeError('The %s edge checks failed' % ', '.join(prefixes))
        po.run_acceptance(cr, run_id, operation)
    except Exception:
        cr.GATEWAY_SNIPPET.write_text(old)
        cr.p.reload_host_nginx()
        raise


# --- I1: app registration, reviewers and the OAuth token authority --------------------------------

def parse_reviewers(text):
    """`github-<login>=<CF Meet user id>` entries, comma or newline separated (none: {})."""
    entries = [e.strip().lower() for e in re.split(r'[,\n]', text or '') if e.strip()]
    if not all(REVIEWER.fullmatch(e) for e in entries):
        raise RuntimeError('CFMEET_APP_REVIEWERS must list github-<login>=<CF Meet user id> entries')
    return dict(e.split('=', 1) for e in entries)


def app_reviewers(cr):
    path = cr.WORK / REVIEWERS_FILE
    return parse_reviewers(path.read_text()) if path.exists() else {}


def pairwise_key_material():
    """64 url-safe characters from 48 random bytes, never placeholder-looking."""
    while True:
        value = secrets.token_urlsafe(48)
        if len(set(value)) >= 12 and not any(f in value.lower() for f in PLACEHOLDER_FRAGMENTS):
            return value + '\n'


def pairwise_key(cr):
    """Generated once (never over an existing key: a new key changes every app's `sub`), owned
    by the Platform Access user, 0400. Returns (path, generated)."""
    path = cr.p.SECRETS / PAIRWISE_KEY
    generated = not path.exists() or not path.read_text().strip()
    text = cr.secret_file(path, pairwise_key_material, uid=cr.SERVICES['platform-access']['uid'], mode=0o400)
    if len(text) < PAIRWISE_MIN_LEN or len(set(text)) < 12:
        raise RuntimeError('The OAuth pairwise key file is too short or too uniform')
    return path, generated


def activate_oauth_apps(cr, sha, run_id, confirm):
    confirms = [c for c in (confirm or '').split(',') if c]
    if set(confirms) - {NO_REVIEWERS_CONFIRM}:
        raise RuntimeError('activate-oauth-apps takes only confirm=' + NO_REVIEWERS_CONFIRM)
    dp = cr.env_of('developer-platform')
    if dp.get('DEVELOPER_PLATFORM_GATEWAY_API_ENABLED') != 'true':
        raise RuntimeError('Run activate-developer-platform first')
    cr.tls_route_present(cr.SERVICES['platform-access']['tls'])
    cr.has_signing_identity('developer-platform')
    names = app_reviewers(cr)
    if not names and NO_REVIEWERS_CONFIRM not in confirms:
        raise RuntimeError('No app reviewers are named (set the CFMEET_APP_REVIEWERS variable of the production '
                           'environment), or confirm=%s to register apps that nobody can review yet'
                           % NO_REVIEWERS_CONFIRM)
    key, generated = pairwise_key(cr)
    cr.emit('oauthPairwiseKey', {'path': PAIRWISE_KEY_TARGET, 'generated': generated})
    cr.emit('appReview', {'reviewers': sorted(names), 'reviewDecisionsPossible': bool(names)})
    flags = flags_for(OAUTH_APPS)
    access = dict(flags['platform-access'], **{PAIRWISE_KEY_VAR: PAIRWISE_KEY_TARGET})
    console = dict(flags['developer-platform'],
                   **po.base_urls(cr, 'developer-platform', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))
    if names:
        console[REVIEWERS_VAR] = ','.join(sorted(names))
    cr.apply_activation(OAUTH_APPS, sha, run_id, [
        ('platform-access', access, {'mounts': [(key, PAIRWISE_KEY_TARGET)]}),
        ('developer-platform', console)], post=lambda: po.run_acceptance(cr, run_id, OAUTH_APPS))


# --- I2: OAuth endpoints, consent relay and the gateway's OAuth mode ------------------------------

def oauth_edge_checks(cr):
    """Metadata 200 JSON (S256 only, our token endpoint) and an unauthenticated token request
    401 invalid_client, both answered by the gateway (its x-correlation-id stamp)."""
    checks = {}
    status, headers, doc = http_json(cr, 'GET', OAUTH_METADATA_PATH, headers={'Accept': 'application/json'})
    ok = status == 200 and 'x-correlation-id' in headers and 'json' in headers.get('content-type', '') \
        and doc.get('token_endpoint') == cr.PUBLIC + OAUTH_TOKEN_PATH \
        and doc.get('code_challenge_methods_supported') == ['S256']
    checks['GET %s 200 JSON from the gateway' % OAUTH_METADATA_PATH] = 'PASS' if ok else 'FAIL %d' % status
    status, headers, doc = http_json(cr, 'POST', OAUTH_TOKEN_PATH, b'grant_type=authorization_code',
                                     {'Content-Type': 'application/x-www-form-urlencoded', 'Accept': 'application/json'})
    ok = status == 401 and doc.get('error') == 'invalid_client' and 'x-correlation-id' in headers
    checks['POST %s without client authentication -> 401 invalid_client' % OAUTH_TOKEN_PATH] = \
        'PASS' if ok else 'FAIL %d %s' % (status, doc.get('error') or '')
    return checks


def activate_oauth(cr, sha, run_id, confirm):
    po.require_gateway_edge(cr)
    meeting = cr.env_of('meeting-service')
    if not meeting.get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('Meeting Service has no receiver trust bundle (the userinfo route needs one)')
    if not meeting.get('CFMEET_COMMUNICATION_ENVIRONMENT_ID'):
        raise RuntimeError('Personal app installs need the Meeting Service\'s first-party communications scope '
                           '(CFMEET_COMMUNICATION_*)')
    dp = cr.env_of('developer-platform')
    if not dp.get('CF_MEETING_SERVICE_BASE_URL'):
        raise RuntimeError('The Developer Platform has no Meeting Service target (activate-developer-console)')
    gateway = cr.env_of('api-gateway')
    if not gateway.get('CF_PLATFORM_ACCESS_BASE_URL'):
        raise RuntimeError('The gateway has no Platform Access target (activate-developer-platform)')
    for name in ('meeting-service', 'developer-platform', 'api-gateway'):
        cr.has_signing_identity(name)
    cr.tls_route_present(cr.SERVICES['meeting-service']['tls'])
    po.refuse_collisions(cr, OAUTH_EDGE)
    flags = flags_for(OAUTH)
    meeting_overrides = dict(flags['meeting-service'], **po.base_urls(
        cr, 'meeting-service', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))
    cr.apply_activation(OAUTH, sha, run_id, [
        ('meeting-service', meeting_overrides), ('developer-platform', flags['developer-platform']),
        ('api-gateway', flags['api-gateway'])],
        post=lambda: with_edge(cr, OAUTH_EDGE, lambda: oauth_edge_checks(cr), OAUTH, run_id, 'oauthEdge'))


# --- I3: delegated access ------------------------------------------------------------------------

def activate_delegated(cr, sha, run_id, confirm):
    meeting = cr.env_of('meeting-service')
    if meeting.get('MEETING_PLATFORM_ACCESS_ENABLED') != 'true':
        raise RuntimeError('Delegated apps build on MEETING_PLATFORM_ACCESS_ENABLED (activate-platform-meetings)')
    messaging = cr.env_of('messaging')
    if not messaging.get('CF_PLATFORM_ACCESS_BASE_URL'):
        raise RuntimeError('Messaging has no Platform Access target (activate-partner-chat)')
    if 'meetings' in (cr.env_of('api-gateway').get('API_GATEWAY_DISABLED_DOMAINS') or '').split(','):
        raise RuntimeError('The meetings domain is disabled on the gateway')
    flags = flags_for(DELEGATED)
    cr.apply_activation(DELEGATED, sha, run_id, [
        ('meeting-service', flags['meeting-service']), ('messaging', flags['messaging'])],
        post=lambda: po.run_acceptance(cr, run_id, DELEGATED))


# --- I4: workspace install policy ----------------------------------------------------------------

def activate_installs(cr, sha, run_id, confirm):
    if not cr.env_of('meeting-service').get('CF_ORGANIZATIONS_BASE_URL'):
        raise RuntimeError('The Meeting Service has no Organizations target (organization cutover)')
    if not cr.env_of('organizations').get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('Organizations has no receiver trust bundle')
    cr.tls_route_present(cr.SERVICES['organizations']['tls'])
    cr.has_signing_identity('meeting-service')
    cr.apply_activation(INSTALLS, sha, run_id, [('organizations', flags_for(INSTALLS)['organizations'])],
                        post=lambda: po.run_acceptance(cr, run_id, INSTALLS))


# --- I5: install events --------------------------------------------------------------------------

def activate_events(cr, sha, run_id, confirm):
    destination = (cr.env_of('messaging').get('MESSAGING_PUBLIC_EVENTS_DESTINATION') or 'calling').strip()
    if destination not in EVENT_DESTINATIONS:
        raise RuntimeError('Messaging relays public events to %r, not the Developer Platform: switch '
                           'MESSAGING_PUBLIC_EVENTS_DESTINATION through its own activation first (never changed here)'
                           % destination)
    cr.tls_route_present(cr.SERVICES['platform-access']['tls'])
    flags = flags_for(EVENTS)
    target = (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)
    cr.apply_activation(EVENTS, sha, run_id, [
        ('developer-platform', dict(flags['developer-platform'], **po.base_urls(cr, 'developer-platform', target))),
        # The worker projects APP_GRANT changes from Platform Access (it needs the target too).
        ('developer-platform-worker', dict(flags['developer-platform-worker'],
                                           **po.base_urls(cr, 'developer-platform-worker', target)))],
        post=lambda: po.run_acceptance(cr, run_id, EVENTS))


# --- I6: app directory ---------------------------------------------------------------------------

def directory_edge_checks(cr):
    """An anonymous list 200 JSON from the gateway; an unknown app 404."""
    checks = {}
    status, headers, doc = http_json(cr, 'GET', DIRECTORY_PATH, headers={'Accept': 'application/json'})
    ok = status == 200 and 'x-correlation-id' in headers and isinstance(doc.get('items'), list)
    checks['anonymous GET %s 200 from the gateway' % DIRECTORY_PATH] = 'PASS' if ok else 'FAIL %d' % status
    status, headers, _ = http_json(cr, 'GET', DIRECTORY_PATH + '/00000000-0000-4000-8000-000000000000',
                                   headers={'Accept': 'application/json'})
    checks['anonymous GET %s/<unknown> 404' % DIRECTORY_PATH] = \
        'PASS' if status == 404 and 'x-correlation-id' in headers else 'FAIL %d' % status
    return checks


def activate_directory(cr, sha, run_id, confirm):
    po.require_gateway_edge(cr)
    po.refuse_collisions(cr, DIRECTORY_EDGE)
    cr.apply_activation(DIRECTORY, sha, run_id, [('developer-platform', flags_for(DIRECTORY)['developer-platform'])],
                        post=lambda: with_edge(cr, DIRECTORY_EDGE, lambda: directory_edge_checks(cr), DIRECTORY,
                                               run_id, 'appDirectoryEdge'))


# --- I7: in-meeting apps -------------------------------------------------------------------------

def meeting_key_uid(cr):
    """The owner of the Meeting Service's own signing key file (the uid its process reads as)."""
    c = cr.p.inspect(cr.SERVICES['meeting-service']['container'])
    source = cr.mount_source(c, cr.p.env_of(c)['CF_SERVICE_AUTH_PRIVATE_KEY_FILE'])
    return cr.os.stat(source).st_uid


def context_signing_key(cr):
    """Generated once (never over an existing key: apps cache the JWKS), 0400. (path, generated)."""
    path = cr.p.SECRETS / CONTEXT_KEY
    generated = not path.exists() or not path.read_text().strip()
    text = cr.secret_file(path, lambda: po.ed25519_private_key_pem(cr), uid=meeting_key_uid(cr), mode=0o400)
    if not text.startswith('-----BEGIN PRIVATE KEY-----'):
        raise RuntimeError('The app-context signing key file is not a PKCS#8 PEM key')
    return path, generated


def context_keys_checks(cr):
    """The public JWKS: exactly one Ed25519 public key (never a private part)."""
    status, _, doc = http_json(cr, 'GET', CONTEXT_KEYS_PATH, headers={'Accept': 'application/json'})
    keys = doc.get('keys') if isinstance(doc.get('keys'), list) else []
    ok = status == 200 and len(keys) == 1 and isinstance(keys[0], dict) and keys[0].get('kty') == 'OKP' \
        and keys[0].get('crv') == 'Ed25519' and bool(keys[0].get('x')) and 'd' not in keys[0]
    return {'GET %s: one public Ed25519 key' % CONTEXT_KEYS_PATH: 'PASS' if ok else 'FAIL %d (%d keys)' % (status, len(keys))}


def activate_in_meeting(cr, sha, run_id, confirm):
    meeting = cr.env_of('meeting-service')
    if meeting.get('MEETING_OAUTH_AUTHORIZE_ENABLED') != 'true':
        raise RuntimeError('In-meeting apps build on MEETING_OAUTH_AUTHORIZE_ENABLED (activate-oauth)')
    cr.has_signing_identity('meeting-service')
    key, generated = context_signing_key(cr)
    cr.emit('appContextSigningKey', {'algorithm': 'Ed25519', 'format': 'PKCS#8 PEM', 'path': CONTEXT_KEY_TARGET,
                                     'generated': generated})
    overrides = dict(flags_for(IN_MEETING)['meeting-service'], **{CONTEXT_KEY_VAR: CONTEXT_KEY_TARGET})

    def post():
        checks = context_keys_checks(cr)
        cr.emit('appContextKeys', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('The app-context key set check failed')
        po.run_acceptance(cr, run_id, IN_MEETING)

    cr.apply_activation(IN_MEETING, sha, run_id, [
        ('meeting-service', overrides, {'mounts': [(key, CONTEXT_KEY_TARGET)]})], post=post)


ACTIVATIONS = {
    OAUTH_APPS: activate_oauth_apps,
    OAUTH: activate_oauth,
    DELEGATED: activate_delegated,
    INSTALLS: activate_installs,
    EVENTS: activate_events,
    DIRECTORY: activate_directory,
    IN_MEETING: activate_in_meeting,
}
