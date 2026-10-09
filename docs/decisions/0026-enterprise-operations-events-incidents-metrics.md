# ADR 0026: Enterprise operations — durable routing events, incidents, bounded metrics

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F cross-cutting work (§141–§199). Implemented in
  `services/calling/src/calls/app/enterprise/mod.rs` (`record_step`),
  `services/calling/src/calls/app/incidents.rs`, `services/calling/src/metrics.rs`,
  `services/calling/src/calls/service/lifecycle.rs` (missed-call counter), migration
  `calling 000030`, and the suites `enterprise_operations`, `enterprise_security`,
  `enterprise_load` plus the race tests added to the voicemail, recording and business-hours
  suites. Builds on ADRs 0018–0025.

## Context

Part F adds long-lived enterprise state machines (ring groups, queues, IVR sessions,
voicemail and recording artifacts) on top of the canonical Call. Operators and downstream
consumers need durable facts about them, a way to notice conditions that must never be
silent, and metrics that stay bounded at any scale — without a second event system, a
second incident system that crosses service databases, or labels that leak identities.

## Decision

### Durable enterprise events (transactional outbox)

* Every enterprise routing decision is written ONCE as evidence (`enterprise_routing_steps`)
  and, in the SAME transaction, as an `ENTERPRISE_ROUTING_STEP` Call event published
  through `calling_outbox_events` (`calling.enterprise-routing-step`): step number,
  decision, reason, destination type and — except for a public contact — the enterprise
  destination id. Queue entered / offered / assigned / abandoned / overflowed, ring-group
  steps, IVR results, schedule routes, forwarding and voicemail are therefore durable,
  ordered (Call sequence) and contract-validated facts. Realtime / push delivery is never
  the authority (ADR 0025).
* Voicemail and recording artifacts keep their own events (`VOICEMAIL_*`, `RECORDING_*`).
* Realtime signals (delivery only): ring offers (`call.incoming` / `call.ringing`),
  `call.queued`, `call.queue_position`, `call.ivr_waiting`, `call.ivr_input` (validity only,
  never the key), `call.recording_started` / `stopped`, and `call.voicemail_started` (the
  caller is now leaving a message; `maxSeconds` only).

### Idempotency where duplicate effects matter (§148)

| Effect | Mechanism |
|---|---|
| Enterprise Call create | `Idempotency-Key` (command store); a failed attempt commits nothing and can be retried with the same key |
| Ring-group / queue assignment | `call_answer_group_claims` primary key + per-participant claim under the Call row lock |
| Queue dispatch | per-queue advisory lock, `SKIP LOCKED` on Calls, unique pending offer per entry / agent |
| Transfer | `Idempotency-Key`, one transfer in flight per Call |
| Voicemail creation / finalization | one message per Call (`UNIQUE (call_id)`); delivery is idempotent (`AlreadyStored`), one object |
| Recording start / stop | one active recording per Call (partial unique index) → `AlreadyActive`; stop and finalization are state-guarded |
| Forward-to-public | Part E gates: idempotent spend reservation and Telephony command keys |

### Operational incidents (§168)

Calling records its own incidents in `calling_incidents`, with exactly the discipline of
Telephony's (`telephony_incidents`, Part E §50): a closed category vocabulary, severity,
deduplication while OPEN by a stable key (category + resource) with an occurrence count,
bounded restricted evidence (opaque ids and codes only), immutable identity columns. Each
service keeps its own because no service writes another's database (this is the existing
incident pattern applied to Calling, not a second system). Categories:

| Category | Raised when |
|---|---|
| `STUCK_QUEUE_ENTRY` | an entry is still WAITING / OFFERING 120 s past its deadline (its Call could not be processed) |
| `QUEUE_ASSIGNMENT_CONFLICT` | an admitted answer finds its queue entry already gone (never a second owner) |
| `IVR_ROUTING_CORRUPTION` | a pinned, validated IVR version names a node that does not exist |
| `VOICEMAIL_FINALIZATION_FAILURE` | a voicemail capture fails or is lost |
| `RECORDING_ARTIFACT_FAILURE` | a recording capture fails or is lost |
| `FORWARDING_LOOP_REPEATED` | a routing loop / hop budget is blocked (one incident per destination; occurrences count repeats) |

Orphan enterprise SIP legs / bridges are Telephony's incidents (its reconciliation reads
enterprise legs from the edge). Raising never fails the operation: inside a transaction
it runs in a savepoint and commits with the operation that observed it.

### Bounded metrics (§165–§167)

Counters, gauges and histograms whose label values are `&'static str` from bounded
vocabularies (result, state, reason, outcome, decision, category, route class, kind…):
enterprise calls and outcomes, extension resolution failures, ring-group attempts /
winners / no-answer, queue entries / offers / exits (answered, abandoned, timed out,
overflowed), queue wait seconds, `calling_enterprise_queue_depth` (gauge), IVR entries /
invalid input / timeouts / results, schedule evaluations (open / closed), forwards and
loop blocks, voicemail and recording results, capture deliveries, artifact access,
incidents per category, and `calling_missed_calls_total` (the ADR 0025 derivation, counted
once at the Call's terminal transition). No Call, extension, user or queue id, telephone
number, key or object key is ever a label (asserted over the rendered exposition); Call
and correlation ids appear only as tracing fields.

### Reliability and failure behaviour

* Workers (queue deadlines and dispatch, IVR timers, voicemail / recording finalization,
  retention) run on every instance with `FOR UPDATE SKIP LOCKED` / advisory locks and
  persisted deadlines; a crashed or slow instance's work is picked up later without
  duplicate effects (certified by the stuck-entry test: locked Call → incident, released
  → drained once).
* A database failure inside routing commits nothing (no Call, offer, step or event); the
  same request with the same key succeeds afterwards.
* Telephony / the SIP edge down: application enterprise Calls are unaffected; SIP leg
  commands stay durable and retried; a SIP Call is never claimed CONNECTED without media
  evidence and can always be ended (terminate is never blocked).
* Object storage down: voicemail / recording finalization waits in a recoverable state
  (ADR 0023 / 0024). No Redis is used by Calling; realtime is PostgreSQL LISTEN / NOTIFY and
  clients recover from the API.

## Certification

* `tests/enterprise_operations.rs` — routing-step outbox events (contract-validated), one
  incident with occurrences for repeated loop blocks, IVR corruption incident, stuck
  queue entry incident and later drain, bounded metric labels and the missed-call
  counter, database failure mid-routing, Telephony down.
* `tests/enterprise_security.rs` — scope-isolation matrix (enterprise caller,
  configuration administrator, voicemail administrator) over every administrative,
  voicemail and recording route; contract scan for numbers, provider / edge ids, keys,
  object keys and URLs (only signed media-access grants, with expiry).
* Races: `concurrent_starts_record_once`, `stop_racing_the_call_end_stops_once`,
  `concurrent_finalization_stores_once`, `delete_racing_retention_deletes_once`,
  `schedule_changes_during_route_selection_are_consistent`, plus the existing ring-group,
  queue, abandon and transfer races.
* `tests/enterprise_load.rs` — measured throughput (printed `LOAD …` lines) with FIFO /
  unique positions under a queue burst, bounded fan-out with one winner, index-only plans.
* Mutation tests A–J (§188): see the Part F certification report.

## Consequences

* Every routing step adds one Call event and one outbox row; consumers can rebuild
  enterprise routing timelines without reading Calling's tables.
* Incidents are evidence for operators, not control flow; resolving one is an operator
  action (`status = RESOLVED`).
