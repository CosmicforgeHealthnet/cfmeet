# ADR 0043 — Numbering lifecycle stewardship: suspension, release, quarantine, history, usage, risk, portability, reconciliation and controls

Status: Accepted (Part IV Workstreams P–W). Builds on ADR 0033 (lifecycle), 0036
(provider commands), 0038–0040 (provisioning, assignment, grants).

## Context

Workstreams C–O made a Digital Communications Number acquirable, assignable and usable
for voice and SMS. What happens AFTER activation — a suspension, a release, the cooldown
before reuse, a new owner, the evidence of who held a number when, its costs, its abuse
and its portability — had only state protections (the 14-state lifecycle, the assignment
edges and grant revocation). The workflows had to be added without a second state
machine, a second provider executor, a second grant system or a second audit store.

## Decisions

1. **One orchestration module over the existing authorities.** `src/stewardship/` only
   composes the lifecycle transition path, the guarded assignment step
   (`step_assignment_in`, the same history / grant revocation / event as every change),
   the durable provider command executor, the outbox and the incident register. Workflow
   records advance by reading their provider command's outcome (a worker pass and the API),
   never by a new executor; nothing possibly executed is resubmitted.
2. **Suspension is local first (P).** One transaction: number and assignment SUSPENDED
   (leaving ACTIVE revokes every grant; a grant issued concurrently fails verification
   against current state), the suspension record (source, reason, actor, evidence, time),
   the provider SUSPEND where the adapter supports it, `number.suspended`. Ownership is
   retained. Restoration re-checks the open assignment, the KYC / eligibility basis, the
   tenant standing, risk, the active binding, the capability truth and every routed
   family's readiness, records each check, then (when the provider was suspended) submits
   RESTORE and returns the assignment to ACTIVE only on the provider's confirmation
   (`number.restored`). Operator / risk / provider suspensions are lifted by operators.
3. **Release is a record, in the mandatory order (Q).** `number_release_requests`:
   grants stopped → voice / SMS routing removed (configured routes retired, so no consumer
   readiness survives) → organizational resources detached → removal evidence confirmed
   (else FAILED with an incident) → provider release (`PROVIDER_RELINQUISH`) or none
   (`POOL_RETURN`) → unknown outcome waits for reconciliation → quarantine. `PORTED_OUT` is
   completed by a port-out only.
4. **Quarantine is policy and evidence.** Cooldown by jurisdiction, number type, synthetic
   class and risk (`QP1_*` policy references), operator review / extension / hold, late
   voice / SMS traffic recorded (`number_late_activity`, refused, never delivered; repeated
   traffic requires a review). Completion is a `SKIP LOCKED` worker: cooldown over, review
   satisfied, every provider command settled, and — for a pool return — the provider's
   fresh read-back that CF Meet still holds the number. A relinquished number becomes
   RELEASED, never reassignable, whatever the provider's inventory says later. The
   database refuses REASSIGNABLE while a release is open or a quarantine is not completed,
   provider-confirmed and pool-returned.
5. **Reassignment is a new acquisition basis.** A fresh reservation confirmed by the
   provider read-back (no hold is taken on a number CF Meet already holds), a NEW
   assignment with the new owner's eligibility, a new routing profile (routing revision
   advances) and NEW provider route configurations the consumers must acknowledge before
   activation. Nothing of the previous owner carries over.
6. **History is read, audited and masked (R).** Ownership periods come from the
   assignment records; operators state a purpose and a case reference on every read
   (`number_history_access`, written before anything is returned); owners are masked by a
   per-case digest. Ownership verification answers MATCH / NO_MATCH / AMBIGUOUS for a
   stated claim — possession of a reassigned number never establishes a historical owner
   and other owners are never revealed.
7. **Usage is internal cost evidence (S).** `number_usage_records` keyed by a source key
   (retries, callbacks and restarts never duplicate), provider costs never customer
   prices, PROVISIONAL then FINAL (immutable). Rental / setup / renewal accrue from the
   number's commercial facts; carriers report voice seconds / SMS segments
   (`number-usage.report`). Customers see quantities only.
8. **Risk extends, never duplicates (T).** Allocation decisions at reservation (limits,
   velocity, restricted countries, multi-country review, mass registration / farming);
   signals from the carriers (`number-risk.report`) scored per number; a restricting score
   suspends through decision 2 (source RISK); per-number spend refuses new outbound grants;
   a provider cost spike opens a provisioning breaker. Decisions and appeals are appended;
   originals are never modified. App-to-app communication never consults any of it.
9. **Portability is a provider-independent foundation (U).** Nine states, a number digest
   (never the number), protected KYC evidence as ownership proof, a `PortingGateway` port
   whose production default supports nothing (`NUMBERING_PORTING_ENABLED` off by default),
   uncertain outcomes resolved by status queries only, port-out releasing routing only on
   authoritative completion. No live portability provider is integrated.
10. **Recurring reconciliation fails closed (V).** A sweep compares the lifecycle with the
    provider read-back for every controlled number (skipping numbers with a command in
    flight): provider drift raises an incident and suspends an ACTIVE number (source
    PROVIDER) or holds a quarantine / reassignable number; UNKNOWN commands go to the G
    reconciliation.
11. **Kill switches (W).** `number_operational_controls` (+ append-only events):
    `DCN_INBOUND_ROUTING` (resolutions and inbound grant verification answer
    `ROUTING_DISABLED`), `DCN_CALLER_IDENTITY`, `DCN_SMS_SENDER`, `NUMBER_PROVISIONING`,
    `NUMBER_PORTING`, `RELAY_LEASES`. Restrictive only; operators only.
12. **Events.** `number.suspended`, `number.restored` (the two specification-named,
    two-segment types — the only ones the outbox accepts besides
    `number.<aggregate>.<change>`), `number.release.changed`, `number.quarantine.changed`,
    `number.usage.recorded`, `number.risk.decided`, `number.port.changed`,
    `number.relay-lease.changed`.

## Consequences

Every workflow is resumable after a crash and safe under concurrent workers; the
provider is never asked twice for a possibly executed operation. Real carriers still need
their adapter's SUSPEND / RESTORE / RELEASE / PORT semantics certified (REAL PROVIDER
CERTIFICATION PENDING).
