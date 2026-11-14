"""Coordinated production release of every CF Meet backend service from one main SHA.

Dispatched only by .github/workflows/cfmeet-coordinated-release.yml (main, green CI, one
concurrency group with every other production change). Runs on the VPS as root:

    python3 coordinated_release.py <operation> <release sha> <run id> [confirmation]

Operations
  inspect                      read-only: resources, CF Meet containers, migration versions,
                               feature flags (non-secret values only), mail presence.
  backup                       pg_dump -Fc of every CF Meet database plus the Meeting Service
                               database, sha256, pg_restore --list, isolated restore tests.
  provision                    idempotent: databases/roles, service keys, trust bundles and
                               private TLS routes of the services that are new to production.
  release                      gates -> backup -> provision -> per service, in dependency
                               order: migrations (migrator role) -> swap -> health -> image check.
  verify                       images == release SHA, health, migration histories, safe
                               provider defaults, public smoke, Organizations delivery counts.
                               Strict: only a SHA with a completed coordinated release.
  certify                      read-only live-production certification of the deployed
                               topology, whatever revisions it runs (live_certification.py).
  rollback                     restores the containers of the latest journal entry (a release
                               or one activation). Migrations are never rolled back.
  activate-* / org-cutover-*   separately approved activation steps (see ACTIVATIONS).

Secrets (database URLs, passwords, keys) stay on the VPS: they are written only to 0600
files under root-only directories and are never printed. Only containers this module owns
are ever stopped, renamed, created or removed (`owned`); other workloads on the host
(cosmicforge-*, cfmeet-staging-*, LiveKit) are never touched.
"""
import base64
import copy
import datetime
import hashlib
import json
import os
import re
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit, quote

import production as p
import communications_release as release
import email_operations as mail_ops
import developer_access
import org_first_activation

WORK = p.ROOT / 'coordinated'
BACKUPS = p.ROOT / 'backups'
JOURNAL = WORK / 'journal.json'
CUTOVER = WORK / 'org-cutover'
TLS_CONF = p.ROOT / 'tls-proxy' / 'nginx.conf'
IMAGE_REPO = 'ghcr.io/cosmicforgehealthnet/cfmeet-'
PROBE_IMAGE = 'curlimages/curl:8.12.1'
PG_CLIENT = 'postgres:16-alpine'
MEETING = 'cfmeet-production-backend'
PUBLIC = 'https://api.cfmeet.com'
WEB_ORIGIN = 'https://cfmeet.com'
# Every browser origin of the production web app: cfmeet.com redirects to www.cfmeet.com
# (Vercel), so the browser's Origin is normally the www form.
WEB_ORIGINS = (WEB_ORIGIN, 'https://www.cfmeet.com')
SHA_RE = re.compile(r'[a-f0-9]{40}')
RUN_RE = re.compile(r'[0-9]{1,20}(-[0-9]{1,4})?')  # run id, optionally -attempt


def private_url(port):
    """Internal HTTPS URL through the private TLS proxy (Docker alias api.cfmeet.com)."""
    return 'https://api.cfmeet.com:%d' % port


# --- Service catalogue -------------------------------------------------------------------

def _service(container, port, health, *, tls=None, db=None, role=None, migrations=(),
             history=(), memory=None, uid=10001, image=None, identity=None, command=(),
             existing=False, env=None):
    return dict(container=container, port=port, health=health, tls=tls, db=db, role=role,
                migrations=tuple(migrations), history=tuple(history), memory=memory, uid=uid,
                image=image, identity=identity, command=list(command), existing=existing,
                env=dict(env or {}))


SERVICES = {
    'platform-access': _service('cfmeet-platform-access', 8094, '/ready', tls=8443,
        db='cfmeet_platform_access', role='platform_access', migrations=['platform-access-migration'],
        history=['platform_access_seaql_migrations'], existing=True),
    'organizations': _service('cfmeet-organizations', 8101, '/ready', tls=8450,
        db='cfmeet_organizations', role='organizations', migrations=['organizations-migration'],
        history=['organizations_seaql_migrations'], memory=192,
        env={'ORGANIZATIONS_BIND_ADDRESS': '0.0.0.0:8101', 'ORGANIZATIONS_PROVISIONING_ENABLED': 'false'}),
    'messaging': _service('cfmeet-messaging', 8097, '/ready', tls=8444, db='cfmeet_messaging',
        role='messaging', migrations=['messaging-migration', 'messaging-communications-migration'],
        history=['messaging_seaql_migrations'], existing=True),
    'calling': _service('cfmeet-calling', 8090, '/health', tls=8445, db='cfmeet_calling', role='calling',
        migrations=['calling-migration', 'calling-communications-migration'],
        history=['calling_seaql_migrations'], existing=True),
    'communications': _service('cfmeet-communications', 8080, '/ready', tls=8447,
        db='cfmeet_comm_production_hub', migrations=['communications-migration'],
        history=['communications_seaql_migrations'], existing=True),
    'numbering': _service('cfmeet-numbering', 8095, '/ready', tls=8451, db='cfmeet_numbering',
        role='numbering', migrations=['numbering-migration'], history=['numbering_seaql_migrations'],
        memory=192, env={'NUMBERING_BIND_ADDRESS': '0.0.0.0:8095', 'NUMBERING_PORTING_ENABLED': 'false'}),
    'privacy-relay': _service('cfmeet-privacy-relay', 8091, '/health', tls=8452, db='cfmeet_privacy_relay',
        role='privacy_relay', migrations=['privacy-relay-migration'], history=['privacy_relay_seaql_migrations'],
        memory=192, env={'PRIVACY_RELAY_BIND_ADDRESS': '0.0.0.0:8091', 'PRIVACY_RELAY_ENABLED': 'false',
                         'CONTACT_VAULT_ENABLED': 'false', 'RELAY_NUMBER_POOL_ENABLED': 'false'}),
    'sms-gateway': _service('cfmeet-sms-gateway', 8096, '/ready', tls=8453, db='cfmeet_sms_gateway',
        role='sms_gateway', migrations=['sms-gateway-migration'], history=['sms_gateway_seaql_migrations'],
        memory=192, env={'SMS_GATEWAY_BIND_ADDRESS': '0.0.0.0:8096',
                         'SMS_GATEWAY_CONTENT_KEY_ID': 'content-production-1',
                         'SMS_GATEWAY_CONTENT_KEY_FILE': '/run/secrets/sms-content.key',
                         'SMS_GATEWAY_FINGERPRINT_KEY_FILE': '/run/secrets/sms-fingerprint.key'}),
    'telephony': _service('cfmeet-telephony', 8092, '/health', tls=8454, db='cfmeet_telephony',
        role='telephony', migrations=['telephony-migration'], history=['telephony_seaql_migrations'],
        memory=192, uid=10002, env={'TELEPHONY_BIND_ADDRESS': '0.0.0.0:8092', 'TELEPHONY_ENABLED': 'false',
            'PUBLIC_OUTBOUND_CALLING_ENABLED': 'false', 'PUBLIC_INBOUND_CALLING_ENABLED': 'false',
            'CALLBACK_RELAY_ENABLED': 'false', 'ENTERPRISE_SIP_ENABLED': 'false', 'TELEPHONY_SIP_EDGE': 'none'}),
    'meeting-service': _service(MEETING, 8080, '/health', tls=8448,
        migrations=['migration', 'account-security-migration', 'meetings-calendar-migration',
                    'profile-notifications-migration', 'account-hardening-migration',
                    'organization-integration-migration', 'account-communications-migration',
                    'lifecycle-delivery-migration'],
        history=['seaql_migrations'], existing=True),
    'developer-platform': _service('cfmeet-developer-platform', 8098, '/ready', tls=8455,
        db='cfmeet_developer_platform', role='developer_platform', migrations=['developer-platform-migration'],
        history=['developer_platform_seaql_migrations'], memory=256,
        env={'DEVELOPER_PLATFORM_BIND_ADDRESS': '0.0.0.0:8098', 'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'false',
             'DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED': 'false',
             'DEVELOPER_PLATFORM_WEBHOOKS_ENABLED': 'false'}),
    # Same image, identity, database and trust as the API; no listener (health = stays running).
    'developer-platform-worker': _service('cfmeet-developer-platform-worker', None, None,
        db='cfmeet_developer_platform', role='developer_platform', image='developer-platform',
        identity='developer-platform', command=['./developer-platform-worker'], memory=128,
        env={'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'false',
             'DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED': 'false',
             'DEVELOPER_PLATFORM_WEBHOOKS_ENABLED': 'false',
             'DEVELOPER_PLATFORM_WORKER_INSTANCE_ID': 'worker-production-1'}),
    # No database, no trust bundle (it receives no internal call); public only after
    # activate-developer-platform + activate-gateway-routing.
    'api-gateway': _service('cfmeet-api-gateway', 8099, '/ready', memory=192,
        env={'API_GATEWAY_BIND_ADDRESS': '0.0.0.0:8099', 'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'false',
             'API_GATEWAY_WEBSOCKETS_ENABLED': 'false', 'API_GATEWAY_CORS_ALLOWED_ORIGINS': ','.join(WEB_ORIGINS)}),
}

# Owners before callers (final-production-activation-runbook §1).
ORDER = ('platform-access', 'organizations',
         'messaging', 'calling', 'communications', 'numbering', 'privacy-relay', 'sms-gateway', 'telephony',
         'meeting-service', 'developer-platform', 'developer-platform-worker', 'api-gateway')
DEPENDS = {
    'organizations': ('platform-access',),
    'messaging': ('platform-access',), 'calling': ('platform-access',),
    'communications': ('platform-access',), 'numbering': ('platform-access',),
    'privacy-relay': ('platform-access',), 'telephony': ('platform-access',),
    'sms-gateway': ('platform-access', 'numbering'),
    'meeting-service': ('platform-access', 'organizations', 'messaging', 'calling', 'communications'),
    'developer-platform': ('platform-access', 'meeting-service', 'calling'),
    'developer-platform-worker': ('developer-platform',),
    'api-gateway': ('platform-access', 'developer-platform'),
}
IMAGES = ('platform-access', 'organizations', 'messaging', 'calling', 'communications', 'numbering',
          'privacy-relay', 'sms-gateway', 'telephony', 'meeting-service', 'developer-platform',
          'api-gateway', 'security-scan-gateway')

# --- Resource gate -----------------------------------------------------------------------

SAFETY_MARGIN_MIB = 1024
# One migration / probe container at a time plus the isolated restore-test database.
TRANSIENT_MIB = 256 + 64 + 512


def mem_available_mib(meminfo=None):
    text = meminfo if meminfo is not None else Path('/proc/meminfo').read_text()
    for line in text.splitlines():
        if line.startswith('MemAvailable:'):
            return int(line.split()[1]) // 1024
    raise RuntimeError('MemAvailable missing from /proc/meminfo')


def required_mib(new_services):
    """Memory the release adds: the limits of containers that do not exist yet. A
    replaced container keeps its own limit and its predecessor is stopped first."""
    return sum(SERVICES[name]['memory'] for name in new_services) + TRANSIENT_MIB


def resource_gate(available_mib, required, margin=SAFETY_MARGIN_MIB):
    remaining = available_mib - required
    result = dict(memAvailableMiB=available_mib, requiredMiB=required, safetyMarginMiB=margin,
                  remainingAfterReleaseMiB=remaining, shortfallMiB=max(0, margin - remaining))
    if remaining < margin:
        raise RuntimeError('Resource gate: %d MiB short (available %d, required %d, margin %d); nothing changed'
                           % (margin - remaining, available_mib, required, margin))
    return result


def disk_free_bytes(path='/'):
    st = os.statvfs(path)
    return st.f_bavail * st.f_frsize


# --- Container ownership -----------------------------------------------------------------

_SERVICE_NAMES = '|'.join(re.escape(s['container'].removeprefix('cfmeet-')) for s in SERVICES.values())
OWNED_RE = re.compile(r'^cfmeet-(%s|gateway-ingress)(-(rollback|failed|rolled-back)-[A-Za-z0-9-]+)?$' % _SERVICE_NAMES)
RESTORE_RE = re.compile(r'^cfmeet-restore-test-[0-9]+(-[0-9]+)?-[a-z][a-z0-9-]*$')
EXEC_ONLY = ('cfmeet-core-db', 'cfmeet-core-tls')
FOREIGN_PREFIXES = ('cosmicforge-', 'cfmeet-staging-', 'lk-server')


def owned(name, exec_only=False):
    if name.startswith(FOREIGN_PREFIXES):
        return False
    if exec_only and name in EXEC_ONLY:
        return True
    return bool(OWNED_RE.match(name) or RESTORE_RE.match(name))


def _own(name, exec_only=False):
    if not owned(name, exec_only):
        raise RuntimeError('Refusing to operate on a container this release does not own: ' + name)


def stop(name):
    _own(name)
    p.run('docker', 'stop', '--time', '30', name)


def start(name):
    _own(name)
    p.run('docker', 'start', name)


def rename(old, new):
    _own(old)
    _own(new)
    p.run('docker', 'rename', old, new)


def remove(name):
    _own(name)
    p.run('docker', 'rm', '-f', name)


def container_names():
    return p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()


# --- Never print secrets -----------------------------------------------------------------

