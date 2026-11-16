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
