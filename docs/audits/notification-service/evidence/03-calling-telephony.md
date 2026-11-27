# Evidence 03: Notification behaviour in Calling, Telephony, SMS Gateway, Numbering and Privacy Relay

- **Audit purpose:** an inventory for a future centralized Notification Service.
- **Mode:** read-only. No repository file was changed. No state-changing git command, deployment or production endpoint call was made. No secret values were read or copied.
- **Repository state:** `/home/user/cfmeet` at HEAD `6f3743f806434a55bad8297f104d28a55e350e2e` (2026-11-15). The working tree is clean.
- **Classification vocabulary:**
  - **implemented**: code exists and is tested.
  - **configured**: wired with a real adapter or key, per code or ops docs.
  - **activated**: a flag is on in production, per an ops report.
  - **production-tested**: an ops report cites a production run.
  - **disabled**, **broken**, **deprecated**, **absent**: as named.
- **Rule:** activation is never inferred from code. Activation claims below cite `docs/operations/*` only.

---

## 0. Executive summary

| # | Finding | Classification |
|---|---|---|
| 1 | Calling alerts devices to an incoming call **only through its own realtime signaling WebSocket** (`GET /v1/calling/signaling`). Fan-out uses PostgreSQL `LISTEN/NOTIFY`. Devices recover through `GET /v1/calling/invitations` and `GET /v1/calling/history`. | implemented; production-tested (runs 41580265031, 41581639199) |
| 2 | **No mobile or OS push exists for calls.** No VoIP push, PushKit, CallKit, FCM, APNs or Web Push. Calling builds a transport-neutral `cfmeet.calling.push-invitation` payload and hands it to a `PushDispatcher`. The only production implementation is `NoPushTransport`, which logs at debug level and drops the payload. It is hard-wired in `main.rs:453`. Calling does **not** call Messaging's push pipeline. | push hook implemented; transport **absent**; activation BLOCKED_OWNER_ACTION |
| 3 | First-answer arbitration, "answered elsewhere" cancellation, decline, expiry and ring-group/queue answer-group claims run inside one PostgreSQL transaction under the Call row lock, with DB uniqueness guarantees. Signals are published after commit. | implemented; production-tested |
| 4 | Missed calls are a **derived read model**: SQL at read time. No missed flag, missed-call table or missed-call notification exists. Only the `calling_missed_calls_total` metric and the generic `call.no_answer` / `call.invitation_expired` events exist. | implemented; production-tested ("missed-call history", run 41581639199) |
| 5 | Voicemail deposit raises the durable events `VOICEMAIL_RECEIVED` and `VOICEMAIL_READY` (outbox), which become customer webhooks `voicemail.received/ready`. The **mailbox owner gets no realtime signal, push or email.** Only the *caller* gets `call.voicemail_started`. | events implemented; owner notification **absent** |
| 6 | When a recording becomes available, `RECORDING_READY` goes to the outbox and becomes the `recording.ready` webhook. The `call.recording_started/stopped` realtime notice goes to all participant endpoints, and that notice is a compliance requirement. | implemented; production-tested (recording notice) |
| 7 | Calling has a full **transactional outbox** (`calling_outbox_events`). Every call event commits with its outbox row (deferred constraint trigger). Since 2026-11-12, production publishes to the Developer Platform (`CALLING_PUBLIC_EVENTS_DESTINATION=developer-platform`). Calling's own webhook delivery is **retired** (`CALLING_WEBHOOK_AUTHORITY=retired`, `CALLING_WEBHOOK_DELIVERY_ENABLED=false`). | activated (configuration and grants); end-to-end producer event not driven in acceptance |
| 8 | Operational incidents (`calling_incidents`) are DB-only. They are read through the `platform_admin` CLI. Paging is **NOT CONFIGURED** repo-wide and no Calling alert rules exist. | implemented; alerting absent |
| 9 | Telephony, SMS Gateway, Numbering and Privacy Relay send **no user notifications**. Each has an outbox whose lifecycle events could feed one. All external telecom is **disabled / EXCLUDED_BY_OWNER** in production. | disabled |
| 10 | **No service sends SMS OTPs.** The SMS Gateway's only adapter is `deterministic`, which is refused in staging and production. `SMS_GATEWAY_PROVIDER` is unset in production. Phone verification reports `PROVIDER_BLOCKED`. Meeting-service OTPs are email/Redis codes, not SMS. | absent / disabled |

---

## 1. Calling: incoming call ringing (device alerting)

### 1.1 Mechanism

| Aspect | Evidence |
|---|---|
| Design authority | `docs/decisions/0008-calling-endpoints-ringing-and-answer-arbitration.md`, §"Realtime signals, recovery and push" (lines 85–95) |
| Endpoint registry | `call_endpoints` table, migration `services/calling/migration/src/m20261007_000010_call_endpoints_and_invitations.rs` lines 41–94 |
| Endpoint columns | `endpoint_kind` ∈ WEB/MOBILE/DESKTOP/ENTERPRISE_APP; `presence_state` ONLINE/OFFLINE; `last_seen_at`; `push_target_ref UUID` (nullable, opaque); `status` ACTIVE/REVOKED |
| Endpoint constraints | `ck_call_endpoints_key` refuses phone numbers and tel/sip URIs; `uq_call_endpoints_active_key`; `idx_call_endpoints_principal_active` |
| Ring offers | `call_invitations`, same migration lines 144–223. One row per (Call, participant, endpoint, generation). Statuses PENDING→RINGING→ACCEPTED/DECLINED/ANSWERED_ELSEWHERE/CANCELLED/EXPIRED. Constraints: `uq_call_invitations_one_live_per_endpoint` (partial unique), `idx_call_invitations_live_expiry`, transition / no-delete / immutable triggers |
| Signal contract | `services/calling/src/signaling/signals.rs` (`SIGNAL_SCHEMA = "cfmeet.calling.realtime-signal"`, line 18). `SignalType` enum lines 23–63 (27 types). Contract file: `packages/contracts/calling/realtime/v1.schema.json` |
| Signal transport | `RealtimeSignalBus` trait, `services/calling/src/signaling/bus.rs:65`. `PgNotifySignalBus` uses channel `cfmeet_calling_signals` (`bus.rs:25`), NOTIFY payload ≤ 7000 B (`bus.rs:27`), and `pg_notify` (`bus.rs:251`). Per-endpoint subscriber buffer is 256 (`bus.rs:30`). Wired in `services/calling/src/main.rs:127-128` |
| Delivery channel | `GET /v1/calling/signaling` (WebSocket), `services/calling/src/signaling/ws.rs`. Subprotocol `cfmeet.signaling.v1` (line 34). Close codes 4001 (credential expired, line 37) and 4401 (authority revoked, line 41). Presence refresh every 30 s (line 43). `ping` → `signaling.pong` (line 189). Authority re-checked every 5 s |
| Auth | Signaling credential `cfsig_…`: HS256 with key `CALLING_SIGNALING_SIGNING_KEY`. TTL `CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS`, default 600 s, max 3600 (`signaling/credentials.rs:118,145`). Minted by the backend through `POST /v1/calling/signaling-credentials` (`endpoints/http.rs:46-47`, scope `calls.write`) |
| Where Incoming is emitted | `calls/app/mod.rs:592-621` (`invitation_signal`). Emitters: `create.rs:900` (new call), `enterprise/mod.rs:1324` (enterprise routing), `enterprise/queues.rs:932` (queue agent offer), `transfer.rs:397` (transfer target), `ringing.rs:664` (next ring round / ordered ring group) |
| Commit discipline | `PendingSignals` (`calls/app/mod.rs:490-504`) are collected inside the transaction. `publish()` (`mod.rs:509-518`) runs **after** `txn.commit()`, e.g. `create.rs:911-912`. A failed publish logs `"realtime signal not published (recoverable via API)"` (`mod.rs:512`) and nothing else. No metric is emitted |
| Recovery | `GET /v1/calling/invitations` (`recoverInvitations`) and `GET /v1/calling/history` (`recoverCallHistory`). Both live in `signaling/http.rs:44-62` and use the signaling credential |
| Invitation TTL | `CALLING_INVITATION_TTL_SECONDS`, default 45, range 10–300 (`calls/app/mod.rs:148,186-191`). **Discrepancy:** ADR 0008 line 52 says "≤ 10 min"; code caps at 300 s |
| Expiry worker | `run_workers(1s, 2s)` (`main.rs:503-504`; `workers.rs:390-405`). `expire_due(200)` runs every 1 s on every instance with SKIP LOCKED |
| Endpoint fan-out cap | `CALLING_MAX_ENDPOINTS_PER_PARTICIPANT`, default 10 (`calls/app/mod.rs`). Test `app_ringing.rs:649 past_the_endpoint_cap_the_newest_registrations_ring` |
| Presence | Orders ringing only and never excludes an endpoint (ADR 0008 line 27). Test `app_ringing.rs:297` |

