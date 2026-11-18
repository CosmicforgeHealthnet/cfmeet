"""CF Meet Platform "Build" activations (ADR 0062 / 0063, docs/platform/build-internal-contracts.md).

Imported by coordinated_release.py (``cr``). Every feature is default-off in its service; each
operation switches one phase on through ``cr.apply_activation`` (journaled; receivers of new
grants are recreated with a merged trust bundle that never drops an issuer, key or grant; any
failed post-check restores every recreated container, and ``rollback`` restores the latest
entry). The post-check of every phase runs the two-tenant platform acceptance
(platform_acceptance.py) for that phase and every phase already on: two fresh developer
workspaces A and B, the phase's milestone and the cross-tenant refusals, always cleaned up.

* ``activate-platform-users`` (B1): Platform Access ``PLATFORM_ACCESS_END_USERS_ENABLED`` with
  the per-environment caps written explicitly, Developer Console ``DEVELOPER_PLATFORM_USERS_API_ENABLED``,
  ``platform-end-users.delegate`` (Developer Platform -> Platform Access) and the ``/v1/users``
  family at the api.cfmeet.com edge (gateway snippet; restored on any failed check).
* ``activate-platform-user-tokens`` (B2): an Ed25519 signing key generated once on the VPS
  (PKCS#8 PEM, owned by the Platform Access user, 0400, mounted read-only, never printed),
  ``PLATFORM_ACCESS_USER_TOKENS_ENABLED`` / ``PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED``, the
  console's allowed websites and SANDBOX token generator, the gateway's user-token pre-check and
  partner-website CORS; ``platform-user-token.verify`` and ``platform-environment-origins.read``
  for Messaging, Calling, the Meeting Service and the API Gateway.
* ``activate-partner-chat`` (B3): Messaging partner session devices (needs
  ``MESSAGING_SESSION_DEVICES_ENABLED``) and, while the usage export runs, the ``MESSAGE_SENT`` fact.
* ``activate-partner-calls`` (B4): Calling's user-token exchange. Calls stay on their direct
  routes: moving ``/v1/calls`` and ``/v1/calling`` behind the gateway is the separate, owner-approved
  ``activate-gateway-calling-edge`` (confirm=owner-approved-calling-edge), a journaled snippet
  change restored automatically on a failed check. The ``/v1/calling/signaling`` WebSocket is
  never moved (the gateway snippet carries no upgrade handling and gateway WebSockets are off).
* ``activate-partner-push`` (B5): partner push on the Notification Service (both replicas), the
  console's sealed push-credential upload (``notifications-push-provider.manage``), Messaging and
  Calling partner recipients; needs the first-party push and call-push activations.
* ``activate-platform-meetings`` (B6): Meeting Service platform access, user-token join and
  partner meeting chat; Platform Access ``ensure-meeting-platform``; Messaging platform meeting
  chat sync; the gateway's Meeting Service upstream. The machine meeting family (``/api/v2/...``)
  is NOT routed at the edge: the edge machinery routes path prefixes on the one api.cfmeet.com
  server block and cannot separate partner machine traffic from the first-party ``/api/v2``
  traffic on the same paths (owner edge-topology decision). The acceptance reaches the gateway
  for those calls through its loopback relay.

Rollback: containers (and with them the previous trust bundles and flags) come back through the
journal; the edge prefixes of an operation leave the gateway snippet first (``undo_edge``).
Generated keys are kept on the VPS (never deleted, never rotated over).
"""

USERS ='activate-platform-users'
TOKENS = 'activate-platform-user-tokens'
CHAT = 'activate-partner-chat'
CALLS = 'activate-partner-calls'
CALLING_EDGE = 'activate-gateway-calling-edge'
PUSH = 'activate-partner-push'
MEETINGS = 'activate-platform-meetings'
ACCEPTANCE = 'platform-acceptance'

