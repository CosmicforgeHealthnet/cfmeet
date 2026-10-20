# CF Meet Messaging

The Unified Messaging backend (Part V Workstreams C–D and Parts E–G,
[ADR 0045](../../docs/decisions/0045-messaging-domain-and-cryptographic-core.md),
[0046](../../docs/decisions/0046-platform-access-messaging-scopes-and-device-authority.md),
[0047](../../docs/decisions/0047-messaging-service-devices-and-key-transparency.md),
[0048](../../docs/decisions/0048-conversation-aggregate-security-profiles-and-ordering.md),
[0049](../../docs/decisions/0049-messaging-realtime-gateway.md),
[0050](../../docs/decisions/0050-messaging-attachments.md),
[0051](../../docs/decisions/0051-messaging-search-moderation-compliance.md)).
An independent service (`cfmeet.messaging`, port 8097) with its own PostgreSQL database.

It owns: conversations (eight generic context classes, persisted PRIVATE_E2EE / MANAGED
profiles, lifecycle, policy snapshots, the message-sequence allocator), Messaging devices and their **public** MLS material (device signature keys,
published KeyPackages), the append-only key-transparency log, membership, the message
store (opaque MLS ciphertext or sealed MANAGED text) and the MLS Delivery Service, the
durable per-conversation event log (outbox) and the realtime gateway, attachment
metadata (bytes in protected object storage), reports, moderation, legal holds,
retention / purge, exports, deletion requests, idempotency records and its audit. It never holds a private key, group / epoch secret or
plaintext E2EE content — MLS runs on client devices (`packages/messaging-crypto`).
Identities belong to Platform Access; Messaging reads no other service's database.

## API (contract `cfmeet.messaging.http-api`, served at `/openapi.json`)

Every `/v1` request carries a CF Meet API credential (`Authorization: Bearer …`, verified
by Platform Access) and the acting principal (`CF-Acting-Principal`), honoured only within
the credential's principal restriction and when Platform Access resolves it ACTIVE in the
credential's own environment. Client tokens are refused there
(`CLIENT_TOKENS_NOT_ENABLED`); only the device-direct realtime routes accept them.

| Route | Scope |
|---|---|
| `GET /health`, `GET /ready`, `GET /openapi.json` | — |
| `POST /v1/messaging/devices/registration-challenges` | `messages.send` |
| `POST /v1/messaging/devices` | `messages.send` |
| `POST /v1/messaging/devices/{deviceId}/key-packages` | `messages.send` |
| `POST /v1/messaging/devices/{deviceId}/revoke` | `messages.send` |
| `GET /v1/messaging/principals/{principalId}/devices` | `messages.read` |
| `GET /v1/messaging/key-transparency/proofs` | `messages.read` |
| `POST /v1/conversations` (`Idempotency-Key`) | `conversations.create` |
| `GET /v1/conversations`, `GET /v1/conversations/{id}` | `messages.read` (own, with acting principal) or `conversations.manage` (environment) |
| `PATCH /v1/conversations/{id}` | `conversations.manage` (owner when a principal acts) |
| `GET` / `PUT /v1/messaging/policy` | `conversations.manage` |
| Members: `GET` / `POST /v1/conversations/{id}/members`, `PATCH` (role) / `DELETE …/members/{principalId}` | `conversations.manage` / member rules |
| Bans: `GET` / `POST /v1/conversations/{id}/bans`, `DELETE …/bans/{principalId}` | `conversations.manage` (OWNER / ADMIN) |
| Replies / threads: `replyToMessageId` on send, `?threadRootId=` on list | `messages.send` / `messages.read` |
| Reactions (MANAGED; E2EE reactions are messages): `GET …/messages/{id}/reactions`, `PUT` / `DELETE …/reactions/{reaction}` | `messages.read` / `messages.send` |
| Pins: `GET /v1/conversations/{id}/pins`, `PUT` / `DELETE …/pins/{messageId}` (OWNER / ADMIN) | `messages.read` / `messages.send` |
| Unread: `GET /v1/conversations/{id}/unread` | `messages.read` |
| SMS Bridge (MANAGED only, explicit disclosure): `GET /v1/messaging/sms-bridge/disclosure`, `GET` / `POST …/sms-bridges`, `POST …/{bridgeId}/close`, `GET …/messages/{id}/sms-transport` | `messages.read` / `conversations.manage` |
| Usage facts (no prices): `GET /v1/messaging/usage?from=&to=` | `usage.read` |
| Messages: `GET` / `POST /v1/conversations/{id}/messages`, `GET` / `PATCH` / `DELETE …/messages/{messageId}`, `PUT …/read-cursor`, `GET …/events` | `messages.read` / `messages.send` |
| MLS Delivery Service: `…/mls/*` (commits, group state, KeyPackage claims, Welcomes) | `messages.read` / `messages.send` |
| Realtime: `POST /v1/messaging/client-tokens`, `POST /v1/messaging/client-sessions/{id}/revoke`, presence settings, push registrations | `messages.read` / `messages.send` |
| Realtime (device-direct): `POST /v1/realtime/tickets` (client token), `GET /v1/realtime` (WebSocket, single-use ticket) | — |
| Attachments: `POST …/attachments`, `POST …/{attachmentId}/finalize`, `GET …/{attachmentId}`, `POST …/download-grants`, `GET …/content` | `attachments.write` / `attachments.read` |
| Search (MANAGED only): `GET /v1/conversations/{id}/search`, `GET /v1/messaging/search` | `messages.read` |
| Reports: `POST /v1/conversations/{id}/reports` | `messages.send` |
| Moderation (administration): `GET /v1/messaging/reports[/{id}]`, `GET` / `POST /v1/messaging/moderation/actions` | `moderation.manage` |
| Compliance (administration): legal holds, deletion requests, `GET /v1/messaging/audit` | `conversations.manage` |
| Exports (administration, MANAGED only): `POST /v1/messaging/exports`, `GET …/{id}`, `POST …/{id}/download-grants` | `messaging.export` |

