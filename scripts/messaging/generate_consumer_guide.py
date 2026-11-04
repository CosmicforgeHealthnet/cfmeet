"""Account-native transport map derived from the real relay allowlist and contracts.
Run --check in backend CI. No frontend code or generated browser asset is touched.
"""
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
TARGET = ROOT / 'docs/operations/messaging-native-api-map.md'
RELAY = ROOT / 'services/meeting-service/src/services/unified_messaging/relay.rs'
CONTRACT = ROOT / 'packages/contracts/bundled/public/messaging/v1.openapi.json'
ADMIN_PREFIXES = ('/v1/messaging/policy', '/v1/messaging/legal-holds', '/v1/messaging/reports',
    '/v1/messaging/moderation', '/v1/messaging/exports', '/v1/messaging/deletion-requests', '/v1/messaging/audit')


def shape(path):
    return '/'.join('Reaction' if x == '{reaction}' else 'Id' if x.startswith('{') else x
                    for x in path.removeprefix('/v1/').split('/'))


def mappings(source):
    table = source.split('pub static ROUTES:', 1)[1].split('\n];', 1)[0]
    routes = set()
    for method, segments in re.findall(r'r\(\s*Method::(\w+),\s*&\[([^]]+)\]', table):
        parts = re.findall(r'Lit\("([^"]+)"\)|\b(Id|Reaction)\b', segments)
        routes.add((method.lower(), '/'.join(a or b for a, b in parts)))
    direct = dict(((method, path), native) for method, path, native in re.findall(
        r'\("(\w+)",\s*"([^"]+)"\)\s*=>\s*(?:\{\s*)?Some\(\s*"([^"]+)"\s*\)', source))
    assert len(direct) == 6, 'Identity bridge documentation map changed; review parser'
    return routes, direct


