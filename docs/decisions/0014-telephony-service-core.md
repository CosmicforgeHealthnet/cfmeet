# ADR 0014: Telephony service core — legs, commands, transmit fence and provider-neutral events

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part E §39–§40 (and the execution model §44–§51 build on). Implemented
  in `services/telephony/` (own crate, own PostgreSQL database, migrations 000001–000006)
  and `docs/architecture/telephony-contract-inventory.md`.

## Context

Calling owns the logical Call and its canonical legs; Privacy Relay owns contexts,
sessions, the Contact Vault and relay numbers. Someone must execute public-network legs:
hold raw E.164 numbers for the shortest possible time, talk to interconnects and the SIP
edge, absorb at-least-once provider callbacks, and survive crashes **without ever
dialling twice or billing a leg that never happened**. That is a separate trust and
failure domain, so it is a separate service.

## Decision

### Service and authority

`services/telephony` is its own deployable with its own database. No other service reads
it; it reads no other service's database (repository guard). Calling commands it over
service-authenticated `/internal/v1` routes with least-privilege permissions
(`telephony-leg.originate | terminate | hold | resume`, `telephony-bridge.manage`,
`telephony-status.read`); Telephony reports back to Calling with
`telephony-event.apply`, `telephony-reconciliation.apply`, `telephony-inbound.offer`.
Customer credentials (`cfmk_*`, verifiers, peppers, customer `Authorization`) never reach
Telephony.

### Raw numbers

An originate command carries only references: the canonical CallLeg, the protected-call
authorization proof and a single-use Contact Resolution Grant. Telephony resolves the
grant at execution time through Privacy Relay (`resolve-for-execution`), receives the
number and the server-selected caller identity (the session's relay number) and passes
them straight to the provider adapter. Raw E.164 is held in memory in a redacting
`ExecutionNumber`, never persisted, logged or emitted. Callers cannot set a caller ID.

### Legs

`telephony_legs` (one per execution attempt) with a trigger-enforced graph:

```
PLANNED → ORIGINATING → RINGING → CONNECTED → (HOLD ⇄ CONNECTED) → ENDED
   any non-terminal → FAILED | UNKNOWN ;  UNKNOWN → CONNECTED | ENDED | FAILED (only by evidence)
```

At most one non-terminal execution per canonical CallLeg (partial unique index that
includes `UNKNOWN`), so a second leg can never be started while the first may be live.

### Commands and the transmit fence

Every action is a durable `telephony_commands` row written **before** any provider
action (intake is idempotent by key + digest; replays return the same command). Workers
claim with `FOR UPDATE SKIP LOCKED` leases. Before calling the provider, the worker
commits `transmitted_at` (the fence):

* a lease that expires **before** the fence is a safe retry (nothing was sent);
* a lease that expires **after** the fence, a timeout or a transport error is
  `UNKNOWN_OUTCOME` — never FAILED, never retried blindly. A trigger forbids returning a
  transmitted command to PENDING.

Unknown outcomes open an incident and are resolved only by evidence: a late callback
bound by correlation key, or a reconciliation query (§48, §50).

### Provider bindings and events

`provider_call_bindings` link a leg to a provider object (`PREPARED → SENT → BOUND |
ABSENT → RELEASED`) by a correlation key Telephony chose before transmission. Provider
callbacks are authenticated per adapter, deduplicated by `(provider, provider_event_id)`,
mapped to `NormalizedEvent`s and applied in order; stale, late-after-terminal and
unbindable events are recorded with a disposition (`QUARANTINED` events of unknown
objects never attach to an arbitrary call). Calling receives only neutral leg / bridge
events through a per-leg ordered transactional outbox.

### Provider neutrality

The `TelecomInterconnectProvider` trait and the `SipMediaEdge` port are the only places
provider specifics live (`src/providers/`, `src/sip/`). Public contracts never carry
provider names, carrier IDs, provider call IDs, SIP Call-IDs, dialog IDs, SIP bodies,
webhook payloads, carrier status strings or credentials. Credentials are files named by
reference; the database stores references only.

### Safety switches

`TELEPHONY_ENABLED`, `TELEPHONY_PUBLIC_OUTBOUND_ENABLED`,
`TELEPHONY_PUBLIC_INBOUND_ENABLED`, `TELEPHONY_CALLBACK_RELAY_ENABLED` all default to
**off**; provider / route enable flags and circuits stop new paid legs only. App RTC never
depends on Telephony.

## Consequences

* A crash at any point leaves either a safe retry or an explicit `UNKNOWN` with an
  incident — never a silent double origination.
* Usage is recorded once per leg (append-only, unique per leg) from evidence, so billing
  can be reconciled independently of event delivery.
