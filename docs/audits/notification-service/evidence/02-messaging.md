# 02 — Messaging notification inventory (services/messaging)

Read-only audit of `/home/user/cfmeet` at `main` HEAD `6f3743f` (2026-11-15). Nothing was modified, deployed or called.
No secret values or message content appear here; only environment variable names.

Status vocabulary: **implemented** (code exists on main) · **configured** (env/flags exist and are wired) ·
**activated** (documented as switched on in production, with run evidence) · **production-tested** (documented live
evidence) · **disabled** · **broken** · **deprecated/stale** · **absent**.

---

## 0. Executive summary

| Area | Status | Evidence |
|---|---|---|
| Push job pipeline (outbox -> push jobs -> leased dispatcher) | implemented, wired in `main.rs`; runs in prod but every job ends `NOT_CONFIGURED` (no adapter) | `realtime/fanout.rs:125-134`, `realtime/push.rs:240-283,398-534`, `main.rs:376-411` |
| APNs adapter (token/.p8, HTTP/2) | implemented, **not configured / not activated** | `push_adapters.rs:132-248`; tooling refuses APNs vars (`infra/calling/production/coordinated_release.py:1243-1250`) |
| FCM HTTP v1 adapter | implemented, **not configured / not activated** | `push_adapters.rs:264-452`; same refusal |
| Web Push (RFC 8030/8291/8292, VAPID) adapter | implemented; VAPID public-key route added 2026-11-15 (`42b509c`); activation tooling `activate-web-push` exists; **no recorded production run** | `push_adapters.rs:458-683`; `push.rs:66-81`; `coordinated_release.py:1878-1926,2410-2411` |
| LOCAL_TEST adapter | implemented; refused outside local/test runtimes | `push.rs:101-131`; `config/mod.rs:916-927` |
| Expo / VoIP (PushKit) / SMS / email notifications from Messaging | **absent** | grep; APNs uses `apns-push-type: alert` only (`push_adapters.rs:215`) |
| Production activation of any push provider | **not activated** | `CFMEET_MULTI_DEVICE_E2EE_FINAL_REPORT.md:175` (2026-11-15) "provider credentials are not configured (unchanged)"; `CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md:249` (2026-11-12) `MESSAGING_PUSH_PROVIDER=none` |
| Production-tested push delivery | **never** (fakes only) | `push_adapters.rs:28-30`; `messaging-section-8-security-review.md:61`; `messaging-native-api-map.md:104` |
| Client push registration (web service worker / mobile expo-notifications) | **absent** in `apps/web` and `apps/mobile` | `apps/mobile/app/(settings)/communication.tsx:25-26` ("Push notifications: not available yet"); `apps/mobile/features/messaging/hooks.ts:1`; web only allowlists relay paths (`apps/web/src/lib/messaging/routes.ts:47-48`) |
| Push triggers | only `messaging.message.created` with `message_kind = APPLICATION` and `notify != false` | `fanout.rs:128-134`, `push.rs:246` |
| Payload | metadata only `{v, type:"message", conversationId, eventSequence, sequence}`; identical for PRIVATE_E2EE and MANAGED; no sender, no text, no ciphertext, no keys | `push.rs:265-266`, `push_adapters.rs:53,76-91`, DB CHECK `m20261106_000004_realtime.rs:139-140` |
| Mute / notification preferences | **not in Messaging**; only in Communications Hub, applied at dispatch for the governed **personal** scope only | `push.rs:340-394`; `services/communications/src/policy.rs:173-208` |
| MANAGED organization push | effectively **impossible today**: push registration requires an E2EE `messaging_devices` row; org clients use session devices which can never register push | `push.rs:172`, `messages/e2ee.rs:37-65`, `m20261202_000030_session_devices_inbox.rs:4-7`, `http/middleware.rs:404-420` |
| Push vs realtime online state | push is sent to every registered device regardless of WebSocket presence (no "offline only" logic) | `push.rs:260-282` (no presence/hub lookup) |

---

## 1. Push pipeline end-to-end

### 1.1 Flow

```
send (REST / client token / delegated mirror / SMS inbound)
  -> messages::{e2ee,managed}::send  -> record_created_notify()  [same DB txn]
       outbox row  messaging.message.created  payload {message_id, sequence, message_kind,
       sender_principal_id, sender_device_id, security_provenance, notify,
       initiating_service_account_id, environment_kind}
  -> OutboxPublisher (lease, per-aggregate ordering)  -> RealtimeTransport::deliver_event
       1. push::fanout_jobs()  (INSERT ... ON CONFLICT (event_id, device_id) DO NOTHING)
       2. broker pointer (Local or Redis pub/sub)  -> gateway connections
  -> push::run  (every MESSAGING_WORKER_INTERVAL_MS)  -> dispatch_due(batch 200, max_attempts 8)
       reauthorize -> PushAdapters::send -> APNs | FCM | Web Push | LOCAL_TEST
```

| Step | File:lines | Notes |
|---|---|---|
| Event recorded with message | `services/messaging/src/messages/mod.rs:662-718` (`record_created_notify`) | `notify` and two authority references are put in the outbox payload (lines 689-693). Comment: "Never accepted from the message request or exposed as push payload." |
| `notify` source | `messages/mod.rs:270-272` (`SendRequest.notify`), `messages/e2ee.rs:236`, `messages/managed.rs:294` | Default `true`; **caller-controlled** (see §9 G-5) |
| Meeting legacy import | `integrations/meeting.rs:613` | `notify=false` (ADR 0053 lines 69-70: no push for meeting chat) |
| SMS bridge inbound | `sms_bridge/mod.rs:574` | `notify=true`: inbound SMS creates pushes |
| MLS commits | `mls/mod.rs:304` (`record_created`) | kind `MLS_COMMIT`, filtered out by `fanout.rs:129` |
| Push job creation | `realtime/fanout.rs:125-134` -> `realtime/push.rs:240-283` | Only `messaging.message.created` + `message_kind == "APPLICATION"`; fanout failure -> outbox `Retryable` |
| Recipient selection | `push.rs:262-271` | members `state='ACTIVE'`, not the sender (`IS DISTINCT FROM sender`), registration `enabled`, device `status='ACTIVE'` |
| Dispatcher | `push.rs:398-514`, loop `push.rs:527-534`, spawned `main.rs:408-411` | sequential sends within a pass |
| Adapter router | `push_adapters.rs:689-835` | `metadata()` whitelist enforced before APNs/FCM/Web Push (line 816) |

### 1.2 What triggers a push (and what does not)

