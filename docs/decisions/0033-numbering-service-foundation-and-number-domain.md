# ADR 0033 — Numbering: service foundation and the number domain

Status: Accepted (Part IV Workstreams C–D). Builds on ADR 0032 (Platform Access) and the
per-service ownership of ADR 0031. Supersedes the "Numbering is future work" note of
`docs/architecture/service-data-ownership.md`.

## Context

Part IV introduces public numbers that customers own, route and later port: Digital
Communications Numbers (DCNs). Part III only had Privacy Relay's temporary relay pool,
which must not grow into a number authority. Calls, SIP / PSTN execution, relay sessions
and SMS transport already have owners (Calling, Telephony, Privacy Relay, later SMS).
The number authority must be correct before any carrier, KYC provider or public API is
connected: identity independent of providers, explicit uncertainty, no premature
activation, strict tenant and environment isolation, and reliable change propagation.

## Decisions

1. **Own service, own database.** `services/numbering` owns Number Resources, provider
   bindings, capabilities, inventory observations, reservations, DCN assignments,
   routing profiles, assignment history and the lifecycle audit, plus its outbox, job
   queue and idempotency keys (`number_*`, `dcn_*`, `numbering_*`; migration history
   `numbering_seaql_migrations`). No foreign key or query leaves those tables; the
   boundary guard (`NUMBERING_DOMAIN_ELSEWHERE`) refuses number tables elsewhere. It owns
   no call, SIP / PSTN, relay-session or SMS-transport data, and no KYC / SMS /
   portability / fraud / billing table yet (later workstreams create their own).
2. **CF Meet ids are canonical.** A Number Resource's UUID is the identity of the number
   and survives provider replacement and porting. Provider identifiers and provider
   references live only on `number_provider_bindings` / inventory snapshots and never
   leave Numbering: the internal API exposes opaque binding references, events carry no
   number and no provider id (database CHECK on the outbox payload).
3. **One lifecycle policy, enforced twice.** The 14 states (`DISCOVERED … RELEASED`,
   `FAILED`, `UNKNOWN_PROVIDER_STATE`) have one edge set: `lifecycle::allowed` in Rust and
   `number_lifecycle_allowed` in SQL (a test compares all 196 pairs). Every change goes
   through `transition_in` (expected revision, edge, prerequisites, actor / reason /
   evidence, lifecycle audit, outbox event, one transaction); the database refuses state
   changes without the policy's transaction-local session flag, off-edge, without the
   revision step, or leaving `UNKNOWN_PROVIDER_STATE` except in reconciliation mode.
   Uncertainty records the operation and the state it came from; only a RECONCILIATION
   actor with evidence resolves it, so irreversible provider actions are never repeated
   blindly.
4. **Evidence over inference.** Classification and VERIFIED capabilities need evidence
   (never for synthetic inventory); mobile is never inferred from SMS or the E.164
   format; capability downgrades that impact open assignments need an explicit
   acknowledgement and report the impacted assignments.
5. **Observation is not allocation.** Inventory snapshots are append-only facts with a
   freshness bound. Allocation is a reservation, scoped to the trusted context, PENDING
   until a provider hold is confirmed; at most one current reservation per number
   (unique partial index; concurrent attempts serialize on the resource row). Expiry is
   exclusive at `expires_at` and never implies a provider release: a held live number
   stays RESERVED with release PENDING until evidence; a live PENDING hold becomes
   UNKNOWN. Quarantined and uncertain numbers are never allocated (domain and trigger).
6. **Assignments are separate records.** One open assignment per number; ownership
   (PRINCIPAL for personal / professional, ORGANIZATION for every organizational class,
   including employee-direct) is immutable on the record — a new owner is a new
   assignment, history is append-only. ACTIVE only through eligibility (recorded
   decision), a held reservation of the same scope, a pinned CURRENT routing profile
   revision and provisioning with bindings covering every routable capability;
   the database refuses an ACTIVE assignment on a non-ACTIVE number and an ACTIVE number
   without an ACTIVATING assignment.
7. **Environment separation.** SANDBOX / TEST environments use `SYNTHETIC_TEST`
   inventory only; PRODUCTION never does — in the domain and in database triggers.
8. **Trusted context from Platform Access only.** Customer credentials are verified by
   Platform Access on every request (`platform-credential.authenticate`); the tenant /
   application / environment come only from the authority chain. SAFETY_ONLY authority is
   refused; Platform Access unreachable is a 503. Internal routes require a service token
   and a permission from Numbering's own trust bundle (`number-resource.read`: Calling,
   Telephony).
9. **Reliability foundation.** Transactional outbox in the platform envelope (leased
   claims with tokens, `SKIP LOCKED`, per-aggregate order, bounded backoff,
   DEAD_LETTER); a leased job queue whose expired claims are recovered by declared retry
   safety (IDEMPOTENT → retried; RECONCILE_ON_UNCERTAINTY → UNKNOWN_OUTCOME, resolved
   only by reconciliation); scoped idempotency keys with request fingerprints. Readiness
   reports backlog, dead letters and unknown outcomes (counts only). The outbox publisher
   is wired once a consumer transport exists; until then events stay PENDING and visible.
10. **Scope of this workstream.** No public search / provisioning / assignment-mutation
    API, no carrier adapter, no KYC / SMS / portability: the domain operations those
    workstreams call exist and are tested; nothing contacts a carrier.

## Consequences

* Numbering can be deployed, migrated and tested without Docker, carriers or KYC
  providers; synthetic inventory is distinguishable everywhere.
* Every later Numbering workflow (provider reservation, release, porting, KYC) reuses the
  outbox, job and idempotency foundations and the lifecycle policy instead of adding
  ad-hoc state changes.
* Contracts: `cfmeet.numbering.common`, `cfmeet.numbering.event`,
  `cfmeet.numbering.internal-api` (conformance tests compare them with the Rust types,
  the database CHECK constraints and the routes).
