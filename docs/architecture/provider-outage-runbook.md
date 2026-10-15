# Provider / carrier outage runbook

Operator procedure for an outage or severe degradation of a telecom interconnect
(carrier / provider), a Telephony route, or the CF-owned SIP / media edge. It restates,
as steps, the behaviour certified in Part I §88 and ADR 0017; it introduces no new
mechanism. Application (app-to-app RTC) calling never depends on Telephony and keeps
working throughout.

Status note: no lawful carrier interconnect is certified yet (ADR 0016). Until one is,
this runbook has been exercised against the deterministic adapter and the CF-owned edge
only (`failover`, `public_failover`, `sip_edge_outage`, `reconciliation`,
`operator_admin`, `degradation_matrix` suites).

## 1. Detect

| Signal | Where |
|---|---|
| `PROVIDER_CIRCUIT_OPENED` incident (5 consecutive definite provider-side failures on a route or provider) | `telephony-admin incidents` |
| `UNKNOWN_ORIGINATION_OUTCOME` / `UNKNOWN_TERMINATION_OUTCOME` / `UNKNOWN_CONTROL_OUTCOME` incidents | `telephony-admin incidents` |
| `SIP_EDGE_UNAVAILABLE` incident (provider health refresh saw the edge degraded / unavailable) | `telephony-admin incidents` |
| `CALLBACK_AUTH_FAILURES`, `PROVIDER_EVENT_GAP` incidents (callbacks failing or missing) | `telephony-admin incidents` |
| `REPEATED_ROUTE_DENIALS` incident (Calling refusing public routes) | `platform_admin incidents list --status open` |
| `telephony_provider_circuits{state}`, `telephony_route_circuits{state}`, `telephony_circuit_opened_total{scope}`, `telephony_sip_edge_health{state}`, `telephony_provider_responses_total{operation,class}`, `telephony_sip_responses_total{class}`, `telephony_command_queue{state="UNKNOWN_OUTCOME"}` | Telephony metrics listener (`TELEPHONY_METRICS_ADDRESS`) |
| Route / provider `health` and `circuit_state` | `telephony-admin route list`, `telephony-admin provider list` |

Metric labels are enums only; no number, Call, tenant or provider event id appears in
them (Part I §84). Incident evidence carries no telephone number.

## 2. Contain

Choose the narrowest control that stops new paid legs on the failing part. Every
mutating command names its operator (`--operator <id>` or `CF_OPERATOR_ID`) and writes
one operator audit row in the same transaction as its effect.

| Scope | Command / switch | Effect |
|---|---|---|
| One route | `telephony-admin --operator <id> route disable <route-id>` | Lasting stop of new legs on that route until `route enable` |
| One interconnect | `telephony-admin --operator <id> provider disable <provider-id>` | Lasting stop of new legs on every route of that provider |
| Short hold, then automatic probe | `telephony-admin --operator <id> provider circuit <provider-id> OPEN` (or `route circuit <route-id> OPEN`) | Circuit breaker hold: after 60 s one probe origination is allowed (`HALF_OPEN`); success closes it, failure re-opens it |
| All public outbound | `CALLING_PUBLIC_OUTBOUND_ENABLED=false` (Calling) and / or `PUBLIC_OUTBOUND_CALLING_ENABLED=false` (Telephony) | New app-to-phone and public outbound legs refused (`PUBLIC_ROUTE_DENIED`) |
| Public inbound | `CALLING_PUBLIC_INBOUND_ENABLED=false` / `PUBLIC_INBOUND_CALLING_ENABLED=false` | New phone-to-app / phone-to-phone offers refused |
| Callback relay | `CALLING_CALLBACK_RELAY_ENABLED=false` / `CALLBACK_RELAY_ENABLED=false` | New callback relays refused |
| All Telephony | `TELEPHONY_ENABLED=false` | Every new paid effect refused |

What containment never does:

* It never ends or orphans live Calls. A disabled provider's callbacks are still
  authenticated and applied to its in-flight legs; TERMINATE, reconciliation, usage
  finalization and cleanup never consult a switch (`feature_rollback::*`,
  `provider_events`).
* It never changes a public contract: consumers see the same provider-neutral refusals.
* It never touches Calling, Privacy Relay or application RTC state.

With more than one eligible interconnect, Telephony steers new legs to healthy routes by
itself (circuit, health, capacity), and Calling fails over a Call only after a
**definite** provider-side failure, within the routing policy's attempt budget (ADR 0017).
Disabling the failing interconnect makes that steering immediate instead of waiting for
the circuit threshold.

## 3. Resolve uncertain legs — never retry by hand

A leg whose outcome is UNKNOWN may be a live paid call. Do **not** re-originate it,
re-queue its command or edit its row: a database trigger refuses to put a transmitted
command back to `PENDING`, and Telephony refuses a second execution to the same party of
the same Call while one is live or UNKNOWN.

* The reconciler reads each UNKNOWN leg back from the provider. A correlated event or a
  found call binds the leg; proven absence makes it a definite failure (which may then
  fail over); an inconclusive read-back keeps it UNKNOWN with an escalating incident.
* A terminate the provider refused, or whose outcome is unknown, holds the leg in
  `ENDING` and is re-sent idempotently with backoff until the provider confirms.
* While the provider is unreachable these legs stay UNKNOWN / ENDING. That is the
  intended safe state: nothing is re-sent and no usage fact is invented.
* After the provider is back, watch `telephony_command_queue{state="UNKNOWN_OUTCOME"}`
  drain to zero and the UNKNOWN incidents stop recurring.

## 4. Recover

1. Confirm the provider / edge is healthy again: `telephony-admin provider list` and
   `route list` show `health` HEALTHY (or DEGRADED) and the circuit CLOSED, and
   `SIP_EDGE_UNAVAILABLE` has resolved itself on a healthy refresh.
2. Re-enable what was disabled, narrowest first:
   `telephony-admin --operator <id> provider enable <provider-id>`, then
   `route enable <route-id>`; restore any switch changed in §2.
3. Watch the first originations (`telephony_provider_responses_total`,
   `telephony_circuit_opened_total`). A failing probe re-opens the circuit, and that is
   correct behaviour.
4. Resolve the incidents whose condition is handled:
   `telephony-admin --operator <id> incidents resolve <incident-id> --note "<short note>"`
   (and `platform_admin incidents resolve --incident <id>` for Calling's). A repeat of
   the condition opens a new incident.

## 5. Record

Record the timeline, the operator audit rows written in §2 and §4, the incidents
resolved, and any leg still UNKNOWN after recovery (escalate it to the carrier with the
provider-side reference held in Telephony's restricted provider bindings, never a
customer number in a ticket).

## References

* ADR 0016 (first interconnect, real-carrier certification pending), ADR 0017 (failover,
  circuit breaker, unknown outcome), ADR 0031 (incidents, metrics, kill switches).
* `services/telephony/README.md` (admin CLI, metrics, incident categories),
  `services/calling/README.md` §95 (Calling switches), `infra/telephony/README.md`
  (SIP / media edge planes).
* Part I §86–§88: [`part-i-reliability-certification.md`](part-i-reliability-certification.md).