### 1.2 Push path (VoIP / mobile)

| Item | Evidence | Classification |
|---|---|---|
| `PushDispatcher` trait | `services/calling/src/signaling/push.rs:19-22`. "No mobile push vendor is integrated" (line 3) | implemented (interface) |
| `NoPushTransport` | `push.rs:26-33`. Logs `debug!("push-ready offer not dispatched: no push transport is configured")` | the only production implementation |
| Production wiring | `services/calling/src/main.rs:453` `push: Arc::new(NoPushTransport)`. Not configurable: there is no env var for it | **absent** |
| Default in `AppCallService::new` | `calls/app/mod.rs:326`. Override `with_push()` at `mod.rs:387-390` (used only by tests) | — |
| Payload builder | `signals::push_payload` (`signals.rs:186-205`). Schema `cfmeet.calling.push-invitation` v1. Fields: callId, invitationId, callKind, caller.displayName (optional), issuedAt, expiresAt. No phone numbers, tokens or media credentials | implemented |
| Payload contract | `packages/contracts/calling/push/invitation/v1.schema.json`. Counter-examples: `caller-phone-number.json`, `media-credential.json`, `api-credential.json`, `phone-number-as-display-name.json` | implemented |
| Trigger | `push_offer()` (`calls/app/mod.rs:625-645`). Only for `SignalType::Incoming` and only when the endpoint has `push_target_ref` (`mod.rs:610-616`) | implemented |
| `pushTargetRef` contract | `packages/contracts/calling/control/v1.schema.json:1098-1105`: "Opaque reference to a push target held by the application (no push token)" | implemented |
| Who sets `pushTargetRef` | No other service. A repo-wide grep finds `push_target_ref` / `pushTargetRef` only under `services/calling/`. The Communications Hub registers device push through Messaging (`services/communications/src/devices.rs:314-349`), which is **not** linked to Calling endpoints | not integrated |
| Tests | `RecordingPush` fake (`services/calling/tests/common/mod.rs:548-560`). `http_contract.rs:1391 app_calling_responses_signals_and_pushes_validate_against_the_contracts` asserts that push payloads are produced and validate against the schema (lines 1700-1702) | tested with a fake only |
| Ops status | `docs/operations/profile-notifications-sections-9-11.md:165-179`: "Push — EXTERNAL_BLOCKED (CAL-003 …) … Calling's `NoPushTransport` … Activation needs … a Calling `PushDispatcher` reusing Messaging's adapters (APNs VoIP / PushKit for iOS)". `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:31,119`: "Calling has no `PushDispatcher`" → **BLOCKED_OWNER_ACTION**. `cfmeet-final-production-certification.md:171`: `MESSAGING_PUSH_PROVIDER=none` | **absent / disabled** |

**Answer: does Calling use Messaging's push pipeline or its own?** Neither is active. Calling has its own transport-neutral hook (`PushDispatcher`) with no transport behind it. The documented plan (ops doc above) is that a future Calling `PushDispatcher` would reuse Messaging's APNs/FCM/WebPush adapters (`services/messaging/src/realtime/push*.rs`). Those adapters are disabled (`MESSAGING_PUSH_PROVIDER=none`) and certified only against local fakes.

### 1.3 Production evidence for ringing

- `docs/operations/calling-backend-production-2026-11-03.md:38`: "Voice/video, ringing, accept, hold/resume, … reconnection, end | Real production media passed in run 41580265031."
- Line 39: "PASS: incoming ringing/accept/decline, cancellation, missed-call history, three-party real audio, … ring groups, queues, IVR …, internal transfer … Run 41581639199 succeeded."
- Lines 48-52: an early run failed because "abandoned test-account endpoints kept other offers ringing after one device declined". The fix was in the test driver. The behaviour is by design: "A decline on one device must not suppress other live offers."
- Flags (same doc, lines 12-25): `CALLING_NEW_CALLS_ENABLED=true`, `CALLING_APP_CALLING_ENABLED=true`, `ENTERPRISE_CALLING_ENABLED=true`. Re-confirmed in `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:98` (ALREADY_ACTIVE) and `cfmeet-final-production-certification.md:176-179`.
- Note: these runs used realtime signaling with endpoint-scoped signaling credentials. No push was involved.

---

## 2. Calling: first-answer arbitration and "answered elsewhere"

| Aspect | Evidence |
|---|---|
| ADR | 0008 §"Single winner per participant" (lines 56-70) |
| Claim table | `call_answer_claims` (`m20261007_000011_call_answer_arbitration.rs:102-124`): `uq_call_answer_claims_participant UNIQUE (call_id, participant_id)`, `uq_call_answer_claims_invitation`, `uq_call_answer_claims_session` |
| Group claim table | `call_answer_group_claims` (`m20261009_000022_enterprise_ring_groups.rs:118-127`), `PRIMARY KEY (call_id, answer_group_id)`. Index `idx_call_invitations_answer_group` (line 112) |
| Code | `AppCallService::accept_invitation` (`services/calling/src/calls/app/ringing.rs:139-449`). Row lock `lock_call` (line 160). Group claim `claim_answer_group_in` (`enterprise/ring_groups.rs:532`, called at `ringing.rs:295`). Losers are finished with `InvitationStatus::AnsweredElsewhere` in the same txn (`ringing.rs:308-318`) |
| Signals (post-commit) | `call.accepted` to all Call endpoints (`ringing.rs:401`). `call.answered` if the first answer (`ringing.rs:411`). `call.answered_elsewhere` to each losing endpoint, including group losers (`ringing.rs:419-430`). Publish at `ringing.rs:432-433` |
| Late answer | A losing invitation returns `CallError::AnsweredElsewhere` (`ringing.rs:175`). An expired one returns `InvitationExpired` (410) |
| Decline | `decline_invitation` (`ringing.rs:451`). Ends only that endpoint's offer. Signal `call.declined` (`ringing.rs:529`) |
| Revocation cancels offers | `cancel_endpoint_invitations` (`ringing.rs:791`). Signal `call.ring_cancelled` (`ringing.rs:838`) |
| Expiry | `expire_call_invitations` (`ringing.rs:709`). Signal `call.invitation_expired` (`ringing.rs:753`). Ring outcome → NO_ANSWER / DECLINED (`evaluate_ring_outcome_in`, `ringing.rs:565`) |
| Caller cancel | `control.rs:129,276` emits `call.ring_cancelled` to live offers |
| Tests | `tests/app_races.rs:62 concurrent_accepts_have_exactly_one_winner`, `:138 group_arbitration_holds_per_participant_under_contention`, `:207 accept_racing_cancel_leaves_one_consistent_outcome`, `:343 the_single_winner_constraint_is_the_database_guarantee` (mutation test). `tests/app_ringing.rs:361 one_device_decline_declines_only_that_offer`, `:432 offers_expire_on_the_server_and_are_never_revived`, `:492 group_calls_have_one_winning_device_per_participant` |
| Classification | implemented; production-tested (run 41581639199) |

