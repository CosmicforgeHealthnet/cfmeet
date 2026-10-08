# ADR 0015: SIP / media edge stack and network topology

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part E §41. Implemented in `services/telephony/src/sip/` (the
  `SipMediaEdge` port and its `AriEdge` adapter), `services/telephony/src/edge_events.rs`,
  `services/telephony/src/inbound.rs`, `services/telephony/src/bridges/` and
  `infra/telephony/` (deployment templates and the loopback certification bench).

## Context

Public-network calls need a SIP signalling and media boundary that:

* carriers connect to (SIP over UDP / TLS, RTP media);
* anchors every public leg's media so the application RTC side, other phone legs and
  CF Meet's own containers never see a carrier's media address, and no party's identity
  crosses the bridge;
* lets **only Telephony** decide what is dialled (toll-fraud boundary);
* supports early media / ringing, answer, DTMF (RFC 4733), hold / resume (re-INVITE),
  redirect / transfer primitives, multi-leg bridges, and per-leg media statistics;
* can be certified in CI without credentials, carriers or Docker images that need a
  registry token.

Only Telephony may hold raw E.164 numbers; Calling and the business services must never
see SIP, carrier identifiers, dialog IDs or call IDs.

## Options considered

| Option | Strengths | Weaknesses for CF Meet |
|---|---|---|
| **Asterisk 20 + ARI** (chosen) | Distribution package on Ubuntu 24.04; ARI is a REST + WebSocket control API that maps directly onto a "command / event" port; mixing bridges anchor media; channel-level DTMF, hold, redirect, RTP statistics; PJSIP `identify` by IP; Stasis apps mean the dialplan never dials | Single-process B2BUA (scale horizontally by edge instance + route); not a high-capacity SBC |
| FreeSWITCH + ESL / mod_verto | Strong media engine and conferencing | Current packages need a SignalWire access token (cannot be installed reproducibly in CI or in an air-gapped build); ESL is an event socket rather than a resource API |
| Kamailio / OpenSIPS | Very high signalling capacity, excellent as SBC / load balancer | Signalling only: needs an additional media server (rtpengine) **and** a B2BUA for bridges, DTMF and hold — three components to secure and certify |
| Hosted CPaaS SIP domain | No edge to operate | Moves media anchoring and caller-identity control to a vendor; ties the platform to one provider's call model; violates the provider-neutral boundary of §42 |

## Decision

1. **Asterisk 20 via ARI is the SIP / media edge.** Telephony is the only ARI client.
   The port `SipMediaEdge` (`src/sip/mod.rs`) hides it: originate / answer / ring /
   reject / hangup / hold / resume / DTMF / redirect / bridge / statistics commands and a
   normalized `EdgeEvent` stream. Nothing SIP-shaped crosses into Telephony's domain or
   further: ARI channel IDs are the provider correlation keys, and raw event payloads are
   hashed into `ari-…` event IDs.
2. **The dialplan never dials.** Every carrier dialog (`from-trunk`) and every
   application-media dialog (`from-app-media`) enters the Stasis application; Telephony
   validates and decides. Unidentified sources match no endpoint and are refused before
   any dialplan runs.
3. **Media is anchored.** `direct_media = no`; every multi-party call is a mixing bridge
   on the edge. Connected-line updates are disabled so no identity crosses a bridge.
4. **Application media attaches by a single-use join code.** A bridge gets an opaque
   `cfmj…` media join code; the RTC media gateway dials the internal media peer with it.
   Calling and clients only ever see the code, never SIP addresses or numbers. The edge
   answers an inbound carrier dialog only once the application media has joined (the
   caller hears ringing until then).
5. **Network topology** (see `infra/telephony/README.md`): public SIP / RTP listeners
   accept only carrier CIDRs (firewall + PJSIP identify); ARI listens on a private control
   network and Telephony refuses a public ARI URL outside local / test; an internal SIP
   transport serves only the RTC media gateway; Telephony's API is service-authenticated
   and its provider callbacks are provider-authenticated.
6. **Secrets are injected as files** (`/run/secrets/edge/…`, `TELEPHONY_ARI_PASSWORD_FILE`,
   `TELECOM_PROVIDER_SECRETS_DIR`); the database holds only references.
7. **Edge failure is safe.** With no edge (or an unreachable one) every public route fails
   with a definite pre-send failure; app RTC is unaffected. A lost event stream is
   reconnected, and reconciliation (§50) queries channel state, so a missing event cannot
   leave a leg open.

## Certification

`services/telephony/tests/sip_certification.rs` boots a real Asterisk with
`infra/telephony/certification/` on loopback addresses and drives SIPp carriers and an
application-media peer through outbound, busy, no-answer, inbound, application-media
bridge with real RTP, DTMF, hold / resume, phone-to-phone, redirect, unknown-peer refusal
and edge-down cases. CI runs it with `TELEPHONY_SIP_CERT_REQUIRED=1`
(`.github/workflows/telephony-sip-certification.yml`).

This certifies the edge, SIP signalling and media anchoring. It does **not** certify a
real carrier: the SIPp peers are simulators. Real-carrier certification is a separate,
pending step (ADR 0016).

## Consequences

* One Asterisk process is a capacity and failure unit; horizontal scale is several edges
  behind per-route SIP peers, each with its own ARI URL (a later evolution of the
  `SipMediaEdge` registry). A dedicated SBC (Kamailio) can be added in front later
  without changing Telephony's port.
* The edge is stateful for the duration of a dialog; an edge restart ends its live
  calls, which reconciliation then records as ended (never as a silent success).
