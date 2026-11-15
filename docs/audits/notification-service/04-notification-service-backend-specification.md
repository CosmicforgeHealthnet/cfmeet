# Document 4 — Unified Notification Service: Backend Specification (revision 2, proposal)

**Every API, table and event here is a NEW PROPOSAL unless marked EXISTING.** Nothing is implemented or authorized.

| | |
|---|---|
| Service | `cfmeet-notification-service` (service identity `cfmeet.notifications`) |
| Role | **The single notification authority of CF Meet**: preferences, email, push, in-app, operator alerts and (if D-W) customer webhooks |
| Database | `cfmeet_notifications` in `cfmeet-core-db` (prefix `notif_`) |
| Runtime | 2 replicas, each running API + workers (Postgres `SKIP LOCKED`), on the existing VPS |
| Scope | first-party users, first-party organizations, and platform tenants for Messaging push (through the Messaging `registerPush` facade) |

---

## 1. Binding principles

| # | Principle |
|---|---|
| P1 | **Never in a synchronous business path.** Sign-in, code verification, call establishment, message send and webhook delivery never wait for this service. Producers use transactional outboxes. Calling's push call is post-commit, best-effort and capped at 1 s. |
| P2 | **Authorities are asked, not copied.** Membership, identity, conversation entitlement, call participants and blocks/privacy are read from their owners at send time. Short-TTL caches are keyed by the owner's version. |
| P3 | **Preferences have one writer: this service.** The Hub's existing preference endpoints become facades (Phase 4.2). |
| P4 | **Secrets in transit are sealed.** Codes, invitation tokens and push handles are sealed at rest. Producers seal codes and tokens to the service's HPKE public key (RFC 9180). The service decrypts them only to render a message, and erases the content at the final state, as today's ledger does. Codes and tokens never appear in logs, the inbox, push payloads or admin APIs. |
| P5 | **E2EE content never enters the service.** Message push carries the existing 5-key metadata payload, authorized by Messaging. |
| P6 | **One routing decision per message.** At creation time each notification is routed to exactly one system, old or new, by a fence (§9). |
| P7 | **Lanes isolate criticality.** CRITICAL (codes, security notices), CALL (call push fast path) and STANDARD each have their own claim queries, concurrency budget and alert thresholds. STANDARD backlog can never starve CRITICAL. |
| P8 | **At-least-once internally, deduplicated at every boundary.** Exactly-once *external* delivery is not claimed. |

---

## 2. Capability matrix (Part 7, items 1–27)

| # | Capability | Today | Unified service | Source of code |
|---|---|---|---|---|
| 1 | Event ingestion | five outboxes, no common sink | ingest API + per-producer outbox destination | **new** |
| 2 | Validation and versioning | v1 envelopes | `cfmeet.notifications.event` v1 | **new** |
| 3 | Recipient resolution | Meeting SQL, Messaging fan-out, Calling | owner read APIs (§3.4) | integrate |
| 4 | Account/org authorization | owners | rechecked at dispatch | integrate |
| 5 | Preferences | Hub | **moved here** | port of the Hub tables and logic (`preferences.rs`, `policy.rs:174-208`) |
| 6 | Per-channel policy | absent | channel matrix | **new** |
| 7 | In-app inbox | absent | owned | **new** |
| 8 | Realtime fan-out | per-service WS | polling first; nudge later (D-6) | deferred |
| 9-11 | Web Push / FCM / APNs (+ VoIP) | Messaging adapters, unconfigured | **moved here** | port of `push_adapters.rs` with tests |
| 12 | Transactional email | Meeting ledger | **moved here** | port of `email_delivery.rs` (ledger, worker, Resend, SMTP, Svix) with tests |
| 13 | Durable queues | 4 models | one model, three lanes | **new** |
| 14 | Retry/backoff | 4 policies | one policy per channel (`cf-webhooks::retry` semantics, honours `Retry-After`) | reuse |
| 15 | Dedupe/idempotency | ledger key, push `(event,device)` | kept and unified (§10.3) | port |
| 16 | Scheduling/reminders | Meeting reminder scan | **moved here** (scheduler) | **new** + Meeting logic port (`enqueue_due_reminders`, "still true" checks) |
| 17 | Coalescing | absent | coalesce keys + provider collapse IDs | **new** |
| 18 | Expiry/cancellation | ledger expiry; job checks | `expiresAt`, `cancelKey` | **new** |
| 19 | Delivery and read status | scattered | unified history; inbox read state | **new** |
| 20 | Templates/localization | Meeting templates (English) | all templates here; `locale` from Account, fallback `en` | port + **new** |
| 21 | Time zones | Meeting `format_time` | recipient tz → context tz → UTC | port |
| 22 | Quiet hours | absent | here | **new** |
| 23 | Cross-device sync | Messaging cursors | inbox read state + collapse/clear | **new** |
| 24 | History/retention | scattered | here | **new** |
| 25 | Metrics/auditing | partial; no alerts deployed | here + rules | **new** |
| 26 | Provider failure handling | per-adapter classification | ported + circuit breakers | port + **new** |
| 27 | Security/privacy | strong in places | §8 | port + **new** |