# Phase order and the acceptance stage each phase proves.
STAGES = {USERS: 'users', TOKENS: 'tokens', CHAT: 'chat', CALLS: 'calls', PUSH: 'push', MEETINGS: 'meetings'}
STAGE_ORDER = (USERS, TOKENS, CHAT, CALLS, PUSH, MEETINGS)

# Journal prerequisites (coordinated_release.run checks them before anything changes).
REQUIRES = {
    USERS: ('activate-developer-console', 'activate-gateway-families'),
    TOKENS: (USERS,),
    CHAT: (TOKENS,),
    CALLS: (TOKENS,),
    CALLING_EDGE: (CALLS, 'activate-gateway-families'),
    PUSH: (CHAT, CALLS, 'activate-notifications-push', 'activate-calling-push'),
    MEETINGS: (TOKENS, CHAT),
}
OPERATIONS = (USERS, TOKENS, CHAT, CALLS, CALLING_EDGE, PUSH, MEETINGS)

# (service, flag, operation that switches it on). Every flag is refused as anything but off
# (unset / false) until its operation is recorded (coordinated_release.safe_defaults).
PLATFORM_FLAGS = (
    ('platform-access', 'PLATFORM_ACCESS_END_USERS_ENABLED', USERS),
    ('developer-platform', 'DEVELOPER_PLATFORM_USERS_API_ENABLED', USERS),
    ('platform-access', 'PLATFORM_ACCESS_USER_TOKENS_ENABLED', TOKENS),
    ('platform-access', 'PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED', TOKENS),
    ('developer-platform', 'DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED', TOKENS),
    ('developer-platform', 'DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED', TOKENS),
    ('api-gateway', 'API_GATEWAY_USER_TOKENS_ENABLED', TOKENS),
    ('api-gateway', 'API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED', TOKENS),
    ('messaging', 'MESSAGING_PARTNER_SESSION_DEVICES_ENABLED', CHAT),
    ('messaging', 'MESSAGING_MESSAGE_SENT_EXPORT_ENABLED', CHAT),
    ('calling', 'CALLING_USER_TOKEN_EXCHANGE_ENABLED', CALLS),
    ('notifications', 'NOTIFICATIONS_PARTNER_PUSH_ENABLED', PUSH),
    ('notifications-2', 'NOTIFICATIONS_PARTNER_PUSH_ENABLED', PUSH),
    ('developer-platform', 'DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED', PUSH),
    ('messaging', 'MESSAGING_PARTNER_PUSH_ENABLED', PUSH),
    ('calling', 'CALLING_PARTNER_PUSH_ENABLED', PUSH),
    ('platform-access', 'PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED', MEETINGS),
    ('meeting-service', 'MEETING_PLATFORM_ACCESS_ENABLED', MEETINGS),
    ('meeting-service', 'MEETING_PLATFORM_USER_JOIN_ENABLED', MEETINGS),
    ('meeting-service', 'MEETING_PLATFORM_CHAT_ENABLED', MEETINGS),
)

# Contract §2.1 caps, written explicitly so `inspect` shows them.
END_USER_CAPS = {'PLATFORM_ACCESS_MAX_END_USERS_SANDBOX': '100', 'PLATFORM_ACCESS_MAX_END_USERS_TEST': '1000',
                 'PLATFORM_ACCESS_MAX_END_USERS_PRODUCTION': '100000'}

USER_TOKEN_KEY = 'platform-access-user-token-signing.pem'
USER_TOKEN_KEY_TARGET = '/run/secrets/platform-user-token-signing.pem'
USER_TOKEN_KEY_VAR = 'PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY_FILE'

FLAGS = tuple(dict.fromkeys([flag for _, flag, _ in PLATFORM_FLAGS] + sorted(END_USER_CAPS) + [USER_TOKEN_KEY_VAR]))

