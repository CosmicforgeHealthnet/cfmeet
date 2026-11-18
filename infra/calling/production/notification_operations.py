"""Notification Service activation operations (roadmap N6 grants, N14 canary governance).

Two coordinated-release operations, each journaled and reversible:

* ``activate-notifications-owners`` grants the owner read APIs to ``cfmeet.notifications``
  (X1/X2 Meeting, X3 Messaging, X4 Communications, X5 Organizations) and points both
  replicas at the owners' private TLS routes (``NOTIFICATIONS_AUTHORITIES``). Each owner
  route is first proven to exist (an unauthenticated probe must answer 401, never 404).
  Nothing is delivered by this step: every route is still governed by ``notif_routing``.

* ``activate-notifications-routing`` changes ONE ``notif_routing`` row per run. The
  ``confirm`` input names it: ``<category>-<channel>-<mode>`` in lower case with hyphens,
  e.g. ``contact-requests-in-app-shadow``. Gates (Document 6 N14, Document 4 §14.3):

  - any mode may move to ``off`` or ``shadow`` (no delivery is possible in SHADOW);
  - ``active`` for an EMAIL row needs a configured service email provider and gate E1
    (``activate-resend-email`` recorded) — E2 evidence is checked by the operator report;
  - ``active`` for any CRITICAL category (codes, security notices) is refused by this
    version: N12 gates E3 / E4, the fallback drill and live alerting are not yet evidenced;
  - ``active`` for social categories needs ``activate-notifications-owners`` (blocks and
    privacy are asked at the Hub, rule 4 / 6).

  The previous mode and version are recorded in the journal entry; ``rollback`` restores
  them (no container restart: the service reads the table on every event).

* ``activate-notifications-relay`` (owner-approved 2026-11-16) turns on the Meeting Service
  user relay U1-U9 under ``/api/v2/notifications``: both Notification Service replicas trust
  the Meeting Service for ``notifications-user.forward`` (I2 only), and the Meeting Service
  gets ``CF_NOTIFICATIONS_BASE_URL`` (the Notification Service private TLS route). The relay
  answers 401 without an Account session before and after; nothing is delivered by this step
  (the inbox only shows what ACTIVE routing rows produced).

* ``activate-messaging-publications`` (roadmap N7b) lets Messaging publish to
  I1: both replicas trust ``cfmeet.messaging`` for ``notifications-event.publish``, and
  Messaging gets ``CF_NOTIFICATIONS_BASE_URL``, which starts its publication relay. Nothing
  is published while ``MESSAGE_WAKEUPS`` is ``LEGACY_ONLY``; nothing is delivered by the
  service while its own routing rows are OFF.

* ``activate-notifications-email`` (roadmap N5 / N11, owner decision 2026-11-17) makes Resend
  the Notification Service's email provider with the credentials already on the VPS
  (``/opt/cfmeet-core/secrets/resend-api-key`` and ``resend-webhook-secret``, root-only, never
  printed, never leaving the VPS). The replicas run as their own user, so each activation
  writes service-owned 0400 copies next to the originals and mounts them read-only on both
  replicas (``NOTIFICATIONS_RESEND_API_KEY_FILE`` / ``NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE``).
  The provider account keeps its one webhook on the Meeting Service, which hands events for
  messages it did not send to the service's own Svix-verified callback. Nothing is sent by
  this step: every ``notif_routing`` EMAIL row stays as it is (OFF until a routing run).

* ``activate-notification-fence`` changes ONE producer routing-fence row per run (Document 4
  §9.6). ``confirm`` is ``<service>-<category>-<mode>`` in lower case with hyphens, e.g.
  ``messaging-message-wakeups-shadow``. Modes: ``legacy-only``, ``shadow``, ``fenced-old``,
  ``fenced-new`` (canary 100: every new decision NEW, the legacy path is skipped for it). Only
  for categories whose producer code records decisions (``FENCE_ROUTABLE``), only while the
  producer image reports ``routingProtocol`` (M-12). ``fenced-new`` needs SHADOW or FENCED_OLD
  first and every Notification Service routing row the category is delivered on ACTIVE, so
  a NEW decision is never left undelivered. Journaled; ``rollback`` restores the previous mode
  (decisions already made stay sticky: a NEW decision is delivered by the service).
"""

import json
import re

CHANNELS = {'email': 'EMAIL', 'push': 'PUSH', 'in-app': 'IN_APP', 'operator': 'OPERATOR'}
MODES = {'off': 'OFF', 'shadow': 'SHADOW', 'active': 'ACTIVE'}
CRITICAL = frozenset({'AUTH_VERIFICATION', 'AUTH_PASSWORD_RESET', 'SECURITY_DEVICE_STEP_UP',
                      'SECURITY_PASSWORD_CHANGED', 'SECURITY_DEVICE_LINKED'})
SOCIAL = frozenset({'CONTACT_REQUESTS', 'GROUP_INVITATIONS', 'MISSED_CALLS', 'VOICEMAIL', 'MESSAGE_WAKEUPS'})
CATEGORY_RE = re.compile(r'[A-Z][A-Z_]{1,39}')

# Owner read APIs: (owner service, permissions granted to cfmeet.notifications, probe path,
# NOTIFICATIONS_AUTHORITIES names, base-URL variable).
OWNERS = (
    ('meeting-service', ('account-contacts.read', 'meeting-notification-recipients.read'),
     '/internal/v1/accounts/notification-contacts/resolve', ('account', 'meeting'), 'CF_MEETING_SERVICE_BASE_URL'),
    ('messaging', ('messaging-wakeups.authorize',),
     '/internal/v1/notifications/wakeups/authorize', ('messaging',), 'CF_MESSAGING_BASE_URL'),
    ('communications', ('communications-restrictions.read',),
     '/internal/v1/communications/restrictions/check', ('communications',), 'CF_COMMUNICATIONS_BASE_URL'),
    ('organizations', ('organization-invitations.read',),
     '/internal/v1/organization-invitations/00000000-0000-0000-0000-000000000000/invitee/resolve', ('organizations',),
     'CF_ORGANIZATIONS_BASE_URL'),
)

GRANTS = tuple(('activate-notifications-owners', owner, 'notifications', permissions)
               for owner, permissions, _probe, _names, _var in OWNERS)

# U1-U9 relay: I2 is granted to the Meeting Service on BOTH replicas (each container has its
# own trust-bundle file; the identity is the same).
RELAY_PERMISSIONS = ('notifications-user.forward',)
RELAY_PROBE = '/internal/v1/notifications/dispatch'
RELAY_PUBLIC_PROBE = '/api/v2/notifications/read'
GRANTS += tuple(('activate-notifications-relay', replica, 'meeting-service', RELAY_PERMISSIONS)
                for replica in ('notifications', 'notifications-2'))

REPLICAS = ('notifications', 'notifications-2')

# N7b: Messaging publishes message wake-ups (shadow copies, later NEW decisions) to I1.
PUBLICATION_PERMISSIONS = ('notifications-event.publish',)
PUBLICATION_PROBE = '/internal/v1/notification-events'
GRANTS += tuple(('activate-messaging-publications', replica, 'messaging', PUBLICATION_PERMISSIONS)
                for replica in REPLICAS)

# N7: the other producers publish through the same I1 grant. Each activation grants
# notifications-event.publish to ONE producer on both replicas and gives the producer's containers
# CF_NOTIFICATIONS_BASE_URL (its relay starts; nothing is delivered while the service's routing
# rows for its categories are OFF). `outbox`: the producer's publication table, proven to exist
# in the running image's database before the switch.
PRODUCERS = {
    'organizations': dict(operation='activate-organizations-publications', containers=('organizations',),
                          outbox='organization_notification_publications'),
    'communications': dict(operation='activate-communications-publications', containers=('communications',),
                           outbox='communication_notification_publications'),
    # Credential expiring / expired (no legacy user notice). The production-access decision
    # stays on its legacy email (developer_access.notify_applicant) until
    # DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE is switched together with it.
    'developer-platform': dict(operation='activate-developer-publications',
                               containers=('developer-platform', 'developer-platform-worker'),
                               outbox='devplat_notification_publications'),
    # Missed call / voicemail ready / recording ready (no legacy user notice; FIRST_PARTY tenants only).
    'calling': dict(operation='activate-calling-publications', containers=('calling',),
                    outbox='calling_notification_publications'),
    # MEETING_INVITATION / MEETING_NOTIFICATION / ORGANIZATION_INVITATION through the fence.
    'meeting-service': dict(operation='activate-meeting-publications', containers=('meeting-service',),
                            outbox='meeting_notification_publications', sealing=True),
}
GRANTS += tuple((spec['operation'], replica, issuer, PUBLICATION_PERMISSIONS)
                for issuer, spec in PRODUCERS.items() for replica in REPLICAS)

