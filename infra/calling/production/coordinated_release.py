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
  deploy-monitoring            Prometheus, Alertmanager, blackbox and heartbeat exporters
                               (monitoring_stack.py); also run after every successful release,
                               where a monitoring failure is reported but never fails it.
  activate-* / org-cutover-*   separately approved activation steps (see ACTIVATIONS); the CF Meet
                               Platform "Build" phases live in platform_operations.py, the CF Meet
                               Apps ("Integrate") phases in integrate_operations.py.
  platform-acceptance          two fresh developer workspaces through the public API (cleaned up):
                               every recorded platform phase, cross-tenant refusals, flag-off checks.

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
import stat
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
import notification_operations as notif_ops
import key_rotation
import platform_operations as platform_ops
import integrate_operations as integrate_ops
import meeting_recording_operations
import send_policy_operations
import kt_witness_operations
import audit_operations
import provider_operations

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
# M1: production answers browser requests from the production web origins only. Every
# release of an existing service re-applies these settings (staging and local keep theirs;
# they never run through this module), and verify checks them.
RELEASE_PINNED_ENV = {'meeting-service': {'FRONTEND_URLS': ','.join(WEB_ORIGINS)}}
SHA_RE = re.compile(r'[a-f0-9]{40}')
RUN_RE = re.compile(r'[0-9]{1,20}(-[0-9]{1,4})?')  # run id, optionally -attempt


def private_url(port):
    """Internal HTTPS URL through the private TLS proxy (Docker alias api.cfmeet.com)."""
    return 'https://api.cfmeet.com:%d' % port


# --- Service catalogue -------------------------------------------------------------------

def _service(container, port, health, *, tls=None, db=None, role=None, migrations=(),
             history=(), memory=None, uid=10001, image=None, identity=None, command=(),
             existing=False, env=None, upstream=None, probe=None):
    """upstream: the container port the private TLS route forwards to (default `port`);
    probe: (path, expected status) a caller checks that route with (default (health, 200))."""
    return dict(container=container, port=port, health=health, tls=tls, db=db, role=role,
                migrations=tuple(migrations), history=tuple(history), memory=memory, uid=uid,
                image=image, identity=identity, command=list(command), existing=existing,
                env=dict(env or {}), upstream=upstream or port, probe=probe or (health, 200))


NOTIFICATIONS_DATA_KEY_TARGET = '/run/secrets/notifications-data.key'
NOTIFICATIONS_DARK_ENV = {
    'NOTIFICATIONS_BIND_ADDRESS': '0.0.0.0:8102',
    'NOTIFICATIONS_EMAIL_PROVIDER': 'none',
    'NOTIFICATIONS_PUSH_PROVIDER': 'none',
    'NOTIFICATIONS_DATA_KEY_ID': 'notifications-data-1',
    'NOTIFICATIONS_DATA_KEY_FILE': NOTIFICATIONS_DATA_KEY_TARGET,
    # B6 / C5: raised (to DATABASE_POOLS) only by activate-database-pools, after checking the
    # core database's connection budget.
    'NOTIFICATIONS_DATABASE_MAX_CONNECTIONS': '8',
}

# CF Meet Admin Bridge (ADR 0068, docs/admin/cfmeet-admin-bridge.md): the only doorway from
# the CosmicForge central admin (HQ). Deployed dark: ADMIN_BRIDGE_ENABLED, _WRITES_ENABLED and
# every module switch off, no owner base URL. Only the loopback ops listener answers (the
# release health probe runs inside the container's network namespace); the HQ mTLS listener
# is not started while disabled.
ADMIN_BRIDGE_MODULES = ('ACCOUNTS', 'MEETINGS', 'MESSAGING', 'CALLING', 'COMMUNICATIONS', 'ORGANIZATIONS',
                        'PARTNERS', 'IDENTITY', 'APPS', 'VERIFICATION', 'PRICING', 'FINANCE', 'NOTIFICATIONS',
                        'PRIVACY', 'OPERATIONS', 'AUDIT', 'TELECOM')
ADMIN_BRIDGE_DARK_ENV = dict({
    'ADMIN_BRIDGE_ENABLED': 'false',
    'ADMIN_BRIDGE_WRITES_ENABLED': 'false',
    'ADMIN_BRIDGE_OPS_BIND_ADDRESS': '127.0.0.1:8190',
    'ADMIN_BRIDGE_HQ_BIND_ADDRESS': '0.0.0.0:8443',
    'ADMIN_BRIDGE_DATABASE_MAX_CONNECTIONS': '5',
}, **{'ADMIN_BRIDGE_MODULE_' + m: 'false' for m in ADMIN_BRIDGE_MODULES})

# The HQ port (design §14; owner decision 2026-11-20: private tunnel). HQ staging reaches the
# bridge over a private tunnel (e.g. WireGuard) between the HQ staging host and the CF Meet
# host, never over a public address: HQ only connects to a pinned private IP. The relay
# listens ONLY on the CF Meet host's tunnel address (ADMIN_BRIDGE_HQ_LISTEN_ADDRESS) and admits
# ONLY HQ's tunnel address (ADMIN_BRIDGE_HQ_ALLOWED_SOURCE); both must be RFC 1918 private IPv4
# (HQ refuses 100.64/10 as shared CGNAT space, never private). TLS is passed straight through (the bridge terminates mutual TLS and checks
# HQ's client certificate on every request). It is NEVER routed by api.cfmeet.com or any
# public ingress and is never part of the private TLS proxy (which terminates TLS). Declared
# here; the tunnel and relay are installed by the HQ enablement step of the runbook (a
# privileged operator step); the relay by activate-admin-bridge-hq, never by `release`.
ADMIN_BRIDGE_HQ_PORT = 8460
ADMIN_BRIDGE_HQ_RELAY = 'cfmeet-admin-bridge-hq-relay'
ADMIN_BRIDGE_HQ_RELAY_NET = 'cfmeet-admin-bridge-hq-net'
ADMIN_BRIDGE_HQ_SOURCE_ENV = 'ADMIN_BRIDGE_HQ_ALLOWED_SOURCE'
ADMIN_BRIDGE_HQ_LISTEN_ENV = 'ADMIN_BRIDGE_HQ_LISTEN_ADDRESS'
ADMIN_BRIDGE_SERVER_NAME = 'admin-bridge.cfmeet.internal'

# Owner events (ADR 0068 A7, design §11): the bridge's optional third listener, plain HTTP on
# the private network (ADMIN_BRIDGE_EVENTS_BIND_ADDRESS; started only while the bridge is
# enabled), behind its own private TLS route like every other internal service. Owners post
# POST /internal/v1/hq-bridge/events to private_url(ADMIN_BRIDGE_EVENTS_TLS) with a
# cf-service+jwt for cfmeet.admin-bridge granted hq-bridge-events.publish in the bridge's
# trust bundle. Unset (the dark default) nothing listens; activate-admin-bridge-events sets it.
# Unrelated to the HQ port: HQ never reaches this listener and owners never reach 8443.
ADMIN_BRIDGE_EVENTS_PORT = 8191
ADMIN_BRIDGE_EVENTS_BIND = '0.0.0.0:%d' % ADMIN_BRIDGE_EVENTS_PORT
ADMIN_BRIDGE_EVENTS_TLS = 8457
ADMIN_BRIDGE_EVENTS_PATH = '/internal/v1/hq-bridge/events'


def _private_ipv4(value, var, allow_prefix):
    """An RFC 1918 private IPv4 address, optionally a CIDR of /24 or narrower. 100.64/10 is
    refused: HQ treats it as shared CGNAT space, never private."""
    v = (value or '').strip()
    m = re.fullmatch(r'(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(/(\d{1,2}))?', v)
    if not m or any(int(o) > 255 for o in m.groups()[:4]):
        raise RuntimeError('%s must be a private IPv4 address' % var)
    if m.group(5) and (not allow_prefix or not 24 <= int(m.group(6)) <= 32):
        raise RuntimeError('%s must be one private address%s' % (var, ' or a CIDR of /24 or narrower' if allow_prefix else ''))
    a, b = int(m.group(1)), int(m.group(2))
    private = a == 10 or (a == 172 and 16 <= b <= 31) or (a == 192 and b == 168)
    if not private:
        raise RuntimeError('%s must be an RFC 1918 private (tunnel) address, never public or CGNAT' % var)
    return v


def admin_bridge_hq_relay_config(allowed_source):
    """nginx stream relay (TCP passthrough, no TLS termination) admitting only HQ's tunnel
    address. It runs in a container on its own network (published ONLY on the CF Meet host's
    tunnel address, admin_bridge_hq_relay_args) joined to the private network, so Docker's DNS
    finds the bridge after every recreate. Traffic arriving on the tunnel interface is DNATed
    to the container with its source address intact, so `allow` sees HQ's tunnel address.
    Refuses public, wildcard or malformed addresses."""
    source = _private_ipv4(allowed_source, ADMIN_BRIDGE_HQ_SOURCE_ENV, allow_prefix=True)
    return ('daemon off;\nworker_processes 1;\npid /tmp/nginx.pid;\nerror_log stderr warn;\n'
            'events { worker_connections 256; }\nstream {\n    resolver 127.0.0.11 valid=10s ipv6=off;\n'
            '    server {\n        listen %d;\n        allow %s;\n        deny all;\n'
            '        set $peer %s:8443;\n        proxy_pass $peer;\n'
            '        proxy_connect_timeout 5s;\n        proxy_timeout 60s;\n    }\n}\n'
            % (ADMIN_BRIDGE_HQ_PORT, source, SERVICES['admin-bridge']['container']))


def admin_bridge_hq_relay_args(image, config, listen_address):
    """The relay container: published on the CF Meet host's tunnel address only (never 0.0.0.0
    or a public address), read-only, no capabilities, unprivileged."""
    listen = _private_ipv4(listen_address, ADMIN_BRIDGE_HQ_LISTEN_ENV, allow_prefix=False)
    return ['docker', 'create', '--name', ADMIN_BRIDGE_HQ_RELAY, '--restart', 'unless-stopped',
            '--network', ADMIN_BRIDGE_HQ_RELAY_NET, '--publish', '%s:%d:%d/tcp' % (listen, ADMIN_BRIDGE_HQ_PORT, ADMIN_BRIDGE_HQ_PORT),
            '--read-only', '--tmpfs', '/tmp:rw,size=1m', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
            '--user', '101:101', '--memory', '32m', '--pids-limit', '32', '--log-opt', 'max-size=5m',
            '--log-opt', 'max-file=2', '--mount', 'type=bind,src=%s,dst=/etc/nginx/nginx.conf,readonly' % config,
            '--entrypoint', 'nginx', image, '-e', 'stderr', '-c', '/etc/nginx/nginx.conf']

SERVICES = {
    'platform-access': _service('cfmeet-platform-access', 8094, '/ready', tls=8443,
        db='cfmeet_platform_access', role='platform_access', migrations=['platform-access-migration'],
        history=['platform_access_seaql_migrations'], existing=True),
    'organizations': _service('cfmeet-organizations', 8101, '/ready', tls=8450,
        db='cfmeet_organizations', role='organizations', migrations=['organizations-migration'],
        history=['organizations_seaql_migrations'], memory=192,
        env={'ORGANIZATIONS_BIND_ADDRESS': '0.0.0.0:8101', 'ORGANIZATIONS_PROVISIONING_ENABLED': 'false'}),
    # Unified Notification Service (docs/audits/notification-service, roadmap N2): two replicas
    # of one image, identity and database, coordinating only through the database. Deployed
    # dark: every route OFF, no email or push provider, no owner API; the data key (sealing at
    # rest + keyed digests) is generated on the VPS by provision.
    'notifications': _service('cfmeet-notifications', 8102, '/ready', tls=8456, db='cfmeet_notifications',
        role='notifications', migrations=['notifications-migration'], history=['notifications_seaql_migrations'],
        memory=256, env=dict(NOTIFICATIONS_DARK_ENV, NOTIFICATIONS_REPLICA_ID='notifications-1')),
    'notifications-2': _service('cfmeet-notifications-2', 8102, '/ready', db='cfmeet_notifications',
        role='notifications', image='notifications', identity='notifications', memory=256,
        env=dict(NOTIFICATIONS_DARK_ENV, NOTIFICATIONS_REPLICA_ID='notifications-2')),
    'messaging': _service('cfmeet-messaging', 8097, '/ready', tls=8444, db='cfmeet_messaging',
        role='messaging', migrations=['messaging-migration', 'messaging-communications-migration',
                                      'messaging-notification-fence-migration', 'messaging-send-policy-projection-migration',
                                      'messaging-history-transfer-v2-migration'],
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
                    'lifecycle-delivery-migration', 'notification-fence-migration',
                    'gap-closure-migration', 'platform-build-migration', 'meeting-audit-migration',
                    'account-privacy-migration','account-phone-migration','admin-operations-migration',
                    'messaging-installations-migration', 'meeting-concurrent-devices-migration'],
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
    # HQ admin bridge (ADR 0068): dark (see ADMIN_BRIDGE_DARK_ENV); /ready on the loopback ops
    # port. Its private TLS route serves ONLY the owner events listener (A7): the route forwards
    # to the events port, never to the ops or HQ listener, and answers 502 until
    # activate-admin-bridge-events starts the listener. A GET of the events path is 405.
    'admin-bridge': _service('cfmeet-admin-bridge', 8190, '/ready', db='cfmeet_admin_bridge',
        role='admin_bridge', migrations=['admin-bridge-migration'], history=['admin_bridge_seaql_migrations'],
        memory=128, env=ADMIN_BRIDGE_DARK_ENV, tls=ADMIN_BRIDGE_EVENTS_TLS, upstream=ADMIN_BRIDGE_EVENTS_PORT,
        probe=(ADMIN_BRIDGE_EVENTS_PATH, 405)),
    # No database, no trust bundle (it receives no internal call); public only after
    # activate-developer-platform + activate-gateway-routing.
    'api-gateway': _service('cfmeet-api-gateway', 8099, '/ready', memory=192,
        env={'API_GATEWAY_BIND_ADDRESS': '0.0.0.0:8099', 'API_GATEWAY_PUBLIC_ROUTING_ENABLED': 'false',
             'API_GATEWAY_WEBSOCKETS_ENABLED': 'false', 'API_GATEWAY_CORS_ALLOWED_ORIGINS': ','.join(WEB_ORIGINS)}),
}

# Owners before callers (final-production-activation-runbook §1).
ORDER = ('platform-access', 'organizations', 'notifications', 'notifications-2',
         'messaging', 'calling', 'communications', 'numbering', 'privacy-relay', 'sms-gateway', 'telephony',
         'meeting-service', 'developer-platform', 'developer-platform-worker', 'admin-bridge', 'api-gateway')
DEPENDS = {
    'organizations': ('platform-access',),
    'notifications': ('platform-access',),
    'notifications-2': ('notifications',),
    'messaging': ('platform-access',), 'calling': ('platform-access',),
    'communications': ('platform-access',), 'numbering': ('platform-access',),
    'privacy-relay': ('platform-access',), 'telephony': ('platform-access',),
    'sms-gateway': ('platform-access', 'numbering'),
    'meeting-service': ('platform-access', 'organizations', 'messaging', 'calling', 'communications'),
    'developer-platform': ('platform-access', 'meeting-service', 'calling'),
    'developer-platform-worker': ('developer-platform',),
    'api-gateway': ('platform-access', 'developer-platform'),
    # Reads its owners lazily (OWNER_UNAVAILABLE until reachable); released after them anyway.
    'admin-bridge': ('organizations',),
}
IMAGES = ('platform-access', 'organizations', 'notifications', 'messaging', 'calling', 'communications', 'numbering',
          'privacy-relay', 'sms-gateway', 'telephony', 'meeting-service', 'developer-platform',
          'admin-bridge', 'api-gateway', 'security-scan-gateway')

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
OWNED_RE = re.compile(r'^cfmeet-(%s|gateway-ingress|admin-bridge-hq-relay)(-(rollback|failed|rolled-back)-[A-Za-z0-9-]+)?$' % _SERVICE_NAMES)
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
    'API_GATEWAY_DISABLED_DOMAINS', 'API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE', 'DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS',
    'ORGANIZATIONS_AUTHORITY_MODE', 'ORGANIZATION_CALLING_SCOPE', 'CFMEET_USAGE_EXPORT_ENABLED',
    'EMAIL_PROVIDER', 'EMAIL_FROM_ADDRESS', 'EMAIL_VERIFICATION_REQUIRED', 'EMAIL_VERIFICATION_ENFORCE_FROM',
    'EMAIL_VERIFICATION_GRACE_UNTIL', 'FEATURE_MEETING_V2_ENABLED', 'FEATURE_RTC_PROVIDER_CONTROL_V2',
    'CONFERENCE_EMPTY_TIMEOUT_SECS', 'FEATURE_RECORDING_V2_ENABLED',
) + platform_ops.FLAGS + integrate_ops.FLAGS + tuple(sorted(p.SAFE_ENV))
SECRET_HINT = re.compile(r'(SECRET|PASSWORD|TOKEN|KEY|PEPPER|DATABASE_URL|REDIS_URL|CREDENTIAL|DSN)', re.I)


