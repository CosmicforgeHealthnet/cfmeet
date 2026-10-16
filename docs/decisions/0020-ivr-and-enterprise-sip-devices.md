# ADR 0020: Declarative IVR on one Call; enterprise SIP devices on the CF Meet edge

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §57 (IVR) and the enterprise SIP device path it needs (§54,
  §179). Implemented in `services/calling/src/enterprise/{ivr,sip_devices}.rs`
  (configuration), `services/calling/src/calls/app/enterprise/{ivr,sip}.rs` (runtime),
  Calling migrations 000024 (IVR) and 000025 (SIP device bindings, ANSWER / PROMPT
  commands), Telephony migration 000007 (enterprise devices, ENTERPRISE_INTERNAL legs)
  and `services/telephony/src/enterprise.rs`. Builds on ADR 0014 (Telephony core),
  ADR 0015 (Asterisk edge) and ADR 0018 (enterprise calling architecture).

## Context

An IVR answers a caller, plays prompts and routes on keys. The common designs are
unacceptable here: dialplan fragments or scripts stored by tenants (code execution on
the media edge), a second "IVR call" record, digits written to logs or tables, or a flow
that changes under a caller mid-call. The callers are both RTC endpoints (application
keypad) and desk phones registered on CF Meet's own SIP edge, which Part E built only
for public-network legs.

## Decision

### A typed, versioned, declarative graph

* A flow version is a JSON graph of typed nodes only: `PROMPT`, `MENU`,
  `ROUTE_EXTENSION`, `ROUTE_RING_GROUP`, `ROUTE_QUEUE`, `ROUTE_VOICEMAIL`,
  `ROUTE_SCHEDULE`, `HANGUP` (`deny_unknown_fields`: a `dialplan`, `script` or URL
  field is a validation error). Graph validation on publish: unique node ids, every
  reference resolves, the start node exists, every node is reachable, menu bounds
  (1-12 single-key distinct options, input timeout 2-60 s, retries 0-5, explicit
  `onInvalid` / `onTimeout`), no prompt-only cycle, at most 32 nodes, ≤ 64 KiB.
* Route targets are guarded foreign keys of the scope (`enterprise_ivr_flow_destinations`),
  so a flow can never reach another tenant's resource and a referenced resource cannot
  be deleted under it.
* Versions are append-only; activation is an optimistic-concurrency pointer change.
  A session pins `flow_version_id` at entry: activating a newer version never changes a
  running Call.

### One session per Call, bounded

* `enterprise_ivr_sessions` holds the business state (current node, retries, steps,
  deadline, the selected branch as a node id). The Call stays the canonical Call
  (RINGING, enterprise phase IVR); a ROUTE_* node hands the SAME Call to the router.
* Every session is bounded by `MAX_IVR_STEPS` (48): a menu cycle fails safe
  (`STEP_BUDGET_EXHAUSTED`, the Call FAILED with a routing failure) instead of looping.
* Timers are rows (`input_deadline`), served by the restart-safe expiry worker under the
  Call row lock with SKIP LOCKED — never an in-process timer.
* Keys are used once, inside the transaction that decides, and never stored, logged or
  used as metric labels. Metrics carry only VALID / INVALID and the input channel.

### Built-in prompts with nominal lengths

Prompts are a controlled built-in catalog (`WELCOME`, `MENU`, `INVALID_OPTION`, …).
Calling never names a sound file; Telephony maps each prompt to edge-local audio and an
RTC client plays its own copy. Each prompt has a nominal length that the media plane's
audio must fit (checked in Telephony's tests against the installed sounds). The menu's
input timer starts after the prompts it queued; a HANGUP node's prompt plays for its
nominal length (the session stays ACTIVE without awaiting input) before the platform
ends the Call, and a caller who hangs up during it was served (`COMPLETED_IN_IVR`), not
abandoned. Customer-provided prompt audio is not accepted by this deployment (no audio
in the Calling database); it would arrive as a protected artifact reference later.

### Enterprise SIP devices

* Telephony owns the edge side: `telephony_enterprise_devices` maps an opaque device
  reference (`dev-…`) to the edge endpoint the device authenticates as, and to the
  scope whose Calling receives its dialogs. Calling owns the business side:
  `enterprise_sip_devices` binds the same reference to a principal. Calling never sees
  the device's SIP identity, address or credentials; Telephony never learns the
  principal.
* A dialog from a device arrives in Stasis with the `enterprise` argument. Telephony
  creates an `ENTERPRISE_INTERNAL` leg (no provider, route, spend, protected party or
  usage fact — a CHECK enforces it), binds its channel directly, and offers the dialog
  to Calling (`POST /internal/v1/telephony/enterprise-calls`, permission
  `telephony-enterprise.offer`). Calling creates the canonical Call (origin
  `ENTERPRISE_DEVICE`) as the bound principal, with a `PRIVATE_NETWORK` caller leg
  bound to the Telephony leg, and routes it like any enterprise Call.
* Calling drives the edge through its durable telephony command outbox:
  `ANSWER` and `PROMPT` (built-in prompt) join `TERMINATE`, executed by Telephony on
  the bound channel (`telephony-leg.media`). Commands of a leg are executed in creation
  order; a command that reaches Telephony before the leg it targets is committed is
  retried (`LEG_NOT_YET_KNOWN`, bounded), never failed.
* Keypad input (RFC 4733) is forwarded by Telephony to
  `POST /internal/v1/telephony/enterprise-keypad` as it arrives — never persisted or
  logged on either side.
* Rejections (unknown / disabled device, invalid extension, rate limit, Calling
  refusal) are bounded codes; dialogs are rate limited per device.

## Certification

`services/calling/tests/enterprise_sip_certification.rs` (workflow
`enterprise-calling-certification.yml`) runs Calling (served on a real port), the real
Telephony binary with its own database and signed internal HTTP, a real Asterisk edge
(`infra/telephony/certification/`) and SIPp as an enterprise desk phone with RTP and
RFC 4733 keys. Certified: prompt + valid key → extension on the same Call; invalid key +
retry, valid key → queue; timeout + retry → hangup by the platform; explicit hangup
branch (GOODBYE played first); caller hang-up abandonment. Route-to-voicemail is
certified with voicemail capture (§60). Each SIPp key press carries increasing RTP
sequence / timestamp, as a real phone does (the edge ignores an event not newer than
the last one).

No carrier is involved: this certifies internal enterprise SIP only. Real PSTN
certification remains the Part E external blocker (ADR 0016).

## Consequences

* Tenants configure behaviour, never code; the media edge executes a closed vocabulary.
* An IVR never duplicates the Call or the router; voicemail, schedules and queues are
  just destinations.
* Desk phones reuse the Part E edge and command path without touching paid-call safety:
  an internal leg can never reserve spend or reach a provider.
