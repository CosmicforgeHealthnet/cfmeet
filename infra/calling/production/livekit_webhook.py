"""LiveKit -> Calling provider callbacks (participant joined / left, room started / finished).

Calling moves a Call from ANSWERED to CONNECTED as soon as LiveKit reports the second
participant (POST /internal/provider/media/events, signed by LiveKit with Calling's API key).
Without a `webhook:` block in /opt/livekit/livekit.yaml nothing is delivered and Calls only
connect on Calling's periodic reconcile tick (10-15 s after answer).

Operations:
  livekit-webhook-inspect   read-only: webhook facts (booleans and counts only) and whether the
                            Calling callback route answers on the target LiveKit would use.
  livekit-webhook           adds exactly one `webhook:` block (Calling's own LiveKit key, Calling's
                            callback URL) and restarts lk-server. Refuses: a different existing
                            webhook block (never overwritten), a webhook key that is not one of
                            LiveKit's keys, an unreachable callback route, or any active
                            participant (the restart would drop media). Every other key of the
                            file is proved unchanged before the write; failure restores the
                            previous file. Afterwards a signed delivery is proved end to end: a
                            probe room named like a Calling resource for a Call that does not
                            exist is created and deleted, and Calling must have recorded LiveKit's
                            callback for it (quarantined: no Call matches; nothing else changes).
  livekit-webhook-rollback  restores the file saved by `livekit-webhook`, only while the current
                            file is still exactly what that operation wrote.

Nothing secret is printed: no key ids, secrets, URLs with credentials or addresses.
"""
import json
import time
import uuid
from pathlib import Path

import production as p

CONFIG = Path('/opt/livekit/livekit.yaml')
EVIDENCE = p.ROOT / 'livekit-webhook'
CALLBACK_PATH = '/internal/provider/media/events'
# The Calling loopback proxy (cfmeet-calling-loopback) publishes Calling on the host loopback;
# a host-network LiveKit reaches it there. A bridged LiveKit on the core network reaches the
# Calling container directly.
HOST_URL = 'http://127.0.0.1:18090' + CALLBACK_PATH
BRIDGE_URL = 'http://cfmeet-calling:8090' + CALLBACK_PATH
# The probe room's callbacks: resource events that matched no Call (quarantined, no call id).
PROBE_SQL = ("SELECT count(*) FROM call_media_provider_events WHERE call_id IS NULL AND outcome = 'QUARANTINED' "
             "AND event_kind IN ('RESOURCE_STARTED','RESOURCE_FINISHED') AND received_at >= to_timestamp(%d)")


def emit(kind, value):
    print(json.dumps({kind: value}), flush=True)


def target_url(container, net=None):
    """Where lk-server can reach Calling's callback route, or None."""
    host = container.get('HostConfig', {})
    if host.get('NetworkMode') == 'host':
        return HOST_URL
    if (net or p.NET) in (container.get('NetworkSettings', {}).get('Networks') or {}):
        return BRIDGE_URL
    return None


def desired_block(url, key):
    return {'api_key': key, 'urls': [url]}


def render(original, block):
    """The file with exactly one appended webhook block (json scalars are valid YAML)."""
    return (original.rstrip() + '\nwebhook:\n  api_key: ' + json.dumps(block['api_key'])
            + '\n  urls:\n' + ''.join('    - %s\n' % json.dumps(u) for u in block['urls']))


def plan(original, url, key):
    """('present' | 'add', new_text) or raises when the change is not safe to make."""
    import yaml
    before = yaml.safe_load(original) or {}
    block = desired_block(url, key)
    if key not in (before.get('keys') or {}):
        raise RuntimeError("Calling's LiveKit key is not one of LiveKit's keys; refusing")
    existing = before.get('webhook')
    if existing == block:
        return 'present', original
    if existing:
        raise RuntimeError('LiveKit already has a different webhook block; refusing to overwrite it')
    changed = render(original, block)
    after = yaml.safe_load(changed)
    if {k: v for k, v in after.items() if k != 'webhook'} != before or after.get('webhook') != block:
        raise RuntimeError('rendered configuration differs outside the webhook block; refusing')
    return 'add', changed


def route_status(url):
    """HTTP status of an unsigned POST to the callback route, from where LiveKit runs.
    Calling answers 401 to an unsigned callback: the route is reachable and verifying."""
    if url == HOST_URL:
        out = p.run('curl', '-s', '-o', '/dev/null', '-w', '%{http_code}', '--max-time', '10',
                    '-X', 'POST', '-H', 'Content-Type: application/json', '--data', '{}', url)
    else:
        out = p.run('docker', 'run', '--rm', '--network', p.NET, 'curlimages/curl:8.12.1', '-s', '-o', '/dev/null',
                    '-w', '%{http_code}', '--max-time', '10', '-X', 'POST', '-H', 'Content-Type: application/json',
                    '--data', '{}', url)
    return int(out or 0)


