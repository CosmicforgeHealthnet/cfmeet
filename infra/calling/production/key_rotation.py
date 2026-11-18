"""P16: rotation of a service's service-auth signing key (overlapping keys, three steps).

Imported by coordinated_release.py. Each step is a separate, journaled activation for ONE
service, named in the run's confirmation as `service=<name>` (e.g. `service=calling`):

1. `activate-service-key-publish` — generates the next Ed25519 key on the VPS
   (`<identity>-service-auth.<kid>.pem`, kid `<identity>-production-<n+1>`, owner = the
   service, mode 0400) and ADDS its public key next to the current one in every receiver's
   trust bundle that trusts the service (permissions unchanged). Receivers are recreated with
   the new bundle (same image and configuration); the service still signs with the old key.
2. `activate-service-key-switch` — requires step 1 for that service. Recreates the service
   with `CF_SERVICE_AUTH_KEY_ID=<new kid>` and the new key file mounted at
   `/run/secrets/service-auth.pem`. Both keys are trusted, so no request fails during the
   switch; later releases clone the running container, so the new key stays in use.
3. `activate-service-key-retire` — requires step 2 for that service and at least
   RETIRE_AFTER since it (every token signed with the old key has expired: lifetimes are
   at most 300 s). Removes the OLD kid from every receiver's bundle; the old key file is kept
   on disk (never deleted by tooling) for the record.

Every step is undone by `rollback` (the previous containers and bundle files are restored);
a failed step restores itself. Steps never print key material and never modify a previous
bundle file (new bundle versions are written next to it). Runbook:
docs/operations/coordinated-production-release.md (service key rotation).
"""

import copy
import json
import re
from datetime import datetime, timedelta, timezone

import production as p

OPERATIONS = ('activate-service-key-publish', 'activate-service-key-switch', 'activate-service-key-retire')
KEY_TARGET = '/run/secrets/service-auth.pem'
RETIRE_AFTER = timedelta(minutes=15)
KID = re.compile(r'^(?P<identity>[a-z][a-z0-9-]*)-production-(?P<n>[0-9]{1,4})$')


def service_from(cr, confirm):
    """The `service=<name>` of the confirmation: a released service with a signing identity."""
    names = [w.split('=', 1)[1] for w in (confirm or '').split(',') if w.strip().startswith('service=')]
    if len(names) != 1 or names[0] not in cr.SERVICES:
        raise RuntimeError('Name exactly one service to rotate (confirm: service=<name>)')
    return names[0]


def next_kid(kid):
    m = KID.match(kid or '')
    if not m:
        raise RuntimeError('Unexpected signing key id; refusing to derive the next one')
    return '%s-production-%d' % (m.group('identity'), int(m.group('n')) + 1)


def add_key(bundle, issuer, key):
    """`bundle` with `key` trusted for `issuer` next to its current keys (None: not a receiver)."""
    out = copy.deepcopy(bundle)
    for entry in out['trusted_issuers']:
        if entry['issuer'] == issuer:
            if not any(k.get('kid') == key['kid'] for k in entry['keys']):
                entry['keys'].append(key)
            return out
    return None


def remove_key(bundle, issuer, kid):
    """`bundle` without `kid` for `issuer` (None: not a receiver). Never removes the last key."""
    out = copy.deepcopy(bundle)
    for entry in out['trusted_issuers']:
        if entry['issuer'] == issuer:
            keys = [k for k in entry['keys'] if k.get('kid') != kid]
            if not keys:
                raise RuntimeError('Refusing to remove the last trusted key of %s' % issuer)
            entry['keys'] = keys
            return out
    return None


def receivers(cr):
    """Running services with a trust bundle: {name: (target, source, bundle)}."""
    out = {}
    for name in cr.ORDER:
        container = cr.SERVICES[name]['container']
        if container not in cr.container_names():
            continue
        c = p.inspect(container)
        if not p.env_of(c).get('CF_SERVICE_TRUST_BUNDLE_FILE'):
            continue
        out[name] = cr.current_bundle(c)
    return out


def stat_of(cr, source):
    """The superseded bundle file's ownership and mode (the new version copies them)."""
    return cr.os.stat(source)


def changed_bundles(cr, transform, run_id):
    """{receiver: (target, new bundle path)} for every bundle `transform` changes."""
    out = {}
    for name, (target, source, bundle) in receivers(cr).items():
        new = transform(bundle)
        if new is None or new == bundle:
            continue
        p.write_private(cr.WORK / 'trust' / 'backup' / ('%s.%s.before.json' % (name, run_id)), json.dumps(bundle))
        out[name] = (target, cr.write_bundle(name, new, run_id, stat_of(cr, source)))
    return out