FLAGS = (
    'CF_RUNTIME_ENVIRONMENT', 'ORGANIZATIONS_PROVISIONING_ENABLED', 'ORGANIZATIONS_EVENT_DESTINATIONS',
    'ORGANIZATIONS_AUTHORITY_MODE', 'ORGANIZATION_EVENTS_ENABLED', 'ORGANIZATION_CALLING_SCOPE',
    'CALLING_ORGANIZATION_FORWARDING_ENABLED', 'ORGANIZATIONS_SERVICE_BINDINGS_ENABLED', 'NOTIFICATIONS_EMAIL_ENABLED', 'EMAIL_VERIFICATION_REQUIRED',
    'MESSAGING_E2EE_DEVICES_ENABLED', 'MESSAGING_PUSH_PROVIDER', 'SMS_GATEWAY_PROVIDER', 'TELEPHONY_ENABLED',
    'PUBLIC_OUTBOUND_CALLING_ENABLED', 'PUBLIC_INBOUND_CALLING_ENABLED', 'CALLBACK_RELAY_ENABLED',
    'ENTERPRISE_SIP_ENABLED', 'TELEPHONY_SIP_EDGE', 'PRIVACY_RELAY_ENABLED', 'CONTACT_VAULT_ENABLED',
    'RELAY_NUMBER_POOL_ENABLED', 'NUMBERING_PROVIDERS', 'NUMBERING_PORTING_ENABLED',
    'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED', 'DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED',
    'DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL', 'DEVELOPER_PLATFORM_WEBHOOKS_ENABLED',
    'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE', 'API_GATEWAY_PUBLIC_ROUTING_ENABLED',
    'API_GATEWAY_WEBSOCKETS_ENABLED', 'API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED', 'API_GATEWAY_CENTRAL_USAGE_ENABLED',
    'FEATURE_CONFERENCE_REALTIME_ENABLED', 'FEATURE_WEBINAR_MODE_ENABLED', 'FEATURE_QUALITY_TELEMETRY_ENABLED',
    'FEATURE_HEALTHCARE_PROFILE_ENABLED', 'MESSAGING_SESSION_DEVICES_ENABLED', 'EMAIL_VERIFICATION_ENFORCEMENT',
    'DEVELOPER_SESSION_BRIDGE_ENABLED', 'CALLING_WEBHOOK_AUTHORITY', 'CALLING_WEBHOOK_DELIVERY_ENABLED',
    'CALLING_PUBLIC_EVENTS_DESTINATION', 'MESSAGING_PUBLIC_EVENTS_DESTINATION', 'MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION',
    'API_GATEWAY_DISABLED_DOMAINS', 'DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS',
    'ORGANIZATIONS_AUTHORITY_MODE', 'ORGANIZATION_CALLING_SCOPE', 'CFMEET_USAGE_EXPORT_ENABLED',
    'EMAIL_PROVIDER', 'EMAIL_FROM_ADDRESS', 'EMAIL_VERIFICATION_REQUIRED', 'EMAIL_VERIFICATION_ENFORCE_FROM',
    'EMAIL_VERIFICATION_GRACE_UNTIL', 'FEATURE_MEETING_V2_ENABLED', 'FEATURE_RTC_PROVIDER_CONTROL_V2',
    'CONFERENCE_EMPTY_TIMEOUT_SECS', 'FEATURE_RECORDING_V2_ENABLED',
) + tuple(sorted(p.SAFE_ENV))
SECRET_HINT = re.compile(r'(SECRET|PASSWORD|TOKEN|KEY|PEPPER|DATABASE_URL|REDIS_URL|CREDENTIAL|DSN)', re.I)


def safe_flags(env):
    """Known feature flags with their values; anything that looks like a secret or a
    credential-bearing URL is reduced to presence. Other variables: names only."""
    flags = {}
    for name in FLAGS:
        if name not in env:
            continue
        value = env[name]
        if SECRET_HINT.search(name) or '://' in value or '@' in value or len(value) > 120:
            flags[name] = 'present'
        else:
            flags[name] = value
    return flags


def mail_presence(env):
    return {'smtpHostConfigured': bool(env.get('SMTP_HOST')),
            'smtpCredentialsConfigured': bool(env.get('SMTP_USERNAME')) and bool(env.get('SMTP_PASSWORD')),
            'resendConfigured': env.get('EMAIL_PROVIDER') == 'resend' and bool(env.get('RESEND_API_KEY_FILE'))}


def mail_available(env):
    """A working transport is configured: Resend (key mounted) or SMTP host + credentials."""
    m = mail_presence(env)
    return m['resendConfigured'] or (m['smtpHostConfigured'] and m['smtpCredentialsConfigured'])


def scrub(text):
    return re.sub(r'[a-z][a-z0-9+.-]*://\S+', '[redacted-url]', text or '')[-2000:]


def emit(kind, value):
    print(json.dumps({kind: value}, sort_keys=True, default=str), flush=True)


# --- Images ------------------------------------------------------------------------------

def image_ref(service, sha):
    return IMAGE_REPO + service + ':' + sha


def registry(sha):
    path = WORK / ('registry-' + sha + '.json')
    if not path.exists():
        raise RuntimeError('Image manifest for this SHA was not transferred by the workflow')
    return json.loads(path.read_text())


def verify_image(service, sha, manifest):
    """The local image of the release SHA is exactly the one the workflow verified."""
    if service not in manifest:
        raise RuntimeError('Image manifest lacks ' + service)
    meta = json.loads(p.run('docker', 'image', 'inspect', image_ref(service, sha)))[0]
    labels = (meta.get('Config') or {}).get('Labels') or {}
    if labels.get('org.opencontainers.image.revision') != sha:
        raise RuntimeError('Image source revision mismatch: ' + service)
    if meta['Id'] != manifest[service]['imageId']:
        raise RuntimeError('Image differs from the workflow-verified image: ' + service)
    return meta['Id']


def verify_running(container, image_id, sha):
    c = p.inspect(container)
    if not c['State']['Running'] or c['Image'] != image_id:
        raise RuntimeError('Running image differs from the release image: ' + container)
    labels = (json.loads(p.run('docker', 'image', 'inspect', c['Image']))[0].get('Config') or {}).get('Labels') or {}
    if labels.get('org.opencontainers.image.revision') != sha:
        raise RuntimeError('Running image revision differs from the release SHA: ' + container)


def image_of(name):
    return SERVICES[name]['image'] or name


RELEASE_SCOPE = Path(__file__).resolve().parent / 'release-scope.json'


def expected_image(name, sha, manifest=None):
    """(image id, revision) a service must run under the completed release `sha`: the
    release's own image, or — for a service the release kept because its build inputs did
    not change — the exact image it kept running (recorded in the journal entry)."""
    entry = last_release(sha)
    kept = (entry or {}).get('kept') or {}
    if name in kept:
        return kept[name]['imageId'], kept[name]['revision']
    manifest = manifest or registry(sha)
    return manifest[image_of(name)]['imageId'], sha


def release_scope(sha, path=None):
    """Images to replace for this release (release_scope.py on the runner). A service is kept
    only when the runner proved its build inputs unchanged since the previous completed
    release and the VPS sees it running exactly that release's recorded image."""
    path = path or RELEASE_SCOPE
    previous = last_release()
    if not path.exists() or not previous:
        return set(IMAGES), {}
    doc = json.loads(path.read_text())
    if doc.get('head') != sha or doc.get('base') != previous['sha']:
        emit('releaseScope', 'scope document does not match this release: full release')
        return set(IMAGES), {}
    unchanged = set(doc.get('unchanged') or []) & set(IMAGES)
    kept = {}
    for name in ORDER:
        if image_of(name) not in unchanged or SERVICES[name]['container'] not in container_names():
            continue
        try:
            image_id, revision = expected_image(name, previous['sha'])
            verify_running(SERVICES[name]['container'], image_id, revision)
        except (RuntimeError, KeyError):
            unchanged.discard(image_of(name))
            continue
        kept[name] = {'imageId': image_id, 'revision': revision}
    # A service sharing its image with a replaced one is replaced too.
    for name in list(kept):
        if image_of(name) not in unchanged:
            del kept[name]
    return set(IMAGES) - unchanged, kept


# --- Health --------------------------------------------------------------------------------

def health(name, attempts=45):
    s = SERVICES[name]
    if not s['port']:
        return stays_running(s['container'])
    url = 'http://127.0.0.1:%d%s' % (s['port'], s['health'])
    for attempt in range(attempts):
        try:
            p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'], PROBE_IMAGE,
                  '-fsS', '--max-time', '4', '-o', '/dev/null', url)
            return
        except RuntimeError:
            if attempt % 10 == 0:
                emit('healthWait', name)
            time.sleep(2)
    raise RuntimeError('Health check failed: ' + name)


def stays_running(container, seconds=20):
    before = p.inspect(container)
    time.sleep(seconds)
    after = p.inspect(container)
    if not (after['State']['Running'] and after['RestartCount'] == before['RestartCount']):
        raise RuntimeError('Container does not stay running: ' + container)


# --- Journal (release and activation records, newest last) ----------------------------------

def load_journal():
    return json.loads(JOURNAL.read_text()) if JOURNAL.exists() else []


def commit(entry):
    journal = [e for e in load_journal() if e['id'] != entry['id']] + [entry]
    p.write_private(JOURNAL, json.dumps(journal, indent=1, sort_keys=True))


def begin(operation, sha, run_id, irreversible=False):
    entry = dict(id=operation + ':' + run_id, operation=operation, sha=sha, runId=run_id,
                 startedAt=now(), status='running', irreversible=irreversible, components=[])
    commit(entry)
    return entry


def done(operation):
    return any(e['operation'] == operation and e['status'] == 'done' for e in load_journal())


def last_release(sha=None):
    found = [e for e in load_journal() if e['operation'] == 'release' and e['status'] == 'done'
             and (sha is None or e['sha'] == sha)]
    return found[-1] if found else None


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')


# --- Swap with rollback ----------------------------------------------------------------------

def no_active_calls():
    """Never stop Calling underneath an established call (communications_release)."""
    for attempt in range(90):
        active = release.db_sql('cfmeet_calling', "SELECT count(*) FROM calls WHERE current_state NOT IN "
                                "('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED');")
        if active == '0':
            return
        if attempt % 15 == 0:
            emit('waitForActiveCalls', int(active))
        time.sleep(2)
    raise RuntimeError('Active calls prevented a safe Calling swap; nothing changed')


def swap(name, cfg, run_id, entry):
    """Replace a running owned container by `cfg`, keeping the original stopped for rollback.
    Health and identity are checked by the caller; any failure there calls restore()."""
    container = SERVICES[name]['container']
    if name == 'calling':
        no_active_calls()
    previous = container + '-rollback-' + run_id
    rename(container, previous)
    component = dict(service=name, name=container, previous=previous, created=False, status='swapping')
    entry['components'].append(component)
    commit(entry)
    try:
        stop(previous)
        p.docker_api('POST', '/containers/create?name=' + container, cfg)
        p.docker_api('POST', '/containers/' + container + '/start')
    except Exception:
        restore(component, run_id)
        commit(entry)
        raise
    return component


def restore(component, run_id):
    """Inverse of swap()/create(): the exact previous container, or none for a new service."""
    names = container_names()
    name, previous = component['name'], component.get('previous')
    if component['status'] in ('unchanged', 'rolled-back'):
        return
    if name in names:
        try:
            stop(name)
        except RuntimeError:
            pass
        suffix = '-rolled-back-' if component.get('created') else '-failed-'
        rename(name, name + suffix + run_id + '-' + str(time.time_ns()))
    if previous and previous in container_names():
        rename(previous, name)
        start(name)
    component['status'] = 'rolled-back'
    emit('componentRolledBack', {'service': component['service'], 'restoredPrevious': bool(previous)})


def checked(component, name, entry, run_id, check):
    try:
        check()
    except Exception:
        restore(component, run_id)
        commit(entry)
        raise
    component['status'] = 'released'
    commit(entry)


# --- Trust bundles and grants ----------------------------------------------------------------

def service_id(name):
    s = SERVICES.get(name)
    return 'cfmeet.' + ((s and s['identity']) or name)


# (operation, receiver, issuer, permissions): final-production-activation-runbook §2,
# organizations-cutover-runbook (Organizations bundle) and the developer-platform gateway
# activation checklist §4. Every row is checked against packages/service-auth/src/permissions.rs
# (test_coordinated_release.py). Nothing is granted by `release`: new services start dark.
GRANTS = (
    ('activate-provisioning', 'platform-access', 'organizations', ('platform-identity.provision-first-party',)),
    # ADR 0058 Amendment A2: automatic Calling / Messaging bindings inside each organization's
    # own FIRST_PARTY triple (closed scope set; never operator administration).
    ('activate-organization-bindings', 'platform-access', 'organizations',
     ('platform-identity.bind-organization-services',)),
    ('activate-org-events', 'meeting-service', 'organizations', ('meeting-organization-events.apply',)),
    ('org-cutover-shadow', 'organizations', 'meeting-service',
     ('organization.read', 'organization.write', 'organization.act-for-user', 'organization.administer')),
    # Empty-dataset first activation (org_first_activation.py): the same Meeting Service grants
    # as the shadow step it replaces.
    ('org-cutover-first-activation', 'organizations', 'meeting-service',
     ('organization.read', 'organization.write', 'organization.act-for-user', 'organization.administer')),
    ('activate-enterprise-calling', 'platform-access', 'meeting-service', ('platform-principal.ensure',)),
    ('activate-enterprise-calling', 'calling', 'meeting-service', ('calling-request.act-for-organization',)),
    ('activate-enterprise-calling', 'messaging', 'meeting-service', ('messaging-application.delegate',)),
    ('activate-developer-platform', 'platform-access', 'api-gateway', ('platform-credential.authenticate',)),
    ('activate-developer-platform', 'platform-access', 'developer-platform',
     ('platform-identity.read', 'platform-identity.delegate')),
    ('activate-developer-platform', 'developer-platform', 'api-gateway',
     ('developer-request.forward', 'developer-entitlement.read', 'developer-usage.record')),
    # Developer Console sessions: the Meeting Service verifies its own CF Meet sessions for
    # the Developer Platform (session bridge); it never shares the session secret.
    ('activate-developer-console', 'meeting-service', 'developer-platform', ('user-session.verify',)),
    # Production-access review: the operator console (no container; key in the secret
    # store) administers the Developer Platform; manual decisions by named reviewers only.
    ('activate-production-access-review', 'developer-platform', 'operator-console',
     ('developer-platform.administer',)),
    # Customer webhooks: every first-party producer publishes to the central ingestion.
    ('activate-developer-webhooks', 'developer-platform', 'meeting-service', ('developer-event.publish',)),
    ('activate-developer-webhooks', 'developer-platform', 'calling', ('developer-event.publish',)),
    ('activate-developer-webhooks', 'developer-platform', 'messaging', ('developer-event.publish',)),
    # Customer API families through the gateway (receivers authorize the gateway's forwards).
    ('activate-gateway-families', 'calling', 'api-gateway', ('calling-request.forward',)),
    ('activate-gateway-families', 'messaging', 'api-gateway', ('messaging-request.forward',)),
)


def validate_grants(registry_, grants=GRANTS):
    """registry_: {permission: (receiver service id, {grantable issuer ids})}. Returns errors."""
    errors = []
    for operation, receiver, issuer, permissions in grants:
        for permission in permissions:
            if permission not in registry_:
                errors.append('%s: unknown permission %s' % (operation, permission))
                continue
            owner, grantable = registry_[permission]
            if owner != service_id(receiver):
                errors.append('%s: %s is received by %s, not %s' % (operation, permission, owner, service_id(receiver)))
            if service_id(issuer) not in grantable:
                errors.append('%s: %s may not be granted to %s' % (operation, permission, service_id(issuer)))
    return errors


def empty_bundle(name):
    return dict(bundle_version=1, receiver=service_id(name), runtime_environment='production', trusted_issuers=[])