def safe_flags(env):
    """Known feature flags with their values; anything that looks like a secret or a
    credential-bearing URL is reduced to presence. Other variables: names only."""
    flags = {}
    for name in FLAGS:
        if name not in env:
            continue
        value = env[name]
        if name.endswith('_ENABLED') and value.strip().lower() in ('true', 'false', '0', '1', ''):
            flags[name] = value  # a boolean switch is never a secret (e.g. *_USER_TOKENS_ENABLED)
        elif SECRET_HINT.search(name) or '://' in value or '@' in value or len(value) > 120:
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


ADMIN_BRIDGE_GRANTS = (
    ('activate-admin-bridge', 'organizations', 'admin-bridge', ('organization.administer', 'organization.operator-read')),
)

# The other owners the bridge reads (A2) and commands (A3), one activation per owner family:
# the owner grants (each permission the owner's bridge routes check; the bridge-only ones
# are refused to every other issuer by the registry), the owner's private TLS base URL on
# the bridge and the module switches those owners serve. The bridge stays as enabled or
# disabled as it was; commands also need ADMIN_BRIDGE_WRITES_ENABLED (not set here).
ADMIN_BRIDGE_OWNERS = {
    'activate-admin-bridge-notifications': {
        'grants': [(replica, ('notifications.administer', 'notifications-observability.read'))
                   for replica in ('notifications', 'notifications-2')],
        'urls': {'CF_NOTIFICATIONS_BASE_URL': 'notifications'},
        'modules': ('NOTIFICATIONS',),
    },
    # Partner reads span both owners (keys, service accounts, websites and users live in
    # Platform Access); verification (A3) is the Developer Platform's.
    'activate-admin-bridge-partners': {
        'grants': [('developer-platform', ('developer-platform.administer',)),
                   ('platform-access', ('platform-identity.administer',))],
        'urls': {'CF_DEVELOPER_PLATFORM_BASE_URL': 'developer-platform',
                 'CF_PLATFORM_ACCESS_BASE_URL': 'platform-access'},
        'modules': ('PARTNERS', 'IDENTITY', 'APPS', 'VERIFICATION'),
    },
    'activate-admin-bridge-communications': {
        'grants': [('communications', ('communications.administer',))],
        'urls': {'CF_COMMUNICATIONS_BASE_URL': 'communications'},
        'modules': ('COMMUNICATIONS',),
    },
    # The Meeting Service mounts its operator routes only with ADMIN_OPERATOR_API_ENABLED.
    'activate-admin-bridge-meetings': {
        'grants': [('meeting-service', ('account.administer', 'meeting.administer'))],
        'urls': {'CF_MEETING_SERVICE_BASE_URL': 'meeting-service'},
        'modules': ('ACCOUNTS', 'MEETINGS'),
        'owner_env': {'meeting-service': {'ADMIN_OPERATOR_API_ENABLED': 'true'}},
    },
    'activate-admin-bridge-messaging': {
        'grants': [('messaging', ('messaging-firstparty.administer',))],
        'urls': {'CF_MESSAGING_BASE_URL': 'messaging'},
        'modules': ('MESSAGING',),
    },
    'activate-admin-bridge-calling': {
        'grants': [('calling', ('calling.administer',))],
        'urls': {'CF_CALLING_BASE_URL': 'calling'},
        'modules': ('CALLING',),
    },
}
ADMIN_BRIDGE_GRANTS += tuple((op, receiver, 'admin-bridge', permissions)
                             for op, spec in ADMIN_BRIDGE_OWNERS.items()
                             for receiver, permissions in spec['grants'])

# A7 owner events: the owners that publish to the bridge's events listener (each one's
# switch is "on when CF_ADMIN_BRIDGE_BASE_URL is set"; no companion flag), the containers
# that need the URL (the Developer Platform API records, its worker relays) and the outbox
# each image's migrations create (proof the released image carries the publisher). The
# BRIDGE receives hq-bridge-events.publish in its own trust bundle; the registry also allows
# Meeting, Organizations, Platform Access and Notifications, which publish nothing yet.
ADMIN_BRIDGE_EVENTS_OPERATION = 'activate-admin-bridge-events'
ADMIN_BRIDGE_EVENT_PUBLISHERS = {
    'developer-platform': {'containers': ('developer-platform', 'developer-platform-worker'),
                           'outbox': 'devplat_bridge_events'},
    'messaging': {'containers': ('messaging',), 'outbox': 'messaging_admin_bridge_events'},
    'calling': {'containers': ('calling',), 'outbox': 'calling_admin_bridge_events'},
    'communications': {'containers': ('communications',), 'outbox': 'communication_admin_bridge_events'},
}
ADMIN_BRIDGE_GRANTS += tuple((ADMIN_BRIDGE_EVENTS_OPERATION, 'admin-bridge', publisher, ('hq-bridge-events.publish',))
                             for publisher in ADMIN_BRIDGE_EVENT_PUBLISHERS)

# A6 plan enforcement: the Meeting Service and Messaging read a CF Meet plan holder's
# effective limits from the Developer Platform (GET /internal/v1/plan-limits/..., receiver
# cfmeet.developer-platform, developer-entitlement.read) and enforce them where the usage
# happens. Both fail open. The switches are refused as on until this operation is recorded;
# MEETING_PLAN_ENFORCEMENT_ENABLED is the Meeting Service's earlier name for its switch
# (refused as true too, never set).
PLAN_ENFORCEMENT_OPERATION = 'activate-plan-enforcement'
PLAN_ENFORCEMENT_CONFIRM = 'plan-limits-reviewed'
PLAN_ENFORCEMENT_CACHE_SECONDS = '60'
PLAN_ENFORCEMENT = {
    'meeting-service': {'MEETING_PLAN_ENFORCEMENT': 'on', 'MEETING_PLAN_CACHE_SECONDS': PLAN_ENFORCEMENT_CACHE_SECONDS},
    'messaging': {'MESSAGING_PLAN_ENFORCEMENT': 'on', 'MESSAGING_PLAN_CACHE_SECONDS': PLAN_ENFORCEMENT_CACHE_SECONDS},
}
PLAN_ENFORCEMENT_SWITCHES = {
    'meeting-service': {'MEETING_PLAN_ENFORCEMENT': ('off', 'on'), 'MEETING_PLAN_ENFORCEMENT_ENABLED': ('false', 'true')},
    'messaging': {'MESSAGING_PLAN_ENFORCEMENT': ('off', 'on')},
}
PLAN_ENFORCEMENT_GRANTS = tuple((PLAN_ENFORCEMENT_OPERATION, 'developer-platform', reader, ('developer-entitlement.read',))
                                for reader in PLAN_ENFORCEMENT)

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
    # P6: Calling counts the API requests of its direct routes (the gateway counts the
    # forwarded ones) once usage export is activated (CALLING_DIRECT_USAGE_ACCOUNTING=on).
    ('activate-usage-export', 'developer-platform', 'calling', ('developer-usage.record',)),
    # Customer API families through the gateway (receivers authorize the gateway's forwards).
    ('activate-gateway-families', 'calling', 'api-gateway', ('calling-request.forward',)),
    ('activate-gateway-families', 'messaging', 'api-gateway', ('messaging-request.forward',)),
    # Notification Service N0-1 (S-1): the Meeting Service checks notification opt-outs at the
    # Hub before sending a meeting notice. The Hub release (communications_release.HUB_INCOMING)
    # carries the same grant; this operation applies it without a Hub release.
    ('activate-meeting-notice-preferences', 'communications', 'meeting-service', ('communications-preferences.read',)),
    # S2: account deletion and the data download reach the person's call history.
    ('activate-account-privacy', 'calling', 'meeting-service', ('calling-account.privacy',)),
    # HQ admin bridge (ADR 0068), phase A2.0: the Organizations operator reads. Every owner
    # receives the bridge's grants through its own trust bundle; later phases add one row per
    # owner and permission here (ADMIN_BRIDGE_GRANTS), never a wildcard.
) + ADMIN_BRIDGE_GRANTS + PLAN_ENFORCEMENT_GRANTS + notif_ops.GRANTS + platform_ops.GRANTS + integrate_ops.GRANTS


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


# services/admin-bridge/deploy/db-roles.sql (step=grants) in this tool's role names (runtime
# = role, migrator = role_migrator): the generic SCHEMA_SQL grants are narrowed right after
# every bridge migration so the runtime role can never change history (audit, revocations,
# policy revisions) or the schema. Fails the release (and rolls the step back) otherwise.
ADMIN_BRIDGE_GRANTS_SQL = """BEGIN;
REVOKE ALL ON SCHEMA public FROM {runtime};
GRANT USAGE ON SCHEMA public TO {runtime};
REVOKE TEMPORARY ON DATABASE {db} FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {runtime};
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public REVOKE ALL ON TABLES FROM {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public REVOKE ALL ON SEQUENCES FROM {runtime};
ALTER DEFAULT PRIVILEGES FOR ROLE {migrator} IN SCHEMA public REVOKE EXECUTE ON FUNCTIONS FROM {runtime};
GRANT SELECT, INSERT ON admin_remote_principals, admin_security_claims, admin_revocations, admin_rate_limits,
    admin_policy_revisions, admin_audit_entries, admin_dataset_days, admin_projections TO {runtime};
GRANT UPDATE (revoked_at) ON admin_remote_principals TO {runtime};
GRANT UPDATE ON admin_rate_limits, admin_dataset_days, admin_projections TO {runtime};
GRANT DELETE ON admin_security_claims, admin_rate_limits TO {runtime};
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO {runtime};
GRANT SELECT ON admin_bridge_seaql_migrations TO {runtime};
-- A3.0 operations ledger (once its migration has run): insert and the send-once
-- transitions its trigger allows; never DELETE or TRUNCATE.
DO $$
BEGIN
    IF to_regclass('public.admin_operations') IS NOT NULL THEN
        EXECUTE 'GRANT SELECT, INSERT, UPDATE ON admin_operations TO {runtime}';
    END IF;
END $$;
DO $$
DECLARE bad TEXT;
BEGIN
    SELECT string_agg(table_name || ':' || privilege_type, ', ') INTO bad
      FROM information_schema.role_table_grants
     WHERE grantee = '{runtime}'
       AND (privilege_type IN ('TRUNCATE', 'REFERENCES', 'TRIGGER')
            OR (privilege_type IN ('UPDATE', 'DELETE')
                AND table_name IN ('admin_audit_entries', 'admin_revocations', 'admin_policy_revisions'))
            OR (privilege_type = 'DELETE' AND table_name = 'admin_operations'));
    IF bad IS NOT NULL THEN
        RAISE EXCEPTION 'admin bridge runtime role has forbidden privileges: %', bad;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner WHERE r.rolname = '{runtime}') THEN
        RAISE EXCEPTION 'admin bridge runtime role must own nothing';
    END IF;
END $$;
COMMIT;
"""


def admin_bridge_grants_sql():
    s = SERVICES['admin-bridge']
    assert re.fullmatch(r'[a-z_]+', s['role']) and re.fullmatch(r'cfmeet_[a-z_]+', s['db'])
    return ADMIN_BRIDGE_GRANTS_SQL.format(runtime=s['role'], migrator=s['role'] + '_migrator', db=s['db'])


def migrate(name, image_id):
    s = SERVICES[name]
    if not s['migrations']:
        return
    url, network = migrator_target(name)
    path = release.env_file(WORK / 'run' / (name + '.migration.env'), {'DATABASE_URL': url})
    try:
        for binary in s['migrations']:
            emit('migrate', {'service': name, 'binary': binary})
            # run_binary: a failed migration reports a scrubbed, value-masked stderr tail (the
            # reason), never the raw output (production 2026-11-18: only "subprocess failed").
            run_binary(['docker', 'run', '--rm', '--network', network, '--memory', '256m', '--env-file', str(path),
                        '--entrypoint', '/app/' + binary, image_id, 'up'], step=binary)
    finally:
        path.unlink(missing_ok=True)
    if name == 'admin-bridge':
        release.db_sql(s['db'], admin_bridge_grants_sql())
        emit('grants', {'service': name, 'runtime': 'least-privilege (db-roles.sql)'})


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
    if SERVICES['communications']['container'] in names:
        # Aggregate compatibility evidence only: never expose account/device IDs.
        # Combined legacy Calling/Messaging links cannot safely opt into retained
        # history until their split/revocation compatibility protocol is reviewed.
        try:
            coverage = release.db_sql(SERVICES['communications']['db'], """
SELECT json_build_object(
 'activeCombinedSessionLinks', count(*) FILTER (
   WHERE state='ACTIVE' AND account_session_ref IS NOT NULL
     AND chat_device_ref IS NOT NULL AND call_endpoint_ref IS NOT NULL),
 'activeChatOnlySessionLinks', count(*) FILTER (
   WHERE state='ACTIVE' AND account_session_ref IS NOT NULL
     AND chat_device_ref IS NOT NULL AND call_endpoint_ref IS NULL)
) FROM communication_devices;
""")
            emit('messagingLifecycleCoverage', json.loads(coverage))
        except (RuntimeError, ValueError):
            emit('messagingLifecycleCoverage', {'status': 'not_available'})
    emit('journal', [{k: e[k] for k in ('operation', 'sha', 'runId', 'status')} for e in load_journal()[-10:]])
    emit('plannedNewContainers', planned_new(names))
    emit('resourceGatePreview', dict(memAvailableMiB=mem_available_mib(), requiredMiB=required_mib(planned_new(names)),
                                     safetyMarginMiB=SAFETY_MARGIN_MIB))


def planned_new(names=None):
    names = container_names() if names is None else names
    return [n for n in ORDER if SERVICES[n]['memory'] and SERVICES[n]['container'] not in names]


# --- backup ----------------------------------------------------------------------------------

def safe_stderr(raw, lines=3, limit=400):
    """The last lines of a tool's stderr, safe to print: URLs (credentials) redacted, quoted and
    key-value literals masked (they can carry row data), bounded."""
    text = scrub(raw.decode(errors='replace') if isinstance(raw, bytes) else raw)
    text = re.sub(r'\(([^()]*)\)=\(([^()]*)\)', r'(\1)=(...)', text)
    text = re.sub(r'"[^"\n]{1,200}"', '"..."', text)
    text = re.sub(r"'[^'\n]{1,200}'", "'...'", text)
    tail = [l.strip() for l in text.splitlines() if l.strip()][-lines:]
    return ' | '.join(tail)[:limit]


def run_binary(args, stdin_path=None, stdout_path=None, step=None):
    """Like production.run for binary streams. stderr (may hold credentials) is never printed
    raw: a failure reports only a scrubbed, value-masked tail (safe_stderr)."""
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
        raise RuntimeError('subprocess failed: %s%s (exit %d): %s' % (
            ' '.join(args[:2]), ' [%s]' % step if step else '', proc.returncode, safe_stderr(proc.stderr)))
    return proc.stdout


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def toc_entries(dump, major):
    listing = run_binary(['docker', 'run', '--rm', '-i', '--network', 'none', 'postgres:%d-alpine' % major,
                          'pg_restore', '--list'], stdin_path=dump, step='pg_restore --list').decode()
    return [line for line in listing.splitlines() if line.strip() and not line.startswith(';')]


def docker_step(args, step, input=None):
    """production.run with a diagnosable failure: the step and a safe stderr tail (no credentials
    are ever passed to these throwaway, network-less containers)."""
    proc = subprocess.run(args, input=input, capture_output=True, text=True)
    if proc.returncode:
        raise RuntimeError('subprocess failed: %s [%s] (exit %d): %s' % (
            ' '.join(args[:2]), step, proc.returncode, safe_stderr(proc.stderr)))
    return proc.stdout.strip()


