# Document 4 — Notification Service Backend Specification (proposal)

**Every API, table and event in this document is a NEW PROPOSAL unless marked EXISTING.** Nothing here is implemented or authorized.

| | |
|---|---|
| Service | `cfmeet-notification-service` (service identity `cfmeet.notifications`) |
| Database | `cfmeet_notifications` in `cfmeet-core-db` (table prefix `notif_`) |
| Pattern | Same as Organizations and Developer Platform: own Cargo workspace, `migration/` crate, raw-SQL migrations, `cf-migration-guard`, `cf-service-auth`, Postgres `SKIP LOCKED` workers in-process |
| Scope | **First-party CF Meet users** (personal scope and first-party organizations). Platform customers' Messaging push (`registerPush` with customer credentials) and customer webhooks stay with Messaging and the Developer Platform |

---

## 1. Design principles (binding)

| # | Principle |
|---|---|
| P1 | **Never in a critical path.** NS is never called synchronously by sign-in, OTP, call establishment, message send or webhook delivery. Producers reach NS only through their existing retrying outbox workers. The one exception is the optional Calling push fast path, which is post-commit, best-effort, and capped at 1 s. |
| P2 | **Authority stays with owners.** NS asks the owning service for recipients, membership, account state and preferences. It stores no authority data beyond short-TTL caches keyed by version. |
| P3 | **No secrets, no plaintext, no addresses.** NS never receives or stores: E2EE plaintext or keys; OTP or verification codes; invitation or bearer tokens; email addresses; phone numbers. Email is enqueued **by reference** (`cfmeet-user:<uuid>`), and the Meeting Service resolves the address. Push payloads are metadata-only, as Messaging enforces today. |
| P4 | **Token-bearing messages stay with the token minter.** Meeting and organization invitation emails, OTP, and reset codes are sent by the service that minted the token. |
| P5 | **One delivery authority per (category, channel) at a time.** This is enforced by `notif_channel_authority` plus deterministic transport idempotency keys (§9). |
| P6 | **At-least-once internally, deduplicated at every boundary.** NS does not claim exactly-once *external* delivery. Email is exactly-once *at the ledger*. Push is at-least-once with collapse keys. |
| P7 | **Additive change to existing services.** No existing public contract is removed. Any change to an existing service is additive and individually reversible. |

---

## 2. Capability matrix (Part 7, items 1–27)

| # | Capability | Exists today? | Target owner | Work |
|---|---|---|---|---|
| 1 | Event ingestion | Partial: five per-service outboxes | NS ingest API | **new**. Producers add NS as a destination |
| 2 | Validation and schema versioning | Envelopes v1 exist (`cfmeet.platform.event-envelope`, Hub/Org `schemaVersion:1`) | NS | **new** contract `cfmeet.notifications.event` v1 (§5) |
| 3 | Recipient resolution | Meeting `RECIPIENTS_SQL`, Messaging fan-out, Calling endpoints | **owners** (unchanged); NS asks | integrate |
| 4 | Account and org authorization | Account, Organizations, Platform Access | owners | integrate (recheck at dispatch) |
| 5 | Preferences | Hub (7 categories, mutes) | **Hub** (single writer) | extend additively (§7) |
| 6 | Per-channel policy | absent | NS evaluator + Hub channel matrix | **new** |
| 7 | In-app inbox | **absent** | NS | **new** |
| 8 | Realtime fan-out | Per-service WS (Calling, Messaging, conference) | owners; NS inbox uses polling first | **deferred** (D-6) |
| 9 | Web Push | Messaging adapter (not configured) | `packages/push-transport` | extract (D-1) |
| 10 | FCM | Messaging adapter (not configured) | same | extract |
| 11 | APNs | Messaging adapter (not configured; alert type only; no VoIP) | same, plus a VoIP variant for calls (D-2) | extract + extend |
| 12 | Transactional email orchestration | Meeting ledger + producers | Meeting ledger (transport); NS orchestrates NS-owned categories | integrate (new enqueue API on Meeting) |
| 13 | Durable queues | Postgres `SKIP LOCKED` everywhere | NS `notif_deliveries` | **new** (same pattern) |
| 14 | Retry/backoff | 4 different policies | NS uses `cf-webhooks::retry` semantics (honours `Retry-After`) | reuse |
| 15 | Dedupe/idempotency | ledger `UNIQUE idempotency_key`, push `UNIQUE (event, device)` | NS `UNIQUE` at ingest, notification and delivery | **new** + reuse |
| 16 | Scheduling and reminders | Meeting reminder scan (`enqueue_due_reminders`) | **Meeting keeps the reminder trigger**; NS schedules deferrals (quiet hours, `deliverAfter`) | **new** (internal) |
| 17 | Grouping and coalescing | absent (no collapse keys) | NS `coalesce_key` window + provider collapse IDs | **new** |
| 18 | Expiration and cancellation | ledger `expires_at`; job "still true" checks | NS `expires_at` + `cancelKey` events | **new** |
| 19 | Delivery and read status | ledger statuses, push job status, Messaging cursors | NS deliveries (cross-channel), inbox read state | **new** (inbox read ≠ message read) |
| 20 | Templates and localization | Meeting templates (English only; `users.locale` unused) | NS templates for NS categories; Meeting keeps its own | **new**, locale fallback `en` |
| 21 | Time-zone-aware scheduling | Meeting `format_time` (recipient tz → space tz → UTC) | NS uses Account `users.timezone` via the Hub policy read | **new** |
| 22 | Quiet hours and category rules | **absent** | Hub stores, NS enforces | **new** |
| 23 | Cross-device sync | Messaging read-cursor sync; no push clear | NS inbox read state + `notifications.inbox.changed` | **new** |
| 24 | History and retention | scattered | NS (90-day inbox default) | **new** |
| 25 | Metrics and auditing | per-service counters; no alerts deployed | NS `notif_*` metrics + rules file | **new** |
| 26 | Provider failure handling | per-adapter classification exists | reuse adapter classification; NS circuit-breaks per provider | reuse + **new** breaker |
| 27 | Security and privacy enforcement | strong in Messaging and Calling payloads | NS §8 | **new** (same rules) |

---

## 3. API catalogue

The fields listed for each endpoint follow Part 12.

**Conventions for every NS API:**
- Errors use the existing CF Meet shape `{ "error": { "code", "message", "requestId" } }`.
- Common error codes: `VALIDATION_ERROR` 400, `AUTHENTICATION_REQUIRED` 401, `INTERNAL_AUTHENTICATION_FAILED` 401, `FORBIDDEN` 403, `NOT_FOUND` 404, `VERSION_CONFLICT` 409, `IDEMPOTENCY_CONFLICT` 409 (same key, different body), `RATE_LIMITED` 429 with `Retry-After`, `UNAVAILABLE` 503.
- Mutations require an `Idempotency-Key` (1–128 chars). Keys are stored for 24 h, as in the Hub and Organizations.
- Pagination uses an opaque cursor: `limit` 1–100 (default 30), `cursor` = base64url(`created_at`,`id`). The response carries `nextCursor | null`.

