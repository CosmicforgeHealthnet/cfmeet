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
* the **capability and country catalog** (Workstream E, [ADR 0034](../../docs/decisions/0034-numbering-capability-and-country-catalog.md)):
  versioned country / number-type entries (real and synthetic classes kept apart), one
  fail-closed **capability resolver** (supported / usable, normalized reasons,
  provenance, revisions), evidence expiry, revocation, stale-update and source-authority
  protection; voice / SMS never promise third-party (OTP, WhatsApp, banking) acceptance;
* the reliability foundation every later workflow reuses: a **transactional outbox**
  (platform envelope, leased multi-instance publication, per-aggregate order, bounded
  backoff, dead-letter), a **leased job queue** (restart recovery by retry safety;
  provider-affecting work interrupted mid-flight is parked `UNKNOWN_OUTCOME`, never
  blindly repeated) and **scoped idempotency keys**.

It does NOT own logical calls, SIP / PSTN execution, relay sessions or SMS transport
(Calling, Telephony, Privacy Relay, the SMS Gateway), and it never reads another service's
database. Sandbox / test environments use synthetic test inventory only; production never
does (domain and database).

## HTTP surface

Contract `cfmeet.numbering.internal-api` (messages `cfmeet.numbering.common`, events
`cfmeet.numbering.event`):

| Route | Authentication | Purpose |
|---|---|---|
| `GET /health`, `GET /ready` | none | liveness; readiness with outbox / dead-letter / unknown-outcome counts |
| `GET /openapi.json` | none | the served API description: exactly the checked-in contract `cfmeet.numbering.internal-api` |
| `GET /v1/numbers/countries`, `GET /v1/numbers/countries/{cc}/number-types` | customer, `numbers.search` | supported countries / number types of the caller's inventory class (H) |
| `GET /v1/numbers/search` | customer, `numbers.search` | provider-neutral inventory search: observations with freshness, capability provenance, KYC indicator; bounded pages, per-environment quota (H) |
| `POST /v1/numbers/reservations` | customer, `numbers.manage`, `Idempotency-Key` required | eligibility-bound reservation with a durable provider hold (202, PENDING → HELD) (H) |
| `GET /v1/numbers/reservations`, `POST /v1/numbers/reservations/{id}/cancel` | customer, `numbers.read` / `numbers.manage` | the caller's reservations; cancellation with an evidence-only provider release (H) |
| `POST /v1/numbers/acquisitions`, `GET /v1/numbers/acquisitions[/{id}]` | customer, `numbers.manage` (`Idempotency-Key`) / `numbers.read` | provision a held reservation (eligibility re-evaluated, hold read back, durable PROVISION command); status, served capabilities, route readiness (I) |
| `POST /v1/numbers/acquisitions/{id}/routes` | customer, `numbers.manage`, `Idempotency-Key` | provider voice / SMS route configuration on the current binding (I) |
| `POST /internal/v1/routing/inbound-resolutions` | service token, `number-route.resolve` (Telephony) | a received VOICE call resolved from canonical state with a revision-bound inbound grant (K); any other family is `FAMILY_NOT_SUPPORTED` |
| `POST /internal/v1/routing/sms-destination-resolutions` | service token, `number-sms-route.resolve` (SMS Gateway only) | the destination of a received SMS resolved from canonical state with a revision-bound INBOUND_SMS grant (Part IV M) |
| `POST /internal/v1/routing/caller-identity-grants`, `…/sms-sender-grants` | service token + delegated context, `number-caller-identity.authorize` (Calling) / `number-sms-sender.authorize` (SMS Gateway) | single-use outbound grants for the scope's own ACTIVE numbers (K) |
| `POST /internal/v1/routing/grant-verifications` | service token, `number-grant.verify` | the audience verifies / consumes a grant against current state (K) |
| `GET /internal/v1/routing/number-assignments/{numberResourceId}` | service token, `number-caller-identity.authorize`, delegated context | Calling's number-free view of the scope's current assignment and routed targets, for its caller-identity entitlement policy (L) |

