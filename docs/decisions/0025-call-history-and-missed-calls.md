# ADR 0025: Call history and missed calls are derived from Call evidence

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §62 (§130–§140). Implemented in
  `services/calling/src/calls/app/history.rs` (derivation, query, safe views),
  `services/calling/src/calls/http.rs` (`GET /v1/calls` filters),
  `services/calling/src/signaling/http.rs` (`GET /v1/calling/history`) and migration
  `calling 000029`. Builds on ADR 0006 (canonical Call), ADR 0008 (endpoints, offers,
  per-participant answer arbitration) and the enterprise ADRs 0018–0024.

## Context

Clients lose notifications: push transports drop or fail, realtime connections break, a
device is off. If "missed" were whatever a client was told (or acknowledged), the history
of the same person would differ per device and a lost push could erase or invent a missed
call. A principal often has several devices (phone, browser, desktop), and enterprise
routing offers one Call to several people (ring groups, queues).

## Decision

### History is the canonical Call, per logical participant

A principal's history is the set of Calls it participates in: exactly one
`call_participants` row per principal per Call (`UNIQUE (call_id, principal_id)`), however
many devices were offered. Every field comes from canonical evidence — the participant,
its ring offers (`call_invitations`) on every device, its sessions' connection intervals,
the Call's state and timestamps, the enterprise context. No "missed" flag is stored, and
push delivery, realtime signals and client acknowledgements are not inputs: they can
neither create, erase nor change an entry.

### Exact derivation (`ParticipantOutcome`)

* **OUTBOUND** (the participant is the Call's initiator): `ANSWERED` once the Call has an
  `answered_at`; `NOT_ANSWERED` when it reached a terminal state without one;
  `IN_PROGRESS` otherwise. An initiator never "missed" its own Call.
* **INBOUND** (a recipient) — first match wins:
  1. `ANSWERED` — one of the participant's offers is ACCEPTED (on any of its devices) or
     one of its sessions has a connection interval. `answeredAt` is the earliest of these.
  2. `DECLINED` — the participant declined an offer (its own decision, not a miss).
  3. `ANSWERED_ELSEWHERE` — its offers ended ANSWERED_ELSEWHERE without the participant
     answering: another participant of the same answer group (a ring-group or queue
     colleague) won the Call.
  4. `RINGING` — an offer is still PENDING / RINGING.
  5. `MISSED` — at least one offer was issued and every offer ended unanswered (EXPIRED,
     cancelled because the caller hung up, the Call ended or the ring moved on — e.g. an
     ORDERED ring group's next member, a queue's next agent, voicemail).
  6. `NOT_REACHED` — never offered and the Call is terminal; `IN_PROGRESS` otherwise.

`missed` is exactly `outcome = MISSED`. The outcome is computed at read time, so it is
always the current consequence of the evidence (a hunt-group member whose turn expired is
MISSED even while a colleague is still being rung).

### Multi-device and answered elsewhere

The losing devices of a participant that answered receive ANSWERED_ELSEWHERE offers, but
the participant matched rule 1 — one entry, ANSWERED, on every device. A ring-group
colleague's win is rule 3, never MISSED. There is never a second entry for the same
logical participant.

### API

* `GET /v1/calls` (platform credential, `calls.read`) gains `principalId` — the
  participant-scoped history (`CallHistoryList`) — with `direction`, `outcome`, `missed`
  (which require `principalId`), the existing `state` (terminal outcome), time bounds and
  cursor, and `enterpriseDestinationType` + `enterpriseDestinationId` (EXTENSION as
  dialed or reached, RING_GROUP, QUEUE, IVR, VOICEMAIL; never a public contact) for either
  listing. Without `principalId` the listing keeps its shape.
* `GET /v1/calling/history` (signaling credential) — recovery: the credential's own
  principal's history with the same derivation and filters; `principalId` is refused.
* Keyset pagination on the Call's `(created_at, id)` (newest first), the same opaque
  cursor as `GET /v1/calls`.

### Safe history entry

The Call core (id, kind, state, terminal reason, requested / ringing / answered /
connected / ended timestamps), `durationMs` (CONNECTED → end), the participation
(direction, outcome, missed, added / answered / left), other participants' principal ids
and display names (≤ 10), enterprise labels (dialed internal extension, its label, phase,
outcome, ring group / queue label), the voicemail link (message id, state, read state —
only for an owner of the mailbox; audio through the audited voicemail routes) and
`recordingAvailable` (a READY recording; metadata and audio through the audited
recording routes). Never a telephone number, contact reference, provider / edge
identifier, object key or URL.

### Performance

`call_participants.call_created_at` is an immutable copy of the Call's `created_at`,
stamped by a trigger on insert; `idx_call_participants_history (tenant, application,
environment, principal, call_created_at DESC, call_id DESC)` makes one page one index
range (no sort, no table scan — certified with `EXPLAIN`). Per-page enrichment is one
bounded query per kind (≤ 100 Calls). `idx_enterprise_routing_steps_destination` serves
destination filters.

## Certification

`services/calling/tests/call_history.rs`: realtime bus down and no push → the Call rings,
times out, the reconnecting recipient (either device) reads ONE missed Call; device B
never notified, device A answers → ANSWERED, not missed; declined / cancelled /
connected-with-duration / ringing; ring-group colleague → ANSWERED_ELSEWHERE and ORDERED
hunt → MISSED; voicemail link for the mailbox owner only; pagination, bounds, scope and
validation; index-only history plan and the immutable `call_created_at`.
`tests/enterprise_recording.rs` asserts `recordingAvailable` on both participants'
histories.

## Consequences

* "Missed" is a derived read model; analytics wanting snapshots must read it, not store a
  copy that could diverge.
* A recipient added without an offer and without a session (not a path Calling creates)
  would read NOT_REACHED.