def render():
    contract = json.loads(CONTRACT.read_text(encoding='utf-8'))
    routes, direct = mappings(RELAY.read_text(encoding='utf-8'))
    rows, native, served = [], 0, 0
    for path, item in contract['paths'].items():
        for method, op in item.items():
            if method not in ('get', 'post', 'put', 'patch', 'delete'): continue
            is_admin = path.startswith(ADMIN_PREFIXES)
            if not is_admin: served += 1
            transport = direct.get((method, path))
            if not transport and (method, shape(path)) in routes:
                transport = '/api/v2/messaging/relay' + path
            if transport:
                native += 1
                audience = 'Account session'
            elif path.startswith('/v1/realtime'):
                transport, audience = path, 'Device token / one-use WS ticket'
            else:
                transport = 'Not exposed by Account relay'
                audience = 'Application administration; excluded from public Swagger' if is_admin else 'Separate service/application context; not native personal Messaging'
            rows.append('| ' + ' | '.join((method.upper(), '`' + path + '`', op['operationId'],
                '`' + transport + '`' if transport.startswith('/') else transport, audience)) + ' |')
    return f'''# Messaging — native authenticated API map

Generated from the Account relay and Messaging {contract['info']['version']} contract.
Canonical SDK contract: **{len(rows)} operations**; audience-filtered public Swagger:
**{served} operations**; native Account transports: **{native} operations**.
There are no new canonical message/conversation/device endpoints.

CF Meet consumer REST calls use the native path below with the existing Account bearer
session. Account derives the principal/scope and signs the internal request. Never send
service credentials, an operator token or `CF-Acting-Principal` from a client. The SDK's
canonical application API paths use a separately authenticated application gateway;
their presence in the catalogue does not prove public VPS routing. Native aliases use
the same Messaging request/response schemas and preserve owner status/error bodies.

Public Swagger JSON: `https://api.cfmeet.com/api-docs/messaging.json`. Each native
operation carries `x-cfmeet-native-path`, `x-cfmeet-native-auth` and
`x-cfmeet-native-principal`. Source schemas, scopes, parameters, errors, pagination,
examples and operation IDs remain in
`packages/contracts/messaging/http/v1.openapi.json` and its self-contained bundles.
An operation's presence does not certify runtime availability.

## Security and device prerequisites

Production PRIVATE_E2EE enrollment is BLOCKED pending independent assessment and
accepted cryptographic decisions. Production continues rejecting the enable flag.
Disabled enrollment returns 503 `FEATURE_DISABLED` / `E2EE_DEVICES_DISABLED`; private
conversation creation returns 409 `E2EE_NOT_AVAILABLE`. Neither chooses MANAGED.
`GET /api/v2/communications/me/capabilities` must be checked; a ready HTTP service
alone does not imply device security readiness or witnessed transparency.

Authenticated, active users first resolve their own identity with
`GET /api/v2/messaging/me`, establish an accepted Communications connection, then enroll
devices in an authorized non-production environment. Challenge -> client-held Ed25519/
HPKE keys -> challenge-bound MLS registration KeyPackage -> publish one-use public
KeyPackages. No server private-key read/decrypt endpoint exists. Signed tree heads and
inclusion/consistency proofs need pinned log/witness keys from a trusted release channel.
Never treat UNWITNESSED as witnessed. Account login/logout and device/session revocation
are separate from optional encrypted history transfer; a new device gets no old epoch
keys automatically. Backend backups store opaque client-encrypted bytes only.

Use the shared crypto core's `prepare_add_devices`, `prepare_remove_devices` and
`prepare_update_keys`; submit the Commit with its old epoch, wait for matching canonical
acceptance, then `accept_pending_commit`. Publish Welcomes only after acceptance.
On confirmed rejection/supersession use `discard_pending_commit` and process the winning
Commit. On ambiguous timeout reconcile/retry the SAME clientMessageId/bytes before
discarding. Exported pending state must be encrypted under device-protected storage.
Immediate local-simulation compatibility helpers are not the network commit protocol.
Frontend/native bindings must adopt this staged lifecycle before production client approval.

For send/edit use a stable clientMessageId and the same ciphertext on retries; generate
fresh ciphertext only for a new submission. Conversation create forwards a stable
`Idempotency-Key`. Direct reuse preserves canonical IDs. `expectedVersion` and
`expectedEditCount` protect updates. Respect 429 and Retry-After; do not reset rate limits.
Messages page by afterSequence/beforeSequence; conversations use cursor/limit; replay
uses afterEventSequence. Only committed owner events are delivery evidence. A WebSocket
notice or push is not proof of durable storage; acknowledge monotonic event cursors and
recover gaps from PostgreSQL-backed history after reconnect.

PRIVATE_E2EE clients submit authenticated MLS PrivateMessage application ciphertext;
public application frames are refused with 422 `CONTENT_REJECTED` /
`APPLICATION_MUST_BE_PRIVATE`. The server checks framing/device/membership, while
clients verify AEAD, sender binding and AAD before rendering. Private edits/reactions are
encrypted application messages; MANAGED row-based edit/reaction APIs do not relabel them.
Reply/thread IDs are disclosed metadata; quoted text remains encrypted.

Attachments: encrypt on device -> upload intent (ciphertext size/digest,
application/octet-stream) -> upload with the exact signed headers -> finalize -> attach
the ID to the encrypted message -> obtain an authorized short-lived download grant ->
verify/decrypt on the recipient. Private file names, descriptors and attachment keys stay
inside MLS. Private files are NOT_APPLICABLE_ENCRYPTED for scanning, never falsely clean.
Managed files need explicit allowed/disclosed context, encryption at rest and a successful
malware scan before READY. Server thumbnails exist only for supported MANAGED images.
Native clients download from grants; the JSON relay is not a binary-content proxy.

Realtime: obtain a short-lived device-bound client token through the Account bridge,
POST `/v1/realtime/tickets` with `Authorization: CF-Client-Token <token>`, then connect to
`wss://api.cfmeet.com/v1/realtime` using the returned one-use ticket in
Sec-WebSocket-Protocol, never the URL. Browser origins must match the existing allowlist.
Typing/presence are ephemeral and privacy-filtered; durable events are replayed by cursor.
Expired/revoked tickets, sessions or devices fail closed.

Push registration uses the device owner; payloads contain routing metadata only.
APNs/FCM/Web Push delivery needs real configured providers and physical-device evidence.
Recipient contact MUTED suppresses delayed notifications. MENTIONS also suppresses
ordinary opaque pushes because the server cannot infer private mentions. Group membership,
blocks, privacy and account state are rechecked before delayed governed push dispatch.
Logout/all-session revocation disables registrations without deleting device identities.

Negative examples: no accepted connection => direct create/send refused; foreign or
revoked device => token/send denied; missing group membership => messages/attachments
not found; stale epoch => conflict; private-to-managed mutation => immutable-profile
refusal; Hub unavailable => governed sends/push fail closed. No phone verification is
required. Meetings and established permitted service contexts retain independent rules.

## Operation map

| Method | Canonical schema path | Operation ID | Actual native/public device path | Audience |
|---|---|---|---|---|
''' + '\n'.join(rows) + '\n'


if __name__ == '__main__':
    rendered = render()
    if '--check' in sys.argv:
        assert TARGET.is_file() and TARGET.read_text(encoding='utf-8') == rendered, 'Native Messaging guide is stale'
        print('Native Messaging contract/route guide current')
    else:
        TARGET.write_text(rendered, encoding='utf-8')
        print('Wrote native Messaging contract/route guide')
