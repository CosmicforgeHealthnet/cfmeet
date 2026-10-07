# CF Meet Calling service

`services/calling/` is the Rust/Axum service for the CF Meet **Calling** domain. It is a
peer of `services/meeting-service/` and is independently buildable and deployable: it
shares no source, models, migrations or database with the Meeting Service.

## What is implemented

**Part B §7–§14 (platform trust foundation) and Part C §15–§19 (Calling core).** The
service is the canonical authority for the **logical Call**: a CF Meet object with a CF
Meet-generated UUID that stays the same whatever later executes it (application RTC,
SIP, the public network, a relay, enterprise infrastructure, several legs or attempts).
Provider room / dialog / carrier / request identifiers are never Call identities.

| Area | Status |
|---|---|
| §7–§10 Platform consumer model, first-party / external consumers, API credential security, `PlatformAuthContext` vs `ResolvedPrincipal` | Implemented |
| §11 Internal service authentication (`packages/service-auth`, `/internal/v1/*`, delegated context, internal client) | Implemented (no internal business routes yet) |
| §12 Contract authority: `packages/contracts/calling/**` describes exactly what is implemented; conformance tests | Implemented |
| §13 Data ownership: own database only | Enforced (`architecture_boundaries`, `make service-boundaries`) |
| §14 Runtime direction: events only through the platform event envelope | Implemented for call events (transactional outbox; no publisher yet) |
| **§15** Calling service responsibility: canonical logical Call, provider-independent, HTTP-independent domain service | **Implemented** (`src/calls/`) |
| **§16** Required entities: Call, Call Participant, Call Session (+ connection intervals), Call Leg, Call Attempt, Call Route, Call Authorization, Call Event (+ outbox), Call Quality Record, Call Usage Record | **Implemented** (persistence, ownership, states, domain primitives) |
| **§17** Canonical state machine: the 17 states, transition graph, terminal states, optimistic versioning | **Implemented** (Rust + database defence in depth) |
| **§18** Transition rules: actions not client-set states, stale-command protection, no terminal resurrection, AUTHORIZED needs an APPROVED decision, paid public-network guard, CONNECTED / ENDED evidence, normalized failure categories | **Implemented** |
| **§19** Participant / session model: principal-scoped participants, server media identities, multi-device sessions, explicit reconnect, interval-based participation time | **Implemented** |
| Concurrent-call limit (Part B foundation) | **Enforced** at Call creation (race-safe) |
| Read API: `GET /v1/calls`, `GET /v1/calls/{call_id}` (`calls.read`) | **Implemented** |
| Telecom-spend limits | Resolved; enforced by future telephony operations |

**Deferred (later sections / services):** multi-device ringing and answer arbitration
(§20), route / attempt orchestration and the routing engine (§21), app-to-app voice and
video calling, the RTC provider contract and LiveKit call rooms (§22–§24), provider
execution of cancel / end / hold / transfer (§25), public call creation and control
APIs, realtime signaling and push ringing, webhooks and event delivery (the outbox has
no publisher yet), Privacy Relay, Contact Vault, Telephony (SIP / public network,
carrier adapters, provider callbacks and reconciliation), enterprise calling and
billing.

HTTP surface: `GET /health`, `GET /v1/calls`, `GET /v1/calls/{call_id}` (contract:
[`packages/contracts/calling/http/v1.openapi.json`](../../packages/contracts/calling/http/v1.openapi.json)).
The customer authentication middleware and `PlatformAuthContext` extractor, and the
internal service authentication middleware and `InternalCaller` extractor, are in place.
Every path under `/internal/v1/` — including unknown ones — requires a valid service
token and answers `401 INTERNAL_AUTHENTICATION_FAILED` otherwise; customer API
credentials are never accepted there, and service tokens are never accepted on customer
routes.

## Calling core (Part C §15–§19)

ADR: [0006 — Canonical Call aggregate and state machine](../../docs/decisions/0006-canonical-call-aggregate-and-state-machine.md).

### Entities