def merge_grant(bundle, issuer, key, permissions):
    """Adds permissions (and the issuer key) without ever dropping an issuer, key or grant."""
    merged = copy.deepcopy(bundle)
    assert merged.get('runtime_environment') == 'production', 'Trust bundle is not a production bundle'
    for entry in merged['trusted_issuers']:
        if entry['issuer'] == issuer:
            entry['permissions'] = list(entry['permissions']) + [x for x in permissions if x not in entry['permissions']]
            if not any(k.get('kid') == key['kid'] for k in entry['keys']):
                entry['keys'].append(key)
            return merged
    merged['trusted_issuers'].append(dict(issuer=issuer, audience=merged['receiver'],
        runtime_environment=merged['runtime_environment'], keys=[key], permissions=list(permissions)))
    return merged


def mount_source(c, target):
    found = [m['Source'] for m in c['Mounts'] if m['Destination'] == target]
    if len(found) != 1:
        raise RuntimeError('Expected exactly one mount at ' + target + ' in ' + c['Name'].lstrip('/'))
    return found[0]


def current_bundle(c):
    target = p.env_of(c).get('CF_SERVICE_TRUST_BUNDLE_FILE')
    if not target:
        raise RuntimeError('Receiver has no trust bundle configured: ' + c['Name'].lstrip('/'))
    source = mount_source(c, target)
    return target, source, json.loads(Path(source).read_text())


# Issuers without a container of their own (operator tooling identities).
OPERATOR_IDENTITIES = ('operator-console',)


def issuer_key(name):
    if name == 'operator-console':
        return developer_access.operator_public_key(sys.modules[__name__])
    c = p.inspect(SERVICES[name]['container'])
    e = p.env_of(c)
    if not e.get('CF_SERVICE_AUTH_PRIVATE_KEY_FILE') or not e.get('CF_SERVICE_AUTH_KEY_ID'):
        raise RuntimeError('Issuer has no service signing identity: ' + name)
    return release.public_key(mount_source(c, e['CF_SERVICE_AUTH_PRIVATE_KEY_FILE']), e['CF_SERVICE_AUTH_KEY_ID'])


def write_bundle(name, bundle, run_id, like):
    """A new bundle version with the owner and mode of the file it supersedes (`like`)."""
    folder = WORK / 'trust'
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = folder / ('%s.%s.trust.json' % (name, run_id))
    path.write_text(json.dumps(bundle, sort_keys=True) + '\n')
    os.chown(path, like.st_uid, like.st_gid)
    os.chmod(path, like.st_mode & 0o444)
    return path


def set_mount(cfg, target, source):
    """Point the bind mount at `target` to `source`, read-only (Binds or Mounts form)."""
    host = cfg['HostConfig']
    found = False
    if host.get('Binds'):
        binds = []
        for bind in host['Binds']:
            parts = bind.split(':')
            if len(parts) >= 2 and parts[1] == target:
                binds.append('%s:%s:ro' % (source, target))
                found = True
            else:
                binds.append(bind)
        host['Binds'] = binds
    for mount in host.get('Mounts') or []:
        if mount.get('Target') == target:
            mount.update(Type='bind', Source=str(source), ReadOnly=True)
            found = True
    if not found:
        host.setdefault('Mounts', []).append(dict(Type='bind', Source=str(source), Target=target, ReadOnly=True))


def bundles_for(operation, run_id):
    """{receiver: (target, new bundle path)} for the grants of `operation`. Previous bundle
    files are never modified; a copy of each is kept next to the new version."""
    receivers = {}
    for op, receiver, issuer, permissions in GRANTS:
        if op != operation:
            continue
        if receiver not in receivers:
            c = p.inspect(SERVICES[receiver]['container'])
            target, source, bundle = current_bundle(c)
            assert bundle['receiver'] == service_id(receiver), 'Trust bundle receiver mismatch: ' + receiver
            receivers[receiver] = [target, source, bundle]
        receivers[receiver][2] = merge_grant(receivers[receiver][2], service_id(issuer), issuer_key(issuer), permissions)
    out = {}
    for receiver, (target, source, bundle) in receivers.items():
        before = json.loads(Path(source).read_text())
        assert {x['issuer'] for x in before['trusted_issuers']} <= {x['issuer'] for x in bundle['trusted_issuers']}
        p.write_private(WORK / 'trust' / 'backup' / ('%s.%s.before.json' % (receiver, run_id)), json.dumps(before))
        out[receiver] = (target, write_bundle(receiver, bundle, run_id, os.stat(source)))
    return out


# --- Database access (secrets only in 0600 files and stdin) ----------------------------------

def secret_file(path, factory, uid=None, mode=0o400):
    if not path.exists() or not path.read_text().strip():
        p.write_private(path, factory())
    if uid is not None:
        os.chown(path, uid, uid)
    os.chmod(path, mode)
    return path.read_text().strip()


def password(role, kind):
    path = p.SECRETS / ('%s-%s.password' % (role, kind))
    if not path.exists():
        raise RuntimeError('Missing %s password for %s; run provision first' % (kind, role))
    return path.read_text().strip()


def core_url(user, secret, host, port, db):
    return 'postgres://%s:%s@%s:%d/%s' % (user, quote(secret, safe=''), host, port, db)


def runtime_url(name):
    s = SERVICES[name]
    return core_url(s['role'], password(s['role'], 'runtime'), 'cfmeet-core-db', 5432, s['db'])


def migrator_target(name):
    """(DATABASE_URL, docker network) for the service's migrator role. Fails closed."""
    s = SERVICES[name]
    if name == 'meeting-service':
        return release.meeting_migration_url(p.env_of(p.inspect(MEETING))), 'container:' + MEETING
    if name == 'communications':
        config = release.WORK / (s['db'] + '.database.private.json')
        if not config.exists():
            raise RuntimeError('Communications database configuration missing; refusing to guess')
        return json.loads(config.read_text())['migrator'], p.NET
    host, port, db = 'cfmeet-core-db', 5432, s['db']
    if s['container'] in container_names():
        url = urlsplit(p.env_of(p.inspect(s['container'])).get('DATABASE_URL', ''))
        if url.hostname:
            host, port, db = url.hostname, url.port or 5432, url.path.lstrip('/')
    return core_url(s['role'] + '_migrator', password(s['role'], 'migrator'), host, port, db), p.NET


def migrate(name, image_id):
    s = SERVICES[name]
    if not s['migrations']:
        return
    url, network = migrator_target(name)
    path = release.env_file(WORK / 'run' / (name + '.migration.env'), {'DATABASE_URL': url})
    try:
        for binary in s['migrations']:
            emit('migrate', {'service': name, 'binary': binary})
            p.run('docker', 'run', '--rm', '--network', network, '--memory', '256m', '--env-file', str(path),
                  '--entrypoint', '/app/' + binary, image_id, 'up')
    finally:
        path.unlink(missing_ok=True)


def meeting_psql(sql, url=None):
    """psql inside the Meeting Service network namespace; the URL never leaves a 0600 file."""
    url = url or p.env_of(p.inspect(MEETING))['DATABASE_URL']
    path = release.env_file(WORK / 'run' / 'meeting-psql.env', {'DATABASE_URL': url})
    try:
        return p.run('docker', 'run', '--rm', '-i', '--network', 'container:' + MEETING, '--env-file', str(path),
                     PG_CLIENT, 'sh', '-c', 'exec psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1', input=sql)
    finally:
        path.unlink(missing_ok=True)


def history_tables(sql):
    rows = sql("SELECT table_name FROM information_schema.tables WHERE table_schema='public' "
               "AND table_name LIKE '%migrations' ORDER BY 1;")
    return [t for t in rows.splitlines() if re.fullmatch(r'[a-z0-9_]+', t)]


def history_state(sql):
    state = {}
    for table in history_tables(sql):
        count, latest = sql('SELECT count(*), coalesce(max(version), \'\') FROM "%s";' % table).split('|')
        state[table] = {'applied': int(count), 'latest': latest}
    return state


def core_databases():
    rows = release.db_sql('postgres', "SELECT datname FROM pg_database WHERE datname LIKE 'cfmeet%' "
                          "AND NOT datistemplate ORDER BY 1;")
    return [d for d in rows.splitlines() if re.fullmatch(r'cfmeet_[a-z0-9_]+', d)]


# --- inspect ---------------------------------------------------------------------------------

def cfmeet_containers(names):
    return [n for n in names if (n.startswith('cfmeet-') and not n.startswith('cfmeet-staging-')) or n == 'lk-server']


def image_revision(image_id):
    try:
        meta = json.loads(p.run('docker', 'image', 'inspect', image_id))[0]
    except (RuntimeError, OSError, ValueError, IndexError):
        return None
    return ((meta.get('Config') or {}).get('Labels') or {}).get('org.opencontainers.image.revision')


def summarize(c):
    labels = c['Config'].get('Labels') or {}
    env = p.env_of(c)
    h = c['HostConfig']
    # `revision` is the image's own label (authoritative). A container cloned by a release
    # keeps the label of the image it was first created from: reported separately.
    return dict(name=c['Name'].lstrip('/'), state=c['State']['Status'],
                health=(c['State'].get('Health') or {}).get('Status', 'not_configured'),
                image=c['Config']['Image'], imageId=c['Image'],
                revision=image_revision(c['Image']), containerLabelRevision=labels.get('org.opencontainers.image.revision'),
                releaseSha=labels.get('com.cfmeet.release.sha'), memoryMiB=(h.get('Memory') or 0) // 1048576,
                restart=(h.get('RestartPolicy') or {}).get('Name'), networks=sorted(c['NetworkSettings']['Networks']),
                ports=h.get('PortBindings') or {}, environmentNames=sorted(env), flags=safe_flags(env))


def inspect(sha, run_id):
    names = container_names()
    emit('resources', dict(memAvailableMiB=mem_available_mib(), diskFreeGiB=round(disk_free_bytes('/') / 2**30, 1)))
    stats = p.run('docker', 'stats', '--no-stream', '--format', '{{.Name}} {{.MemUsage}}')
    for name in cfmeet_containers(names):
        try:
            emit('container', summarize(p.inspect(name)))
        except RuntimeError:
            emit('container', {'name': name, 'state': 'disappeared'})
    emit('memoryUsage', [line for line in stats.splitlines() if line.split(' ')[0] in cfmeet_containers(names)])
    emit('notDeployed', [n for n in ORDER if SERVICES[n]['container'] not in names])
    emit('scanGatewayDeployed', 'cosmicforge-scan-gateway' in names)
    for db in core_databases():
        emit('migrations', {'database': db, 'histories': history_state(lambda q, d=db: release.db_sql(d, q))})
    if MEETING in names:
        emit('migrations', {'database': 'meeting-service', 'histories': history_state(meeting_psql)})
        emit('mail', mail_presence(p.env_of(p.inspect(MEETING))))
    emit('journal', [{k: e[k] for k in ('operation', 'sha', 'runId', 'status')} for e in load_journal()[-10:]])
    emit('plannedNewContainers', planned_new(names))
    emit('resourceGatePreview', dict(memAvailableMiB=mem_available_mib(), requiredMiB=required_mib(planned_new(names)),
                                     safetyMarginMiB=SAFETY_MARGIN_MIB))


def planned_new(names=None):
    names = container_names() if names is None else names
    return [n for n in ORDER if SERVICES[n]['memory'] and SERVICES[n]['container'] not in names]


# --- backup ----------------------------------------------------------------------------------

def run_binary(args, stdin_path=None, stdout_path=None):
    """Like production.run for binary streams; stderr (may hold credentials) is discarded."""
    stdin = open(stdin_path, 'rb') if stdin_path else subprocess.DEVNULL
    stdout = open(stdout_path, 'wb') if stdout_path else subprocess.PIPE
    try:
        proc = subprocess.run(args, stdin=stdin, stdout=stdout, stderr=subprocess.PIPE)
    finally:
        if stdin_path:
            stdin.close()
        if stdout_path:
            stdout.close()
    if proc.returncode:
        raise RuntimeError('subprocess failed: ' + ' '.join(args[:2]))
    return proc.stdout


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def toc_entries(dump, major):
    listing = run_binary(['docker', 'run', '--rm', '-i', '--network', 'none', 'postgres:%d-alpine' % major,
                          'pg_restore', '--list'], stdin_path=dump).decode()
    return [line for line in listing.splitlines() if line.strip() and not line.startswith(';')]


def restore_test(dump, major, expected, run_id, label):
    """Restore into a throwaway, network-less PostgreSQL and compare every migration history."""
    name = 'cfmeet-restore-test-%s-%s' % (run_id, label)
    _own(name)
    p.run('docker', 'run', '-d', '--name', name, '--network', 'none', '--memory', '512m',
          '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', 'postgres:%d-alpine' % major)
    try:
        for _ in range(60):
            try:
                p.run('docker', 'exec', name, 'pg_isready', '-q', '-U', 'postgres')
                break
            except RuntimeError:
                time.sleep(1)
        p.run('docker', 'exec', name, 'createdb', '-U', 'postgres', 'restore_test')
        run_binary(['docker', 'exec', '-i', name, 'pg_restore', '-U', 'postgres', '-d', 'restore_test',
                    '--no-owner', '--no-privileges'], stdin_path=dump)
        sql = lambda q: p.run('docker', 'exec', '-i', name, 'psql', '-XAtq', '-U', 'postgres', '-d', 'restore_test', input=q)
        restored = {t: v['applied'] for t, v in history_state(sql).items()}
        if restored != expected:
            raise RuntimeError('Restore test history mismatch: ' + label)
        tables = int(sql("SELECT count(*) FROM information_schema.tables WHERE table_schema='public';"))
        return {'status': 'PASS', 'tables': tables, 'histories': restored}
    finally:
        remove(name)