# N10: the Notification Service pulls the Hub's preferences (communications-preferences.export,
# received by the Hub) and the Hub facade forwards U6 / U7 (notifications-user.forward,
# received by both replicas).
GRANTS += (('activate-preference-copy', 'communications', 'notifications', ('communications-preferences.export',)),) \
    + tuple(('activate-preference-copy', replica, 'communications', ('notifications-user.forward',))
            for replica in REPLICAS)

# N12: the Account CRITICAL lane. The service claims / completes an Account lease (X6 / X7,
# received by the Meeting Service) before it may send a CRITICAL message.
GRANTS += (('activate-critical-lane', 'meeting-service', 'notifications', ('account-route-lease.claim',)),)
CRITICAL_FALLBACK_KEY = 'meeting-account-fallback.key'
CRITICAL_FALLBACK_TARGET = '/run/secrets/account-fallback.key'
# Move order. The device-linked notice and the device-link step-up code first; then the
# passwordless codes (sign-in, email change, action step-up: catalogue 1.2.0, events #32-#34),
# which need NOTIFICATIONS_APP_URL equal to the Meeting Service's APP_URL (the magic link).
# The password categories have no producer left after PR #97.
CRITICAL_ORDER = ('SECURITY_DEVICE_LINKED', 'SECURITY_DEVICE_STEP_UP', 'AUTH_VERIFICATION',
                  'SECURITY_PASSWORD_CHANGED', 'AUTH_PASSWORD_RESET')

# N9: push. The service sends Web Push with Messaging's VAPID key (the key browser
# subscriptions were made with); Messaging mirrors its registrations (I13); Calling's call
# alerts use the fast lane (I3).
GRANTS += tuple(('activate-messaging-push-mirror', replica, 'messaging', ('notifications-push-registration.sync',))
                for replica in REPLICAS) \
    + tuple(('activate-calling-push', replica, 'calling', ('notifications-call-push.dispatch',)) for replica in REPLICAS)
WEB_PUSH_TARGET = '/run/secrets/web-push-vapid.pem'
WEB_PUSH_SUBJECT = 'https://cfmeet.com'

FLAGGED = {'meeting-service': {'NOTIFICATION_OWNER_API_ENABLED': 'true'}}
SCOPE_VARS = ('CFMEET_COMMUNICATION_TENANT_ID', 'CFMEET_COMMUNICATION_APPLICATION_ID', 'CFMEET_COMMUNICATION_ENVIRONMENT_ID',
              'CFMEET_COMMUNICATION_ENVIRONMENT_KIND', 'CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID')


def parse_routing(confirm):
    """``contact-requests-in-app-shadow`` -> ('CONTACT_REQUESTS', 'IN_APP', 'SHADOW')."""
    text = (confirm or '').strip()
    if not re.fullmatch(r'[a-z-]{3,120}', text):
        text = ''
    for suffix, mode in MODES.items():
        if not text.endswith('-' + suffix):
            continue
        rest = text[:-len(suffix) - 1]
        for word, channel in CHANNELS.items():
            if rest.endswith('-' + word):
                category = rest[:-len(word) - 1].replace('-', '_').upper()
                if CATEGORY_RE.fullmatch(category):
                    return category, channel, mode
    raise RuntimeError('confirm must be <category>-<channel>-<mode>, e.g. contact-requests-in-app-shadow '
                       '(channels: %s; modes: %s)' % (', '.join(CHANNELS), ', '.join(MODES)))


def gate(category, channel, mode, facts):
    """Refusal reason, or None. ``facts``: email_provider, resend_done, owners_done."""
    if mode != 'ACTIVE':
        return None
    if category in CRITICAL and not facts.get('critical_lane_done'):
        return ('%s is CRITICAL: ACTIVE needs the Account CRITICAL lane (activate-critical-lane: X6/X7 lease '
                'grant, fallback key) first' % category)
    if channel == 'EMAIL':
        if facts.get('email_provider') in (None, '', 'none'):
            return 'EMAIL ACTIVE needs a configured Notification Service email provider'
        if not facts.get('resend_done'):
            return 'EMAIL ACTIVE needs gate E1 (activate-resend-email with delivery evidence)'
    if category in SOCIAL and not facts.get('owners_done'):
        return '%s ACTIVE needs activate-notifications-owners (blocks and privacy are asked at the Hub)' % category
    return None


def routing_row(cr, category, channel):
    db = cr.SERVICES['notifications']['db']
    out = cr.release.db_sql(db, "SELECT coalesce(json_agg(json_build_object('mode', mode, 'version', version)), '[]') "
                                "FROM notif_routing WHERE category = '%s' AND channel = '%s';" % (category, channel))
    rows = json.loads(out or '[]')
    if len(rows) != 1:
        raise RuntimeError('No routing row %s/%s' % (category, channel))
    return rows[0]


def set_mode(cr, category, channel, mode, run_id, expected_version=None):
    assert CATEGORY_RE.fullmatch(category) and channel in CHANNELS.values() and mode in MODES.values()
    assert re.fullmatch(r'[0-9-]{1,32}', run_id)
    db = cr.SERVICES['notifications']['db']
    guard = '' if expected_version is None else ' AND version = %d' % int(expected_version)
    out = cr.release.db_sql(db, "WITH u AS (UPDATE notif_routing SET mode = '%s', version = version + 1, fence_at = now(), "
                                "changed_by_run = '%s', changed_at = now() WHERE category = '%s' AND channel = '%s'%s "
                                "RETURNING version) SELECT count(*) || ':' || coalesce(max(version), 0) FROM u;"
                                % (mode, run_id, category, channel, guard))
    count, version = (out or '0:0').strip().split(':')
    if count != '1':
        raise RuntimeError('Routing row %s/%s changed concurrently; nothing applied' % (category, channel))
    return int(version)


def activate_routing(cr, sha, run_id, confirm):
    category, channel, mode = parse_routing(confirm)
    cr.require_release(sha)
    env = cr.env_of('notifications')
    facts = dict(email_provider=env.get('NOTIFICATIONS_EMAIL_PROVIDER'), resend_done=cr.done('activate-resend-email'),
                 owners_done=cr.done('activate-notifications-owners'),
                 critical_lane_done=cr.done('activate-critical-lane'))
    refusal = gate(category, channel, mode, facts)
    if refusal:
        raise RuntimeError('Routing gate: ' + refusal + '; nothing changed')
    before = routing_row(cr, category, channel)
    entry = cr.begin('activate-notifications-routing', sha, run_id)
    entry['notificationRouting'] = dict(category=category, channel=channel, before=before['mode'], after=mode)
    cr.commit(entry)
    try:
        version = set_mode(cr, category, channel, mode, run_id, before['version'])
    except Exception:
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['notificationRouting']['version'] = version
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('notificationRouting', entry['notificationRouting'])


def undo_routing(cr, record, run_id):
    """Rollback of one routing change: back to the recorded previous mode."""
    version = set_mode(cr, record['category'], record['channel'], record['before'], run_id)
    cr.emit('notificationRoutingRestored', dict(record, restoredVersion=version))


def private_status(cr, url):
    """HTTP status of an unauthenticated POST through the private TLS proxy (from the private
    network, where api.cfmeet.com is the proxy's alias)."""
    out = cr.p.run('docker', 'run', '--rm', '--network', cr.p.NET, cr.PROBE_IMAGE, '-s', '-o', '/dev/null',
                   '-w', '%{http_code}', '--max-time', '10', '-X', 'POST', '-H', 'content-type: application/json',
                   '-d', '{}', url)
    return (out or '').strip()


def deny_owner_routes_publicly(cr):
    """Apply the public-host denials of the Meeting owner routes X1 / X2 and prove each one
    answers the nginx denial (404 + X-CFMeet-Ingress: denied), never the Meeting Service."""
    cr.release.secure_public_ingress()
    cr.p.reload_host_nginx()
    results = {path: cr.public_denial_probe(path) for path in cr.release.NOTIFICATION_OWNER_DENIAL_PROBES}
    cr.emit('publicOwnerDenial', results)
    if not all(results.values()):
        raise RuntimeError('The public host still reaches a Meeting owner route; nothing was activated')