| Event | Push? | Evidence |
|---|---|---|
| New application message (DIRECT / GROUP / any context, MANAGED or PRIVATE_E2EE) | **Yes** | `fanout.rs:128-129` |
| Inbound SMS bridged into a conversation | Yes (notify=true) | `sms_bridge/mod.rs:574` |
| Mention (MANAGED `mentionedPrincipalIds`) | No distinct push; same generic push as any message. Mentions stored in `messaging_message_mentions` for counts only | `messages/mod.rs:276-287`, migration `m20261202_000030` header lines 17-18 |
| Reply (`replyToMessageId`) | No distinct push (just a message) | `messages/mod.rs:274-276` |
| Edit (`messaging.message.edited`) | No | `messages/mod.rs:670-679`; filter `fanout.rs:128` |
| Reaction (`messaging.reaction.added`) | No | not `message.created` |
| Delete / moderate / expire | No | — |
| Group invitation / message request (`messaging.member.invited`, INVITED members) | **No** — INVITED counterpart receives nothing until accepting | `conversations/service.rs:764-777`; fanout requires `m.state='ACTIVE'` (`push.rs:270`) |
| Membership change (`member.added/removed/updated`) | No push (realtime/inbox signals only) | `fanout.rs:34-47` |
| MLS Welcome available | No push (realtime `device.event` only) | `fanout.rs:58-63` |
| Calls / meeting timeline (`messaging.timeline.changed`) | No push | `communications.rs` `timeline_changed`; Calling has its own `PushDispatcher` with `NoPushTransport` (`services/calling/src/signaling/push.rs`, `services/calling/src/main.rs:453`) |
| KeyPackage low stock (`messaging.device.key-packages-low`) | No push (outbox only) | `devices/mod.rs:975,1033` |

### 1.3 Payload content — proof of no plaintext/keys

* Job payload is built **in SQL** from ids/sequences only: `jsonb_build_object('v', 1, 'type', 'message', 'conversationId', $2::text, 'eventSequence', $3::bigint, 'sequence', $4::bigint)` — `push.rs:265-266`. No message row is read.
* DB guard: `ck_messaging_push_jobs_payload` — object, ≤1024 bytes, refuses keys matching `secret|private|token|ciphertext|plaintext|content|body|text|key` — `migration/src/m20261106_000004_realtime.rs:139-140`. Test proving refusal: `tests/realtime.rs:708-788` (inserting `{"ciphertext":"x"}` must fail).
* Adapter whitelist `METADATA_KEYS = ["v","type","conversationId","eventSequence","sequence"]`; anything else -> `Permanent("payload is not metadata-only")` — `push_adapters.rs:53,76-91,816-818`.
* Outbox payload also guarded by regex CHECK (`m20261101_000001_messaging_foundation.rs:335-337`).
* Per provider wire format:
  * APNs: `{"aps":{"alert":{"loc-key":"CF_MESSAGE_NOTIFICATION"},"mutable-content":1,"sound":"default"},"cf":{...metadata}}` — `push_adapters.rs:203-206`. Metadata in clear to Apple.
  * FCM: data-only `{"message":{"token":..,"data":{...metadata},"android":{"priority":"HIGH","ttl":"86400s"}}}` — `push_adapters.rs:413-417`. Metadata in clear to Google.
  * Web Push: metadata JSON encrypted `aes128gcm` to the subscription keys (RFC 8291) — `push_adapters.rs:546-586,651-657`; the push service sees only size/timing.
* PRIVATE_E2EE vs MANAGED: **identical payload**; MANAGED text is never included (client must fetch). Tests: `tests/push_adapters.rs:494-612` (MANAGED, asserts no "secret", no APNs `body`), `:614-667` (E2EE over Web Push, decrypts and checks metadata only), `tests/realtime.rs:708-788`.
* Not included: sender id, sender name, conversation title, message kind, attachment info, counts/badges.

### 1.4 Providers and credential configuration (env var names only)

Documented in `services/messaging/src/config/mod.rs:44-48`, parsed at `config/mod.rs:916-1016`, built at `main.rs:236-249` (`PushAdapters::build`, `push_adapters.rs:701-749`). Bad key files refuse startup; startup logs `push adapters platforms=[...]` (`main.rs:248`), never credentials.

| Provider | Env vars | Rules | Default |
|---|---|---|---|
| selector | `MESSAGING_PUSH_PROVIDER` | `none` (default) or `local-test` (only if runtime allows insecure transport) | `none` |
| APNs | `MESSAGING_PUSH_APNS_KEY_FILE`, `MESSAGING_PUSH_APNS_KEY_ID`, `MESSAGING_PUSH_APNS_TEAM_ID`, `MESSAGING_PUSH_APNS_TOPIC` (all four or none); `MESSAGING_PUSH_APNS_ENVIRONMENT` (`production`/`sandbox`); `MESSAGING_PUSH_APNS_BASE_URL` (local/test only) | `.p8` PKCS#8 P-256; team/key id 10 chars (`push_adapters.rs:150-162`); ES256 provider token cached 40 min (`:72,172-187`) | off |
| FCM | `MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE`; `MESSAGING_PUSH_FCM_BASE_URL` (local/test only) | RS256 via `ring`, OAuth token cached until 60 s before expiry (`:356-396`); token URI must be Google's (`:306-308`) | off |
| Web Push | `MESSAGING_PUSH_WEB_VAPID_KEY_FILE`, `MESSAGING_PUSH_WEB_VAPID_SUBJECT` (both or neither); `MESSAGING_PUSH_WEB_ALLOWED_HOSTS` (replaces allowlist) | VAPID P-256 PKCS#8; subject `mailto:`/`https:`; default host allowlist `fcm.googleapis.com, *.push.services.mozilla.com, *.notify.windows.com, web.push.apple.com, *.push.apple.com` (`:59-65`); VAPID JWT exp 12 h (`:645`) | off |
| LOCAL_TEST | via `MESSAGING_PUSH_PROVIDER=local-test` | records sends in memory | off |
| `UnconfiguredPush` | — | used only by `Realtime::disabled()` (`realtime/mod.rs:168-176`); production uses `PushAdapters` with all `None` | — |

### 1.5 Activation state (evidence, dated)

