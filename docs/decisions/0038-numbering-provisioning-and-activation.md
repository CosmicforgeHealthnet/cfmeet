# ADR 0038 — Numbering: provisioning and activation prerequisites

Status: Accepted (Part IV Workstream I). Extends ADR 0033–0037.

## Context

A held reservation must become a number the platform actually acquired, through G's
durable provider commands, without ever acquiring twice, provisioning on a stale or
revoked basis, or claiming a number is usable because a provider said "provisioned".
Real routing also needs the provider to point the number at CF Meet and the consumer
that carries the traffic (Telephony / SMS Gateway) to accept it — services integrated in
later workstreams whose acknowledgements must never be fabricated.

## Decisions

1. **Acquisitions.** `number_acquisitions` is the durable provisioning intent of one
   reservation of a scope (one per reservation; a definite failure may be retried). The
   A–D lifecycle edges are unchanged; the PROVISIONING prerequisite accepts an
   eligibility-bound PROVISIONING acquisition of the current HELD reservation as well as
   an assignment holding it (both bases are recorded, never a bare number).
2. **Provisioning order.** Reservation is the caller's and still holds (HELD and
   unexpired, or taken over by an assignment of the scope); eligibility is RE-EVALUATED
   by the F engine for this number and its serving provider with the caller's current
   standing (a recorded decision is never trusted blindly: a KYC revocation after the
   hold refuses; a pre-eligibility hold never provisions); capabilities are supported;
   the provider hold is read back and must still be this reservation's (`HOLD_LOST`
   otherwise, with a drift incident). One transaction then writes the scoped idempotency
   record, the acquisition, PROVISIONING and the `PROVISION` command (key derived from the
   idempotency record). Concurrent requests: one acquisition, one provider execution.
3. **Proof before state.** G's executor applies the provider's answer: proven
   acquisition → binding (only the capabilities the provider serves; a gap is drift),
   PROVISIONED, acquisition PROVISIONED, a HELD reservation CONVERTED (`ACQUISITION`) so
   a racing expiry can no longer release it; refusal → acquisition FAILED, number back to
   RESERVED; unknown / lost / interrupted → acquisition UNKNOWN and read-back
   reconciliation, never a resend. Delayed callbacks are de-duplicated and reconcile.
4. **Route configuration.** Per capability family (VOICE / SMS) a configuration names a
   stable opaque route reference, a gap-free route revision and the binding (revision)
   it was requested on, through durable `CONFIGURE_*` commands. It becomes CONFIGURED
   only on the provider's acknowledgement evidence; a response for a superseded
   configuration changes nothing, one for a binding that changed since FAILS it
   (`STALE_BINDING`), one naming a different route is a conflict incident.
5. **Readiness, never fabricated.** A family is READY only when its route is CONFIGURED on
   the CURRENT binding and the carrier of its traffic acknowledged that revision through
   `POST /internal/v1/number-resources/{id}/route-readiness` (`number-route.acknowledge`:
   Telephony for voice, the new `cfmeet.sms-gateway` identity for SMS; database-enforced).
   A binding change makes the family stale (`BINDING_CHANGED`) until reconfigured and
   re-acknowledged. Synthetic inventory reports `liveRouting: false`.
6. **PROVISIONED is not ACTIVE.** An acquisition carries no owner; an ACTIVE DCN number
   needs an eligible assignment with routing and readiness (Workstream J).

## Consequences

Provisioning and route configuration are certified against the deterministic provider.
Live carrier provisioning and the real Telephony / SMS Gateway acknowledgements are
Workstreams L–O and external certification. A provisioned acquisition that is never
assigned is held at the provider until the P/Q release workflows exist.