# Gateway edge prefixes owned by an operation (gateway snippet on api.cfmeet.com).
USERS_EDGE = ('/v1/users',)
# Every REST route of /v1/calls and /v1/calling, never the /v1/calling/signaling WebSocket
# (an exact `location = /v1/calling/signaling-credentials` does not match /v1/calling/signaling).
CALLING_EDGE_PREFIXES = ('/v1/calls', '/v1/calling/endpoints', '/v1/calling/history', '/v1/calling/invitations',
                         '/v1/calling/limits', '/v1/calling/sessions', '/v1/calling/signaling-credentials',
                         '/v1/calling/usage', '/v1/calling/user-sessions')
CALLING_WEBSOCKETS = ('/v1/calling/signaling',)
EDGE = {USERS: USERS_EDGE, CALLING_EDGE: CALLING_EDGE_PREFIXES}
CALLING_EDGE_CONFIRM = 'owner-approved-calling-edge'
# Anonymous probes of the moved calling routes (one real REST route per prefix).
CALLING_EDGE_PROBES = (('GET', '/v1/calls'), ('GET', '/v1/calling/endpoints'), ('GET', '/v1/calling/history'),
                       ('GET', '/v1/calling/invitations'), ('GET', '/v1/calling/limits'), ('GET', '/v1/calling/usage'),
                       ('POST', '/v1/calling/signaling-credentials'), ('POST', '/v1/calling/user-sessions'))

# Partner push providers the Notification Service must reach (APNs incl. sandbox, FCM + its OAuth).
PUSH_PROVIDER_URLS = ('https://api.push.apple.com/', 'https://api.sandbox.push.apple.com/',
                      'https://fcm.googleapis.com/', 'https://oauth2.googleapis.com/')

GRANTS = (
    (USERS, 'platform-access', 'developer-platform', ('platform-end-users.delegate',)),
) + tuple((TOKENS, 'platform-access', issuer, ('platform-user-token.verify', 'platform-environment-origins.read'))
          for issuer in ('messaging', 'calling', 'meeting-service', 'api-gateway')) \
  + tuple((PUSH, replica, 'developer-platform', ('notifications-push-provider.manage',))
          for replica in ('notifications', 'notifications-2')) \
  + tuple((PUSH, replica, 'messaging', ('notifications-push-registration.sync',))  # platform-principal subjects
          for replica in ('notifications', 'notifications-2')) \
  + (
    # Partner meeting chat principals; MEETING_PLATFORM_ACCESS_ENABLED re-verifies forwarded
    # platform credentials live with Platform Access.
    (MEETINGS, 'platform-access', 'meeting-service',
     ('platform-principal.ensure-meeting-platform', 'platform-credential.authenticate')),
    (MEETINGS, 'messaging', 'meeting-service', ('messaging-meeting.sync-platform',)),
    (MEETINGS, 'meeting-service', 'api-gateway', ('meeting-request.forward',)),
)


def flags_for(operation):
    """{service: {flag: 'true'}} of one operation, in PLATFORM_FLAGS order."""
    out = {}
    for service, flag, op in PLATFORM_FLAGS:
        if op == operation:
            out.setdefault(service, {})[flag] = 'true'
    return out


def safe_default_rules(done):
    """{service: {flag: allowed values}}: off until the owning operation is recorded."""
    rules = {}
    for service, flag, operation in PLATFORM_FLAGS:
        rules.setdefault(service, {})[flag] = (None, 'false', 'true') if done(operation) else (None, 'false')
    return rules


def edge_prefixes(done):
    """The edge prefixes recorded platform operations route to the gateway, in a fixed order."""
    return tuple(p for op in (USERS, CALLING_EDGE) if done(op) for p in EDGE[op])


def enabled_stages(cr, current=None):
    """Acceptance stages of the recorded phases plus `current` (the one being activated)."""
    return tuple(STAGES[op] for op in STAGE_ORDER if op == current or cr.done(op))


# --- shared helpers ----------------------------------------------------------------------------

def base_urls(cr, name, targets):
    """Private TLS base URLs a service still lacks ({var: url}); existing values are kept."""
    env = cr.env_of(name)
    out = {}
    for var, target in targets:
        if not env.get(var):
            cr.tls_route_present(cr.SERVICES[target]['tls'])
            out[var] = cr.private_url(cr.SERVICES[target]['tls'])
    return out


