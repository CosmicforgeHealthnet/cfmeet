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

* ``activate-notification-fence`` changes ONE producer routing-fence row per run (Document 4
  §9.6). ``confirm`` is ``<service>-<category>-<mode>`` in lower case with hyphens, e.g.
  ``messaging-message-wakeups-shadow``. This version moves only between ``LEGACY_ONLY`` and
  ``SHADOW`` (no route can become NEW, so the legacy path keeps delivering everything), only
  for categories whose producer code records decisions (``FENCE_ROUTABLE``), only while the
  producer image reports ``routingProtocol`` (M-12). Journaled; ``rollback`` restores it.
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
    if category in CRITICAL:
        return ('%s is CRITICAL: ACTIVE needs gates E3/E4, a fallback drill within 7 days and live alerting '
                '(N12); this version refuses it' % category)
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
                 owners_done=cr.done('activate-notifications-owners'))
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
FENCE_ROUTABLE = {('messaging', 'MESSAGE_WAKEUPS'): 'activate-messaging-publications'}
FENCE_MODES = {'legacy-only': 'LEGACY_ONLY', 'shadow': 'SHADOW'}


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
    guard = '' if expected_version is None else ' AND routing_version = %d' % int(expected_version)
    out = _producer_sql(cr, service)(
        "WITH u AS (UPDATE %s SET mode = '%s', canary_percent = 0, routing_version = routing_version + 1, "
        "changed_by_run = '%s', changed_at = now() WHERE category = '%s'%s RETURNING routing_version) "
        "SELECT count(*) || ':' || coalesce(max(routing_version), 0) FROM u;"
        % (table, mode, run_id, category, guard))
    count, version = (out or '0:0').strip().split(':')
    if count != '1':
        raise RuntimeError('Routing fence row %s/%s changed concurrently; nothing applied' % (service, category))
    return int(version)


def fence_gate(service, category, mode, before, facts):
    """Refusal reason, or None. ``facts``: protocol (routingProtocol of the running image),
    publications_done (the producer's publication activation is recorded)."""
    if before not in FENCE_MODES.values():
        return ('%s/%s is %s; this version only moves between LEGACY_ONLY and SHADOW'
                % (service, category, before))
    if mode == 'LEGACY_ONLY':
        return None
    if (service, category) not in FENCE_ROUTABLE:
        return ('%s/%s has no producer code that records routing decisions; it must stay LEGACY_ONLY'
                % (service, category))
    if not isinstance(facts.get('protocol'), int) or facts['protocol'] < 1:
        return '%s reports no routingProtocol; release a fence-capable image first' % service
    if not facts.get('publications_done'):
        return 'SHADOW needs %s first' % FENCE_ROUTABLE[(service, category)]
    return None


def activate_fence(cr, sha, run_id, confirm):
    service, category, mode = parse_fence(confirm)
    cr.require_release(sha)
    if mode != 'LEGACY_ONLY' and (service, category) not in FENCE_ROUTABLE:
        raise RuntimeError('Routing fence gate: ' + fence_gate(service, category, mode, 'LEGACY_ONLY', {})
                           + '; nothing changed')
    before = fence_row(cr, service, category)
    prerequisite = FENCE_ROUTABLE.get((service, category))
    facts = dict(protocol=routing_protocol(cr, service),
                 publications_done=bool(prerequisite) and cr.done(prerequisite))
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
        "GROUP BY shadow, state, outcome, last_error ORDER BY shadow, state, outcome, last_error) q));"),
}
SERVICE_DIAGNOSTICS_SQL = (
    "SELECT json_build_object("
    "'inbox', (SELECT coalesce(json_agg(i), '[]') FROM (SELECT producer, shadow, state, count(*) AS count, "
    "max(received_at) AS last FROM notif_event_inbox GROUP BY producer, shadow, state ORDER BY producer, shadow, state) i), "
    "'shadowObservations', (SELECT count(*) FROM notif_shadow_observations), "
    "'deliveries', (SELECT count(*) FROM notif_deliveries), "
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
