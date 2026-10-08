# CF Meet Privacy Relay service

`services/privacy-relay/` is the Rust/Axum peer service that is authoritative for
**Communication Contexts** and **Privacy Relay Sessions** (Part D §26–§30,
[ADR 0010](../../docs/decisions/0010-communication-context-and-privacy-relay-session-architecture.md)),
the **Contact Vault** (§31–§33,
[ADR 0011](../../docs/decisions/0011-contact-vault-cryptography-and-resolution.md)) and the
**temporary relay-number pool** with inbound mapping (§34–§35,
[ADR 0012](../../docs/decisions/0012-temporary-relay-number-assignment-and-cooldown.md)).
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
| §31 Contact Vault: canonical E.164 (`phonenumber`), masked display, AES-256-GCM with random nonce and scope-bound AAD, external keyrings (`CONTACT_VAULT_KEYRING_FILE`), key versions + re-encryption, delegated access (Contact Vault access token minted by Calling), access audit | **Implemented** |
| §32 ContactReference equality: keyed, scoped HMAC-SHA-256 fingerprints (`CONTACT_FINGERPRINT_KEYRING_FILE`, a different secret), deduplication, fingerprint rotation | **Implemented** |
| §33 Verification (trusted `privacy.contacts.verify` assertions, read-time expiry, BLOCKED / INVALID), public-network authority gate, Contact Resolution Grants (Calling) and Telephony-only plaintext resolution | **Implemented** |
| §34 Temporary relay-number inventory (operator CLI), deterministic `SKIP LOCKED` assignment, one holder per number, containment in the session, release / cooldown / drain workers | **Implemented** |
| §35 Inbound relay resolution and inbound caller protection (Telephony only), generation / time / cooldown protection against delayed callbacks | **Implemented** |
| §36 Privacy-safe call evidence (composed by **Calling** from its own records; Privacy Relay proves session ownership) | **Implemented (in Calling)** |
| Feature flags `PRIVACY_RELAY_ENABLED`, `CONTACT_VAULT_ENABLED`, `RELAY_NUMBER_POOL_ENABLED` (all default `false`; restrictive operations always work; disabling the vault or the pool never disables app-only Privacy Relay or ordinary calling) | **Implemented** |

**Executed elsewhere (Part E):** PUBLIC_RELAY / CALLBACK_RELAY decisions, resolution
grants, relay assignments and inbound resolutions are authority that **Calling** (route,
spend, canonical Call) and **Telephony** (`services/telephony`, execution) act on;
Privacy Relay never places, receives or bridges a public call itself, and plaintext
leaves the vault only to Telephony's `resolve-for-execution`. Real lawful carrier
certification is pending (ADR 0016). The permanent Numbering product is not implemented.

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
| `POST /internal/v1/contacts/{ref}/resolution-grants` | `protected-contact.grant-for-execution` (Calling) |
| `POST /internal/v1/contacts/{ref}/resolve-for-execution` | `protected-contact.resolve-for-execution` (Telephony only) |
| `POST /internal/v1/sessions/{id}/relay-number-assignment`, `GET` it, `POST …/release` | `relay-number.assign` (Calling) |
| `POST /internal/v1/relay-routing/resolve-inbound` | `relay-number.resolve-inbound` (Telephony only, no delegated context) |
| `POST /internal/v1/relay-routing/protect-inbound-caller` | `protected-contact.protect-inbound` (Telephony only, no delegated context) |
| `GET /health` | none |

Contact Vault customer routes are served **by Privacy Relay itself** with a Contact
Vault access token (`X-CF-Contact-Authorization`, typ `cf-contact-access+jwt`) that
Calling mints at `POST /v1/privacy-relay/contact-access-grants`: `POST /v1/privacy-relay/contacts`,
`GET|PATCH …/contacts/{ref}`, `POST …/contacts/{ref}/disable`
(`privacy.contacts.read / write`) and `POST …/contacts/{ref}/verification`
(`privacy.contacts.verify`). They are the only routes that accept a telephone number
(`telephoneNumber`); responses carry the ContactReference and a masked display only.

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