def run_acceptance(cr, run_id, operation):
    """The two-tenant acceptance of every phase on (including `operation`); any FAIL raises,
    which restores the activation."""
    import platform_acceptance
    stages = enabled_stages(cr, operation)
    checks = platform_acceptance.run(run_id, stages, cr.emit)
    failed = sorted(k for k, v in checks.items() if v != 'PASS')
    if failed:
        raise RuntimeError('Platform acceptance failed after %s (%d of %d checks): %s'
                           % (operation, len(failed), len(checks), '; '.join(failed[:5])))
    return checks


def served_by_gateway(cr, method, path):
    """(status, answered by the gateway) of an anonymous public request. The gateway always
    stamps x-correlation-id; the direct service routes do not."""
    status, headers = cr.http_status(cr.PUBLIC + path, method,
                                     {'Content-Type': 'application/json'} if method == 'POST' else None)
    return status, 'x-correlation-id' in {k.lower() for k in headers}


def refuse_collisions(cr, prefixes):
    site = cr.SITE.read_text()
    snippets = [f.read_text() for f in cr.Path('/etc/nginx/snippets').glob('cfmeet-*.conf') if f != cr.GATEWAY_SNIPPET]
    collisions = cr.route_collisions([site] + snippets, prefixes)
    if collisions:
        raise RuntimeError('Existing public routes already serve: ' + ', '.join(collisions))


def write_edge(cr, prefixes):
    """Rewrites the gateway snippet with the current edge plus `prefixes`; returns the old text."""
    old = cr.GATEWAY_SNIPPET.read_text()
    edge = tuple(dict.fromkeys(cr.edge_prefixes() + tuple(prefixes)))
    cr.GATEWAY_SNIPPET.write_text(cr.gateway_snippet(edge, docs=cr.done('activate-gateway-docs')))
    return old


def undo_edge(cr, entry):
    """Rollback: only the operation's own prefixes leave the edge (before its containers)."""
    remove = EDGE.get(entry['operation'], ())
    if not remove or not cr.GATEWAY_SNIPPET.exists():
        return
    keep = tuple(p for p in cr.edge_prefixes() if p not in remove)
    cr.GATEWAY_SNIPPET.write_text(cr.gateway_snippet(keep, docs=cr.done('activate-gateway-docs')))
    cr.p.reload_host_nginx()
    cr.emit('platformEdgeRemoved', {'operation': entry['operation'], 'prefixes': list(remove)})


def require_gateway_edge(cr):
    if cr.env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true' or not cr.GATEWAY_SNIPPET.exists():
        raise RuntimeError('Run activate-gateway-routing first')
    if cr.GATEWAY_INCLUDE not in cr.SITE.read_text():
        raise RuntimeError('The gateway snippet is not included by the public site')


# --- B1: partner users -------------------------------------------------------------------------

def activate_users(cr, sha, run_id, confirm):
    require_gateway_edge(cr)
    dp = cr.env_of('developer-platform')
    if dp.get('DEVELOPER_PLATFORM_GATEWAY_API_ENABLED') != 'true':
        raise RuntimeError('Run activate-developer-platform first')
    cr.tls_route_present(cr.SERVICES['platform-access']['tls'])
    cr.has_signing_identity('developer-platform')
    refuse_collisions(cr, USERS_EDGE)
    flags = flags_for(USERS)
    access = dict(flags['platform-access'], **END_USER_CAPS)
    console = dict(flags['developer-platform'],
                   **base_urls(cr, 'developer-platform', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))

    def post():
        old = write_edge(cr, USERS_EDGE)
        try:
            cr.p.reload_host_nginx()  # nginx -t first
            status, gateway = served_by_gateway(cr, 'GET', '/v1/users')
            checks = {'anonymous GET /v1/users answered by the gateway':
                      'PASS' if status in (401, 403) and gateway else 'FAIL %d' % status}
            checks.update(cr.public_smoke())
            cr.emit('platformUsersEdge', checks)
            if any(v != 'PASS' for v in checks.values()):
                raise RuntimeError('The /v1/users edge checks failed')
            run_acceptance(cr, run_id, USERS)
        except Exception:
            cr.GATEWAY_SNIPPET.write_text(old)
            cr.p.reload_host_nginx()
            raise

    cr.apply_activation(USERS, sha, run_id, [('platform-access', access), ('developer-platform', console)], post=post)