---

## 3. API catalogue

**Conventions for every API:**
- Errors use the CF Meet shape `{error:{code,message,requestId}}`. Codes: `VALIDATION_ERROR` 400, `AUTHENTICATION_REQUIRED` 401, `INTERNAL_AUTHENTICATION_FAILED` 401, `FORBIDDEN` 403, `NOT_FOUND` 404, `VERSION_CONFLICT` 409, `IDEMPOTENCY_CONFLICT` 409, `RATE_LIMITED` 429 (`Retry-After`), `UNAVAILABLE` 503.
- Mutations require an `Idempotency-Key` (kept 24 h).
- Pagination uses an opaque cursor: `limit` 1–100 (default 30) and `nextCursor`.

### 3.1 Public user APIs (9 NEW), via the Meeting Service relay

The relay works like `/api/v2/communications/*` today (`services/meeting-service/src/routes/mod.rs:86-89`):
- the Account session is verified by Meeting;
- the request is forwarded to I2 with a `VerifiedAccount`.

The routes are added to `packages/contracts/openapi/meeting-service.json` so that mobile can call them (`apps/mobile/tests/routes.test.ts`).

| Op | Method & path | operationId | Request → response | Validation / idempotency | Rate limit (per account) |
|---|---|---|---|---|---|
| U1 | GET `/api/v2/notifications` | `listMyNotifications` | `cursor, limit, state=unread\|all, category*` → `{items:[NotificationItem], nextCursor, unreadCount}` | category ∈ catalogue | 120/min |
| U2 | GET `/api/v2/notifications/unread-count` | `getMyNotificationUnreadCount` | → `{unreadCount, latestCreatedAt, version}`; ETag / 304 | — | 240/min |
| U3 | PATCH `/api/v2/notifications/{notificationId}` | `updateMyNotification` | `{read?, dismissed?, expectedVersion}` → item | own items only (else 404); key required | 60/min |
| U4 | POST `/api/v2/notifications/read` | `markMyNotificationsRead` | `{ids≤100 \| upTo}` → `{updated, unreadCount}` | key required | 30/min |
| U5 | POST `/api/v2/notifications/dismiss` | `dismissMyNotifications` | `{ids≤100 \| upTo, category*}` → `{dismissed, unreadCount}` | key required | 30/min |
| U6 | GET `/api/v2/notifications/preferences` | `getMyNotificationPreferences` | → `NotificationPreferences` (§7.1) + `version`, `timeZone` (read-only, from Account) | — | 60/min |
| U7 | PUT `/api/v2/notifications/preferences` | `updateMyNotificationPreferences` | full or partial `NotificationPreferences` + `expectedVersion` | optimistic concurrency; key required; `deny_unknown_fields` | 30/min |
| U8 | PUT `/api/v2/notifications/devices/{accountDeviceId}/push` | `registerMyPushEndpoint` | `{platform: APNS\|APNS_VOIP\|FCM\|WEB_PUSH, handle}` → `{pushTargetRef, platform, enabled}` (**never echoes the handle**) | device must be the caller's ACTIVE Account device (ADR 0060); handle validated per platform, including the SSRF host allowlist (ported `push_adapters.rs:492-534, 761-807`); key required | 10/min |
| U9 | DELETE `/api/v2/notifications/devices/{accountDeviceId}/push?platform=` | `removeMyPushEndpoint` | → 204 | idempotent | 10/min |

`NotificationItem` = `{id, category, title, body, action:{kind, ref}, createdAt, readAt, dismissedAt, groupCount, version}`.
- `action.kind` is a closed enum and `ref` is a UUID. **No URLs are returned.**
- No E2EE text, codes or tokens are ever present.

Conversation and contact notification mute are part of `NotificationPreferences`:
- `conversations: {<conversationId>: {notifications: ALL|MENTIONS|MUTED, mutedUntil}}`, edited with a partial PUT;
- `contacts: {<contactRef>: {notifications}}`.

Media auto-download, chat lock and theme are not notification settings and stay in the Hub.

**Compatibility facades (EXISTING APIs; the contracts are unchanged).** Each one forwards to the service after its cutover phase:

| EXISTING endpoint | Becomes | Phase |
|---|---|---|
| Hub `GET/PUT /api/v2/communications/me/notification-preferences` | facade over U6/U7 (same schema: 7 booleans + `securityEvents` + `payloadPolicy`) | 4.2 |
| Hub `GET/PUT /api/v2/communications/conversations/{id}/preferences` | `notifications`/`mutedUntil` read and written through the service; other fields stay in the Hub | 4.2 |
| Hub `PATCH /api/v2/communications/contacts/{id}` (field `notifications`) | that field is forwarded to the service | 4.2 |
| Hub `GET /api/v2/communications/me/notification-delivery-status` | answered from service delivery data | 4.1 |
| Hub `PUT/DELETE /api/v2/communications/devices/{id}/push` | facade over U8/U9 | 4.1 |
| Messaging `PUT/DELETE /v1/messaging/devices/{deviceId}/push-registration`, `GET /v1/messaging/push/configuration` (customer contract) | facade over the service with the tenant scope; the configuration returns the service's VAPID key | 4.1 |
| Calling `POST /v1/calling/endpoints` `pushTargetRef` | client sets it to U8's `pushTargetRef` | 4.1 |

### 3.2 Provider callback (1 NEW public route)

| Method & path | Auth | Behaviour |
|---|---|---|
| POST `/api/v1/notifications/provider-events/resend` | Svix signature (port of `verify_svix`, `email_delivery.rs:941-983`); 404 until a secret is configured; 256 KiB cap | the same processing as today (`apply_provider_event`); dedupe by `(provider, event_id)`; monotonic status. **Unknown message IDs are ignored.** During the drain, Resend is configured with both the old Meeting endpoint and this one, and each applies only its own messages |

### 3.3 Internal APIs on the service (9 NEW + 1 optional)

All use `cf-service-auth`: an Ed25519 single-audience token, a receiver trust bundle, durable `jti` nonces, and request binding emitted from day one.

| Op | Method & path | operationId | Permission (grantable_to) | Contract |
|---|---|---|---|---|
| I1 | POST `/internal/v1/notification-events` | `ingestNotificationEvents` | `notifications-event.publish` (meeting-service, organizations, communications, calling, messaging, developer-platform) | `{events:[≤100]}` → per-event `ACCEPTED\|DUPLICATE\|REJECTED`. `producer == issuer`; per-producer type allowlist; dedupe `(producer,eventId)` + digest; 503 + `Retry-After` under backpressure. CRITICAL events are admitted even under STANDARD backpressure |
| I2 | POST `/internal/v1/notifications/dispatch` | `dispatchNotificationsUser` | `notifications-user.forward` (meeting-service) | relay envelope (`packages/communications-policy` `Dispatch`); path allowlist U1–U9 |
| I3 | POST `/internal/v1/push/call-invitations` | `dispatchCallPush` | `notifications-call-push.dispatch` (calling) | `{pushTargetRefs≤20, payload: cfmeet.calling.push-invitation v1, expiresAt}` → per-target outcome. **Synchronous, ≤ 1 s, no queue, no retry** (the WS ring and recovery API cover failure); APNs VoIP / FCM high priority |
| I4 | GET `/internal/v1/admin/deliveries` | `listNotificationDeliveries` | `notifications-observability.read` (operator-console) | PII-free search by notification/delivery/event id |
| I5 | GET `/internal/v1/admin/dead-letters` | `listNotificationDeadLetters` | same | FAILED deliveries, REJECTED events |
| I6 | POST `/internal/v1/admin/dead-letters/replay` | `replayNotificationDeadLetters` | `notifications-operations.replay` (operator-console, sensitive) | refuses expired or superseded items and **refuses CRITICAL items older than their code expiry** |
| I7 | GET `/internal/v1/admin/shadow-report` | `getNotificationShadowReport` | `notifications-observability.read` | counts per category and channel |
| I8 | GET `/internal/v1/admin/routing` | `getNotificationRouting` | same | current fence and authority per (category, channel); changed only by coordinated-release operations |
| I9 | GET/DELETE `/internal/v1/admin/email-suppressions` | `listEmailSuppressions` / `removeEmailSuppression` | `notifications-operations.replay` | keyed digests only (no addresses) |
| I10 *(optional, Phase 4.7)* | POST `/internal/v1/operator-alerts` | `ingestOperatorAlert` | static bearer file held by Alertmanager (Alertmanager cannot sign service tokens), on a private listener reachable only from the monitoring network | Alertmanager webhook payload → operator group email |

**Not exposed as endpoints, by design:**
- Template resolution (internal).
- Scheduling and cancellation: driven by producer events (`conference.*`, `cancelKey`, `notifications.cancel`).
- Health: `/health`, `/ready` (DB, migrations, worker heartbeats per lane, breaker states).