### 3.1 User APIs (5 NEW), via the Meeting Service relay

**Transport:**
- Clients call `https://api.cfmeet.com/api/v2/notifications/*` with the Account bearer token.
- The Meeting Service verifies the session (exactly as `/api/v2/communications/*` does today, `services/meeting-service/src/routes/mod.rs:86-89`).
- It then forwards `POST /internal/v1/notifications/dispatch` with a `VerifiedAccount` (`packages/communications-policy/src/lib.rs:225-260`).

**Why the relay:**
- production nginx sends `/api/v2/*` straight to the Meeting Service;
- the API gateway's WebSockets and Meeting families are not routed;
- mobile may only call routes in `packages/contracts/openapi/meeting-service.json` (`apps/mobile/tests/routes.test.ts`).

**Owner:** NS (contract `cfmeet.notifications.http` v1). The operations are also added to the meeting-service OpenAPI so that mobile can adopt them.

| Op | Method & path | operationId | Auth / scope | Request | Response | Validation | Idempotency | Rate limit (per account) |
|---|---|---|---|---|---|---|---|---|
| U1 | GET `/api/v2/notifications` | `listMyNotifications` | Account session (`MEETING_SESSION`) | query: `cursor`, `limit`, `state=unread\|all` (default all), `category` (repeatable) | `{ items: [NotificationItem], nextCursor, unreadCount }` | category ∈ catalogue; `limit` 1–100 | n/a (GET) | 120/min |
| U2 | GET `/api/v2/notifications/unread-count` | `getMyNotificationUnreadCount` | same | — | `{ unreadCount, latestCreatedAt, version }`; `ETag`, honours `If-None-Match` → 304 | — | n/a | 240/min (polling target) |
| U3 | PATCH `/api/v2/notifications/{notificationId}` | `updateMyNotification` | same; item must belong to the caller (else 404, never 403) | `{ read?: bool, dismissed?: bool, expectedVersion: int }` | `NotificationItem` | at least one field | `Idempotency-Key` required; same body → same result | 60/min |
| U4 | POST `/api/v2/notifications/read` | `markMyNotificationsRead` | same | `{ ids?: [uuid ≤100] , upTo?: cursor }` (exactly one) | `{ updated: int, unreadCount }` | — | required | 30/min |
| U5 | POST `/api/v2/notifications/dismiss` | `dismissMyNotifications` | same | `{ ids?: [uuid ≤100], upTo?: cursor, category?: [..] }` | `{ dismissed: int, unreadCount }` | — | required | 30/min |

**`NotificationItem` schema:**

```json
{
  "id": "uuid",
  "category": "CALL_MISSED",
  "titleKey": "call.missed.title",
  "title": "Missed call",
  "body": "Ada Lovelace tried to call you",
  "action": { "kind": "OPEN_CALL_HISTORY", "ref": "uuid" },
  "createdAt": "RFC3339",
  "readAt": null,
  "dismissedAt": null,
  "groupCount": 1,
  "version": 3
}
```

- `action.kind` is a **closed enum** (`OPEN_CALL_HISTORY`, `OPEN_VOICEMAIL`, `OPEN_CONVERSATION`, `OPEN_CONTACT_REQUESTS`, `OPEN_ORGANIZATION`, `OPEN_MEETING`, `OPEN_DEVELOPER_CONSOLE`, `OPEN_SECURITY_EVENTS`, `NONE`). `ref` is a UUID. **No URLs are ever returned.** The client maps the kind to its own route. This removes any open-redirect or dangerous-link risk.
- Text is rendered server-side from templates with HTML-escaped parameters. For PRIVATE_E2EE no message text is ever present.

**Web/mobile consumption:**
- Web: a bell with a badge from U2, polled every 30 s with ETag while visible and paused when hidden; list from U1; mark read on open (U3/U4).
- Mobile: the same, once the routes are added to `meeting-service.json`.

**Compatibility:** U1–U5 are new paths. Nothing existing changes. The Hub `/activity`, `/sync` and `/inbox` keep their meaning. Account `/api/v2/me/security-events` stays the security audit view.

### 3.2 Preferences: EXISTING APIs, additively extended (no new endpoint)

| | |
|---|---|
| Endpoints | EXISTING `GET/PUT /api/v2/communications/me/notification-preferences` (`getNotificationPreferences` / `updateNotificationPreferences`, Hub contract `cfmeet.communications.http` 1.3.0 → 1.4.0) |
| Owner | Communications Hub (single writer) |
| Additive request/response fields (all optional on PUT; absent = unchanged) | `channels`: `{ <category>: { push: bool, email: bool, inApp: bool } }`; `quietHours`: `{ enabled: bool, start: "HH:MM", end: "HH:MM", days: [MON..SUN] }` (time zone from Account `users.timezone`, returned read-only as `timeZone`); new category booleans `voicemail`, `organizationUpdates`, `developerUpdates`, `meetingReminders` (default **true**: preserves current behaviour) |
| Unchanged | `expectedVersion` optimistic concurrency, `Idempotency-Key`, `deny_unknown_fields` (the extension adds known fields), `securityEvents:true` (read-only, now meaningful: security is non-suppressible) |
| Compatibility | Old clients that send the 7 legacy booleans keep working. Absent `channels` means "all channels that exist for the category" (current behaviour). Absent `quietHours` means disabled |

### 3.3 Internal APIs on the Notification Service (7 NEW + 2 conditional)

All use `cf-service-auth`:
- an Ed25519 single-audience token;
- the receiver-authoritative trust bundle;
- durable `jti` nonces in `notif_service_nonces` (multi-instance replay guard);
- request binding accepted (`SERVICE_AUTH_REQUEST_BINDING_MODE=accept`, moving to `require` when SEC-012 step 2 lands).

New permissions are added to `packages/service-auth/src/permissions.rs` `REGISTRY` with receiver `cfmeet.notifications`.