`/internal/v1/**` is service-authenticated per operation (receiver trust bundle): the
delegated first-party surface `/internal/v1/delegated/**` and Meeting conversation sync
(Meeting Service), `/internal/v1/context-authorities` (Privacy Relay) and
`/internal/v1/sms-bridge/inbound` (SMS Gateway); see contract
`cfmeet.messaging.internal-api`.

Customer webhooks are NOT a Messaging feature: public events are relayed to Calling's
platform-event ingress and delivered by the existing platform webhook system
(`conversation.*`, `message.*` — ids, sequences and codes only, never content).

## Running locally (no Docker required)

```bash
cp .env.example .env      # DATABASE_URL, CF_* service identity, Platform Access URL
make messaging-migrate
make messaging            # 127.0.0.1:8097
curl http://127.0.0.1:8097/ready
```

Only PostgreSQL (and, for the realtime tests, Redis) is needed: no container runtime.
Platform Access must run too (`make platform-access`); its trust bundle must grant
`cfmeet.messaging` `platform-credential.authenticate`, `platform-principal.resolve` and
`platform-identity.read`. Messaging's own bundle may trust no issuer (internal
operations then refuse every caller). SMS Gateway / Calling are optional
(`CF_SMS_GATEWAY_BASE_URL`, `CF_CALLING_BASE_URL`); without them bridges are refused and
webhook relays wait, in-app messaging is unaffected.

Operator CLI (no network API):

```bash
DATABASE_URL=… CF_OPERATOR_ID=ops-1 cargo run --bin messaging-admin -- kill-switch engage --switch MESSAGE_SEND --reason ABUSE_WAVE
cargo run --bin messaging-admin -- kill-switch list | incidents list | kt checkpoint --out FILE | kt verify --checkpoint FILE
```

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make messaging-test
```

Suites: `conversations`, `foundation`, `startup` (the real binary), `migrations`, `outbox`, `devices`
(real OpenMLS client devices), `transparency` (proofs verified with the RFC 9162
algorithms), `architecture`, `messaging` (message store and MLS Delivery Service with real
devices), `realtime` (real WebSocket clients), `realtime_redis` (two gateway instances,
`MESSAGING_TEST_REDIS_URL`), `attachments` (real MinIO with `OBJECT_STORAGE_TEST_ENDPOINT`,
real clamd with `MESSAGING_TEST_CLAMAV_ADDRESS`), `compliance`, `platform_access_e2e`
(the real Platform Access process and the real Messaging binary), `integrations`
(delegation, Meeting sync, context authority), `sms_bridge`, `engagement` (roles, bans,
replies, reactions, pins, unread, usage), `webhook_relay`, `query_plans` (EXPLAIN
evidence), `operations` (kill switches, incidents, KT rollback, operator CLI) and
`disaster_recovery` (backup / restore with `infra/deployment/backup`). `load` holds the
`#[ignore]`d measurements (`cargo test --release --test load -- --ignored --nocapture`).

## Deployment

Its own image (`services/messaging/Dockerfile`, repository-root context) and its own
release. Nothing deploys it automatically, and it is never part of the Meeting Service
deployment. E2EE device operations are disabled in staging until
`MESSAGING_E2EE_DEVICES_ENABLED=true` is set after the owner decisions of ADR 0045; in
production the value `true` is refused (production E2EE stays disabled pending external
cryptographic certification).
Optional capabilities fail closed when unconfigured: realtime (no
`MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE`), MANAGED text (no
`MESSAGING_MANAGED_KEYRING_FILE`), attachments (no `MESSAGING_ATTACHMENTS_S3_*`),
MANAGED scanning (no `MESSAGING_CLAMAV_ADDRESS`). See `.env.example`.

Key-transparency tree heads are signed (Part V G.5) with the Ed25519 key in
`MESSAGING_KT_SIGNING_KEY_FILE` for the log `MESSAGING_KT_LOG_ID`; both are required.
The key is a server secret separate from the service-authentication key; its public key
is what clients pin (the `keyId` of every head is the first 8 bytes of its SHA-256).
Signed heads are still `UNWITNESSED` (AD-05). Security architecture, AD register and
threat model: `docs/architecture/messaging-security-architecture.md`.