| Date | Source | Statement |
|---|---|---|
| ADR 0049 (undated in excerpt) | `docs/decisions/0049-messaging-realtime-gateway.md:35-37` | "No APNs / FCM adapter exists" — **stale** (adapters now exist) |
| 2026-11-12 | `docs/operations/CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:31,119` | Push OFF; `BLOCKED_OWNER_ACTION`; noted "no route publishes the VAPID public key" — **fixed since** by `42b509c` (`push.rs:66-81`, `realtime/http.rs:71,410-417`) |
| 2026-11-12 | `CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md:143,249` (certify 43336196158) | `MESSAGING_PUSH_PROVIDER=none`, `MESSAGING_E2EE_DEVICES_ENABLED=false`; "Mobile background push is a separate activation" |
| 2026-11-15 | `CFMEET_MULTI_DEVICE_E2EE_FINAL_REPORT.md:39,54,67,175` | E2EE devices activated (run 43663108367, `MESSAGING_E2EE_DEVICES_ENABLED=true`); push "provider credentials are not configured (unchanged)" |
| 2026-11-15 | commit `42b509c` | adds `GET /v1/messaging/push/configuration`, tooling `activate-web-push` (generates VAPID key on VPS, mounts `/run/messaging/web-push-vapid.pem`, subject `https://cfmeet.com`, requires `activate-personal-encryption` done, refuses any APNs/FCM vars, verifies startup log contains `WEB_PUSH`) — `coordinated_release.py:1878-1926,2410-2411`; option listed in `.github/workflows/cfmeet-coordinated-release.yml:22` |
| — | grep of `docs/` | **No workflow run id / report records an `activate-web-push` execution.** Classification: Web Push = implemented + activation tooling ready, **not evidenced as activated**. Even if run, no client subscribes (no service worker). |

Safe-default guard: release/verify/certify refuse `MESSAGING_PUSH_APNS_*`, `_FCM_*`, `_WEB_*` unless the matching activation is recorded (`coordinated_release.py:1243-1250`; tests `infra/calling/production/test_coordinated_release.py:442-492`).

### 1.6 Retry, backoff, dedup, collapse, TTL, error mapping

| Concern | Behaviour | Evidence |
|---|---|---|
| Job idempotency | `UNIQUE (event_id, device_id)` + `ON CONFLICT DO NOTHING`; outbox retry creates nothing twice | migration `:133`; `push.rs:271`; `fanout.rs:126-127` |
| Lease | `FOR UPDATE SKIP LOCKED`, lease token, 60 s lease (hard-coded) | `push.rs:403-405,421` |
| Batch / attempts | 200 jobs per pass, max 8 attempts (hard-coded in `run`) | `push.rs:529` |
| Backoff | `RetryPolicy::default()` 1 s doubling, cap 5 min (1,2,4,...,64 s ≈ 2 min total over 8 attempts) | `events/mod.rs:11-33`; `push.rs:428,485-487` |
| Retry-After from provider | **ignored** | no header parsing in `push_adapters.rs` |
| Job expiry | `expires_at = now + 24h` at fan-out; expired -> `EXPIRED` | `push.rs:278,436-438` |
| Provider TTL | `TTL_SECONDS = 86400` sent as APNs `apns-expiration`, FCM `android.ttl`, Web Push `TTL` header — counted from send time, not job creation | `push_adapters.rs:56,217-220,416,665` |
| Priority | APNs `apns-priority: 10`; FCM `HIGH`; Web Push `urgency: high` | `:216,416,668` |
| Collapse / replace | **absent**: no `apns-collapse-id`, no FCM `collapse_key`, no Web Push `Topic`, no APNs `thread-id` | `push_adapters.rs:203-220,413-417,658-669` |
| Dedup across devices of one user | none (one job per device by design) | `push.rs:262-271` |
| Invalid handle | APNs 410 / 400 `BadDeviceToken|DeviceTokenNotForTopic`; FCM 404 or `UNREGISTERED`; Web Push 404/410 -> `InvalidHandle` -> registration `enabled=FALSE, disabled_reason='INVALID_HANDLE'` only if `push_handle` still equals the attempted one (rotation-race safe) | `push_adapters.rs:237-239,443,678`; `push.rs:466-476` |
| Provider auth errors | APNs 403 Expired/InvalidProviderToken -> drop token, retry; FCM 401 -> drop OAuth token, retry | `:240-243,444-447` |
| Throttling / outage | 429/500/503 (APNs, FCM), 429/5xx (Web Push), transport errors -> `Retryable` | `:244,448,679,226,431,674` |
| Other statuses | `Permanent` -> `FAILED` | `:245,449,680` |
| Not configured platform | job ends `NOT_CONFIGURED` immediately (terminal, never retried later) | `push.rs:439-441` |
| Error text stored | `last_error` bounded to 200 chars, control chars stripped; adapters only emit status + reason codes, never handle | `events/mod.rs:35-38`; `push.rs:503` |
| Metrics | counter `messaging_push_jobs_total{status}`; gauge `messaging_push_jobs{status}` (DB group-by) | `push.rs:456,508-511`; `metrics.rs:260-262` |
| Alerts | **none** for push in `docs/operations/monitoring-alerts.md` | grep |

### 1.7 Dispatch-time reauthorization (`push.rs:319-395`, claim query `:401-423`)

Rechecked per job before calling a provider:
1. Registration `enabled` and device `status='ACTIVE'` (current) -> else `DISABLED` (`:431-435`).
2. `member_active`: current ACTIVE membership of the device's principal, conversation `lifecycle_state='ACTIVE'`, and event within the member's history window (`e.conversation_event_sequence >= m.visible_from_event`) (`:417-420`).
3. DIRECT conversations: block between sender and recipient (`safety::blocks::blocked_between`) (`:329-339`).
4. Governed personal scope only (`state.communications` present and `governs(scope)` and context DIRECT/GROUP): Communications Hub `PolicyCheck` with `Action::MessageNotification` (DIRECT) / `GroupNotification` (GROUP), using authority refs from the outbox payload (`initiating_service_account_id`, `environment_kind`); missing refs -> suppressed (fail closed) (`:340-394`). Hub denies on: blocks, privacy/relationship, recipient contact preference, account-wide direct/group switches, per-conversation mute (`services/communications/src/policy.rs:173-208`). Hub unavailable -> `Retryable` (counts toward 8 attempts).
5. Not checked: Platform Access principal status (suspended/deactivated), read cursor (already read?), presence/online state, credential/session freshness (`credential_issued_at: None`, `push.rs:382`).

Tests: `tests/notification_authorization.rs:43` (member departure), `:77` (direct block), `:131` (revoke-all disables push, device identity kept); Hub side `notification_policy_uses_recipient_preferences_and_live_blocks` (cited in `messaging-section-8-security-review.md:36`).

---

## 2. Device / push-subscription registration

### 2.1 Endpoints