| `protected_contacts` | Ciphertext + nonce + key version, masked display, country, verification state (the only private value, encrypted) |
| `contact_fingerprints` | Scoped keyed fingerprints per key version (equality) |
| `contact_access_audit` | Every sensitive Contact Vault attempt, append-only, without the number |
| `contact_verification_events` | Append-only verification history (trust source, opaque evidence reference) |
| `contact_resolution_grants` | Short-lived single-execution grants (secret hash only) |
| `relay_number_inventory` / `relay_number_events` | Platform-owned public relay numbers (masked in operator output) and their ledger |
| `relay_number_assignments` | One holder per number; ACTIVE → COOLDOWN → RELEASED / REVOKED / EXPIRED |
| `relay_inbound_resolutions` | Every inbound resolution attempt, without any number |

No other column holds a private telephone number, e-mail address, SIP URI or contact
value. **Backups of this database are sensitive**: they hold encrypted contact data and
fingerprints (useless without the keyrings, which must never be stored with them).

### Operations

```bash
# Contact Vault keyrings (written 0600, never printed) and rotation
privacy-relay-admin contact-vault generate-keyring --purpose encryption --version ck-2026-10 --out /run/secrets/contact-keyring.json
privacy-relay-admin contact-vault generate-keyring --purpose fingerprint --version fk-2026-10 --out /run/secrets/fingerprint-keyring.json
privacy-relay-admin contact-vault add-key|activate-key --purpose P --version V --file F
privacy-relay-admin contact-vault rotation-status | reencrypt | backfill-fingerprints | prune-fingerprints --version V
# Temporary relay numbers (public, platform-owned; the binding is an opaque reference)
privacy-relay-admin relay-number add --number +44... --capabilities VOICE_INBOUND,VOICE_OUTBOUND --binding REF [--available true]
privacy-relay-admin relay-number activate|enable|drain|disable --id UUID
privacy-relay-admin relay-number list
```

## Configuration

See [`.env.example`](.env.example). Required: `DATABASE_URL`, `CF_RUNTIME_ENVIRONMENT`,
`CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`,
`CF_SERVICE_TRUST_BUNDLE_FILE`. Optional: `PRIVACY_RELAY_BIND_ADDRESS` (default
`127.0.0.1:8091`), `PRIVACY_RELAY_ENABLED` (default `false`),
`PRIVACY_RELAY_WORKER_INTERVAL_MS` (default 1000), `PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS`,
`CALLING_INTERNAL_BASE_URL` (enforcement delivery; without it jobs stay PENDING),
`CF_SERVICE_TOKEN_TTL_SECONDS`, `CF_INTERNAL_HTTP_TIMEOUT_MS`, `LOG_FORMAT`, `RUST_LOG`,
`CONTACT_VAULT_ENABLED` with `CONTACT_VAULT_KEYRING_FILE`, `CONTACT_FINGERPRINT_KEYRING_FILE`
and `CONTACT_DELEGATION_TRUST_BUNDLE_FILE` (all three required when enabled — no plaintext
fallback, no generated key), `RELAY_NUMBER_POOL_ENABLED`, `RELAY_NUMBER_COOLDOWN_SECONDS`
(3600–7776000, default 259200 = 72 h).
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
Calling mock), `internal_auth`, `migrations`, `contracts`, `contact_safety`,
`architecture`, `contact_vault` (crypto, IDOR, audit, key rotation), `contact_config`,
`contact_equality` (dedup races, fingerprint rotation), `contact_resolution`
(verification, grants, Telephony-only resolution), `contact_logging` (TRACE log canary),
`relay_numbers` (allocation races, cooldown, drain, containment, kill switch),
`relay_inbound` (inbound mapping, delayed callbacks, inbound caller protection) and
`part_d_certification` (flows, cross-tenant, failure injection, rotations). `make privacy-relay-integration` runs Calling's
`privacy_relay_e2e` against the real binary.

CI: [`.github/workflows/privacy-relay.yml`](../../.github/workflows/privacy-relay.yml).

## Container image

```bash
# from the repository root: the image also needs packages/service-auth
docker build -f services/privacy-relay/Dockerfile -t cfmeet-privacy-relay .
```

The image runs as a non-root user and contains `privacy-relay`,
`privacy-relay-migration` and `privacy-relay-admin`. All configuration — including the
keyrings and trust bundles — is supplied at runtime; no key is baked into the image.
