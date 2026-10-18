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
* **Sandbox DCN voice runs only over the synthetic TEST interconnect** (decision 7); paid
  public routes stay PRODUCTION + live-telecom only, unchanged. A real-carrier DCN call
  (public network, billable) is certified separately and remains PENDING until a live
  carrier is attached.

## 7. Synthetic TEST interconnect (Workstream L closure)

Sandbox / test environments get only Numbering's SYNTHETIC inventory, and Calling's paid
public routes require PRODUCTION with `live_telecom_allowed` (Part E). Rather than weaken
that gate, test-only execution is a separate **interconnect class** decided at the
route-policy boundary:

* **Telephony route class.** `telecom_routes.interconnect_class` is `PUBLIC` (default) or
  `SYNTHETIC_TEST`; immutable; a SYNTHETIC_TEST route costs 0 (CHECK). The operator CLI
  admits one only in a local / test runtime, on a SIP_TRUNK provider whose peers are
  loopback-only; the service refuses any non-PUBLIC route outside local / test at load,
  and uses one only with `TELEPHONY_SYNTHETIC_INTERCONNECT_ENABLED` (refused at start-up
  outside local / test). It can therefore never reach a real PSTN destination, carrier
  trunk or billable route.
* **Numbering marks the inventory.** Resolutions, current-assignment views and grant
  verifications carry `inventoryClass` (`SYNTHETIC` / `REAL`); Telephony and Calling read
  it from Numbering, never from the offer.
* **No mixing.** A synthetic DCN travels only a SYNTHETIC_TEST route and a SYNTHETIC_TEST
  route carries only a synthetic DCN (inbound: `INTERCONNECT_MISMATCH` at the edge and in
  Calling; outbound: `INTERCONNECT_MISMATCH` before any provider request). A relay number
  never uses a synthetic route.
* **Calling's route policy.** `RouteInputs.interconnect`: PUBLIC keeps the live-telecom
  gate exactly as before; SYNTHETIC_TEST is never allowed where live telecom is
  (`SYNTHETIC_INTERCONNECT_NOT_ALLOWED`), needs `CALLING_SYNTHETIC_INTERCONNECT_ENABLED`
  (refused at start-up outside local / test) and an ACTIVE SANDBOX / TEST environment,
  and every other policy gate (authorization, permitted classes, flags, spend, incidents,
  verified contacts) still applies; candidate routes are filtered by class. The class is
  recorded on the route plan and sent with the originate command.
* **Numbering keeps its authority.** Ownership, active assignment, voice capability,
  routing authorization, the routing revision and single-use caller grants apply
  unchanged. A sandbox call without a DCN is still a paid public call and is refused
  (`LIVE_TELECOM_NOT_ALLOWED`).

The communications journeys (`services/numbering/tests/communications_e2e.rs`) prove
both directions over real SIP: Scenario A — SIPp carrier → Asterisk edge → Telephony →
Numbering → Privacy Relay → Calling rings the current owner's endpoint, the application
accepts and joins media (real WebRTC client), LiveKit SIP bridges into the edge with the
single-use join code, the call is CONNECTED and completes at zero cost; Scenario B —
the application calls out presenting its DCN, Numbering's caller grant is consumed by
Telephony and the SIPp carrier receives the DCN as `From`. An application-media channel
that presents its join code before the edge bridge exists is held and attached by the
bridge command (exactly once), not refused.