---

## 3. Calling: missed calls and call history

| Aspect | Evidence |
|---|---|
| ADR | `docs/decisions/0025-call-history-and-missed-calls.md`. "No 'missed' flag is stored, and push delivery, realtime signals and client acknowledgements are not inputs" (lines 28-30) |
| Derivation | `services/calling/src/calls/app/history.rs:287-330` (`history_statement_for`): CASE → ANSWERED / DECLINED / ANSWERED_ELSEWHERE / RINGING / **MISSED** (`o.offers > 0`) / NOT_REACHED / IN_PROGRESS. Filter `missed=true` ⇒ `h.outcome = 'MISSED'` (`history.rs:352-354`) |
| Table / index | No new table. Migration `m20261009_000029_call_history.rs` adds `call_participants.call_created_at` (immutable, trigger-stamped) and `idx_call_participants_history (tenant, application, environment, principal, call_created_at DESC, call_id DESC)`, plus `idx_enterprise_routing_steps_destination` |
| APIs | `GET /v1/calls?principalId=&missed=&outcome=&direction=` (`listCalls`, platform credential, `calls.read`; `calls/http.rs:49`). `GET /v1/calling/history` (`recoverCallHistory`, signaling credential; `signaling/http.rs:48`) |
| Metric | `calling_missed_calls_total{call_kind}`, counted once at the terminal transition (`services/calling/src/calls/service/lifecycle.rs:610-631`) |
| Missed-call notification | **Absent.** There is no signal type "missed", no push, no email and no webhook named `call.missed`. A consumer infers it from `call.no_answer` (Call-level), `call.invitation_expired` / `call.ring_cancelled` (per offer), or a history read |
| Tests | `tests/call_history.rs:150 a_missed_call_survives_push_and_realtime_outage`, `:208 one_answered_device_answers_for_the_logical_participant`, `:462 ring_group_members_see_answered_elsewhere_or_missed`, `:601 voicemail_is_linked_for_the_mailbox_owner_only`, `:711 history_reads_one_index_range` |
| Classification | history implemented and production-tested ("missed-call history" PASS, run 41581639199). Missed-call **notification absent** |

**Implication for a Notification Service:** a "you missed a call" notification would need a trigger. It could be derived from the outbox (`CALL_STATE_CHANGED → NO_ANSWER`, or per-participant offers all EXPIRED/CANCELLED) or computed by a history read. Calling currently publishes no per-participant "missed" fact. The public `call.no_answer` event is Call-level and does not cover ring-group members whose turn expired (ADR 0025 rule 5).

---

## 4. Calling: voicemail deposit notification

| Aspect | Evidence |
|---|---|
| ADR | `docs/decisions/0023-protected-object-storage-and-voicemail.md` (events at lines 48, 59, 83) |
| Tables | `voicemail_mailboxes` (`m20261009_000027_voicemail.rs:20-66`): `owner_type` ∈ PRINCIPAL/EXTENSION/QUEUE/RING_GROUP, `owner_id`, `display_label`, `max_message_seconds` 5–600, `retention_days` 1–3650, `status`. **No notification preference or contact columns.** `voicemail_messages` (lines 67+): `state` RECORDING→PROCESSING→READY/FAILED/DELETING→DELETED, `read_state` UNREAD/READ, `received_at`, `ready_at`, `read_at`, `retention_until`, `UNIQUE (call_id)` |
| Durable events | `VOICEMAIL_RECEIVED`: `services/calling/src/calls/app/enterprise/voicemail.rs:315-330` (data: message_id, mailbox_id, capture_source). `VOICEMAIL_READY`: `voicemail.rs:661-680` (message_id, mailbox_id, duration_ms). `VOICEMAIL_DELETED`. All go to `call_events` plus `calling_outbox_events` |
| Public projection | `webhooks/catalog.rs:193-195` → `voicemail.received`, `voicemail.ready`, `voicemail.deleted` (scope `enterprise.voicemail.read`, `catalog.rs:50-52`). Developer Platform catalog mirrors these (`services/developer-platform/src/webhooks/catalog.rs:107-109`) |
| Realtime | `call.voicemail_started` goes to the **caller's** endpoints only (`voicemail.rs:410-428`, `initiator_signal`). Signal doc: "Never the mailbox owner or a message id" (`signals.rs:57-59`). Plus `call.no_answer` (`voicemail.rs:807`) |
| Owner notification | **Absent.** The owner sees the message via `GET /v1/enterprise/voicemail/messages` (`listVoicemailMessages`; `enterprise/http.rs:193`), the history voicemail link (owner only), or a customer webhook |
| Incident | `VOICEMAIL_FINALIZATION_FAILURE` in `calling_incidents` |
| Flags | `VOICEMAIL_ENABLED` (`enterprise/mod.rs:54-84`, unset ⇒ false). Production `true` since 2026-11-03 (`calling-backend-production-2026-11-03.md:17`) |
| Production test | `calling-backend-production-2026-11-03.md:41`: "Voicemail PASS: unanswered extension routes to caller-only real audio capture; owner playback; …; READ state; deletion …; real retention worker deletion." No deposit notification was tested because none exists |

---

## 5. Calling: recording availability

| Aspect | Evidence |
|---|---|
| ADR | `docs/decisions/0024-call-recording-policy.md` (signal at line 51, outbox events at lines 79-81) |
| Tables | `call_recording_policies`, `call_recording_consents`, `call_recording_authorizations`, `call_recordings`, `call_recording_access_audit` (`m20261009_000028_call_recording.rs:31,93,121,174,292`) |
| Events | `RECORDING_STARTED/STOPPED/READY/FAILED/DELETED` (`calls/events.rs:77-81`). READY appended at `calls/app/enterprise/recording.rs:1186`. Public: `recording.ready`, `recording.deleted` (`catalog.rs:196-197`). STARTED/STOPPED/FAILED are **not** public |
| Realtime notice | `call.recording_started` (`recording.rs:479`) and `call.recording_stopped` (`recording.rs:764`) go to every participant endpoint. This is a server-issued **compliance notice**, not an optional notification |
| History | `recordingAvailable` on history entries (ADR 0025 line 83) |
| Flags | `CALL_RECORDING_ENABLED` (production `true`, 11-03 doc line 18) |
| Production test | 11-03 doc line 40: "Recording PASS: … server notice and all-participant consent recorded …" |
| "Recording ready" user notification | **Absent** apart from the customer webhook `recording.ready` |

---

## 6. Calling: enterprise invitations, ring groups, queues, IVR, transfers, participant changes

"Invitation" in Calling means a **ring offer** (`call_invitations`). No email or link "invitation to call" exists in Calling. Organization-scoped calling is forwarded by the Meeting Service (`CALLING_ORGANIZATION_FORWARDING_ENABLED`, `services/calling/src/config/mod.rs:51,177`). That path is a scope/credential bridge, not a notification.