def activate_owners(cr, sha, run_id, confirm):
    cr.require_release(sha)
    names, overrides = [], {}
    for owner, _permissions, probe, authority_names, var in OWNERS:
        tls = cr.SERVICES[owner]['tls']
        cr.tls_route_present(tls)
        # The Meeting owner routes are mounted only with NOTIFICATION_OWNER_API_ENABLED (set
        # below); the other owners serve them unconditionally once released.
        if owner not in FLAGGED:
            status = private_status(cr, cr.private_url(tls) + probe)
            if status != '401':
                raise RuntimeError('Owner API of %s is not served by the running image (probe answered %s); '
                                   'release the owner first' % (owner, status))
        names.extend(authority_names)
        overrides[var] = cr.private_url(tls)
    overrides['NOTIFICATIONS_AUTHORITIES'] = ','.join(names)
    # X4 requires the first-party delegated scope: the same values the Meeting Service bridge
    # already uses (non-secret identifiers), copied verbatim.
    meeting = cr.env_of('meeting-service')
    for var in SCOPE_VARS:
        if not meeting.get(var):
            raise RuntimeError('Meeting Service lacks %s; the Hub owner API needs the first-party scope' % var)
        overrides[var] = meeting[var]
    if not meeting.get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('Meeting Service has no trust bundle; X1/X2 need one')
    cr.has_signing_identity('notifications')
    # The Meeting owner routes must never be reachable through the public host: the denial is
    # applied (host nginx, deny-only) and proven before they are mounted.
    deny_owner_routes_publicly(cr)

    def post():
        for owner, _permissions, probe, _names, _var in OWNERS:
            status = private_status(cr, cr.private_url(cr.SERVICES[owner]['tls']) + probe)
            if status != '401':
                raise RuntimeError('Owner API of %s answers %s after activation' % (owner, status))

    cr.apply_activation('activate-notifications-owners', sha, run_id,
                        [('meeting-service', dict(FLAGGED['meeting-service']))]
                        + [(replica, dict(overrides)) for replica in REPLICAS], post=post)


def activate_relay(cr, sha, run_id, confirm):
    cr.require_release(sha)
    notif_tls = cr.SERVICES['notifications']['tls']
    meeting_tls = cr.SERVICES['meeting-service']['tls']
    cr.tls_route_present(notif_tls)
    cr.tls_route_present(meeting_tls)
    # I2 exists in the running Notification Service image and refuses an unsigned caller.
    status = private_status(cr, cr.private_url(notif_tls) + RELAY_PROBE)
    if status != '401':
        raise RuntimeError('Notification Service I2 answered %s to an unsigned probe (expected 401); '
                           'release the Notification Service first' % status)
    # The running Meeting image serves the relay routes (401 before the session check passes).
    status = private_status(cr, cr.private_url(meeting_tls) + RELAY_PUBLIC_PROBE)
    if status != '401':
        raise RuntimeError('Meeting Service relay answered %s without a session (expected 401); '
                           'release the Meeting Service first' % status)
    # The relay signs with the Meeting identity; it refuses to start without one.
    cr.has_signing_identity('meeting-service')
    overrides = {'CF_NOTIFICATIONS_BASE_URL': cr.private_url(notif_tls)}

    def post():
        status = private_status(cr, cr.private_url(meeting_tls) + RELAY_PUBLIC_PROBE)
        if status != '401':
            raise RuntimeError('Meeting Service relay answers %s without a session after activation' % status)
        status = private_status(cr, cr.private_url(notif_tls) + RELAY_PROBE)
        if status != '401':
            raise RuntimeError('Notification Service I2 answers %s to an unsigned probe after activation' % status)

    cr.apply_activation('activate-notifications-relay', sha, run_id, [('meeting-service', overrides)], post=post)


def activate_messaging_publications(cr, sha, run_id, confirm):
    cr.require_release(sha)
    notif_tls = cr.SERVICES['notifications']['tls']
    cr.tls_route_present(notif_tls)
    # I1 exists in the running Notification Service image and refuses an unsigned caller.
    status = private_status(cr, cr.private_url(notif_tls) + PUBLICATION_PROBE)
    if status != '401':
        raise RuntimeError('Notification Service I1 answered %s to an unsigned probe (expected 401); '
                           'release the Notification Service first' % status)
    # The running Messaging image has the publication outbox (fence history 2) and the fence.
    require_fence_protocol(cr, 'messaging')
    present = (_producer_sql(cr, 'messaging')(
        "SELECT to_regclass('public.messaging_notification_publications') IS NOT NULL;") or '').strip()
    if present not in ('t', 'true'):
        raise RuntimeError('Messaging has no notification publication outbox; release a Messaging image with N7b first')
    cr.has_signing_identity('messaging')
    overrides = {'CF_NOTIFICATIONS_BASE_URL': cr.private_url(notif_tls)}

    def post():
        require_fence_protocol(cr, 'messaging')
        status = private_status(cr, cr.private_url(notif_tls) + PUBLICATION_PROBE)
        if status != '401':
            raise RuntimeError('Notification Service I1 answers %s to an unsigned probe after activation' % status)

    cr.apply_activation('activate-messaging-publications', sha, run_id,
                        [('messaging', overrides)], post=post)


# --- The service's HPKE sealing key (codes, tokens and addresses sealed by producers) --------
# The private key opens sealed parts in the renderer only (NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE on
# both replicas, owned by the service user, 0400, never printed); producers get only the public
# key (CF_NOTIFICATIONS_SEALING_KEY = <kid>:<base64url X25519 public key>). Generated once on the
# VPS; a new pair is never generated over an existing one (sealed rows in flight would not open).
HPKE_PRIVATE = 'notifications-hpke.key'
HPKE_PUBLIC = 'notifications-hpke.pub'
HPKE_TARGET = '/run/secrets/notifications-hpke.key'
X25519_P = 2 ** 255 - 19


def x25519_public(private):
    """RFC 7748 X25519(k, 9): the public key of a 32-byte private key (clamped as the
    Notification Service's HPKE library clamps it). Dependency-free: the VPS needs no extra
    Python package."""
    k = bytearray(private)
    k[0] &= 248
    k[31] &= 127
    k[31] |= 64
    scalar = int.from_bytes(bytes(k), 'little')
    x1, x2, z2, x3, z3, swap = 9, 1, 0, 9, 1, 0
    a24 = 121665
    for t in reversed(range(255)):
        bit = (scalar >> t) & 1
        swap ^= bit
        if swap:
            x2, x3, z2, z3 = x3, x2, z3, z2
        swap = bit
        a, b = (x2 + z2) % X25519_P, (x2 - z2) % X25519_P
        aa, bb = a * a % X25519_P, b * b % X25519_P
        e = (aa - bb) % X25519_P
        c, d = (x3 + z3) % X25519_P, (x3 - z3) % X25519_P
        da, cb = d * a % X25519_P, c * b % X25519_P
        x3 = (da + cb) ** 2 % X25519_P
        z3 = x1 * (da - cb) ** 2 % X25519_P
        x2 = aa * bb % X25519_P
        z2 = e * (aa + a24 * e) % X25519_P
    if swap:
        x2, z2 = x3, z3
    return (x2 * pow(z2, X25519_P - 2, X25519_P) % X25519_P).to_bytes(32, 'little')


def sealing_key(cr, today=None):
    """(kid, private key path, producer value) — generated once, then reused."""
    import base64
    import datetime
    import os
    import secrets
    private, public = cr.p.SECRETS / HPKE_PRIVATE, cr.p.SECRETS / HPKE_PUBLIC
    if not private.exists():
        kid = 'notif-hpke-%s-1' % (today or datetime.date.today()).strftime('%Y-%m')
        raw = secrets.token_bytes(32)
        cr.p.write_private(private, base64.b64encode(raw).decode())
        cr.p.write_private(public, '%s:%s' % (kid, base64.urlsafe_b64encode(x25519_public(raw)).decode().rstrip('=')))
    uid = cr.SERVICES['notifications']['uid']
    os.chown(private, uid, uid)
    os.chmod(private, 0o400)
    value = public.read_text().strip()
    kid = value.split(':', 1)[0]
    if not re.fullmatch(r'notif-hpke-\d{4}-\d{2}(-[a-z0-9]{1,8})?', kid):
        raise RuntimeError('The sealing public key file is malformed')
    return kid, private, value


def producer_sql(cr, service):
    if service == 'meeting-service':
        return cr.meeting_psql
    return lambda q, d=cr.SERVICES[service]['db']: cr.release.db_sql(d, q)


