# CF Meet Privacy Relay service

`services/privacy-relay/` is the Rust/Axum peer service that is authoritative for
**Communication Contexts** and **Privacy Relay Sessions** (Part D §26–§30,
[ADR 0010](../../docs/decisions/0010-communication-context-and-privacy-relay-session-architecture.md)).
It owns its own PostgreSQL database, shares no source with any other service and depends
only on `packages/service-auth` and the contracts in `packages/contracts/privacy-relay/`.

It is generic: a context is a bounded relationship (an order, an appointment, a ticket …)
described only by opaque references, participants, channels, directions, validity and
policy. No consumer's product model and no product-name branching exist here.

## What is implemented

| Section | Status |
|---|---|
| §26 Privacy Relay as a peer service: own database, internal API only, customers through Calling's platform ingress | **Implemented** |
| §27 Communication Context: scope, `externalContextReference` (unique per scope, never logged), participants (principal id or opaque ContactReference id), channels, BIDIRECTIONAL / EXPLICIT directions, validity + grace, relay strategies, fail-safe policy, read-time authority | **Implemented** |
| §28 Consumer vs CF Meet responsibilities: the consumer states the relationship; CF Meet authorizes each communication and enforces | **Implemented** |
| §29 Updates, participant reassignment, suspend / resume / complete / revoke, expiry — versioned (`expectedVersion`), idempotent, audited, enforced on active calls | **Implemented** |
| §30 Privacy Relay Sessions: narrowing-only scopes (INHERITED / NARROWED), snapshot sync on every context change, cascades, `communication.authorize` (context ∩ session), recorded decisions | **Implemented** |
| Feature flag `PRIVACY_RELAY_ENABLED` (default `false`; restrictive operations always work) | **Implemented** |

**Not implemented (later sections — nothing is faked):** Contact Vault (§31–§33: no
protected contact table, no ciphertext, no telephone numbers, no fingerprints, no KMS),
relay-number inventory, public-network / callback relay execution and telephony. The
PUBLIC_RELAY_ALLOWED / CALLBACK_RELAY_ALLOWED strategies are policy only;
`executableRoutes` lists APPLICATION alone.

## API

Internal, service-authenticated (`X-CF-Service-Authorization`, ADR 0004) — contract
[`privacy-relay/internal/v1.openapi.json`](../../packages/contracts/privacy-relay/internal/v1.openapi.json).
Every request also carries a delegated platform context (the customer scope Calling
authenticated); scope comes only from it. Commands take
`{ "idempotencyKey": …, "request": … }`.

| Route | Permission (received by Privacy Relay) |
|---|---|
| `POST /internal/v1/contexts`, `POST …/contexts/{id}/update\|suspend\|resume\|complete\|revoke` | `communication-context.write` |
| `POST /internal/v1/contexts/query`, `GET …/contexts/{id}`, `GET …/contexts/{id}/evidence` | `communication-context.read` |
| `POST /internal/v1/contexts/{id}/sessions`, `POST …/sessions/{id}/suspend\|resume\|revoke` | `relay-session.write` |
| `POST /internal/v1/sessions/query`, `GET …/sessions/{id}` | `relay-session.read` |
| `POST /internal/v1/communication/authorize` | `communication.authorize` |
| `GET /health` | none |

The public customer API (`/v1/privacy-relay/...`, contract
[`privacy-relay/http/v1.openapi.json`](../../packages/contracts/privacy-relay/http/v1.openapi.json))
is served by Calling, which forwards to the routes above. Privacy Relay never sees a
customer credential.

Active-call enforcement goes the other way: Privacy Relay calls Calling's
`POST /internal/v1/privacy-relay/enforcement` (permission
`communication-authority.enforce`, granted in **Calling's** trust bundle; contract
[`privacy-relay/internal/enforcement`](../../packages/contracts/privacy-relay/internal/enforcement/v1.schema.json)).

## Model

| Table | Purpose |
|---|---|
| `communication_contexts` (+ `_participants`, `_channels`, `_directions`) | Contexts, versioned; triggers guard identity columns, lifecycle transitions and terminal reassignment |
| `privacy_relay_sessions` (+ `_participants`, `_channels`, `_directions`) | Sessions; containment trigger keeps validity inside the context |
| `relay_authorization_decisions` | Every AUTHORIZED / DENIED decision with versions and reason |
| `relay_audit_events` | Append-only, gap-free per context |
| `privacy_relay_outbox` | Platform event envelope (`cfmeet.privacy-relay.event`) |
| `privacy_relay_commands` | Idempotency (fingerprint + response) |
| `privacy_relay_enforcement_jobs` | Durable enforcement (PENDING → APPLIED), retried with backoff min(2ⁿ, 300) s |

No column holds a telephone number, e-mail address, SIP URI or other contact value.

## Configuration

See [`.env.example`](.env.example). Required: `DATABASE_URL`, `CF_RUNTIME_ENVIRONMENT`,
`CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`,
`CF_SERVICE_TRUST_BUNDLE_FILE`. Optional: `PRIVACY_RELAY_BIND_ADDRESS` (default
`127.0.0.1:8091`), `PRIVACY_RELAY_ENABLED` (default `false`),
`PRIVACY_RELAY_WORKER_INTERVAL_MS` (default 1000), `PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS`,
`CALLING_INTERNAL_BASE_URL` (enforcement delivery; without it jobs stay PENDING),
`CF_SERVICE_TOKEN_TTL_SECONDS`, `CF_INTERNAL_HTTP_TIMEOUT_MS`, `LOG_FORMAT`, `RUST_LOG`.
Invalid configuration stops the service at startup (exit 2); it also refuses to start
with pending migrations.

## Running locally (no Docker required)

```bash
cp services/privacy-relay/.env.example services/privacy-relay/.env   # then fill it in
make privacy-relay-migrate
make privacy-relay
```

Calling reaches it through `CF_PRIVACY_RELAY_BASE_URL`; the two trust bundles must grant
Calling the five `communication*` / `relay-session*` permissions (receiver Privacy Relay)
and Privacy Relay `communication-authority.enforce` (receiver Calling).

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make privacy-relay-test
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make privacy-relay-integration
```

Each test gets its own database cloned from a migrated template; without
`TEST_DATABASE_URL` the database suites fail unless `CFMEET_SKIP_DB_TESTS=1` is set.
Suites: `contexts`, `reassignment`, `lifecycle`, `expiry`, `sessions`, `authorization`,
`idempotency` (concurrent replays), `isolation` (tenant / application / environment),
`audit` (gap-free audit + outbox), `enforcement` (durable jobs, retry, a real HTTP
Calling mock), `internal_auth`, `migrations`, `contracts`, `contact_safety` and
`architecture`. `make privacy-relay-integration` runs Calling's
`privacy_relay_e2e` against the real binary.

CI: [`.github/workflows/privacy-relay.yml`](../../.github/workflows/privacy-relay.yml).

## Container image

```bash
# from the repository root: the image also needs packages/service-auth
docker build -f services/privacy-relay/Dockerfile -t cfmeet-privacy-relay .
```

The image runs as a non-root user and contains `privacy-relay` and
`privacy-relay-migration`. All configuration is supplied at runtime.