def restore_test(dump, major, expected, run_id, label):
    """Restore into a throwaway, network-less PostgreSQL and compare every migration history.

    The image's entrypoint first runs a temporary server on the Unix socket only (initdb and
    init scripts), stops it and starts the real one. Readiness over the socket can therefore
    report the temporary server; the real server is the only one listening on TCP, so readiness
    and every step use 127.0.0.1 (the loopback exists with --network none)."""
    name = 'cfmeet-restore-test-%s-%s' % (run_id, label)
    _own(name)
    tcp = ['-h', '127.0.0.1', '-U', 'postgres']
    docker_step(['docker', 'run', '-d', '--name', name, '--network', 'none', '--memory', '512m',
                 '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', 'postgres:%d-alpine' % major], 'restore container ' + label)
    try:
        for _ in range(120):
            if subprocess.run(['docker', 'exec', name, 'pg_isready', '-q'] + tcp, capture_output=True).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError('Restore test PostgreSQL did not become ready: ' + label)
        docker_step(['docker', 'exec', name, 'createdb'] + tcp + ['restore_test'], 'createdb ' + label)
        run_binary(['docker', 'exec', '-i', name, 'pg_restore'] + tcp + ['-d', 'restore_test',
                    '--no-owner', '--no-privileges'], stdin_path=dump, step='restore test ' + label)
        sql = lambda q: docker_step(['docker', 'exec', '-i', name, 'psql', '-XAtq'] + tcp + ['-d', 'restore_test'],
                                    'restore query ' + label, input=q)
        restored = {t: v['applied'] for t, v in history_state(sql).items()}
        if restored != expected:
            raise RuntimeError('Restore test history mismatch: ' + label)
        tables = int(sql("SELECT count(*) FROM information_schema.tables WHERE table_schema='public';"))
        return {'status': 'PASS', 'tables': tables, 'histories': restored}
    finally:
        # Never mask the step's own failure with a cleanup failure.
        try:
            remove(name)
        except RuntimeError:
            emit('restoreTestCleanup', {'container': name, 'removed': False})


# Databases whose dump is restored into a throwaway PostgreSQL before every release
# (labels as in the backup manifest: cfmeet_developer_platform -> developer-platform).
RESTORE_TEST_LABELS = ('meeting-service', 'organizations', 'platform-access', 'developer-platform')


def backup(sha, run_id, restore_labels=RESTORE_TEST_LABELS):
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
                           stdout_path=dump, step='pg_dump meeting-service')
            finally:
                env.unlink(missing_ok=True)
            major = meeting_major
        else:
            histories = {t: v['applied'] for t, v in history_state(lambda q, d=db: release.db_sql(d, q)).items()}
            run_binary(['docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'pg_dump', '-Fc', '-d', db], stdout_path=dump,
                       step='pg_dump ' + label)
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
    if name == 'notifications':
        # Master key of the data at rest (sealed columns, keyed digests): one file shared
        # read-only by both replicas, never rotated by a release.
        secret_file(p.SECRETS / 'notifications-data.key', lambda: base64.b64encode(secrets.token_bytes(32)).decode(), s['uid'])


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
                      % (s['tls'], s['container'], s['upstream'], TLS_TAG))
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
           'CF_PLATFORM_ACCESS_BASE_URL': private_url(SERVICES['platform-access']['tls']),
           # SEC-012 / P8: signers bind tokens to the request; receivers verify bound tokens
           # and still accept unbound ones (require is a per-receiver decision).
           'SERVICE_AUTH_REQUEST_BINDING': 'emit',
           'SERVICE_AUTH_REQUEST_BINDING_MODE': 'accept'}
    if name != 'api-gateway':
        env['CF_SERVICE_TRUST_BUNDLE_FILE'] = '/run/secrets/trust.json'
    if name == 'admin-bridge':
        # The bridge treats every CF_<OWNER>_BASE_URL as an owner it reads: none while dark;
        # each activation adds exactly the owners its grants cover.
        env.pop('CF_PLATFORM_ACCESS_BASE_URL')
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
    if identity == 'notifications':
        mounts.append((p.SECRETS / 'notifications-data.key', NOTIFICATIONS_DATA_KEY_TARGET))
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


# Settings every release makes sure a running service has (added only when absent: an
# operator's own value is kept). C4: Calling's call counters on its separate metrics
# listener, on the private network only (no published port); monitoring_stack scrapes it
# (METRICS_ENV) once it is set, so cfmeet-prometheus has calling_* counters.
RELEASE_ENV_DEFAULTS = {
    'calling': {'CALLING_METRICS_ADDRESS': '0.0.0.0:9464'},
}


def release_env_defaults(name, env):
    """The RELEASE_ENV_DEFAULTS `name`'s current env lacks (what the release adds)."""
    return {k: v for k, v in RELEASE_ENV_DEFAULTS.get(name, {}).items() if not (env.get(k) or '').strip()}


def deploy(name, image_id, sha, run_id, entry):
    s = SERVICES[name]
    notif_ops.rollback_floor(sys.modules[__name__], name, image_id)
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
        added = release_env_defaults(name, p.env_of(c)) if name in RELEASE_ENV_DEFAULTS else {}
        added.update(RELEASE_PINNED_ENV.get(name, {}))
        cfg = release.clone(c, container, image_id, added, False)
        if name == 'meeting-service':
            audit_operations.persistent_meeting_logs(cfg)
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
                           'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': (None, 'shadow'),
                           # The decision email has one sender: the legacy Meeting email
                           # (developer_access.notify_applicant) until both switch together.
                           'DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE': (None, '', 'legacy')},
    # Dark until the Notification Service activation operations (N5 / N9) are recorded.
    'notifications': {'NOTIFICATIONS_EMAIL_PROVIDER': (None, '', 'none'), 'NOTIFICATIONS_PUSH_PROVIDER': (None, '', 'none')},
    'notifications-2': {'NOTIFICATIONS_EMAIL_PROVIDER': (None, '', 'none'), 'NOTIFICATIONS_PUSH_PROVIDER': (None, '', 'none')},
    # HQ admin bridge: off, no writes, every module off. A module may be on only after its
    # activation is recorded (safe_defaults); the bridge itself is switched on only by
    # activate-admin-bridge-hq (HQ enrolled, TLS and HMAC mounts, signed policy). Writes stay off.
    'admin-bridge': dict({'ADMIN_BRIDGE_ENABLED': (None, 'false'), 'ADMIN_BRIDGE_WRITES_ENABLED': (None, 'false'),
                          # A7: no owner events listener until activate-admin-bridge-events.
                          'ADMIN_BRIDGE_EVENTS_BIND_ADDRESS': (None, '')},
                         **{'ADMIN_BRIDGE_MODULE_' + m: (None, 'false') for m in ADMIN_BRIDGE_MODULES}),
    # ADR 0068 A4: owner-delivered organization exports, off until their activation.
    'organizations': {'ORGANIZATIONS_EXPORT_DELIVERY_ENABLED': (None, '', 'false')},
}


PUSH_ADAPTER_PREFIXES = ('MESSAGING_PUSH_APNS_', 'MESSAGING_PUSH_FCM_', 'MESSAGING_PUSH_WEB_')


def safe_defaults():
    """SAFE_DEFAULTS, widened only by a completed activation that owns the setting: customer
    webhook delivery may be `active` once activate-developer-webhooks is recorded. Production
    auto-approval and every provider-dependent switch stay fixed."""
    rules = copy.deepcopy(SAFE_DEFAULTS)
    # SMS credentials and owner acceptance are required before a future activation.
    rules.setdefault('meeting-service', {})['PHONE_ACCOUNTS_ENABLED'] = (None, '', 'false', 'true') if done(provider_operations.PHONE_OPERATION) else (None, '', 'false')
    rules['developer-platform-worker'] = dict(rules['developer-platform'])
    if done('activate-developer-webhooks'):
        for name in ('developer-platform', 'developer-platform-worker'):
            rules[name]['DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'] = (None, 'shadow', 'active')
    # Personal E2EE devices may be on only after the owner's recorded release decision
    # (activate-personal-encryption), and only with the release record mounted (checked below).
    if done('activate-personal-encryption'):
        rules['messaging']['MESSAGING_E2EE_DEVICES_ENABLED'] = (None, 'false', 'true')
    # The Notification Service's own Resend provider (activate-notifications-email).
    if done('activate-notifications-email'):
        for name in ('notifications', 'notifications-2'):
            rules[name]['NOTIFICATIONS_EMAIL_PROVIDER'] = (None, '', 'none', 'resend')
    # The service's live push provider (activate-notifications-push; Web Push only).
    if done('activate-notifications-push'):
        for name in ('notifications', 'notifications-2'):
            rules[name]['NOTIFICATIONS_PUSH_PROVIDER'] = (None, '', 'none', 'live')
    rules.setdefault('meeting-service', {})[meeting_recording_operations.FLAG] = \
        (None, '', 'false', 'true') if done(meeting_recording_operations.OPERATION) else (None, '', 'false')
    for name, flag in dict(send_policy_operations.SOURCE_FLAGS, messaging=send_policy_operations.FAST).items():
        rules.setdefault(name, {})[flag] = (None, '', 'false', 'true') if done(send_policy_operations.OPERATION) else (None, '', 'false')
    # Audit M4: the Meeting in-app-only path is on only after its gated activation.
    rules.setdefault('meeting-service', {})[notif_ops.MEETING_IN_APP_FLAG] = \
        (None, '', 'false', 'true') if done(notif_ops.MEETING_IN_APP_OPERATION) else (None, '', 'false')
    # HQ admin bridge: the Organizations module after activate-admin-bridge (its grants).
    if done('activate-admin-bridge'):
        rules['admin-bridge']['ADMIN_BRIDGE_MODULE_ORGANIZATIONS'] = (None, 'false', 'true')
    # ...and each other owner family's modules after its own activation.
    for op, spec in ADMIN_BRIDGE_OWNERS.items():
        if done(op):
            for module in spec['modules']:
                rules['admin-bridge']['ADMIN_BRIDGE_MODULE_' + module] = (None, 'false', 'true')
    # ...and switched on for HQ (reads only) after activate-admin-bridge-hq.
    if done('activate-admin-bridge-hq'):
        rules['admin-bridge']['ADMIN_BRIDGE_ENABLED'] = (None, 'false', 'true')
    # ...and HQ commands after activate-admin-bridge-hq-commands (HQ stage S6).
    if done('activate-admin-bridge-hq-commands'):
        rules['admin-bridge']['ADMIN_BRIDGE_WRITES_ENABLED'] = (None, 'false', 'true')
    # A7 owner events: the listener and each publisher's bridge URL (exactly the private
    # route) only after activate-admin-bridge-events.
    events = done(ADMIN_BRIDGE_EVENTS_OPERATION)
    if events:
        rules['admin-bridge']['ADMIN_BRIDGE_EVENTS_BIND_ADDRESS'] = (None, '', ADMIN_BRIDGE_EVENTS_BIND)
    for spec in ADMIN_BRIDGE_EVENT_PUBLISHERS.values():
        for name in spec['containers']:
            rules.setdefault(name, {})['CF_ADMIN_BRIDGE_BASE_URL'] =                 (None, '', private_url(ADMIN_BRIDGE_EVENTS_TLS)) if events else (None, '')
    # A6 plan enforcement: on only after activate-plan-enforcement.
    for name, switches in PLAN_ENFORCEMENT_SWITCHES.items():
        for var, (off, on) in switches.items():
            rules.setdefault(name, {})[var] = (None, '', off, on) if done(PLAN_ENFORCEMENT_OPERATION) else (None, '', off)
    # A4 owner-delivered organization exports.
    if done(ORGANIZATION_EXPORTS_OPERATION):
        rules['organizations']['ORGANIZATIONS_EXPORT_DELIVERY_ENABLED'] = (None, '', 'false', 'true')
    # CF Meet Platform "Build" flags: off until their activation is recorded (platform_operations).
    for name, flags in platform_ops.safe_default_rules(done).items():
        rules.setdefault(name, {}).update(flags)
    # CF Meet Apps flags: off until their activation is recorded (integrate_operations).
    for name, flags in integrate_ops.safe_default_rules(done).items():
        rules.setdefault(name, {}).update(flags)
    return rules


def allowed_push_adapter_settings():
    """Push adapter variables a completed activation owns, with their only accepted value.
    Web Push (activate-web-push) uses a VAPID key generated on the VPS; APNs and FCM need
    provider credentials that do not exist, so their variables stay refused."""
    if done('activate-web-push'):
        return {'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': WEB_PUSH_KEY_TARGET,
                'MESSAGING_PUSH_WEB_VAPID_SUBJECT': WEB_PUSH_SUBJECT}
    return {}


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
    for replica in notif_ops.REPLICAS:
        if not done(provider_operations.NATIVE_OPERATION):
            for key, value in (envs.get(replica) or {}).items():
                if value and key.startswith(('NOTIFICATIONS_PUSH_APNS_', 'NOTIFICATIONS_PUSH_FCM_')):
                    violations.append(replica + ' ' + key)
    # The push adapters switch on from their credential variables whatever
    # MESSAGING_PUSH_PROVIDER says (services/messaging/src/config/mod.rs): none may be present.
    messaging = envs.get('messaging') or {}
    allowed_push = allowed_push_adapter_settings()
    for var in sorted(v for v in messaging if v.startswith(PUSH_ADAPTER_PREFIXES) and messaging[v]):
        if allowed_push.get(var) != messaging[var]:
            violations.append('messaging ' + var)
    if messaging.get('MESSAGING_E2EE_DEVICES_ENABLED') == 'true' \
            and messaging.get('MESSAGING_E2EE_PRODUCTION_RELEASE_FILE') != E2EE_RELEASE_TARGET:
        violations.append('messaging MESSAGING_E2EE_PRODUCTION_RELEASE_FILE')
    # A4: the Developer Platform delivers owner invitations whenever it holds the Notification
    # Service's sealing key (with CF_NOTIFICATIONS_BASE_URL): only after the activation that
    # gives it the key (owner invitations, or partner push, which seals push secrets to it).
    developer = envs.get('developer-platform') or {}
    if (developer.get('CF_NOTIFICATIONS_SEALING_KEY') or '').strip()             and not (done(OWNER_INVITATIONS_OPERATION) or done(platform_ops.PUSH)):
        violations.append('developer-platform CF_NOTIFICATIONS_SEALING_KEY')
    meeting = envs.get('meeting-service')
    if meeting and meeting.get('NOTIFICATIONS_EMAIL_ENABLED') == 'true' and not mail_available(meeting):
        violations.append('meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials')
    return violations


def running_envs():
    names = container_names()
    return {n: p.env_of(p.inspect(SERVICES[n]['container'])) for n in ORDER if SERVICES[n]['container'] in names}