```text
Call (aggregate root: kind, state, version, first-entry timestamps, terminal reason / failure category)
├── CallParticipant      a trusted principal of the Call's scope (one per principal per Call)
│   └── CallSession      one device / network participation; server-generated media identity
│       └── interval     one connected period (interval 1, 2, … = reconnects)
├── CallRoute            provider-independent routing decision (class, state, policy version, snapshot)
├── CallLeg              canonical Calling-side leg (direction, neutral kind, normalized state, timing,
│                        opaque execution_binding_ref UUID only)
├── CallAttempt          gap-free per-call number, previous-attempt link, explicit UNKNOWN outcome
├── CallAuthorization    APPROVED / DENIED decision evidence; exactly one effective per Call
├── CallEvent            immutable, gap-free per-call sequence (+ one outbox row each)
├── CallQualityRecord    bounded numeric, provider-independent metrics
└── CallUsageRecord      idempotent consumption facts with set-once finalization (not billing)
```

Every row carries `tenant_id / application_id / environment_id` and references its parent
through composite foreign keys, so no child can point at a Call (or principal, session,
leg, route, credential) of another scope; ownership columns are immutable.
`communication_context_ref` / `privacy_relay_ref` / `execution_binding_ref` are opaque
UUIDs of records owned by other services — never foreign keys, never provider ids or
telephone numbers.

### State machine

States: `REQUESTED AUTHORIZED RESOLVING ROUTING DIALING RINGING ANSWERED CONNECTED HELD
TRANSFERRING ENDING` and terminal `ENDED DECLINED BUSY NO_ANSWER FAILED CANCELLED`.

| From | To |
|---|---|
| REQUESTED | AUTHORIZED, FAILED, CANCELLED |
| AUTHORIZED | RESOLVING, FAILED, CANCELLED |
| RESOLVING | ROUTING, FAILED, CANCELLED |
| ROUTING | DIALING, FAILED, CANCELLED |
| DIALING | RINGING, ANSWERED, DECLINED, BUSY, NO_ANSWER, FAILED, CANCELLED |
| RINGING | ANSWERED, DECLINED, BUSY, NO_ANSWER, FAILED, CANCELLED |
| ANSWERED | CONNECTED, ENDING, FAILED, CANCELLED |
| CONNECTED | HELD, TRANSFERRING, ENDING, FAILED |
| HELD | CONNECTED, TRANSFERRING, ENDING, FAILED |
| TRANSFERRING | CONNECTED, HELD, ENDING, FAILED |
| ENDING | ENDED, FAILED |
| terminal | — |

* **Actions, not states.** `CallService::transition(cmd, call_id, expected_version,
  action)` takes an action (`Authorize`, `DenyByPolicy`, `BeginResolution`,
  `BeginRouting`, `BeginDialing`, `MarkRinging`, `Accept`, `ConfirmConnected`, `Hold`,
  `Resume`, `BeginTransfer`, `CompleteTransfer`, `RequestEnd`, `ConfirmEnded`,
  `Decline`, `MarkBusy`, `MarkNoAnswer`, `Fail`, `Cancel`); the target state is derived
  by the server. No API sets a state.
* **Optimistic versioning.** Version 1 at creation, +1 exactly once per transition; a
  command whose `expected_version` is stale gets `STALE_CALL_VERSION` and changes nothing.
* **Evidence.** AUTHORIZED needs the Call's effective APPROVED authorization; a DENIED
  decision can only lead to FAILED / `POLICY` (`DenyByPolicy` — there is no canonical
  DENIED state). Every entry into CONNECTED needs positive evidence of a bidirectional
  media path (`RtcMediaPathEstablished`: connected sessions of two participants;
  `NetworkLegEstablished`: two connected legs; `MixedBridgeEstablished`: a connected
  session and a connected leg) — ANSWERED never implies CONNECTED. ENDING → ENDED needs
  `CleanupConfirmed`, verified `AllParticipationEnded`, or the `ReconciliationTimeout`
  disposition (ENDING for at least 5 minutes); the evidence is the Call's
  `terminal_reason` and is recorded in the event.
* **Terminal is final.** Rust refuses every action on a terminal Call; the database
  refuses any write that changes a terminal state, skips the graph, or changes the
  version without a state change. Late lifecycle, session and leg commands cannot
  resurrect a Call. Terminal transitions close open connection intervals
  (`CALL_TERMINATED`) and end sessions and participants.
* **Failure categories** are neutral: `POLICY TARGET_UNAVAILABLE ROUTING TIMEOUT MEDIA
  NETWORK EXECUTION INTERNAL UNKNOWN`. Provider error codes never enter the call domain.
