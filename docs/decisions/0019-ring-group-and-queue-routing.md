# ADR 0019: Ring-group and queue routing on one Call

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §55 (ring groups) and §56 (call queues). Implemented in
  `services/calling/src/enterprise/{ring_groups,queues}.rs` (configuration) and
  `services/calling/src/calls/app/enterprise/{router,ring_groups,queues}.rs` (runtime),
  Calling migrations 000022 (ring groups, group claims) and 000023 (queues).
  Builds on ADR 0008 (multi-device ringing, single-winner arbitration) and ADR 0018.

## Context

A ring group rings several people for ONE inbound call; a queue holds callers until an
agent is free. The tempting designs are wrong for this platform: one Call per member
("forking" calls), queue calls in a separate table, an in-memory queue in one process,
or a timer per waiting caller. Each breaks one of: one canonical Call, single-winner
answer, restart safety, or multi-instance safety.

## Decision

### One Call, many offers, one winner per answer group

Every ring-group step and every queue offer is an ordinary set of ring offers
(`call_invitations`) of the SAME Call. Each logical group of offers carries an
`answer_group_id`:

* a ring group's offers share one group per routing step;
* a queue entry has one `answer_group_id` for its whole life, so every offer of the
  entry, to whichever agent, belongs to one group.

`call_answer_group_claims` (PRIMARY KEY `(call_id, answer_group_id)`) admits one
answering participant per group, on top of the existing per-participant claim
(`call_answer_claims`). A database trigger refuses an ACCEPTED invitation of a group
without the matching claim, so a buggy writer cannot create a second winner. Losers'
live offers end ANSWERED_ELSEWHERE.

### Ring groups

* SIMULTANEOUS rings every eligible member (bounded by the policy's fan-out and the
  Call's participant limit) for the group timeout.
* ORDERED rings one member for `memberRingSeconds` (never past the group deadline). When
  the member's offers end, the durable invitation-expiry worker — not an in-memory timer —
  evaluates the ring round and the enterprise continuation rings the NEXT eligible member
  (`ring_cursor` in `enterprise_call_contexts`).
* No answer: the group's fallback (extension or queue; voicemail once configured) takes
  the SAME Call through the router, inside the hop budget and visited set (a fallback
  chain back to the group is `LOOP_BLOCKED`). No fallback: ordinary NO_ANSWER / DECLINED.
* Groups never nest (members are principals or extensions that route directly to a
  principal), so a membership cycle cannot exist.

### Queues

* `enterprise_queue_entries` holds one row per waiting Call; FIFO order is
  `(entered_at, seq)`; positions are always computed from that order (never stored), so
  they cannot drift. A Call holds at most one active entry (partial unique index).
* A queued Call is RINGING (pre-answer) with no live offer while it waits; the
  enterprise context phase is QUEUED. The caller gets `call.queued` /
  `call.queue_position` realtime signals and the create / call views carry the position.
* Dispatch takes the queue's transaction-scoped advisory lock with
  `pg_try_advisory_xact_lock` (an instance that cannot take it skips the queue — another
  instance is dispatching it), walks WAITING entries in FIFO order, locks each Call row
  with `SKIP LOCKED` (a Call being changed elsewhere is skipped this pass) and offers it
  to the least recently offered available agent: an ACTIVE member whose principal is
  ACTIVE, with no pending offer of any queue, not on an answered Call, with an eligible
  endpoint. Partial unique indexes allow ONE pending offer per entry and per agent and ONE
  accepted offer per entry, so concurrent dispatchers cannot double-offer.
* An unanswered offer (expired / declined) is resolved by the same ring-round evaluation
  as every other offer; the entry returns to WAITING and the next agent is offered. The
  agent who just missed it cools down for one offer timeout before being offered that
  entry again.
* Capacity is checked under the queue's advisory lock (blocking `pg_advisory_xact_lock`
  for enqueue), so a burst cannot over-fill it. Full: the overflow destination takes the
  Call, else the Call ends BUSY.
* Maximum wait: the worker withdraws the pending offer and the entry OVERFLOWS (the
  overflow destination takes the same Call) or TIMES OUT (NO_ANSWER).
* The caller hangs up: the cancel path marks the entry ABANDONED in the same
  transaction; any other end of the Call is swept to CANCELLED by the worker. Terminal
  entry and offer states are final (trigger-enforced).
* The worker (`expire_due`) runs regardless of the enterprise kill switch, so waiting
  Calls always drain.

### Lock ordering

Everything that changes a queued Call takes the Call row first, then the entry. Dispatch
holds only the queue advisory lock and never waits for a Call row (`SKIP LOCKED`), so it
cannot deadlock with answer / cancel / timeout paths. Enqueue takes the advisory lock
after the Call row; a cross-queue overflow cycle under simultaneous load can at worst
produce a database-detected deadlock that aborts one transaction, which is retried by
reconciliation.

## Consequences

* Restart and multi-instance safety come from PostgreSQL state alone; no coordinator,
  no in-memory timers.
* Queue metrics: entries, offers by outcome, exits by result, overflow reasons and a wait
  histogram — labels are bounded vocabularies, never ids.
* Skills-based routing, priorities and callbacks are out of scope (FIFO only).