def participants():
    return sum(int(r.get('numParticipants', r.get('num_participants', 0))) for r in p.current_rooms())


def calling_key():
    return p.secret('calling-livekit-api-key'), p.secret('calling-livekit-api-secret')


def facts():
    import yaml
    config = yaml.safe_load(CONFIG.read_text()) or {}
    webhook = config.get('webhook') or {}
    key, _ = calling_key()
    container = p.inspect('lk-server')
    url = target_url(container)
    value = {'webhook_configured': bool(webhook.get('urls')),
             'webhook_is_callings': webhook == desired_block(url, key) if url else False,
             'calling_key_in_livekit_keys': key in (config.get('keys') or {}),
             'livekit_network_mode': container.get('HostConfig', {}).get('NetworkMode'),
             'callback_target': 'host-loopback' if url == HOST_URL else 'core-network' if url else None}
    if url:
        try:
            value['callback_route_unsigned_status'] = route_status(url)
        except Exception as error:
            value['callback_route_unsigned_status'] = type(error).__name__
    return value, url


def restart_and_wait():
    p.run('docker', 'restart', 'lk-server')
    for _ in range(30):
        try:
            p.current_rooms()
            return
        except Exception:
            time.sleep(2)
    raise RuntimeError('LiveKit failed restart validation')


def prove_delivery():
    """Create and delete a probe room for a Call id that does not exist; Calling must record
    LiveKit's signed RESOURCE_STARTED / RESOURCE_FINISHED callbacks for it."""
    key, password = calling_key()
    since = int(time.time()) - 1
    room = 'cfc-' + uuid.uuid4().hex
    p.lk_request('RoomService/CreateRoom', {'name': room, 'emptyTimeout': 30}, key, password,
                 {'roomCreate': True})
    try:
        time.sleep(3)
    finally:
        p.lk_request('RoomService/DeleteRoom', {'room': room}, key, password, {'roomCreate': True})
    for _ in range(15):
        count = int((p.run('docker', 'exec', '-u', 'postgres', 'cfmeet-core-db', 'psql', '-XAtq', '-U', 'postgres',
                           '-d', 'cfmeet_calling', '-c', PROBE_SQL % since) or '0').strip())
        if count >= 1:
            return count
        time.sleep(2)
    return 0


def apply(run_id):
    value, url = facts()
    emit('livekitWebhookBefore', value)
    if not url:
        raise RuntimeError('lk-server is neither host-network nor on the core network; no callback target')
    if value.get('callback_route_unsigned_status') != 401:
        raise RuntimeError('Calling callback route is not answering 401 to an unsigned callback; refusing')
    key, _ = calling_key()
    original = CONFIG.read_text()
    action, changed = plan(original, url, key)
    if action == 'add':
        active = participants()
        if active:
            raise RuntimeError('restart deferred: %d participant(s) active in LiveKit; retry when idle' % active)
        stamp = '%s-%d' % (run_id, time.time_ns())
        p.write_private(EVIDENCE / ('livekit.yaml.before.' + stamp), original)
        p.write_private(EVIDENCE / ('livekit.yaml.after.' + stamp), changed)
        try:
            # Write through the existing inode: LiveKit bind-mounts this individual file.
            CONFIG.write_text(changed)
            restart_and_wait()
            p.regression()
        except Exception:
            CONFIG.write_text(original)
            restart_and_wait()
            print('LIVEKIT_ROLLBACK: previous configuration restored', flush=True)
            raise
    delivered = prove_delivery()
    emit('livekitWebhookAfter', dict(facts()[0], action=action, probe_callbacks_recorded=delivered))
    if not delivered:
        raise RuntimeError('LiveKit did not deliver a signed callback that Calling recorded')
    print('LIVEKIT_WEBHOOK: Calling receives signed LiveKit callbacks (%s)' % action, flush=True)


def rollback(run_id):
    saved = sorted(EVIDENCE.glob('livekit.yaml.after.*'))
    if not saved:
        raise RuntimeError('no livekit-webhook change recorded; nothing to roll back')
    after = saved[-1]
    before = after.with_name(after.name.replace('.after.', '.before.'))
    if CONFIG.read_text() != after.read_text():
        raise RuntimeError('LiveKit configuration changed since livekit-webhook; refusing to roll back')
    active = participants()
    if active:
        raise RuntimeError('restart deferred: %d participant(s) active in LiveKit; retry when idle' % active)
    CONFIG.write_text(before.read_text())
    restart_and_wait()
    p.regression()
    after.rename(after.with_name(after.name.replace('.after.', '.rolled-back.')))
    emit('livekitWebhookRolledBack', facts()[0])


def run(operation, sha, run_id):
    if operation == 'livekit-webhook-inspect':
        emit('livekitWebhook', facts()[0])
    elif operation == 'livekit-webhook':
        apply(run_id)
    elif operation == 'livekit-webhook-rollback':
        rollback(run_id)
    else:
        raise RuntimeError('unknown operation')