| Op | Method & path | operationId | Permission (grantable_to) | Request / response | Validation / errors | Idempotency | Limit |
|---|---|---|---|---|---|---|---|
| I1 | POST `/internal/v1/notification-events` | `ingestNotificationEvents` | `notifications-event.publish` (meeting-service, organizations, communications, calling, messaging, developer-platform) | `{ events: [NotificationEvent ≤100] }` → `{ results: [{eventId, outcome: ACCEPTED\|DUPLICATE\|REJECTED, reason?}] }` | per-event schema validation (§5); `producer` must equal the token issuer; unknown `eventType` → REJECTED `UNKNOWN_EVENT_TYPE` (never 500); payload ≤ 4 KiB | `(producer, eventId)` + SHA-256 request digest; same id with a different digest → REJECTED `IDEMPOTENCY_CONFLICT` | 50 req/s per producer; 503 + `Retry-After` under backpressure (§10.3) |
| I2 | POST `/internal/v1/notifications/dispatch` | `dispatchNotificationsUser` | `notifications-user.forward` (meeting-service) | `{ method, path, query, body, idempotencyKey, account: VerifiedAccount }` → the U1–U5 response | path ∈ U1–U5 allowlist | per U-op | — |
| I3 | GET `/internal/v1/admin/deliveries` | `listNotificationDeliveries` | `notifications-observability.read` (operator-console) | query `notificationId \| deliveryId \| eventId \| category`, `from`, `to`, cursor → PII-free rows (ids, category, channel, status, attempts, error class, timestamps; no recipient ref unless `notificationId` is given) | — | GET | 10/s |
| I4 | GET `/internal/v1/admin/dead-letters` | `listNotificationDeadLetters` | same | filters + cursor → FAILED deliveries and REJECTED events | — | GET | 10/s |
| I5 | POST `/internal/v1/admin/dead-letters/replay` | `replayNotificationDeadLetters` | `notifications-operations.replay` (operator-console; sensitive) | `{ deliveryIds ≤100 }` or `{ eventIds ≤100 }` → `{ replayed }` | refuses expired or superseded items; refuses channels whose authority is not ACTIVE for NS | `Idempotency-Key` | audited |
| I6 | GET `/internal/v1/admin/shadow-report` | `getNotificationShadowReport` | `notifications-observability.read` | `from`, `to`, `category` → `{ matched, missingInNs, missingInLegacy, recipientMismatch, policyMismatch, p50/p95 lag }` (counts only) | — | GET | — |
| I7 | GET `/internal/v1/admin/channel-authority` | `getNotificationChannelAuthority` | `notifications-observability.read` | → `[{category, channel, mode, fenceAt, version, changedByRun}]` | read-only. Changes happen **only** through coordinated-release activation operations (audited, journaled) | GET | — |
| I8 *(D-1)* | POST `/internal/v1/push-endpoints` | `applyPushEndpoint` | `notifications-push-endpoint.apply` (communications) | `{ op: UPSERT\|REMOVE, accountRef, accountDeviceRef, platform: APNS\|APNS_VOIP\|FCM\|WEB_PUSH, handle }` → `{ pushTargetRef, platform, enabled }` (**never echoes the handle**) | handle validated per platform (reuse `push_adapters.rs:492-534, 761-807` incl. the SSRF host allowlist); the device must be ACTIVE in Account | `(accountDeviceRef, platform)` upsert | 10/min per device |
| I9 *(D-1)* | POST `/internal/v1/push/dispatch` | `dispatchPush` | `notifications-push.dispatch` (calling, messaging) | `{ pushTargetRefs ≤20, kind: CALL_INVITATION\|MESSAGE_WAKEUP\|NOTIFICATION, payload (schema per kind), collapseId, ttlSeconds ≤ 86400, priority }` → `{ results: [{pushTargetRef, outcome}] }` | payload must validate against `cfmeet.calling.push-invitation` v1 or the Messaging metadata whitelist; anything else → 400 | `(collapseId, pushTargetRef)` within the TTL | Calling: **synchronous, ≤ 1 s budget, no queue** (P1); Messaging: may enqueue |

**Not provided as endpoints, by design (Part 12: "do not create endpoints just to increase the API count"):**
- **Template resolution:** internal to NS. Templates are compiled into the binary and versioned with the contract.
- **Scheduling and cancellation:** expressed in the event contract (`deliverAfter`, `expiresAt`, `cancelKey`, event type `notifications.cancel`), so producers keep a single integration point (I1).
- **Health:** `GET /health` (process) and `GET /ready` (DB + migrations + worker heartbeat + outbound breaker state), as on every service. Metrics are served on a private listener.

### 3.4 New internal APIs on existing services (3 NEW)

