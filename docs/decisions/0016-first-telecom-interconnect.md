# ADR 0016: Provider-neutral interconnect contract and the first interconnect (SIP trunk)

- **Status:** Accepted — real-carrier certification **pending** (blocker, see below)
- **Date:** 2026-10-08
- **Scope:** CF Meet Part E §42–§43. Implemented in `services/telephony/src/providers/`
  (`mod.rs` contract, `sip_trunk.rs` first interconnect, `deterministic.rs` test adapter),
  `services/telephony/tests/provider_contract.rs`,
  `services/telephony/tests/real_interconnect_certification.rs` and the operator CLI
  `telephony-admin provider | route | sip-peer | number`.

## Context

CF Meet needs lawful public-network reach (Nigeria first) without letting any carrier or
CPaaS vendor shape the platform: no provider name, call ID, status string or credential in
a CF Meet contract; failover between interconnects; and exactly-once paid effects in the
face of lost responses.

## Decision

### The contract

`TelecomInterconnectProvider` is the only provider surface business code sees:

* advertised **capabilities** (`OUTBOUND_VOICE`, `INBOUND_VOICE`, `SIP_TRUNK`,
  `STATUS_QUERY`, `EVENTS`, `HEALTH`, `CAPACITY`, `IDEMPOTENT_ORIGINATION`,
  `COST_ESTIMATE`, `DTMF`, `CALLER_ID_SELECTION`) — nothing is assumed from a vendor;
* `originate` → three-valued `Accepted | DefiniteFailure{transmitted} | UnknownOutcome`;
  an unknown is **never** collapsed into a failure;
* `terminate | hold | resume` → `Done | AlreadyGone | DefiniteFailure | Unknown`
  (terminate is idempotent);
* `query_call` by provider reference or by Telephony's pre-transmission correlation key →
  `Active | Ended | Absent | Unknown`. **`Absent` is a proof**: an adapter may answer it
  only when its evidence covers the transmission instant (`QueryKey.transmitted_at`);
  otherwise it must answer `Unknown`;
* `health`, `capacity`;
* `verify_event` (authentication before any parsing or state) and `normalize_event`
  (strict parser into `NormalizedEvent`s).

The generic **contract suite** (`tests/provider_contract.rs`) runs the same assertions
against every adapter.

### The first interconnect: SIP trunk on CF Meet's own edge

The first interconnect is a lawful carrier / aggregator **SIP trunk** terminating on the
CF Meet SIP / media edge (ADR 0015) — not a CPaaS REST API. Reasons:

* media stays anchored on CF Meet's edge (privacy, no vendor in the media path);
* any lawful Nigerian carrier or aggregator offering SIP trunking can be added as another
  `SIP_TRUNK` provider row + PJSIP endpoint, which is what makes failover between
  interconnects real;
* the call model (correlation by edge channel ID, status read-back from the edge, events
  over the private control plane) is identical for every trunk.

Consequences of the adapter design:

* SIP-trunk events come over the authenticated edge control plane; the adapter **refuses
  every HTTP callback**.
* Read-back relies on the edge: a live channel, or the edge's end history. The edge's
  history proves absence only for requests transmitted while its event stream was
  continuously connected and nothing was evicted (`history_complete_since`); after an edge
  or Telephony restart the answer is `Unknown`, and the leg stays UNKNOWN until evidence or
  operator resolution — it never fails over.
* Trunk credentials and peer addresses live only in the edge's secret-injected config
  (`/run/secrets/edge/trunks/<peer>.conf`); Telephony stores references.

### The deterministic adapter

`DETERMINISTIC_TEST` is a scriptable fake for the contract suite and the race / failure
tests. It is refused by the registry outside the `local` / `test` runtime environments and
is never described as a carrier.

## Acceptance findings

| Check | Result |
|---|---|
| Contract suite, deterministic adapter | PASS |
| Contract suite, SIP-trunk adapter over a scripted edge | PASS (found and fixed: an edge-history "ended" channel was reported `Active`; absence was claimed without evidence covering the transmission) |
| SIP-trunk adapter over the real Asterisk edge with SIPp carriers | PASS (`sip_certification.rs`, ADR 0015) |
| **Real carrier** (lawful interconnect, real number, real PSTN) | **NOT RUN — BLOCKED** |

## Blocker: real lawful telecom interconnect certification

No lawful interconnect agreement, trunk credentials, authorized caller-ID range or
operator-owned test line exist in this environment. `tests/real_interconnect_certification.rs`
is the gate: it runs a real outbound call (ringing, answer, RTP received, BYE) through a
configured trunk and is reported **NOT RUN** without that configuration
(`TELEPHONY_REAL_CERT_REQUIRED=1` makes that a failure). Until it passes against a real
carrier, Part E production certification is **partial**: every public-network flag stays
off by default, and no route may be enabled in production.

Steps to certify once an interconnect exists: register provider / sip-peer / route /
number (disabled), deploy the trunk secret, run the real certification and the inbound
check (a call from a real handset to the relay number), verify CDRs against
`telephony_usage_records`, then enable the route for an internal tenant.