def apply_bundles(cr, operation, sha, run_id, bundles, record):
    """Recreates every receiver with its new bundle; failure restores the step."""
    entry = cr.begin(operation, sha, run_id)
    entry['keyRotation'] = record
    cr.commit(entry)
    try:
        for name, bundle in bundles.items():
            cr.recreate(name, {}, bundle, run_id, entry)
    except Exception:
        for component in reversed(entry['components']):
            cr.restore(component, run_id)
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)
    p.regression()
    return entry


def last_done(cr, operation, service):
    found = [e for e in cr.load_journal() if e['operation'] == operation and e['status'] == 'done'
             and (e.get('keyRotation') or {}).get('service') == service]
    return found[-1] if found else None


def current_identity(cr, name):
    c = p.inspect(cr.SERVICES[name]['container'])
    e = p.env_of(c)
    if not e.get('CF_SERVICE_AUTH_KEY_ID') or e.get('CF_SERVICE_AUTH_PRIVATE_KEY_FILE') != KEY_TARGET:
        raise RuntimeError('%s has no rotatable service signing identity' % name)
    return e['CF_SERVICE_AUTH_KEY_ID']


def publish(cr, sha, run_id, confirm):
    name = service_from(cr, confirm)
    cr.require_release(sha)
    old = current_identity(cr, name)
    new = next_kid(old)
    s = cr.SERVICES[name]
    key = p.SECRETS / ('%s-service-auth.%s.pem' % (s['identity'] or name, new))
    if not key.exists():
        p.run('openssl', 'genpkey', '-algorithm', 'ED25519', '-out', str(key))
    cr.os.chown(key, s['uid'], s['uid'])
    cr.os.chmod(key, 0o400)
    import communications_release as release
    jwk = release.public_key(key, new)
    issuer = cr.service_id(name)
    bundles = changed_bundles(cr, lambda b: add_key(b, issuer, jwk), run_id)
    cr.emit('serviceKeyPublish', {'service': name, 'oldKid': old, 'newKid': new, 'receivers': sorted(bundles)})
    apply_bundles(cr, 'activate-service-key-publish', sha, run_id, bundles,
                  {'service': name, 'oldKid': old, 'newKid': new, 'keyFile': str(key)})


def switch(cr, sha, run_id, confirm):
    name = service_from(cr, confirm)
    cr.require_release(sha)
    published = last_done(cr, 'activate-service-key-publish', name)
    if not published:
        raise RuntimeError('Run activate-service-key-publish for %s first' % name)
    record = published['keyRotation']
    if current_identity(cr, name) != record['oldKid']:
        raise RuntimeError('%s no longer signs with %s; publish again' % (name, record['oldKid']))
    entry = cr.begin('activate-service-key-switch', sha, run_id)
    entry['keyRotation'] = dict(record, switchedAt=cr.now())
    cr.commit(entry)
    try:
        cr.recreate(name, {'CF_SERVICE_AUTH_KEY_ID': record['newKid']}, (KEY_TARGET, record['keyFile']), run_id, entry)
    except Exception:
        for component in reversed(entry['components']):
            cr.restore(component, run_id)
        entry['status'] = 'failed'
        cr.commit(entry)
        raise
    entry['status'] = 'done'
    cr.commit(entry)
    cr.emit('serviceKeySwitch', {'service': name, 'kid': record['newKid']})
    p.regression()


def retire(cr, sha, run_id, confirm, now=None):
    name = service_from(cr, confirm)
    cr.require_release(sha)
    switched = last_done(cr, 'activate-service-key-switch', name)
    if not switched:
        raise RuntimeError('Run activate-service-key-switch for %s first' % name)
    record = switched['keyRotation']
    now = now or datetime.now(timezone.utc)
    at = datetime.fromisoformat(record['switchedAt'].replace('Z', '+00:00'))
    if now - at < RETIRE_AFTER:
        raise RuntimeError('Wait until tokens signed with %s have expired (%s after the switch)'
                           % (record['oldKid'], RETIRE_AFTER))
    if current_identity(cr, name) != record['newKid']:
        raise RuntimeError('%s does not sign with %s; refusing to retire %s' % (name, record['newKid'], record['oldKid']))
    issuer = cr.service_id(name)
    bundles = changed_bundles(cr, lambda b: remove_key(b, issuer, record['oldKid']), run_id)
    cr.emit('serviceKeyRetire', {'service': name, 'retiredKid': record['oldKid'], 'receivers': sorted(bundles)})
    apply_bundles(cr, 'activate-service-key-retire', sha, run_id, bundles, dict(record, retired=True))
