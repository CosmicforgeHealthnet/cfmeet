# ADR 0041 — Digital Communications Numbers on the voice path

Status: Accepted (Part IV Workstream L). Extends ADR 0040 (routing grants), ADR 0013–0017
(public-network calling) and ADR 0018 (enterprise calling).

## Context

Numbering (ADR 0033–0040) owns Digital Communications Numbers: assignments, lifecycle,
routing revisions and signed routing grants. Telephony executes calls on interconnects,
Calling owns the canonical Call, Privacy Relay owns protected contacts. A DCN must carry
voice in both directions without any of these services copying another's authority,
without Calling ever seeing a number, and without a caller presenting a number it is not
entitled to — while a Numbering outage must never affect application or relay-number
calls.

## Decisions

1. **Local allowlist, remote authority.** Telephony keeps a DCN row in
   `telecom_number_bindings` (`number_kind = 'DCN'`, the Numbering `number_resource_id`;
   one kind per public number, trigger-enforced). The canonical number is read from
   Numbering at bind time (`telephony-admin dcn bind`), never typed by an operator. The
   row decides only WHERE Telephony can carry the number; who it routes to is decided by
   Numbering per call.
2. **Inbound.** A dialled DCN is resolved per call (`inbound-resolutions`, VOICE). The
   binding and Numbering must name the same Number Resource. NOT_ROUTED / UNAVAILABLE /
   an anonymous caller / a mismatch reject the dialog before anything else. Privacy
   Relay protects the caller in the scope of Numbering's signed INBOUND_VOICE grant
   (`protect-dcn-caller`, verified with Privacy Relay's dedicated routing-grant trust —
   never its service-auth trust), so Telephony asserts no scope. Calling receives the
   grant and references only (`dcn-inbound-calls`), verifies the grant against
   Numbering's CURRENT state, requires the relayed claims to equal the verified ones and
   routes ONLY to the verified target (principal: PSTN_TO_APP without a relay session;
   enterprise extension / ring group / queue: the enterprise router through an
   extension). The leg records the DCN evidence (number resource, assignment, routing
   revision, grant id) — never the number or the grant.
3. **Outbound presentation.** Numbering decides ownership; Calling decides entitlement.
   A principal-owned number is presented only by its owner; an organization-owned number
   only by an ACTIVE principal reached through its CURRENT voice target (extension
   principal, ring group member, active queue member, department member) — general
   organizational scope never suffices. The check runs at call creation and again at
   dispatch, where Numbering issues one CALLER_IDENTITY grant for exactly that
   origination (`leg:{callLegId}:{commandId}`).
4. **Single use at the paid effect.** Telephony stores the grant with the command
   (`dcn_identity_grant`, scrubbed by trigger at transmit or on leaving the active
   states), consumes it with Numbering immediately before route selection, requires the
   verified scope and number to be the command's, and presents the number only on an
   interconnect it is bound to for VOICE_OUTBOUND. Refused / reused / mismatched →
   `CALLER_IDENTITY_DENIED`, no provider request, no fallback to a relay identity.
   Numbering unreachable → the attempt is rescheduled (or fails), one
   `NUMBERING_UNAVAILABLE` incident; a response lost after consumption fails closed on
   retry (never a second consumption).
5. **Activation and revocation.** Telephony acknowledges Numbering's CONFIGURED VOICE
   route (`route-readiness`) only while an ENABLED local VOICE_INBOUND binding exists on
   an ENABLED route (operator `dcn acknowledge`, and a worker reconciliation for route
   revisions); `dcn disable` stops carrying the number. Suspension, release, transfer or
   re-routing at Numbering take effect per call through resolution and grant
   verification — no copy of Numbering's lifecycle exists elsewhere.
6. **Fail closed, isolated.** Without `NUMBERING_INTERNAL_BASE_URL` (Telephony),
   `CF_NUMBERING_BASE_URL` (Calling) or `NUMBER_GRANT_TRUST_BUNDLE_FILE` (Privacy Relay)
   every DCN operation is refused; application, relay-number and enterprise-device calls
   never call Numbering.

## Consequences

* DCN calls to IVR / voicemail / schedule entry destinations are refused until Telephony
  can play prompts on a public inbound leg (documented limitation).
* The full suspend / release orchestration across services remains Workstreams P/Q;
  Workstream L only enforces current state per call.
* **Sandbox DCNs are not callable for voice.** Numbering assigns synthetic DCNs only to
  SANDBOX / TEST environments, while Calling carries paid public-network routes
  (PSTN_TO_APP, APP_TO_PSTN) only in a PRODUCTION environment allowed live telecom
  (Part E). A routed call to a sandbox DCN therefore travels the whole chain — edge,
  Telephony's allowlist, Numbering's grant, Privacy Relay's caller protection, Calling's
  grant verification and routing to the verified target — and is refused by Calling's
  live-telecom gate (`LIVE_TELECOM_NOT_ALLOWED`); an outbound call presenting one is
  refused before the presentation check. The Step 20 journeys
  (`services/numbering/tests/communications_e2e.rs`) prove exactly that chain; ringing an
  application from a DCN needs a production number from a live carrier. Whether sandbox
  DCN voice should run against the deterministic interconnect is a Part E policy
  decision, deliberately not changed here.