def activate_publications(cr, sha, run_id, producer):
    """One producer of PRODUCERS: grant I1 and point its containers at the service."""
    spec = PRODUCERS[producer]
    if not spec['outbox']:
        raise RuntimeError('%s has no publication outbox in this release tooling' % producer)
    cr.require_release(sha)
    notif_tls = cr.SERVICES['notifications']['tls']
    cr.tls_route_present(notif_tls)
    probe = cr.private_url(notif_tls) + PUBLICATION_PROBE
    status = private_status(cr, probe)
    if status != '401':
        raise RuntimeError('Notification Service I1 answered %s to an unsigned probe (expected 401)' % status)
    present = (producer_sql(cr, producer)(
        "SELECT to_regclass('public.%s') IS NOT NULL;" % spec['outbox']) or '').strip()
    if present not in ('t', 'true'):
        raise RuntimeError('%s has no %s table; release an image with its N7 publication first'
                           % (producer, spec['outbox']))
    for container in spec['containers']:
        cr.has_signing_identity(container)
    overrides = {'CF_NOTIFICATIONS_BASE_URL': cr.private_url(notif_tls)}
    changes = []
    if spec.get('sealing'):
        # Sealed parts (invitation tokens / addresses): the service opens them, the producer
        # only seals. Both replicas get the private key, the producer the public one.
        kid, private, public = sealing_key(cr)
        changes += [(replica, {'NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE': HPKE_TARGET, 'NOTIFICATIONS_HPKE_KEY_ID': kid},
                     {'mounts': [(private, HPKE_TARGET)]}) for replica in REPLICAS]
        overrides['CF_NOTIFICATIONS_SEALING_KEY'] = public
        cr.emit('notificationsSealingKey', {'kid': kid, 'replicas': list(REPLICAS), 'producer': producer})
    changes += [(container, dict(overrides)) for container in spec['containers']]

    def post():
        status = private_status(cr, probe)
        if status != '401':
            raise RuntimeError('Notification Service I1 answers %s to an unsigned probe after activation' % status)

    cr.apply_activation(spec['operation'], sha, run_id, changes, post=post)


# --- Routing fence v1 (Document 4 §9.6): producers' protocol and the rollback floor ---------
# Producers with an old delivery path keep their routing decisions in their own database.
FENCE_PRODUCERS = {
    'meeting-service': dict(table='notification_routing_state', binary='notification-fence-migration'),
    'messaging': dict(table='messaging_notification_routing_state', binary='messaging-notification-fence-migration'),
}


def _producer_sql(cr, service):
    if service == 'meeting-service':
        return cr.meeting_psql
    return lambda q, d=cr.SERVICES[service]['db']: cr.release.db_sql(d, q)


def fenced_categories(cr, service):
    """'CATEGORY:MODE' of every category of `service` that is not LEGACY_ONLY ([] before the
    fence migration exists)."""
    table = FENCE_PRODUCERS[service]['table']
    sql = _producer_sql(cr, service)
    if (sql("SELECT to_regclass('public.%s') IS NOT NULL;" % table) or '').strip() not in ('t', 'true'):
        return []
    out = (sql("SELECT coalesce(string_agg(category || ':' || mode, ',' ORDER BY category), '') FROM %s "
               "WHERE mode <> 'LEGACY_ONLY';" % table) or '').strip()
    return [x for x in out.split(',') if x]


def image_supports_fence(cr, service, image):
    binary = FENCE_PRODUCERS[service]['binary']
    out = cr.p.run('docker', 'run', '--rm', '--network', 'none', '--entrypoint', '/bin/sh', image, '-c',
                   'test -x /app/%s && echo fence || echo none' % binary)
    return (out or '').strip() == 'fence'


def rollback_floor(cr, service, image):
    """M-13: never run a producer image without the routing fence while any of its categories
    has left LEGACY_ONLY; route them back first (activate-notifications-routing / §9.8)."""
    if service not in FENCE_PRODUCERS:
        return
    fenced = fenced_categories(cr, service)
    if fenced and not image_supports_fence(cr, service, image):
        raise RuntimeError('Rollback floor: %s has categories outside LEGACY_ONLY (%s) and image %s has no routing '
                           'fence; route every category back to LEGACY_ONLY first' % (service, ', '.join(fenced), image))


def routing_protocol(cr, service):
    """routingProtocol reported by the running producer's readiness JSON (None when absent)."""
    s = cr.SERVICES[service]
    out = cr.p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'], cr.PROBE_IMAGE,
                   '-sS', '--max-time', '4', 'http://127.0.0.1:%d%s' % (s['port'], s['health']))
    try:
        value = json.loads(out or '{}').get('routingProtocol')
    except ValueError:
        return None
    return value if isinstance(value, int) else None


def require_fence_protocol(cr, service, minimum=1):
    """M-12: a category of `service` may leave LEGACY_ONLY only while its running image reports
    routingProtocol >= minimum (pre-fence binaries do not report it)."""
    value = routing_protocol(cr, service)
    if value is None or value < minimum:
        raise RuntimeError('%s reports routingProtocol %s (need >= %d); release a fence-capable image first'
                           % (service, value, minimum))
    return value


# --- Producer routing-fence mode (Document 4 §9.6) ------------------------------------------
# Categories whose producer code records a routing decision before writing legacy rows. A
# category outside this set must stay LEGACY_ONLY: its guard trigger would refuse the legacy
# rows its producer still writes without a decision.
# Each routable producer category: the activation that lets the producer publish, and the
# Notification Service routing rows that must be ACTIVE before its route may become NEW (a NEW
# decision skips the legacy path, so the service must deliver it on the same channels).
FENCE_ROUTABLE = {
    ('messaging', 'MESSAGE_WAKEUPS'): dict(publications='activate-messaging-publications',
                                           service_channels=('PUSH',), service_category='MESSAGE_WAKEUPS',
                                           new_requires=('activate-messaging-push-mirror',)),
    # Meeting's non-critical email categories (N11). The CRITICAL ones (AUTH_*, SECURITY_*) stay
    # LEGACY_ONLY until the N12 lease path exists.
    ('meeting-service', 'MEETING_INVITATION'): dict(publications='activate-meeting-publications',
                                                    service_channels=('EMAIL',), service_category='MEETING_INVITATION'),
    ('meeting-service', 'ORGANIZATION_INVITATION'): dict(publications='activate-meeting-publications',
                                                         service_channels=('EMAIL',),
                                                         service_category='ORGANIZATION_INVITATION'),
    ('meeting-service', 'MEETING_NOTIFICATION'): dict(publications='activate-meeting-publications',
                                                      service_channels=('EMAIL', 'IN_APP'),
                                                      service_category='MEETING_NOTIFICATION'),
}
# N12: the CRITICAL categories, once the lane is activated (lease grant, fallback key).
FENCE_ROUTABLE.update({
    ('meeting-service', category): dict(publications='activate-critical-lane', service_channels=('EMAIL',),
                                        service_category=category)
    for category in CRITICAL_ORDER})
# The sign-in email carries a magic link the service builds from NOTIFICATIONS_APP_URL; NEW
# would send a different link unless it equals the Meeting Service's APP_URL.
FENCE_ROUTABLE[('meeting-service', 'AUTH_VERIFICATION')]['same_app_url'] = True
SERVICE_DEFAULT_APP_URL = 'https://cfmeet.com'
MEETING_DEFAULT_APP_URL = 'http://localhost:3000'


def app_url(value, default):
    return (value or default).rstrip('/')


def app_urls(cr):
    """{container: APP_URL} of the Meeting Service and both service replicas (defaults applied)."""
    urls = {'meeting-service': app_url(cr.env_of('meeting-service').get('APP_URL'), MEETING_DEFAULT_APP_URL)}
    for replica in REPLICAS:
        urls[replica] = app_url(cr.env_of(replica).get('NOTIFICATIONS_APP_URL'), SERVICE_DEFAULT_APP_URL)
    return urls
FENCE_MODES = {'legacy-only': 'LEGACY_ONLY', 'shadow': 'SHADOW', 'fenced-old': 'FENCED_OLD', 'fenced-new': 'FENCED_NEW'}
# FENCED_NEW routes every new decision NEW (canary 100); the canary percentage is not a
# workflow input (confirm accepts letters and hyphens only).
FENCED_NEW_CANARY = 100