# --- B2: user tokens and allowed websites --------------------------------------------------------

def ed25519_private_key_pem(cr):
    """A fresh PKCS#8 PEM Ed25519 key (what Platform Access reads from the key FILE)."""
    pem = cr.p.run('openssl', 'genpkey', '-algorithm', 'ED25519')
    if not pem.startswith('-----BEGIN PRIVATE KEY-----'):
        raise RuntimeError('openssl did not produce a PKCS#8 Ed25519 key')
    return pem + '\n'


def user_token_signing_key(cr):
    """Generated once (never over an existing key: rotation is a separate, retiring step),
    owned by the Platform Access user, 0400. Returns (path, generated)."""
    path = cr.p.SECRETS / USER_TOKEN_KEY
    generated = not path.exists() or not path.read_text().strip()
    text = cr.secret_file(path, lambda: ed25519_private_key_pem(cr), uid=cr.SERVICES['platform-access']['uid'],
                          mode=0o400)
    if not text.startswith('-----BEGIN PRIVATE KEY-----'):
        raise RuntimeError('The user-token signing key file is not a PKCS#8 PEM key')
    return path, generated


def activate_user_tokens(cr, sha, run_id, confirm):
    cr.tls_route_present(cr.SERVICES['platform-access']['tls'])
    for issuer in ('messaging', 'calling', 'meeting-service', 'api-gateway'):
        cr.has_signing_identity(issuer)
    if cr.env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true':
        raise RuntimeError('Run activate-gateway-routing first')
    key, generated = user_token_signing_key(cr)
    cr.emit('userTokenSigningKey', {'algorithm': 'Ed25519', 'format': 'PKCS#8 PEM', 'path': USER_TOKEN_KEY_TARGET,
                                    'generated': generated})
    flags = flags_for(TOKENS)
    access = dict(flags['platform-access'], **{USER_TOKEN_KEY_VAR: USER_TOKEN_KEY_TARGET})
    gateway = dict(flags['api-gateway'],
                   **base_urls(cr, 'api-gateway', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))
    cr.apply_activation(TOKENS, sha, run_id, [
        ('platform-access', access, {'mounts': [(key, USER_TOKEN_KEY_TARGET)]}),
        ('developer-platform', flags['developer-platform']),
        ('api-gateway', gateway)], post=lambda: run_acceptance(cr, run_id, TOKENS))


# --- B3: partner chat ----------------------------------------------------------------------------

def activate_chat(cr, sha, run_id, confirm):
    messaging = cr.env_of('messaging')
    if messaging.get('MESSAGING_SESSION_DEVICES_ENABLED') != 'true':
        raise RuntimeError('Partner session devices build on MESSAGING_SESSION_DEVICES_ENABLED '
                           '(activate-first-party-features)')
    cr.has_signing_identity('messaging')
    overrides = dict(flags_for(CHAT)['messaging'],
                     **base_urls(cr, 'messaging', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))
    if messaging.get('CFMEET_USAGE_EXPORT_ENABLED') != 'true':
        # MESSAGE_SENT rides on the usage exporter (activate-usage-export); never half-configured.
        overrides.pop('MESSAGING_MESSAGE_SENT_EXPORT_ENABLED')
        cr.emit('messageSentExport', 'not enabled: Messaging does not export usage (activate-usage-export first)')
    cr.apply_activation(CHAT, sha, run_id, [('messaging', overrides)], post=lambda: run_acceptance(cr, run_id, CHAT))


