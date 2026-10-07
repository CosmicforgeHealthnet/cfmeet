# ADR 0009: Call control, idempotency, hold and application transfer

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part C §21, §25, §55–§70, §76, §89–§91; implemented in
  `services/calling/src/calls/app/` (`create.rs`, `control.rs`, `transfer.rs`),
  `src/calls/service/commands.rs`, migration 000013

## Context

Clients retry, run on several devices and race each other. Control must never be a
generic state setter, must not silently apply a command decided on an old view of the
Call, must take effect once per request even under retries and concurrency, and must
be enforced where it matters — at the media provider — not only in the database.

## Decision

### Commands, not state writes

The public surface is explicit commands: `POST /v1/calls` (create),
`…/accept|decline|cancel|end|hold|resume|transfer`, `…/sessions/{sid}/remove`,
media credentials. There is no PATCH and no state field in any body (bodies refuse
unknown fields; architecture guard). Scope comes only from the authenticated
credential; foreign Calls, principals, endpoints, offers and sessions are 404.

### Expected version: one concurrency mechanism

Every state-changing command carries `expectedVersion`; a different current version is
409 `STALE_CALL_VERSION` with no effect. Commands run under the Call row lock; the
version check, the transition and its evidence commit together.

### Idempotency

`Idempotency-Key` is required for creation and transfer and honoured on every other
command. `call_commands` records (scope, operation, key) → request fingerprint
(SHA-256 over operation, target and canonical body), status IN_PROGRESS → COMPLETED and
the response snapshot (bounded; no token / secret fields). A transaction-scoped advisory
lock serializes concurrent duplicates. Same key + same request → the recorded response
with `Idempotent-Replayed: true`; same key + different request → 409
`IDEMPOTENCY_CONFLICT`; a still-running duplicate → 409 `CONFLICT`.

### Creation and routing (§21)

Create = REQUESTED → APP_DIRECT_V1 authorization (AUTHORIZED) → target resolution →
one APPLICATION route, one shared RTC leg, attempt 1, private media binding → provider
ensure (outside the transaction) → DIALING → initiator session authorized → offers →
RINGING. A provider timeout leaves the Call ROUTING with the attempt outcome unknown;
reconciliation resumes it and, after `media_ensure_deadline`, fails it (MEDIA). No
eligible endpoint fails it (ROUTING). Retries never duplicate the route, leg, attempt
or room (deterministic name).

### Cancel vs end

* **Cancel** is pre-active (REQUESTED … ANSWERED): offers cancelled, attempts finished,
  application routes / legs closed, sessions ended and revoked, media torn down;
  CANCELLED immediately. Repeating it on a CANCELLED Call is a no-op; it is refused on a
  CONNECTED Call.
* **End** is for active Calls: ENDING first; connections removed, room terminated;
  **ENDED only on cleanup evidence** (`CLEANUP_CONFIRMED`). If the provider does not
  confirm, the Call stays ENDING and reconciliation retries. Usage is finalized from
  Calling's interval evidence.

### Hold / resume semantics

Hold applies the held mask to every admitted connection **at the provider first**
(no audio / video publishing; subscription unchanged; never data), then commits HELD.
If the provider does not confirm, the answer is 503 `MEDIA_UNAVAILABLE` and the Call
stays CONNECTED. Resume re-derives each connection's permissions from the *current*
authorized state (`call_participant_media_permissions` + call kind), never from a stored
pre-hold grant, so permissions revoked in the meantime (e.g. a removed session) stay
revoked; resume also requires media-path evidence (`CALL_EVIDENCE_REQUIRED` otherwise).
A provider that cannot update permissions cannot run application calls at all
(`CAPABILITY_UNAVAILABLE`).

### Application transfer

Transfer keeps **the same Call**: `call_transfers` (REQUESTED → RINGING_TARGET →
ACCEPTED → COMPLETED | FAILED | CANCELLED; one active per Call) records the transferring
participant, the target principal and the pre-transfer state (CONNECTED or HELD). The
Call is TRANSFERRING; the target's endpoints get TRANSFER offers (single-winner as
above). When the target's session is admitted next to a remaining party, the
transferring participant is retired (sessions removed at the provider), the transfer
COMPLETED and the Call CONNECTED again. A principal participates in a Call once.

Recovery: target declines (TARGET_DECLINED), does not answer (TARGET_NO_ANSWER) or
accepts but never joins within `transfer_join_deadline` (MEDIA_PATH_LOST) → the
transfer FAILS, its offers are cancelled, the target participant retired, its leg
closed, and the Call returns to its pre-transfer state when the original parties still
have a media path (otherwise it starts ending). It is never left in TRANSFERRING; ending
the Call cancels an active transfer. Idempotency-Key replays return the recorded
transfer without a second target path.

### Available actions

`GET /v1/calls/{id}/actions` derives, per credential, which commands are currently valid
(scope, state, live offers, provider capabilities) with a reason (`INSUFFICIENT_SCOPE`,
`STATE`, `CAPABILITY_UNAVAILABLE`) — clients do not compute them.

## Consequences

* Every mutation is attributable (actor kind: customer application, application
  endpoint, internal service) and evidenced in `call_events` + outbox.
* Clients must send `expectedVersion` and should send `Idempotency-Key` on every
  command; both are part of the public contract.