def parse_fence(confirm):
    """``messaging-message-wakeups-shadow`` -> ('messaging', 'MESSAGE_WAKEUPS', 'SHADOW')."""
    text = (confirm or '').strip()
    if re.fullmatch(r'[a-z-]{3,120}', text):
        for suffix, mode in FENCE_MODES.items():
            if not text.endswith('-' + suffix):
                continue
            rest = text[:-len(suffix) - 1]
            for service in FENCE_PRODUCERS:
                if rest.startswith(service + '-'):
                    category = rest[len(service) + 1:].replace('-', '_').upper()
                    if CATEGORY_RE.fullmatch(category):
                        return service, category, mode
    raise RuntimeError('confirm must be <service>-<category>-<mode>, e.g. messaging-message-wakeups-shadow '
                       '(services: %s; modes: %s)' % (', '.join(FENCE_PRODUCERS), ', '.join(FENCE_MODES)))


def fence_row(cr, service, category):
    table = FENCE_PRODUCERS[service]['table']
    out = _producer_sql(cr, service)(
        "SELECT coalesce(json_agg(json_build_object('mode', mode, 'version', routing_version)), '[]') "
        "FROM %s WHERE category = '%s';" % (table, category))
    rows = json.loads(out or '[]')
    if len(rows) != 1:
        raise RuntimeError('No routing fence row %s/%s' % (service, category))
    return rows[0]


def set_fence_mode(cr, service, category, mode, run_id, expected_version=None):
    assert service in FENCE_PRODUCERS and CATEGORY_RE.fullmatch(category) and mode in FENCE_MODES.values()
    assert re.fullmatch(r'[0-9-]{1,32}', run_id)
    table = FENCE_PRODUCERS[service]['table']
    canary = FENCED_NEW_CANARY if mode == 'FENCED_NEW' else 0
    guard = '' if expected_version is None else ' AND routing_version = %d' % int(expected_version)
    out = _producer_sql(cr, service)(
        "WITH u AS (UPDATE %s SET mode = '%s', canary_percent = %d, routing_version = routing_version + 1, "
        "changed_by_run = '%s', changed_at = now() WHERE category = '%s'%s RETURNING routing_version) "
        "SELECT count(*) || ':' || coalesce(max(routing_version), 0) FROM u;"
        % (table, mode, canary, run_id, category, guard))
    count, version = (out or '0:0').strip().split(':')
    if count != '1':
        raise RuntimeError('Routing fence row %s/%s changed concurrently; nothing applied' % (service, category))
    return int(version)


def fence_gate(service, category, mode, before, facts):
    """Refusal reason, or None. ``facts``: protocol (routingProtocol of the running image),
    publications_done (the producer's publication activation is recorded), service_modes
    ({channel: mode} of the Notification Service routing rows of the category)."""
    if mode == 'LEGACY_ONLY':
        return None
    spec = FENCE_ROUTABLE.get((service, category))
    if not spec:
        return ('%s/%s has no producer code that records routing decisions; it must stay LEGACY_ONLY'
                % (service, category))
    if not isinstance(facts.get('protocol'), int) or facts['protocol'] < 1:
        return '%s reports no routingProtocol; release a fence-capable image first' % service
    if not facts.get('publications_done'):
        return '%s needs %s first' % (mode, spec['publications'])
    if mode == 'FENCED_NEW':
        missing = [op for op in spec.get('new_requires', ()) if op not in facts.get('done', ())]
        if missing:
            return 'FENCED_NEW needs %s first' % ', '.join(missing)
        # Document 4 §9.4: OLD -> NEW only through SHADOW (observed) or FENCED_OLD.
        if before not in ('SHADOW', 'FENCED_OLD', 'FENCED_NEW'):
            return 'FENCED_NEW needs SHADOW or FENCED_OLD first (now %s)' % before
        modes = facts.get('service_modes') or {}
        missing = [c for c in spec['service_channels'] if modes.get(c) != 'ACTIVE']
        if missing:
            return ('FENCED_NEW would skip the legacy path while the Notification Service does not deliver %s on %s; '
                    'activate those routing rows first' % (spec['service_category'], ', '.join(missing)))
        urls = facts.get('app_urls') or {}
        if spec.get('same_app_url') and len(set(urls.values())) != 1:
            return ('FENCED_NEW would send magic links to a different web app: APP_URL %s; '
                    'run activate-critical-lane again on this release' % json.dumps(urls, sort_keys=True))
    return None


def service_modes(cr, category):
    db = cr.SERVICES['notifications']['db']
    out = cr.release.db_sql(db, "SELECT coalesce(json_object_agg(channel, mode), '{}') FROM notif_routing "
                                "WHERE category = '%s';" % category)
    return json.loads(out or '{}')


def activate_fence(cr, sha, run_id, confirm):
    service, category, mode = parse_fence(confirm)
    cr.require_release(sha)
    if mode != 'LEGACY_ONLY' and (service, category) not in FENCE_ROUTABLE:
        raise RuntimeError('Routing fence gate: ' + fence_gate(service, category, mode, 'LEGACY_ONLY', {})
                           + '; nothing changed')
    before = fence_row(cr, service, category)
    spec = FENCE_ROUTABLE.get((service, category)) or {}
    facts = dict(protocol=routing_protocol(cr, service),
                 publications_done=bool(spec) and cr.done(spec['publications']),
                 service_modes=service_modes(cr, spec['service_category']) if mode == 'FENCED_NEW' else {},
                 done=tuple(op for op in spec.get('new_requires', ()) if cr.done(op)),
                 app_urls=app_urls(cr) if mode == 'FENCED_NEW' and spec.get('same_app_url') else {})
    refusal = fence_gate(service, category, mode, before['mode'], facts)
    if refusal:
        raise RuntimeError('Routing fence gate: ' + refusal + '; nothing changed')
    entry = cr.begin('activate-notification-fence', sha, run_id)
    entry['notificationFence'] = dict(service=service, category=category, before=before['mode'], after=mode)
    cr.commit(entry)
    try:
        version = set_fence_mode(cr, service, category, mode, run_id, before['version'])
    except Exception:
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['notificationFence']['version'] = version
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('notificationFence', entry['notificationFence'])


def undo_fence(cr, record, run_id):
    """Rollback of one fence change: back to the recorded previous mode."""
    version = set_fence_mode(cr, record['service'], record['category'], record['before'], run_id)
    cr.emit('notificationFenceRestored', dict(record, restoredVersion=version))