| Method + path | Handler | Auth | Scope | operationId (contract `packages/contracts/messaging/http/v1.openapi.json` v1.17.0) |
|---|---|---|---|---|
| `PUT /v1/messaging/devices/{deviceId}/push-registration` | `realtime/http.rs:395-406` -> `push::register` `push.rs:147-202` | customer credential + `CF-Acting-Principal`, or `CF-Client-Token` (own device only, `http/middleware.rs:154,177-201,422-425`); session-device tokens refused (`middleware.rs:404-420`) | `messages.send` | `registerPush` |
| `DELETE /v1/messaging/devices/{deviceId}/push-registration` | `realtime/http.rs:419-429` -> `push::unregister` `push.rs:204-222` | same | `messages.send` | `unregisterPush` |
| `GET /v1/messaging/push/configuration` | `realtime/http.rs:410-417` -> `push::configuration` `push.rs:68-81` | customer credential + acting principal (NOT in client-token allowlist) | `messages.read` | `getPushConfiguration` |
| Delegated mirror of the three above: `/internal/v1/delegated/v1/messaging/...` | same handlers via `http/mod.rs:95-106`, `middleware.rs:690-750` | service token with `messaging-application.delegate`, first-party only | delegated scopes | (mirror; `tests/delegated_mirror.rs:71-74`) |
| `POST /internal/v1/communications/devices/push` | `communications/owner.rs:607-664` (`device_push`, REGISTER/REMOVE, calls canonical `push::register/unregister`) | service token `MessagingCommunicationsApply` + Hub delegated context, governed personal scope, NATIVE_CF_USER must equal `principalId` | — | `applyMessagingCommunicationsDevicePush` (communications-internal v1.3.0) |
| `POST /internal/v1/communications/push-status` | `owner.rs:673-765` | `MessagingCommunicationsRead` | — | `readMessagingCommunicationsPushStatus` — per device: registered, platform, last job state, coarse failure class, pending/dead counts; never handle or provider text |
| `POST /internal/v1/communications/devices/verify` | `owner.rs:497-522` | read | — | `verifyMessagingCommunicationsDevice` (returns `pushRegistered`, `pushPlatform`) |
| `POST /internal/v1/communications/devices/revoke` | `owner.rs:548-590` -> `devices::revoke` (disables push) | apply | — | `revokeMessagingCommunicationsDevice` |

Request schema `PushRegistrationRequest {platform: APNS|FCM|WEB_PUSH|LOCAL_TEST, handle 8..4096, enabled?}`; response `PushRegistration {deviceId, platform, enabled, pushDeliveryConfigured, updatedAt}`; `PushConfiguration {platforms{APNS,FCM,WEB_PUSH}, webPush: null|{applicationServerKey}, payload:"METADATA_ONLY"}` — `packages/contracts/messaging/common/v1.schema.json` (defs `PushRegistrationRequest`, `PushRegistration`, `PushUnregistered`, `PushConfiguration`, marked "1.25.0").

Public routing: `infra/api-gateway/public-routes.json:3305-3317`, `infra/api-gateway/route-manifest.json:6248-6268,6712-6715`; Meeting Service account relay `/api/v2/messaging/relay/v1/messaging/...` allowlists the three routes (`services/meeting-service/src/services/unified_messaging/relay.rs:454-482,979`; `docs/operations/messaging-native-api-map.md:158-160`).

Registration validation: platform enum + length/control chars (`push.rs:154-163`); structural shape (`push_adapters.rs:761-777`: APNs 64-200 hex, FCM 32-4096 `[A-Za-z0-9-_:.]`, Web Push = PushSubscription JSON with http(s) endpoint); if Web Push adapter configured, full check incl. HTTPS + host allowlist + P-256 `p256dh` + 16-byte `auth` (`push_adapters.rs:492-534,801-807`); `require_own_device` — device must be an ACTIVE `messaging_devices` row of the acting principal in scope (`messages/e2ee.rs:37-65`).

### 2.2 Tables (migration `services/messaging/migration/src/m20261106_000004_realtime.rs`)

`messaging_push_registrations` (`:94-113`)

| Column | Type | Notes |
|---|---|---|
| id | UUID PK | |
| tenant_id, application_id, environment_id, principal_id | UUID NOT NULL | scope copied from caller |
| device_id | UUID NOT NULL | `UNIQUE (device_id)`, FK -> `messaging_devices(id)` (E2EE/MLS devices only) |
| platform | VARCHAR(16) | CHECK in (APNS, FCM, WEB_PUSH, LOCAL_TEST) |
| push_handle | VARCHAR(4096) | CHECK length 8..4096, no control chars; **stored in plaintext** (Web Push: endpoint + `p256dh` + `auth`) |
| enabled | BOOLEAN default TRUE | |
| disabled_reason | VARCHAR(48) | `^[A-Z][A-Z0-9_]{1,47}$`; values used: `UNREGISTERED`, `DEVICE_REVOKED`, `INVALID_HANDLE`, `SESSIONS_REVOKED`, `ACCOUNT_SESSION_REVOKED` |
| created_at, updated_at | TIMESTAMPTZ | |

No index on `(environment_id, principal_id)` although fan-out joins on it (`push.rs:268`); no uniqueness on `push_handle`.

`messaging_push_jobs` (`:115-142`)

| Column | Type | Notes |
|---|---|---|
| id | UUID PK | |
| event_id | UUID | outbox event id (no FK) |
| conversation_id, environment_id, device_id | UUID | |
| registration_id | UUID FK -> registrations | |
| payload | JSONB | metadata CHECK (`:139-140`) |
| status | VARCHAR(24) | PENDING, SENT, FAILED, EXPIRED, DISABLED, NOT_CONFIGURED |
| attempts | INT ≥0 | |
| next_attempt_at, expires_at | TIMESTAMPTZ | |
| lease_token, lease_expires_at | UUID / TIMESTAMPTZ | both-or-neither CHECK |
| last_error | VARCHAR(200) | |
| created_at, completed_at | TIMESTAMPTZ | |

Keys/indexes: `UNIQUE (event_id, device_id)`; partial index `ix_messaging_push_jobs_due (next_attempt_at) WHERE status='PENDING'` (`:142`). No index on `device_id` (used by MS-7 push-status LATERAL query, `owner.rs:700-719`). **No retention/purge** of terminal jobs anywhere (`workers/mod.rs`, `compliance/*` grep).

Related: `messaging_session_devices` (`m20261202_000030_session_devices_inbox.rs:26-45`) — explicitly "can never own ... push registrations" (`:4-7`). `messaging_communication_inbox` / `messaging_communication_revocations` (`migration/src/communications.rs:7-15`).

### 2.3 Linkage to devices, revocation, membership

