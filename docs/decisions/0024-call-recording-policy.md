# ADR 0024: Call recording is a separate, evidence-based authority on the canonical Call

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §61 (§115–§129). Implemented in
  `services/calling/src/enterprise/recording.rs` (policies, views, access audit),
  `services/calling/src/calls/app/enterprise/recording.rs` (decisions, consent,
  start / stop, finalization, worker), Telephony's capture path
  (`services/telephony/src/capture.rs`, CAPTURE purpose RECORDING, CAPTURE_STOP) and
  migrations `calling 000028` / `telephony 000009`. Builds on ADR 0018 and ADR 0023
  (protected storage, capture delivery).

## Context

Recording a call is regulated differently by jurisdiction, organization and purpose.
The right to place or join a Call says nothing about the right to record it, and a
client claiming "everyone consented" proves nothing. Meeting recording has its own
policy in another service; reusing it would couple services and conflate purposes.

## Decision

### A separate typed policy

`call_recording_policies` (one ACTIVE default per scope, versioned, audited, never
deleted): `mode` DISABLED / MANUAL_ALLOWED / AUTO_WHEN_AUTHORIZED (automatic recording is
never assumed), `noticeRequired`, `consentParticipants` NONE / ALL_PARTICIPANTS /
OTHER_PARTICIPANTS, an opaque `jurisdictionRef` (compliance configuration is
authoritative input; the platform never infers recording law from a country, and a
reference that looks like a number is refused), `retentionDays`, `maxRecordingSeconds`
(≤ 1440: one WAV delivery of ≤ 24 MiB). Managing policies needs
`enterprise.recording.manage` AND the operator-granted MANAGE_RECORDING capability.
Recording authority is independent of the right to call, enterprise membership, Privacy
Relay authorization and any meeting recording configuration.

### Server-authoritative decisions

Every request (a participant through the application, a recording administrator, or the
system for AUTO) first writes an immutable `call_recording_authorizations` row: Call,
policy and version, mode, jurisdiction reference, actor, required participants, notice
evidence, consent evidence, ALLOWED / DENIED and a bounded reason. The checks, in order:
kill switch `CALL_RECORDING_ENABLED` (default off; NEW recordings only), an enterprise
Call, the ACTIVE default policy and its mode, CONNECTED, the actor (a principal must be
a participant), notice, consent, protected storage, a capture path. A denial is 403
`RECORDING_NOT_AUTHORIZED`; the decision and its reason are readable by participants and
administrators. A Call already being recorded returns the running recording (idempotent;
no new decision). The Call itself is never affected by a recording decision.

### Notice and consent evidence

* **Notice** is issued by the server, never asserted by a client: a realtime
  `call.recording_started` signal to every application participant endpoint and, for an
  enterprise SIP device's leg, a tone (built-in BEEP prompt) at the edge before the
  capture. A participant the server cannot reach makes the decision
  NOTICE_UNDELIVERABLE.
* **Consent** is each required participant's OWN decision for ONE Call, recorded through
  that participant's endpoint (an endpoint with a session of an active participant of
  this Call; anything else is 404). Consents are append-only; the latest per participant
  (by sequence) counts; a replayed decision is idempotent; a consent on another Call never
  counts. There is no "everyone consented" input. A participant with no consent channel
  (a SIP desk phone today) cannot satisfy a consent requirement — the decision is DENIED,
  never assumed. A DECLINED by a participant a running recording relies on stops it
  (CONSENT_WITHDRAWN).
* AUTO_WHEN_AUTHORIZED: the worker attempts a system decision for CONNECTED enterprise
  Calls once per consent change (a consent watermark on each decision), and the consent
  endpoint attempts immediately; never more than one recording per Call.

### Execution and artifacts

* SIP: Calling enqueues the BEEP notice and a provider-neutral CAPTURE (purpose
  RECORDING) for the device leg; Telephony records the leg's BRIDGE (the mixed call
  audio) when it is bridged, else its channel, into the transient spool; CAPTURE_STOP
  stops it early (never blocked by a kill switch). Delivery, integrity checks, protected
  storage, spool deletion, retry / dead-letter are those of ADR 0023.
* RTC: the neutral `CallMediaCapture` interface (`start` / `stop`); the default
  deployment has no RTC capture adapter, so an application-only Call is DENIED
  CAPTURE_UNAVAILABLE — never a fake recording.
* `call_recordings`: STARTING → RECORDING → STOPPING → PROCESSING → READY (or FAILED) →
  DELETING → DELETED (trigger-guarded), one active per Call, stop reasons EXPLICIT /
  CALL_ENDED / POLICY_REVOKED / CONSENT_WITHDRAWN / CAPTURE_FAILED / MAX_DURATION. Events
  RECORDING_STARTED / STOPPED / READY / FAILED / DELETED (opaque ids and bounded reasons
  only) through the outbox.
* Stops: explicit (participant / administrator), Call end (worker), policy disable or a
  stricter change (POLICY_REVOKED), consent withdrawal, maximum length, capture failure.

### Access and retention

Participants of the recorded Call (through the application) and recording
administrators may read metadata, get a short-lived (≤ 300 s) signed read of the one
object (minted after authorization, never stored), and delete. Every attempt — granted
or denied — is written to the append-only `call_recording_access_audit` (actor, recording,
Call, operation, outcome, reason, request / correlation, time). Retention moves READY
recordings past the policy's days to DELETING as the system; deletion is asynchronous and
idempotent. Recording retention is never tied to any meeting configuration.

## Certification

* `tests/enterprise_recording.rs`: policy administration and capability, every denial
  reason, consent ownership / binding / replay / withdrawal, idempotent start, Call end,
  protected artifacts, cross-tenant 404, audited access incl. denials, revocation, maximum
  length, storage outage, capture failure, AUTO only with evidence.
* Telephony `tests/enterprise_capture.rs`: RECORDING bounds, bridge vs channel capture,
  CAPTURE_STOP under kill switches.
* Real stack `tests/enterprise_sip_certification.rs` scenario 6: a SIPp desk phone and a
  SIPp application-media side bridged at a real Asterisk edge; DENIED without consent;
  ALLOWED with edge tone + app notice; the bridge's mixed audio recorded; explicit stop
  via CAPTURE_STOP; stored in a real S3-compatible bucket; signed retrieval of the same
  WAV; access audited.

## Consequences

* Consent from SIP desk phones (e.g. keypad consent) and an RTC capture adapter (media
  egress) are not implemented; both are recorded as deferred items. Policies requiring
  their consent deny recording rather than assume it.
* Recordings longer than 24 minutes need a chunked delivery path (not offered).