def backup(sha, run_id, restore_labels=('meeting-service', 'organizations', 'platform-access')):
    folder = BACKUPS / run_id
    folder.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chmod(BACKUPS, 0o700)
    meeting_url = release.meeting_migration_url(p.env_of(p.inspect(MEETING)))
    meeting_sql = lambda q: meeting_psql(q, meeting_url)
    meeting_major = int(meeting_sql('SHOW server_version_num;')) // 10000
    core_major = int(release.db_sql('postgres', 'SHOW server_version_num;')) // 10000
    databases = core_databases()
    size = sum(int(release.db_sql('postgres', "SELECT pg_database_size('%s');" % d)) for d in databases)
    size += int(meeting_sql('SELECT pg_database_size(current_database());'))
    free = disk_free_bytes(str(BACKUPS))
    if free < 2 * size + 5 * 2**30:
        raise RuntimeError('Backup disk gate: %d GiB free, need %d GiB' % (free // 2**30, (2 * size + 5 * 2**30) // 2**30))
    entries = []
    for db in databases + ['meeting-service']:
        label = 'meeting-service' if db == 'meeting-service' else db.removeprefix('cfmeet_').replace('_', '-')
        dump = folder / (label + '.dump')
        dump.touch(mode=0o600)
        if db == 'meeting-service':
            histories = {t: v['applied'] for t, v in history_state(meeting_sql).items()}
            env = release.env_file(WORK / 'run' / 'meeting-dump.env', {'DATABASE_URL': meeting_url})
            try:
                run_binary(['docker', 'run', '--rm', '--network', 'container:' + MEETING, '--env-file', str(env),
                            'postgres:%d-alpine' % meeting_major, 'sh', '-c', 'exec pg_dump -Fc "$DATABASE_URL"'],
                           stdout_path=dump)
            finally:
                env.unlink(missing_ok=True)
            major = meeting_major
        else:
            histories = {t: v['applied'] for t, v in history_state(lambda q, d=db: release.db_sql(d, q)).items()}
            run_binary(['docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'pg_dump', '-Fc', '-d', db], stdout_path=dump)
            major = core_major
        os.chmod(dump, 0o600)
        toc = toc_entries(dump, major)
        if not toc or not all(any(' ' + t + ' ' in line for line in toc) for t in histories):
            raise RuntimeError('Backup table of contents incomplete: ' + label)
        item = dict(label=label, file=dump.name, bytes=dump.stat().st_size, sha256=sha256(dump),
                    tocEntries=len(toc), histories=histories, restoreTest='NOT_RUN', postgresMajor=major)
        if label in restore_labels:
            item['restoreTest'] = restore_test(dump, major, histories, run_id, label)
        entries.append(item)
        emit('backup', {k: v for k, v in item.items() if k != 'histories'})
    manifest = dict(runId=run_id, sha=sha, createdAt=now(), databases=entries,
                    note='Same-host copy; off-host encrypted copy is an owner step (backup-and-restore.md).')
    p.write_private(folder / 'manifest.json', json.dumps(manifest, indent=1, sort_keys=True))
    for item in entries:  # re-read what is on disk
        if sha256(folder / item['file']) != item['sha256']:
            raise RuntimeError('Backup checksum changed after writing: ' + item['label'])
    emit('backupManifest', {'path': str(folder / 'manifest.json'), 'databases': len(entries),
                            'restoreTested': [e['label'] for e in entries if e['restoreTest'] != 'NOT_RUN']})
    return manifest


def require_offsite(manifest):
    """Owner gate 1 is also checked mechanically: the fresh backup must have an encrypted
    off-host copy whose restore drill passed on a separate host (offsite_backup.record)."""
    import offsite_backup
    doc = offsite_backup.verified(manifest['runId'], ('meeting-service', 'organizations'))
    if not doc:
        raise RuntimeError('Backup %s has no verified off-host copy and restore drill; run the off-host backup '
                           'workflow (cfmeet-offsite-backup.yml) first' % manifest['runId'])
    emit('offsiteBackup', {'runId': manifest['runId'], 'artifactId': doc['artifactId'], 'runUrl': doc['runUrl']})
    return doc


def fresh_backup(max_age_hours, required):
    manifests = sorted(BACKUPS.glob('*/manifest.json'), key=lambda f: f.stat().st_mtime)
    for path in reversed(manifests):
        m = json.loads(path.read_text())
        age = time.time() - path.stat().st_mtime
        tested = {e['label'] for e in m['databases'] if isinstance(e['restoreTest'], dict)
                  and e['restoreTest'].get('status') == 'PASS'}
        if age <= max_age_hours * 3600 and set(required) <= tested:
            return m
    raise RuntimeError('No verified backup with restore tests of %s within %d h; run backup'
                       % (', '.join(required), max_age_hours))


# --- provision -------------------------------------------------------------------------------

DB_SQL = """SELECT format('CREATE ROLE %I LOGIN NOINHERIT', '{migrator}') WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{migrator}') \\gexec
SELECT format('CREATE ROLE %I LOGIN NOINHERIT', '{runtime}') WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{runtime}') \\gexec
ALTER ROLE {migrator} PASSWORD '{migrator_pw}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE {runtime} PASSWORD '{runtime_pw}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
SELECT format('CREATE DATABASE %I OWNER %I', '{db}', '{migrator}') WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '{db}') \\gexec
REVOKE ALL ON DATABASE {db} FROM PUBLIC;
GRANT CONNECT ON DATABASE {db} TO {runtime};
"""
SCHEMA_SQL = """REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO {runtime};
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {runtime};
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO {runtime};
"""


def provision_database(name):
    """cfmeet-core-db.yml's least-privilege pattern, idempotent, for one new service."""
    s = SERVICES[name]
    role, db = s['role'], s['db']
    pw = {kind: secret_file(p.SECRETS / ('%s-%s.password' % (role, kind)), lambda: secrets.token_hex(32))
          for kind in ('migrator', 'runtime')}
    assert re.fullmatch(r'[a-z_]+', role) and re.fullmatch(r'cfmeet_[a-z_]+', db)
    assert all(re.fullmatch(r'[A-Za-z0-9]{32,128}', v) for v in pw.values()), 'Unexpected password file format'
    values = dict(migrator=role + '_migrator', runtime=role, db=db, migrator_pw=pw['migrator'], runtime_pw=pw['runtime'])
    release.db_sql('postgres', DB_SQL.format(**values))
    release.db_sql(db, SCHEMA_SQL.format(**values))


def provision_identity(name):
    s = SERVICES[name]
    if s['identity']:
        return
    key = p.SECRETS / (name + '-service-auth.pem')
    if not key.exists():
        p.run('openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(key))
    os.chown(key, s['uid'], s['uid'])
    os.chmod(key, 0o400)
    if name != 'api-gateway':
        bundle = WORK / 'trust' / (name + '.trust.json')
        if not bundle.exists():
            bundle.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            bundle.write_text(json.dumps(empty_bundle(name), sort_keys=True) + '\n')
        os.chown(bundle, s['uid'], s['uid'])
        os.chmod(bundle, 0o400)
    if name == 'sms-gateway':
        for key_name in ('sms-gateway-content.key', 'sms-gateway-fingerprint.key'):
            secret_file(p.SECRETS / key_name, lambda: base64.b64encode(secrets.token_bytes(32)).decode(), s['uid'])


TLS_MARKER = '  # CFMEET COORDINATED RELEASE PRIVATE ROUTES'
TLS_TAG = ' # coordinated-release'


def tls_routes(text):
    """Private HTTPS routes (same form as the existing ones) for the new internal services."""
    lines = text.rstrip().splitlines()
    if TLS_MARKER in lines:
        i = lines.index(TLS_MARKER)
        j = i + 1
        while j < len(lines) and lines[j].endswith(TLS_TAG):
            j += 1
        lines = lines[:i] + lines[j:]
    body = '\n'.join(lines)
    routes = []
    for name in ORDER:
        s = SERVICES[name]
        if not s['tls'] or s['existing']:
            continue
        if re.search(r'listen\s+%d[\s;]' % s['tls'], body):
            raise RuntimeError('Private TLS port already in use: %d' % s['tls'])
        routes.append('  server { listen %d ssl; server_name api.cfmeet.com; location / { set $peer %s:%d; '
                      'proxy_pass http://$peer$request_uri; proxy_http_version 1.1; proxy_set_header Host $host; '
                      'proxy_set_header X-Forwarded-Proto https; proxy_read_timeout 120s; } }%s'
                      % (s['tls'], s['container'], s['port'], TLS_TAG))
    assert body.endswith('}'), 'Unexpected private TLS proxy configuration'
    return body[:-1].rstrip() + '\n' + TLS_MARKER + '\n' + '\n'.join(routes) + '\n}\n'


def install_tls_routes(run_id):
    before = TLS_CONF.read_text()
    after = tls_routes(before)
    if after == before:
        return False
    p.write_private(WORK / 'tls' / ('nginx.conf.before-' + run_id), before)
    TLS_CONF.write_text(after)  # the proxy's read-only bind mount requires the same inode
    try:
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-t')
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-s', 'reload')
    except Exception:
        TLS_CONF.write_text(before)
        p.run('docker', 'exec', 'cfmeet-core-tls', 'nginx', '-s', 'reload')
        raise
    assert not p.inspect('cfmeet-core-tls')['HostConfig']['PortBindings'], 'Private TLS proxy must stay private'
    return True


def provision(sha, run_id):
    for name in ORDER:
        if SERVICES[name]['existing']:
            continue
        if SERVICES[name]['db'] and not SERVICES[name]['identity']:
            provision_database(name)
        provision_identity(name)
    emit('provisioned', {'databases': [n for n in ORDER if SERVICES[n]['db'] and not SERVICES[n]['existing']
                                       and not SERVICES[n]['identity']],
                         'privateTlsRoutesChanged': install_tls_routes(run_id)})


# --- release ---------------------------------------------------------------------------------

def template_env(name):
    s = SERVICES[name]
    identity = s['identity'] or name
    env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'LOG_FORMAT': 'json',
           'CF_SERVICE_AUTH_KEY_ID': identity + '-production-1',
           'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/run/secrets/service-auth.pem',
           'CF_PLATFORM_ACCESS_BASE_URL': private_url(SERVICES['platform-access']['tls'])}
    if name != 'api-gateway':
        env['CF_SERVICE_TRUST_BUNDLE_FILE'] = '/run/secrets/trust.json'
    if s['db']:
        env['DATABASE_URL'] = runtime_url(name)
    env.update(s['env'])
    return env


def template_mounts(name):
    s = SERVICES[name]
    identity = s['identity'] or name
    mounts = [(p.SECRETS / (identity + '-service-auth.pem'), '/run/secrets/service-auth.pem')]
    if name != 'api-gateway':
        mounts.append((WORK / 'trust' / (identity + '.trust.json'), '/run/secrets/trust.json'))
    if name == 'sms-gateway':
        mounts += [(p.SECRETS / 'sms-gateway-content.key', '/run/secrets/sms-content.key'),
                   (p.SECRETS / 'sms-gateway-fingerprint.key', '/run/secrets/sms-fingerprint.key')]
    return mounts


def create_args(name, image_id, env_path, sha):
    """A new service: private network only, no published port, bounded, read-only, no caps."""
    s = SERVICES[name]
    memory = '%dm' % s['memory']
    args = ['docker', 'create', '--name', s['container'], '--restart', 'unless-stopped',
            '--network', p.NET, '--network-alias', s['container'], '--env-file', str(env_path),
            '--memory', memory, '--memory-swap', memory, '--pids-limit', '256', '--read-only',
            '--tmpfs', '/tmp:rw,noexec,nosuid,size=16m', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
            '--log-opt', 'max-size=20m', '--log-opt', 'max-file=5',
            '--label', 'com.cfmeet.release.sha=' + sha, '--label', 'com.cfmeet.release.managed-by=coordinated-release']
    for source, target in template_mounts(name):
        args += ['--mount', 'type=bind,src=%s,dst=%s,readonly' % (source, target)]
    return args + [image_id] + s['command']


def deploy(name, image_id, sha, run_id, entry):
    s = SERVICES[name]
    container = s['container']
    if container in container_names():
        c = p.inspect(container)
        if c['Image'] == image_id and c['State']['Running']:
            health(name)
            verify_running(container, image_id, sha)
            entry['components'].append(dict(service=name, name=container, previous=None, created=False, status='unchanged'))
            commit(entry)
            return
        if not c['State']['Running']:
            raise RuntimeError('Existing service container is not running; refusing to replace it: ' + container)
        cfg = release.clone(c, container, image_id, {}, False)
        labels = cfg.setdefault('Labels', {}) or {}
        cfg['Labels'] = labels
        # The cloned container labels would shadow the new image's OCI labels (Docker keeps
        # explicit container labels): drop them so the container reports the release image.
        for key in [k for k in labels if k.startswith('org.opencontainers.image.')]:
            del labels[key]
        labels['com.cfmeet.release.sha'] = sha
        component = swap(name, cfg, run_id, entry)
    else:
        if s['existing']:
            raise RuntimeError('Expected running production service is absent: ' + container)
        env_path = release.env_file(WORK / 'env' / (container + '.env'), template_env(name))
        component = dict(service=name, name=container, previous=None, created=True, status='creating')
        entry['components'].append(component)
        commit(entry)
        _own(container)
        try:
            p.run(*create_args(name, image_id, env_path, sha))
            start(container)
        except Exception:
            restore(component, run_id)
            commit(entry)
            raise
    checked(component, name, entry, run_id, lambda: (health(name), verify_running(container, image_id, sha)))


SAFE_DEFAULTS = {
    'sms-gateway': {'SMS_GATEWAY_PROVIDER': (None, '', 'none')},
    'numbering': {'NUMBERING_PROVIDERS': (None, ''), 'NUMBERING_PORTING_ENABLED': (None, 'false')},
    'telephony': {k: (None, 'false') for k in ('TELEPHONY_ENABLED', 'PUBLIC_OUTBOUND_CALLING_ENABLED',
                  'PUBLIC_INBOUND_CALLING_ENABLED', 'CALLBACK_RELAY_ENABLED', 'ENTERPRISE_SIP_ENABLED')}
                 | {'TELEPHONY_SIP_EDGE': (None, 'none')},
    'privacy-relay': {k: (None, 'false') for k in ('PRIVACY_RELAY_ENABLED', 'CONTACT_VAULT_ENABLED', 'RELAY_NUMBER_POOL_ENABLED')},
    'messaging': {'MESSAGING_E2EE_DEVICES_ENABLED': (None, 'false'), 'MESSAGING_PUSH_PROVIDER': (None, '', 'none')},
    'calling': {'CALLING_PUBLIC_OUTBOUND_ENABLED': (None, 'false'), 'CALLING_PUBLIC_INBOUND_ENABLED': (None, 'false')},
    'developer-platform': {'DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL': (None, 'false'),
                           'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': (None, 'shadow')},
}


PUSH_ADAPTER_PREFIXES = ('MESSAGING_PUSH_APNS_', 'MESSAGING_PUSH_FCM_', 'MESSAGING_PUSH_WEB_')


def safe_defaults():
    """SAFE_DEFAULTS, widened only by a completed activation that owns the setting: customer
    webhook delivery may be `active` once activate-developer-webhooks is recorded. Production
    auto-approval and every provider-dependent switch stay fixed."""
    rules = copy.deepcopy(SAFE_DEFAULTS)
    rules['developer-platform-worker'] = dict(rules['developer-platform'])
    if done('activate-developer-webhooks'):
        for name in ('developer-platform', 'developer-platform-worker'):
            rules[name]['DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'] = (None, 'shadow', 'active')
    return rules


def safe_default_violations(envs):
    """envs: {service: env}. Provider-dependent features must stay off in production."""
    violations = []
    for name, rules in safe_defaults().items():
        env = envs.get(name)
        if env is None:
            continue
        for var, allowed in rules.items():
            if env.get(var) not in allowed:
                violations.append('%s %s' % (name, var))
    # The push adapters switch on from their credential variables whatever
    # MESSAGING_PUSH_PROVIDER says (services/messaging/src/config/mod.rs): none may be present.
    messaging = envs.get('messaging') or {}
    for var in sorted(v for v in messaging if v.startswith(PUSH_ADAPTER_PREFIXES) and messaging[v]):
        violations.append('messaging ' + var)
    meeting = envs.get('meeting-service')
    if meeting and meeting.get('NOTIFICATIONS_EMAIL_ENABLED') == 'true' and not mail_available(meeting):
        violations.append('meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials')
    return violations


def running_envs():
    names = container_names()
    return {n: p.env_of(p.inspect(SERVICES[n]['container'])) for n in ORDER if SERVICES[n]['container'] in names}


def release_all(sha, run_id):
    p.identity()
    manifest = registry(sha)
    images = {service: verify_image(service, sha, manifest) for service in IMAGES if service != 'security-scan-gateway'}
    scope, kept = release_scope(sha)
    emit('releaseScope', {'replaced': sorted(n for n in ORDER if n not in kept), 'kept': sorted(kept)})
    violations = safe_default_violations(running_envs())
    if violations:
        raise RuntimeError('Provider-dependent features are not at their safe defaults: ' + ', '.join(violations))
    emit('resourceGate', resource_gate(mem_available_mib(), required_mib(planned_new())))
    backup(sha, run_id)
    provision(sha, run_id)
    entry = begin('release', sha, run_id)
    entry['kept'] = kept
    commit(entry)
    try:
        for name in ORDER:
            if name in kept:
                emit('kept', {'service': name, 'revision': kept[name]['revision']})
                continue
            image_id = images[image_of(name)]
            # Additive migrations, before the image that needs them; never rolled back.
            if SERVICES[name]['container'] not in container_names() or p.inspect(SERVICES[name]['container'])['Image'] != image_id:
                migrate(name, image_id)
            deploy(name, image_id, sha, run_id, entry)
            emit('released', name)
        entry['status'] = 'done'
        commit(entry)
    except Exception:
        entry['status'] = 'failed'
        commit(entry)
        raise
    scan_failed = False
    if 'security-scan-gateway' not in scope:
        entry['scanGateway'] = 'kept (build inputs unchanged)'
    elif 'cosmicforge-scan-gateway' in container_names():
        # Shared component with its own verified upgrade and self-rollback (scan_gateway.py).
        import scan_gateway
        try:
            scan_gateway.configure(sha, run_id)
            entry['scanGateway'] = 'released'
        except Exception as error:
            scan_failed = True
            entry['scanGateway'] = 'upgrade failed (%s); previous gateway restored' % type(error).__name__
    else:
        entry['scanGateway'] = 'not deployed (separate scan-gateway-configure operation)'
    commit(entry)
    verify(sha, run_id)
    if scan_failed:
        raise RuntimeError('Release verified, but the scan gateway upgrade failed and was restored; '
                           'rerun scan-gateway-configure')


# --- verify ----------------------------------------------------------------------------------

def http_status(url, method='GET', headers=None):
    request = urllib.request.Request(url, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, dict(response.headers)
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers)


def public_smoke():
    checks = {}
    status, _ = http_status(PUBLIC + '/api-docs/swagger-config.json')
    checks['public_swagger'] = 'PASS' if status == 200 else 'FAIL'
    status, headers = http_status(PUBLIC + '/api/v2/me/profile', 'OPTIONS', {
        'Origin': WEB_ORIGIN, 'Access-Control-Request-Method': 'GET', 'Access-Control-Request-Headers': 'authorization'})
    allowed = {k.lower(): v for k, v in headers.items()}.get('access-control-allow-origin')
    checks['cors_preflight_from_web'] = 'PASS' if status < 400 and allowed in (WEB_ORIGIN, '*') else 'FAIL'
    for path in ('/internal/v1/organizations', '/internal/v1/admin/event-delivery'):
        status, _ = http_status(PUBLIC + path)
        checks['internal_not_public ' + path] = 'PASS' if status in (401, 403, 404) else 'FAIL'
    return checks


def event_delivery():
    rows = release.db_sql('cfmeet_organizations', "SELECT coalesce(json_object_agg(state, n), '{}') FROM "
                          "(SELECT state, count(*) n FROM organization_outbox GROUP BY state) s;")
    return json.loads(rows or '{}')


def verify(sha, run_id):
    entry = last_release(sha)
    if not entry:
        latest = last_release()
        raise RuntimeError('No completed coordinated release of this SHA is recorded (latest completed release: %s). '
                           'verify certifies a coordinated release only; use certify for the live deployment'
                           % (latest['sha'] if latest else 'none'))
    manifest = registry(sha)
    checks = {}
    for name in ORDER:
        try:
            health(name, attempts=10)
            verify_running(SERVICES[name]['container'], *expected_image(name, sha, manifest))
            checks['running ' + name] = 'PASS'
        except Exception:
            checks['running ' + name] = 'FAIL'
    for name in ORDER:
        s = SERVICES[name]
        if not s['history'] or s['identity']:
            continue
        sql = meeting_psql if name == 'meeting-service' else (lambda q, d=s['db']: release.db_sql(d, q))
        state = history_state(sql)
        checks['migrations ' + name] = 'PASS' if all(state.get(t, {}).get('applied', 0) > 0 for t in s['history']) else 'FAIL'
        emit('migrationHistories', {'service': name, 'histories': state})
    violations = safe_default_violations(running_envs())
    checks['provider_features_off'] = 'PASS' if not violations else 'FAIL'
    checks.update(public_smoke())
    if SERVICES['organizations']['container'] in container_names():
        delivery = event_delivery()
        emit('organizationsEventDelivery', delivery)
        checks['organizations_no_dead_events'] = 'PASS' if not delivery.get('DEAD') else 'FAIL'
    p.regression()
    emit('verification', {'sha': sha, 'runId': run_id, 'checks': checks, 'violations': violations})
    failed = [k for k, v in checks.items() if v != 'PASS']
    if failed:
        raise RuntimeError('Verification failed: ' + ', '.join(failed))


# --- rollback --------------------------------------------------------------------------------

def rollback(sha, run_id):
    journal = load_journal()
    candidates = [e for e in journal if e['status'] in ('done', 'failed')
                  and (e['components'] or e.get('routing') or e.get('edge'))]
    if not candidates:
        raise RuntimeError('Nothing to roll back')
    entry = candidates[-1]
    if entry.get('irreversible'):
        raise RuntimeError('%s cannot be rolled back (organizations-cutover-runbook §7: never back to a stale writer)'
                           % entry['operation'])
    if entry.get('routing'):
        undo_gateway_routing(entry['routing'])
    if entry['operation'] == 'activate-gateway-families' and GATEWAY_SNIPPET.exists():
        # Families leave the edge before the gateway loses their upstreams (no 5xx window).
        GATEWAY_SNIPPET.write_text(gateway_snippet(GATEWAY_PREFIXES, docs=done('activate-gateway-docs')))
        p.reload_host_nginx()
    if entry.get('edge'):
        undo_gateway_docs(entry['edge'])
    for component in reversed(entry['components']):
        restore(component, run_id)
        commit(entry)
    for component in entry['components']:
        if component['previous']:
            health(component['service'])
    entry['status'] = 'rolled-back'
    entry['rolledBackBy'] = run_id
    commit(entry)
    p.regression()
    emit('rollback', {'operation': entry['operation'], 'sha': entry['sha'], 'runId': entry['runId'],
                      'migrationsRetained': True})


# --- activation steps ------------------------------------------------------------------------

def recreate(name, overrides, bundle, run_id, entry, extra=None):
    """Same image, preserved configuration, changed flags and/or trust bundle; `extra` may add
    read-only secret mounts ([(source, target)]) and networks ([name])."""
    container = SERVICES[name]['container']
    c = p.inspect(container)
    cfg = release.clone(c, container, c['Image'], overrides, False)
    if bundle:
        set_mount(cfg, bundle[0], bundle[1])
    for source, target in (extra or {}).get('mounts', ()):
        release.add_mount(cfg, source, target)
    for network in (extra or {}).get('networks', ()):
        cfg['NetworkingConfig']['EndpointsConfig'].setdefault(network, {'Aliases': [container]})
    component = swap(name, cfg, run_id, entry)
    checked(component, name, entry, run_id, lambda: health(name))


ACTIVATION_BASE = Path(__file__).resolve().parent / 'activation-base.json'


def activation_base(sha, path=None):
    """The released SHA an activation of `sha` acts on: `sha` itself when it was released,
    otherwise the latest completed release, provided the runner proved (activation_base.py)
    that no image build input changed between that release and `sha`."""
    if last_release(sha):
        return sha
    latest = last_release()
    if not latest:
        raise RuntimeError('Activation requires a completed coordinated release')
    path = path or ACTIVATION_BASE
    if not path.exists():
        raise RuntimeError('Activation requires the completed coordinated release of this SHA, or the runner\'s '
                           'proof that its images equal the latest release (%s)' % latest['sha'])
    doc = json.loads(path.read_text())
    if doc.get('head') != sha or doc.get('base') != latest['sha'] or doc.get('equivalent') is not True \
            or doc.get('changed') or not doc.get('inputs'):
        raise RuntimeError('Image build inputs changed since the latest release %s (or the proof does not match); '
                           'release this SHA first' % latest['sha'])
    emit('activationBase', {'head': sha, 'release': latest['sha'], 'buildInputsChecked': len(doc['inputs'])})
    return latest['sha']


def require_release(sha):
    base = activation_base(sha)
    manifest = registry(base)
    for name in ORDER:
        verify_running(SERVICES[name]['container'], *expected_image(name, base, manifest))
    return base


def apply_activation(operation, sha, run_id, changes, irreversible=False, post=None):
    """changes: [(service, env overrides[, extra])], applied in order; receivers of new grants
    are recreated first (with any overrides of their own). Failure restores the whole step."""
    bundles = bundles_for(operation, run_id)
    changes = [tuple(c) + ({},) * (3 - len(c)) for c in changes]
    listed = {c[0] for c in changes}
    plan = [(name, {}, {}) for name in bundles if name not in listed] + changes
    entry = begin(operation, sha, run_id, irreversible)
    try:
        for name, overrides, extra in plan:
            recreate(name, overrides, bundles.get(name), run_id, entry, extra)
            emit('activated', {'operation': operation, 'service': name, 'flags': safe_flags(overrides),
                               'trustBundleChanged': name in bundles})
        if post:
            post()
    except Exception:
        for component in reversed(entry['components']):
            restore(component, run_id)
        entry['status'] = 'failed'
        commit(entry)
        raise
    entry['status'] = 'done'
    commit(entry)
    p.regression()


def env_of(name):
    return p.env_of(p.inspect(SERVICES[name]['container']))


def tls_route_present(port):
    if not re.search(r'listen\s+%d\s+ssl' % port, TLS_CONF.read_text()):
        raise RuntimeError('Private TLS route %d is not installed' % port)


def has_signing_identity(name):
    e = env_of(name)
    missing = [k for k in ('CF_SERVICE_AUTH_KEY_ID', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE') if not e.get(k)]
    if missing or e.get('CF_RUNTIME_ENVIRONMENT') != 'production':
        raise RuntimeError('%s lacks its production signing identity (%s)' % (name, ', '.join(missing) or 'environment'))


def activate_provisioning(sha, run_id, confirm):
    has_signing_identity('organizations')
    apply_activation('activate-provisioning', sha, run_id,
                     [('organizations', {'ORGANIZATIONS_PROVISIONING_ENABLED': 'true'})])


# The Organizations migration that records automatic bindings (source, verification, expiry).
BINDINGS_MIGRATION = 'm20261113_000005_automatic_service_bindings'
BINDING_STATE_SQL = ("SELECT json_build_object('failed', (SELECT count(*) FROM organization_provisioning "
                     "WHERE state = 'FAILED'), 'automaticBindings', (SELECT count(*) FROM "
                     "organization_scope_service_bindings WHERE source = 'AUTOMATIC'));")


def activate_organization_bindings(sha, run_id, confirm):
    """ADR 0058 Amendment A2: every organization gets its Calling and Messaging service bindings
    automatically (Platform Access creates the service account, credential and fixed scope grants
    inside the organization's own triple; Organizations stores ids only and marks the organization
    READY only after verifying them). Grants Organizations `platform-identity.bind-organization-services`
    in the Platform Access bundle and switches ORGANIZATIONS_SERVICE_BINDINGS_ENABLED on."""
    has_signing_identity('organizations')
    if env_of('organizations').get('ORGANIZATIONS_PROVISIONING_ENABLED') != 'true':
        raise RuntimeError('Automatic bindings require Organizations provisioning')
    db = SERVICES['organizations']['db']
    applied = release.db_sql(db, "SELECT count(*) FROM organizations_seaql_migrations WHERE version = '%s';"
                             % BINDINGS_MIGRATION).strip()
    if applied != '1':
        raise RuntimeError('Organizations lacks migration %s: release first' % BINDINGS_MIGRATION)

    def post():
        time.sleep(15)
        state = json.loads(release.db_sql(db, BINDING_STATE_SQL))
        emit('organizationBindings', state)
        if state['failed']:
            raise RuntimeError('Organization provisioning workflows FAILED after enabling automatic bindings')

    apply_activation('activate-organization-bindings', sha, run_id,
                     [('organizations', {'ORGANIZATIONS_SERVICE_BINDINGS_ENABLED': 'true'})], post=post)


def activate_org_events(sha, run_id, confirm):
    tls_route_present(SERVICES['meeting-service']['tls'])
    applied = history_state(meeting_psql).get('organization_integration_migrations', {}).get('applied', 0)
    if not applied:
        raise RuntimeError('Meeting Service lacks organization-integration-migration; refusing event delivery')
    has_signing_identity('organizations')

    def post():
        time.sleep(30)
        delivery = event_delivery()
        emit('organizationsEventDelivery', delivery)
        if delivery.get('DEAD'):
            raise RuntimeError('Dead organization events after activation')

    apply_activation('activate-org-events', sha, run_id, [
        ('meeting-service', {'ORGANIZATION_EVENTS_ENABLED': 'true'}),
        ('organizations', {'ORGANIZATIONS_EVENT_DESTINATIONS': 'cfmeet.meeting-service',
                           'CF_MEETING_SERVICE_BASE_URL': private_url(SERVICES['meeting-service']['tls'])})], post=post)


INVENTORY_SQL = """BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;
SELECT json_build_object(
 'organizations', (SELECT count(*) FROM organizations),
 'memberships', (SELECT count(*) FROM organization_memberships),
 'organizationsByStatus', (SELECT coalesce(json_object_agg(k, n), '{}') FROM (SELECT status::text k, count(*) n FROM organizations GROUP BY 1) s),
 'membershipsByStatusRole', (SELECT coalesce(json_object_agg(k, n), '{}') FROM (SELECT status::text || '/' || role::text k, count(*) n FROM organization_memberships GROUP BY 1) s),
 'duplicateSlugs', (SELECT count(*) FROM (SELECT lower(slug) FROM organizations GROUP BY 1 HAVING count(*) > 1) d),
 'duplicateLiveMemberships', (SELECT count(*) FROM (SELECT organization_id, user_id FROM organization_memberships WHERE status::text <> 'REMOVED' GROUP BY 1, 2 HAVING count(*) > 1) d),
 'ownerlessOrganizations', (SELECT count(*) FROM organizations o WHERE NOT EXISTS (SELECT 1 FROM organization_memberships m WHERE m.organization_id = o.id AND m.role::text = 'OWNER' AND m.status::text = 'ACTIVE')),
 'slugsOutsideContract', (SELECT count(*) FROM organizations WHERE slug !~ '^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$'),
 'orphanSpaceOrganizations', (SELECT count(*) FROM meeting_spaces s LEFT JOIN organizations o ON o.id = s.organization_id WHERE s.organization_id IS NOT NULL AND o.id IS NULL));
COMMIT;
"""


def org_export(sha, run_id, confirm):
    """organizations-cutover-runbook §1-2: counts-only inventory, then the read-only export."""
    inventory = json.loads(meeting_psql(INVENTORY_SQL))
    emit('organizationsInventory', inventory)
    if inventory['slugsOutsideContract'] or inventory['duplicateSlugs'] or inventory['duplicateLiveMemberships']:
        raise RuntimeError('Rows the import refuses exist; fix them through the legacy routes first')
    if inventory['ownerlessOrganizations'] and 'ownerless-accepted' not in confirm:
        raise RuntimeError('Ownerless organizations exist; owner decision required (confirm: ownerless-accepted)')
    meeting = p.inspect(MEETING)
    folder = CUTOVER / run_id
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    out = folder / 'organizations-export.json'
    out.touch(mode=0o600)
    env = release.env_file(WORK / 'run' / 'export.env', {'DATABASE_URL': p.env_of(meeting)['DATABASE_URL']})
    try:
        with open(out, 'wb') as f:
            proc = subprocess.run(['docker', 'run', '--rm', '--network', 'container:' + MEETING, '--env-file', str(env),
                                   '--entrypoint', '/app/export-organizations', meeting['Image']],
                                  stdout=f, stderr=subprocess.PIPE, text=False)
    finally:
        env.unlink(missing_ok=True)
    if proc.returncode:
        out.unlink(missing_ok=True)
        raise RuntimeError('Export failed: ' + scrub(proc.stderr.decode(errors='replace')))
    doc = json.loads(out.read_text())
    summary = dict(runId=run_id, at=now(), file=str(out), schemaVersion=doc['schemaVersion'],
                   counts=doc['counts'], checksum=doc['checksum'])
    if (doc['counts']['organizations'], doc['counts']['memberships']) != (inventory['organizations'], inventory['memberships']):
        raise RuntimeError('Export counts differ from the inventory; organization writes are still happening')
    p.write_private(CUTOVER / 'latest-export.json', json.dumps(summary))
    emit('organizationsExport', {k: v for k, v in summary.items() if k != 'file'})
    record_step('org-cutover-export', sha, run_id)
    return summary


def organizations_import(image, env, export_path=None):
    """The image's own operator CLI on the Organizations database (the export is streamed on
    stdin: the 0600 root file is never mounted into the container)."""
    args = ['docker', 'run', '--rm', '--network', p.NET, '--env-file', str(env),
            '--entrypoint', '/app/organizations-import', image]
    if export_path is None:
        return json.loads(run_binary(args + ['--reconciliation']).decode())
    return json.loads(run_binary(args[:2] + ['-i'] + args[2:] + ['/dev/stdin'], stdin_path=export_path).decode())


def import_summary(report):
    return dict(imported=report['imported'], unchanged=report['unchanged'], conflicts=len(report['conflicts']),
                checksum=report['checksum'])


def org_import(sha, run_id, confirm, backup_gate=True):
    """organizations-cutover-runbook §3: import, reconcile, prove idempotence, delete the export.
    backup_gate=False only for the empty-dataset first activation (ADR 0059 Amendment 1: owner
    decision, no backup prerequisite); the standard import keeps owner gate 1."""
    if backup_gate:
        if 'offsite-backup-verified' not in confirm:
            raise RuntimeError('Owner gate 1: confirm with offsite-backup-verified after the off-host copy and drill')
        require_offsite(fresh_backup(24, ('meeting-service', 'organizations')))
    latest = json.loads((CUTOVER / 'latest-export.json').read_text())
    export_path = Path(latest['file'])
    if not export_path.exists():
        raise RuntimeError('Latest export already consumed; run org-cutover-export again')
    org = p.inspect(SERVICES['organizations']['container'])
    env = release.env_file(WORK / 'run' / 'import.env', {'DATABASE_URL': p.env_of(org)['DATABASE_URL']})
    try:
        first = import_summary(organizations_import(org['Image'], env, export_path))
        emit('organizationsImport', first)
        if first['conflicts'] or first['checksum'] != latest['checksum']:
            raise RuntimeError('Import conflicts or checksum mismatch; owner gate 7 (no overwrite)')
        reconciliation = organizations_import(org['Image'], env)
        emit('organizationsReconciliation', {'mismatches': reconciliation['mismatches'],
             'organizations': reconciliation['organizations'], 'memberships': reconciliation['memberships']})
        if reconciliation['mismatches'] or reconciliation['checksum'] != latest['checksum']:
            raise RuntimeError('Reconciliation mismatch')
        again = import_summary(organizations_import(org['Image'], env, export_path))
        if again['imported'] or again['conflicts']:
            raise RuntimeError('A repeated import was not fully unchanged')
    finally:
        env.unlink(missing_ok=True)
    export_path.unlink()  # holds names and user ids: removed after step 3
    p.write_private(CUTOVER / 'latest-import.json', json.dumps(dict(runId=run_id, at=now(), **first)))
    record_step('org-cutover-import', sha, run_id)


def record_step(operation, sha, run_id):
    entry = begin(operation, sha, run_id)
    entry['status'] = 'done'
    commit(entry)


def org_shadow(sha, run_id, confirm):
    has_signing_identity('meeting-service')
    tls_route_present(SERVICES['organizations']['tls'])
    apply_activation('org-cutover-shadow', sha, run_id, [('meeting-service', {
        'ORGANIZATIONS_AUTHORITY_MODE': 'shadow',
        'CF_ORGANIZATIONS_BASE_URL': private_url(SERVICES['organizations']['tls'])})])


def org_authoritative(sha, run_id, confirm):
    if not ('quiesced' in confirm and 'offsite-backup-verified' in confirm):
        raise RuntimeError('Owner gates 1 and 6: confirm with offsite-backup-verified,quiesced after the legacy '
                           'organization write routes are blocked and the access log shows none succeeded')
    if env_of('meeting-service').get('ORGANIZATIONS_AUTHORITY_MODE') != 'shadow':
        raise RuntimeError('The Meeting Service must be in shadow mode first')
    org_export(sha, run_id + '-final', confirm)
    org_import(sha, run_id + '-final', confirm)
    apply_activation('org-cutover-authoritative', sha, run_id,
                     [('meeting-service', {'ORGANIZATIONS_AUTHORITY_MODE': 'authoritative'})], irreversible=True)


def activate_enterprise_calling(sha, run_id, confirm):
    meeting = env_of('meeting-service')
    if meeting.get('ORGANIZATIONS_AUTHORITY_MODE') != 'authoritative':
        raise RuntimeError('Per-organization Enterprise Calling requires authoritative Organizations')
    overrides = {'ORGANIZATION_CALLING_SCOPE': 'organization'}
    for var, service in (('CF_CALLING_BASE_URL', 'calling'), ('CF_MESSAGING_BASE_URL', 'messaging')):
        if not meeting.get(var):
            overrides[var] = private_url(SERVICES[service]['tls'])
    apply_activation('activate-enterprise-calling', sha, run_id, [
        ('calling', {'CALLING_ORGANIZATION_FORWARDING_ENABLED': 'true'}), ('meeting-service', overrides)])


def activate_developer_platform(sha, run_id, confirm):
    tls_route_present(SERVICES['developer-platform']['tls'])
    apply_activation('activate-developer-platform', sha, run_id, [
        ('developer-platform', {'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'true'}),
        ('api-gateway', {'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'true',
                         'CF_DEVELOPER_PLATFORM_BASE_URL': private_url(SERVICES['developer-platform']['tls'])})])


def first_party_feature_changes(meeting_env):
    """The supported first-party features that depend on nothing external. Notification email
    only when the Meeting Service already carries real SMTP credentials. The healthcare profile is
    a compliance decision and is never switched here."""
    meeting = {'FEATURE_CONFERENCE_REALTIME_ENABLED': 'true', 'FEATURE_WEBINAR_MODE_ENABLED': 'true',
               'FEATURE_QUALITY_TELEMETRY_ENABLED': 'true'}
    if mail_available(meeting_env):
        meeting['NOTIFICATIONS_EMAIL_ENABLED'] = 'true'
    # MANAGED realtime only: session devices never acquire E2EE privileges.
    return [('messaging', {'MESSAGING_SESSION_DEVICES_ENABLED': 'true'}), ('meeting-service', meeting)]


def activate_first_party_features(sha, run_id, confirm):
    if env_of('messaging').get('MESSAGING_E2EE_DEVICES_ENABLED') == 'true':
        raise RuntimeError('PRIVATE_E2EE devices are on in production; refusing to change Messaging')
    changes = first_party_feature_changes(env_of('meeting-service'))
    if 'NOTIFICATIONS_EMAIL_ENABLED' not in changes[1][1]:
        emit('notificationEmail', 'not enabled: SMTP credentials are not configured on the Meeting Service')
    apply_activation('activate-first-party-features', sha, run_id, changes)


def web_origin_checks():
    """Every production web origin passes the gateway's CORS preflight; a foreign origin does not."""
    checks = {}
    for origin, allowed in tuple((o, True) for o in WEB_ORIGINS) + (('https://evil.example', False),):
        status, headers = http_status(PUBLIC + '/v1/developer-platform/context', 'OPTIONS', {
            'Origin': origin, 'Access-Control-Request-Method': 'GET', 'Access-Control-Request-Headers': 'authorization'})
        got = {k.lower(): v for k, v in headers.items()}.get('access-control-allow-origin')
        ok = (status < 400 and got == origin) if allowed else got != origin
        checks['cors ' + origin] = 'PASS' if ok else 'FAIL %d' % status
    return checks


def activate_web_origins(sha, run_id, confirm):
    """The API Gateway answers CORS for every production web origin (cfmeet.com and the
    www.cfmeet.com it redirects to). Exact origins only, never a wildcard; nothing else changes."""

    def post():
        checks = web_origin_checks()
        emit('webOriginChecks', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Gateway CORS does not match the production web origins')

    apply_activation('activate-web-origins', sha, run_id, [
        ('api-gateway', {'API_GATEWAY_CORS_ALLOWED_ORIGINS': ','.join(WEB_ORIGINS)})], post=post)


def activate_developer_console(sha, run_id, confirm):
    """Developer Console sessions (Part VI §50): the Developer Platform verifies a CF Meet
    session through the Meeting Service's session bridge (`user-session.verify`); without it
    every DEVELOPER_SESSION request is refused and no customer can create an organization,
    project, environment, credential or webhook. The bridge fails closed (revocation store
    must be reachable; guests and participants never authenticate a developer)."""
    tls_route_present(SERVICES['meeting-service']['tls'])
    has_signing_identity('developer-platform')
    if not env_of('meeting-service').get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('Meeting Service has no receiver trust bundle')
    apply_activation('activate-developer-console', sha, run_id, [
        ('meeting-service', {'DEVELOPER_SESSION_BRIDGE_ENABLED': 'true'}),
        ('developer-platform', {'CF_MEETING_SERVICE_BASE_URL': private_url(SERVICES['meeting-service']['tls'])})])


WEBHOOK_KEY = 'developer-platform-webhook-secret.key'
WEBHOOK_KEY_TARGET = '/run/secrets/webhook-secret.key'
WEBHOOK_KEY_ID = 'prod-1'
WEBHOOK_EGRESS_NET = 'cfmeet-webhook-egress'
CALLING_WEBHOOK_STATE_SQL = ("SELECT (SELECT count(*) FROM webhook_subscriptions WHERE status <> 'DELETED') || ',' || "
                             "(SELECT count(*) FROM webhook_deliveries WHERE status NOT IN "
                             "('SUCCEEDED','FAILED','CANCELLED','EXPIRED'));")


def webhook_preconditions():
    """The Developer Platform becomes the only webhook authority without the Calling export
    only when Calling holds no customer webhook state, and no Calling migration ever began."""
    subscriptions, open_deliveries = release.db_sql('cfmeet_calling', CALLING_WEBHOOK_STATE_SQL).split(',')
    if int(subscriptions) or int(open_deliveries):
        raise RuntimeError('Calling holds %s webhook subscriptions / %s open deliveries: follow the webhook '
                           'migration runbook (phase-06) instead' % (subscriptions, open_deliveries))
    started = release.db_sql('cfmeet_developer_platform', 'SELECT count(*) FROM devplat_webhook_migration_state;')
    if int(started):
        raise RuntimeError('A Calling webhook migration is recorded; finish it with the migration runbook')
    emit('webhookPreconditions', {'callingSubscriptions': 0, 'callingOpenDeliveries': 0, 'migrationState': 'none'})


def webhook_egress_network():
    """A dedicated, non-internal bridge for the delivery worker only (cfmeet-core-private is
    --internal). The worker's STRICT endpoint policy (packages/webhooks) allows https only and
    refuses private, loopback, link-local and metadata addresses, re-checked at connect time,
    so this network reaches public customer endpoints and nothing on the host."""
    if WEBHOOK_EGRESS_NET not in p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
        p.run('docker', 'network', 'create', '--driver', 'bridge', '--opt',
              'com.docker.network.bridge.enable_icc=false', WEBHOOK_EGRESS_NET)
    return WEBHOOK_EGRESS_NET


def activate_developer_webhooks(sha, run_id, confirm):
    """Customer webhooks on the Developer Platform: signed (HMAC, sealed per-subscription
    secrets under a 32-byte key ring), retried with jittered exponential backoff, auto-disabled
    after repeated failures, observable through deliveries/attempts and replay. Calling's
    legacy webhook authority is retired (it holds no customer state, checked first) and every
    first-party producer publishes to the central ingestion."""
    if env_of('developer-platform').get('DEVELOPER_PLATFORM_GATEWAY_API_ENABLED') != 'true':
        raise RuntimeError('Run activate-developer-platform first')
    tls_route_present(SERVICES['developer-platform']['tls'])
    webhook_preconditions()
    uid = SERVICES['developer-platform']['uid']
    key = p.SECRETS / WEBHOOK_KEY
    secret_file(key, lambda: base64.b64encode(secrets.token_bytes(32)).decode() + '\n', uid=uid, mode=0o400)
    network = webhook_egress_network()
    dp = private_url(SERVICES['developer-platform']['tls'])
    webhooks = {'DEVELOPER_PLATFORM_WEBHOOKS_ENABLED': 'true', 'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': 'active',
                'DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE': WEBHOOK_KEY_TARGET,
                'DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID': WEBHOOK_KEY_ID}
    mount = {'mounts': [(key, WEBHOOK_KEY_TARGET)]}
    apply_activation('activate-developer-webhooks', sha, run_id, [
        ('developer-platform', webhooks, mount),
        # Credential lifecycle notices (credential.expiring_soon / expired) every 5 minutes.
        ('developer-platform-worker', dict(webhooks, DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS='300'),
         dict(mount, networks=[network])),
        ('calling', {'CALLING_WEBHOOK_AUTHORITY': 'retired', 'CALLING_WEBHOOK_DELIVERY_ENABLED': 'false',
                     'CALLING_PUBLIC_EVENTS_DESTINATION': 'developer-platform', 'CF_DEVELOPER_PLATFORM_BASE_URL': dp}),
        ('messaging', {'MESSAGING_PUBLIC_EVENTS_DESTINATION': 'developer-platform', 'CF_DEVELOPER_PLATFORM_BASE_URL': dp}),
        ('meeting-service', {'MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION': 'developer-platform',
                             'CF_DEVELOPER_PLATFORM_BASE_URL': dp})])


# Customer API families the gateway serves in addition to the Developer Platform families.
# Calling's /v1/calls, /v1/calling and /v1/enterprise and Messaging's /v1/realtime keep their
# existing direct routes (working production functionality is not re-routed). Numbers, SMS
# and Privacy Relay stay unrouted and their gateway domains disabled (excluded by the owner).
FAMILY_PREFIXES = ('/v1/abuse-reports', '/v1/blocks', '/v1/conversations', '/v1/limits', '/v1/messaging',
                   '/v1/usage', '/v1/webhook-deliveries', '/v1/webhooks')
DISABLED_DOMAINS = 'numbering,sms,privacy-relay'
# One real route of each family (a bare prefix can be unknown to the route table).
FAMILY_PROBES = (('GET', '/v1/abuse-reports'), ('GET', '/v1/blocks'), ('GET', '/v1/conversations'),
                 ('GET', '/v1/limits'), ('GET', '/v1/messaging/audit'), ('GET', '/v1/usage'),
                 ('POST', '/v1/webhook-deliveries/00000000-0000-4000-8000-000000000000/replay'),
                 ('GET', '/v1/webhooks'))


def activate_gateway_families(sha, run_id, confirm):
    """Messaging (/v1/conversations, /v1/messaging), Calling account controls (/v1/abuse-reports,
    /v1/blocks, /v1/limits, /v1/usage) and central customer webhooks (/v1/webhooks,
    /v1/webhook-deliveries → Developer Platform) through the API Gateway: platform credential
    authentication at the gateway, entitlement quotas and rate limits, a request-bound service
    token that the receiver authorizes by grant, tenant context delegated, never forwarded."""
    if env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true' or not GATEWAY_SNIPPET.exists():
        raise RuntimeError('Run activate-gateway-routing first')
    site = SITE.read_text()
    snippets = [f.read_text() for f in Path('/etc/nginx/snippets').glob('cfmeet-*.conf') if f != GATEWAY_SNIPPET]
    collisions = route_collisions([site] + snippets, FAMILY_PREFIXES)
    if collisions:
        raise RuntimeError('Existing public routes already serve: ' + ', '.join(collisions))
    old_snippet = GATEWAY_SNIPPET.read_text()

    def post():
        GATEWAY_SNIPPET.write_text(gateway_snippet(GATEWAY_PREFIXES + FAMILY_PREFIXES,
                                                   docs=done('activate-gateway-docs')))
        try:
            p.reload_host_nginx()
            checks = gateway_family_checks()
            emit('gatewayFamilies', checks)
            if any(v != 'PASS' for v in checks.values()):
                raise RuntimeError('Gateway family checks failed')
        except Exception:
            GATEWAY_SNIPPET.write_text(old_snippet)
            p.reload_host_nginx()
            raise

    apply_activation('activate-gateway-families', sha, run_id, [
        ('api-gateway', {'CF_CALLING_BASE_URL': private_url(SERVICES['calling']['tls']),
                         'CF_MESSAGING_BASE_URL': private_url(SERVICES['messaging']['tls']),
                         'API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED': 'true',
                         'API_GATEWAY_DISABLED_DOMAINS': DISABLED_DOMAINS})], post=post)


def gateway_family_checks():
    """Unauthenticated requests on every new family are refused by the gateway (401/403, never
    2xx/404/5xx); a forged bearer is refused; CORS answers the web origin only; excluded
    families stay unrouted; internal paths stay private."""
    checks = {}
    for method, path in FAMILY_PROBES:
        status, _ = http_status(PUBLIC + path, method, {'Content-Type': 'application/json'} if method == 'POST' else None)
        checks['anonymous %s %s' % (method, path)] = 'PASS' if status in (401, 403) else 'FAIL %d' % status
    status, _ = http_status(PUBLIC + '/v1/conversations', headers={'Authorization': 'Bearer cfk_invalid'})
    checks['forged credential'] = 'PASS' if status in (401, 403) else 'FAIL %d' % status
    for origin, allowed in tuple((o, True) for o in WEB_ORIGINS) + (('https://evil.example', False),):
        _, headers = http_status(PUBLIC + '/v1/conversations', 'OPTIONS', {
            'Origin': origin, 'Access-Control-Request-Method': 'GET'})
        got = {k.lower(): v for k, v in headers.items()}.get('access-control-allow-origin')
        checks['cors ' + origin] = 'PASS' if (got == origin) == allowed else 'FAIL'
    for path in ('/v1/numbers', '/v1/sms/messages', '/v1/privacy-relay/contexts'):
        status, _ = http_status(PUBLIC + path)
        checks['excluded ' + path] = 'PASS' if status in (401, 403, 404) else 'FAIL %d' % status
    checks.update(public_smoke())
    return checks


USAGE_STREAMS_SQL = ("SELECT coalesce(json_agg(json_build_object('producer', producer, 'stream', stream, "
                     "'positionAt', position_at) ORDER BY producer, stream), '[]') FROM devplat_usage_streams;")


def activate_usage_export(sha, run_id, confirm):
    """Part VI §55 unified usage ledger: Calling, Messaging and the Meeting Service export their
    billing-neutral usage facts (safe refs only; platform application environments only, never
    CF Meet consumer usage) to the Developer Platform, authenticated by the
    `developer-event.publish` grants of activate-developer-webhooks. Customer /v1/usage and
    /v1/limits stay served by Calling (API_GATEWAY_CENTRAL_USAGE_ENABLED is not switched)."""
    for name in ('calling', 'messaging', 'meeting-service'):
        if not env_of(name).get('CF_DEVELOPER_PLATFORM_BASE_URL'):
            raise RuntimeError('%s has no Developer Platform target; run activate-developer-webhooks' % name)

    def post():
        time.sleep(60)
        streams = json.loads(release.db_sql('cfmeet_developer_platform', USAGE_STREAMS_SQL) or '[]')
        emit('usageStreams', {'acknowledgedStreams': streams})

    apply_activation('activate-usage-export', sha, run_id, [
        (name, {'CFMEET_USAGE_EXPORT_ENABLED': 'true'}) for name in ('calling', 'messaging', 'meeting-service')], post=post)


# --- API Gateway public routing (host nginx, api.cfmeet.com) -----------------------------------

# Only the Developer Platform families: every other public family keeps its existing direct
# Meeting / Calling / Messaging route. Numbers and SMS stay off until providers exist.
GATEWAY_PREFIXES = ('/v1/commercial-plans', '/v1/developer-platform', '/v1/environments', '/v1/invitations',
                    '/v1/organizations', '/v1/production-access-requests', '/v1/projects', '/v1/scopes')
GATEWAY_RELAY = 'cfmeet-gateway-ingress'
GATEWAY_RELAY_NET = 'cfmeet-gateway-ingress-net'
GATEWAY_RELAY_PORT = 18110
SITE = Path('/etc/nginx/sites-available/api.cfmeet.com')
SITE_LINK = Path('/etc/nginx/sites-enabled/api.cfmeet.com')
GATEWAY_SNIPPET = Path('/etc/nginx/snippets/cfmeet-api-gateway.conf')
GATEWAY_INCLUDE = 'include /etc/nginx/snippets/cfmeet-api-gateway.conf;'
ROUTES_ANCHOR = 'include /etc/nginx/snippets/cfmeet-calling-routes.conf;'


# The API Gateway's public documentation (services/api-gateway/src/docs.rs): the catalogue, the
# aggregate OpenAPI and the read-only reference are exact paths; the per-domain documents the
# reference and the catalogue link to live under /openapi/. GET only (and CORS preflight).
DOCS_EXACT = ('/discovery', '/openapi.json', '/docs')
DOCS_PREFIX = '/openapi'


def gateway_snippet(prefixes=GATEWAY_PREFIXES, docs=False):
    common = ('    proxy_pass http://127.0.0.1:%d;\n    proxy_http_version 1.1;\n    proxy_set_header Host $host;\n'
              '    proxy_set_header X-Forwarded-Proto https;\n    client_max_body_size 1m;\n    proxy_read_timeout 60s;\n'
              % GATEWAY_RELAY_PORT)
    scope = 'Developer Platform families' if tuple(prefixes) == GATEWAY_PREFIXES else 'customer API families'
    out = ['# CF Meet API Gateway: public %s only (coordinated release).' % scope,
           '# Never /internal, operator or provider-callback paths; existing routes are unchanged.']
    for prefix in prefixes:
        assert prefix.startswith('/v1/') and '/internal' not in prefix and '/admin' not in prefix
        out.append('location = %s {\n%s}' % (prefix, common))
        out.append('location ^~ %s/ {\n%s}' % (prefix, common))
    if docs:
        read_only = common + '    limit_except GET OPTIONS { deny all; }\n'
        out.append('# Public API documentation and discovery (activate-gateway-docs).')
        for path in DOCS_EXACT:
            out.append('location = %s {\n%s}' % (path, read_only))
        out.append('location ^~ %s/ {\n%s}' % (DOCS_PREFIX, read_only))
    return '\n'.join(out) + '\n'


def edge_prefixes():
    """The /v1 families the edge currently routes to the gateway (journal-derived)."""
    return GATEWAY_PREFIXES + (FAMILY_PREFIXES if done('activate-gateway-families') else ())


def route_collisions(texts, prefixes=GATEWAY_PREFIXES):
    """Existing explicit locations for any gateway prefix (refuse rather than shadow them)."""
    found = []
    for text in texts:
        for prefix in prefixes:
            if re.search(r'location\s+(=|\^~|~\*?)?\s*%s(/|\s|\{)' % re.escape(prefix), text):
                found.append(prefix)
    return sorted(set(found))


def gateway_relay_config():
    return ('daemon off;\nworker_processes 1;\npid /tmp/nginx.pid;\nerror_log stderr warn;\n'
            'events { worker_connections 512; }\nstream {\n    resolver 127.0.0.11 valid=10s ipv6=off;\n'
            '    server {\n        listen 8099;\n        set $peer %s:8099;\n        proxy_pass $peer;\n'
            '        proxy_connect_timeout 5s;\n        proxy_timeout 300s;\n    }\n}\n' % SERVICES['api-gateway']['container'])


def gateway_relay_args(image, config):
    return ['docker', 'create', '--name', GATEWAY_RELAY, '--restart', 'unless-stopped', '--network', GATEWAY_RELAY_NET,
            '--publish', '127.0.0.1:%d:8099' % GATEWAY_RELAY_PORT, '--read-only', '--tmpfs', '/tmp:rw,size=1m',
            '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--user', '101:101', '--memory', '32m',
            '--pids-limit', '32', '--log-opt', 'max-size=5m', '--log-opt', 'max-file=2',
            '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % config,
            '--entrypoint', 'nginx', image, '-e', 'stderr', '-c', '/etc/nginx/nginx.conf']


def activate_gateway_routing(sha, run_id, confirm):
    if env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true':
        raise RuntimeError('Run activate-developer-platform first (gateway routing is still disabled)')
    assert SITE_LINK.resolve() == SITE, 'Unexpected public site topology'
    site = SITE.read_text()
    if site.count(ROUTES_ANCHOR) != 1:
        raise RuntimeError('Unexpected public site topology (calling routes include)')
    snippets = [f.read_text() for f in Path('/etc/nginx/snippets').glob('cfmeet-*.conf') if f != GATEWAY_SNIPPET]
    collisions = route_collisions([site] + snippets)
    if collisions:
        raise RuntimeError('Existing public routes already serve: ' + ', '.join(collisions))
    old_snippet = GATEWAY_SNIPPET.read_text() if GATEWAY_SNIPPET.exists() else None
    evidence = WORK / 'gateway-routing' / run_id
    p.write_private(evidence / 'site.before', site)
    entry = begin('activate-gateway-routing', sha, run_id)
    entry['routing'] = {'relayCreated': False}
    commit(entry)
    try:
        if GATEWAY_RELAY not in container_names():
            config = WORK / 'gateway-relay.conf'
            config.write_text(gateway_relay_config())
            config.chmod(0o644)
            if GATEWAY_RELAY_NET not in p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
                p.run('docker', 'network', 'create', GATEWAY_RELAY_NET)
            _own(GATEWAY_RELAY)
            p.run(*gateway_relay_args(p.inspect('cfmeet-core-tls')['Image'], config))
            p.run('docker', 'network', 'connect', p.NET, GATEWAY_RELAY)
            start(GATEWAY_RELAY)
            entry['routing']['relayCreated'] = True
            commit(entry)
        for _ in range(15):
            try:
                with urllib.request.urlopen('http://127.0.0.1:%d/ready' % GATEWAY_RELAY_PORT, timeout=5) as r:
                    if r.status == 200:
                        break
            except (urllib.error.URLError, OSError):
                time.sleep(2)
        else:
            raise RuntimeError('Gateway relay does not reach the API Gateway')
        GATEWAY_SNIPPET.write_text(gateway_snippet())
        if GATEWAY_INCLUDE not in site:
            SITE.write_text(site.replace(ROUTES_ANCHOR, ROUTES_ANCHOR + '\n    ' + GATEWAY_INCLUDE))
        p.reload_host_nginx()  # runs nginx -t first
        status, _ = http_status(PUBLIC + '/v1/developer-platform/context')
        if status not in (401, 403):
            raise RuntimeError('Gateway family is not served by the gateway (status %d)' % status)
        status, headers = http_status(PUBLIC + '/v1/developer-platform/context', 'OPTIONS', {
            'Origin': WEB_ORIGIN, 'Access-Control-Request-Method': 'GET'})
        checks = public_smoke()
        checks['gateway_cors'] = 'PASS' if {k.lower(): v for k, v in headers.items()}.get(
            'access-control-allow-origin') == WEB_ORIGIN else 'FAIL'
        emit('gatewayRouting', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Public smoke failed after gateway routing')
        p.regression()
    except Exception:
        undo_gateway_routing(entry['routing'], old_snippet)
        entry['status'] = 'failed'
        commit(entry)
        raise
    entry['status'] = 'done'
    commit(entry)


def http_document(url, headers=None, limit=4 * 2**20):
    """(status, headers, body bytes) of a bounded GET."""
    request = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, dict(response.headers), response.read(limit)
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), b''


def gateway_docs_checks():
    """The documentation is served by the gateway, with truthful family states, CORS for the web
    origins only, GET only; gateway health stays private; existing routes are unchanged."""
    checks = {}
    status, headers, body = http_document(PUBLIC + '/discovery', {'Origin': 'https://www.cfmeet.com'})
    try:
        doc = json.loads(body)
    except ValueError:
        doc = {}
    checks['discovery'] = 'PASS' if status == 200 and doc.get('service') == 'cfmeet.api-gateway' and \
        doc.get('documentation') == {'aggregate': '/openapi.json', 'reference': '/docs'} else 'FAIL %d' % status
    families = {f.get('domain'): f for f in doc.get('families', [])}
    off = [d for d in ('numbering', 'sms', 'privacy-relay') if families.get(d, {}).get('enabled') is not False]
    checks['discovery: excluded telecom families reported disabled'] = 'PASS' if families and not off else \
        'FAIL ' + ','.join(off)
    for origin, allowed in tuple((o, True) for o in WEB_ORIGINS) + (('https://evil.example', False),):
        _, headers, _ = http_document(PUBLIC + '/discovery', {'Origin': origin})
        got = {k.lower(): v for k, v in headers.items()}.get('access-control-allow-origin')
        checks['cors GET /discovery ' + origin] = 'PASS' if (got == origin) == allowed else 'FAIL'
    status, _, body = http_document(PUBLIC + '/openapi.json')
    try:
        aggregate = json.loads(body)
    except ValueError:
        aggregate = {}
    checks['openapi aggregate'] = 'PASS' if status == 200 and str(aggregate.get('openapi', '')).startswith('3.') \
        and not any(p.startswith('/internal') for p in aggregate.get('paths', {})) else 'FAIL %d' % status
    status, headers, body = http_document(PUBLIC + '/docs')
    kind = {k.lower(): v for k, v in headers.items()}.get('content-type', '')
    checks['docs reference'] = 'PASS' if status == 200 and kind.startswith('text/html') and b'swagger-ui' in body \
        else 'FAIL %d' % status
    for domain in [d for d, f in sorted(families.items()) if f.get('enabled')][:3] or ['calling']:
        status, _, _ = http_document(PUBLIC + '/openapi/%s.json' % domain)
        checks['openapi domain ' + domain] = 'PASS' if status == 200 else 'FAIL %d' % status
    status, _ = http_status(PUBLIC + '/discovery', 'POST', {'Content-Type': 'application/json'})
    checks['documentation is read-only (POST refused)'] = 'PASS' if status in (403, 404, 405) else 'FAIL %d' % status
    # The edge's /health is the Meeting Service's own public health route; the gateway's /health
    # and /ready must never answer there (they identify as "api-gateway").
    for path in ('/ready', '/health'):
        _, _, body = http_document(PUBLIC + path)
        checks['gateway %s not public' % path] = 'FAIL' if b'api-gateway' in body else 'PASS'
    status, _ = http_status(PUBLIC + '/v1/developer-platform/context')
    checks['existing gateway family unchanged'] = 'PASS' if status in (401, 403) else 'FAIL %d' % status
    checks.update(public_smoke())
    return checks


def activate_gateway_docs(sha, run_id, confirm):
    """Routes the API Gateway's public documentation (/discovery, /openapi.json, /docs and the
    per-domain /openapi/{domain}.json they link to) at the api.cfmeet.com edge, for the Developer
    Console's Docs page. Edge only: no container, flag or grant changes. Refuses to shadow an
    existing location; any failed check restores the previous snippet."""
    if env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true' or not GATEWAY_SNIPPET.exists():
        raise RuntimeError('Run activate-gateway-routing first')
    site = SITE.read_text()
    if GATEWAY_INCLUDE not in site:
        raise RuntimeError('The gateway snippet is not included by the public site')
    snippets = [f.read_text() for f in Path('/etc/nginx/snippets').glob('cfmeet-*.conf') if f != GATEWAY_SNIPPET]
    collisions = route_collisions([site] + snippets, DOCS_EXACT + (DOCS_PREFIX,))
    if collisions:
        raise RuntimeError('Existing public routes already serve: ' + ', '.join(collisions))
    old_snippet = GATEWAY_SNIPPET.read_text()
    p.write_private(WORK / 'gateway-docs' / run_id / 'snippet.before', old_snippet)
    entry = begin('activate-gateway-docs', sha, run_id)
    entry['edge'] = {'docs': True, 'prefixes': list(edge_prefixes())}
    commit(entry)
    try:
        GATEWAY_SNIPPET.write_text(gateway_snippet(edge_prefixes(), docs=True))
        p.reload_host_nginx()  # nginx -t first
        checks = gateway_docs_checks()
        emit('gatewayDocs', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Gateway documentation checks failed')
        p.regression()
    except Exception:
        GATEWAY_SNIPPET.write_text(old_snippet)
        p.reload_host_nginx()
        entry['status'] = 'failed'
        commit(entry)
        raise
    entry['status'] = 'done'
    commit(entry)


def undo_gateway_docs(edge):
    """Removes only the documentation locations; the /v1 families stay as they are."""
    if GATEWAY_SNIPPET.exists():
        GATEWAY_SNIPPET.write_text(gateway_snippet(edge_prefixes(), docs=False))
        p.reload_host_nginx()


def undo_gateway_routing(routing, old_snippet=None):
    """Removes only this step's include, snippet and relay; later site changes are kept."""
    SITE.write_text(SITE.read_text().replace('\n    ' + GATEWAY_INCLUDE, ''))
    if old_snippet is None:
        GATEWAY_SNIPPET.unlink(missing_ok=True)
    else:
        GATEWAY_SNIPPET.write_text(old_snippet)
    p.reload_host_nginx()
    if routing.get('relayCreated') and GATEWAY_RELAY in container_names():
        remove(GATEWAY_RELAY)


# --- dispatcher ------------------------------------------------------------------------------

ACTIVATIONS = {
    'activate-provisioning': ((), activate_provisioning),
    'activate-org-events': (('activate-provisioning',), activate_org_events),
    'activate-organization-bindings': (('activate-provisioning',), activate_organization_bindings),
    'org-cutover-export': (('activate-org-events',), org_export),
    'org-cutover-import': (('org-cutover-export',), org_import),
    'org-cutover-shadow': (('org-cutover-import',), org_shadow),
    'org-cutover-authoritative': (('org-cutover-shadow',), org_authoritative),
    # Owner decision 2026-11-13: an empty dataset may skip import/shadow/7-day observation.
    'org-cutover-first-activation': (('activate-org-events',), lambda sha, run_id, confirm:
                                     org_first_activation.activate(sys.modules[__name__], sha, run_id, confirm)),
    # A tuple inside `requires` lists alternatives: either cutover procedure makes Organizations authoritative.
    # Organization Calling / Messaging need the automatic bindings (ADR 0058 Amendment A2).
    'activate-enterprise-calling': ((('org-cutover-authoritative', 'org-cutover-first-activation'),
                                     'activate-organization-bindings'), activate_enterprise_calling),
    'activate-developer-platform': ((), activate_developer_platform),
    'activate-gateway-routing': (('activate-developer-platform',), activate_gateway_routing),
    'activate-first-party-features': ((), activate_first_party_features),
    'activate-web-origins': (('activate-gateway-routing',), activate_web_origins),
    'activate-gateway-docs': (('activate-gateway-routing',), activate_gateway_docs),
    'activate-developer-console': (('activate-developer-platform',), activate_developer_console),
    'activate-developer-webhooks': (('activate-developer-console',), activate_developer_webhooks),
    'activate-gateway-families': (('activate-gateway-routing', 'activate-developer-webhooks'), activate_gateway_families),
    'activate-usage-export': (('activate-developer-webhooks',), activate_usage_export),
    # Final completion: Resend becomes the email provider only after a verified send.
    'activate-resend-email': ((), lambda sha, run_id, confirm: mail_ops.activate_resend_email(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-production-access-review': (('activate-developer-console',), lambda sha, run_id, confirm:
                                          developer_access.activate(sys.modules[__name__], sha, run_id, confirm)),
}
OPERATIONS = ('inspect', 'backup', 'provision', 'release', 'verify', 'rollback', 'certify') + tuple(ACTIVATIONS) \
    + ('developer-acceptance', 'organization-acceptance', 'diagnose', 'email-acceptance', 'developer-access-review')
# The HTTPS receiver of the acceptance webhook: a public endpoint answering 204 to any POST.
# (api.cfmeet.com itself resolves to the private TLS proxy on cfmeet-core-private, which the
# worker's STRICT endpoint policy refuses as a private address.)
ACCEPTANCE_WEBHOOK_RECEIVER = 'https://httpbin.org/status/204'


def run(operation, sha, run_id, confirm=''):
    if operation not in OPERATIONS:
        raise RuntimeError('Unknown coordinated release operation')
    if not SHA_RE.fullmatch(sha) or not RUN_RE.fullmatch(run_id):
        raise RuntimeError('Invalid SHA or run id')
    if operation == 'certify':
        # Read-only: no journal entry, no work directories, no confirmation accepted.
        if confirm:
            raise RuntimeError('certify takes no confirmation')
        import live_certification
        live_certification.certify(sha, run_id)
        return
    for folder in (WORK, WORK / 'run', WORK / 'env', WORK / 'trust'):
        folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    if operation == 'inspect':
        inspect(sha, run_id)
    elif operation == 'diagnose':
        # Read-only: mail configuration and failure evidence, email ledger, conference lifecycle.
        if confirm:
            raise RuntimeError('diagnose takes no confirmation')
        mail_ops.diagnose(sys.modules[__name__], sha, run_id)
    elif operation == 'developer-access-review':
        # One audited operator decision (or read) by a named reviewer; no image change.
        developer_access.review(sys.modules[__name__], sha, run_id, confirm)
    elif operation == 'email-acceptance':
        # Writes one acceptance email (and with confirm=otp one deactivated acceptance account).
        if not done('activate-resend-email'):
            raise RuntimeError('Acceptance needs activate-resend-email')
        mail_ops.email_acceptance(sys.modules[__name__], sha, run_id, confirm)
    elif operation == 'backup':
        backup(sha, run_id)
    elif operation == 'provision':
        provision(sha, run_id)
    elif operation == 'release':
        release_all(sha, run_id)
    elif operation == 'verify':
        verify(sha, run_id)
    elif operation == 'rollback':
        rollback(sha, run_id)
    elif operation == 'organization-acceptance':
        # Writes acceptance data (three fresh accounts, two organizations it archives again)
        # through the public API only; requires every organization activation it tests.
        missing = [o for o in ('activate-organization-bindings', 'activate-enterprise-calling') if not done(o)]
        if not (done('org-cutover-authoritative') or done('org-cutover-first-activation')):
            missing.insert(0, 'org-cutover-authoritative or org-cutover-first-activation')
        if missing:
            raise RuntimeError('Acceptance needs ' + ', '.join(missing))
        import organization_acceptance
        checks = organization_acceptance.run(run_id, emit)
        emit('organizationAcceptanceSummary', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Organization acceptance failed')
    elif operation == 'developer-acceptance':
        # Writes acceptance data through the public API only; requires the activations it tests.
        missing = [o for o in ('activate-developer-console', 'activate-developer-webhooks', 'activate-gateway-families')
                   if not done(o)]
        if missing:
            raise RuntimeError('Acceptance needs ' + ', '.join(missing))
        import developer_acceptance
        checks = developer_acceptance.run(run_id, ACCEPTANCE_WEBHOOK_RECEIVER)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Developer acceptance failed')
    else:
        requires, step = ACTIVATIONS[operation]
        missing = [' or '.join(r) if isinstance(r, tuple) else r for r in requires
                   if not (any(done(x) for x in r) if isinstance(r, tuple) else done(r))]
        if missing:
            raise RuntimeError('Activation order: complete %s first' % ', '.join(missing))
        p.identity()
        require_release(sha)
        step(sha, run_id, confirm)
        emit('activationComplete', operation)


def main(argv):
    try:
        if len(argv) not in (3, 4):
            raise RuntimeError('usage: coordinated_release.py <operation> <sha> <run id> [confirmation]')
        run(*argv)
    except Exception as exc:
        # Library errors can contain connection strings: print the type, and the message
        # only for this module's own assertions and runtime errors (scrubbed of URLs).
        print('COORDINATED_RELEASE_FAILED:', type(exc).__name__,
              scrub(str(exc)) if isinstance(exc, (AssertionError, RuntimeError)) else '', flush=True)
        return 1
    return 0


if __name__ == '__main__':
    os.umask(0o077)
    sys.exit(main(sys.argv[1:]))