| Trigger | Effect on push | Evidence |
|---|---|---|
| Device revoke (REST `POST /v1/messaging/devices/{id}/revoke`, Hub `devices/revoke`, account-device replacement fan-out via Hub) | same transaction: `disable_device` -> `DEVICE_REVOKED`; sessions revoked; `messaging.device.revoked` event | `devices/mod.rs:846-899`; `push.rs:225-237`; ADR 0060 line 46; `services/communications/src/devices.rs:68,408` |
| Revoke all client sessions (`POST /v1/messaging/client-sessions/revoke-all`) | all registrations of principal in env -> `SESSIONS_REVOKED` | `realtime/sessions.rs:493-495` |
| Hub event carrying `principalId` + `credentialsNotBefore` (account session revocation) | registrations with `updated_at <= cutoff` -> `ACCOUNT_SESSION_REVOKED` | `communications.rs:350-358` |
| Single client-session revoke | **no** push change | `sessions.rs:~420-460` |
| Membership removal / ban / conversation lock | no registration change; job suppressed at dispatch by `member_active` | `push.rs:417-420` |
| Org member SUSPENDED/REMOVED (Organizations -> Meeting Service -> Platform Access principal SUSPENDED) | **no** effect on registrations or dispatch; Messaging only flushes its auth cache from the identity feed | `services/meeting-service/src/services/organization_access.rs:8-13`; `identity/cache.rs:1-21,221-228` |
| PRINCIPAL_ERASURE data deletion | **does not delete push registrations or jobs** (only messages, attachments, presence) | `compliance/deletion.rs:1-6,258-265` |
| Rechecked before push? | Device status: yes; membership: yes; block (DIRECT): yes; Hub policy: personal scope only; principal account status: only via Hub in personal scope | §1.7 |

---

## 3. Mute, preferences, requests, receipts, offline queue, multi-device, realtime

| Topic | Where | Status / notes |
|---|---|---|
| Per-conversation notification mute | Communications Hub `communication_conversation_preferences` (`notifications<>'ALL' AND muted_until`) | implemented in Hub; applied to Messaging push only for governed personal scope (`policy.rs:200-207`). **Absent in Messaging**; org/MANAGED scope has no mute. Messaging "mute" = moderation send restriction (`membership/mod.rs:811`, `moderation/actions.rs:25-26`), unrelated to notifications |
| Contact preference ALL/MUTED/MENTIONS | Hub `communication_contact_aliases.notifications` | MENTIONS suppresses ALL pushes because ciphertext is opaque (`policy.rs:180-189`; `messaging-native-api-map.md:105-107`) — also applied to MANAGED messages where mentions are known server-side |
| Account-wide direct/group switches | Hub `communication_notification_preferences` | `policy.rs:190-199` |
| Per-device enable/disable | `messaging_push_registrations.enabled` | implemented (`push.rs:138-141,204-222`) |
| Message requests | `requests_require_acceptance` (DIRECT): counterpart INVITED, gets no events/push until accept | `conversations/policy.rs:83-85`, `conversations/service.rs:764-777`. No notification of a pending request at all (push or otherwise, beyond `GET /v1/messaging/invitations`) |
| Read receipts | `messaging_read_cursors`; event `messaging.read-cursor.updated` (coalesced, `MESSAGING_RECEIPT_COALESCE_WINDOW_MS` default 1500) | `messages/receipts.rs:1-26,240-330`; migration `m20261203_000031` |
| Delivery receipts | `messaging_received_cursors` (device ack or read); `messaging.delivery-cursor.updated` | migration `m20261123_000021_received_cursors.rs:1-21`. A push is **not** delivery (`push.rs:15`) |
| Own-device read sync | `messaging.read-cursor.synced` -> broker `ReadCursor` to own sessions/inbox | `fanout.rs:53-57`; no push-side "clear notification" |
| Offline delivery queue | none separate: PostgreSQL event stream + REST replay `GET /v1/conversations/{id}/events` (`replayConversationEvents`) and resume from stored ack cursor; push jobs are only wake-ups | `realtime/mod.rs:6-24`; `connection.rs:1-16` |
| Realtime gateway | WebSocket `GET /v1/realtime` (`connectRealtime`), tickets `POST /v1/realtime/tickets` (`issueRealtimeTicket`), subprotocol `cfmeet.messaging.v1`; frames incl. `event`, `inbox.event`, `inbox.resync`, `device.event`, `receipts.snapshot`, `typing`, `presence`, `subscription.revoked`, `server.draining` | `realtime/http.rs:1-72`; `connection.rs`; contract `packages/contracts/messaging/realtime/v1.schema.json` |
| Inbox realtime (org + personal list updates) | broker pointers carry ACTIVE member ids (≤1024) | `fanout.rs:68-117`; `hub.rs:315-334` |
| Push vs online | **Not coordinated**: jobs created for all registered devices, connected or not; presence (Redis/memory) never consulted | `push.rs:240-283`; `ephemeral.rs:31` |
| Session devices (MANAGED realtime without E2EE) | `MESSAGING_SESSION_DEVICES_ENABLED` (prod `true`, 2026-11-12) | can never register push (`middleware.rs:404-420`; migration `_000030` header) |

---

## 4. Notification-related HTTP endpoints and consumers

### 4.1 Customer surface (contract `messaging/http/v1.openapi.json` 1.17.0)

| Method | Path | operationId | Auth (contract) | Notification relevance |
|---|---|---|---|---|
| PUT | `/v1/messaging/devices/{deviceId}/push-registration` | registerPush | customerCredential or clientToken | push handle |
| DELETE | same | unregisterPush | customerCredential or clientToken | push off |
| GET | `/v1/messaging/push/configuration` | getPushConfiguration | customerCredential | VAPID key + platforms |
| POST | `/v1/conversations/{id}/messages` | sendMessage | both | `notify` flag |
| PUT | `/v1/conversations/{id}/read-cursor` | setReadCursor | both | read receipts/unread |
| GET | `/v1/conversations/{id}/unread` | getUnreadCount | both | badge source (no push badge) |
| GET | `/v1/conversations/{id}/events` | replayConversationEvents | both | offline catch-up |
| POST | `/v1/realtime/tickets` | issueRealtimeTicket | clientToken | realtime |
| GET | `/v1/realtime` | connectRealtime | realtimeTicket | realtime |
| POST | `/v1/messaging/client-tokens` | issueClientToken | customerCredential | realtime |
| POST | `/v1/messaging/client-sessions/{id}/revoke` | revokeClientSession | customerCredential | — |
| POST | `/v1/messaging/client-sessions/revoke-all` | revokeAllClientSessions | customerCredential | disables all push |
| POST | `/v1/messaging/session-devices` / `/revoke` | issueSessionDeviceToken / revokeSessionDevices | customerCredential | MANAGED realtime |
| GET/PUT | `/v1/messaging/presence/settings` | get/putPresenceSettings | both | presence |
| GET/POST/DELETE | `/v1/messaging/blocks[/{principalId}]` | listBlocks/createBlock/removeBlock | both | block suppresses DIRECT push |
| GET | `/v1/messaging/invitations`; POST `/v1/conversations/{id}/invitation/accept|decline` | listInvitations, acceptInvitation, declineInvitation | both | requests (no push) |
| GET | `/v1/messaging/devices/{id}/welcomes` | listWelcomes | both | Welcome (realtime only) |