# --- Read-only notification diagnostics (diagnose): counts only, never ids or content ------
PRODUCER_DIAGNOSTICS_SQL = {
    'messaging': (
        "SELECT json_build_object("
        "'fence', (SELECT coalesce(json_agg(json_build_object('category', category, 'mode', mode, "
        "'routingVersion', routing_version) ORDER BY category), '[]') FROM messaging_notification_routing_state), "
        "'decisions', (SELECT coalesce(json_agg(d), '[]') FROM (SELECT route, reason, count(*) AS count "
        "FROM messaging_notification_route_decisions GROUP BY route, reason ORDER BY route, reason) d), "
        "'publications', (SELECT coalesce(json_agg(q), '[]') FROM (SELECT shadow, state, outcome, last_error, "
        "count(*) AS count, min(created_at) AS first, max(created_at) AS last FROM messaging_notification_publications "
        "GROUP BY shadow, state, outcome, last_error ORDER BY shadow, state, outcome, last_error) q), "
        # Why a SHADOW phase may see no decisions: the message events since the fence last
        # changed (by kind and outbox state) and the enabled push registrations (counts only).
        "'messageEventsSinceFenceChange', (SELECT coalesce(json_agg(m), '[]') FROM (SELECT "
        "coalesce(payload->>'message_kind', '') AS kind, status, count(*) AS count FROM messaging_outbox_events "
        "WHERE event_type = 'messaging.message.created' AND occurred_at >= (SELECT changed_at FROM "
        "messaging_notification_routing_state WHERE category = 'MESSAGE_WAKEUPS') GROUP BY 1, 2 ORDER BY 1, 2) m), "
        "'messageEventsLast7Days', (SELECT count(*) FROM messaging_outbox_events WHERE event_type = "
        "'messaging.message.created' AND occurred_at > now() - interval '7 days'), "
        "'enabledPushRegistrations', (SELECT count(*) FROM messaging_push_registrations WHERE enabled), "
        "'pushJobsSinceFenceChange', (SELECT count(*) FROM messaging_push_jobs WHERE created_at >= (SELECT changed_at "
        "FROM messaging_notification_routing_state WHERE category = 'MESSAGE_WAKEUPS')));"),
    # The Meeting fence: modes, decisions by category / route / reason (AUTO_FAILOVER and
    # REROUTED_* show the failover path), publications by state and outcome, and the legacy
    # ledger rows written since each category last changed mode (0 while FENCED_NEW routes all).
    'meeting-service': (
        "SELECT json_build_object("
        "'fence', (SELECT coalesce(json_agg(json_build_object('category', category, 'mode', mode, "
        "'fallback', critical_fallback, 'routingVersion', routing_version) ORDER BY category), '[]') "
        "FROM notification_routing_state), "
        "'decisions', (SELECT coalesce(json_agg(d), '[]') FROM (SELECT category, route, reason, "
        "count(*) FILTER (WHERE ns_completed_at IS NOT NULL) AS completed, count(*) AS count, max(decided_at) AS last "
        "FROM notification_route_decisions GROUP BY 1, 2, 3 ORDER BY 1, 2, 3) d), "
        "'publications', (SELECT coalesce(json_agg(q), '[]') FROM (SELECT category, shadow, state, outcome, last_error, "
        "count(*) AS count, max(created_at) AS last FROM meeting_notification_publications "
        "GROUP BY 1, 2, 3, 4, 5 ORDER BY 1, 2, 3, 4, 5) q), "
        "'legacyEmailsSinceFenceChange', (SELECT coalesce(json_agg(e), '[]') FROM (SELECT r.category, e.status, "
        "count(*) AS count FROM email_deliveries e JOIN notification_routing_state r ON r.category = e.category "
        "AND e.created_at >= r.changed_at GROUP BY 1, 2 ORDER BY 1, 2) e), "
        # The CRITICAL lane (account_notification_outbox, N12): each row's relay state with its
        # decision's lease / acknowledgement / completion flags (counts and error classes only).
        "'criticalOutbox', (SELECT coalesce(json_agg(c), '[]') FROM (SELECT d.category, d.route, d.reason, o.shadow, "
        "o.state, o.outcome, o.last_error, o.settled_at IS NOT NULL AS settled, d.lease_count AS leases, "
        "d.ns_acked_at IS NOT NULL AS acked, d.ns_completed_at IS NOT NULL AS completed, "
        "o.expires_at > now() AS open, count(*) AS count, max(o.created_at) AS last "
        "FROM account_notification_outbox o JOIN notification_route_decisions d ON d.decision_id = o.decision_id "
        "GROUP BY 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 ORDER BY 1, 2, 3, 4, 5, 6, 7) c));"),
}
SERVICE_DIAGNOSTICS_SQL = (
    "SELECT json_build_object("
    "'inbox', (SELECT coalesce(json_agg(i), '[]') FROM (SELECT producer, shadow, state, count(*) AS count, "
    "max(received_at) AS last FROM notif_event_inbox GROUP BY producer, shadow, state ORDER BY producer, shadow, state) i), "
    "'shadowObservations', (SELECT count(*) FROM notif_shadow_observations), "
    "'deliveries', (SELECT count(*) FROM notif_deliveries), "
    # Delivery evidence by channel (counts only): every delivery, its push attempts with the
    # provider's HTTP status class, the email ledger by provider status, the inbox items.
    "'deliveriesByChannel', (SELECT coalesce(json_agg(d), '[]') FROM (SELECT d.channel, n.route_category AS category, "
    "d.status, count(*) AS count, max(coalesce(d.completed_at, d.created_at)) AS last FROM notif_deliveries d JOIN notif_notifications n "
    "ON n.id = d.notification_id GROUP BY 1, 2, 3 ORDER BY 1, 2, 3) d), "
    "'pushAttempts', (SELECT coalesce(json_agg(a), '[]') FROM (SELECT a.outcome, (a.http_status / 100) || 'xx' AS http, "
    "count(*) AS count FROM notif_delivery_attempts a JOIN notif_deliveries d ON d.id = a.delivery_id "
    "WHERE d.channel = 'PUSH' GROUP BY 1, 2 ORDER BY 1, 2) a), "
    "'emailMessages', (SELECT coalesce(json_agg(e), '[]') FROM (SELECT category, lane, status, provider, "
    "last_error_class, last_error_status, route_decision_id IS NOT NULL AS decided, max(attempts) AS attempts, "
    "count(*) AS count, max(updated_at) AS last FROM notif_email_messages GROUP BY 1, 2, 3, 4, 5, 6, 7 "
    "ORDER BY 1, 2, 3, 4) e), "
    "'emailProviderEvents', (SELECT count(*) FROM notif_email_provider_events), "
    "'inboxItems', (SELECT coalesce(json_agg(i), '[]') FROM (SELECT category, count(*) AS count FROM notif_inbox_items "
    "GROUP BY 1 ORDER BY 1) i), "
    "'pushEndpoints', (SELECT count(*) FROM notif_push_endpoints), "
    "'routingActive', (SELECT coalesce(json_agg(r), '[]') FROM (SELECT category, channel FROM notif_routing "
    "WHERE mode = 'ACTIVE' ORDER BY 1, 2) r), "
    "'routingNotOff', (SELECT count(*) FROM notif_routing WHERE mode <> 'OFF'));")


def notification_diagnostics(cr):
    """Producer fence / publication counts and the service's inbox counts (read-only)."""
    out = {}
    for service, sql in PRODUCER_DIAGNOSTICS_SQL.items():
        try:
            out[service] = json.loads(_producer_sql(cr, service)(sql) or 'null')
        except (RuntimeError, ValueError):
            out[service] = 'unavailable'
    try:
        out['notifications'] = json.loads(cr.release.db_sql(cr.SERVICES['notifications']['db'],
                                                            SERVICE_DIAGNOSTICS_SQL) or 'null')
    except (RuntimeError, ValueError):
        out['notifications'] = 'unavailable'
    cr.emit('notificationDiagnostics', out)
    return out


# --- N5 / N11: the Notification Service's own Resend provider ---------------------------------
RESEND_SOURCES = ('resend-api-key', 'resend-webhook-secret')
NOTIFICATIONS_RESEND_TARGETS = {'resend-api-key': '/run/secrets/resend-api-key',
                                'resend-webhook-secret': '/run/secrets/resend-webhook-secret'}
NOTIFICATIONS_EMAIL_FROM = 'CF Meet <no-reply@cfmeet.com>'
PROVIDER_CALLBACK = '/api/v1/notifications/provider-events/resend'
SECRET_FORMS = {'resend-api-key': re.compile(r're_[A-Za-z0-9_]{8,200}'),
                'resend-webhook-secret': re.compile(r'whsec_[A-Za-z0-9+/=]{16,200}')}


def check_root_secret(path, form):
    """The original VPS secret: a regular root-owned file, no group/other access, one line of the
    expected form. Nothing about the value is returned or printed."""
    import os
    import stat
    st = os.lstat(path)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != 0 or st.st_mode & 0o077:
        raise RuntimeError('%s must be a regular root-owned file without group/other access' % path.name)
    text = path.read_text().strip()
    if '\n' in text or not form.fullmatch(text):
        raise RuntimeError('%s does not hold a value of the expected form' % path.name)


def service_copy(cr, source, owner_uid):
    """A 0400 copy owned by the service user (the replicas do not run as root), refreshed from
    the root-only original on every activation. The value is never printed."""
    import os
    target = source.with_name('notifications-' + source.name)
    cr.p.write_private(target, source.read_text().strip())
    os.chown(target, owner_uid, owner_uid)
    os.chmod(target, 0o400)
    return target


def email_changes(cr):
    uid = cr.SERVICES['notifications']['uid']
    mounts, overrides = [], {'NOTIFICATIONS_EMAIL_PROVIDER': 'resend', 'NOTIFICATIONS_EMAIL_FROM': NOTIFICATIONS_EMAIL_FROM}
    for name in RESEND_SOURCES:
        source = cr.p.SECRETS / name
        check_root_secret(source, SECRET_FORMS[name])
        mounts.append((service_copy(cr, source, uid), NOTIFICATIONS_RESEND_TARGETS[name]))
    overrides['NOTIFICATIONS_RESEND_API_KEY_FILE'] = NOTIFICATIONS_RESEND_TARGETS['resend-api-key']
    overrides['NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE'] = NOTIFICATIONS_RESEND_TARGETS['resend-webhook-secret']
    return overrides, mounts