| Op | Owner | Method & path | operationId | Permission | Purpose |
|---|---|---|---|---|---|
| X1 | Meeting Service | POST `/internal/v1/email/deliveries` | `enqueueEmailDelivery` | `email-delivery.enqueue` (grantable to notifications) | `{ category ∈ NS-allowed set, recipientRef: cfmeet-user:<uuid> \| meeting-email-principal:<uuid>, templateKey, params, idempotencyKey, legacyNoticeRef?, expiresAt }` → `{ deliveryId, created }`. **Meeting resolves the address** (NS never sees it). Meeting renders through its own template layer (wrap/escape). `legacyNoticeRef` makes Meeting compute the *legacy* key `meeting-notification:{job}:{sha256(lower(addr))}` (`notifications.rs:1037-1038`), so dual-run is exact-once at `UNIQUE (idempotency_key)`. Refuses `AUTH_*`/`SECURITY_*` categories (P4). Wraps the existing `email_delivery::enqueue` (`email_delivery.rs:124-185`) |
| X2 | Meeting Service | GET `/internal/v1/email/deliveries/{id}` | `getEmailDelivery` | `email-delivery.read` | PII-free status (same fields as today's `/internal/operations/email/deliveries/{id}`, `handlers/email.rs`), but with service auth instead of the shared `INTERNAL_API_TOKEN` |
| X3 | Communications Hub | POST `/internal/v1/communications/notification-policy/resolve` | `resolveNotificationPolicy` | `communications-preferences.read` (**extend `grantable_to` to notifications**; fix the missing Meeting grant in N0) | `{ category, accountRefs ≤100, actorRef?, conversationId? }` → `{ schemaVersion: 2, results: [{accountRef, allowed, channels: {push, email, inApp}, quietHours: {active, endsAt}, timeZone, reason}] }`. One call per batch. Evaluates category, channel matrix, contact mute, conversation mute, blocks and quiet hours **inside the Hub**. Cost ≤ 3 signed calls per batch, versus ~6 per recipient for `policy/check` (§10.4). The existing `resolveNotificationPreferences` stays (Meeting uses it until Phase 4.2) |

### 3.5 EXISTING APIs reused unchanged

| API | Reuse |
|---|---|
| Hub `PUT/DELETE /api/v2/communications/devices/{id}/push` | The single first-party push registration API. Under D-1 the Hub forwards to I8 instead of Messaging `/internal/v1/communications/devices/push` (Phase 4.3) |
| Hub `GET /api/v2/communications/me/notification-delivery-status` | Gains NS channel status (additive fields) |
| Calling `POST /v1/calling/endpoints` (`pushTargetRef`) | The client sets `pushTargetRef` = the value returned by the Hub push facade (D-1) |
| Platform Access `POST /internal/v1/identity/changes` | NS polls it (permission `platform-identity.read`), as Calling and Messaging do, to drop suspended or tombstoned principals |
| Organizations `POST /internal/v1/organization-events` contract | NS implements the **consumer** side (same contract as the Meeting Service) |

---

## 4. Recipient resolution (by category)

| Category | Recipient authority | How NS obtains recipients | Recheck at dispatch |
|---|---|---|---|
| Meeting notices (reschedule, cancel, reminder) | Meeting Service (`RECIPIENTS_SQL`, `notifications.rs:892-914`) | the event carries `recipientRefs` (≤ 200, computed by Meeting in the same pass that writes the job); protected titles applied by Meeting (`presented_title`) | the account is active (Hub policy result) |
| Org invitation (in-app copy) | Organizations | D-5: Organizations read `invitation → inviteeAccountRef?` | invitation still PENDING (Org read) |
| Org membership changes | Organizations | event `aggregateId` + payload `userId` (Org contract) | — |
| Contact request / accepted | Hub | event payload `targetProfile` → `accountRef` | Hub policy (block / privacy) |
| Message request / group invitation | Messaging | event payload member principal → account ref via Hub `principals/resolve` | Messaging membership still INVITED |
| Missed call | Calling | new derived fact `CALL_PARTICIPANT_MISSED` (per participant, ADR 0025 rule applied in Calling) | — |
| Voicemail | Calling (mailbox owner may be EXTENSION/QUEUE/RING_GROUP) | Calling resolves owners to principal refs in the outbox fact (it has the enterprise data) | — |
| Recording ready | Calling (`call_recording_authorizations`) | the fact carries authorized principal refs | — |
| Developer notices | Developer Platform | event carries `developerAccountRefs` (organization admins / applicant) | — |
| Account security copies (optional, Phase 4.4) | Account | event `accountRef` | — (security is always allowed) |

---

## 5. Event contracts

### 5.1 Envelope `cfmeet.notifications.event` v1 (NEW)

This envelope aligns with `cfmeet.platform.event-envelope`.

```json
{
  "schemaVersion": 1,
  "eventId": "evt_<32hex> | uuid",
  "eventType": "calling.call.participant-missed",
  "eventVersion": 1,
  "producer": "cfmeet.calling",
  "occurredAt": "RFC3339",
  "correlationId": "corr_<32hex>",
  "scope": { "tenantId": "uuid", "applicationId": "uuid", "environmentId": "uuid" },
  "subject": { "kind": "CALL", "id": "uuid", "version": 7 },
  "recipients": [{ "ref": "cfmeet-user:<uuid>" }],
  "delivery": {
    "dedupeKey": "string<=200",
    "coalesceKey": "string<=200|null",
    "deliverAfter": "RFC3339|null",
    "expiresAt": "RFC3339|null",
    "cancelKey": "string<=200|null",
    "priority": "NORMAL|HIGH"
  },
  "data": { }
}
```

**Validation:**
- JSON object ≤ 4 KiB.
- `data` keys must match `^[a-z][a-zA-Z0-9]{0,47}$`. Keys matching `secret|private|token|ciphertext|plaintext|content|body|password|email|phone|key` are **refused**. This mirrors Messaging's CHECK at `m20261106_000004_realtime.rs:139-140`.
- Strings ≤ 200 chars, with control characters stripped.
- `recipients` ≤ 200.
- `producer` must equal the authenticated service identity.
- `eventType` must be registered for that producer (closed per-producer allowlist, as in Developer Platform `catalog.rs:29-52`).

**Versioning:**
- `schemaVersion` is the envelope version.
- `eventVersion` is per type.
- NS accepts the current and previous `eventVersion` per type. A new major version is a new contract entry in `packages/contracts/manifest.json`.

### 5.2 Inbound event catalogue (24 types)

| # | eventType | Producer | Source (existing → change) | Category |
|---|---|---|---|---|
| 1-5 | `organizations.invitation.{created,accepted,declined,revoked,expired}` | Organizations | `organization_outbox`, events exist → add a subscriber row for `cfmeet.notifications` (`outbox.rs:41-59`) | ORG_INVITATION / ORG_INVITATION_RESPONSE |
| 6-8 | `organizations.membership.{role_changed,suspended,removed}` | Organizations | exist → subscriber | ORG_MEMBERSHIP |
| 9 | `organizations.ownership.transferred` | Organizations | exists → subscriber | ORG_MEMBERSHIP |
| 10 | `organizations.organization.suspended` | Organizations | exists → subscriber | ORG_MEMBERSHIP |
| 11 | `communications.connection.requested` | Hub | `communication_outbox`, exists → add destination `NOTIFICATIONS` (CHECK change) | CONTACT_REQUEST |
| 12 | `communications.connection.connected` | Hub | exists → destination | CONTACT_ACCEPTED |
| 13 | `messaging.member.invited` | Messaging | outbox event exists → add NS publication | MESSAGE_REQUEST / GROUP_INVITATION |
| 14 | `calling.call.participant-missed` | Calling | **new derived fact** (additive event type; no business-rule change) | CALL_MISSED |
| 15 | `calling.voicemail.ready` | Calling | `VOICEMAIL_READY` exists → add notifications destination with owner refs | VOICEMAIL |
| 16 | `calling.recording.ready` | Calling | `RECORDING_READY` exists → destination with authorized refs | RECORDING_READY |
| 17-19 | `meeting-service.notice.{rescheduled,cancelled,reminder}` | Meeting | `meeting_notifications` rows exist (same transaction) → the worker forwards instead of rendering (Phase 4.2) | MEETING_CHANGE / MEETING_REMINDER |
| 20-21 | `account.security.{device-linked,password-changed}` | Meeting (Account) | `account_security_events` exist → optional in-app copy (Phase 4.4) | ACCOUNT_SECURITY |
| 22-23 | `developer-platform.credential.{expiring-soon,expired}` | Developer Platform | lifecycle sweep exists (webhook) → add a human-notice publication | DEVELOPER_CREDENTIAL |
| 24 | `developer-platform.production-access.decided` | Developer Platform | activity row exists → publication | DEVELOPER_ACCESS |

Control event: `notifications.cancel` (any producer, matching `cancelKey`). It cancels pending deliveries and dismisses unread inbox items for that key, for example a meeting cancelled before its reminder.

### 5.3 Outbound events (1)

`notifications.inbox.changed` `{ accountRef, unreadCount, version }`. It is emitted to an internal stream only, as a future realtime nudge (D-6). It never carries content.

---

## 6. Data design (NEW tables; database `cfmeet_notifications`)

Each migration follows `docs/architecture/migration-conventions.md`:
- raw SQL `UP`/`DOWN`;
- history table `notifications_seaql_migrations`;
- prefix `notif_`;
- append-only triggers where marked;
- the server refuses to start with pending migrations.

```sql
-- m20270101_000001_notifications_foundation (illustrative date)

CREATE TABLE notif_event_inbox (            -- ingest dedupe (append-only)
  producer        VARCHAR(64)  NOT NULL CHECK (producer ~ '^cfmeet\.[a-z][a-z0-9-]*$'),
  event_id        VARCHAR(64)  NOT NULL,
  request_digest  CHAR(64)     NOT NULL,
  event_type      VARCHAR(96)  NOT NULL,
  event_version   SMALLINT     NOT NULL,
  received_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
  state           VARCHAR(16)  NOT NULL CHECK (state IN ('PENDING','PROCESSED','REJECTED','SHADOWED')),
  reject_reason   VARCHAR(64),
  envelope        JSONB        NOT NULL CHECK (pg_column_size(envelope) <= 4096),
  processed_at    TIMESTAMPTZ,
  lease_id UUID, lease_until TIMESTAMPTZ, attempts INT NOT NULL DEFAULT 0,
  PRIMARY KEY (producer, event_id)
);
CREATE INDEX ix_notif_event_inbox_due ON notif_event_inbox (received_at) WHERE state = 'PENDING';

CREATE TABLE notif_notifications (          -- one logical notification per recipient
  id              UUID PRIMARY KEY,
  recipient_ref   VARCHAR(64)  NOT NULL CHECK (recipient_ref ~ '^cfmeet-user:[0-9a-f-]{36}$'),
  category        VARCHAR(40)  NOT NULL,
  dedupe_key      VARCHAR(200) NOT NULL,
  coalesce_key    VARCHAR(200),
  cancel_key      VARCHAR(200),
  source_producer VARCHAR(64)  NOT NULL,
  source_event_id VARCHAR(64)  NOT NULL,
  subject_kind    VARCHAR(32), subject_id UUID,
  template_key    VARCHAR(64)  NOT NULL,
  params          JSONB        NOT NULL CHECK (pg_column_size(params) <= 2048),
  priority        VARCHAR(8)   NOT NULL DEFAULT 'NORMAL',
  policy_snapshot JSONB        NOT NULL,      -- decision + reasons + Hub prefs version (audit)
  state           VARCHAR(16)  NOT NULL CHECK (state IN ('ROUTED','SUPPRESSED','CANCELLED','EXPIRED','SHADOW')),
  deliver_after   TIMESTAMPTZ, expires_at TIMESTAMPTZ,
  created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
  UNIQUE (recipient_ref, dedupe_key)
);
CREATE INDEX ix_notif_notifications_cancel ON notif_notifications (cancel_key) WHERE cancel_key IS NOT NULL;

CREATE TABLE notif_deliveries (             -- one per (notification, channel, target)
  id              UUID PRIMARY KEY,
  notification_id UUID NOT NULL REFERENCES notif_notifications(id) ON DELETE CASCADE,
  channel         VARCHAR(16) NOT NULL CHECK (channel IN ('EMAIL','PUSH','IN_APP')),
  target_ref      VARCHAR(80) NOT NULL,       -- 'account' | push endpoint id
  status          VARCHAR(20) NOT NULL CHECK (status IN
                  ('PENDING','DEFERRED','DISPATCHING','HANDED_OFF','DELIVERED','FAILED','EXPIRED','CANCELLED','SUPPRESSED','SHADOW')),
  attempts        INT NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 64),
  next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  lease_id UUID, lease_until TIMESTAMPTZ,
  CHECK ((status = 'DISPATCHING') = (lease_id IS NOT NULL)),
  transport_ref   VARCHAR(128),               -- e.g. Meeting email delivery id
  last_error_class VARCHAR(64),
  authority_version INT NOT NULL,             -- channel-authority version at creation (fence)
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), completed_at TIMESTAMPTZ,
  UNIQUE (notification_id, channel, target_ref)
);
CREATE INDEX ix_notif_deliveries_due ON notif_deliveries (next_attempt_at) WHERE status IN ('PENDING','DEFERRED');

CREATE TABLE notif_delivery_attempts (      -- append-only
  delivery_id UUID NOT NULL REFERENCES notif_deliveries(id) ON DELETE CASCADE,
  attempt     INT  NOT NULL,
  started_at  TIMESTAMPTZ NOT NULL, finished_at TIMESTAMPTZ,
  outcome     VARCHAR(16) NOT NULL CHECK (outcome IN ('OK','RETRY','PERMANENT','BREAKER_OPEN')),
  error_class VARCHAR(64), http_status INT,
  PRIMARY KEY (delivery_id, attempt)
);

CREATE TABLE notif_inbox_items (            -- user-visible history
  id              UUID PRIMARY KEY,           -- = notification id
  recipient_ref   VARCHAR(64) NOT NULL,
  category        VARCHAR(40) NOT NULL,
  template_key    VARCHAR(64) NOT NULL,
  params          JSONB NOT NULL CHECK (pg_column_size(params) <= 2048),
  action_kind     VARCHAR(32) NOT NULL, action_ref UUID,
  group_key       VARCHAR(200), group_count INT NOT NULL DEFAULT 1,
  silent          BOOLEAN NOT NULL DEFAULT false, -- muted/category-off: recorded, not counted
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  read_at TIMESTAMPTZ, dismissed_at TIMESTAMPTZ, expires_at TIMESTAMPTZ NOT NULL,
  version         BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX ix_notif_inbox_recipient ON notif_inbox_items (recipient_ref, created_at DESC, id DESC) WHERE dismissed_at IS NULL;
CREATE INDEX ix_notif_inbox_unread    ON notif_inbox_items (recipient_ref) WHERE read_at IS NULL AND dismissed_at IS NULL AND silent = false;
CREATE UNIQUE INDEX uq_notif_inbox_group ON notif_inbox_items (recipient_ref, group_key) WHERE group_key IS NOT NULL AND read_at IS NULL AND dismissed_at IS NULL;

CREATE TABLE notif_channel_authority (      -- single authority per (category, channel)
  category VARCHAR(40) NOT NULL, channel VARCHAR(16) NOT NULL,
  mode     VARCHAR(8)  NOT NULL CHECK (mode IN ('OFF','SHADOW','ACTIVE')),
  fence_at TIMESTAMPTZ NOT NULL, version INT NOT NULL,
  changed_by_run VARCHAR(64) NOT NULL, changed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (category, channel)
);

CREATE TABLE notif_shadow_observations (    -- temporary (dropped in Phase 8)
  id BIGSERIAL PRIMARY KEY, category VARCHAR(40) NOT NULL, dedupe_key VARCHAR(200) NOT NULL,
  side VARCHAR(8) NOT NULL CHECK (side IN ('LEGACY','NS')), recipient_digest CHAR(64) NOT NULL,
  decision VARCHAR(16) NOT NULL, observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (category, dedupe_key, side, recipient_digest)
);

CREATE TABLE notif_service_nonces (jti VARCHAR(64) PRIMARY KEY, issuer VARCHAR(64) NOT NULL, expires_at TIMESTAMPTZ NOT NULL);

CREATE TABLE notif_deferrals (              -- quiet hours / deliverAfter / coalesce windows
  delivery_id UUID PRIMARY KEY REFERENCES notif_deliveries(id) ON DELETE CASCADE,
  release_at TIMESTAMPTZ NOT NULL, reason VARCHAR(24) NOT NULL CHECK (reason IN ('QUIET_HOURS','DELIVER_AFTER','COALESCE'))
);
CREATE INDEX ix_notif_deferrals_due ON notif_deferrals (release_at);

-- conditional on D-1
CREATE TABLE notif_push_endpoints (
  id UUID PRIMARY KEY,                        -- = pushTargetRef
  account_ref VARCHAR(64) NOT NULL, account_device_ref UUID NOT NULL,
  platform VARCHAR(12) NOT NULL CHECK (platform IN ('APNS','APNS_VOIP','FCM','WEB_PUSH')),
  handle_sealed BYTEA NOT NULL, handle_key_id VARCHAR(32) NOT NULL,   -- AES-256-GCM via cf-webhooks secret_box
  handle_digest CHAR(64) NOT NULL,            -- keyed HMAC, for uniqueness without plaintext
  enabled BOOLEAN NOT NULL DEFAULT true, disabled_reason VARCHAR(48),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (account_device_ref, platform),
  UNIQUE (handle_digest)                      -- one handle → one device (fixes the shared-handle leak, Messaging P-4)
);
```

**Retention** (retention worker, hourly, batched):

| Data | Retention |
|---|---|
| Inbox items | `expires_at` = created + 90 d (configurable 7–365) |
| Deliveries and attempts | 30 d after completion |
| Notifications | deleted when no inbox item and no delivery remains |
| Event inbox | 30 d (the dedupe window; producers never replay older events) |
| Nonces | at `expires_at` |
| Shadow observations | 30 d |

Account erasure (Account `ACCOUNT_DISABLED`/erasure event through the Hub, or Platform Access tombstone through the change feed) deletes every row for that `recipient_ref` within 24 h.

**Sensitive fields:**
- `params` holds display names, organization names and **protected** meeting titles only (already sanitized by the producer).
- `handle_sealed` is encrypted at rest. Messaging today stores handles in plaintext; NS does not.
- NS holds no email addresses.

---

## 7. Preferences and policy (Part 8)

### 7.1 Model (Hub remains the single writer)

The existing data is kept as is:
- the 7 category booleans (defaults true; an absent row means all true);
- conversation mute (`ALL`/`MENTIONS`/`MUTED`, `muted_until`);
- contact mute (`ALL`/`MUTED`/`MENTIONS`);
- the privacy table.

**Additive Hub migration** (`communications` history; no default changes):

```sql
ALTER TABLE communication_notification_preferences
  ADD COLUMN voicemail BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN organization_updates BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN developer_updates BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN meeting_reminders BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN channel_overrides JSONB NOT NULL DEFAULT '{}'::jsonb
      CHECK (jsonb_typeof(channel_overrides) = 'object' AND pg_column_size(channel_overrides) <= 2048),
  ADD COLUMN quiet_hours JSONB
      CHECK (quiet_hours IS NULL OR jsonb_typeof(quiet_hours) = 'object');
ALTER TABLE communication_outbox DROP CONSTRAINT <destination_check>,
  ADD CONSTRAINT ck_communication_outbox_destination CHECK (destination IN ('CALLING','MESSAGING','NOTIFICATIONS'));
```

- **Time zone:** read from Account `users.timezone` (Meeting Service; Section 9). The Hub returns it in the X3 answer. NS does not store it.
- **Organization policy:** none exists today. This is reserved for a later `organization notification policy` owned by Organizations (D-7). The precedence slot is defined now so that adding it later does not reorder the rules.

### 7.2 Precedence (evaluated top to bottom; first terminal rule wins)

| # | Rule | Effect | Applies to |
|---|---|---|---|
| 1 | Recipient no longer authorized: account inactive or erased, principal suspended or tombstoned, membership revoked, device revoked | **drop** (record SUPPRESSED, reason) | all |
| 2 | Operator hold / legal restriction (Hub `communication_operator_holds`) | drop, except ACCOUNT_SECURITY | all |
| 3 | **Security-critical** category (ACCOUNT_SECURITY) | **always deliver**: email by Account (unchanged), in-app copy. Ignores mute, category, channel and quiet hours. Never coalesced or deferred | security |
| 4 | Recipient blocked the actor (Hub + domain blocks) | drop | social categories (message, contact, call, invitation from a person) |
| 5 | Organization policy (future, D-7) | may force or forbid channels | ORG_* categories in that org |
| 6 | Privacy restrictions (e.g. `contact_request_permissions=NOBODY`) | drop (defence in depth; the owner already enforces this at action time) | contact, call |
| 7 | Category preference **off** | no push or email; in-app item recorded `silent=true` (not counted in unread) | non-security |
| 8 | Channel preference off for the category | remove that channel | non-security |
| 9 | Contact mute / conversation mute (active) | no push; in-app `silent=true`. `MENTIONS` = allow only if the producer flags `mentionsRecipient=true` (MANAGED only; PRIVATE_E2EE cannot, so it behaves as MUTED, as today) | messages, calls from that contact |
| 10 | Quiet hours active (recipient time zone) | defer push to the quiet-hours end (`notif_deferrals`), unless `expiresAt` comes first, in which case the push is dropped and in-app is kept. Email is not deferred (asynchronous anyway) | non-security, non-HIGH |
| 11 | Device state: no enabled push endpoint, or the endpoint is disabled | no push | push |
| 12 | Coalescing (`coalesce_key` window, default 10 min) and per-recipient rate cap (default 30 pushes/h) | merge into the group item; collapse ID on push | non-security |

**Examples:**
- An ordinary chat mute (rule 9) suppresses a push. A password-changed notice (rule 3) is still emailed and shown in-app.
- A blocked contact's missed call (rule 4) produces nothing.
- A meeting reminder at 23:00 during quiet hours: the email is sent, and the push is deferred to 07:00 unless the meeting has already started.

**Defaults:** unchanged. No existing preference value or default is modified. Every new field defaults to the current behaviour.

---

## 8. Security and privacy (Part 9: threat model)

| Threat | Protection (specified) | Reuses |
|---|---|---|
| Cross-account / cross-tenant leakage | Every user API filters by `VerifiedAccount.accountRef`. A foreign ID gives 404 (never 403). Every event carries `scope`, validated against the producer's grant; NS stores `recipient_ref` only after the owner resolved it. Add the authorization-matrix test pattern (`services/messaging/tests/authorization_matrix.rs`) for U1–U5 and I1–I9 | Messaging matrix pattern |
| Forged service events | `cf-service-auth` Ed25519, one audience, receiver trust bundle, `producer == issuer`, per-producer event-type allowlist, durable `jti` | ADR 0004 |
| Unauthorized recipient lookup | NS exposes no lookup API. Recipients come only from owners. Admin APIs return no recipient unless queried by exact notification ID (audited) | — |
| Device-token theft | Handles sealed with AES-256-GCM (`cf-webhooks::secret_box`) and never returned by any API. `UNIQUE handle_digest` (keyed HMAC) stops one handle serving two accounts. Endpoints disabled on device revoke, sign-out-everywhere, account session end, principal suspension and erasure | `packages/webhooks/src/secret_box.rs` |
| Spam / abuse | Per-producer ingest limits. Per-recipient caps (rule 12). Coalescing. User API rate limits (§3.1). Contact-request notices respect Hub cooldown rules (owner-enforced) | — |
| Replay | `(producer, eventId)` + request digest; `jti` nonces; `dedupe_key` uniqueness per recipient | Hub pattern |
| Duplicate dispatch | `UNIQUE (notification_id, channel, target_ref)`. The lease is renewed per item, and the lease length is ≥ 3× the worst-case item time (fixes the Messaging B-1 / Meeting S-8 class). Email exact-once at the ledger `UNIQUE (idempotency_key)`. Push collapse IDs | — |
| E2EE metadata exposure | NS never handles message events. Messaging decides wake-ups and sends only the 5-key metadata payload. Push for other categories carries `{v, type, category, ref}` only, plus an optional caller display name for calls (already allowed by `cfmeet.calling.push-invitation` v1). Recommend Web Push (encrypted) where possible; note that APNs and FCM see metadata in clear (residual, documented) | Messaging whitelist |
| Stale permissions after device revocation | Dispatch-time recheck against the endpoint `enabled` flag. Revocation events from the Account → Hub outbox disable endpoints in the same NS transaction. Change-feed poll for principal status | Hub events, Platform Access feed |
| Deleted users / revoked org members | Rule 1. Org `membership.removed` cancels pending ORG_* deliveries for that recipient (`cancelKey`) | — |
| Webhook forgery | N/A: NS receives no external webhooks. Provider callbacks stay in the Meeting Service (Svix verification) | — |
| Provider credential exposure | Credentials are files mounted 0400, read at startup, never logged. The release tooling refuses provider variables until activation (as `coordinated_release.py:1243-1250` does) | release safe defaults |
| Dangerous action links | No URLs in inbox items or push: closed `action.kind` enum + UUID. Email links are built by Meeting from `APP_URL` with fixed paths | — |
| Sensitive data in logs | Log only IDs, category, channel, status and error class. Never params, handles or recipient refs at info level. A CI test greps for tracing of `params`/`handle` (like `test_flags_and_container_summaries_carry_no_secret_values`) | — |
| Retention and deletion | §6 retention. Erasure within 24 h. Shadow data dropped at Phase 8 | — |

---

## 9. Single-authority mechanism (used by Document 5)

`notif_channel_authority(category, channel)` has three modes:
- **OFF:** NS ignores the category.
- **SHADOW:** NS computes, writes `SHADOW` rows and `notif_shadow_observations`, and **never calls a transport**.
- **ACTIVE:** NS delivers.

The legacy producer reads the *same* switch value through its configuration. Each change is made by one coordinated-release operation that sets both sides in one journaled step.

| Channel | Legacy side | How double sending is prevented |
|---|---|---|
| Email (meeting notices) | Meeting notification worker | Both sides enqueue with the **same deterministic idempotency key** (X1 `legacyNoticeRef`) into the same ledger: `UNIQUE (idempotency_key)` → exactly one row → one send. Even a mis-set switch cannot double-send. This is the strongest guarantee in the design |
| In-app | none (new) | n/a |
| Push (non-call, D-1) | Messaging dispatcher (if Web Push was activated there first) | **Fence:** jobs with `created_at < fence_at` finish on the legacy side; jobs at or after `fence_at` are created only by the new side (Messaging `fanout_jobs` checks the switch). Collapse ID = `eventId` gives provider-side coalescing. APNs `apns-collapse-id`, FCM `collapse_key` and Web Push `Topic` replace rather than duplicate *visible* notifications, but **are not delivery idempotency**. A residual duplicate wake-up within the fence second is possible and accepted (metadata only) |
| Call push | Calling `PushDispatcher` (`NoPushTransport` today) | Only one implementation is wired at a time (configuration). There is no legacy transport, so nothing can duplicate |

---

## 10. Reliability design (Part 11)

### 10.1 Flow classification

| Class | Flows | Requirement | NS involvement |
|---|---|---|---|
| R0 critical, synchronous | sign-in, OTP issue/verify, call ring/answer, message send/receive | must work with NS down | **none** |
| R1 security notices | password changed, new device, step-up code | durable; delivered even if NS is down | Account → ledger directly (unchanged); NS only adds in-app copies |
| R2 time-sensitive | missed call, voicemail, meeting reminder, call push | at-least-once; expire if late | NS (call push fast path is best-effort) |
| R3 informational | org changes, contact requests, developer notices, recordings | at-least-once; delay tolerated | NS |

### 10.2 Failure behaviour

| Failure | Behaviour | Recovery |
|---|---|---|
| NS outage | Producers' outboxes keep events PENDING and retry: Organizations 12 attempts up to 1024 s backoff; Hub 12; Calling 10. **Risk:** a long outage exhausts attempts → DEAD. Mitigation: NS returns 503 + `Retry-After`; the producer DEAD-letter replay operations exist (Organizations `/admin/event-delivery/replay`, Hub `/replay`, Calling CLI). The runbook replays after recovery. Calling push fast path: timeout ≤ 1 s → logged + `calling_push_dispatch_failed_total`; ringing unaffected | replay DEAD; NS dedupe makes replay safe |
| NS database outage | NS `/ready` = 503; ingest 503 (producers retry); user APIs 503 (apps show "notifications unavailable", nothing else breaks) | automatic |
| Redis outage | NS does not use Redis. The Meeting Service needs Redis for OTP (unchanged, existing risk) | n/a |
| Email provider outage | The ledger retries (existing). NS deliveries stay `HANDED_OFF`; NS polls X2 for terminal status at most every 5 min, for ≤ 24 h | ledger |
| Push provider outage | Per-provider circuit breaker (open after 20 consecutive 5xx/timeouts, half-open after 60 s). Deliveries `DEFERRED`; expire at `expiresAt` | automatic |
| Network interruption | Lease expiry → re-claim. Idempotent transports | automatic |
| Worker crash after dispatch, before commit | Email: re-enqueue hits the same key → `created=false`, no duplicate. Push: possible duplicate wake-up; collapse ID makes it visually single | — |
| Duplicate event delivery | `(producer, eventId)` → DUPLICATE | — |
| Delayed or reordered events | `subject.version` monotonic per subject. Older versions are applied only if they do not supersede a newer state (e.g. a `reminder` older than a `cancelled` for the same conference is dropped) | — |
| Scheduled-meeting change | Meeting emits `cancelKey = conference:<id>:start:<epoch>`. A reschedule cancels the old reminder and creates a new one (mirrors `reminder_key`, `notifications.rs:757`) | — |
| Device replacement | Account → Hub event → NS disables the old device's endpoints. The new device registers through the Hub facade | — |
| Account suspension | Change feed / Hub account event → rule 1 drops pending; the inbox is kept but inaccessible (Account refuses the session) | — |
| Org membership revocation | `membership.removed` → cancel pending ORG_* for that recipient | — |

### 10.3 Queue guarantees, idempotency boundaries, backpressure

| Concern | Specification |
|---|---|
| Guarantees | At-least-once from producer to NS. Exactly-once *recording* at NS ingest. At-least-once NS → transport, with idempotent transports (email exact-once at the ledger; push best-effort deduplicated by collapse key) |
| Idempotency boundaries | (1) producer outbox → NS: `(producer, eventId)`. (2) Event → notification: `(recipient_ref, dedupe_key)`. (3) Notification → delivery: `(notification_id, channel, target_ref)`. (4) Delivery → email: ledger `idempotency_key`. (5) Delivery → push: `(collapseId, pushTargetRef)` within the TTL |
| Backpressure | Ingest returns 503 + `Retry-After` when `PENDING` events exceed 10,000 or the oldest is older than 5 min. The dispatcher keeps a bounded concurrency (8 per channel) and caps per-provider rate |
| Leases | Dispatcher lease 120 s, **renewed every 30 s per in-flight item**. A batch is never larger than lease / (provider timeout × 3) |
| Recovery procedures | Runbook: (a) replay producer DEAD rows; (b) I5 replay of NS FAILED deliveries; (c) flip the channel authority back to legacy (Document 5 §7) |

**Exactly-once external delivery is not claimed.** The deterministic email key gives exactly one ledger row per (notice, recipient). Provider delivery after that is Resend/SMTP behaviour.

### 10.4 Policy-call load

The Hub `policy/check` costs about 6 signed calls per recipient (evidence/04 §2.2.5). X3 is batched (≤ 100 recipients per call, ≤ 3 downstream calls per batch). NS caches results for 60 s keyed by Hub preference `version`, and invalidates on Hub `NOTIFICATION_PREFERENCE` change-feed entries.

---

## 11. Workers

| Worker | Responsibility | Cadence | Concurrency / locking |
|---|---|---|---|
| W1 ingest processor | validate → resolve recipients (owner calls) → policy (X3 batch) → notifications, inbox items, deliveries (one transaction per event) | 250 ms + wake on ingest | `SKIP LOCKED` on `notif_event_inbox`, batch 50 |
| W2 delivery dispatcher | channel handlers: EMAIL → X1 (then poll X2); PUSH → `push-transport`; IN_APP → mark DELIVERED on insert | 500 ms | leased, renewed; per-channel concurrency 8; breaker per provider |
| W3 scheduler | release `notif_deferrals`; expire past `expires_at`; apply `notifications.cancel` | 5 s | `SKIP LOCKED` |
| W4 retention | §6 retention + erasure + nonce sweep | hourly | batched 1,000 |
| W5 shadow comparator *(temporary)* | match LEGACY vs NS observations; metrics for I6 | 60 s | single pass |

Every worker is registered in `scripts/service-boundaries/operations_registry.json` (owner, source, entry, tests).

---

## 12. Shared package responsibilities

| Package | Change |
|---|---|
| `packages/service-auth` | add `services::NOTIFICATIONS`; permissions `notifications-event.publish`, `notifications-user.forward`, `notifications-observability.read`, `notifications-operations.replay`, `email-delivery.enqueue`, `email-delivery.read`, and conditionally `notifications-push-endpoint.apply` and `notifications-push.dispatch`; extend `communications-preferences.read.grantable_to` |
| `packages/push-transport` *(new, D-1)* | moved from `services/messaging/src/realtime/push_adapters.rs` unchanged (APNs, FCM, Web Push, SSRF allowlist, metadata whitelist, RFC 8291), plus an `APNS_VOIP` push type for calls (D-2) |
| `packages/webhooks` | reuse `retry` (backoff, Retry-After) and `secret_box` (handle sealing) |
| `packages/contracts` | `notifications/http/v1.openapi.json`, `notifications/internal/v1.openapi.json`, `notifications/events/v1.schema.json`; manifest entries; Hub 1.4.0; Meeting internal email contract |
| `packages/communications-policy` | `VerifiedAccount` / `Dispatch` reused for I2 |
| `packages/migration-guard` | used by the NS migration binary |

---

## 13. Operational requirements

| Area | Requirements |
|---|---|
| Metrics | `notif_events_ingested_total{producer,outcome}`; `notif_deliveries_total{channel,category,status}`; `notif_delivery_latency_seconds{channel}` (event `occurredAt` → handed off); `notif_pending{channel}`; `notif_oldest_pending_age_seconds`; `notif_provider_breaker_open{provider}`; `notif_shadow_mismatch_total{category,kind}`; `notif_inbox_unread_queries_total` |
| Alert rules | `infra/monitoring/prometheus/rules/notifications.rules.yml`: dead letters increasing, oldest pending > 10 min, breaker open > 15 min, shadow mismatch > 0.5 %. Paging stays gated on D-9 |
| Audit | Authority changes are made only by coordinated-release operations and are recorded in the VPS release journal (`/opt/cfmeet-core/coordinated/journal.json`) together with `changed_by_run`. Replays (I5) and admin reads by notification ID emit structured audit log events carrying the operator identity (`x-cf-operator`, as `developer-access-review` does today). No extra table is needed |
| Deployment | Coordinated release `SERVICES` entry, 256 MiB, health `/ready`, private TLS 84xx. Activation operations `activate-notifications-shadow`, `activate-notifications-inbox`, `activate-notifications-channel` (per category) |