Internal: communications-internal v1.3.0 — `applyMessagingCommunicationsDevicePush`, `readMessagingCommunicationsPushStatus`, `verifyMessagingCommunicationsDevice`, `revokeMessagingCommunicationsDevice`, `applyMessagingCommunicationsEvent` (`communications.rs:159-181`).

### 4.2 Consumers (brief grep)

| Consumer | Uses push endpoints? | Evidence |
|---|---|---|
| `apps/web` | Only relay allowlist entries; **no** `pushManager.subscribe`, no service worker, no call to registerPush/getPushConfiguration | `apps/web/src/lib/messaging/routes.ts:47-48`; grep for `serviceWorker`/`applicationServerKey` returns nothing. Browser `Notification` API used only for incoming calls in an open tab (`apps/web/src/components/calling/IncomingCallLayer.tsx:9,71`) |
| `apps/mobile` | **No** push; polling | `apps/mobile/features/messaging/hooks.ts:1`; `apps/mobile/app/(settings)/communication.tsx:25-26`; no `expo-notifications` |
| Communications Hub | registers/removes via MS-6, reads MS-7; mirrors `push_registered_at`/`push_platform` in its device registry | `services/communications/src/devices.rs:1-25,342,519` |
| Meeting Service relay | forwards the three routes for Account sessions (personal) | `unified_messaging/relay.rs:454-482` |
| SDKs | generated operations include getPushConfiguration (`sdks/typescript/src/generated/operations.ts`, `sdks/dart/...`) | commit `42b509c` file list |

---

## 5. Events / outbox / inbox

| Item | Detail | Evidence |
|---|---|---|
| Outbox table | `messaging_outbox_events` (id UUID, `sequence BIGSERIAL` unique, event_type `^messaging(\.[a-z][a-z0-9-]*){2,}$`, `schema_version SMALLINT default 1`, aggregate, scope, correlation/causation, payload ≤4096 B with content-key CHECK, status PENDING/PUBLISHED/DEAD_LETTER, lease, conversation_id + conversation_event_sequence) | `m20261101_000001_messaging_foundation.rs:304-345` |
| Event id | `evt_<uuid simple>` | `events/outbox.rs:184-186` |
| Envelope | `cfmeet.platform.event-envelope` (event_id, event_type, schema_version, occurred_at, producer_service, runtime_environment, correlation_id, payload, ownership, causation_id). `initiating_service_account_id`/`environment_kind` are **stripped** from external envelopes | `outbox.rs:190-231` |
| Ordering | per-aggregate commit order (claim skips events whose earlier same-aggregate event is PENDING); per-conversation gap-free `conversation_event_sequence` | `outbox.rs:91-131,290-317` |
| Retry / DLQ | lease `MESSAGING_WORKER_LEASE_SECONDS` (60), max `MESSAGING_OUTBOX_MAX_ATTEMPTS` (12), backoff 1 s..5 min, then `DEAD_LETTER` (visible in metrics) | `outbox.rs:1-20,319-360`; `config/mod.rs:24,710-722` |
| Replay | conversation events replayed from DB by cursor (`replayConversationEvents`, WebSocket resume) | `realtime/mod.rs:9-24` |
| Push idempotency | `(event_id, device_id)` | §1.6 |
| Contract | `packages/contracts/messaging/events/v1.schema.json` lists 45 `messaging.*` types | |
| Producers (push-relevant) | `messages/mod.rs:681` (message.created/edited), `devices/mod.rs:874-890` (device.revoked), `realtime/sessions.rs:440-452,500-516` (client-session.revoked), `messages/receipts.rs:100,277` | |
| Consumers inside Messaging | `RealtimeTransport` (push jobs + broker) `fanout.rs:119-163`; webhook relay trigger -> `messaging_webhook_relay` -> Calling platform-event ingress and/or Developer Platform (`m20261114_000012_webhook_relay.rs:35-47`; `main.rs:304-370`, `MESSAGING_PUBLIC_EVENTS_DESTINATION`) — customer-webhook "notifications" for `message.created` (APPLICATION), conversation/member lifecycle, deletes |
| Inbound events from other services | Communications Hub -> `POST /internal/v1/communications/events` (`communications.rs:299-366`): schema_version must be 1, type `communications.*`; idempotent inbox `messaging_communication_inbox(event_id PK, request_digest SHA-256)` with identity-conflict detection; handles `communications.timeline.changed` (version-fenced) and any event with `principalId`+`credentialsNotBefore` (revoke sessions + disable push) |
| Platform Access | identity change feed poll `POST /internal/v1/identity/changes` every `MESSAGING_IDENTITY_FEED_INTERVAL_MS` (2000) — **only flushes the auth cache**, no push effect | `identity/cache.rs:64-108,221-228`; `main.rs:128-131` |
| Organizations | no direct consumption in Messaging; Organizations -> Meeting Service -> Platform Access principal SUSPENDED; Messaging sees it only on next request auth / realtime revalidation | `organization_access.rs:8-13` |
| Privacy Relay | context authority (`/internal/v1/context-authorities`) locks/closes conversations -> dispatch `lifecycle_state='ACTIVE'` check suppresses push | `integrations/context_authority.rs:1-15`; `push.rs:420` |
| Stale doc | `outbox.rs:17-20` says "No consumer exists yet ... publisher is not started" — publisher is started (`main.rs:377-399`) | |

---

## 6. Workers (all spawned in `services/messaging/src/main.rs`)

| Worker | Spawn | Interval | Batch / concurrency | Locking |
|---|---|---|---|---|
| Outbox publisher (realtime fan-out + push job creation) | `main.rs:377-399` `run_with_wakeup` | `publisher_interval` 250 ms (min 50) + `Notify` wake after every successful mutation (`middleware.rs:349-352`) | 500 | lease + SKIP LOCKED, per-aggregate ordering, claim-token-guarded completion |
| Push dispatcher `realtime::push::run` | `main.rs:408-411` | `MESSAGING_WORKER_INTERVAL_MS` (default 1000) | 200 per pass, sequential provider calls (10 s HTTP timeout each, `push_adapters.rs:158,325,623`), max attempts 8 | lease 60 s + SKIP LOCKED, lease-token-guarded completion |
| Receipt coalescing flush | `main.rs:401-407` | 250 ms | `flush_due` | durable rows |
| Webhook relay | `main.rs:367-371` | worker interval | — | queue table |
| Broker listener `hub::run` | `realtime/mod.rs:179-182` | push-based | per instance | — |
| Identity feed watcher | `main.rs:127-132` | 2000 ms | 500 changes/page | — |
| Maintenance | `main.rs:440-443` | 30 s | tickets, history transfers, KeyPackages (no push job cleanup) | idempotent |