# The replicas live on cfmeet-core-private, which is --internal (no route out). Their providers
# (Resend, the browser push services) are public HTTPS endpoints: like the webhook worker, the
# replicas also join a dedicated non-internal bridge without inter-container traffic. Production
# 2026-11-17: without it every send failed with NETWORK and the leased CRITICAL codes expired.
NOTIFICATIONS_EGRESS_NET = 'cfmeet-notifications-egress'
RESEND_PROBE_URL = 'https://api.resend.com/'
PUSH_PROBE_URLS = ('https://fcm.googleapis.com/', 'https://updates.push.services.mozilla.com/',
                   'https://web.push.apple.com/')


def egress_network(cr):
    if NOTIFICATIONS_EGRESS_NET not in cr.p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
        cr.p.run('docker', 'network', 'create', '--driver', 'bridge', '--opt',
                 'com.docker.network.bridge.enable_icc=false', NOTIFICATIONS_EGRESS_NET)
    return NOTIFICATIONS_EGRESS_NET


def egress_status(cr, container, url):
    """HTTP status a GET to a public provider answers from inside `container`'s network
    namespace ('000' when unreachable). Any HTTP answer proves the route out."""
    try:
        out = cr.p.run('docker', 'run', '--rm', '--network', 'container:' + container, cr.PROBE_IMAGE, '-s',
                       '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10', url)
    except Exception:  # curl exits non-zero when it cannot connect
        return '000'
    return (out or '').strip() or '000'


def require_egress(cr, urls, kind):
    """Every replica reaches every provider URL, or the activation is restored."""
    statuses = {replica: {url: egress_status(cr, cr.SERVICES[replica]['container'], url) for url in urls}
                for replica in REPLICAS}
    cr.emit('notificationsEgress', {'kind': kind, 'statuses': statuses})
    unreachable = sorted('%s %s' % (r, u) for r, by in statuses.items() for u, code in by.items()
                         if not re.fullmatch(r'[1-5][0-9][0-9]', code))
    if unreachable:
        raise RuntimeError('The Notification Service cannot reach its %s provider: %s' % (kind, ', '.join(unreachable)))


def activate_email(cr, sha, run_id, confirm):
    cr.require_release(sha)
    if not cr.done('activate-resend-email'):
        raise RuntimeError('Gate E1 first: activate-resend-email (the provider account and domain proven)')
    notif_tls = cr.SERVICES['notifications']['tls']
    cr.tls_route_present(notif_tls)
    callback = cr.private_url(notif_tls) + PROVIDER_CALLBACK
    # Before: the callback is not configured (404) or already is (401 to an unsigned event).
    before = private_status(cr, callback)
    if before not in ('404', '401'):
        raise RuntimeError('The Notification Service provider callback answered %s before activation' % before)
    overrides, mounts = email_changes(cr)
    network = egress_network(cr)
    cr.emit('notificationsEmail', {'provider': 'resend', 'sender': NOTIFICATIONS_EMAIL_FROM,
                                   'webhookSigning': True, 'replicas': list(REPLICAS), 'egressNetwork': network})

    def post():
        # The webhook secret is loaded: an unsigned event is refused (401), never 404.
        status = private_status(cr, callback)
        if status != '401':
            raise RuntimeError('The provider callback answers %s after activation (expected 401 unsigned)' % status)
        require_egress(cr, (RESEND_PROBE_URL,), 'email')

    cr.apply_activation('activate-notifications-email', sha, run_id,
                        [(replica, dict(overrides), {'mounts': list(mounts), 'networks': [network]})
                         for replica in REPLICAS], post=post)


# --- N10: preference copy and the Hub facade switch ---------------------------------------------
PREFERENCE_VERIFY_SECONDS = 600
PREFERENCE_KINDS = ('ACCOUNT', 'CONVERSATION', 'CONTACT')


def _hub_sql(cr):
    return lambda q: cr.release.db_sql(cr.SERVICES['communications']['db'], q)


def _ns_sql(cr):
    return lambda q: cr.release.db_sql(cr.SERVICES['notifications']['db'], q)


def copy_state(cr):
    out = _ns_sql(cr)("SELECT row_to_json(c) FROM (SELECT mode, phase, snapshots_completed, rows_written, "
                      "rows_unchanged, rows_skipped, changes_applied, last_error, verify_requested_at, verified_at, "
                      "verification FROM notif_preference_copy WHERE singleton) c;")
    return json.loads(out or 'null')


def hub_authority(cr):
    out = _hub_sql(cr)("SELECT row_to_json(a) FROM (SELECT mode, version FROM communication_preference_authority "
                       "WHERE singleton) a;")
    return json.loads(out or 'null')


def set_hub_authority(cr, mode, run_id, expected):
    assert mode in ('HUB', 'FREEZE', 'SERVICE') and re.fullmatch(r'[0-9-]{1,32}', run_id)
    guard = '' if expected is None else " AND mode = '%s'" % expected
    out = _hub_sql(cr)("WITH u AS (UPDATE communication_preference_authority SET mode = '%s', version = version + 1, "
                       "changed_by_run = '%s', changed_at = now() WHERE singleton%s RETURNING mode) "
                       "SELECT count(*) FROM u;" % (mode, run_id, guard))
    if (out or '').strip() != '1':
        raise RuntimeError('The Hub preference authority changed concurrently; nothing applied')


def set_copy_mode(cr, mode, run_id):
    assert mode in ('OFF', 'RUN') and re.fullmatch(r'[0-9-]{1,32}', run_id)
    _ns_sql(cr)("UPDATE notif_preference_copy SET mode = '%s', version = version + 1, changed_by_run = '%s', "
                "updated_at = now() WHERE singleton;" % (mode, run_id))


def activate_preference_copy(cr, sha, run_id, confirm):
    """Grants the export / facade permissions and starts the copy + catch-up (W7). The Hub
    stays the preference authority (mode HUB); nothing a user sees changes."""
    cr.require_release(sha)
    for op in ('activate-notifications-owners', 'activate-communications-publications'):
        if not cr.done(op):
            raise RuntimeError('The preference copy needs %s first' % op)
    if not copy_state(cr):
        raise RuntimeError('The Notification Service has no preference copy control row; release it first')
    if not hub_authority(cr):
        raise RuntimeError('The Hub has no preference authority row; release it first')

    def post():
        set_copy_mode(cr, 'RUN', run_id)
        cr.emit('preferenceCopy', {'mode': 'RUN', 'state': copy_state(cr)})

    cr.apply_activation('activate-preference-copy', sha, run_id, [], post=post)


def verification_complete(doc):
    if not isinstance(doc, dict) or doc.get('complete') is not True:
        return False
    for kind in PREFERENCE_KINDS:
        k = doc.get(kind) or doc.get(kind.lower()) or {}
        if not k.get('complete') or k.get('different') or k.get('skipped') \
                or not (k.get('equal') == k.get('hubRows') == k.get('serviceRows')):
            return False
    return True


def activate_preference_authority(cr, sha, run_id, confirm, sleep=None, clock=None):
    """Freeze the Hub preference writes, run the final copy pass and verification, and move the
    Hub endpoints to the Notification Service only when every kind matches exactly; otherwise
    back to HUB. Journaled; rollback sets HUB."""
    import time
    sleep = sleep or time.sleep
    clock = clock or time.monotonic
    cr.require_release(sha)
    if not cr.done('activate-preference-copy'):
        raise RuntimeError('activate-preference-copy first')
    state = copy_state(cr)
    if not state or state.get('mode') != 'RUN' or state.get('phase') != 'CATCH_UP' \
            or (state.get('snapshots_completed') or 0) < 1 or state.get('last_error'):
        raise RuntimeError('The preference copy is not caught up (needs RUN, CATCH_UP, one snapshot, no error): %s'
                           % json.dumps({k: state.get(k) for k in ('mode', 'phase', 'snapshots_completed',
                                                                   'last_error')} if state else None))
    if (hub_authority(cr) or {}).get('mode') != 'HUB':
        raise RuntimeError('The Hub preference authority is not HUB')
    entry = cr.begin('activate-preference-authority', sha, run_id)
    entry['preferenceAuthority'] = dict(before='HUB', after='SERVICE')
    cr.commit(entry)
    try:
        set_hub_authority(cr, 'FREEZE', run_id, 'HUB')
        _ns_sql(cr)("UPDATE notif_preference_copy SET verify_requested_at = clock_timestamp() WHERE singleton;")
        deadline = clock() + PREFERENCE_VERIFY_SECONDS
        while True:
            state = copy_state(cr)
            if state.get('verified_at') and state.get('verify_requested_at') \
                    and state['verified_at'] >= state['verify_requested_at']:
                break
            if clock() >= deadline:
                raise RuntimeError('The final preference pass did not finish in %ds' % PREFERENCE_VERIFY_SECONDS)
            sleep(5)
        cr.emit('preferenceVerification', state.get('verification'))
        if not verification_complete(state.get('verification')):
            raise RuntimeError('Preference verification did not match exactly; the Hub stays the authority')
        set_hub_authority(cr, 'SERVICE', run_id, 'FREEZE')
        set_copy_mode(cr, 'OFF', run_id)
    except Exception:
        try:
            set_hub_authority(cr, 'HUB', run_id, None)
        finally:
            entry['status'] = 'failed'
            cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('preferenceAuthority', {'mode': 'SERVICE'})


