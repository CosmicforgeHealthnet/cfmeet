# Enterprise Calling & Continuity — implementation map (Part F §52–§62)

Baseline: `staging` at `f28bdb92977747def4f2ac56b02bb59770359071` (Part E complete; its one
external item — real lawful telecom interconnect certification — stays pending, ADR 0016).

This map records what already exists, what Part F adds and where. It is the Stage 0
result of the Part F implementation; ADRs 0018–0025 hold the decisions.

## 1. What already exists (audited, reused — never re-implemented)

| Capability | Where | Part F use |
|---|---|---|
| Canonical Call, participants, sessions, legs, routes, attempts, authorizations, events + transactional outbox, optimistic versions, 17-state machine | `services/calling/src/calls/` (ADR 0006) | The ONLY Call model. Enterprise calls are ordinary Calls; no `enterprise_calls` / `pbx_calls` table |
| Endpoints, invitations, server-time ring expiry, per-participant single-winner arbitration (`uq_call_answer_claims_participant`), ANSWERED_ELSEWHERE, decline / no-answer derivation, recovery API | `calls/app/ringing.rs`, `calls/service/invitations.rs` (ADR 0008) | Ring groups and queue offers ring through it; Part F adds a **group-level** answer claim on top |
| Call control: cancel / end / hold / resume / transfer (TRANSFERRING, same Call id, failure recovery), `expectedVersion`, `Idempotency-Key` | `calls/app/control.rs`, `calls/app/transfer.rs` (ADR 0009) | Extended with enterprise transfer TARGETS (extension, ring group, queue) and enterprise policy; no parallel enterprise control |
| Routing Engine with `ENTERPRISE_INTERNAL` (representable, not executable until now) | `routing/` (ADR 0013) | Made executable for internal-only enterprise calls (PRIVATE_NETWORK route class, no paid leg) |
| Telephony core, SIP / media edge (Asterisk 20 + ARI): legs, durable commands, DTMF / hold / redirect / bridge primitives, normalized events, reconciliation | `services/telephony/` (ADR 0014–0017) | Extended with neutral enterprise primitives: internal SIP device dialogs, answer, prompt playback, DTMF forwarding, media capture + transient capture content |
| Public-network execution: ContactReference + Contact Resolution Grant, spend reservations, Privacy Relay authority, failover | `calls/app/public/`, `telephony/spend.rs` | Public forwarding reuses it unchanged (no bypass) |
| Tenant / application / environment trust model, scopes registry, principals (incl. `ENTERPRISE_MEMBER`) | `platform/` (ADR 0003) | The enterprise security boundary (§7 of the brief): no new "organization" authority |
| Object storage | Only Meeting Service (`services/meeting-service/src/services/artifacts/storage.rs`) | NOT imported. A neutral `packages/object-storage` is introduced (ADR 0023); Meeting Service unchanged |

## 2. Organization scope

`Tenant` is the organization security boundary. Every enterprise resource is scoped by
`(tenant_id, application_id, environment_id)` with composite foreign keys to
`platform_environments`; ownership columns are immutable (trigger). The extension
namespace is exactly that triple. No overlapping organization identity is created
(ADR 0018).

## 3. Code placement

```text
services/calling/src/enterprise/          configuration domain (HTTP-independent service)
├── mod.rs           EnterpriseService, scope, versioning, admin audit, feature flags
├── policy.rs        typed EnterpriseCallPolicy (§18)
├── departments.rs   departments + membership (§53, §145)
├── destinations.rs  typed destination references, scope validation
├── extensions.rs    extensions, number format / namespace (§54)
├── ring_groups.rs   ring groups + members (§55)
├── queues.rs        queues + members (§56 configuration)
├── ivr.rs           typed IVR graph, validation, versions, activation (§57)
├── business_hours.rs IANA-timezone schedules, windows, exceptions, evaluation (§58)
├── forwarding.rs    typed forwarding rules (§58)
├── voicemail.rs     mailboxes + message metadata (§60)
├── recording.rs     call-recording policies (§61)
└── http.rs          /v1/enterprise/** admin and user API

services/calling/src/calls/app/enterprise/  runtime orchestration of enterprise CALLS
├── mod.rs           entry (targetExtension), enterprise call context, hop budget
├── router.rs        destination resolution → ring / queue / IVR / voicemail / forward
├── ring_groups.rs   simultaneous / ordered stepping on ring-round exhaustion
├── queues.rs        queue entries, FIFO offers, timeouts, overflow, abandonment
├── ivr.rs           IVR session engine (RTC keypad + Telephony DTMF)
├── voicemail.rs     capture, finalization worker, retention worker
├── recording.rs     recording authorization, consent, capture, finalizer
├── sip.rs           enterprise SIP device dialogs offered by Telephony
└── workers.rs       durable, multi-instance (SKIP LOCKED) enterprise workers

packages/object-storage/   neutral protected object storage (S3 SigV4 + memory backend)
```

## 4. Schema families (Calling's forward-only history, migrations 000020+)

| Stage | Tables |
|---|---|
| 1 | `enterprise_call_policies`, `enterprise_departments`, `enterprise_department_members`, `enterprise_admin_audit` |
| 2 | `enterprise_extensions`, `enterprise_call_contexts` (per-Call enterprise routing state, FK to `calls`), `enterprise_routing_steps`; `call_invitations.answer_group_id` + `call_answer_group_claims` |
| 3 | `enterprise_ring_groups`, `enterprise_ring_group_members` |
| 4 | `enterprise_call_queues`, `enterprise_queue_members`, `enterprise_queue_entries`, `enterprise_queue_offers` |
| 5 | `enterprise_ivr_flows`, `enterprise_ivr_flow_versions` (immutable), `enterprise_ivr_sessions` |
| 6 | `enterprise_business_hours_rules`, `…_windows`, `…_exceptions`, `enterprise_forwarding_rules` |
| 8 | `voicemail_mailboxes`, `voicemail_messages` |
| 9 | `call_recording_policies`, `call_recording_authorizations`, `call_recording_consents`, `call_recordings`, `call_recording_access_audit` (ADR 0024) |
| 10 | `call_participants.call_created_at` (trigger-stamped, immutable), `idx_call_participants_history`, `idx_enterprise_routing_steps_destination` — history is derived, never stored (ADR 0025) |

