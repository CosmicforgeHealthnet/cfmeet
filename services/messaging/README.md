# CF Meet Messaging

The Unified Messaging backend foundation (Part V Workstreams C–D,
[ADR 0045](../../docs/decisions/0045-messaging-domain-and-cryptographic-core.md),
[0046](../../docs/decisions/0046-platform-access-messaging-scopes-and-device-authority.md),
[0047](../../docs/decisions/0047-messaging-service-devices-and-key-transparency.md),
[0048](../../docs/decisions/0048-conversation-aggregate-security-profiles-and-ordering.md)).
An independent service (`cfmeet.messaging`, port 8097) with its own PostgreSQL database.

It owns: conversations (eight generic context classes, persisted PRIVATE_E2EE / MANAGED
profiles, lifecycle, policy snapshots, the message-sequence allocator), Messaging devices and their **public** MLS material (device signature keys,
published KeyPackages), the append-only key-transparency log, idempotency records, its
audit and its transactional outbox. It never holds a private key, group / epoch secret or
plaintext E2EE content — MLS runs on client devices (`packages/messaging-crypto`).
Identities belong to Platform Access; Messaging reads no other service's database.

## API (contract `cfmeet.messaging.http-api`, served at `/openapi.json`)

Every `/v1` request carries a CF Meet API credential (`Authorization: Bearer …`, verified
by Platform Access) and the acting principal (`CF-Acting-Principal`), honoured only within
the credential's principal restriction and when Platform Access resolves it ACTIVE in the
credential's own environment. Client tokens are refused (`CLIENT_TOKENS_NOT_ENABLED`).

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

`/internal/v1/**` is service-authenticated and exposes no operation yet.

## Running locally (no Docker required)

```bash
cp .env.example .env      # DATABASE_URL, CF_* service identity, Platform Access URL
make messaging-migrate
make messaging            # 127.0.0.1:8097
curl http://127.0.0.1:8097/ready
```

Platform Access's trust bundle must grant `cfmeet.messaging`
`platform-credential.authenticate`, `platform-principal.resolve` and
`platform-identity.read`. Messaging's own bundle trusts no issuer.

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make messaging-test
```

Suites: `conversations`, `foundation`, `startup` (the real binary), `migrations`, `outbox`, `devices`
(real OpenMLS client devices), `transparency` (proofs verified with the RFC 9162
algorithms), `architecture`, and `platform_access_e2e` (the real Platform Access process).

## Deployment

Its own image (`services/messaging/Dockerfile`, repository-root context) and its own
release. Nothing deploys it automatically, and it is never part of the Meeting Service
deployment. E2EE device operations are disabled in staging / production until
`MESSAGING_E2EE_DEVICES_ENABLED=true` is set after the owner decisions of ADR 0045.