def release_all(sha, run_id, before_source_swap=None):
    p.identity()
    # Unchanged services retain their verified previous image and OCI revision.
    # The manifest intentionally contains only the newly required images.
    scope, kept = release_scope(sha)
    manifest = registry(sha)
    required = {image_of(name) for name in ORDER if name not in kept}
    required.update(scope - {'security-scan-gateway'})
    missing = required - set(manifest)
    if missing:
        raise RuntimeError('Release plan lacks required images: ' + ', '.join(sorted(missing)))
    images = {service: verify_image(service, sha, manifest) for service in required}
    emit('releaseScope', {'replaced': sorted(n for n in ORDER if n not in kept), 'kept': sorted(kept)})
    violations = safe_default_violations(running_envs())
    if violations:
        raise RuntimeError('Provider-dependent features are not at their safe defaults: ' + ', '.join(violations))
    emit('resourceGate', resource_gate(mem_available_mib(), required_mib(planned_new())))
    # All services already deployed: attest the new main commit without repeat
    # backups, provision, migrations or container replacements.
    if not scope and len(kept) == len(ORDER):
        entry = begin('release', sha, run_id)
        entry['kept'] = kept
        entry['status'] = 'done'
        entry['scanGateway'] = 'kept (build inputs unchanged)'
        commit(entry)
        verify(sha, run_id)
        return
    if any(name not in kept for name in send_policy_operations.OWNERS):
        if before_source_swap:
            before_source_swap()
        send_policy_operations.require_remote_path(sys.modules[__name__])
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
    # D1: the Meeting Service's own Swagger UI / OpenAPI are not served in production
    # (customer documentation is published through the developer portal).
    for path in ('/api-docs/swagger-config.json', '/api-docs/openapi.json'):
        status, _ = http_status(PUBLIC + path)
        checks['docs_not_public ' + path] = 'PASS' if status == 404 else 'FAIL'
    status, _ = http_status(PUBLIC + '/swagger-ui/')
    checks['swagger_reference'] = 'PASS' if status in (200, 404) else 'FAIL'
    status, headers = http_status(PUBLIC + '/api/v2/me/profile', 'OPTIONS', {
        'Origin': WEB_ORIGIN, 'Access-Control-Request-Method': 'GET', 'Access-Control-Request-Headers': 'authorization'})
    allowed = {k.lower(): v for k, v in headers.items()}.get('access-control-allow-origin')
    checks['cors_preflight_from_web'] = 'PASS' if status < 400 and allowed in (WEB_ORIGIN, '*') else 'FAIL'
    for path in ('/internal/v1/organizations', '/internal/v1/admin/event-delivery'):
        status, _ = http_status(PUBLIC + path)
        checks['internal_not_public ' + path] = 'PASS' if status in (401, 403, 404) else 'FAIL'
    return checks


# The Organizations outbox holds two kinds of rows (services/organizations/src/store.rs emit):
# one `cfmeet.reconciliation` RECORD per event (event history; never dispatched, so it stays
# PENDING with 0 attempts by design) and one row per subscribed CONSUMER (delivered by the
# worker). Only consumer rows are delivery; a reconciliation record is never a delivery failure.
RECONCILIATION_DESTINATION = 'cfmeet.reconciliation'
EVENT_STALL_MINUTES = 15
EVENT_DELIVERY_SQL = """
WITH o AS (SELECT seq, event_id, destination, event_type, aggregate_id, state, attempts, created_at
           FROM organization_outbox),
     c AS (SELECT * FROM o WHERE destination <> 'cfmeet.reconciliation')
SELECT json_build_object(
  'rows', (SELECT count(*) FROM o),
  'states', (SELECT coalesce(json_object_agg(state, n), '{}') FROM (SELECT state, count(*) n FROM o GROUP BY state) a),
  'byDestination', (SELECT coalesce(json_object_agg(destination, s), '{}') FROM
      (SELECT destination, json_object_agg(state, n) s FROM
          (SELECT destination, state, count(*) n FROM o GROUP BY destination, state) a GROUP BY destination) b),
  'byDestinationEventState', (SELECT coalesce(json_agg(json_build_object('destination', destination,
          'eventType', event_type, 'state', state, 'count', n) ORDER BY destination, event_type, state), '[]') FROM
      (SELECT destination, event_type, state, count(*) n FROM o GROUP BY destination, event_type, state) a),
  'reconciliationOutOfRecordShape', (SELECT count(*) FROM o WHERE destination = 'cfmeet.reconciliation'
      AND (state <> 'PENDING' OR attempts <> 0)),
  'consumerRowsWithoutRecord', (SELECT count(*) FROM c WHERE NOT EXISTS (SELECT 1 FROM o r
      WHERE r.event_id = c.event_id AND r.destination = 'cfmeet.reconciliation')),
  'consumerUndeliveredStalled', (SELECT coalesce(json_object_agg(destination, n), '{}') FROM
      (SELECT destination, count(*) n FROM c WHERE state IN ('PENDING', 'INFLIGHT')
          AND created_at < now() - make_interval(mins => %(stall)d) GROUP BY destination) a),
  'consumerBlockedByDead', (SELECT count(*) FROM c WHERE c.state IN ('PENDING', 'INFLIGHT') AND EXISTS
      (SELECT 1 FROM c d WHERE d.destination = c.destination AND d.aggregate_id = c.aggregate_id
          AND d.seq < c.seq AND d.state = 'DEAD')),
  'oldestConsumerUndeliveredSeconds', (SELECT coalesce(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::bigint
      FROM c WHERE state IN ('PENDING', 'INFLIGHT')));
""" % {'stall': EVENT_STALL_MINUTES}


def event_delivery():
    """Destination- and event-type-aware outbox report (counts only; no payloads)."""
    rows = release.db_sql('cfmeet_organizations', EVENT_DELIVERY_SQL)
    return json.loads(rows or '{}')


def configured_event_destinations(env):
    return sorted({d.strip() for d in (env or {}).get('ORGANIZATIONS_EVENT_DESTINATIONS', '').split(',') if d.strip()})


def classify_event_delivery(report, destinations):
    """Separates the reconciliation record from consumer delivery and returns
    {result, failures, reconciliationRecords, consumerDelivery, undeliverableUnconfigured}.
    Failures are genuine delivery defects only: a DEAD consumer row, a consumer row of a
    configured destination still undelivered after EVENT_STALL_MINUTES, a row blocked behind
    a DEAD predecessor, a consumer row without its record, or a reconciliation record that
    was touched by the dispatcher. Rows of a destination that is not configured cannot be
    delivered by design; they are reported, not failed."""
    by_destination = report.get('byDestination') or {}
    record = by_destination.get(RECONCILIATION_DESTINATION) or {}
    consumers = {d: s for d, s in by_destination.items() if d != RECONCILIATION_DESTINATION}
    failures = []
    for d, states in sorted(consumers.items()):
        if states.get('DEAD'):
            failures.append('%s: %d DEAD' % (d, states['DEAD']))
    for d, n in sorted((report.get('consumerUndeliveredStalled') or {}).items()):
        if n and d in destinations:
            failures.append('%s: %d undelivered after %d minutes' % (d, n, EVENT_STALL_MINUTES))
    if report.get('consumerBlockedByDead'):
        failures.append('%d consumer rows blocked behind a DEAD event' % report['consumerBlockedByDead'])
    if report.get('consumerRowsWithoutRecord'):
        failures.append('%d consumer rows without a reconciliation record' % report['consumerRowsWithoutRecord'])
    if report.get('reconciliationOutOfRecordShape'):
        failures.append('%d reconciliation records were dispatched or modified' % report['reconciliationOutOfRecordShape'])
    unconfigured = {d: sum(n for st, n in s.items() if st in ('PENDING', 'INFLIGHT'))
                    for d, s in consumers.items() if d not in destinations}
    return dict(result='FAIL' if failures else 'PASS', failures=failures,
                reconciliationRecords=dict(count=sum(record.values()), dispatched=report.get('reconciliationOutOfRecordShape', 0),
                                           note='never dispatched by design (event history)'),
                consumerDelivery=consumers, configuredDestinations=destinations,
                undeliverableUnconfigured={d: n for d, n in unconfigured.items() if n},
                oldestConsumerUndeliveredSeconds=report.get('oldestConsumerUndeliveredSeconds', 0),
                byDestinationEventState=report.get('byDestinationEventState', []))


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
    # Notification Service N7: every fence producer runs a fence-capable image (routingProtocol >= 1).
    for name in notif_ops.FENCE_PRODUCERS:
        try:
            checks['routing_protocol ' + name] = 'PASS' if notif_ops.routing_protocol(sys.modules[__name__], name) else 'FAIL'
        except Exception:
            checks['routing_protocol ' + name] = 'FAIL'
    violations = safe_default_violations(running_envs())
    checks['provider_features_off'] = 'PASS' if not violations else 'FAIL'
    checks.update(pinned_env_checks(running_envs()))
    checks.update(public_smoke())
    if SERVICES['organizations']['container'] in container_names():
        delivery = classify_event_delivery(event_delivery(), configured_event_destinations(
            running_envs().get('organizations')))
        emit('organizationsEventDelivery', delivery)
        checks['organizations_event_delivery'] = delivery['result']
    p.regression()
    emit('verification', {'sha': sha, 'runId': run_id, 'checks': checks, 'violations': violations})
    failed = [k for k, v in checks.items() if v != 'PASS']
    if failed:
        raise RuntimeError('Verification failed: ' + ', '.join(failed))


def pinned_env_checks(envs):
    """M1: every RELEASE_PINNED_ENV setting holds on the running container."""
    return {'pinned %s %s' % (name, key): 'PASS' if (envs.get(name) or {}).get(key) == value else 'FAIL'
            for name, pins in RELEASE_PINNED_ENV.items() for key, value in pins.items()}


# --- rollback --------------------------------------------------------------------------------

def rollback(sha, run_id):
    journal = load_journal()
    candidates = [e for e in journal if e['status'] in ('done', 'failed')
                  and (e['components'] or e.get('routing') or e.get('edge') or e.get('notificationRouting')
                       or e.get('notificationFence') or e.get('preferenceAuthority')
                       or e.get('edgeLimits') or e.get('platformEdge') or e.get('databaseCapacity') or e.get('serviceLogs'))]
    if not candidates:
        raise RuntimeError('Nothing to roll back')
    entry = candidates[-1]
    if entry.get('irreversible'):
        raise RuntimeError('%s cannot be rolled back (organizations-cutover-runbook §7: never back to a stale writer)'
                           % entry['operation'])
    if any(component['service'] in ('communications', 'calling') for component in entry['components']):
        send_policy_operations.require_remote_path(sys.modules[__name__])
    # Routing fence rollback floor (Document 4 §9.6): checked for every producer before anything moves.
    for component in entry['components']:
        previous = component.get('previous')
        if previous and component['status'] not in ('unchanged', 'rolled-back') and previous in container_names():
            notif_ops.rollback_floor(sys.modules[__name__], component['service'], p.inspect(previous)['Image'])
    if entry.get('serviceLogs'):
        audit_operations.restore_service_logs(entry['serviceLogs'])
    if entry.get('routing'):
        undo_gateway_routing(entry['routing'])
    if entry['operation'] == 'activate-gateway-families' and GATEWAY_SNIPPET.exists():
        # Families leave the edge before the gateway loses their upstreams (no 5xx window).
        GATEWAY_SNIPPET.write_text(gateway_snippet(GATEWAY_PREFIXES, docs=done('activate-gateway-docs')))
        p.reload_host_nginx()
    if entry['operation'] in platform_ops.EDGE:
        # Platform prefixes (/v1/users, the calling edge, /v1/meetings) leave the edge before their containers.
        platform_ops.undo_edge(sys.modules[__name__], entry)
    if entry['operation'] in integrate_ops.EDGE:
        # CF Meet Apps prefixes (/v1/oauth, /v1/app-directory) leave the edge before their containers.
        integrate_ops.undo_edge(sys.modules[__name__], entry)
    if entry['operation'] == 'activate-admin-bridge-hq':
        remove_admin_bridge_hq_relay()  # HQ's way in closes before the bridge goes dark
    if entry.get('edge'):
        undo_gateway_docs(entry['edge'])
    if entry.get('edgeLimits'):
        undo_edge_rate_limits(entry['edgeLimits'])
    if entry.get('notificationRouting'):
        notif_ops.undo_routing(sys.modules[__name__], entry['notificationRouting'], run_id)
    if entry.get('notificationFence'):
        notif_ops.undo_fence(sys.modules[__name__], entry['notificationFence'], run_id)
    if entry.get('preferenceAuthority'):
        notif_ops.undo_preference_authority(sys.modules[__name__], entry['preferenceAuthority'], run_id)
    if entry.get('databaseCapacity'):
        audit_operations.restore_capacity(sys.modules[__name__], entry['databaseCapacity'])
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
    if name == 'meeting-service':
        audit_operations.persistent_meeting_logs(cfg)
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
        delivery = classify_event_delivery(event_delivery(), configured_event_destinations(
            running_envs().get('organizations')))
        emit('organizationsEventDelivery', delivery)
        if any(states.get('DEAD') for states in delivery['consumerDelivery'].values()):
            raise RuntimeError('Dead organization events after activation')

    apply_activation('activate-org-events', sha, run_id, [
        ('meeting-service', {'ORGANIZATION_EVENTS_ENABLED': 'true'}),
        ('organizations', {'ORGANIZATIONS_EVENT_DESTINATIONS': 'cfmeet.meeting-service',
                           'CF_MEETING_SERVICE_BASE_URL': private_url(SERVICES['meeting-service']['tls'])})], post=post)


def activate_meeting_notice_preferences(sha, run_id, confirm):
    """N0-1 (S-1): grant communications-preferences.read to the Meeting Service in the Hub's
    trust bundle (merged; no existing issuer, key or grant is dropped). Only the Hub is
    recreated; a failure restores the previous bundle. No flag, provider or route changes."""
    has_signing_identity('meeting-service')
    apply_activation('activate-meeting-notice-preferences', sha, run_id, [])


def activate_admin_bridge(sha, run_id, confirm):
    """ADR 0068, phase A2.0: grant the admin bridge the Organizations operator reads in the
    Organizations trust bundle (merged; nothing dropped), point the bridge at Organizations and
    switch its Organizations module on. The bridge stays disabled (ADMIN_BRIDGE_ENABLED=false)
    until HQ is enrolled (docs/operations/admin-bridge-runbook.md); a failure restores both."""
    has_signing_identity('admin-bridge')
    tls_route_present(SERVICES['organizations']['tls'])
    apply_activation('activate-admin-bridge', sha, run_id, [
        ('admin-bridge', {'CF_ORGANIZATIONS_BASE_URL': private_url(SERVICES['organizations']['tls']),
                          'ADMIN_BRIDGE_MODULE_ORGANIZATIONS': 'true'})])



# --- HQ admin bridge: switched on for HQ (runbook §3) -----------------------------------------
# The operator stages, BEFORE the run and never through chat or git:
#   /opt/cfmeet-core/admin-bridge/enable.json   the non-secret settings (ADMIN_BRIDGE_ENABLE_KEYS)
#   /opt/cfmeet-core/admin-bridge/tls/          server.crt, server.key, hq-client-ca.pem
#   /opt/cfmeet-core/admin-bridge/hmac/         <X-HQ-Key-ID>.key (base64url text, the file HQ reads)
# and enrols HQ (enroll-cert, enroll-hmac, enroll-staff-key) and puts the signed READ-ONLY policy
# (policy-put). Every file is owned by the bridge's uid; no key is group or world readable.
# Commands stay off (ADMIN_BRIDGE_WRITES_ENABLED=false): this switches the reads on.
ADMIN_BRIDGE_ROOT = p.ROOT / 'admin-bridge'
ADMIN_BRIDGE_UID = 10001
ADMIN_BRIDGE_TLS_TARGET = '/run/admin-bridge/tls'
ADMIN_BRIDGE_HMAC_TARGET = '/run/admin-bridge/hmac'
ADMIN_BRIDGE_TLS_FILES = {'server.crt': 0o022, 'server.key': 0o077, 'hq-client-ca.pem': 0o022}
ADMIN_BRIDGE_ENABLE_KEYS = ('policy_revision', 'policy_key_id', 'policy_public_key_hex', 'hq_workload_uri',
                            'enrolled_markets', 'enrolled_tenants', 'listen_address', 'allowed_source')
ADMIN_BRIDGE_HQ_CONFIRM = 'hq-enrolled-read-only'
_LOWER_UUID = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
_KID = r'[A-Za-z0-9][A-Za-z0-9._:-]{0,127}'


