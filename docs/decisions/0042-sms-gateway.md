# ADR 0042 — SMS Gateway: the authoritative SMS transport service

Status: Accepted (Part IV Workstreams M–O). Builds on ADR 0040 (routing grants: SMS
sender grants and inbound resolution) and ADR 0032 (Platform Access).

## Context

Numbering owns Digital Communications Numbers: assignments, KYC, lifecycle, routing and
the signed routing grants. SMS needs a transport authority — durable message records,
provider submission, provider callbacks, delivery receipts and safety controls — that
must not copy Numbering's authority, must never send from a number the caller does not
own, and must never send a message twice after an ambiguous provider outcome. Customer
webhooks already exist in Calling (the platform's webhook delivery), and identities in
Platform Access.

## Decisions

1. **A separate service with its own database.** `services/sms-gateway`
   (`cfmeet.sms-gateway`, port 8096) owns only `sms_*` tables (repository guard
   `SMS_DOMAIN_ELSEWHERE`; architecture test). It reaches Platform Access, Numbering and
   Calling over authenticated internal HTTP with narrow permissions
   (`platform-credential.authenticate`, `platform-principal.resolve`,
   `number-sms-sender.authorize`, `number-grant.verify`, `number-sms-route.resolve`,
   `platform-event.publish`), and serves no internal operation itself (its trust bundle
   trusts no issuer). Docker is never needed locally; no live carrier credential is
   needed anywhere outside an enabled adapter.
2. **Transport state machine, enforced twice.** REQUESTED → AUTHORIZED → SUBMITTING →
   SUBMITTED → DELIVERED; FAILED / EXPIRED from the pre-submission states; SUBMITTING →
   AUTHORIZED only for a *definitely unsent* attempt; SUBMITTING / SUBMITTED → UNKNOWN;
   UNKNOWN → SUBMITTED / DELIVERED / FAILED only on provider evidence. Inbound messages
   are RECEIVED. The domain checks transitions; a trigger refuses any other transition,
   any change of the message's facts, any rewrite of content and any delete, and forces
   the optimistic version to advance by one.
3. **Never a blind resubmission.** Submissions are attempts under a lease; a unique index
   allows at most one attempt that is live or may have reached the provider (CLAIMED /
   SUBMITTED / UNKNOWN), so after an UNKNOWN outcome no further attempt can even be
   inserted. An ambiguous timeout or a lease that expired mid-submission is UNKNOWN; it
   is resolved only by a delivery receipt or a provider status query (reconciliation).
4. **Protected content.** Content, remote numbers and released senders are stored only
   AEAD-sealed (`v2.<kid>.<…>`, bound to the record and purpose; rotation overlap) —
   CHECK constraints refuse anything else. Remote numbers are also stored as keyed
   fingerprints (opt-out, threads, rate windows) and masked (`+***1234`), the only form
   ever returned. Outbox payloads are refused by the database if they carry a content,
   number, grant or secret key or an E.164-looking value. Logs carry ids and codes only.
   Content is purged after its retention; unattributable inbound messages are quarantined
   sealed and purged.
5. **Numbering is the authority, per message.** The gateway keeps the facts Numbering
   answered for ONE message (number resource, assignment, routing revision, grant id),
   never a copy of assignments, KYC or the number lifecycle.
   * *Outbound:* the sender is `from.numberResourceId`; Numbering issues a single-use
     SMS_SENDER grant for the delegated scope (and principal) and the gateway consumes it
     with the message's operation reference before the message becomes AUTHORIZED, storing
     the released sender sealed. An arbitrary sender id or raw number cannot be expressed
     (contract counter-examples). Numbering unreachable: 503, nothing recorded.
   * *Inbound:* the destination is resolved by a dedicated SMS resolver
     (`POST /internal/v1/routing/sms-destination-resolutions`, `number-sms-route.resolve`,
     SMS Gateway only). `inbound-resolutions` is voice-only (`FAMILY_NOT_SUPPORTED`).
     ROUTED → the message is stored for the owning environment; anything else is
     quarantined and never delivered to a customer; Numbering unreachable → 503 so the
     provider retries, nothing stored.
6. **Provider abstraction.** One adapter trait (submit with four outcomes — accepted,
   rejected, definitely not sent, unknown; status query; callback authentication before
   parsing; callback parsing). The deterministic adapter scripts every fault mode for
   tests and local use and is refused in staging and production; a reusable contract
   suite every adapter must pass. Callbacks are HMAC-signed over the raw body with a
   timestamp window and de-duplicated by `(provider, event id)`; provider references are
   internal only.
7. **Events through the platform webhook delivery.** Message events
   (`sms-gateway.message.{requested,submitted,delivered,failed,unknown,blocked,received}`)
   are written in the transport's transaction to the gateway's outbox and published to
   Calling's platform event ingestion, which delivers them as `sms.*` customer webhooks
   with the existing signing and retry machinery. No second webhook system.
8. **Threads are a transport index.** A thread groups the messages between one Number
   Resource and one remote party (fingerprint) in one environment, so replies need no
   number. It is not the Part V conversation model.
9. **Safety controls with reason codes.** Destination prefix allowlist, per-environment
   and per-recipient rate windows (429, not recorded), opt-outs (STOP / START keywords
   inbound, customer blocks), estimated daily spend, identical-content fan-out (abuse);
   a refused message other than a rate limit is recorded FAILED with its reason and an
   `sms.blocked` event. The complete fraud / investigation engine stays in Workstream T.

## Consequences

* A Numbering outage stops SMS (fail closed) and nothing else; voice and app calls are
  unaffected.
* An UNKNOWN message may stay UNKNOWN until provider evidence arrives; that is preferred
  over a duplicate send.
* A real aggregator adapter must pass the contract suite against its sandbox before it
  can be configured; none is enabled in this build.