* **Paid public-network guard.** `require_public_network_authorization(scope, call)`
  returns a `PublicNetworkAuthorization` proof only for an authorized, executable
  (not ENDING / terminal) Call whose effective decision is APPROVED in a live-telecom
  environment. It executes nothing; future execution commands require the proof.

### Participants, sessions and participation time

* A participant is a logical identity — a `ResolvedPrincipal` of the caller's own scope
  (foreign principals are reported as not found; the database pins principal scope and
  kind snapshot). Exactly one participant per principal per Call: adding it again is
  idempotent; concurrent additions cannot duplicate it. Display names are bounded,
  normalized presentation metadata (no control / bidi characters, no telephone-number
  content) and never identify or authorize anyone.
* A session is one device / network participation. Its media identity is generated by
  the server (`cfmid_` + 256 random bits) and never accepted as input. A participant may
  hold several sessions at once.
* Reconnect is explicit: `reconnect_session` renews the same session with connection
  interval n + 1; `replace_session` ends a session whose device / connection identity
  changed and creates a new session for the same participant. Neither changes the Call
  or the participant.
* Participation time comes from intervals: per session the sum, per participant the
  **union** across its sessions (concurrent devices are not double counted), open
  intervals up to the query instant.

### Events and the transactional outbox

Every mutation appends immutable `CallEvent`s (`CALL_REQUESTED`, `CALL_STATE_CHANGED`,
`CALL_AUTHORIZATION_RECORDED`, participant / session / route / leg / attempt / quality /
usage facts) and one `calling_outbox_events` row per event, in the same transaction as
the mutation. The outbox row is a `cfmeet.platform.event-envelope` (event type
`calling.<event-type>`, e.g. `calling.call-state-changed`) whose payload is a
[`cfmeet.calling.call-lifecycle-event`](../../packages/contracts/calling/events/call-lifecycle/v1.schema.json).
The database refuses `UPDATE` / `DELETE` / `TRUNCATE` of events, sequence gaps, events
that misdescribe the Call, a new Call version without its state event and an event
without its outbox row. Nothing is published yet; a future publisher consumes `PENDING`
rows.

### Concurrent-call limit

Creating a Call takes a transaction-scoped advisory lock per application and counts
non-terminal Calls against the effective per-credential limit and, when configured, the
application-wide limit. Terminal Calls stop counting.

### Read API

`GET /v1/calls` (keyset pagination by `createdAt DESC, callId DESC`, `limit` 1–100,
opaque `cursor`, filters `state`, `createdAfter`, `createdBefore`,
`initiatorPrincipalId`; unknown or repeated parameters → 400) and
`GET /v1/calls/{call_id}` (participants, session summaries, durations). Both require
`calls.read`; scope comes only from the credential; a foreign or malformed id answers
exactly like a missing one (404). Responses are `Cache-Control: no-store` and never
contain media identities, execution bindings, provider data, credentials, principal
references or telephone numbers.

## Internal service authentication

```text
customer request ─► PlatformAuthContext ─► delegated_context() ─► DelegatedPlatformContext
                                                                     │ signed into
peer service ◄─ X-CF-Service-Authorization: Bearer <EdDSA JWS> ◄──────┘ the service token
                X-CF-Request-Id (new per hop) / X-CF-Correlation-Id (stable per operation)
```

* Calling signs as `cfmeet.calling` with its own key (`CF_SERVICE_AUTH_KEY_ID`,
  `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`); tokens are bound to exactly one audience and one
  runtime environment, live 120 s by default (≤ 300 s) and carry no permissions.
* Inbound, Calling verifies against its trust bundle (`CF_SERVICE_TRUST_BUNDLE_FILE`):
  pinned algorithm and token type, known issuer and `kid`, exact audience, runtime
  environment, lifetime, and header ids equal to the signed ids. Permissions come only
  from Calling's own bundle (`InternalCaller::require_permission`). Calling grants
  `operational-callback.deliver` at most, and it is never granted
  `protected-contact.resolve-for-execution` by any receiver.
* Outbound helpers: `internal_auth::delegation::{delegated_context,
  outbound_for_customer, outbound_next_hop}` and `ServiceAuthRuntime::client()` (HTTPS in
  staging / production, no redirects, no proxies, `/internal/v1/` paths only).

