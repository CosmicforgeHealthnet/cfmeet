# ADR 0022: Enterprise hold, resume and transfer reuse the canonical Call actions

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §59 (§90–§99). Implemented in
  `services/calling/src/calls/app/enterprise/transfer.rs` (target resolution and policy)
  and the existing Part C transfer (`services/calling/src/calls/app/transfer.rs`), with
  routing evidence on the enterprise context. Builds on ADR 0009 (call control,
  idempotency, transfer) and ADR 0018–0021.

## Context

Enterprise users hold, resume and hand calls to colleagues, groups and queues. A
parallel "enterprise transfer" would duplicate the Part C state machine, its idempotency,
its single-winner answer and its recovery rules — and diverge from them.

## Decision

* **Hold / resume** are the canonical actions: CONNECTED → HELD and HELD → CONNECTED on
  the current version, enforced at the media provider first (RTC publish permissions;
  resume only on provider-confirmed media-path evidence). Calling stays authoritative.
  A bridged SIP / public leg hears the held media path; the edge's own hold primitive
  (music on hold) is not driven by Calling in this deployment.
* **Transfer** is the canonical `POST /v1/calls/{call_id}/transfer` with exactly one
  target: `targetPrincipalId` (Part C), `targetExtension` or `targetDestination` (Part F,
  requiring `enterprise.calls.use`). An enterprise target is resolved in the trusted
  scope to ONE principal:
  * EXTENSION → followed (bounded, loop-safe) to its destination;
  * ENTERPRISE_MEMBER / PRINCIPAL → that principal;
  * RING_GROUP → the first member, in order, with an eligible endpoint;
  * QUEUE → the first available agent (the same availability rule as queue offers);
  * SCHEDULE → its current OPEN / CLOSED route;
  * PUBLIC_CONTACT → refused (403 when the enterprise policy forbids public forwarding,
    422 otherwise: public transfer of an established Call is not offered — public
    reachability is provided by forwarding, ADR 0021);
  * IVR / VOICEMAIL → not transfer targets (400).
* The enterprise policy of an enterprise Call (`transferAllowed`) gates every transfer
  of it, whatever the target.
* The transfer itself is unchanged Part C: TRANSFERRING, the target joins the SAME Call
  (same id, authorization and history), the stable path is kept until the outcome is
  known, the transferring participant is retired only on the target's verified media
  join; a declined / unanswered / stalled transfer recovers to CONNECTED (or HELD if it
  started held) and retires the target path — no orphan participant, ring offer, leg or
  bridge. Enterprise evidence: `TRANSFER / TRANSFER_STARTED | TRANSFER_COMPLETED |
  TRANSFER_FAILED` routing steps.
* Races keep one owner: one in-flight transfer per Call (a second is 409), the answer
  claim admits one answerer, an end during a transfer cancels its offers, and a
  transferring party leaving first still lets the target take the Call.

## Certification

`tests/rtc_certification.rs` (real LiveKit): hold / resume of an enterprise Call with
provider-enforced permissions; extension B → extension C transfer on one Call with real
media (B removed from the room, A hears C); a declined transfer leaving the Call
CONNECTED with media intact.

SIP: enterprise SIP devices are callers in this deployment and an established SIP ↔ RTC
media path is the Part E LiveKit SIP bridge, which this certification environment does
not run. Telephony executes HOLD / RESUME on ENTERPRISE_INTERNAL legs at the Asterisk
edge, but an end-to-end SIP hold / redirect certification of a CONNECTED enterprise Call
is recorded as pending with that bridge.

## Consequences

* One state machine, one idempotency model, one recovery path for every transfer.
* A principal joins a Call once (Part C): a declined target is not re-offered on the
  same Call.