# --- B4: partner calls (+ the optional, owner-approved calling edge) ------------------------------

def activate_calls(cr, sha, run_id, confirm):
    cr.has_signing_identity('calling')
    overrides = dict(flags_for(CALLS)['calling'],
                     **base_urls(cr, 'calling', (('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'),)))
    cr.apply_activation(CALLS, sha, run_id, [('calling', overrides)], post=lambda: run_acceptance(cr, run_id, CALLS))


def calling_edge_checks(cr):
    """Every moved REST family answers anonymously from the gateway (401/403 + its stamp); the
    signaling WebSocket still answers from Calling directly; first-party smoke unchanged."""
    checks = {}
    for method, path in CALLING_EDGE_PROBES:
        status, gateway = served_by_gateway(cr, method, path)
        checks['gateway %s %s' % (method, path)] = 'PASS' if status in (401, 403) and gateway else 'FAIL %d' % status
    for path in CALLING_WEBSOCKETS:
        status, gateway = served_by_gateway(cr, 'GET', path)
        checks['direct %s (WebSocket)' % path] = 'PASS' if not gateway and status in (400, 401, 403, 426) \
            else 'FAIL %d' % status
    status, _ = cr.http_status(cr.PUBLIC + '/v1/calls', headers={'Authorization': 'Bearer cfmk_sbx_invalid_invalid'})
    checks['forged credential on /v1/calls'] = 'PASS' if status in (401, 403) else 'FAIL %d' % status
    checks.update(cr.public_smoke())
    return checks


def activate_calling_edge(cr, sha, run_id, confirm):
    """Owner decision 1 (plan): /v1/calls and /v1/calling (REST) behind the gateway, which brings
    partner-website CORS, quotas and call metering. Edge only (the gateway already forwards
    Calling since activate-gateway-families). Journaled; restored on any failed check."""
    if CALLING_EDGE_CONFIRM not in (confirm or '').split(','):
        raise RuntimeError('activate-gateway-calling-edge is an owner decision: confirm=' + CALLING_EDGE_CONFIRM)
    require_gateway_edge(cr)
    gateway = cr.env_of('api-gateway')
    if not gateway.get('CF_CALLING_BASE_URL'):
        raise RuntimeError('The gateway has no Calling upstream (activate-gateway-families)')
    if 'calling' in (gateway.get('API_GATEWAY_DISABLED_DOMAINS') or '').split(','):
        raise RuntimeError('The calling domain is disabled on the gateway')
    refuse_collisions(cr, CALLING_EDGE_PREFIXES)
    cr.p.write_private(cr.WORK / 'calling-edge' / run_id / 'snippet.before', cr.GATEWAY_SNIPPET.read_text())
    entry = cr.begin(CALLING_EDGE, sha, run_id)
    entry['platformEdge'] = {'prefixes': list(CALLING_EDGE_PREFIXES)}
    cr.commit(entry)
    old = None
    try:
        old = write_edge(cr, CALLING_EDGE_PREFIXES)
        cr.p.reload_host_nginx()  # nginx -t first
        checks = calling_edge_checks(cr)
        cr.emit('gatewayCallingEdge', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Gateway calling edge checks failed')
        if cr.done(CALLS):
            run_acceptance(cr, run_id, CALLS)
        cr.p.regression()
    except Exception:
        if old is not None:
            cr.GATEWAY_SNIPPET.write_text(old)
            cr.p.reload_host_nginx()
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)


# --- B5: partner push ------------------------------------------------------------------------------