| Feature | Realtime signals | Durable record / outbox | Public webhook | Push / email | Code refs |
|---|---|---|---|---|---|
| Ring group (SIMULTANEOUS / ORDERED) | `call.incoming` per member endpoint; `call.answered_elsewhere` to losers | `ENTERPRISE_ROUTING_STEP`, `CALL_INVITATION_*` | `call.incoming`, `call.answered_elsewhere`, … | none | `calls/app/enterprise/ring_groups.rs:1-13`; claims `ring_groups.rs:532` |
| Queue | `call.queued` (caller, `enterprise/mod.rs:1362`); `call.queue_position` (caller, `queues.rs:1131`); `call.incoming` to agents (`queues.rs:932`); `call.ring_cancelled` (`queues.rs:655`) | `enterprise_queue_entries`, `enterprise_queue_offers` (`m20261009_000023_enterprise_queues.rs:101,178`), routing-step events | queue facts are **not public** (routing steps are internal) | none | `calls/app/enterprise/queues.rs:1-22` |
| IVR | `call.ivr_waiting`, `call.ivr_input` (caller; validity only, never the digit) | routing step | not public | none | `enterprise/ivr.rs:731` |
| Hold / resume | `call.held`, `call.resumed` | `CALL_STATE_CHANGED` | `call.held`, `call.resumed` | none | `control.rs` |
| Transfer | `call.transfer_started` (`transfer.rs:387`); `call.incoming` to target (`transfer.rs:397`); `call.transferred` (`transfer.rs:693`); `call.transfer_failed` (`ringing.rs:692`) | `CALL_TRANSFER_REQUESTED/COMPLETED/FAILED/CANCELLED` | `call.transfer_started/transferred/transfer_failed` | none | `calls/app/transfer.rs` |
| Participant add/remove/update | no dedicated signal (call-level signals only) | `CALL_PARTICIPANT_ADDED/UPDATED/REMOVED` | **not projected** (no catalog mapping) | none | `calls/events.rs:39-41`; `catalog.rs:162-206` |
| Public route failover | `call.route_changed` | `CALL_ROUTE_STATE_CHANGED` | `call.route_changed` | none | `public/events.rs` |

The full emitter map, produced by grepping `SignalType::*` across the code, is in Appendix A.

---

## 7. Calling: durable events, outbox and webhooks to the platform

### 7.1 Tables

| Table | Migration | Key columns / constraints | Retention |
|---|---|---|---|
| `call_events` | 000005 (core). Type CHECK extended in `m20261009_000030_enterprise_operations.rs:66-87` | Gap-free per-call `sequence`; immutable (UPDATE/DELETE/TRUNCATE refused) | **Retained forever.** `CALLING_RETENTION_CALL_EVENTS_DAYS` must stay unset; startup refuses a value (`retention/mod.rs:13,132,139`) |
| `calling_outbox_events` | `m20261007_000009_call_outbox.rs:18-105` | `event_id` `evt_<32hex>` UNIQUE; `source_event_id` UNIQUE FK → `call_events` ON DELETE RESTRICT; envelope CHECK (producer `cfmeet.calling`, ≤ 16 KiB); immutable columns; deferred constraint trigger `trg_call_events_require_outbox` | No retention sweep exists. PENDING rows cannot be deleted (`calling_outbox_events_guard_delete`) |
| outbox added cols | `m20261010_000031_platform_events_webhooks.rs:25-39`: `lease_until`, `last_error_code`, status `DEAD_LETTER` (transitions PENDING>PUBLISHED, PENDING>DEAD_LETTER, DEAD_LETTER>PENDING). `m20261201_000047_public_event_destinations.rs:15-21`: `local_outcome`, `developer_platform_outcome` ∈ ACCEPTED/REJECTED | — | — |
| `webhook_subscriptions`, `webhook_events`, `webhook_deliveries`, `webhook_delivery_attempts` | `m20261010_000031_platform_events_webhooks.rs:41-190` | `webhook_events.producer` CHECK widened to sms-gateway (`m20261025_000045_sms_webhook_producer.rs:12-15`) and messaging (`m20261113_000046`). Immutable | 90 days (`CALLING_RETENTION_WEBHOOK_HISTORY_DAYS`, `retention/mod.rs:16-17,140`) |
| `calling_incidents` | `m20261009_000030_enterprise_operations.rs:22-64`; categories extended in `m20261015_000043_calling_operational_safeguards.rs:111,207` | `uq_calling_incidents_open_key (dedupe_key) WHERE status='OPEN'`; evidence ≤ 4 KiB | RESOLVED deleted after 365 days (`CALLING_RETENTION_INCIDENTS_DAYS`) |

Observation: migration file names `m20261201_000047` and `m20261201_000048` are dated after today (2026-11-15). This is a naming anomaly only and is noted without further inference.

### 7.2 Publisher and worker

- Insert: `CallService` appends the event and its outbox row together (`services/calling/src/calls/service/mod.rs:224-271`).
- Worker loop: `main.rs:510-531`, every 1 s.
  - `publish_outbox_to(destinations, 200)`.
  - `deliver_due_once(50)`.
- `OUTBOX_MAX_ATTEMPTS = 10` and `OUTBOX_LEASE_SECONDS = 60` (`webhooks/worker.rs:16-17`).
- Dead letter: raises an `OUTBOX_DEAD_LETTER` incident (`worker.rs:387-397`). Replay: `replay_dead_lettered_outbox` (`worker.rs:411`).
- Destinations: `CALLING_PUBLIC_EVENTS_DESTINATION` = `local` (default) | `developer-platform` | `both` (`webhooks/publication.rs:62-90`).
  - Only `call.*`, `recording.*` and `voicemail.*` reach the Developer Platform (`publication.rs:119-125`, `is_published_to_developer_platform`).
  - `usage.*` and internal-only facts never leave Calling.
- Projection allow-list: `webhooks/catalog.rs:28-78` (public catalog of 49 types) and `ALLOWED_KEYS` (`catalog.rs:93+`).
  - No phone numbers, URIs, provider ids or free text are allowed.
  - `project_calling` is at `catalog.rs:162-206`.
- Limit notices: `limit.warning` and `limit.exceeded` (`webhooks/notices.rs:1-17`). Warning at 80% crossing; deduplicated per window. The ops docs give no explicit production state for these notices. They flow only through the local transport, which is now retired: `usage.*` and `limit.*` are not in the Developer Platform filter.

### 7.3 Calling as the platform's legacy event hub (cross-producer)

- `POST /internal/v1/platform/events` (`publishPlatformEvent`, cfServiceAuth, permission `platform-event.publish`) is implemented in `services/calling/src/webhooks/ingest.rs:20-61`.
- Accepted producers: `cfmeet.privacy-relay`, `cfmeet.sms-gateway` and `cfmeet.messaging` (`ingest.rs:40-45`).
- These envelopes are projected into `relay.*`, `sms.*`, `conversation.*` and `message.*` (`catalog.rs:211-300`).

### 7.4 Production state of webhooks and events