ADR: [0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md).

## Model

```text
Tenant (FIRST_PARTY | EXTERNAL; ACTIVE | SUSPENDED)
└── Application (ACTIVE | SUSPENDED) ── allowed scopes ── limit policy
    └── Environment (SANDBOX | TEST | PRODUCTION; ACTIVE | SUSPENDED; live_telecom_allowed)
        ├── Service account (ACTIVE | SUSPENDED | REVOKED)
        │   └── API credential (ACTIVE | REVOKED; validity window; rotation links) ── granted scopes ── limit policy
        └── Principal (7 categories, opaque reference; ACTIVE | SUSPENDED | DEACTIVATED)
```

* Every child row carries its whole ownership chain and references its parent with a
  composite foreign key on `(…ownership…, id)`, so a row can never point at a parent of
  another tenant / application / environment. Ownership columns are immutable (trigger).
* An application has exactly one environment of each kind. SANDBOX and TEST can never
  have `live_telecom_allowed = true` (validated and `CHECK`-enforced); PRODUCTION starts
  with it disabled.
* FIRST_PARTY is a trust classification only. CosmicForge products and external
  customers are ordinary applications; privileges exist only as explicit scopes.

## API credentials

Format: `cfmk_<sbx|test|live>_<public id: 24 hex>_<secret: 64 hex>`.

* The secret is 32 bytes from the OS CSPRNG. The environment tag is convenience only; it
  grants nothing and is bound into the verifier, so an edited tag never verifies.
* The database stores `HMAC-SHA-256(API_CREDENTIAL_PEPPER, context ‖ tag ‖ public id ‖
  secret)` plus algorithm and pepper version — never the secret. Verification is
  constant time. The raw credential is returned once, at creation or rotation.
* Authentication (`Authorization: Bearer <credential>`) loads the credential's full
  authority chain in one query and requires: verifier match, credential ACTIVE, not
  revoked, inside its validity window and rotation overlap, and service account,
  environment, application and tenant all ACTIVE. No caches: suspension and revocation
  apply to the next request. Every failure returns the same `AUTHENTICATION_FAILED` 401.
* Scopes come from a server-side registry, must be allowed for the application
  (foreign-key enforced) and are loaded from the database at authentication; requests
  cannot add scopes. `PlatformAuthContext::require_scope` is the check for handlers.
* Rotation issues an independent successor for the same service account (scopes and
  credential limits preserved) and limits the predecessor to `min(original expiry,
  now + overlap)`. Revocation is immediate and idempotent.

## Principals

`PlatformAuthContext` says which application / service account is calling.
`PrincipalResolver::resolve(ctx, reference)` resolves *who takes part* — only inside the
context's tenant, application and environment; a principal elsewhere is reported exactly
like a nonexistent one. Authentication never creates or implies a principal; a service
account is a principal only if explicitly registered as a linked SERVICE_ACCOUNT
principal.

Categories: `NATIVE_CF_USER`, `EXTERNAL_APPLICATION_PRINCIPAL`, `ENTERPRISE_MEMBER`,
`SERVICE_ACCOUNT`, `TELEPHONE_ENDPOINT`, `TEMPORARY_EXTERNAL_PARTICIPANT`,
`BOT_OR_SERVICE`. References are opaque; no plaintext contact data is stored —
E.164-style and `tel:`/`sip:` values are refused, and TELEPHONE_ENDPOINT references must
be opaque UUID handles (for the future Contact Vault).

## Limits

Application- and credential-level limits (requests per minute, concurrent
communications, daily / monthly telecom spend in minor units of the tenant's billing
currency) are set by operators only. The effective policy takes the stricter value where
both levels configure a limit, otherwise the configured one, otherwise the platform
default. Telecom spend defaults to zero and is always zero where live telecom is not
allowed. The request-rate limit is enforced (shared PostgreSQL one-minute windows, per
credential and per application) and the concurrent-communication limit is enforced when a
Call is created (see *Concurrent-call limit*); telecom spend is resolved for the future
telephony operations that consume it.

## Configuration