def admin_bridge_enable_settings(record):
    """enable.json â†’ (bridge env overrides, relay listen address, relay allowed source). Refuses
    anything but the exact keys and values the bridge and HQ accept."""
    if not isinstance(record, dict) or sorted(record) != sorted(ADMIN_BRIDGE_ENABLE_KEYS):
        raise RuntimeError('enable.json must hold exactly: ' + ', '.join(ADMIN_BRIDGE_ENABLE_KEYS))
    revision = record['policy_revision']
    if type(revision) is not int or revision < 1:
        raise RuntimeError('policy_revision must be an integer >= 1')
    if not isinstance(record['policy_key_id'], str) or not re.fullmatch(_KID, record['policy_key_id']):
        raise RuntimeError('policy_key_id is not a key id')
    if not isinstance(record['policy_public_key_hex'], str) \
            or not re.fullmatch(r'[0-9a-f]{64}', record['policy_public_key_hex']):
        raise RuntimeError('policy_public_key_hex must be 64 lowercase hex (Ed25519 public key)')
    uri = record['hq_workload_uri']
    if not isinstance(uri, str) or not re.fullmatch(r'cf://(staging|production)/product-admin/cfmeet', uri):
        raise RuntimeError('hq_workload_uri must be cf://<staging|production>/product-admin/cfmeet')
    markets, tenants = record['enrolled_markets'], record['enrolled_tenants']
    if not isinstance(markets, list) or not 1 <= len(markets) <= 16 \
            or not all(isinstance(m, str) and re.fullmatch(r'[A-Z][A-Z0-9_]{1,31}', m) for m in markets):
        raise RuntimeError('enrolled_markets must be 1-16 market codes (e.g. GLOBAL)')
    if not isinstance(tenants, list) or not 1 <= len(tenants) <= 16 \
            or not all(isinstance(t, str) and re.fullmatch(_LOWER_UUID, t) for t in tenants):
        raise RuntimeError('enrolled_tenants must be 1-16 Platform Access tenant UUIDs in canonical lowercase')
    if len(set(markets)) != len(markets) or len(set(tenants)) != len(tenants):
        raise RuntimeError('enrolled_markets and enrolled_tenants must not repeat a value')
    listen = _private_ipv4(record['listen_address'], ADMIN_BRIDGE_HQ_LISTEN_ENV, allow_prefix=False)
    source = _private_ipv4(record['allowed_source'], ADMIN_BRIDGE_HQ_SOURCE_ENV, allow_prefix=True)
    overrides = {
        'ADMIN_BRIDGE_ENABLED': 'true',
        'ADMIN_BRIDGE_WRITES_ENABLED': 'false',
        'ADMIN_BRIDGE_POLICY_REVISION': str(revision),
        'ADMIN_BRIDGE_POLICY_KEY_ID': record['policy_key_id'],
        'ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX': record['policy_public_key_hex'],
        'ADMIN_BRIDGE_HQ_WORKLOAD_URI': uri,
        'ADMIN_BRIDGE_ENROLLED_MARKETS': ','.join(markets),
        'ADMIN_BRIDGE_ENROLLED_TENANTS': ','.join(tenants),
        'ADMIN_BRIDGE_TLS_CERT_FILE': ADMIN_BRIDGE_TLS_TARGET + '/server.crt',
        'ADMIN_BRIDGE_TLS_KEY_FILE': ADMIN_BRIDGE_TLS_TARGET + '/server.key',
        'ADMIN_BRIDGE_CLIENT_CA_FILE': ADMIN_BRIDGE_TLS_TARGET + '/hq-client-ca.pem',
        'ADMIN_BRIDGE_HMAC_KEY_DIR': ADMIN_BRIDGE_HMAC_TARGET,
    }
    return overrides, listen, source


def hmac_key_bytes(text):
    """The bridge's decode_hmac_key_file: base64url text (padding optional, surrounding
    whitespace trimmed) of at least 32 bytes; anything else is refused."""
    v = text.strip()
    if not re.fullmatch(r'[A-Za-z0-9_-]+={0,2}', v):
        return None
    try:
        key = base64.urlsafe_b64decode(v.rstrip('=') + '=' * (-len(v.rstrip('=')) % 4))
    except ValueError:
        return None
    return key if len(key) >= 32 else None


def _sealed(path, mask, uid=ADMIN_BRIDGE_UID):
    st = os.lstat(path)
    if stat.S_ISLNK(st.st_mode):
        raise RuntimeError('%s must not be a symbolic link' % path.name)
    if st.st_uid != uid:
        raise RuntimeError('%s must be owned by uid %d (the bridge)' % (path.name, uid))
    if st.st_mode & mask:
        raise RuntimeError('%s is too permissive (mode %o)' % (path.name, st.st_mode & 0o777))
    return st


def admin_bridge_material(root=ADMIN_BRIDGE_ROOT):
    """Checks the staged TLS and HMAC files; returns {kid: sha256 of the decoded key}. Key
    material is read only to compute digests and is never emitted."""
    for d in (root / 'tls', root / 'hmac'):
        if not d.is_dir():
            raise RuntimeError('Missing %s (stage it first: runbook §3)' % d)
        _sealed(d, 0o022)
    for name, mask in ADMIN_BRIDGE_TLS_FILES.items():
        f = root / 'tls' / name
        if not f.exists():
            raise RuntimeError('Missing tls/%s' % name)
        if not stat.S_ISREG(_sealed(f, mask).st_mode):
            raise RuntimeError('tls/%s must be a regular file' % name)
    extra = sorted(x.name for x in (root / 'tls').iterdir() if x.name not in ADMIN_BRIDGE_TLS_FILES)
    if extra:
        raise RuntimeError('Unexpected files in tls/: ' + ', '.join(extra))
    digests = {}
    for f in sorted((root / 'hmac').iterdir()):
        if not f.name.endswith('.key') or not re.fullmatch(_KID, f.name[:-4]):
            raise RuntimeError('hmac/ holds only <X-HQ-Key-ID>.key files (found %s)' % f.name)
        if not stat.S_ISREG(_sealed(f, 0o077).st_mode):
            raise RuntimeError('hmac/%s must be a regular file' % f.name)
        key = hmac_key_bytes(f.read_text(encoding='ascii', errors='replace'))
        if key is None:
            raise RuntimeError('hmac/%s is not base64url text of at least 32 bytes' % f.name)
        digests[f.name[:-4]] = hashlib.sha256(key).hexdigest()
    if not digests:
        raise RuntimeError('hmac/ holds no key')
    return digests


ADMIN_BRIDGE_ENROLMENT_SQL = (
    "SELECT json_build_object("
    "'certs', (SELECT count(*) FROM admin_remote_principals WHERE kind = 'CLIENT_CERT' AND %(active)s), "
    "'staffKeys', (SELECT count(*) FROM admin_remote_principals WHERE kind = 'STAFF_KEY' AND %(active)s), "
    "'hmac', (SELECT coalesce(json_object_agg(key_id, material), '{}') FROM admin_remote_principals "
    "WHERE kind = 'HMAC_KEY' AND %(active)s), "
    "'policies', (SELECT coalesce(json_object_agg(revision, key_id), '{}') FROM admin_policy_revisions));"
) % {'active': "revoked_at IS NULL AND not_before <= now() AND expires_at > now() + interval '1 day'"}


def admin_bridge_enrolment_problems(state, digests, revision, policy_key_id):
    """What is missing before HQ can be admitted (empty: ready)."""
    problems = []
    if not state.get('certs'):
        problems.append('no active HQ client certificate (enroll-cert)')
    if not state.get('staffKeys'):
        problems.append('no active HQ staff-token key (enroll-staff-key)')
    enrolled = state.get('hmac') or {}
    for kid, digest in sorted(digests.items()):
        if enrolled.get(kid) != digest:
            problems.append('hmac/%s.key is not the active enrolled HMAC key %s (enroll-hmac)' % (kid, kid))
    policies = state.get('policies') or {}
    if str(revision) not in policies:
        problems.append('access policy revision %d is not stored (policy-put)' % revision)
    elif policies[str(revision)] != policy_key_id:
        problems.append('access policy revision %d is signed by another key id' % revision)
    elif any(int(r) > revision for r in policies):
        problems.append('a newer access policy revision than %d is stored' % revision)
    return problems


def admin_bridge_ready_document():
    s = SERVICES['admin-bridge']
    out = p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'], PROBE_IMAGE,
                '-fsS', '--max-time', '4', 'http://127.0.0.1:%d%s' % (s['port'], s['health']))
    return json.loads(out)


def install_admin_bridge_hq_relay(listen, source):
    if ADMIN_BRIDGE_HQ_RELAY in container_names():
        raise RuntimeError('%s already exists: roll back the previous enablement first' % ADMIN_BRIDGE_HQ_RELAY)
    config = WORK / 'admin-bridge-hq-relay.conf'
    config.write_text(admin_bridge_hq_relay_config(source))
    config.chmod(0o644)
    if ADMIN_BRIDGE_HQ_RELAY_NET not in p.run('docker', 'network', 'ls', '--format', '{{.Name}}').splitlines():
        p.run('docker', 'network', 'create', ADMIN_BRIDGE_HQ_RELAY_NET)
    _own(ADMIN_BRIDGE_HQ_RELAY)
    p.run(*admin_bridge_hq_relay_args(p.inspect('cfmeet-core-tls')['Image'], config, listen))
    p.run('docker', 'network', 'connect', p.NET, ADMIN_BRIDGE_HQ_RELAY)
    start(ADMIN_BRIDGE_HQ_RELAY)
    p.run('docker', 'exec', ADMIN_BRIDGE_HQ_RELAY, 'nginx', '-t', '-c', '/etc/nginx/nginx.conf')
    bindings = p.inspect(ADMIN_BRIDGE_HQ_RELAY)['HostConfig']['PortBindings']
    if bindings != {'%d/tcp' % ADMIN_BRIDGE_HQ_PORT: [{'HostIp': listen, 'HostPort': str(ADMIN_BRIDGE_HQ_PORT)}]}:
        raise RuntimeError('HQ relay must be published on the tunnel address only')


def remove_admin_bridge_hq_relay():
    if ADMIN_BRIDGE_HQ_RELAY in container_names():
        remove(ADMIN_BRIDGE_HQ_RELAY)


def activate_admin_bridge_hq(sha, run_id, confirm):
    """Runbook §3 (HQ stage S3): switch the bridge on for HQ, READ-ONLY. Refuses unless HQ is
    enrolled (client certificate, staff-token key, the staged HMAC key's digest), the pinned
    signed policy revision is stored, and every staged file is sealed. Recreates the bridge with
    the settings and read-only mounts, checks /ready, then installs the tunnel-only relay. A
    failure (or `rollback`) removes the relay first, then restores the dark bridge."""
    if confirm != ADMIN_BRIDGE_HQ_CONFIRM:
        raise RuntimeError('activate-admin-bridge-hq needs confirm=%s (owner go-ahead, HQ enrolled, '
                           'read-only policy)' % ADMIN_BRIDGE_HQ_CONFIRM)
    has_signing_identity('admin-bridge')
    record = json.loads((ADMIN_BRIDGE_ROOT / 'enable.json').read_text())
    overrides, listen, source = admin_bridge_enable_settings(record)
    digests = admin_bridge_material()
    state = json.loads(release.db_sql(SERVICES['admin-bridge']['db'], ADMIN_BRIDGE_ENROLMENT_SQL))
    problems = admin_bridge_enrolment_problems(state, digests, record['policy_revision'], record['policy_key_id'])
    if problems:
        raise RuntimeError('HQ is not ready to be admitted: ' + '; '.join(problems))
    emit('adminBridgeHq', {'policyRevision': record['policy_revision'], 'hmacKeys': sorted(digests),
                           'workload': record['hq_workload_uri'], 'markets': record['enrolled_markets'],
                           'tenants': len(record['enrolled_tenants'])})

    def admitted():
        try:
            ready = admin_bridge_ready_document()
            if ready.get('enabled') is not True or ready.get('policy_revision') != record['policy_revision']:
                raise RuntimeError('The bridge did not come up enabled on policy revision %d'
                                   % record['policy_revision'])
            install_admin_bridge_hq_relay(listen, source)
            emit('adminBridgeHqRelay', {'port': ADMIN_BRIDGE_HQ_PORT, 'openapiSha256': ready.get('openapi_sha256')})
        except Exception:
            remove_admin_bridge_hq_relay()
            raise

    apply_activation('activate-admin-bridge-hq', sha, run_id, [
        ('admin-bridge', overrides, {'mounts': [(str(ADMIN_BRIDGE_ROOT / 'tls'), ADMIN_BRIDGE_TLS_TARGET),
                                                (str(ADMIN_BRIDGE_ROOT / 'hmac'), ADMIN_BRIDGE_HMAC_TARGET)]})],
        post=admitted)


# HQ stage S6 (owner gate G1, 2026-11-20: HQ may read and modify): commands on. The policy
# signer first stores a NEW signed revision that adds the command permissions (policy-put,
# offline signature) and the operator updates enable.json's policy_revision to it.
ADMIN_BRIDGE_COMMANDS_CONFIRM = 'hq-commands-two-person'
ADMIN_BRIDGE_LEDGER_SQL = "SELECT to_regclass('public.admin_operations') IS NOT NULL;"


def activate_admin_bridge_hq_commands(sha, run_id, confirm):
    """Switches HQ commands on (ADMIN_BRIDGE_WRITES_ENABLED=true) at a newer signed policy
    revision. Refuses, changing nothing, unless the bridge is enabled for HQ, the operations
    ledger exists, the revision is newer than the running one, stored, signed by the
    configured key and the newest, and HQ is still fully enrolled. Every command keeps its
    own gates: module switch, policy permission, two-person approval for R3/R4."""
    if confirm != ADMIN_BRIDGE_COMMANDS_CONFIRM:
        raise RuntimeError('activate-admin-bridge-hq-commands needs confirm=%s (owner go-ahead, HQ stage S6)'
                           % ADMIN_BRIDGE_COMMANDS_CONFIRM)
    has_signing_identity('admin-bridge')
    running = env_of('admin-bridge')
    if running.get('ADMIN_BRIDGE_ENABLED') != 'true':
        raise RuntimeError('The bridge is not enabled for HQ: run activate-admin-bridge-hq first')
    record = json.loads((ADMIN_BRIDGE_ROOT / 'enable.json').read_text())
    admin_bridge_enable_settings(record)  # the same validation as at enablement
    revision = record['policy_revision']
    current = int(running.get('ADMIN_BRIDGE_POLICY_REVISION') or 0)
    if revision <= current:
        raise RuntimeError('enable.json policy_revision %d is not newer than the running revision %d: '
                           'put the signed revision that adds the command permissions first' % (revision, current))
    if record['policy_key_id'] != running.get('ADMIN_BRIDGE_POLICY_KEY_ID') \
            or record['policy_public_key_hex'] != running.get('ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX'):
        raise RuntimeError('The policy signing key differs from the running one: rotate it separately')
    db = SERVICES['admin-bridge']['db']
    if release.db_sql(db, ADMIN_BRIDGE_LEDGER_SQL).strip() != 't':
        raise RuntimeError('The operations ledger (admin_operations) is missing: release the A3 bridge first')
    digests = admin_bridge_material()
    state = json.loads(release.db_sql(db, ADMIN_BRIDGE_ENROLMENT_SQL))
    problems = admin_bridge_enrolment_problems(state, digests, revision, record['policy_key_id'])
    if problems:
        raise RuntimeError('HQ is not ready for commands: ' + '; '.join(problems))
    emit('adminBridgeHqCommands', {'policyRevision': revision, 'previousRevision': current})

    def on_new_revision():
        ready = admin_bridge_ready_document()
        if ready.get('enabled') is not True or ready.get('policy_revision') != revision:
            raise RuntimeError('The bridge did not come up on policy revision %d' % revision)

    apply_activation('activate-admin-bridge-hq-commands', sha, run_id, [
        ('admin-bridge', {'ADMIN_BRIDGE_WRITES_ENABLED': 'true', 'ADMIN_BRIDGE_POLICY_REVISION': str(revision)})],
        post=on_new_revision)


def activate_admin_bridge_owner(operation):
    """One owner family for the HQ admin bridge (ADR 0068, ADMIN_BRIDGE_OWNERS): grant the
    owner permissions in each owner's trust bundle (merged; nothing dropped), set the owner
    switch it needs, point the bridge at the owners' private TLS routes and switch on their
    modules. Failure restores every container."""
    spec = ADMIN_BRIDGE_OWNERS[operation]

    def run(sha, run_id, confirm):
        has_signing_identity('admin-bridge')
        for owner in spec['urls'].values():
            tls_route_present(SERVICES[owner]['tls'])
        bridge = {var: private_url(SERVICES[owner]['tls']) for var, owner in spec['urls'].items()}
        bridge.update({'ADMIN_BRIDGE_MODULE_' + m: 'true' for m in spec['modules']})
        changes = [(owner, env) for owner, env in spec.get('owner_env', {}).items()]
        apply_activation(operation, sha, run_id, changes + [('admin-bridge', bridge)])
    return run


def table_present(service, table):
    """True when `table` exists in `service`'s database (the released image's migrations ran)."""
    assert re.fullmatch(r'[a-z_]+', table)
    sql = notif_ops.producer_sql(sys.modules[__name__], service)
    return (sql("SELECT to_regclass('public.%s') IS NOT NULL;" % table) or '').strip() in ('t', 'true')