Voice resolutions, current-assignment views and valid grant verifications carry the number's `inventoryClass` (`SYNTHETIC` / `REAL`, never inferred by the caller): Telephony and Calling run a SYNTHETIC number only over the SYNTHETIC_TEST interconnect of a local / test runtime and a REAL one only over public routes (ADR 0041 decision 7). A grant issued at an earlier routing revision is refused `STALE_ROUTING_REVISION` even while the assignment is unchanged (`routing_grants`).
| `GET /internal/v1/number-resources/{id}/routes`, `POST …/route-readiness` | service token, `number-route.acknowledge` (Telephony: voice, SMS Gateway: SMS) | current route configuration; the carrier's readiness acknowledgement (I) |
| `POST /v1/numbers/assignments` | customer, `numbers.assign`, `Idempotency-Key` required | assign an acquisition / held reservation to its legal owner (organization = authenticated tenant; principals resolved by Platform Access), eligibility re-evaluated, class-bounded routing targets (J) |
| `POST /v1/numbers/assignments/{id}/activate` | customer, `numbers.assign` | ACTIVE only when provisioned, eligible and every routed family READY (J) |
| `GET / POST /v1/numbers/assignments/{id}/routing`, `POST …/offboarding` | customer, `numbers.read` / `numbers.manage` | pinned routing revision and readiness; routing revisions; employee offboarding (J) |
| `POST /v1/numbers/assignments/{id}/transfer` | customer, `numbers.assign`, `Idempotency-Key` required | same-owner transfer (retarget or linked successor); cross-owner fails closed (J) |
| `GET / POST /v1/numbers/pools`, `GET /v1/numbers/pools/{id}`, `POST …/members`, `DELETE …/members/{assignmentId}` | customer, `numbers.read` / `numbers.manage` | organizational pools (J) |
| `GET /v1/admin/number-resources/{id}/assignment-history` | `numbers.admin` (first-party) + `X-CF-Operator` | every owner's assignment history of a number (J) |
| `GET /v1/numbers/assignments[/{id}[/history]]` | customer credential → Platform Access, `numbers.read` | the caller's own DCN assignments (filters, keyset pages) |
| `GET /v1/numbers/reservations/{id}` | customer credential → Platform Access, `numbers.read` | the caller's own reservation |
| `GET /internal/v1/number-resources/{id}` | service token, `number-resource.read` (Calling, Telephony) | canonical, provider-independent Number Resource |
| `GET /internal/v1/number-resources/{id}/capabilities` | service token, `number-resource.read` | resolved capabilities with provenance and revisions |
| `GET /v1/numbers/catalog/countries/{cc}` | customer, `numbers.search` / `numbers.read` | catalog of the caller's inventory class |
| `GET /v1/numbers/assignments/{id}/capabilities` | customer, `numbers.read` | resolved capabilities of the caller's own number |
| `POST /v1/admin/catalog/…`, `POST /v1/admin/number-resources/{id}/capabilities/{c}/revoke` | `numbers.admin` (first-party) + `X-CF-Operator` | catalog publication / withdrawal, capability revocation |
| `GET /v1/numbers/kyc/requirements/{cc}/{type}/{ownership}` | customer, `numbers.kyc.read` | KYC requirements in force (fail closed) |
| `POST /v1/numbers/kyc/cases`, `…/cases/{id}/evidence`, `…/submit`, `…/appeals` | customer, `numbers.kyc.manage`, `Idempotency-Key` | open a case, attach protected evidence, submit, appeal |
| `GET /v1/numbers/kyc/cases/{id}[/history]` | customer, `numbers.kyc.read` | case status, missing requirements, immutable history |
| `POST /v1/numbers/eligibility`, `GET /v1/numbers/eligibility/{id}` | customer, `numbers.kyc.read` | explainable eligibility decision (never reserves) |
| `POST /v1/admin/kyc/requirements`, `GET /v1/admin/kyc/cases/{id}`, `POST …/decisions`, `…/revoke`, `POST /v1/admin/kyc/evidence/{id}/access` | `numbers.admin` (first-party) + `X-CF-Operator` | requirement publication, manual review, revocation, audited evidence read |
| `GET /internal/v1/assignments/{id}/kyc-status` | service token, `number-resource.read` | whether an assignment's KYC basis is in force |
| `POST /provider-callbacks/v1/{providerId}` | provider signature (HMAC, replay window) | authenticated, de-duplicated provider events |
| `POST /v1/numbers/assignments/{id}/suspend`, `…/restore`, `GET …/suspension` | customer, `numbers.manage` / `numbers.read` | organization suspension (local-first: grants refused and revoked at once, provider route disable durable); restoration re-checks owner, eligibility, capability and provider state; operator / risk suspensions are lifted only by an operator (P) |
| `POST /v1/numbers/assignments/{id}/release`, `GET …/quarantine`, `GET /v1/numbers/releases/{id}` | customer, `numbers.manage` (`Idempotency-Key`) / `numbers.read` | release → routing removed → quarantine (`POOL_RETURN` or `PROVIDER_RELINQUISH`; scheduled releases; an unknown provider outcome never becomes reassignable) (Q) |
| `POST /v1/numbers/reassignments` | customer, `numbers.assign`, `Idempotency-Key` | a REASSIGNABLE number to a new owner: provider read-back, risk, eligibility, a new reservation, new route configuration; the previous owner's history stays theirs (Q) |
| `GET /v1/numbers/usage` | customer, `numbers.read` | the caller's own usage / provider-cost evidence (never customer prices) (S) |
| `GET /v1/numbers/risk-decisions/{id}`, `POST …/appeals` | customer, `numbers.read` / `numbers.manage` | explainable allocation / signal / spend decisions and appeals (T) |
| `POST / GET /v1/numbers/ports[/{id}]`, `POST …/evidence`, `…/submit`, `…/cancel` | customer, `numbers.manage` / `numbers.read` | provider-independent port-in / port-out requests; `NUMBERING_PORTING_ENABLED=false` by default, no live portability provider (U) |
| `POST /v1/admin/number-resources/{id}/suspend`, `…/restore`, `…/release`, `GET …/quarantine`, `POST /v1/admin/quarantines/{id}/review` | `numbers.admin` (first-party) + `X-CF-Operator` | operator lifecycle actions and quarantine review (P, Q) |
| `GET /v1/admin/number-resources/{id}/ownership-periods`, `POST …/ownership-verifications`, `POST …/lifecycle-evidence`, `GET …/usage`, `GET …/risk-decisions`, `POST /v1/admin/risk-decisions/{id}/appeal-decisions` | `numbers.admin` + `X-CF-Operator` (+ purpose / case reference for evidence) | ownership history, audited masked verification, lifecycle evidence, usage, risk and appeals (R, S, T) |
| `GET /v1/admin/controls`, `POST /v1/admin/controls/{control}` | `numbers.admin` + `X-CF-Operator` | emergency kill switches `DCN_INBOUND_ROUTING`, `DCN_CALLER_IDENTITY`, `DCN_SMS_SENDER`, `NUMBER_PROVISIONING`, `NUMBER_PORTING`, `RELAY_LEASES` (append-only audit) (W) |
| `POST /v1/admin/reconciliation/lifecycle`, `POST /v1/admin/relay-numbers/{ref}/retire` | `numbers.admin` + `X-CF-Operator` | lifecycle drift reconciliation (fail closed); relay-number retirement (V, Part P) |
| `POST /internal/v1/number-usage` | service token, `number-usage.report` (Telephony, SMS Gateway) | idempotent carrier usage / cost reports (S) |
| `POST /internal/v1/number-risk-signals` | service token, `number-risk.report` (Calling, Telephony, SMS Gateway) | abuse / fraud signals; may suspend with source `RISK` (T) |
| `POST /internal/v1/relay-inventory/imports`, `POST /internal/v1/relay-leases`, `POST …/{id}/release`, `GET /internal/v1/relay-numbers/{ref}/resolution` | service token, `number-relay.lease` (Privacy Relay only) | relay numbers as `PRIVACY_RELAY`-class Number Resources: idempotent import, temporary leases, resolution (Part P, ADR 0044) |