See [`.env.example`](.env.example). Required: `DATABASE_URL`, `API_CREDENTIAL_PEPPER`
(≥ 32 random bytes, e.g. `openssl rand -base64 48`; placeholder values are rejected and
there is no fallback), `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`,
`CF_SERVICE_AUTH_PRIVATE_KEY_FILE` and `CF_SERVICE_TRUST_BUNDLE_FILE`. The service exits
at startup if its identity, key, trust bundle or runtime environment is missing or
inconsistent. The Calling service owns its own database.

## Running locally (no Docker required)

```bash
cp .env.example .env              # set DATABASE_URL and API_CREDENTIAL_PEPPER
# A local signing key (outside the repository) and a trust bundle with no peers yet:
mkdir -p ~/.cfmeet && cargo run -q --manifest-path ../../packages/service-auth/Cargo.toml \
  --bin cf-service-keygen -- generate --kid calling-local-1 --out ~/.cfmeet/calling-local-1.pem
echo '{"bundle_version":1,"receiver":"cfmeet.calling","runtime_environment":"local","trusted_issuers":[]}' \
  > ~/.cfmeet/calling-trust-bundle.json
# then set CF_SERVICE_AUTH_PRIVATE_KEY_FILE / CF_SERVICE_TRUST_BUNDLE_FILE in .env
cargo run -p calling-migration -- up
cargo run --bin calling
curl http://127.0.0.1:8090/health
```

From the repository root: `make calling-migrate`, `make calling`.

## Administration

`platform_admin` uses the same domain services as future administration APIs. There are
no public credential-management endpoints.

```bash
A="cargo run -q --bin platform_admin --"
$A tenant create --slug acme --name "Acme" --classification external
$A application create --tenant acme --slug dispatch --name "Dispatch"
$A environment create --tenant acme --application dispatch --environment production
$A service-account create --tenant acme --application dispatch --environment production --name backend
$A scope grant --tenant acme --application dispatch --scope principals.read
$A credential create --tenant acme --application dispatch --environment production \
   --service-account backend --scope principals.read      # prints the secret ONCE
$A credential rotate --credential <public id> --overlap-minutes 60
$A credential revoke --credential <public id>
$A principal register --tenant acme --application dispatch --environment production \
   --kind external-application-principal --reference user-123
$A limits set-credential --credential <public id> --requests-per-minute 120
$A limits effective --credential <public id>
```

`$A --help` lists every command (suspend / reactivate for tenants, applications,
environments, service accounts and principals; `environment set-live-telecom`; scope
registry; credential show / list). Output is JSON on stdout; audit lines go to stderr.

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --workspace --no-fail-fast
```

Each test clones its own database from a migrated template (no Docker needed). Without
`TEST_DATABASE_URL` the database suites fail, unless `CFMEET_SKIP_DB_TESTS=1` is set
(explicit SKIPPED lines). Suites: `migrations`, `tenant_isolation`,
`first_party_external`, `credential_storage`, `authentication`, `environment_isolation`,
`scopes`, `rotation`, `secret_leakage`, `principals`, `limits`, `admin_cli`,
`architecture_boundaries`, `internal_auth` (service-token authentication on
`/internal/v1/*`), `outbound` (delegated context and the internal HTTP client over
loopback HTTP), `http_contract` (conformance with `packages/contracts`, including call
responses, call vocabularies and every outbox envelope / lifecycle payload) and the
Calling core suites `call_state_machine`, `call_concurrency` (repeated races),
`call_authorization`, `call_participation`, `call_evidence` (events, outbox, injected
failures), `call_records` and `call_api`.

CI: [`.github/workflows/calling.yml`](../../.github/workflows/calling.yml).

## Container image

```bash
# from the repository root: the image also needs packages/service-auth
docker build -f services/calling/Dockerfile -t cfmeet-calling .
```

The image runs as a non-root user and contains `calling`, `calling-migration` and
`platform_admin`. All configuration is supplied at runtime.

## Decisions

* [ADR 0003 — Calling platform consumer, credential and principal trust model](../../docs/decisions/0003-calling-platform-trust-model.md)
* [ADR 0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md)
* [ADR 0005 — Contract authority and runtime boundaries](../../docs/decisions/0005-contract-authority-and-runtime-boundaries.md)
* [ADR 0006 — Canonical Call aggregate and state machine](../../docs/decisions/0006-canonical-call-aggregate-and-state-machine.md)
* [Service data ownership](../../docs/architecture/service-data-ownership.md)