### 3.4 New internal read APIs on existing services (5 NEW)

| Op | Owner | Method & path | operationId | Permission | Returns |
|---|---|---|---|---|---|
| X1 | Account (Meeting) | POST `/internal/v1/accounts/notification-contacts/resolve` | `resolveAccountContacts` | `account-contacts.read` (notifications) | per `cfmeet-user:<uuid>` (≤ 100): `{status, email, displayName, locale, timeZone, devices:[{accountDeviceRef, state}]}`. Addresses go only into email ledger rows |
| X2 | Meeting | POST `/internal/v1/conferences/{id}/notification-recipients/resolve` | `resolveConferenceRecipients` | `meeting-notification-recipients.read` | the port of `RECIPIENTS_SQL` (`notifications.rs:892-914`): recipient refs (accounts + email principals with address), `presentedTitle` (protected-title rule applied by Meeting), start time, space time zone, "still true" state |
| X3 | Messaging | POST `/internal/v1/notifications/wakeups/authorize` | `authorizeWakeups` | `messaging-wakeups.authorize` | for `(conversationId, eventSequence, senderPrincipal)`: the entitled principals (ACTIVE member, history window, not the sender, DIRECT block check, conversation ACTIVE, `mentionsRecipient` flag for MANAGED). The port of `push.rs:262-271, 319-339, 417-420` logic, exposed instead of executed locally |
| X4 | Hub | POST `/internal/v1/communications/restrictions/check` | `checkCommunicationRestrictions` | `communications-restrictions.read` | batch `{actorRef, recipientRefs≤100, action}` → blocked / privacy-denied / operator hold per recipient. Replaces the per-recipient ~6-call `policy/check` for notifications |
| X5 | Organizations | POST `/internal/v1/organization-invitations/{id}/invitee/resolve` | `resolveInvitationInvitee` | `organization-invitations.read` | `{status, inviteeAccountRef?}` (never the email). Owner decision D-5 |

---

## 4. Recipient resolution by category

