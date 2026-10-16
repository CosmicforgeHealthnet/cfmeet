# CF Meet Numbering

The canonical authority for public numbers in CF Meet (Part IV Workstreams C–D,
[ADR 0033](../../docs/decisions/0033-numbering-service-foundation-and-number-domain.md)).
It owns, in its own PostgreSQL database:

* **Number Resources** — the CF Meet id is the canonical identity of a number; E.164,
  country and calling code; classification only from evidence; inventory origin
  (`LIVE_PROVIDER`, `MANUAL_IMPORT`, `SYNTHETIC_TEST`) and observation freshness;
  compliance / risk references; routing and binding revisions; commercial facts;
* the **14-state lifecycle** (`DISCOVERED … UNKNOWN_PROVIDER_STATE`) behind one
  centralized transition policy, enforced in Rust *and* by database triggers, with an
  append-only lifecycle audit. `UNKNOWN_PROVIDER_STATE` records the uncertain operation
  and is left only by reconciliation with evidence;
* **provider bindings** with history (at most one active binding per capability;
  provider ids and references never leave Numbering — consumers see opaque binding refs);
* **capabilities** with provenance and effective dates (VERIFIED only with evidence, never
  for synthetic inventory; never inferred; downgrades refused while an open assignment
  relies on them unless the impact is acknowledged);
* **inventory observations** (append-only; an observation is never an allocation) and
  **reservations** (scoped to the trusted context, one current reservation per number,
  PENDING until a provider hold is confirmed; expiry never implies a provider release);
* **DCN assignments** (`REQUESTED … CLOSED`; one open assignment per number; ACTIVE only
  through eligibility, a held reservation, a pinned routing profile and provisioning),
  ownership classes (an organization-owned number routed to an employee stays
  organizational), versioned **routing profiles** with opaque targets, and append-only
  **assignment history** (reassignment is a new record);
* the reliability foundation every later workflow reuses: a **transactional outbox**
  (platform envelope, leased multi-instance publication, per-aggregate order, bounded
  backoff, dead-letter), a **leased job queue** (restart recovery by retry safety;
  provider-affecting work interrupted mid-flight is parked `UNKNOWN_OUTCOME`, never
  blindly repeated) and **scoped idempotency keys**.

It does NOT own logical calls, SIP / PSTN execution, relay sessions or SMS transport
(Calling, Telephony, Privacy Relay, later SMS), and it never reads another service's
database. Sandbox / test environments use synthetic test inventory only; production never
does (domain and database).

## HTTP surface

Contract `cfmeet.numbering.internal-api` (messages `cfmeet.numbering.common`, events
`cfmeet.numbering.event`):

| Route | Authentication | Purpose |
|---|---|---|
| `GET /health`, `GET /ready` | none | liveness; readiness with outbox / dead-letter / unknown-outcome counts |
| `GET /v1/numbers/assignments[/{id}[/history]]` | customer credential → Platform Access, `numbers.read` | the caller's own DCN assignments |
| `GET /v1/numbers/reservations/{id}` | customer credential → Platform Access, `numbers.read` | the caller's own reservation |
| `GET /internal/v1/number-resources/{id}` | service token, `number-resource.read` (Calling, Telephony) | canonical, provider-independent Number Resource |

Customer credentials are verified by **Platform Access** on every request; the tenant /
application / environment come only from its authority chain (a request-supplied id is
never authority). Platform Access unreachable → 503 (fail closed). Full public search /
provisioning / assignment-mutation APIs, KYC, SMS, portability and real carrier adapters
are later workstreams; no telecom carrier is contacted.

## Running locally (no Docker required)

```bash
cp .env.example .env     # DATABASE_URL, CF_SERVICE_*, CF_PLATFORM_ACCESS_BASE_URL
make numbering-migrate
make numbering           # listens on 127.0.0.1:8095
curl http://127.0.0.1:8095/ready
```

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make numbering-test
```

`make numbering-test` builds Platform Access first: the customer-authentication tests
start the real Platform Access binary with its own database.

| Suite | Covers |
|---|---|
| `service` | probes, trusted / untrusted / unpermitted internal callers, customer auth through Platform Access, scope enforcement, tenant isolation, correlation / causation, failure handling, OpenAPI = routes |
| `startup` | the real binary: fail-closed configuration, pending migrations refused, serving once migrated |
| `migrations` | fresh / down / re-up, upgrade with data, exact table set, no cross-service FK, least-privilege runtime role |
| `outbox` | atomicity, immutability, ordering, backoff, dead-letter, concurrent publishers, lease takeover, envelope conformance |
| `workers` | lease exclusivity, retry / dead-letter, restart recovery, UNKNOWN_OUTCOME parking, idempotency keys |
| `number_resources` | D10.1–D10.7: identity, E.164, lifecycle policy (Rust = SQL), version conflicts, bindings, capabilities, inventory |
| `allocation` | D10.8–D10.17: reservation contention and expiry, ownership, exclusivity, isolation, history, routing revisions, premature activation, quarantine / uncertainty, rollback |
| `contracts` | Rust = contract = database vocabulary, error / rule codes, event types |