Polymorphic destinations (`EXTENSION`, `PRINCIPAL`, `RING_GROUP`, `QUEUE`, `IVR`,
`VOICEMAIL`, `SCHEDULE`, `PUBLIC_CONTACT`) are `(type, id)` pairs validated by one
database trigger function (`enterprise_check_destination`) that proves the target exists
in the SAME tenant / application / environment — a cross-tenant destination cannot be
written even by a buggy service. `PUBLIC_CONTACT` is an opaque ContactReference (Privacy
Relay-owned; never a foreign key, never a number). Config rows are never deleted
(disable only), so historical Calls keep their references.

## 5. Runtime model (one logical Call)

* Entry: `POST /v1/calls` with `targetExtension` (scope `calls.write` + `enterprise.calls.use`,
  flag `ENTERPRISE_CALLING_ENABLED`) or an enterprise SIP device dialling an extension
  (offered by Telephony). Calling authorizes (`ENTERPRISE_INTERNAL_V1` policy), resolves
  the extension in the trusted scope, records an `ENTERPRISE_INTERNAL` route plan and
  ONE canonical Call.
* `enterprise_call_contexts` holds the entry extension snapshot (number / label), the
  current destination, hop count and the visited set; `enterprise_routing_steps` is the
  append-only routing evidence. Hop budget (policy, default 8) + visited set make loops
  impossible.
* While the enterprise system routes (ring-group stepping, queue wait, IVR, voicemail
  greeting / capture) the Call is pre-answer (`RINGING`); a member / agent answer moves it
  to `ANSWERED` → `CONNECTED` on media evidence. Voicemail ends the Call `NO_ANSWER` with
  the message linked; a caller hang-up while waiting is `CANCELLED` (queue entry
  `ABANDONED`).
* Group single winner: invitations of a ring-group step / queue offer carry an
  `answer_group_id`; `call_answer_group_claims` (PRIMARY KEY `(call_id, answer_group_id)`)
  admits exactly one answering participant per group, under the existing Call row lock.
  Losers get `ANSWERED_ELSEWHERE` and their participants are retired.
* Ordered ring groups and queue offer timeouts ride the existing durable invitation
  expiry worker: when a ring round has no live offer, the enterprise router advances
  BEFORE the ordinary DECLINED / NO_ANSWER derivation.
* Queue workers, IVR timers, voicemail / recording finalizers and retention run on every
  instance with `FOR UPDATE SKIP LOCKED` and persisted deadlines (restart-safe).
* History (§62, `calls/app/history.rs`, ADR 0025): each principal's Calls with the
  participant outcome (ANSWERED / DECLINED / ANSWERED_ELSEWHERE / RINGING / MISSED / …)
  derived at read time from offers on every device, sessions and the Call state — never
  from push or realtime delivery; `GET /v1/calls?principalId=` and the endpoint's
  `GET /v1/calling/history`.

## 6. Telephony additions (execution only)

Calling decides; Telephony executes provider-neutral commands:

* enterprise SIP devices (`telephony_enterprise_devices`: opaque device reference ↔ edge
  peer reference) — an internal dialog from a known device is offered to Calling
  (`POST /internal/v1/telephony/enterprise-calls`), never a carrier, never spend;
* `answer`, `prompt` (built-in prompts only), DTMF forwarding as normalized
  `DTMF_RECEIVED` events (digit never logged / persisted by either side; Calling
  persists only the selected IVR branch);
* media capture of an enterprise leg (`CAPTURE`, `telephony_captures`): the edge records
  into its transient spool; Telephony pushes the finished audio (size / duration /
  SHA-256) to Calling (`POST /internal/v1/telephony/captures`, permission
  `telephony-capture.deliver`) and deletes the spool copy only after Calling's 200
  (retry with backoff; dead-letter + incident, spool still deleted) — ADR 0023.

Calling uploads artifacts to protected object storage; Telephony never holds storage
credentials.

## 7. Flags (default `false`; kill switches stop NEW effects only)

`ENTERPRISE_CALLING_ENABLED`, `VOICEMAIL_ENABLED`, `CALL_RECORDING_ENABLED`. Never
blocked: call termination, disabling extensions / forwarding, finalizing captured
artifacts, deleting recordings / voicemail, retention cleanup.

## 8. Scopes

`enterprise.config.read`, `enterprise.config.write`, `enterprise.calls.use`,
`enterprise.voicemail.read`, `enterprise.voicemail.manage`, `enterprise.recording.manage`
— registered, never granted implicitly.

## 9. Certification plan

| Area | Evidence |
|---|---|
| Extension A → B, ring groups, hold / resume, transfer | PostgreSQL suites + real LiveKit / WebRTC (`rtc_certification`) |
| IVR, voicemail, recording capture, SIP internal leg | real Asterisk + SIPp (DTMF RFC 4733, RTP), real Telephony + Calling |
| Object storage | S3-compatible server (MinIO in CI; moto locally) with SigV4 |
| Public forwarding | Part E simulated adapter only — real carrier certification remains the Part E blocker |