| Category | How recipients are obtained | Recheck at dispatch |
|---|---|---|
| Account codes and security notices | event `accountRef`; address from X1 | account status (a security notice still goes to a deactivated account's address if Account requests it) |
| Meeting reminder, change, cancellation | X2 at fire time | X2 "still true" |
| Meeting invitation email | event carries the invitee ref (account or email principal) and the sealed token | — |
| Organization invitation email | event carries the invitee address **sealed** and the token sealed (Organizations stores only the hash; the Meeting relay seals both before posting) | invitation still PENDING (X5) |
| Organization changes | event `userId` | — |
| Contacts | event target profile → account ref | X4 |
| Message wake-up / message request | X3 | X3 again |
| Missed call / voicemail / recording | refs in the Calling fact | — |
| Developer notices | refs in the Developer Platform event | — |
| Operator alerts | static operator group (config) | — |

---

## 5. Event contracts

### 5.1 Envelope `cfmeet.notifications.event` v1

This is the same envelope as in revision 1, with two additions:
- `sealed`: an HPKE ciphertext object `{kid, enc, ct}` for codes, tokens and invitee addresses;
- `priority`: `CRITICAL|HIGH|NORMAL`.

Rules:
- `data` keys matching `secret|private|token|ciphertext|plaintext|content|body|password|email|phone|key|code` are refused **outside `sealed`**.
- Maximum size 4 KiB (8 KiB with `sealed`).
- `producer` must equal the issuer.
- Each producer has an `eventType` allowlist.
- Each type has its own `eventVersion`. The current and previous versions are accepted.

### 5.2 Inbound catalogue (31 types)

| # | eventType | Producer | Lane | Source |
|---|---|---|---|---|
| 1-5 | `organizations.invitation.{created,accepted,declined,revoked,expired}` | Organizations | STANDARD | existing events → new subscriber |
| 6-10 | `organizations.membership.{role_changed,suspended,removed}`, `organizations.ownership.transferred`, `organizations.organization.suspended` | Organizations | STANDARD | existing → subscriber |
| 11-12 | `communications.connection.{requested,connected}` | Hub | STANDARD | existing → destination `NOTIFICATIONS` |
| 13 | `messaging.message.created` | Messaging | HIGH | existing outbox event → publication to the service (ids only) |
| 14 | `messaging.member.invited` | Messaging | STANDARD | existing → publication |
| 15 | `calling.call.participant-missed` | Calling | HIGH | **new derived fact** (D-10) |
| 16-17 | `calling.voicemail.ready`, `calling.recording.ready` | Calling | STANDARD | existing → destination |
| 18-20 | `meeting-service.conference.{scheduled,rescheduled,cancelled}` | Meeting | STANDARD | written with the existing `record_*` calls in the same transaction (`outbox_events` channel `NOTIFICATION`) |
| 21 | `meeting-service.invitation.email-requested` | Meeting | STANDARD | `v2_create_invitation` with `sendEmail`; token sealed |
| 22 | `meeting-service.organization-invitation.email-requested` | Meeting relay | STANDARD | `v2_create_organization_invitation` with `sendEmail`; token + address sealed |
| 23-25 | `account.{verification-code,password-reset-code,device-step-up-code}.requested` | Account | **CRITICAL** | `account_notification_outbox`, same transaction as code storage |
| 26-27 | `account.security.{password-changed,device-linked}` | Account | **CRITICAL** | same outbox, same transaction (fixes S-9) |
| 28-29 | `developer-platform.credential.{expiring-soon,expired}` | Developer Platform | STANDARD | lifecycle sweep → publication |
| 30 | `developer-platform.production-access.decided` | Developer Platform | STANDARD | replaces the ops-script email |
| 31 | `operator.alert.fired` | Alertmanager (I10) | HIGH | optional |

Control event: `notifications.cancel` (`cancelKey`).

### 5.3 Outbound (1)

`notifications.inbox.changed {accountRef, unreadCount, version}`: internal, for a future realtime nudge.

---

## 6. Data design (16 tables)

The column details for 1–9 are as in revision 1. The DDL below highlights what is new. All tables follow `migration-conventions.md` and the server refuses to start with pending migrations.

| # | Table | Purpose | Key constraints | Retention |
|---|---|---|---|---|
| 1 | `notif_event_inbox` | ingest dedupe | PK `(producer, event_id)`; `request_digest`; state PENDING/PROCESSED/REJECTED/SHADOWED; lane | 30 d |
| 2 | `notif_notifications` | logical notification per recipient | `UNIQUE (recipient_ref, dedupe_key)`; `cancel_key`; `policy_snapshot`; lane | with children |
| 3 | `notif_deliveries` | one per (notification, channel, target) | `UNIQUE (notification_id, channel, target_ref)`; lease (renewed); `routing_version` | 30 d after completion |
| 4 | `notif_delivery_attempts` | append-only attempts | PK `(delivery_id, attempt)` | 30 d |
| 5 | `notif_inbox_items` | user-visible history | indexes for list and unread; group key; `silent` | 90 d (D-8) |
| 6 | `notif_schedules` | reminders, quiet-hour deferrals, coalesce windows | `UNIQUE schedule_key`; `release_at` index; `cancel_key` | on fire/cancel + 7 d |
| 7 | `notif_routing` | fence + authority per (category, channel) | PK `(category, channel)`; `mode OFF\|SHADOW\|ACTIVE`; `fence_at`; `version`; `changed_by_run` | current |
| 8 | `notif_service_nonces` | `jti` replay guard | PK `jti` | expiry |
| 9 | `notif_shadow_observations` | temporary comparison | unique per (category, key, side, digest) | 30 d; dropped in Phase 8 |
| 10 | `notif_email_messages` | **the email ledger** (port of `email_deliveries`) | see DDL | recipient erased +30 d after final; row +60 d; **non-final rows expire** (fixes evidence/01 §9.4) |
| 11 | `notif_email_provider_events` | port of `email_delivery_events` | PK `(provider, event_id)` | 60 d |
| 12 | `notif_email_suppressions` | bounce/complaint suppression (fixes S-12) | PK `recipient_digest` | until removed |
| 13 | `notif_push_endpoints` | account-device push registry (sealed handles) | `UNIQUE (scope, account_device_ref, platform)`; `UNIQUE handle_digest` | disabled + 30 d then deleted |
| 14 | `notif_preferences` | account-level categories, channels, quiet hours | PK `account_ref`; `version` | life of account |
| 15 | `notif_conversation_preferences` | conversation notification mute | PK `(account_ref, conversation_id)`; `muted_until` only with MUTED | life |
| 16 | `notif_contact_preferences` | contact notification mute | PK `(account_ref, contact_ref)` | life |

```sql
CREATE TABLE notif_email_messages (            -- port of email_deliveries
  id               UUID PRIMARY KEY,           -- also the Resend Idempotency-Key
  delivery_id      UUID UNIQUE REFERENCES notif_deliveries(id) ON DELETE SET NULL,
  category         VARCHAR(48)  NOT NULL CHECK (category ~ '^[A-Z][A-Z0-9_]{1,47}$'),
  lane             VARCHAR(10)  NOT NULL CHECK (lane IN ('CRITICAL','STANDARD')),
  idempotency_key  VARCHAR(160) NOT NULL UNIQUE,  -- same key formats as today (otp:verify:…, meeting-notification:…, devplat-access:…)
  recipient_digest CHAR(64)     NOT NULL,      -- keyed HMAC (was unsalted SHA-256)
  recipient_sealed BYTEA,                      -- AES-256-GCM (cf-webhooks secret_box); erased after retention
  subject_sealed   BYTEA        NOT NULL,      -- sealed (was plaintext, kept 60 d)
  body_sealed      BYTEA,                      -- erased at final state (codes/tokens live only here)
  status           VARCHAR(16)  NOT NULL DEFAULT 'QUEUED' CHECK (status IN
                   ('QUEUED','SENDING','ACCEPTED','DELIVERED','DELAYED','BOUNCED','COMPLAINED','FAILED','EXPIRED','SUPPRESSED')),
  provider         VARCHAR(16), provider_message_id VARCHAR(128),
  attempts         INT NOT NULL DEFAULT 0, next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  lease_holder     VARCHAR(96), lease_expires_at TIMESTAMPTZ,
  last_error_class VARCHAR(64), last_error_status INT,
  expires_at       TIMESTAMPTZ  NOT NULL,      -- mandatory (codes: their TTL)
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), accepted_at TIMESTAMPTZ, delivered_at TIMESTAMPTZ, final_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX uq_notif_email_provider_msg ON notif_email_messages (provider, provider_message_id) WHERE provider_message_id IS NOT NULL;
CREATE INDEX ix_notif_email_due_critical ON notif_email_messages (next_attempt_at) WHERE status IN ('QUEUED','SENDING') AND lane = 'CRITICAL';
CREATE INDEX ix_notif_email_due_standard ON notif_email_messages (next_attempt_at) WHERE status IN ('QUEUED','SENDING') AND lane = 'STANDARD';

CREATE TABLE notif_push_endpoints (
  id UUID PRIMARY KEY,                          -- = pushTargetRef
  tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,  -- first-party or platform tenant
  account_ref VARCHAR(64), principal_id UUID,   -- first-party account or platform principal
  account_device_ref UUID NOT NULL,             -- ADR 0060 device, or Messaging device for platform tenants
  platform VARCHAR(12) NOT NULL CHECK (platform IN ('APNS','APNS_VOIP','FCM','WEB_PUSH','LOCAL_TEST')),
  handle_sealed BYTEA NOT NULL, handle_key_id VARCHAR(32) NOT NULL, handle_digest CHAR(64) NOT NULL UNIQUE,
  enabled BOOLEAN NOT NULL DEFAULT true, disabled_reason VARCHAR(48),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (environment_id, account_device_ref, platform)
);

CREATE TABLE notif_preferences (               -- port of communication_notification_preferences + extensions
  account_ref VARCHAR(64) PRIMARY KEY, version BIGINT NOT NULL DEFAULT 1,
  direct_messages BOOLEAN NOT NULL DEFAULT true, group_messages BOOLEAN NOT NULL DEFAULT true,
  incoming_calls BOOLEAN NOT NULL DEFAULT true,  missed_calls BOOLEAN NOT NULL DEFAULT true,
  contact_requests BOOLEAN NOT NULL DEFAULT true, group_invitations BOOLEAN NOT NULL DEFAULT true,
  meeting_invitations BOOLEAN NOT NULL DEFAULT true, meeting_reminders BOOLEAN NOT NULL DEFAULT true,
  voicemail BOOLEAN NOT NULL DEFAULT true, organization_updates BOOLEAN NOT NULL DEFAULT true,
  developer_updates BOOLEAN NOT NULL DEFAULT true,
  channel_overrides JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (pg_column_size(channel_overrides) <= 2048),
  quiet_hours JSONB, source_version BIGINT,     -- Hub version at copy time (Phase 4.2)
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

All default values equal today's Hub behaviour: an absent row means everything is enabled.

---

## 7. Preferences and policy (Part 8)

### 7.1 Model

The service is the **single writer** from Phase 4.2. Before that the Hub is the writer and the service reads (shadow). The model holds:
- account categories (11);
- a channel matrix (`channel_overrides[category] = {push, email, inApp}`);
- quiet hours (`{enabled, start, end, days}`, in the Account time zone from X1);
- conversation mute (`ALL|MENTIONS|MUTED` + `mutedUntil` ≤ 366 d, same validation as `chat.rs:91-102`);
- contact mute (`ALL|MUTED|MENTIONS`);
- a security category that is fixed `true` and not stored.

Organization policy slot: D-7.

### 7.2 Precedence

These are the same twelve rules as revision 1, evaluated top to bottom, first terminal rule wins. The only difference: rules 7–9 now read local tables instead of calling the Hub.

| # | Rule | Effect |
|---|---|---|
| 1 | Recipient not authorized (account inactive or erased, principal suspended, membership revoked, device revoked) | drop |
| 2 | Operator hold (X4) | drop, except security |
| 3 | **Security-critical** (CRITICAL lane) | always deliver on email and in-app; ignore mute, category, channel, quiet hours; never coalesce or defer |
| 4 | Block (X4) | drop for social categories |
| 5 | Organization policy (D-7) | force or forbid channels |
| 6 | Privacy (X4) | drop |
| 7 | Category off | no push or email; in-app silent |
| 8 | Channel off | remove that channel |
| 9 | Contact / conversation mute | no push; in-app silent. MENTIONS allowed only when X3 sets `mentionsRecipient` (MANAGED). PRIVATE_E2EE behaves as MUTED, as today |
| 10 | Quiet hours | defer push to the window end (unless expired); email not deferred |
| 11 | No enabled push endpoint | no push |
| 12 | Coalesce window (10 min) and per-recipient cap (30 pushes/h) | merge / collapse |

Defaults are unchanged.

---

## 8. Security and privacy (Part 9)

| Threat | Protection |
|---|---|
| Cross-account / cross-tenant leakage | `VerifiedAccount` filtering; 404 for foreign IDs; tenant scope on push endpoints and events; authorization-matrix test suite for U1–U9 and I1–I10 |
| Forged service events | `cf-service-auth`; `producer == issuer`; per-producer type allowlist; durable `jti` |
| Unauthorized recipient lookup | no lookup API; X1–X5 grantable to `cfmeet.notifications` only; admin APIs PII-free |
| **Code/token exposure** (new: the service now carries codes and tokens) | HPKE-sealed from the producer's outbox to render time; bodies sealed at rest and erased at the final state; never logged; I6 refuses to replay expired CRITICAL items; codes still verified only by Account, so a leaked email body cannot be verified anywhere else; HPKE private key 0400, rotatable (`kid`) |
| Device-token theft | handles sealed (AES-256-GCM, `cf-webhooks::secret_box`), never returned, `UNIQUE handle_digest`; disabled on device revoke, session end, suspension and erasure (fixes S-7, S-13) |
| Spam / abuse | ingest limits per producer; per-recipient caps; coalescing; suppression list; user API rate limits |
| Replay | `(producer,eventId)` + digest; `jti`; per-recipient `dedupe_key` |
| Duplicate dispatch | delivery `UNIQUE`; email `UNIQUE idempotency_key` with the **same key formats as today**; leases renewed every 30 s (fixes S-8); push collapse IDs |
| E2EE metadata | the service never receives message content; X3 authorizes every wake-up; 5-key payload; Web Push preferred; APNs/FCM metadata exposure is documented as a residual |
| Stale permissions | dispatch-time recheck (X1 status, X3, endpoint state); Platform Access change feed |
| Deleted users / revoked members | rule 1; `membership.removed` → `cancelKey` |
| Webhook forgery | Resend Svix verification (ported); no other inbound webhooks |
| Provider credential exposure | 0400 files; never logged; release tooling refuses provider variables until activation |
| Dangerous action links | closed `action.kind` enum in inbox and push; email links built from `APP_URL` with fixed paths (as Meeting templates do today) |
| Sensitive logs | IDs, category, lane, status and error class only; CI grep test |
| Retention/deletion | §6; erasure of every row for an account within 24 h |

---

## 9. Routing fence: one system per message

`notif_routing(category, channel)` holds `mode`, `fence_at` and `version`. **The producer reads the same value** (through its release-managed configuration, set by the same coordinated-release operation) and decides **once, at message creation**:

| Producer path | Before the fence | After the fence |
|---|---|---|
| Meeting `EmailService` / `email_delivery::enqueue` callers | old ledger row (`email_deliveries`) | `NOTIFICATION` outbox event → the service. **No old ledger row** |
| Meeting notification worker (reminders, changes) | old worker renders and sends | jobs created after the fence are not created (the service schedules from `conference.*` events); old PENDING jobs finish |
| Account code and security emails | old path (direct enqueue) | `account_notification_outbox` → the service. Break-glass `ACCOUNT_EMAIL_FALLBACK=local` re-routes new messages to the old ledger instantly |
| Messaging push | `messaging_push_jobs` | `message.created` → the service; Messaging creates no new push jobs |
| Calling push | `NoPushTransport` | `CALLING_PUSH_TRANSPORT=notifications` |
| Hub preference writes | Hub tables | the service's tables (facade) |

Because each message exists in exactly one system, there is no cross-ledger duplication. Existing idempotency key formats are kept, so even a mis-ordered retry across the fence collapses: the old ledger and the new one never hold the same key, because the routing decision precedes key creation.

Provider idempotency (the Resend key is the ledger ID; SMTP has none) is **not** relied on for migration safety.

---

## 10. Reliability (Part 11)

### 10.1 Flow classes

| Class | Flows | Requirement | Design |
|---|---|---|---|
| R0 synchronous critical | sign-in, code verification, ringing, message send | must work with the service down | the service is never in these paths |
| R1 critical delivery | verification, reset and step-up codes; security notices | durable; p95 handoff to provider ≤ baseline + 5 s; survives service restarts | Account transactional outbox; CRITICAL lane on both replicas; dedicated claim index; alerting before cutover; break-glass fallback until Phase 8 |
| R2 time-sensitive | call push, message wake-ups, missed calls, reminders | at-least-once, expire when late | CALL fast lane (≤ 1 s, best-effort); HIGH priority |
| R3 informational | org, contacts, developer, recordings | delay tolerated | STANDARD |

### 10.2 Failure behaviour

| Failure | Behaviour |
|---|---|
| Service outage (both replicas) | Producer outboxes hold events: Account outbox (new, **unbounded retries for CRITICAL until the code expires**), Hub, Organizations, Calling, Messaging, Meeting. Users waiting for a code see a delay. Runbook: if the outage exceeds 2 min, the operator sets `ACCOUNT_EMAIL_FALLBACK=local` (one operation). Ringing, sign-in and messaging are unaffected |
| One replica down | the other claims everything (`SKIP LOCKED`) |
| Service database outage | `/ready` 503; same as a service outage |
| Redis outage | not used by the service. Meeting OTP state uses Redis (unchanged, existing risk) |
| Email provider outage | ledger retry (ported); provider breaker; optional SMTP secondary provider for CRITICAL (D-14) |
| Push provider outage | breaker; DEFERRED; expire |
| Worker crash after send, before commit | email: Resend `Idempotency-Key` = message ID within Resend's window (SMTP: possible duplicate, documented); push: duplicate wake-up collapsed |
| Duplicate / delayed / reordered events | dedupe; `subject.version`; `cancelKey` |
| Meeting change | `conference.rescheduled` replaces the schedule (`cancelKey` + new `schedule_key`) |
| Device replacement | Account event disables the old device's endpoints |
| Account suspension / org revocation | rule 1; cancel pending items |

### 10.3 Guarantees and boundaries

| Concern | Specification |
|---|---|
| Idempotency boundaries | outbox → service `(producer,eventId)`; event → notification `(recipient, dedupe_key)`; notification → delivery `(notification, channel, target)`; delivery → email `idempotency_key` (legacy formats kept); email → provider `Idempotency-Key`; push `(collapseId, target)` |
| Backpressure | STANDARD ingest 503 above 10,000 pending or 5 min age; **CRITICAL is never refused**; per-lane concurrency (CRITICAL 4, CALL 8, STANDARD 8) |
| Leases | 120 s, renewed every 30 s |

**Exactly-once external delivery is not claimed.**

---

## 11. Workers

| Worker | Lane | Cadence |
|---|---|---|
| W1 ingest processor | all | 100 ms wake + poll |
| W2 critical dispatcher | CRITICAL | 100 ms |
| W3 standard dispatcher (email, push, in-app) | HIGH / STANDARD | 500 ms |
| W4 scheduler (reminders, deferrals, expiry, cancel) | — | 5 s |
| W5 retention and erasure | — | hourly |
| W6 shadow comparator (temporary) | — | 60 s |

The CALL fast lane is a synchronous handler (I3), not a worker.

---

## 12. Shared packages

| Package | Change |
|---|---|
| `packages/service-auth` | identity `cfmeet.notifications`; new permissions (I1–I9, X1–X5) |
| `packages/push-transport` | optional intermediate step (N4); the adapters end up inside the service |
| `packages/webhooks` | `retry`, `secret_box` |
| new `packages/notification-client` | outbox helpers for producers: envelope builder, HPKE sealing, routing-fence check. Keeps every producer integration identical |
| `packages/contracts` | notifications HTTP, internal, events; Hub 1.4.0 (facade notes); Meeting / Account / Messaging / Hub / Organizations internal additions |

---

## 13. Operations

| Area | Requirements |
|---|---|
| Metrics | per lane and channel: ingested, delivered, failed, pending, oldest age, latency, breaker state, suppression count, shadow mismatches |
| Alert rules | CRITICAL oldest pending > 60 s; CRITICAL failure rate above baseline; dead letters > 0; breaker open > 5 min; replica down |
| Deployment | 2 `SERVICES` entries (or 1 entry with 2 replicas) in the coordinated release, 256 MiB each; activation operations per phase (`activate-notifications-*`) |
| Runbooks | outage → break-glass; replay; provider credential rotation; HPKE key rotation |