def activate_admin_bridge_events(sha, run_id, confirm):
    """ADR 0068 A7: owner events to the bridge. Grants hq-bridge-events.publish to each
    publisher in the BRIDGE's trust bundle (merged; nothing dropped), starts the bridge's events
    listener behind its private TLS route and gives each publisher CF_ADMIN_BRIDGE_BASE_URL
    (its outbox starts recording and relaying). One operation: one listener, one bundle and one
    owner decision; any failure restores every container (the bridge first in, last out)."""
    bridge = env_of('admin-bridge')
    if bridge.get('ADMIN_BRIDGE_ENABLED') != 'true':
        raise RuntimeError('The bridge starts its events listener only while enabled: run activate-admin-bridge-hq first')
    if not bridge.get('CF_SERVICE_TRUST_BUNDLE_FILE'):
        raise RuntimeError('The bridge has no receiver trust bundle (CF_SERVICE_TRUST_BUNDLE_FILE)')
    has_signing_identity('admin-bridge')
    tls_route_present(ADMIN_BRIDGE_EVENTS_TLS)
    for publisher, spec in ADMIN_BRIDGE_EVENT_PUBLISHERS.items():
        if not table_present(publisher, spec['outbox']):
            raise RuntimeError('%s has no %s table: release an image with its A7 owner events first'
                               % (publisher, spec['outbox']))
        for container in spec['containers']:
            has_signing_identity(container)
    url = private_url(ADMIN_BRIDGE_EVENTS_TLS)
    probe = url + ADMIN_BRIDGE_EVENTS_PATH
    changes = [('admin-bridge', {'ADMIN_BRIDGE_EVENTS_BIND_ADDRESS': ADMIN_BRIDGE_EVENTS_BIND})]
    changes += [(container, {'CF_ADMIN_BRIDGE_BASE_URL': url})
                for spec in ADMIN_BRIDGE_EVENT_PUBLISHERS.values() for container in spec['containers']]

    def listening():
        status = notif_ops.private_status(sys.modules[__name__], probe)
        emit('adminBridgeEvents', {'route': ADMIN_BRIDGE_EVENTS_TLS, 'unsignedPost': status,
                                   'publishers': sorted(ADMIN_BRIDGE_EVENT_PUBLISHERS)})
        if status != '401':
            raise RuntimeError('The bridge events listener answered %s to an unsigned post (expected 401)' % status)

    apply_activation(ADMIN_BRIDGE_EVENTS_OPERATION, sha, run_id, changes, post=listening)


def activate_plan_enforcement(sha, run_id, confirm):
    """ADR 0068 A6: the Meeting Service and Messaging enforce CF Meet user plan limits. Grants
    developer-entitlement.read to both in the Developer Platform's trust bundle (merged), sets
    CF_DEVELOPER_PLATFORM_BASE_URL where it is not set yet and switches enforcement on. Both
    fail open (today's limits) when the read fails. Needs the owner's review of the active plan
    limits (confirm=plan-limits-reviewed): from here on they refuse real joins, recordings and
    uploads. Any failure restores every container."""
    if confirm != PLAN_ENFORCEMENT_CONFIRM:
        raise RuntimeError('%s needs confirm=%s (the owner reviewed the active plan limits)'
                           % (PLAN_ENFORCEMENT_OPERATION, PLAN_ENFORCEMENT_CONFIRM))
    has_signing_identity('developer-platform')
    tls_route_present(SERVICES['developer-platform']['tls'])
    dp = private_url(SERVICES['developer-platform']['tls'])
    changes = []
    for name, flags in PLAN_ENFORCEMENT.items():
        has_signing_identity(name)
        current = env_of(name).get('CF_DEVELOPER_PLATFORM_BASE_URL')
        if current and current != dp:
            raise RuntimeError('%s points CF_DEVELOPER_PLATFORM_BASE_URL somewhere else than the private route' % name)
        changes.append((name, dict(flags, **({} if current else {'CF_DEVELOPER_PLATFORM_BASE_URL': dp}))))
    apply_activation(PLAN_ENFORCEMENT_OPERATION, sha, run_id, changes)


# --- Owner-delivered links (ADR 0068 A4 / A7) -------------------------------------------------
# A secret a bridge command produces goes straight to its rightful recipient, never to HQ.
# Messaging exports, Platform Access keys and the Developer Console reveal (CONSOLE_REVEAL_ONCE)
# and the Meeting Service account export (its own email, sealed under its existing secret) need
# no setting. Two need one: the Developer Platform's first-party owner invitation and the
# Organizations export link, both sealed to the Notification Service's HPKE key (the same pair
# notification_operations.sealing_key generates once; the replicas get the private key, the
# producer the public one) and relayed through I1 (their publications activation first).
OWNER_INVITATIONS_OPERATION = 'activate-developer-owner-invitations'
ORGANIZATION_EXPORTS_OPERATION = 'activate-organization-export-delivery'
ORGANIZATIONS_EXPORT_KEYS = 'organizations-export.keys'
ORGANIZATIONS_EXPORT_KEYS_TARGET = '/run/secrets/organizations-export.keys'
EXPORT_KEY_LINE = re.compile(r'[A-Za-z0-9._-]{1,64}:[A-Za-z0-9_-]{43}')


def notification_sealing(producer):
    """(public key, changes giving each replica the private key where it lacks it)."""
    cr = sys.modules[__name__]
    kid, private, public = notif_ops.sealing_key(cr)
    changes = []
    for replica in notif_ops.REPLICAS:
        if not env_of(replica).get('NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE'):
            changes.append((replica, {'NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE': notif_ops.HPKE_TARGET,
                                      'NOTIFICATIONS_HPKE_KEY_ID': kid},
                            {'mounts': [(private, notif_ops.HPKE_TARGET)]}))
    emit('notificationsSealingKey', {'kid': kid, 'producer': producer,
                                     'replicasMounted': [c[0] for c in changes]})
    return public, changes


def require_publisher(name, containers, publications):
    notif_tls = SERVICES['notifications']['tls']
    tls_route_present(notif_tls)
    for container in containers:
        if env_of(container).get('CF_NOTIFICATIONS_BASE_URL') != private_url(notif_tls):
            raise RuntimeError('%s does not publish to the Notification Service: run %s first' % (container, publications))
    has_signing_identity(name)


def activate_developer_owner_invitations(sha, run_id, confirm):
    """The first-party workspace owner invitation (partners.workspace.register-first-party):
    the Developer Platform seals the claim link to the Notification Service and its worker
    relays it through I1. Sets CF_NOTIFICATIONS_SEALING_KEY on the API (the worker relays
    sealed rows and needs only CF_NOTIFICATIONS_BASE_URL, set by activate-developer-publications)."""
    require_publisher('developer-platform', ('developer-platform', 'developer-platform-worker'),
                      'activate-developer-publications')
    if not table_present('developer-platform', 'devplat_owner_deliveries'):
        raise RuntimeError('The Developer Platform has no devplat_owner_deliveries table: release its A4 image first')
    public, changes = notification_sealing('developer-platform')
    current = env_of('developer-platform').get('CF_NOTIFICATIONS_SEALING_KEY')
    if current and current != public:
        raise RuntimeError('The Developer Platform holds another sealing key than the Notification Service\'s')
    apply_activation(OWNER_INVITATIONS_OPERATION, sha, run_id,
                     changes + [('developer-platform', {'CF_NOTIFICATIONS_SEALING_KEY': public})])


def organizations_export_keys(today=None):
    """The Organizations export at-rest key file, generated once on the VPS (one
    `<key id>:<base64url 32 bytes>` line; the first line seals, every line opens), owned by the
    service user, 0400. Never regenerated: exports in flight must stay readable."""
    path = p.SECRETS / ORGANIZATIONS_EXPORT_KEYS

    def new():
        kid = 'exp-%s' % (today or datetime.date.today()).strftime('%Y-%m')
        return '%s:%s\n' % (kid, base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip('='))

    text = secret_file(path, new, uid=SERVICES['organizations']['uid'])
    lines = [x.strip() for x in text.splitlines() if x.strip() and not x.startswith('#')]
    if not lines or not all(EXPORT_KEY_LINE.fullmatch(x) for x in lines):
        raise RuntimeError('The Organizations export key file is malformed')
    return path


def activate_organization_export_delivery(sha, run_id, confirm):
    """organizations.organization.export: the export is kept sealed under the generated key
    file and its single-use link reaches the requester only through the Notification Service.
    Sets ORGANIZATIONS_EXPORT_DELIVERY_ENABLED, CF_NOTIFICATIONS_SEALING_KEY and
    ORGANIZATIONS_EXPORT_KEYS_FILE (mounted read-only). Organizations refuses to start while
    its build lacks the notice type organizations.organization.export_ready: the health check
    then fails and every container is restored."""
    require_publisher('organizations', ('organizations',), 'activate-organizations-publications')
    if not table_present('organizations', 'organization_exports'):
        raise RuntimeError('Organizations has no organization_exports table: release its A4 image first')
    keys = organizations_export_keys()
    public, changes = notification_sealing('organizations')
    apply_activation(ORGANIZATION_EXPORTS_OPERATION, sha, run_id, changes + [
        ('organizations', {'ORGANIZATIONS_EXPORT_DELIVERY_ENABLED': 'true', 'CF_NOTIFICATIONS_SEALING_KEY': public,
                           'ORGANIZATIONS_EXPORT_KEYS_FILE': ORGANIZATIONS_EXPORT_KEYS_TARGET},
         {'mounts': [(keys, ORGANIZATIONS_EXPORT_KEYS_TARGET)]})])


PUBLIC_SITE = Path('/etc/nginx/sites-available/api.cfmeet.com')


def public_denial_confirmed(headers_text):
    """True when a public response is the N0-6 nginx denial (404 + X-CFMeet-Ingress: denied),
    not an answer of the Meeting Service."""
    lines = [line.strip() for line in headers_text.replace('\r', '').split('\n') if line.strip()]
    status = lines[0].split()[1] if lines and lines[0].startswith('HTTP/') and len(lines[0].split()) > 1 else ''
    marked = any(line.lower().split(':', 1)[0] == 'x-cfmeet-ingress'
                 and line.split(':', 1)[1].strip().lower() == 'denied' for line in lines if ':' in line)
    return status == '404' and marked


def public_denial_probe(path):
    proc = subprocess.run(['curl', '-sS', '--max-time', '8', '--resolve', 'api.cfmeet.com:443:127.0.0.1',
                           '-o', '/dev/null', '-D', '-', 'https://api.cfmeet.com' + path],
                          text=True, capture_output=True)
    if proc.returncode != 0:
        raise RuntimeError('Public ingress probe failed to complete: ' + path)
    return public_denial_confirmed(proc.stdout)


def activate_public_internal_denial(sha, run_id, confirm):
    """N0-6 (S-3): the public server block refuses the Meeting Service's operator routes
    (/internal/operations/*, /internal/metrics, /internal/rtc/metrics). Host nginx only: no
    container changes. Each denial is proven through the public host; any failed proof
    restores the previous site file. Signed provider callbacks are left reachable."""
    previous = PUBLIC_SITE.read_text()
    entry = begin('activate-public-internal-denial', sha, run_id)
    try:
        release.secure_public_ingress()
        p.reload_host_nginx()  # probes must reach the new worker generation
        results = {path: public_denial_probe(path) for path in release.PUBLIC_DENIAL_PROBES}
        emit('publicInternalDenial', results)
        if not all(results.values()):
            raise RuntimeError('Public ingress still reaches a Meeting Service operator route')
    except Exception:
        if PUBLIC_SITE.read_text() != previous:
            PUBLIC_SITE.write_text(previous)
            p.reload_host_nginx()
        entry['status'] = 'failed'
        commit(entry)
        raise
    entry['status'] = 'done'
    commit(entry)


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


# --- Personal E2EE (owner release decision) ---------------------------------------------------

E2EE_RELEASE_SOURCE = p.ROOT / 'messaging' / 'e2ee-production-release.json'
E2EE_RELEASE_TARGET = '/run/messaging/e2ee-production-release.json'
E2EE_CONFIRM = 'owner-approved-encryption-without-independent-assessment'
E2EE_DECISION_LOG = 'production personal E2EE enabled by owner release decision'


def e2ee_release_record(run_id, witnessed, now=None):
    """The owner's release decision as Messaging validates it. It states the ACTUAL status:
    no independent cryptographic assessment was performed, and (without witnesses) the
    key-transparency log is unwitnessed. It is never a certification claim."""
    now = now or time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    risks = ['NO_INDEPENDENT_CRYPTOGRAPHIC_ASSESSMENT', 'WEB_CODE_DELIVERY_TRUST']
    if not witnessed:
        risks.insert(1, 'KEY_TRANSPARENCY_UNWITNESSED')
    return {'version': 1, 'decisionId': 'OWNER-E2EE-PRODUCTION-RUN-' + run_id, 'decidedByRole': 'PRODUCT_OWNER',
            'decidedAt': now, 'scope': 'PERSONAL_PRIVATE_E2EE', 'independentAssessment': 'NOT_PERFORMED',
            'acceptedResidualRisks': risks}


def activate_personal_e2ee(sha, run_id, confirm):
    """Owner decision (2026-11-15): enable personal PRIVATE_E2EE devices in production WITHOUT an
    independent cryptographic assessment, with the residual risks recorded. Messaging refuses to
    start unless the mounted release record validates; the multi-device Account policy (trusted
    devices only) gates enrollment. Organization MANAGED messaging is unchanged."""
    if confirm != E2EE_CONFIRM:
        raise RuntimeError('activate-personal-encryption needs confirm=' + E2EE_CONFIRM + ' (owner decision)')
    messaging, meeting = env_of('messaging'), env_of('meeting-service')
    if messaging.get('CF_RUNTIME_ENVIRONMENT') != 'production':
        raise RuntimeError('Messaging is not the production runtime')
    missing = [k for k in ('MESSAGING_KT_LOG_ID', 'MESSAGING_KT_SIGNING_KEY_FILE', 'MESSAGING_KT_CHECKPOINT_FILE')
               if not messaging.get(k)]
    if missing:
        raise RuntimeError('Key transparency is not fully configured: ' + ', '.join(missing))
    if meeting.get('ACCOUNT_E2EE_REQUIRES_TRUSTED_DEVICE', '').strip().lower() in ('false', '0', 'off', 'no'):
        raise RuntimeError('The trusted-device gate for E2EE enrollment is switched off on the Meeting Service')
    record = e2ee_release_record(run_id, bool(messaging.get('MESSAGING_KT_WITNESSES_FILE')))
    release.save(E2EE_RELEASE_SOURCE, json.dumps(record, indent=2, sort_keys=True) + '\n', False)
    emit('e2eeRelease', {k: record[k] for k in ('decisionId', 'independentAssessment', 'acceptedResidualRisks')})

    def started():
        out = subprocess.run(['docker', 'logs', '--since', '10m', SERVICES['messaging']['container']],
                             capture_output=True, text=True, timeout=60)
        if E2EE_DECISION_LOG not in out.stdout + out.stderr:
            raise RuntimeError('Messaging did not log the owner release decision at startup')
        emit('e2eeStartup', 'release decision validated by Messaging')

    apply_activation('activate-personal-encryption', sha, run_id, [
        ('messaging', {'MESSAGING_E2EE_DEVICES_ENABLED': 'true',
                       'MESSAGING_E2EE_PRODUCTION_RELEASE_FILE': E2EE_RELEASE_TARGET},
         {'mounts': [(str(E2EE_RELEASE_SOURCE), E2EE_RELEASE_TARGET)]})], post=started)


# Web Push (Section 4 of the backend closure): browser push through the standard Web Push
# protocol (RFC 8030/8291/8292) needs no provider account, only a VAPID key pair, generated
# here on the VPS. APNs and FCM stay off (no provider credentials).
WEB_PUSH_KEY_NAME = 'web-push-vapid.pem'
WEB_PUSH_KEY_TARGET = '/run/messaging/web-push-vapid.pem'
# The VAPID contact (RFC 8292 `sub`): the public site, never a personal address.
WEB_PUSH_SUBJECT = 'https://cfmeet.com'