Customer credentials are verified by **Platform Access** on every request; the tenant /
application / environment come only from its authority chain (a request-supplied id is
never authority). Platform Access unreachable → 503 (fail closed). The customer
`/v1/numbers/**` family is also public through the **API Gateway** (Part VI §51): the gateway
forwards the request WITHOUT `Authorization`, with its own service token
(`numbering-request.forward`, granted to `cfmeet.api-gateway` by Numbering's trust bundle)
whose delegated context names the authenticated credential; Numbering asks Platform Access
to re-verify that credential live (`POST /internal/v1/credentials/delegated-authority`) and
admits the request exactly like a presented credential (same scopes, SAFETY_ONLY refusal,
throttling, 401 / 503). A token together with `Authorization`, a token without the
permission or credential id, and any gateway token on `/v1/admin/**` or another surface are
refused (401). Search and reservations are
Workstream H ([ADR 0037](../../docs/decisions/0037-numbering-inventory-search-and-reservation.md)),
provisioning and activation prerequisites Workstream I ([ADR 0038](../../docs/decisions/0038-numbering-provisioning-and-activation.md)),
ownership and assignment Workstream J ([ADR 0039](../../docs/decisions/0039-numbering-ownership-and-assignment.md)),
routing grants Workstream K ([ADR 0040](../../docs/decisions/0040-numbering-routing-grants.md): a dedicated grant key,
`NUMBERING_GRANT_KEY_ID` / `NUMBERING_GRANT_PRIVATE_KEY_FILE`; unset → grant routes answer 503);
lifecycle stewardship P–W ([ADR 0043](../../docs/decisions/0043-numbering-lifecycle-stewardship.md)) and the relay-number
authority migration ([ADR 0044](../../docs/decisions/0044-relay-number-authority-migration.md)). Real carrier and
portability adapters are not integrated: provider operations run through the provider-neutral contract and the
deterministic synthetic provider (ADR 0036); no telecom carrier is contacted.

Stewardship policy (all optional, fail closed when malformed): `NUMBERING_QUARANTINE_DAYS` (90),
`NUMBERING_QUARANTINE_SYNTHETIC_DAYS` (7), `NUMBERING_QUARANTINE_REVIEW_AFTER_LATE_EVENTS` (25),
`NUMBERING_RISK_MAX_ACTIVE_NUMBERS` (100), `NUMBERING_RISK_DAILY_ALLOCATIONS` / `…_WEEKLY_ALLOCATIONS` (50 / 200),
`NUMBERING_RISK_RESTRICTED_COUNTRIES` (none), `NUMBERING_NUMBER_SPEND_THRESHOLD_MINOR` (50000),
`NUMBERING_PROVIDER_COST_SPIKE_MINOR` (1000000), `NUMBERING_PORTING_ENABLED` (false),
`NUMBERING_RELAY_LEASE_MAX_HOURS` (9600). The stewardship workers (suspensions, releases, quarantine completion,
relay-lease expiry, usage accrual, ports; drift reconciliation and usage finalization every tenth pass) run every
30 s in the service process.

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
| `service` | probes, trusted / untrusted / unpermitted internal callers, customer auth through Platform Access (presented and API-Gateway-forwarded credentials; administration refuses the gateway), scope enforcement, tenant isolation, correlation / causation, failure handling, OpenAPI = routes |
| `startup` | the real binary: fail-closed configuration, pending migrations refused, serving once migrated |
| `migrations` | fresh / down / re-up, upgrade with data, exact table set, no cross-service FK, least-privilege runtime role |
| `outbox` | atomicity, immutability, ordering, backoff, dead-letter, concurrent publishers, lease takeover, envelope conformance |
| `workers` | lease exclusivity, retry / dead-letter, restart recovery, UNKNOWN_OUTCOME parking, idempotency keys |
| `number_resources` | D10.1–D10.7: identity, E.164, lifecycle policy (Rust = SQL), version conflicts, bindings, capabilities, inventory |
| `allocation` | D10.8–D10.17: reservation contention and expiry, ownership, exclusivity, isolation, history, routing revisions, premature activation, quarantine / uncertainty, rollback |
| `contracts` | Rust = contract = database vocabulary, error / rule codes, event types |
| `capability_catalog` | Workstream E: catalog versioning, verified vs synthetic, combinations / false-mobile prevention, expiry / stale / conflicting evidence, upgrades / downgrades / revocation, binding replacement, normalized errors, HTTP isolation and admin gating, external-verification claims, migration rollback |
| `kyc_eligibility` | Workstream F: requirement selection / versioning / provider extensions, the case lifecycle and database guards, sealed / typed / retained / access-audited evidence, every deterministic verifier outcome, unknown outcomes across a worker restart, privileged manual review and appeals, expiry / revalidation / revocation, eligibility outcomes and isolation, assignment admission, concurrency and idempotency, sensitive-data leakage, HTTP scopes and contracts, migration rollback |
| `inventory_reservations` | Workstream H: canonical ingestion (repeated / concurrent / stale), search filters / capabilities / KYC / freshness / outages / pagination / quotas, idempotent eligibility-bound reservations, single-winner contention, cancellation and expiry with evidence-only releases, unknown release / hold outcomes, late holds, production separation, database guards, migration rollback, the customer API and the served OpenAPI |
| `provisioning_activation` | Workstream I: reserve → provision through the durable executor, eligibility / KYC re-evaluation and revocation, hold read-back, idempotency and single acquisition under concurrency, unknown / lost / partial / interrupted outcomes, delayed callbacks, expiry racing provisioning, route configuration with stale / changed-binding protection, consumer readiness, the acquisition API, database guards, migration rollback |
| `ownership_assignment` | Workstream J: owners from trusted context (real Platform Access principal resolution), eligibility bound to the owner and re-evaluated, per-class target policy, activation only on proven readiness, reservation-first assignment, idempotency and single assignment under concurrency, employee offboarding, organizational pools, same-owner transfers with linked successors, cross-owner refusal, database guards, the assignment / pool / admin API, migration rollback |
| `routing_grants` | Workstream K: inbound voice / SMS resolution (carrier-only, outcomes for unknown / inactive / unready / unrouted numbers), caller-identity and SMS-sender grants (own ACTIVE numbers only, capability and principal checks, one per operation), audience-bound verification and single consumption under concurrency, immediate revocation on routing change / suspension / transfer / capability revocation, expiry / forgery / tampering, protected-runtime synthetic refusal, fail-closed configuration, database guards, migration rollback, the served contract |
| `dcn_lifecycle` | Part IV Step 21 milestone against the deterministic provider: A — a personal DCN from search through KYC, eligibility, reservation, provisioning, assignment, readiness and activation to authorized voice / SMS (real Platform Access); B — an organizational DCN offboarded from one employee to another; C — contention and uncertain outcomes (competing tenants, duplicate / unknown / lost provisioning, expiry during provisioning, KYC revocation, capability downgrade, routing races); D — suspended / releasing / quarantined / reassigned numbers |
| `provider_contract` | Workstream G: the reusable certification suite (and its gating), registry environment restrictions, inventory → canonical resources with provenance and F requirements, the whole lifecycle through durable commands, idempotency / in-flight exclusivity, every scripted fault (not transmitted, unknown after execution, lost, refusals, partial provisioning, release unknown, drift, outage, process interruption), authenticated / de-duplicated / order-safe callbacks, capability downgrades into E, no resurrection, binding replacement, migration rollback |
| `lifecycle_stewardship` | Part IV P–W: suspension / restoration and a grant race; release → quarantine → reassignment (pool return, relinquish with an unknown outcome, scheduled release, late traffic, two-worker completion, previous-owner isolation); ownership periods and audited masked verification; idempotent usage; allocation / signal / spend risk and appeals; porting (flagged, unsupported, uncertain); drift reconciliation and kill switches; database guards; the customer / operator / internal HTTP routes (real Platform Access); backup.sh / restore.sh resuming a release with one provider RELEASE across a restart and two workers (needs pg_dump; `DISASTER_RECOVERY_CERT_REQUIRED=1` in CI) |
| `relay_cutover_e2e` | Part P across real processes: a served Numbering and the real Privacy Relay binary — backfill, compatibility verification, LOCAL → NUMBERING cutover with routing equivalence, leases, rollback to LOCAL (part of `make communications-e2e`) |
| `communications_e2e` | Part IV Step 20, across REAL processes (each with its own database) around Numbering served on a real port: SMS — inbound DCN SMS to the routed team, outbound SMS with an UNKNOWN submission reconciled (never resubmitted), delivery receipts, `sms.*` webhooks through Calling, revocation / lifecycle / wrong-tenant / unsupported refusals, a Numbering outage failing closed (Platform Access, SMS Gateway, Calling); voice — carrier calls over real SIP (SIPp into the certification Asterisk edge) through Telephony's DCN allowlist, Numbering's per-call grant, Privacy Relay's caller protection and Calling's grant verification, forged scope / target, expiry, employee offboarding, capability downgrade, suspended / releasing / quarantined and SMS-only numbers, a Numbering outage while app-to-app calls still work (Telephony, Privacy Relay, Calling on LiveKit). `make communications-e2e`; SKIPPED without the binaries unless `CFMEET_REQUIRE_E2E=1` / `CFMEET_REQUIRE_VOICE_E2E=1` |