def activate_push(cr, sha, run_id, confirm):
    import notification_operations as notif_ops
    notif_tls = cr.SERVICES['notifications']['tls']
    cr.tls_route_present(notif_tls)
    for name in ('messaging', 'calling'):
        if not cr.env_of(name).get('CF_NOTIFICATIONS_BASE_URL'):
            raise RuntimeError('%s has no CF_NOTIFICATIONS_BASE_URL (its Notification Service activation first)' % name)
    cr.has_signing_identity('developer-platform')
    kid, private, public = notif_ops.sealing_key(cr)
    flags = flags_for(PUSH)
    changes = []
    for replica in notif_ops.REPLICAS:
        overrides, extra = dict(flags[replica]), {}
        if not cr.env_of(replica).get('NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE'):
            # The console seals partner keys to this pair; the replicas open them.
            overrides.update(NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE=notif_ops.HPKE_TARGET, NOTIFICATIONS_HPKE_KEY_ID=kid)
            extra = {'mounts': [(private, notif_ops.HPKE_TARGET)]}
        changes.append((replica, overrides, extra))
    console = dict(flags['developer-platform'], CF_NOTIFICATIONS_SEALING_KEY=public,
                   CF_NOTIFICATIONS_BASE_URL=cr.private_url(notif_tls))
    changes += [('developer-platform', console), ('messaging', flags['messaging']), ('calling', flags['calling'])]
    cr.emit('partnerPushSealingKey', {'kid': kid, 'replicas': list(notif_ops.REPLICAS)})

    def post():
        notif_ops.require_egress(cr, PUSH_PROVIDER_URLS, 'partner push')
        run_acceptance(cr, run_id, PUSH)

    cr.apply_activation(PUSH, sha, run_id, changes, post=post)


# --- B6: meetings ----------------------------------------------------------------------------------

def activate_meetings(cr, sha, run_id, confirm):
    meeting = cr.env_of('meeting-service')
    if not meeting.get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('Meeting Service has no receiver trust bundle')
    cr.has_signing_identity('meeting-service')
    cr.tls_route_present(cr.SERVICES['meeting-service']['tls'])
    gateway = cr.env_of('api-gateway')
    if 'meetings' in (gateway.get('API_GATEWAY_DISABLED_DOMAINS') or '').split(','):
        raise RuntimeError('The meetings domain is disabled on the gateway')
    flags = flags_for(MEETINGS)
    meeting_overrides = dict(flags['meeting-service'], **base_urls(cr, 'meeting-service', (
        ('CF_PLATFORM_ACCESS_BASE_URL', 'platform-access'), ('CF_MESSAGING_BASE_URL', 'messaging'))))
    gateway_overrides = base_urls(cr, 'api-gateway', (('CF_MEETING_SERVICE_BASE_URL', 'meeting-service'),))
    cr.emit('platformMeetingsEdge', {'routed': False, 'reason': 'the machine meeting operations share /api/v2 paths '
                                     'with first-party traffic on api.cfmeet.com; edge routing is an owner decision'})
    cr.apply_activation(MEETINGS, sha, run_id, [
        ('platform-access', flags['platform-access']), ('meeting-service', meeting_overrides),
        ('api-gateway', gateway_overrides)], post=lambda: run_acceptance(cr, run_id, MEETINGS))


# --- read-only acceptance operation ----------------------------------------------------------------

def acceptance(cr, sha, run_id, confirm):
    """`platform-acceptance`: the two-tenant acceptance of every recorded phase, and flag-off
    checks of the others. Writes acceptance data through the public API only (cleaned up)."""
    if confirm:
        raise RuntimeError('platform-acceptance takes no confirmation')
    if not cr.done(USERS):
        raise RuntimeError('Acceptance needs ' + USERS)
    import platform_acceptance
    checks = platform_acceptance.run(run_id, enabled_stages(cr), cr.emit)
    cr.emit('platformAcceptanceSummary', checks)
    if any(v != 'PASS' for v in checks.values()):
        raise RuntimeError('Platform acceptance failed')
    return checks


ACTIVATIONS = {
    USERS: activate_users,
    TOKENS: activate_user_tokens,
    CHAT: activate_chat,
    CALLS: activate_calls,
    CALLING_EDGE: activate_calling_edge,
    PUSH: activate_push,
    MEETINGS: activate_meetings,
}