| Setting | Value | Evidence |
|---|---|---|
| `CALLING_WEBHOOK_AUTHORITY` | `retired` | `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:56,75`; `infra/calling/production/coordinated_release.py:2036` |
| `CALLING_WEBHOOK_DELIVERY_ENABLED` | `false` | same |
| `CALLING_PUBLIC_EVENTS_DESTINATION` | `developer-platform` | same (`coordinated_release.py:2037`) |
| Precondition | Calling had 0 webhook subscriptions and 0 open deliveries | run 43173688215 |
| Developer Platform webhooks | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=true`, mode `active` | same doc. Note: the earlier `cfmeet-final-production-certification.md:172` (2026-11-10) showed `false`/shadow, superseded on 2026-11-12 |
| Status label | "ACTIVE_VERIFIED (configuration, grants and health …)". Caveat in the same row: "an end-to-end producer event needs a platform principal (FIRST_PARTY only), so it was not driven by the external acceptance" | `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:75` |

**Classification:** the outbox and publication are implemented and activated. Delivery of a Calling event end-to-end to a customer is **not production-tested** per the ops doc. Calling's local webhook delivery is **deprecated / retired**, and its records are retained.

Tests: `tests/webhooks.rs:480 outbox_events_become_signed_public_deliveries`, `:608 retries_dead_letter_timeouts_and_manual_replay`, `:739 concurrent_workers_and_crash_recovery_deliver_once`, `:796 transport_outage_keeps_the_outbox_authoritative`, `:854` (privacy relay), `:965 sms_gateway_events_become_sms_webhooks_with_safe_data_only`, `:1088` (messaging). Also `tests/webhook_migration.rs`.

---

## 8. Calling: operations events, incidents and metrics (ADR 0026)

| Item | Evidence | Classification |
|---|---|---|
| Durable routing facts | `ENTERPRISE_ROUTING_STEP` call event plus outbox for every enterprise decision (`calls/app/enterprise/mod.rs` `record_step`; ADR 0026 lines 23-32) | implemented; **not public** (no catalog mapping) |
| Incidents | `services/calling/src/calls/app/incidents.rs:17-45`. Categories: STUCK_QUEUE_ENTRY, QUEUE_ASSIGNMENT_CONFLICT, IVR_ROUTING_CORRUPTION, VOICEMAIL_FINALIZATION_FAILURE, RECORDING_ARTIFACT_FAILURE, FORWARDING_LOOP_REPEATED, REPEATED_BLOCKED_ATTEMPTS, REPEATED_ROUTE_DENIALS, ABUSE_REPORTS_BURST, TELEPHONY_COMMAND_DEAD, OUTBOX_DEAD_LETTER, WORKER_BACKLOG, CREDENTIAL_GUESSING, … | implemented |
| Incident read / resolve | Operator CLI only: `services/calling/src/bin/platform_admin.rs:136-163,1513-1545` (`incidents list|resolve`, audited). No HTTP API | implemented |
| Worker backlog | `CALLING_WORKER_BACKLOG_INCIDENT_SECONDS` (default 600). Outbox pending gauges in `workers.rs:332-333` | implemented |
| Metrics | `calling_outbox_pending`, `calling_outbox_oldest_pending_age_seconds`, `calling_outbox_dead_lettered_total`, `calling_outbox_published_total`, `calling_webhook_deliveries_*`, `calling_missed_calls_total`, `calling_call_ring_seconds`, `calling_enterprise_*`. Optional listener `CALLING_METRICS_ADDRESS` (`main.rs:553`). **No metric** exists for realtime-signal publish success or failure, or for push dispatch | partial |
| Alerting | `docs/operations/monitoring-alerts.md:3-5` ("Nothing is deployed, and paging is NOT CONFIGURED"); lines 128-130 ("Calling, numbering, telephony, privacy-relay, sms-gateway … define no alert conditions. No rules exist for them yet") | **absent** |
| Disk alert (infra) | `calling-production-2026-11-02.md:53-55`: a systemd capacity check writes to the journal; "External alert delivery is not configured" | absent |

---

## 9. Calling: endpoints related to notification

All rows below come from `packages/contracts/calling/http/v1.openapi.json`, extracted by script.

| Method | Path | operationId | Auth | Handler |
|---|---|---|---|---|
| POST | `/v1/calling/endpoints` | registerEndpoint | platformCredential (`calls.write`) | `endpoints/http.rs:44,127` |
| GET | `/v1/calling/endpoints` | listEndpoints | platformCredential (`calls.read`) | `endpoints/http.rs:44,181` |
| DELETE | `/v1/calling/endpoints/{endpoint_id}` | revokeEndpoint | platformCredential (`calls.write`) | `endpoints/http.rs:45` |
| POST | `/v1/calling/signaling-credentials` | mintSignalingCredential | platformCredential (`calls.write`) | `endpoints/http.rs:46-47` |
| GET | `/v1/calling/signaling` | openSignalingChannel | signalingCredential | `signaling/ws.rs:67` |
| GET | `/v1/calling/invitations` | recoverInvitations | signalingCredential | `signaling/http.rs:47,70` |
| GET | `/v1/calling/history` | recoverCallHistory | signalingCredential | `signaling/http.rs:48` |
| POST | `/v1/calling/invitations/{id}/accept` | acceptInvitation | signalingCredential | `signaling/http.rs:49-52` |
| POST | `/v1/calling/invitations/{id}/decline` | declineInvitation | signalingCredential | `signaling/http.rs:53-56` |
| POST | `/v1/calls/{call_id}/accept` / `decline` | acceptCall / declineCall | platformCredential | backend acts for an endpoint |
| GET | `/v1/calls` | listCalls (history filters `principalId`, `missed`, `outcome`) | platformCredential | `calls/http.rs:49` |
| GET | `/v1/enterprise/voicemail/messages` (+ `/{id}`, `/read-state`, `/media-access`) | listVoicemailMessages, … | platformCredential (`enterprise.voicemail.*`) | `enterprise/http.rs:193` |
| GET | `/v1/enterprise/recordings` (+ `/{id}`, `/media-access`, `/access-audit`) | listRecordings, … | platformCredential (`enterprise.recording.*`) | `enterprise/http.rs:232` |
| POST/GET/PATCH/DELETE | `/v1/webhooks…`, `/v1/webhook-deliveries/{id}/replay` | createWebhook, …, replayWebhookDelivery | platformCredential | `webhooks/http.rs:38-45`. **Retired in prod (503)** |
| POST | `/internal/v1/platform/events` | publishPlatformEvent | cfServiceAuth | `webhooks/ingest.rs:20` |
| GET/POST | `/internal/v1/webhook-migration/*` | export / rollback | cfServiceAuth | `webhooks/ownership_migration.rs` |

---

## 10. Calling: feature flags relevant to notifications

| Variable | Default | Production (evidence) | Effect |
|---|---|---|---|
| `CALLING_NEW_CALLS_ENABLED` | true (`calls/app/mod.rs:104,233`) | true (11-03 doc) | master switch for new calls |
| `CALLING_APP_CALLING_ENABLED` | true | true | app-to-app RTC calls |
| `ENTERPRISE_CALLING_ENABLED` | false (`enterprise/mod.rs:55,75`) | true | ring groups, queues, IVR |
| `VOICEMAIL_ENABLED` | false | true | voicemail routing / capture |
| `CALL_RECORDING_ENABLED` | false | true | recordings and recording notices |
| `CALLING_INVITATION_TTL_SECONDS` | 45 (10–300) | not reported | ring window |
| `CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS` | 600 (≤ 3600) | not reported | WS credential lifetime |
| `CALLING_SIGNALING_SIGNING_KEY` | required | present (not read) | signaling credential key |
| (push transport) | **no variable**; hard-wired `NoPushTransport` | n/a | push absent |
| `CALLING_PUBLIC_EVENTS_DESTINATION` | local | developer-platform | outbox receivers |
| `CALLING_WEBHOOK_AUTHORITY` | active | retired | customer webhook routes return 503 |
| `CALLING_WEBHOOK_DELIVERY_ENABLED` | true | false | delivery worker sends nothing |
| `CALLING_WEBHOOK_MUTATIONS_FROZEN` | false | unchanged (checklist line 82) | migration freeze |
| `CALLING_WEBHOOK_SECRET_KEY` | unset ⇒ webhooks unavailable | not reported | sealing key |
| `CALLING_RETENTION_WEBHOOK_HISTORY_DAYS` / `_INCIDENTS_DAYS` | 90 / 365 | not reported | retention |
| `CALLING_PUBLIC_INBOUND/OUTBOUND_ENABLED`, `CALLING_CALLBACK_RELAY_ENABLED`, `CALLING_SYNTHETIC_INTERCONNECT_ENABLED` | false | false | PSTN paths (no PSTN-originated ringing) |

---

## 11. Latency-critical parts that must remain in Calling

These are on the synchronous ring/answer path. A centralized Notification Service must not own them, sit in front of them, or gate them.

1. **Offer issuance plus `call.incoming` signal build.**
   - What: transactional creation of `call_invitations` and the post-commit signal.
   - Refs: `create.rs:895-912`; `mod.rs:592-621`.
   - Budget: the ring window is 45 s by default.
2. **First-answer arbitration.**
   - What: `call_answer_claims` / `call_answer_group_claims` under the Call row lock, and media authorization only for the winner.
   - Refs: `ringing.rs:139-449`.
   - Why: it requires DB uniqueness in the same transaction.
3. **"Answered elsewhere" / ring-cancel / expiry signals.**
   - What: these stop ringing on other devices.
   - Why: they must follow the committed winner immediately. Today: `PgNotifySignalBus` plus WS, with a 1 s expiry sweep (`main.rs:504`).
4. **The realtime signaling WebSocket** and its auth and revocation (4001/4401). Ref: `signaling/ws.rs`.
5. **Invitation recovery** (`GET /v1/calling/invitations`) and accept/decline with `expectedVersion`.
6. **Server-issued compliance notices.**
   - What: `call.recording_started/stopped`.
   - Why: they are part of the recording consent flow (ADR 0024 line 51).
   - Also: caller-facing in-call signals `call.queued`, `call.queue_position`, `call.ivr_waiting`, `call.voicemail_started`.
7. **Push-payload construction for ring offers.**
   - What: `push_payload`, which needs the invitation id and expiry from the same commit.
   - A future *transport* (APNs VoIP / FCM high-priority) can be external, but the dispatch call (`PushDispatcher::dispatch`) is in-process, post-commit and best-effort.
   - If push moves to a Notification Service, it must be a low-latency, non-queued fast path (VoIP push has hard OS deadlines) or stay as an in-Calling dispatcher that reuses shared adapters. This matches the ops doc plan (profile-notifications-sections-9-11.md:177).

**Candidates for a Notification Service** (asynchronous, derived from the outbox, not latency-critical):

- missed-call notifications (derived);
- voicemail-deposit notification to the mailbox owner (`VOICEMAIL_READY`);
- recording-available notification (`RECORDING_READY`);
- operator incident alerting (`calling_incidents`, `OUTBOX_DEAD_LETTER`, `WORKER_BACKLOG`);
- limit warnings (`limit.warning/exceeded`);
- customer webhooks (already centralized in the Developer Platform).

---

## 12. Telephony (`services/telephony`)

| Aspect | Evidence | Classification |
|---|---|---|
| Role | Executes PSTN/SIP legs for Calling. Never decides who may call (`services/telephony/README.md:1-16`) | — |
| User notifications | **None.** A grep for sms/notif/email/push/alert/webhook finds only voicemail capture/prompt terms (`src/capture.rs:43-44`, `src/enterprise.rs:53` `VOICEMAIL_GREETING`) | absent |
| Event outbox | `telephony_outbox` (`migration/src/m20261008_000003_commands_events.rs:150-176`). Event types: LEG_ACCEPTED, LEG_RINGING, LEG_ANSWERED, LEG_CONNECTED, LEG_HELD, LEG_RESUMED, LEG_ENDED, LEG_FAILED, LEG_BUSY, LEG_NO_ANSWER, LEG_DECLINED, LEG_STATUS_UNKNOWN, BRIDGE_ESTABLISHED, BRIDGE_ENDED, LEG_DTMF, USAGE_FINALIZED, RECONCILIATION_RESULT. Delivered only to Calling `POST /internal/v1/telephony/events` (`applyTelephonyEvent`) in per-leg order (`src/events/outbox.rs:1-15`). Dead letter added in `m20261015_000015_outbox_dead_letter.rs` | implemented |
| Inbound offers | → Calling `POST /internal/v1/telephony/inbound-calls`. This becomes Calling ringing (PSTN→app), so Telephony is upstream of a notification trigger | implemented; disabled |
| Incidents | `telephony_incidents`, categories incl. OUTBOX_DEAD_LETTER, SIP_EDGE_UNAVAILABLE, PRIVACY_RELAY_UNAVAILABLE, NUMBERING_UNAVAILABLE, ABUSE_SIGNAL, PROVIDER_CIRCUIT_OPENED, UNKNOWN_*_OUTCOME, ORPHAN_* (grep of migration CHECKs). Read through `telephony-admin incidents` (README lines 118-122) | implemented; no alerting |
| Flags | `TELEPHONY_ENABLED`, `PUBLIC_OUTBOUND_CALLING_ENABLED`, `PUBLIC_INBOUND_CALLING_ENABLED`, `CALLBACK_RELAY_ENABLED`, `ENTERPRISE_SIP_ENABLED`, `TELEPHONY_SYNTHETIC_INTERCONNECT_ENABLED`; all default false (`src/config.rs:9,13,244-248`). `TELEPHONY_SIP_EDGE` | — |
| Production | **disabled.** `cfmeet-final-production-certification.md:167`: `TELEPHONY_ENABLED=false`, `PUBLIC_*_CALLING_ENABLED=false`, `CALLBACK_RELAY_ENABLED=false`, `ENTERPRISE_SIP_ENABLED=false`, `TELEPHONY_SIP_EDGE=none`. `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:42`: External PSTN/SIP **EXCLUDED_BY_OWNER**. README line 31: real interconnect certification BLOCKED (no carrier). The container runs and is healthy (`cfmeet-final-production-certification.md:115`) | disabled |
| Dependencies | Calling (events/inbound), Privacy Relay (contact resolution), Numbering (DCN grants) (README lines 84-94) | — |
| Security-scan relevance | None. Voicemail audio captured by Telephony is system-generated and goes to Calling's artifact store; it does not pass through the scan gateway | n/a |

---

## 13. SMS Gateway (`services/sms-gateway`)

| Aspect | Evidence | Classification |
|---|---|---|
| Role | Authoritative SMS transport: customer `POST /v1/sms/messages`, inbound callbacks, Messaging SMS bridge (`services/sms-gateway/README.md:1-30`; ADR 0042) | — |
| Sending capability | Implemented: state machine, leased attempts, UNKNOWN never resubmitted, receipts, controls, opt-outs (README "Outbound messages") | implemented |
| Provider adapters | **Only `deterministic`**, which is test-only (README "Provider adapters"). `guard_runtime` refuses it in staging/production (`src/providers/mod.rs:200-213`). Config: "`SMS_GATEWAY_PROVIDER` … `deterministic` (local / test only); unset: no provider, sends are refused" (`src/config/mod.rs:18`). Send without a provider → `SmsError::ProviderUnavailable` (`src/sms/send.rs:225`; HTTP 503 `PROVIDER_UNAVAILABLE` "nothing was sent", `src/http/error.rs:59`) | no real carrier adapter exists (**absent**) |
| Production | **disabled.** `SMS_GATEWAY_PROVIDER` unset (`cfmeet-final-production-certification.md:165`; `CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:282`; `coordinated-production-release.md:135`: "no Africa's Talking / SMS OTP / external sending"). `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:41`: "SMS delivery and phone OTP | `SMS_GATEWAY_PROVIDER` unset; release/verify/certify refuse any provider | EXCLUDED_BY_OWNER". `/v1/sms` unrouted at the gateway (same doc line 52; cert line 151: sms 4 routes unrouted). Container healthy (cert line 114) | disabled (activation not suggested) |
| Events emitted | `sms-gateway.message.{requested,submitted,delivered,failed,unknown,blocked,received}` (`src/sms/events.rs:23-45`) → `sms_outbox_events` (`migration/src/m20261025_000001_sms_foundation.rs:396`) → Calling ingest (default) or Developer Platform (`SMS_GATEWAY_PUBLIC_EVENTS_DESTINATION`: `calling` default / `developer-platform` / `both`; `src/infra/platform_events.rs:28-84`; config line 37). The ops docs give no production value for this variable. It is not in the `activate-developer-webhooks` list (`coordinated_release.py:2031-2040`) | implemented; idle |
| Bridge | `sms_bridge_bindings`, `sms_bridge_deliveries` (`m20261028_000003_sms_bridge.rs:18,42`). Inbound SMS is pushed to Messaging as a durable job (`src/sms/bridge.rs:1-17`). Service-to-service, not a user notification | implemented; idle |
| Used by anything for notifications? | **No.** The only callers are customer API, Messaging's SMS bridge (`services/messaging/src/sms_bridge/mod.rs`) and provider callbacks. No service uses it for OTP, alerts or reminders. A grep for `sms-gateway`/`SMS_GATEWAY` outside the service finds only calling webhooks, the developer-platform catalog/ledger and messaging `sms_bridge` | absent |
| Flags / vars | `SMS_GATEWAY_PROVIDER`, `_PROVIDER_CALLBACK_SECRET_FILE`, `_ALLOWED_DESTINATION_PREFIXES` (empty ⇒ nothing allowed), rate/spend/abuse controls, `_CONTENT_RETENTION_DAYS` 30, `_QUARANTINE_RETENTION_DAYS` 7 (`src/config/mod.rs:6-37`) | — |
| Security-scan relevance | None. SMS content is text, sealed with AEAD. No attachments or MMS | n/a |

---

## 14. Numbering (`services/numbering`)

| Aspect | Evidence | Classification |
|---|---|---|
| User notifications | **None** (no SMS/email/push code) | absent |
| Outbox | `numbering_outbox_events` (`migration/src/m20261016_000001_numbering_foundation.rs:22-45`, type CHECK `^number(\.[a-z][a-z0-9-]*){2,}$`). Leased publication, dead letter | implemented |
| Event types | number.acquisition.changed, assignment.changed, binding.changed, capability.changed, catalog.revised, eligibility.evaluated, **kyc-case.changed**, kyc-evidence.received, kyc-requirements.published, pool.changed, port.changed, provider-command.changed, provider-event.received, quarantine.changed, relay-lease.changed, release.changed, reservation.changed, resource.classified/discovered/lifecycle-changed, **suspended**, **restored**, risk.decided, route.changed, routing-profile.revised, usage.recorded (grep over `src/`) | implemented |
| Public subset | `PUBLIC_EVENT_TYPES` (`src/infra/platform_events.rs:53-60`): reservation.changed, assignment.changed, suspended, restored, release.changed, port.changed. Published to the Developer Platform only (`NUMBERING_PUBLIC_EVENTS_DESTINATION`, default `developer-platform`, or `none`; `src/config/mod.rs:14`). **KYC status, eligibility, risk and provider facts never leave Numbering** (README lines 38-48) | implemented |
| KYC status notifications | `number.kyc-case.changed` is internal only (ADR 0035 line 32). There is no customer or user notice when a KYC case needs evidence, is approved or is rejected | absent |
| Number lifecycle notices | `number.suspended` / `number.restored` (ADR 0043 §12, lines 83-87; `src/stewardship/suspension.rs:9,18,612`). Customer-visible only as webhooks via the Developer Platform | implemented; idle |
| Production | **disabled.** `NUMBERING_PROVIDERS` unset, `NUMBERING_PORTING_ENABLED=false`; `/v1/numbers` unrouted; gateway domain `numbering` disabled (`CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:43`; cert line 166). Container healthy (cert line 112). The production value of `CF_DEVELOPER_PLATFORM_BASE_URL` for Numbering is not stated in the ops docs; not inferred | disabled |
| Flags | `NUMBERING_PROVIDERS`, `NUMBERING_PORTING_ENABLED`, `NUMBERING_ELIGIBILITY_ENABLED`, `NUMBERING_KYC_VERIFIER` (`manual` default; `deterministic` refused in protected runtimes, `src/config/kyc.rs:5,130-141`), `NUMBERING_PUBLIC_EVENTS_DESTINATION`; operational kill switches `DCN_SMS_SENDER`, etc. (ADR 0043 §11) | — |
| OTP | Capability catalog states that voice/SMS **never promise** OTP / third-party verification acceptance (`src/numbers/limitations.rs:5-22`; README line 31) | n/a |
| **Security-scan relevance** | KYC evidence upload: PDF/JPEG/PNG ≤ 5 MiB, magic bytes checked, sealed AES-256-GCM (ADR 0035 lines 33-38). **Not routed through `security-scan-gateway`.** The only scan client is `cfmeet.messaging` (`infra/calling/production/scan_gateway.py:47-48`). This matters if a future Notification Service ever attaches or forwards KYC documents | gap noted (no recommendation to activate) |

---

## 15. Privacy Relay (`services/privacy-relay`)

| Aspect | Evidence | Classification |
|---|---|---|
| User notifications | **None.** `CommunicationChannel::Messaging` is "declared for the future but is never executed (no chat / SMS / provider messaging exists)" (`src/vocabulary.rs:63-69`). Relay numbers are voice only (`vocabulary.rs:248-254`) | absent |
| Outbox | `privacy_relay_outbox` (`migration/src/m20261008_000004_relay_audit_and_outbox.rs:145`). Publication columns in `m20261010_000012`, destinations in `m20261101_000017` | implemented |
| Events | privacy-relay.session.{created,updated,suspended,resumed,completed,revoked,expired}, context.{…,participants-changed}, enforcement.{requested,applied,failed}, relay-number.{assigned,cooldown-started,released} (grep over `src/`) | implemented |
| Public projection | Calling `project_privacy_relay` (`services/calling/src/webhooks/catalog.rs:211-241`) → `relay.created/updated/revoked/expired/participant_changed`. Enforcement and relay-number facts are internal | implemented |
| Publisher | `src/events.rs:1-32`. `PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION` = `calling` (default) / `developer-platform` / `both` (`src/config.rs:19,113`). No production value in the ops docs; it is not in the activation list | implemented; idle |
| Messaging authority jobs | `privacy_relay_messaging_authority_jobs` (`m20261026_000016`). `src/messaging_authority.rs:1-12` pushes context authority to Messaging `POST /internal/v1/context-authorities`. Service-to-service, not a notification | implemented |
| Production | **disabled.** `PRIVACY_RELAY_ENABLED`, `RELAY_NUMBER_POOL_ENABLED` and `CONTACT_VAULT_ENABLED` all default false (`src/config.rs:8,21,23`). Ops: "Privacy Relay, contact vault, relay numbers … all `false`; `/v1/privacy-relay` unrouted … EXCLUDED_BY_OWNER" (`CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:44`; cert line 168) | disabled |
| Security-scan relevance | None (no file uploads) | n/a |

---

## 16. SMS OTP and SMS-for-notification: definitive answer

| Question | Answer | Evidence |
|---|---|---|
| Does any service send SMS OTPs? | **No.** | SMS Gateway has no production-capable adapter (§13). Ops: "SMS delivery and phone OTP … EXCLUDED_BY_OWNER" (`CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:41`). Communications contract: "No phone OTP or phone-number discovery … phone verification … PROVIDER_BLOCKED until an SMS or verification provider is approved" (`packages/contracts/communications/http/v1.openapi.json:6,16,12398`) |
| Meeting-service OTP? | Email/Redis verification and password-reset codes, not SMS | `services/meeting-service/src/services/otp.rs:1-30` (Redis; 24 h verification TTL, 1 h reset TTL). Email transport per `profile-notifications-sections-9-11.md:161-163` (`EmailService` / SMTP). Outside this audit's service set; listed for completeness |
| Is the SMS Gateway used by anything for notifications? | **No.** | §13 caller inventory |
| Does Telephony / Numbering / Privacy Relay send SMS? | No | §12, §14, §15 |

---

## 17. Gaps and risks relevant to designing a Notification Service

1. **There is no background/locked-device call alerting.**
   - Problem: a device whose WebSocket is closed (app backgrounded or killed) is not alerted until it reconnects and calls recovery.
   - Impact: missed calls on mobile are therefore structurally likely until a push transport exists.
   - Status: CAL-003 EXTERNAL_BLOCKED.
2. **Push hook is fire-and-forget.**
   - `PushDispatcher::dispatch` has no return value (`push.rs:21`).
   - Push has no metric, no outbox and no retry.
   - Signal publish failures are logged at warn level only (`calls/app/mod.rs:512`).
3. **There is no per-participant missed-call fact.**
   - Only Call-level `call.no_answer` and per-offer events exist.
   - A notifier would have to re-derive the ADR 0025 logic or call the history API.
4. **Voicemail owner resolution.**
   - Mailboxes can be owned by EXTENSION / QUEUE / RING_GROUP, not only by a PRINCIPAL.
   - Mapping a `VOICEMAIL_READY` event to human recipients needs Calling enterprise data, which the public payload does not carry (only `mailboxId`, `messageId`, `durationMs`).
5. **Limit notices** (`limit.*`) and `usage.*` reach only the local, now-retired webhook transport.
   - They are not forwarded to the Developer Platform (`publication.rs:119-125`).
   - Their effective production delivery is therefore nil. This is inferred from code plus the retired flag; the ops docs do not state it.
6. **`CALL_PARTICIPANT_*` and `ENTERPRISE_ROUTING_STEP`** are durable but not public. A notifier needing queue or participant changes would need an internal consumer of `calling_outbox_events`.
7. **Outbox growth.** `calling_outbox_events` has no retention sweep. Published rows are kept indefinitely, together with `call_events`.
8. **Paging.** Alertmanager receivers are empty, and no Calling, Telephony, Numbering, SMS or Privacy Relay alert rules exist. Incidents are visible only through CLIs.
9. **Producer destinations are inconsistent.**
   - Calling, Messaging and Meeting were switched to the Developer Platform.
   - SMS Gateway and Privacy Relay still default to the `calling` ingest. Numbering defaults to the Developer Platform.
   - With Calling's authority retired, SMS and relay events (were those services enabled) would be projected locally but never delivered.
   - Low present impact: both services are disabled.
10. **Documentation drift.**
    - ADR 0008 states that invitation TTL is "≤ 10 min"; code caps it at 300 s.
    - The 2026-11-10 certification lists Developer Platform webhooks as disabled; the 2026-11-12 closure activated them. Use the later document.

---

## Appendix A: realtime signal emitters (from grepping `SignalType::*`)

| Signal | Emitting files (under `services/calling/src/calls/app/`) |
|---|---|
| call.incoming | create.rs, enterprise/mod.rs, enterprise/queues.rs, mod.rs, ringing.rs, transfer.rs |
| call.ringing | mod.rs, public/events.rs |
| call.ring_cancelled | control.rs, enterprise/queues.rs, public/events.rs, ringing.rs, workers.rs |
| call.accepted | ringing.rs |
| call.answered | mod.rs, public/events.rs, ringing.rs |
| call.answered_elsewhere | ringing.rs |
| call.declined | mod.rs, ringing.rs |
| call.cancelled | control.rs, mod.rs, public/events.rs |
| call.connected | media.rs, mod.rs, public/events.rs, transfer.rs |
| call.held / call.resumed | control.rs, mod.rs / control.rs |
| call.transfer_started / transferred / transfer_failed | mod.rs, transfer.rs / transfer.rs / ringing.rs |
| call.ended | control.rs, mod.rs, public/events.rs |
| call.failed | create.rs, enterprise/forward.rs, mod.rs, public/events.rs, workers.rs |
| call.no_answer | enterprise/voicemail.rs, mod.rs |
| call.busy | mod.rs |
| call.invitation_expired | ringing.rs |
| call.queued / call.queue_position | enterprise/mod.rs / enterprise/queues.rs |
| call.ivr_waiting / call.ivr_input | enterprise/ivr.rs |
| call.recording_started / stopped | enterprise/recording.rs |
| call.voicemail_started | enterprise/voicemail.rs |
| call.route_changed | public/events.rs |

## Appendix B: internal call-event types → public webhook types

Source: `services/calling/src/webhooks/catalog.rs:162-206`.

| Internal | Public |
|---|---|
| CALL_REQUESTED | call.requested |
| CALL_STATE_CHANGED → RINGING / ANSWERED / CONNECTED (from HELD: resumed) / HELD / ENDED / FAILED or DENIED / NO_ANSWER / BUSY / CANCELLED / DECLINED | call.ringing / answered / connected or resumed / held / ended / failed / no_answer / busy / cancelled / declined |
| CALL_INVITATION_ISSUED / ACCEPTED / CANCELLED / ANSWERED_ELSEWHERE / EXPIRED | call.incoming / accepted / ring_cancelled / answered_elsewhere / invitation_expired |
| CALL_INVITATION_DECLINED | (no mapping; the Call-level `call.declined` is emitted only when the Call declines) |
| CALL_ROUTE_STATE_CHANGED | call.route_changed |
| CALL_TRANSFER_REQUESTED / COMPLETED / FAILED | call.transfer_started / transferred / transfer_failed |
| VOICEMAIL_RECEIVED / READY / DELETED | voicemail.received / ready / deleted |
| RECORDING_READY / DELETED | recording.ready / deleted |
| CALL_USAGE_FINALIZED | usage.recorded (local only) |
| All others (participants, sessions, legs, attempts, media, ENTERPRISE_ROUTING_STEP, RECORDING_STARTED/STOPPED/FAILED, CALL_TRANSFER_CANCELLED) | not public |

## Appendix C: documents read

- **ADRs:** 0003, 0006, 0008, 0009, 0018–0026, 0030, 0031, 0035 (KYC excerpt), 0041, 0042, 0043. Only the lines relevant to notifications are cited; 0003, 0020–0022 and 0041 contain no notification behaviour beyond what is cited.
- **Architecture:** `enterprise-calling-implementation-map.md` (lines 13, 72-75, 108-141), `telephony-contract-inventory.md` (lines 14, 35-40).
- **Operations:**
  - `calling-production-2026-11-02.md` (historical; flags off)
  - `calling-backend-production-2026-11-03.md` (flags on; runs 41580265031, 41581639199, 41537691425, 41580667339)
  - `cfmeet-final-production-certification.md` (2026-11-10; run 42909534672; re-cert 43070726146)
  - `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md` (2026-11-12; runs 43173688215, 43174233820, 43176010902, 43198041905)
  - `profile-notifications-sections-9-11.md`
  - `monitoring-alerts.md`
  - `shared-security-scanning.md`
  - `coordinated-production-release.md`
  - `developer-platform-gateway-activation-checklist.md`
- **Workflow:** `.github/workflows/cfmeet-calling-production.yml` is referenced by the ops docs and was not opened.