def undo_preference_authority(cr, record, run_id):
    set_hub_authority(cr, 'HUB', run_id, None)
    cr.emit('preferenceAuthorityRestored', {'mode': 'HUB'})


# --- N12: the Account CRITICAL lane ------------------------------------------------------------
def fallback_key(cr):
    """The Account fallback key (32 bytes, base64), generated once on the VPS for the Meeting
    Service only (never on the Notification Service, never regenerated over an existing key:
    rows sealed with it must still open). Never printed."""
    import base64
    import os
    import secrets
    path = cr.p.SECRETS / CRITICAL_FALLBACK_KEY
    if not path.exists():
        cr.p.write_private(path, base64.b64encode(secrets.token_bytes(32)).decode())
    os.chmod(path, 0o400)
    return path


def meeting_health(cr):
    """Meeting /health as JSON (routingProtocol, criticalFallbackKey)."""
    s = cr.SERVICES['meeting-service']
    out = cr.p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'], cr.PROBE_IMAGE,
                   '-sS', '--max-time', '4', 'http://127.0.0.1:%d%s' % (s['port'], s['health']))
    try:
        return json.loads(out or '{}')
    except ValueError:
        return {}


def activate_critical_lane(cr, sha, run_id, confirm):
    """Grants X6 / X7 (account-route-lease.claim) to the service, mounts the Account fallback key
    on the Meeting Service, proves the public host refuses the lease routes. Nothing changes for
    users: every CRITICAL category stays where it is until an activate-notification-fence run."""
    cr.require_release(sha)
    for op in ('activate-meeting-publications', 'activate-notifications-email'):
        if not cr.done(op):
            raise RuntimeError('The critical lane needs %s first' % op)
    require_fence_protocol(cr, 'meeting-service')
    deny_owner_routes_publicly(cr)
    key = fallback_key(cr)
    meeting_app_url = app_urls(cr)['meeting-service']
    if not meeting_app_url.startswith('https://'):
        raise RuntimeError('The Meeting Service APP_URL is not an https URL; nothing changed')

    def post():
        health = meeting_health(cr)
        if health.get('criticalFallbackKey') is not True or not isinstance(health.get('routingProtocol'), int):
            raise RuntimeError('The Meeting Service does not report its fallback key and routing protocol '
                               '(criticalFallbackKey %s, routingProtocol %s)'
                               % (health.get('criticalFallbackKey'), health.get('routingProtocol')))
        cr.emit('criticalLane', {'criticalFallbackKey': True, 'routingProtocol': health['routingProtocol']})

    # The service builds the sign-in magic link from NOTIFICATIONS_APP_URL: the same web app.
    cr.apply_activation('activate-critical-lane', sha, run_id,
                        [('meeting-service', {'CF_ACCOUNT_FALLBACK_KEY_FILE': CRITICAL_FALLBACK_TARGET},
                          {'mounts': [(key, CRITICAL_FALLBACK_TARGET)]})]
                        + [(replica, {'NOTIFICATIONS_APP_URL': meeting_app_url}, {}) for replica in REPLICAS],
                        post=post)


# --- N9: push --------------------------------------------------------------------------------
def notifications_ready(cr, name='notifications'):
    s = cr.SERVICES[name]
    out = cr.p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'], cr.PROBE_IMAGE,
                   '-sS', '--max-time', '4', 'http://127.0.0.1:%d%s' % (s['port'], s['health']))
    try:
        return json.loads(out or '{}')
    except ValueError:
        return {}


def activate_push(cr, sha, run_id, confirm):
    """The service's live push provider with Messaging's existing VAPID key (never a new key:
    every browser subscription was made with it). A service-owned 0400 copy of the key file,
    mounted read-only on both replicas. APNs / FCM stay unconfigured (no credentials)."""
    import messaging_runtime
    import os
    cr.require_release(sha)
    if not cr.done('activate-web-push'):
        raise RuntimeError('No Messaging VAPID key yet: activate-web-push first')
    source = messaging_runtime.WORK / cr.WEB_PUSH_KEY_NAME
    if not source.is_file() or source.stat().st_mode & 0o077:
        raise RuntimeError('The Messaging VAPID key file is missing or unsafe')
    if not source.read_text().lstrip().startswith('-----BEGIN PRIVATE KEY-----'):
        raise RuntimeError('The Messaging VAPID key is not a PKCS#8 PEM')
    uid = cr.SERVICES['notifications']['uid']
    copy = cr.p.SECRETS / 'notifications-web-push-vapid.pem'
    cr.p.write_private(copy, source.read_text())
    os.chown(copy, uid, uid)
    os.chmod(copy, 0o400)
    overrides = {'NOTIFICATIONS_PUSH_PROVIDER': 'live', 'NOTIFICATIONS_PUSH_WEB_VAPID_KEY_FILE': WEB_PUSH_TARGET,
                 'NOTIFICATIONS_PUSH_WEB_SUBJECT': WEB_PUSH_SUBJECT}
    allowed = cr.env_of('messaging').get('MESSAGING_PUSH_WEB_ALLOWED_HOSTS')
    if allowed:
        overrides['NOTIFICATIONS_PUSH_WEB_ALLOWED_HOSTS'] = allowed

    network = egress_network(cr)

    def post():
        for replica in REPLICAS:
            key = notifications_ready(cr, replica).get('webPushApplicationServerKey')
            if not key:
                raise RuntimeError('%s reports no Web Push application server key after activation' % replica)
        require_egress(cr, PUSH_PROBE_URLS, 'push')
        cr.emit('notificationsPush', {'provider': 'live', 'webPushApplicationServerKey': key, 'egressNetwork': network})

    cr.apply_activation('activate-notifications-push', sha, run_id,
                        [(replica, dict(overrides), {'mounts': [(copy, WEB_PUSH_TARGET)], 'networks': [network]})
                         for replica in REPLICAS], post=post)


def activate_messaging_push_mirror(cr, sha, run_id, confirm):
    """Messaging copies its push registrations of CF Meet first-party accounts to the service
    (I13) and keeps them in sync; its own registrations and push stay unchanged."""
    cr.require_release(sha)
    for op in ('activate-notifications-push', 'activate-messaging-publications'):
        if not cr.done(op):
            raise RuntimeError('The registration mirror needs %s first' % op)
    env = cr.env_of('messaging')
    missing = [k for k in ('CF_NOTIFICATIONS_BASE_URL', 'CF_COMMUNICATIONS_PERSONAL_TENANT_ID',
                           'CF_COMMUNICATIONS_PERSONAL_APPLICATION_ID', 'CF_COMMUNICATIONS_PERSONAL_ENVIRONMENT_ID')
               if not env.get(k)]
    if missing:
        raise RuntimeError('Messaging lacks %s' % ', '.join(missing))
    cr.apply_activation('activate-messaging-push-mirror', sha, run_id,
                        [('messaging', {'MESSAGING_PUSH_REGISTRATION_MIRROR': 'on'})])


def activate_calling_push(cr, sha, run_id, confirm):
    """Calling's incoming-call alerts through the service's fast lane (I3), first-party
    recipients only, fire-and-forget (ringing never waits)."""
    cr.require_release(sha)
    for op in ('activate-notifications-push', 'activate-calling-publications'):
        if not cr.done(op):
            raise RuntimeError('Call push needs %s first' % op)
    if not cr.env_of('calling').get('CF_NOTIFICATIONS_BASE_URL'):
        raise RuntimeError('Calling has no CF_NOTIFICATIONS_BASE_URL')
    cr.apply_activation('activate-calling-push', sha, run_id,
                        [('calling', {'CALLING_PUSH_TRANSPORT': 'notifications'})])