Redis (`MESSAGING_REALTIME_REDIS_URL`, optional): broker pub/sub channel `cfmeet:messaging:realtime:v1` (`broker.rs:21`), presence keys `cfmeet:messaging:presence:v1:{..}:{..}` with PEXPIRE (`ephemeral.rs:31,157-175`), service-token replay guard `cfmeet:svc-jti:` (`config/replay_guard.rs:22,64-71`). **Push does not use Redis**; the push queue is PostgreSQL only. Without Redis: in-process broker/presence (`main.rs:229`).

---

## 7. Tests covering push / notifications

| Test | File:line | Proves | Limits |
|---|---|---|---|
| `apns_fcm_and_web_push_deliver_metadata_only_and_map_provider_answers` | `tests/push_adapters.rs:494` | MANAGED group, 3 platforms, metadata only, APNs no `body`, 429 retry, FCM OAuth caching/401, 410/404 -> INVALID_HANDLE disable | local verifying fakes |
| `e2ee_pushes_carry_no_plaintext_and_handles_are_validated` | `push_adapters.rs:614` | Web Push decrypts to metadata; malformed handles and internal hosts refused | fakes |
| `push_configuration_publishes_the_vapid_public_key_and_platforms` | `push_adapters.rs:669` | key = 65-byte uncompressed point, scope enforced | — |
| `an_openssl_generated_vapid_key_configures_web_push_through_the_environment` | `push_adapters.rs:714` | production key path via env parser | needs openssl |
| unit: `only_metadata_leaves_messaging`, `host_allowlist_...`, `subscriptions_cannot_aim_the_server_at_internal_addresses`, `rfc8291_appendix_a_vector`, ES256 pinning, `es256_tokens_verify_with_the_public_key` | `push_adapters.rs:841-966` | whitelist, SSRF guard, RFC vector, signature stability | — |
| `push_payloads_are_metadata_only_and_revoked_devices_get_none` | `tests/realtime.rs:708` | LOCAL_TEST send, sender excluded, unconfigured APNs not delivered, revoke disables, DB CHECK | — |
| `queued_push_is_not_delivered_after_member_departure` / `..._after_a_direct_block` / `revoking_all_sessions_disables_push_without_deleting_device_identity` | `tests/notification_authorization.rs:43,77,131` | dispatch-time reauthorization | no Hub mute test here |
| `push_status_reports_not_configured_and_never_echoes_the_handle` | `tests/communications_owner.rs:416` | MS-7 privacy | — |
| `push_services_report_only_real_configuration` | `communications/owner.rs:809` | LOCAL_TEST never reported | — |
| Authorization matrix rows (PUT/DELETE push-registration, GET push/configuration) | `tests/authorization_matrix.rs:~1081-1089` (added `6f3743f`) | IDOR/scope/denials leave DB unchanged | — |
| Delegated mirror rows | `tests/delegated_mirror.rs:72-74` | mirror reachable only by first-party delegate | — |
| Tooling tests | `infra/calling/production/test_coordinated_release.py:442-492` | safe defaults, `activate-web-push` preconditions/startup check | — |
| CI | `.github/workflows/messaging.yml:377-383` | `cargo test --test realtime` and `--test push_adapters` | not live |
| Missing | — | no test for: dispatch lease expiry/duplicate send across instances, Hub-unavailable retry exhaustion, MENTIONS on MANAGED, push to suspended org principal, job retention, collapse/clear | |

---

## 8. Delegated mirror and authorization matrix (what they are)