def vapid_private_key_pem():
    """A fresh PKCS#8 PEM P-256 key (what Messaging's Web Push adapter requires)."""
    pem = p.run('openssl', 'genpkey', '-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256')
    assert pem.startswith('-----BEGIN PRIVATE KEY-----'), 'openssl did not produce a PKCS#8 key'
    return pem + '\n'


def vapid_public_key_present(key):
    """The key file holds a P-256 private key whose public half derives (text output only:
    p.run decodes stdout as text)."""
    public = p.run('openssl', 'pkey', '-in', str(key), '-pubout', '-outform', 'PEM')
    return public.startswith('-----BEGIN PUBLIC KEY-----')


def activate_web_push(sha, run_id, confirm):
    """Web Push for Messaging: a VAPID key generated once on the VPS (owned by Messaging,
    mode 0400, with a recovery copy: rotating it would invalidate every browser
    subscription), mounted read-only, and the two adapter variables. Payloads stay metadata
    only (enforced by Messaging and its database). Delivery reaches a browser only once a
    client subscribes; no external delivery is claimed by this activation."""
    import messaging_runtime
    messaging = env_of('messaging')
    if messaging.get('CF_RUNTIME_ENVIRONMENT') != 'production':
        raise RuntimeError('Messaging is not the production runtime')
    if messaging.get('MESSAGING_E2EE_DEVICES_ENABLED') != 'true':
        raise RuntimeError('Push registrations belong to E2EE devices; enable personal encryption first')
    foreign = sorted(k for k in messaging if k.startswith(('MESSAGING_PUSH_APNS_', 'MESSAGING_PUSH_FCM_')) and messaging[k])
    if foreign:
        raise RuntimeError('Unexpected APNs/FCM settings present: ' + ', '.join(foreign))
    key = messaging_runtime.protected_file(WEB_PUSH_KEY_NAME, vapid_private_key_pem)
    emit('webPushKey', {'generated': True, 'path': WEB_PUSH_KEY_TARGET, 'subject': WEB_PUSH_SUBJECT,
                        'publicKeyPresent': vapid_public_key_present(key)})

    def started():
        out = subprocess.run(['docker', 'logs', '--since', '10m', SERVICES['messaging']['container']],
                             capture_output=True, text=True, timeout=60)
        logs = out.stdout + out.stderr
        lines = [line for line in logs.splitlines() if 'push adapters' in line]
        if not lines or 'WEB_PUSH' not in lines[-1]:
            raise RuntimeError('Messaging did not start its Web Push adapter')
        emit('webPushStartup', 'Messaging reports the WEB_PUSH adapter')

    apply_activation('activate-web-push', sha, run_id, [
        ('messaging', {'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': WEB_PUSH_KEY_TARGET,
                       'MESSAGING_PUSH_WEB_VAPID_SUBJECT': WEB_PUSH_SUBJECT},
         {'mounts': [(str(key), WEB_PUSH_KEY_TARGET)]})], post=started)


def activate_first_party_features(sha, run_id, confirm):
    if env_of('messaging').get('MESSAGING_E2EE_DEVICES_ENABLED') == 'true':
        raise RuntimeError('PRIVATE_E2EE devices are on in production; refusing to change Messaging')
    changes = first_party_feature_changes(env_of('meeting-service'))
    if 'NOTIFICATIONS_EMAIL_ENABLED' not in changes[1][1]:
        emit('notificationEmail', 'not enabled: SMTP credentials are not configured on the Meeting Service')
    apply_activation('activate-first-party-features', sha, run_id, changes)


