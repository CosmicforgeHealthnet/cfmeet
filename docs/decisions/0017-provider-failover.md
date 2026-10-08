# ADR 0017: Provider failover, unknown outcomes and duplicate-call prevention

- **Status:** Accepted (verified against the deterministic interconnect and the real SIP /
  media edge; real-carrier failover certification is pending with the interconnect
  blocker of ADR 0016)
- **Date:** 2026-10-08
- **Scope:** CF Meet Part E §48 and §51 (§113–§121, §142–§152). Implemented in
  `services/calling/src/routing/engine.rs` (`revise`),
  `services/calling/src/calls/app/public/failover.rs`,
  `services/telephony/src/commands/` (admission, executor) and
  `services/telephony/src/routing.rs` (health, circuits, capacity). Verified by
  `services/calling/tests/public_failover.rs`, `services/telephony/tests/failover.rs`,
  `services/telephony/tests/unknown_outcome.rs` and
  `services/telephony/tests/reconciliation.rs`.

## Context

A paid origination has three possible outcomes: success, definite failure, and an
unknown outcome. The provider may or may not have created a call that costs money and
rings someone. Failing over on an unknown outcome is the classic way a platform calls the
same person twice and pays twice. Never failing over leaves calls failing whenever one
interconnect has trouble. Business authorization (may this Call reach this party at all)
must stay separate from interconnect choice (which carrier carries it).

## Decision

### Definite vs unknown

* **Definite failure**: the provider said no, or the request provably never left
  (`transmitted = false`), or read-back with evidence covering the transmission proves
  absence (`ABSENT_AT_PROVIDER`). Nothing paid exists.
* **Unknown outcome**: a timeout after send, a reset after (part of) the body was sent, a
  malformed success, or a worker that died after the transmit fence. It is never
  recorded as a failure: the leg is `UNKNOWN`, Calling's attempt is `UNKNOWN`, and
  reconciliation reads it back. A correlated provider event, or read-back finding the
  call, binds it and the leg continues. Proven absence turns it into a definite failure.
  Inconclusive read-back keeps it unknown, with an escalating incident. A database
  trigger forbids putting a transmitted command back to `PENDING`.

### Who decides what

* **Calling decides whether** another attempt is allowed. This is the Routing Engine's
  `revise(plan, facts)` on the stored Route Plan:
  * only before `CONNECTED`;
  * only after a definite **provider-side** failure (`ROUTING` / `NETWORK` / `EXECUTION`,
    or `ABSENT_AT_PROVIDER`);
  * only within the plan's attempt budget (`1 + maxFailoverAttempts`, 0–3, per
    environment routing policy; default 0);
  * only for a paid route.

  The callee's own answers (busy, no answer, declined, `TARGET_UNAVAILABLE`) never fail
  over. Neither does a `NOT_SENT` failure (no eligible interconnect is left), a policy
  failure, or an `UNKNOWN`.
* **Telephony decides which** eligible interconnect executes. Its inputs are capability,
  destination country, route and provider health, circuit state, capacity, the
  conservative cost estimate against the reservation, the caller identity's binding to
  the provider, and **attempt history**: an interconnect that already failed on the
  provider side for this party of this Call is not used again.

### What a failover attempt is

It is a new canonical `CallLeg`, because a leg has exactly one execution binding. It
also gets:

* a new `CallAttempt` chained to the failed one (`previous_attempt_id`);
* a new spend reservation, with the failed leg's reservation released (nothing was
  connected) and every spend / velocity limit applied again;
* a new durable `ORIGINATE` command with a fresh single-use Contact Resolution Grant and
  a proof window recomputed from the recorded Privacy Relay decision;
* the protected party re-pointed to the new leg. The database allows that only after the
  party's current leg is `FAILED`.

All of this commits inside the event's transaction (a savepoint), so a refused
reservation leaves nothing half-built.

### Duplicate-call prevention

The same rule holds at both boundaries:

* **Calling** fails over only on a definite failure. An attempt of the same party that is
  still `PENDING` or `UNKNOWN` turns `revise` into `AwaitReconciliation`.
* **Telephony** refuses (`409 EXECUTION_IN_PROGRESS`) any execution to the same protected
  party of the same Call while an earlier one is live or `UNKNOWN`. This is enforced by
  the check and by a partial unique index
  (`uq_telephony_legs_one_live_per_party` on `(calling_call_id, contact_reference)` for
  non-terminal outbound legs), so concurrent submissions cannot slip past it.

### Late-success race

Provider A times out, so its leg is `UNKNOWN`. Reconciliation starts, and A then succeeds
late:

* every failover attempt submitted meanwhile is refused, because A is unresolved;
* A's correlated event (or read-back) binds A and it continues;
* no B was ever executed.

Suppose instead A was proven absent and a provider later reports it alive anyway. A's
leg is terminal, so the event is recorded `LATE_AFTER_TERMINAL` and never resurrects
anything. The reconciler's inventory read-back finds the live provider call bound to a
terminal leg and terminates it. In Calling, an event for the failed leg is stale and
never touches the Call.

### Circuit breaker, health, capacity

* Each route and provider has a circuit: `CLOSED → OPEN` after
  `CIRCUIT_FAILURE_THRESHOLD` (5) consecutive definite provider-side failures, with a
  `PROVIDER_CIRCUIT_OPENED` incident.
* After `CIRCUIT_OPEN_SECONDS` (60) one probe is allowed (`HALF_OPEN`). The probe's
  success closes the circuit; its failure re-opens it.
* Health is `HEALTHY` / `DEGRADED` / `UNAVAILABLE` / `UNKNOWN`. An open circuit reads as
  unavailable.
* Configured capacity is reserved atomically with the route choice. Telephony never
  launches more paid legs than permitted (`CAPACITY_EXHAUSTED`).
* Open circuits and unavailable routes affect public-network routes only. `APP_RTC`
  never depends on Telephony.

### Unresolved terminations

A terminate the provider refuses, or whose outcome stays unknown, never reports the leg
ended while the paid call may be up:

* the leg is held in `ENDING`;
* an `UNKNOWN_TERMINATION_OUTCOME` incident opens;
* the reconciler re-reads and re-terminates (terminate is idempotent) with backoff until
  the provider confirms.

## Consequences

* A second interconnect improves completion only for provider-side trouble. It never
  converts uncertainty into a duplicate call.
* Failover costs one new leg, attempt and reservation per try. The plan's budget (max 3
  failovers) bounds it, and so do the spend policy's velocity, per-party and concurrency
  limits.
* Calling never learns which interconnect carried a leg. Interconnect identities stay
  inside Telephony; Calling passes the plan's opaque candidate references and Telephony
  narrows them.
* With one interconnect (launch), failover simply finds no eligible route
  (`NOT_SENT`), so the Call fails safely. Adding interconnects B and C is configuration
  (`telephony-admin provider | route | sip-peer | number`), not an API change.
