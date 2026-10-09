# ADR 0021: Business hours and forwarding on one Call

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §58 (§77–§88). Implemented in
  `services/calling/src/enterprise/{hours,forwarding}.rs` (configuration and pure
  evaluation), `services/calling/src/calls/app/enterprise/{router,forward}.rs` (routing,
  public forwarding execution) and Calling migration 000026. Builds on ADR 0013 (routing
  engine), ADR 0014 / 0016 (Telephony, real-carrier certification pending), ADR 0010 /
  0011 (Privacy Relay, Contact Vault) and ADR 0018–0020.

## Context

Enterprises route by time of day ("after 17:00 go to the after-hours IVR"), close on
holidays and forward people's calls ("if I don't answer, ring my deputy"; "after hours,
ring the on-call mobile"). The risks: hand-written DST arithmetic, schedule logic copied
into the media edge, forwarding loops, and above all a forward to a public number that
skips the paid-call and privacy controls of Part E because "an administrator configured
it".

## Decision

### Business hours

* A schedule has an **IANA time zone** and is evaluated by `chrono-tz` (the IANA
  database). Calling never adds offsets itself: an instant is converted to local wall
  time, so the skipped spring hour and the repeated autumn hour, southern-hemisphere
  rules and historical offset changes are the library's job.
* Weekly windows are local `HH:MM` ranges per ISO day; an end before the start crosses
  midnight. Overlaps are rejected on the 7-day circle (including Sunday → Monday).
* Exception dates are CLOSED or CUSTOM_HOURS and govern their whole local date: neither
  the date's weekly windows nor a previous day's window crossing midnight apply on it, and
  an exception date's own window never spills into the next day. No global holiday API.
* Evaluation is pure and deterministic: `(schedule version, instant)` → OPEN / CLOSED,
  a bounded reason, the local date / time / offset used, and the selected route. It is
  exposed as `POST /v1/enterprise/business-hours/{id}/evaluation` and is what the router
  uses (`SCHEDULE` destination, IVR `ROUTE_SCHEDULE`). Nothing is evaluated in Asterisk.
* Open routes: member / principal, extension, ring group, queue, IVR. Closed routes: the
  same plus voicemail and a registered public contact (where the policy permits).

### Forwarding

* Typed rules per principal and condition: **ALWAYS** (before ringing), **BUSY** (every
  offer declined), **NO_ANSWER** (offers expired), **UNAVAILABLE** (no eligible endpoint).
  Targets are typed destinations of the scope; a public target is a registered
  **public contact** (opaque Privacy Relay ContactReference + the Privacy Relay Session
  covering it) — never a number.
* Every forward continues the SAME canonical Call (new ring offers, or a public leg).
* **Loops are impossible**: the router adds every configuration node AND every principal
  it reaches to the Call's visited set; following a node or a forwarding rule costs one
  hop of the policy's budget (≤ 16; visited ≤ 32 by a database CHECK). Reaching anything
  twice is `NO_ROUTE / LOOP_BLOCKED`; running out is `NO_ROUTE / HOP_BUDGET_EXHAUSTED`.
  A blocked BUSY / NO_ANSWER forward ends the Call with its ordinary outcome. Obvious
  ALWAYS cycles between principals are also refused at configuration time.
* The enterprise policy gates forwarding (`forwardingAllowed`) and public forwarding
  (`publicForwardingAllowed`, which needs `externalPublicCallingAllowed` and the
  operator-granted `MANAGE_PUBLIC_FORWARDING` capability to configure).

### Public forwarding passes every Part E gate

When routing reaches a public contact the Call waits (phase FORWARDED, no public leg);
after commit (and from the restart-safe worker) Calling runs, outside any lock:

1. enterprise policy (forwarding, public forwarding, external calling) and the
   enterprise's destination-country list;
2. Privacy Relay's **current** authority, acting for the application that placed the
   Call, for the caller towards the contact over PUBLIC_RELAY (fail closed when
   unreachable), with a VERIFIED contact;
3. the environment's routing engine with every Part E input — route classes, country
   policy, public-route flags, live-telecom permission, Telephony route availability and
   spend headroom;
4. the relay identity.

Only then, in one transaction under the Call lock: the Call is bound to the Privacy Relay
context / session (set-once columns, so Part D active-call enforcement reaches it), a
route plan, the public leg, its attempt and CALLEE party, a durable spend reservation
(limits and concurrency) and the ORIGINATE command. Any refusal records
`NO_ROUTE / <bounded reason>` and fails the Call safely; the kill switch refuses new
public forwards. The public leg is bridged to the caller's RTC media exactly like an
APP_TO_PSTN call (a SIP-device caller cannot be forwarded publicly in this deployment).

Public forwarding is fully implemented and exercised against a simulated Telephony and
Privacy Relay. It is **not** certified against a real carrier: real PSTN certification
remains the Part E external blocker (ADR 0016), and no feature flag was turned on.

## Consequences

* Time-zone correctness rests on a maintained database, not on our arithmetic; updates
  arrive with the crate.
* Forwarding cannot create unbounded routing work or a second Call.
* A configured public forward is never more privileged than a user-placed public call.