* **Delegated mirror** (`services/messaging/src/http/mod.rs:95-106`, `http/middleware.rs:690-750`, test `tests/delegated_mirror.rs:1-20`): every customer operation under `/v1/messaging/**` and `/v1/conversations/**` is served a second time at `/internal/v1/delegated/v1/...` for **first-party backends** (Meeting Service relay) that act for an end user via a signed Ed25519 service token holding `messaging-application.delegate`. The end user (initiating principal) is carried signed in the token; a `CF-Acting-Principal` header may only repeat it (mismatch -> `ACTING_PRINCIPAL_MISMATCH`); non-first-party contexts -> `DELEGATED_CONTEXT_INVALID`. Handlers see an ordinary `CustomerCaller` with `DELEGATED_SCOPES`. The test suite checks the mirror table against the contract so a new customer operation fails CI until its mirror path is listed — `6f3743f` added `GET /internal/v1/delegated/v1/messaging/push/configuration`.
* **Authorization matrix** (`tests/authorization_matrix.rs:1-27`): per-route IDOR/authorization suite: every contract operation is executed against the real router + PostgreSQL with each applicable denial (missing/revoked credential, client token on customer route, insufficient scope, cross-tenant ids, non-member, removed/banned member, inactive principal, other principal's device, ...) and must return the expected error, echo no identifiers, and leave a fingerprint of every table unchanged; then the positive call must succeed. `every_operation_is_in_the_matrix` forces new operations into it. `6f3743f` added `GET /v1/messaging/push/configuration` as an environment-level (`Res::Env`), `messages.read`, acting-principal-required row.
* For a Notification Service, these are the existing governance hooks: any new notification endpoint in Messaging is automatically forced into both suites.

---

## 9. Gaps, duplication, bugs, security/privacy concerns

### Gaps
* **G-1 No live delivery anywhere.** All three provider adapters are dormant in production; no client subscribes (web has no service worker; mobile has no push). Push is functionally absent for users.
* **G-2 MANAGED/organization push is structurally blocked.** Registration requires an ACTIVE `messaging_devices` (E2EE) row (`push.rs:172`), while organization/MANAGED clients use session devices that can never hold push registrations. `activate-web-push` even requires `MESSAGING_E2EE_DEVICES_ENABLED=true` (`coordinated_release.py:1902-1903`).
* **G-3 Single trigger type.** No push for message requests/invitations, mentions (distinct), reactions, Welcomes, calls (Calling `NoPushTransport`), membership changes.
* **G-4 Preferences live outside Messaging.** Mute, contact preferences, account switches are Hub-only and applied only to the governed personal scope (`push.rs:348-350`); organization scope has no mute/DND/preference at all. MENTIONS preference suppresses every push even for MANAGED messages whose mentions the server knows.
* **G-5 `notify` is caller-controlled.** Any sender (including device-direct client tokens on `POST /v1/conversations/*/messages`, `middleware.rs:123`) can send `notify:false` and silently suppress recipients' pushes; contract describes it as for "mirrored / migrated writes" only (`common/v1.schema.json:2549-2551`).
* **G-6 No online/offline coordination, no clear-on-read.** Pushes go to connected devices too; reading on one device never retracts notifications elsewhere; no collapse keys/topics, so bursts produce one notification per message per device.
* **G-7 No badge/unread count in payload** (unread is REST/realtime only).
* **G-8 No retention for `messaging_push_jobs`** (every message x device creates a row, including `NOT_CONFIGURED` ones today since registrations can exist); unbounded growth.
* **G-9 No alerts** on push failure/backlog (metrics exist only).
* **G-10 Org principal suspension not applied to push** in organization scope (only membership/device/block rechecked).

### Duplication
* Communications Hub device registry mirrors push state (`push_registered_at`, `push_platform`) alongside Messaging's canonical table (`services/communications/src/devices.rs:18-25`).
* Calling has a separate, unimplemented `PushDispatcher` trait and `cfmeet.calling.push-invitation` payload contract (`services/calling/src/signaling/push.rs`), docs propose "a Calling PushDispatcher reusing Messaging's adapters" (`docs/operations/profile-notifications-sections-9-11.md:165-179`).
* Meeting Service has its own email notification jobs (`meeting_notifications`, dedupe keys, leases) — a third notification pipeline pattern (`profile-notifications-sections-9-11.md:150-160`).
* Retry/lease/backoff logic duplicated between outbox publisher, push dispatcher and webhook relay.

### Bugs / robustness risks
* **B-1 Duplicate sends across instances.** Push lease is a fixed 60 s, but one pass sends up to 200 jobs sequentially, each with up to a 10 s provider timeout plus a possible Hub policy HTTP call; a slow pass can outlive the lease, letting another instance re-claim and re-send already-sent jobs (completion is token-guarded, the send is not) (`push.rs:401-405,421,429-450`).
* **B-2 Poison batch.** `dispatch_due` decodes all claimed rows into non-optional fields from correlated subqueries (`tenant_id`, `application_id`, `event_payload` from `messaging_outbox_events`, `context_class`); a missing outbox/conversation row would fail the whole pass and leave its batch leased until expiry, repeatedly (`push.rs:296-314,406-420`). Latent today because no outbox purge exists.
* **B-3 Hub outage exhausts attempts fast.** Policy-unavailable counts as a retryable attempt; with 8 attempts and 1 s doubling backoff, a ~2 min Hub outage ends jobs `FAILED` (`push.rs:459,481-484`).
* **B-4 Retry-After ignored** on 429/503.
* **B-5 TTL drift.** Provider TTL is a fresh 24 h at send time although the job already aged (comment claims it "matches the job expiry", `push_adapters.rs:55-56`).
* **B-6 Unvalidated Web Push handles stored while the adapter is off.** Without a Web Push adapter, registration only checks JSON shape + http(s) scheme (`push_adapters.rs:767-773`); on later activation those rows are disabled as INVALID_HANDLE at first send (SSRF is still prevented at send time, `:636-640`).
* **B-7 Missing indexes** for fan-out join `(environment_id, principal_id)` on registrations and `device_id` on jobs.
* **Doc drift**: ADR 0049 §7 ("No APNs / FCM adapter exists"), `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:119` ("no route publishes the VAPID public key"), `outbox.rs:17-20` ("publisher is not started"), `messaging-section-8-delivery.md:50` ("checks device ... but not membership" — repaired later per security review :36).

### Security / privacy
* **P-1 Metadata to Apple/Google in clear.** APNs `cf` dict and FCM `data` carry `conversationId`, `eventSequence`, `sequence` unencrypted: provider can link device tokens to conversation ids and message cadence/volume (for PRIVATE_E2EE too). Web Push encrypts the same metadata. No sender id is sent.
* **P-2 Handles stored in plaintext** (Web Push `auth` secret + `p256dh` + endpoint; APNs/FCM tokens) in `messaging_push_registrations.push_handle`, included in DB backups; never returned by APIs (`owner.rs:7-9,469-490,691`) and never logged (adapters log only status/reason; `kind()` maps transport errors to static strings, `push_adapters.rs:250-258`).
* **P-3 Erasure gap.** `PRINCIPAL_ERASURE` leaves push registrations/jobs (`compliance/deletion.rs:258-265`).
* **P-4 Shared/stale handles.** `push_handle` has no uniqueness; the same browser subscription or token registered by two accounts/devices (shared browser, re-login without revoke-all) receives both accounts' wake-ups; disabled registrations keep the handle forever.
* **P-5 Stale devices.** Registrations are disabled on device revoke, revoke-all and Hub session cutoff, but not on single-session revoke, inactivity, principal suspension, or token age; no periodic revalidation.
* Positive controls: DB CHECK on payload keys, adapter key whitelist, SSRF host allowlist at registration and send, no redirects (`push_adapters.rs:108-114`), credentials read from files only, provider tokens never logged, MS-7 never returns handle or provider error text, LOCAL_TEST refused in staging/production.

---

## 10. Key file index

* `services/messaging/src/realtime/push.rs` — trait, configuration route, registration, revocation hook, fan-out, dispatcher, reauthorization.
* `services/messaging/src/realtime/push_adapters.rs` — APNs, FCM, Web Push, router, validation, unit tests.
* `services/messaging/src/realtime/fanout.rs` — outbox -> push jobs + broker.
* `services/messaging/src/realtime/http.rs` — routes (`:51-72,395-429`).
* `services/messaging/src/http/middleware.rs` — client-token allowlist (`:115-171`), session-device refusal (`:404-420`), delegated mirror (`:690-750`).
* `services/messaging/src/config/mod.rs` — env vars (`:44-48`), parsing (`:916-1016`).
* `services/messaging/src/main.rs` — adapters (`:236-258`), workers (`:367-443`).
* `services/messaging/migration/src/m20261106_000004_realtime.rs` — push tables.
* `services/messaging/src/communications.rs`, `services/messaging/src/communications/owner.rs` — Hub inbox, MS-6/MS-7.
* `services/communications/src/policy.rs:173-208` — mute/preference decision.
* `infra/calling/production/coordinated_release.py:1243-1250,1878-1926,2410-2411` — activation tooling.
* Tests: `services/messaging/tests/{push_adapters,notification_authorization,realtime,communications_owner,authorization_matrix,delegated_mirror}.rs`.