def activate_messaging_concurrency(sha, run_id, confirm):
    """Owner-approved concurrent Messaging; tenant/instance/rate budgets remain intact."""
    changes = [('messaging', {'MESSAGING_MAX_ACTIVE_DEVICES': '0',
                             'MESSAGING_REALTIME_PER_PRINCIPAL_CONNECTIONS': '0',
                             'MESSAGING_REALTIME_PER_DEVICE_CONNECTIONS': '0'})]
    apply_activation('activate-messaging-concurrency', sha, run_id, changes)


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
    /v1/webhook-deliveries â†’ Developer Platform) through the API Gateway: platform credential
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
        GATEWAY_SNIPPET.write_text(gateway_snippet(GATEWAY_PREFIXES + FAMILY_PREFIXES + platform_ops.edge_prefixes(done)
                                                   + integrate_ops.edge_prefixes(done),
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

    # P6: Calling also counts its direct-route API requests (grant developer-usage.record).
    direct = {'calling': {'CALLING_DIRECT_USAGE_ACCOUNTING': 'on'}}
    apply_activation('activate-usage-export', sha, run_id, [
        (name, dict({'CFMEET_USAGE_EXPORT_ENABLED': 'true'}, **direct.get(name, {})))
        for name in ('calling', 'messaging', 'meeting-service')], post=post)


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
        out.append('location = /swagger-ui { return 308 /docs; }')
        out.append('location = /swagger-ui/ { return 308 /docs; }')
        for path in DOCS_EXACT:
            out.append('location = %s {\n%s}' % (path, read_only))
        out.append('location ^~ %s/ {\n%s}' % (DOCS_PREFIX, read_only))
    return '\n'.join(out) + '\n'


def edge_prefixes():
    """The /v1 families the edge currently routes to the gateway (journal-derived)."""
    return GATEWAY_PREFIXES + (FAMILY_PREFIXES if done('activate-gateway-families') else ()) \
        + platform_ops.edge_prefixes(done) + integrate_ops.edge_prefixes(done)


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
    for text in [site] + snippets:
        if re.search(r'location\s*=\s*/swagger-ui/?\s*\{', text):
            collisions.append('/swagger-ui')
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


# --- Edge throttling (P7) ----------------------------------------------------------------------

# Per-client-IP limits at the api.cfmeet.com edge (host nginx). Sized not to break the web app
# (bursts of page-load requests), Messaging / signaling WebSockets (long-lived connections, one
# request each) or uploads (one request each; body size limits are unchanged): a sustained
# EDGE_RATE requests per second per IP with an immediate EDGE_BURST, and at most EDGE_CONNECTIONS
# concurrent connections per IP (an office NAT with many users stays far below it). Over the
# limit nginx answers 429. Opt-out: `rollback` of this step (or omitting the activation).
EDGE_RATE = '50r/s'
EDGE_BURST = 200
EDGE_CONNECTIONS = 300
EDGE_ZONES = Path('/etc/nginx/conf.d/cfmeet-edge-rate-limits.conf')
EDGE_SNIPPET = Path('/etc/nginx/snippets/cfmeet-edge-limits.conf')
EDGE_INCLUDE = 'include /etc/nginx/snippets/cfmeet-edge-limits.conf;'
# The API Gateway's per-token ceiling of end-user / device traffic (requests per token per
# minute; the gateway's API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE, 0 = off).
TOKEN_CEILING_PER_MINUTE = '600'


def edge_zones():
    """http-context declarations (conf.d): the shared per-IP zones and the 429 status."""
    return ('# CF Meet edge throttling (coordinated release, activate-edge-rate-limits).\n'
            'limit_req_zone $binary_remote_addr zone=cfmeet_edge_req:20m rate=%s;\n'
            'limit_conn_zone $binary_remote_addr zone=cfmeet_edge_conn:20m;\n'
            'limit_req_status 429;\nlimit_conn_status 429;\nlimit_req_log_level warn;\n'
            'limit_conn_log_level warn;\n' % EDGE_RATE)


def edge_snippet():
    """server-context limits, inherited by every location of the public site."""
    return ('# CF Meet edge throttling: per client IP (activate-edge-rate-limits).\n'
            'limit_req zone=cfmeet_edge_req burst=%d nodelay;\n'
            'limit_conn cfmeet_edge_conn %d;\n' % (EDGE_BURST, EDGE_CONNECTIONS))


def activate_edge_rate_limits(sha, run_id, confirm):
    """P7: per-IP request rate and connection limits at the public api.cfmeet.com server block.
    Host nginx only (no container change). Refuses an unexpected site topology; any failed check
    (nginx -t, public smoke) restores the previous files. Rollback removes exactly this step's
    include and files."""
    assert SITE_LINK.resolve() == SITE, 'Unexpected public site topology'
    site = SITE.read_text()
    if site.count(ROUTES_ANCHOR) != 1:
        raise RuntimeError('Unexpected public site topology (calling routes include)')
    if 'limit_req_zone' in site or 'limit_conn_zone' in site:
        raise RuntimeError('The public site already declares rate limit zones')
    previous = {'site': site, 'zones': EDGE_ZONES.read_text() if EDGE_ZONES.exists() else None,
                'snippet': EDGE_SNIPPET.read_text() if EDGE_SNIPPET.exists() else None}
    p.write_private(WORK / 'edge-rate-limits' / run_id / 'site.before', site)
    entry = begin('activate-edge-rate-limits', sha, run_id)
    entry['edgeLimits'] = {'rate': EDGE_RATE, 'burst': EDGE_BURST, 'connections': EDGE_CONNECTIONS}
    commit(entry)
    try:
        EDGE_ZONES.write_text(edge_zones())
        EDGE_SNIPPET.write_text(edge_snippet())
        if EDGE_INCLUDE not in site:
            SITE.write_text(site.replace(ROUTES_ANCHOR, EDGE_INCLUDE + '\n    ' + ROUTES_ANCHOR))
        p.reload_host_nginx()  # nginx -t first
        checks = public_smoke()
        emit('edgeRateLimits', checks)
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Public smoke failed after edge rate limits')
        p.regression()
    except Exception:
        restore_edge_rate_limits(previous)
        entry['status'] = 'failed'
        commit(entry)
        raise
    entry['status'] = 'done'
    commit(entry)


def restore_edge_rate_limits(previous):
    SITE.write_text(previous['site'])
    for path, text in ((EDGE_ZONES, previous['zones']), (EDGE_SNIPPET, previous['snippet'])):
        if text is None:
            path.unlink(missing_ok=True)
        else:
            path.write_text(text)
    p.reload_host_nginx()


def undo_edge_rate_limits(_edge_limits):
    """The opt-out: removes only this step's include, snippet and zones."""
    SITE.write_text(SITE.read_text().replace(EDGE_INCLUDE + '\n    ', '').replace(EDGE_INCLUDE, ''))
    EDGE_SNIPPET.unlink(missing_ok=True)
    EDGE_ZONES.unlink(missing_ok=True)
    p.reload_host_nginx()


def activate_token_ceiling(sha, run_id, confirm):
    """P7: the API Gateway's per-token ceiling of end-user / device traffic (meeting sessions,
    participant tokens, developer sessions, client tokens, signaling credentials, realtime
    tickets): TOKEN_CEILING_PER_MINUTE requests per token per minute, counted in the shared quota
    store (429 RATE_LIMITED, x-cf-quota-level: GATEWAY_TOKEN). Opt-out: rollback (0 = off)."""
    if env_of('api-gateway').get('API_GATEWAY_PUBLIC_ROUTING_ENABLED') != 'true':
        raise RuntimeError('Run activate-developer-platform first (gateway routing is still disabled)')
    apply_activation('activate-token-ceiling', sha, run_id, [
        ('api-gateway', {'API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE': TOKEN_CEILING_PER_MINUTE})])


# --- S1: per-visitor sign-in limits (host nginx X-Real-IP + Meeting Service header) ----------

# The header the host nginx OVERWRITES with the visitor's address on every location it
# proxies, and the Meeting Service reads for its per-address sign-in limits (S1). Anything a
# visitor sends in X-Real-IP is replaced; X-Forwarded-For is never used for limits.
CLIENT_ADDRESS_HEADER = 'X-Real-IP'
CLIENT_ADDRESS_LINE = 'proxy_set_header X-Real-IP $remote_addr;'


def _location_blocks(text):
    """(start of the `{` after `location ...`, index of its matching `}`) of every location
    block, outermost first. Braces inside quotes or comments are not expected in the site."""
    blocks = []
    for match in re.finditer(r'(?m)^[ \t]*location\b[^{;]*\{', text):
        depth, i = 0, match.end() - 1
        while i < len(text):
            if text[i] == '{':
                depth += 1
            elif text[i] == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth:
            raise RuntimeError('Unbalanced public site location block')
        blocks.append((match.end() - 1, i))
    return blocks


def client_address_site(text):
    """The public site with `proxy_set_header X-Real-IP $remote_addr;` in every location
    that proxies (`proxy_pass`) and does not already overwrite X-Real-IP that way. A location
    that sets X-Real-IP to anything else is refused (an operator decision, not ours).
    Returns (new text, number of locations changed)."""
    inserts = []
    for open_brace, close in _location_blocks(text):
        body = text[open_brace + 1:close]
        # Only this block's own directives (not a nested location's) decide.
        own = re.sub(r'(?s)location\b[^{;]*\{.*?\}', '', body)
        if 'proxy_pass' not in own:
            continue
        found = re.findall(r'proxy_set_header\s+x-real-ip\s+([^;]+);', own, flags=re.IGNORECASE)
        if found:
            if any(v.strip() != '$remote_addr' for v in found):
                raise RuntimeError('A public location sets X-Real-IP to something other than $remote_addr')
            continue
        inserts.append(open_brace + 1)
    out = text
    for at in sorted(inserts, reverse=True):
        out = out[:at] + ' ' + CLIENT_ADDRESS_LINE + out[at:]
    return out, len(inserts)


def activate_client_address_limits(sha, run_id, confirm):
    """S1: the Meeting Service's per-visitor sign-in limits (20 code requests / 30 code checks
    per address per 15 minutes) need the visitor's address. Host nginx terminates TLS for
    api.cfmeet.com (no CDN in front: the edge limits already key on $binary_remote_addr), so
    $remote_addr IS the visitor. Every proxying location of the public site gets
    `proxy_set_header X-Real-IP $remote_addr;` (overwriting any visitor-supplied value), then
    the Meeting Service is recreated with AUTH_CLIENT_IP_HEADER=x-real-ip and
    AUTH_TRUSTED_PROXY_HOPS=1 (the single entry the proxy wrote). nginx -t, the public smoke
    and the Meeting Service health must pass, or the site and container are restored.
    `rollback` restores the Meeting Service (limits off); the header line stays (harmless)."""
    assert SITE_LINK.resolve() == SITE, 'Unexpected public site topology'
    previous = SITE.read_text()
    candidate, changed = client_address_site(previous)
    if 'proxy_pass' not in previous:
        raise RuntimeError('The public site proxies nothing itself: refusing (unexpected topology)')
    p.write_private(WORK / 'client-address' / run_id / 'site.before', previous)
    emit('clientAddressLocations', {'changed': changed})

    def restore_site():
        if SITE.read_text() != previous:
            SITE.write_text(previous)
            p.reload_host_nginx()

    try:
        if candidate != previous:
            SITE.write_text(candidate)
            p.reload_host_nginx()  # nginx -t first

        def post():
            checks = public_smoke()
            emit('clientAddressSmoke', checks)
            if any(v != 'PASS' for v in checks.values()):
                raise RuntimeError('Public smoke failed after the client-address header')

        apply_activation('activate-client-address-limits', sha, run_id, [
            ('meeting-service', {'AUTH_CLIENT_IP_HEADER': CLIENT_ADDRESS_HEADER.lower(),
                                 'AUTH_TRUSTED_PROXY_HOPS': '1'})], post=post)
    except Exception:
        restore_site()
        raise


# --- S2: account deletion reaches Calling (call history) --------------------------------------

def activate_account_privacy(sha, run_id, confirm):
    """S2: the Meeting Service's account deletion and data download reach Calling: grants
    `calling-account.privacy` (Calling <- Meeting Service) so the durable erasure job can hide
    and minimize the person's call history and the download can include it. Messaging
    (`messaging-application.delegate`, PRINCIPAL_ERASURE deletion requests) and the Hub
    (`communications-account.apply`) already hold their grants. Until this runs, the Meeting
    Service keeps the Calling erasure jobs pending and retries them (nothing is lost)."""
    has_signing_identity('meeting-service')
    if not env_of('meeting-service').get('CF_CALLING_BASE_URL'):
        raise RuntimeError('The Meeting Service has no CF_CALLING_BASE_URL (run activate-enterprise-calling first)')
    apply_activation('activate-account-privacy', sha, run_id, [])


# --- B6 / C5: database pool sizes within the server's connection budget ------------------------

# Pool size variable and code default of every service on cfmeet-core-db (the default is what
# a container without the variable uses). Unlisted services count POOL_UNKNOWN.
POOL_SETTINGS = {
    'platform-access': ('PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS', 10),
    'organizations': ('ORGANIZATIONS_DATABASE_MAX_CONNECTIONS', 10),
    'notifications': ('NOTIFICATIONS_DATABASE_MAX_CONNECTIONS', 10),
    'notifications-2': ('NOTIFICATIONS_DATABASE_MAX_CONNECTIONS', 10),
    'messaging': ('MESSAGING_DATABASE_MAX_CONNECTIONS', 10),
    'calling': ('CALLING_DATABASE_MAX_CONNECTIONS', 10),
    'communications': (None, 12),
    'numbering': ('NUMBERING_DATABASE_MAX_CONNECTIONS', 10),
    'privacy-relay': ('PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS', 10),
    'sms-gateway': ('SMS_GATEWAY_DATABASE_MAX_CONNECTIONS', 10),
    'telephony': ('TELEPHONY_DATABASE_MAX_CONNECTIONS', 10),
    'meeting-service': ('DATABASE_MAX_CONNECTIONS', 10),
    'developer-platform': ('DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS', 10),
    'developer-platform-worker': ('DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS', 10),
}
POOL_UNKNOWN = 10
# The pools activate-database-pools sets (the only way they rise above the code defaults of 10
# and the Notification Service template's 8). The Notification Service's code is not changed:
# only its release configuration (two replicas, 16 each).
DATABASE_POOLS = {'messaging': '25', 'calling': '20', 'meeting-service': '20',
                  'notifications': '16', 'notifications-2': '16'}
# Connections kept free for migrators, backups, monitoring and an operator's psql.
POOL_HEADROOM = 15


def pool_plan(envs, overrides):
    """{service: pool size} once `overrides` ({service: {var: value}}) apply to `envs`."""
    plan = {}
    for name in SERVICES:
        # The Meeting Service's database is not listed in SERVICES (it predates the
        # catalogue); counted conservatively, as if it were on cfmeet-core-db too.
        if name not in envs or not (SERVICES[name]['db'] or name == 'meeting-service'):
            continue
        var, default = POOL_SETTINGS.get(name, (None, POOL_UNKNOWN))
        env = dict(envs[name], **overrides.get(name, {}))
        try:
            plan[name] = int(env.get(var) or default) if var else default
        except ValueError:
            plan[name] = default
    return plan


def pool_budget_errors(plan, max_connections, reserved):
    total = sum(plan.values())
    budget = max_connections - reserved - POOL_HEADROOM
    if total > budget:
        return ['pools need %d connections, the server allows %d (max_connections %d - reserved %d - headroom %d): '
                'raise max_connections on cfmeet-core-db first' % (total, budget, max_connections, reserved, POOL_HEADROOM)]
    return []


def activate_database_pools(sha, run_id, confirm):
    """B6 / C5: Messaging, Notifications and Calling logged slow pool acquisition. The only way
    the bigger pools take effect (code defaults stay 10, the Notification template 8): sets
    DATABASE_POOLS on Messaging, Calling, the Meeting Service and both Notification replicas,
    and refuses unless every pool together (the planned values plus every other service's
    current one) fits the core database's max_connections with reserve and headroom.
    `rollback` restores the previous containers (previous pools)."""
    overrides = {name: {POOL_SETTINGS[name][0]: size} for name, size in DATABASE_POOLS.items()}
    envs = running_envs()
    max_connections = int(release.db_sql('postgres', 'SHOW max_connections;').strip())
    reserved = int(release.db_sql('postgres', 'SHOW superuser_reserved_connections;').strip())
    plan = pool_plan(envs, overrides)
    emit('databasePools', {'plan': plan, 'total': sum(plan.values()), 'maxConnections': max_connections})
    errors = pool_budget_errors(plan, max_connections, reserved)
    if errors:
        raise RuntimeError('; '.join(errors))
    apply_activation('activate-database-pools', sha, run_id,
                     [(name, env) for name, env in overrides.items() if name in envs])


# --- dispatcher ------------------------------------------------------------------------------

def _platform(step):
    """A platform_operations / integrate_operations step bound to this module (CF Meet Platform
    "Build", ADR 0062 / 0063; CF Meet Apps, ADR 0064 / 0065)."""
    return lambda sha, run_id, confirm: step(sys.modules[__name__], sha, run_id, confirm)


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
    'activate-messaging-concurrency': ((), activate_messaging_concurrency),
    # Owner decision 2026-11-15: personal E2EE without an independent assessment (recorded).
    'activate-personal-encryption': (('activate-first-party-features',), activate_personal_e2ee),
    # Section 4 of the backend closure: Web Push with a VAPID key generated on the VPS.
    'activate-web-push': (('activate-personal-encryption',), activate_web_push),
    'activate-web-origins': (('activate-gateway-routing',), activate_web_origins),
    'activate-gateway-docs': (('activate-gateway-routing',), activate_gateway_docs),
    # P7: per-IP edge throttling (host nginx) and the gateway's per-token ceiling.
    'activate-edge-rate-limits': ((), activate_edge_rate_limits),
    'activate-token-ceiling': (('activate-gateway-routing',), activate_token_ceiling),
    # P16: service-auth key rotation of one service (confirm: service=<name>), three steps with
    # overlapping keys; each step journaled and undone by rollback (key_rotation.py).
    'activate-service-key-publish': ((), lambda sha, run_id, confirm: key_rotation.publish(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-service-key-switch': ((), lambda sha, run_id, confirm: key_rotation.switch(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-service-key-retire': ((), lambda sha, run_id, confirm: key_rotation.retire(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-developer-console': (('activate-developer-platform',), activate_developer_console),
    'activate-developer-webhooks': (('activate-developer-console',), activate_developer_webhooks),
    'activate-gateway-families': (('activate-gateway-routing', 'activate-developer-webhooks'), activate_gateway_families),
    'activate-usage-export': (('activate-developer-webhooks',), activate_usage_export),
    # Final completion: Resend becomes the email provider only after a verified send.
    'activate-resend-email': ((), lambda sha, run_id, confirm: mail_ops.activate_resend_email(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-production-access-review': (('activate-developer-console',), lambda sha, run_id, confirm:
                                          developer_access.activate(sys.modules[__name__], sha, run_id, confirm)),
    # HQ admin bridge (ADR 0068): owner grants for the served reads, bridge still disabled.
    'activate-admin-bridge': ((), activate_admin_bridge),
    # Runbook §3: switched on for HQ, read-only, with the tunnel-only relay (confirm=hq-enrolled-read-only).
    'activate-admin-bridge-hq': (('activate-admin-bridge',), activate_admin_bridge_hq),
    # The other owner families (grants, owner URL, modules), each after activate-admin-bridge.
    **{op: (('activate-admin-bridge',), activate_admin_bridge_owner(op)) for op in ADMIN_BRIDGE_OWNERS},
    # HQ stage S6: commands on at a newer signed policy (confirm=hq-commands-two-person).
    'activate-admin-bridge-hq-commands': (('activate-admin-bridge-hq',), activate_admin_bridge_hq_commands),
    # A7: owner events to the bridge's listener (one listener, every publisher; needs the bridge enabled).
    ADMIN_BRIDGE_EVENTS_OPERATION: (('activate-admin-bridge-hq',), activate_admin_bridge_events),
    # A6: Meeting and Messaging enforce CF Meet plan limits (confirm=plan-limits-reviewed).
    PLAN_ENFORCEMENT_OPERATION: (('activate-developer-platform',), activate_plan_enforcement),
    # A4: owner-delivered links sealed to the Notification Service (their I1 publications first).
    OWNER_INVITATIONS_OPERATION: (('activate-developer-publications',), activate_developer_owner_invitations),
    ORGANIZATION_EXPORTS_OPERATION: (('activate-organizations-publications',), activate_organization_export_delivery),
    # Notification Service N0-1: the missing Hub preference grant (meeting notices fail closed).
    'activate-meeting-notice-preferences': ((), activate_meeting_notice_preferences),
    # Notification Service N0-6: public host refuses the Meeting Service operator routes.
    'activate-public-internal-denial': ((), activate_public_internal_denial),
    # S1: per-visitor sign-in limits (host nginx X-Real-IP, Meeting AUTH_CLIENT_IP_HEADER).
    'activate-client-address-limits': ((), activate_client_address_limits),
    # S2: account deletion / data download reach Calling (call history).
    'activate-account-privacy': ((), activate_account_privacy),
    # B6 / C5: the bigger database pools, only within the core database's connection budget.
    'activate-database-pools': ((), activate_database_pools),
    audit_operations.CAPACITY_OPERATION: ((), _platform(audit_operations.activate_database_capacity)),
    send_policy_operations.OPERATION: ((), lambda sha, run_id, confirm:
        send_policy_operations.activate(sys.modules[__name__], sha, run_id, confirm)),
    meeting_recording_operations.OPERATION: ((), lambda sha, run_id, confirm:
        meeting_recording_operations.activate(sys.modules[__name__], sha, run_id, confirm)),
    meeting_recording_operations.DEVICE_OPERATION: ((), lambda sha, run_id, confirm:
        meeting_recording_operations.activate_device_local(sys.modules[__name__], sha, run_id, confirm)),
    audit_operations.LOGS_OPERATION: ((), _platform(audit_operations.activate_service_logs)),
    provider_operations.PHONE_OPERATION: ((), _platform(provider_operations.activate_phone)),
    provider_operations.NATIVE_OPERATION: ((), _platform(provider_operations.activate_native_push)),
    kt_witness_operations.OPERATION: ((), _platform(kt_witness_operations.activate)),
    # Notification Service N6: owner read APIs (X1-X5) granted to cfmeet.notifications.
    'activate-notifications-owners': ((), lambda sha, run_id, confirm: notif_ops.activate_owners(
        sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N14: one notif_routing row per run, gated, journaled, reversible.
    'activate-notifications-routing': ((), lambda sha, run_id, confirm: notif_ops.activate_routing(
        sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N8: Meeting Service user relay U1-U9 (owner-approved 2026-11-16).
    'activate-notifications-relay': ((), lambda sha, run_id, confirm: notif_ops.activate_relay(
        sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N7b: Messaging publishes message wake-ups to I1 (shadow first).
    'activate-messaging-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_messaging_publications(
        sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N7: the other producers' I1 grant (one producer per run).
    'activate-organizations-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_publications(
        sys.modules[__name__], sha, run_id, 'organizations')),
    'activate-communications-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_publications(
        sys.modules[__name__], sha, run_id, 'communications')),
    'activate-developer-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_publications(
        sys.modules[__name__], sha, run_id, 'developer-platform')),
    'activate-meeting-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_publications(
        sys.modules[__name__], sha, run_id, 'meeting-service')),
    'activate-calling-publications': ((), lambda sha, run_id, confirm: notif_ops.activate_publications(
        sys.modules[__name__], sha, run_id, 'calling')),
    # Notification Service N9: Web Push with Messaging's VAPID key, the registration mirror, call push.
    'activate-notifications-push': (('activate-web-push',), lambda sha, run_id, confirm: notif_ops.activate_push(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-messaging-push-mirror': (('activate-notifications-push',), lambda sha, run_id, confirm:
                                       notif_ops.activate_messaging_push_mirror(sys.modules[__name__], sha, run_id,
                                                                                confirm)),
    'activate-calling-push': (('activate-notifications-push',), lambda sha, run_id, confirm:
                              notif_ops.activate_calling_push(sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N12: the Account CRITICAL lane (X6 / X7 grant, fallback key, denials).
    'activate-critical-lane': ((), lambda sha, run_id, confirm: notif_ops.activate_critical_lane(
        sys.modules[__name__], sha, run_id, confirm)),
    # Notification Service N10: preference copy + catch-up, then the Hub facade switch.
    'activate-preference-copy': ((), lambda sha, run_id, confirm: notif_ops.activate_preference_copy(
        sys.modules[__name__], sha, run_id, confirm)),
    'activate-preference-authority': (('activate-preference-copy',), lambda sha, run_id, confirm:
                                      notif_ops.activate_preference_authority(sys.modules[__name__], sha, run_id,
                                                                              confirm)),
    # Notification Service N5 / N11: Resend as the service's own provider (VPS credentials).
    'activate-notifications-email': (('activate-resend-email',), lambda sha, run_id, confirm:
                                     notif_ops.activate_email(sys.modules[__name__], sha, run_id, confirm)),
    # Document 4 §9.6: one producer fence row per run, LEGACY_ONLY <-> SHADOW only, reversible.
    'activate-notification-fence': ((), lambda sha, run_id, confirm: notif_ops.activate_fence(
        sys.modules[__name__], sha, run_id, confirm)),
    # Audit M4: meeting notices reach the in-app inbox while their email stays with the Meeting
    # Service (confirm enable | disable; gated on the service's IN_APP row ACTIVE, EMAIL not).
    notif_ops.MEETING_IN_APP_OPERATION: (('activate-meeting-publications',), lambda sha, run_id, confirm:
                                         notif_ops.activate_meeting_in_app(sys.modules[__name__], sha, run_id,
                                                                           confirm)),
    # CF Meet Platform "Build" B1-B6 (ADR 0062 / 0063): partner users, user tokens and allowed
    # websites, partner chat, calls, push and meetings; the calling edge is a separate owner decision.
    **{op: (platform_ops.REQUIRES[op], _platform(step)) for op, step in platform_ops.ACTIVATIONS.items()},
    # CF Meet Apps I1-I7 (ADR 0064 / 0065): app registration and reviewers, OAuth, delegated
    # access, workspace installs, install events, the directory and in-meeting apps.
    **{op: (integrate_ops.REQUIRES[op], _platform(step)) for op, step in integrate_ops.ACTIVATIONS.items()},
}
OPERATIONS = ('inspect', 'backup', 'provision', 'release', 'verify', 'rollback', 'certify') + tuple(ACTIVATIONS) \
    + ('developer-acceptance', 'organization-acceptance', 'diagnose', 'email-acceptance', 'developer-access-review',
       'notifications-acceptance', 'deploy-monitoring', platform_ops.ACCEPTANCE)
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
        audit_operations.recording_receipts(sys.modules[__name__])
        meeting_recording_operations.provider_identity(sys.modules[__name__])
        notif_ops.notification_diagnostics(sys.modules[__name__])
        provider_operations.resend_domain_facts(sys.modules[__name__])
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
        import audit_release
        # Monitoring follows every successful release and never fails it (monitoring_stack.py).
        import monitoring_stack
        audit_release.release(sys.modules[__name__], sha, run_id,
            after_release=lambda: monitoring_stack.deploy_after_release(sys.modules[__name__], emit))
    elif operation == 'deploy-monitoring':
        # Prometheus, Alertmanager and the probe / heartbeat exporters only; no service changes.
        if confirm:
            raise RuntimeError('deploy-monitoring takes no confirmation')
        import monitoring_stack
        monitoring_stack.deploy(sys.modules[__name__], emit)
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
    elif operation == 'notifications-acceptance':
        # Real in-app delivery through the Notification Service: three fresh accounts and two
        # organizations (archived again) through the public API only. Needs the Organizations
        # producer and the ORGANIZATION_UPDATES IN_APP routing row ACTIVE.
        if not done('activate-organizations-publications'):
            raise RuntimeError('Acceptance needs activate-organizations-publications')
        modes = notif_ops.service_modes(sys.modules[__name__], 'ORGANIZATION_UPDATES')
        if modes.get('IN_APP') != 'ACTIVE':
            raise RuntimeError('Acceptance needs the ORGANIZATION_UPDATES IN_APP routing row ACTIVE '
                               '(activate-notifications-routing organization-updates-in-app-active)')
        import notifications_acceptance
        checks = notifications_acceptance.run(run_id, emit)
        emit('notificationsAcceptanceSummary', checks)
        notif_ops.notification_diagnostics(sys.modules[__name__])
        provider_operations.resend_domain_facts(sys.modules[__name__])
        if any(v != 'PASS' for v in checks.values()):
            raise RuntimeError('Notification acceptance failed')
    elif operation == platform_ops.ACCEPTANCE:
        # Two fresh developer workspaces through the public API only (cleaned up); every
        # recorded platform phase plus flag-off checks of the others.
        platform_ops.acceptance(sys.modules[__name__], sha, run_id, confirm)
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
