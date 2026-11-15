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

### 3.3 Internal APIs on the service (11 NEW + 1 optional)

All use `cf-service-auth`: an Ed25519 single-audience token, a receiver trust bundle, durable `jti` nonces, and request binding emitted from day one. **Counting unit:** one operation = one HTTP method on one path = one operationId.

| Op | Method & path | operationId | Permission (grantable_to) | Contract |
|---|---|---|---|---|
| I1 | POST `/internal/v1/notification-events` | `ingestNotificationEvents` | `notifications-event.publish` (meeting-service, organizations, communications, calling, messaging, developer-platform) | `{events:[≤100]}` → per-event `ACCEPTED\|DUPLICATE\|REJECTED\|HELD`. `producer == issuer`; per-producer type allowlist; dedupe `(producer,eventId)` + digest. **Each event carries `route {decisionId, routingVersion, protocol}`** (§9). An event whose route is not `NEW` is REJECTED. An event for a category whose service-side mode is not ACTIVE/SHADOW is **HELD** (stored, never sent, alert raised), never dropped. 503 + `Retry-After` under backpressure. CRITICAL events are admitted even under STANDARD backpressure. `ACCEPTED` is returned only after the event is committed: this is the responsibility boundary (§9.5) |
| I2 | POST `/internal/v1/notifications/dispatch` | `dispatchNotificationsUser` | `notifications-user.forward` (meeting-service) | relay envelope (`packages/communications-policy` `Dispatch`); path allowlist U1–U9 |
| I3 | POST `/internal/v1/push/call-invitations` | `dispatchCallPush` | `notifications-call-push.dispatch` (calling) | `{pushTargetRefs≤20, payload: cfmeet.calling.push-invitation v1, expiresAt}` → per-target outcome. **Synchronous, ≤ 1 s, no queue, no retry** (the WS ring and recovery API cover failure); APNs VoIP / FCM high priority |
| I4 | GET `/internal/v1/admin/deliveries` | `listNotificationDeliveries` | `notifications-observability.read` (operator-console) | PII-free search by notification/delivery/event/decision id |
| I5 | GET `/internal/v1/admin/dead-letters` | `listNotificationDeadLetters` | same | FAILED deliveries, REJECTED and HELD events |
| I6 | POST `/internal/v1/admin/dead-letters/replay` | `replayNotificationDeadLetters` | `notifications-operations.replay` (operator-console, sensitive) | refuses expired or superseded items, items whose route decision is no longer `NEW`, and CRITICAL items older than their code expiry |
| I7 | GET `/internal/v1/admin/shadow-report` | `getNotificationShadowReport` | `notifications-observability.read` | counts per category and channel |
| I8 | GET `/internal/v1/admin/routing` | `getNotificationRouting` | same | current mode, fence and version per (category, channel), plus the **route audit**: per category and hour, the count of accepted decision IDs and a SHA-256 over their sorted list (compared by certify with the producer's own digest, §9.10). Changed only by coordinated-release operations |
| I9 | GET `/internal/v1/admin/email-suppressions` | `listEmailSuppressions` | `notifications-operations.replay` | keyed digests only (no addresses) |
| I10 | DELETE `/internal/v1/admin/email-suppressions/{digest}` | `removeEmailSuppression` | same | audited |
| I11 | POST `/internal/v1/admin/reroute/cancel` | `cancelForReroute` | `notifications-operations.reroute` (operator-console, sensitive) | first phase of the two-phase reroute (§9.8): `{decisionIds≤500}` → per ID `CANCELLED` (never dispatched; now terminal `CANCELLED_FOR_REROUTE`) or `IN_FLIGHT_OR_SENT` (must not be rerouted). Idempotent |
| I12 *(optional, Phase 4.7, D-15)* | POST `/internal/v1/operator-alerts` | `ingestOperatorAlert` | static bearer file held by Alertmanager (Alertmanager cannot sign service tokens), on a private listener reachable only from the monitoring network | Alertmanager webhook payload → operator group email |

**Not exposed as endpoints, by design:**
- Template resolution (internal).
- Scheduling and cancellation: driven by producer events (`conference.*`, `cancelKey`, `notifications.cancel`).
- Health: `/health`, `/ready` (DB, migrations, worker heartbeats per lane, breaker states).

### 3.4 New internal APIs on existing services (7 NEW)

| Op | Owner | Method & path | operationId | Permission | Returns |
|---|---|---|---|---|---|
| X1 | Account (Meeting) | POST `/internal/v1/accounts/notification-contacts/resolve` | `resolveAccountContacts` | `account-contacts.read` (notifications) | per `cfmeet-user:<uuid>` (≤ 100): `{status, email, displayName, locale, timeZone, devices:[{accountDeviceRef, state}]}`. Addresses go only into email ledger rows |
| X2 | Meeting | POST `/internal/v1/conferences/{id}/notification-recipients/resolve` | `resolveConferenceRecipients` | `meeting-notification-recipients.read` | the port of `RECIPIENTS_SQL` (`notifications.rs:892-914`): recipient refs (accounts + email principals with address), `presentedTitle` (protected-title rule applied by Meeting), start time, space time zone, "still true" state |
| X3 | Messaging | POST `/internal/v1/notifications/wakeups/authorize` | `authorizeWakeups` | `messaging-wakeups.authorize` | for `(conversationId, eventSequence, senderPrincipal)`: the entitled principals (ACTIVE member, history window, not the sender, DIRECT block check, conversation ACTIVE, `mentionsRecipient` flag for MANAGED). The port of `push.rs:262-271, 319-339, 417-420` logic, exposed instead of executed locally |
| X4 | Hub | POST `/internal/v1/communications/restrictions/check` | `checkCommunicationRestrictions` | `communications-restrictions.read` | batch `{actorRef, recipientRefs≤100, action}` → blocked / privacy-denied / operator hold per recipient. Replaces the per-recipient ~6-call `policy/check` for notifications |
| X5 | Organizations | POST `/internal/v1/organization-invitations/{id}/invitee/resolve` | `resolveInvitationInvitee` | `organization-invitations.read` | `{status, inviteeAccountRef?}` (never the email). Owner decision D-5 |
| X6 | Account (Meeting) | POST `/internal/v1/accounts/notification-route-decisions/leases` | `claimCriticalSendLeases` | `account-route-lease.claim` (notifications) | CRITICAL lane only (§9.8). `{decisionIds≤100, leaseSeconds≤30}` → per ID `GRANTED` (route still `NEW`; `send_lease_until` set from the Meeting DB clock) \| `REVOKED` (rerouted or superseded) \| `EXPIRED` (code expired). The service sends a CRITICAL email only under a granted lease and aborts the provider call if the lease would lapse. This is a fencing token, not a read |
| X7 | Account (Meeting) | POST `/internal/v1/accounts/notification-route-decisions/completions` | `completeCriticalSends` | `account-route-lease.claim` (notifications) | `{decisionIds≤100}` held under a live lease → sets `ns_completed_at` (terminal; never reroutable). Called immediately after the provider accepts |

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

### 5.1 Envelope `cfmeet.notifications.event` v1 (NEW)

This envelope aligns with `cfmeet.platform.event-envelope`. Producers build it with `packages/notification-client`.

```json
{
  "schemaVersion": 1,
  "eventId": "evt_<32hex> | uuid",
  "eventType": "account.verification-code.requested",
  "eventVersion": 1,
  "producer": "cfmeet.meeting-service",
  "occurredAt": "RFC3339",
  "correlationId": "corr_<32hex>",
  "scope": { "tenantId": "uuid", "applicationId": "uuid", "environmentId": "uuid" },
  "subject": { "kind": "ACCOUNT", "id": "uuid", "version": 3 },
  "recipients": [{ "ref": "cfmeet-user:<uuid>" }],
  "delivery": {
    "priority": "CRITICAL|HIGH|NORMAL",
    "dedupeKey": "string<=200",
    "idempotencyKey": "string<=160|null",
    "coalesceKey": "string<=200|null",
    "deliverAfter": "RFC3339|null",
    "expiresAt": "RFC3339|null",
    "cancelKey": "string<=200|null"
  },
  "route": { "decisionId": "uuid", "routingVersion": 7, "protocol": 1 },
  "data": { "codeTtlMinutes": 1440 },
  "sealed": { "kid": "notif-hpke-2026-11", "enc": "base64url", "ct": "base64url" }
}
```

| Field | Rule |
|---|---|
| Size | ≤ 4 KiB, or ≤ 8 KiB when `sealed` is present |
| `data` keys | must match `^[a-z][a-zA-Z0-9]{0,47}$`. Keys matching `secret\|private\|token\|ciphertext\|plaintext\|content\|body\|password\|email\|phone\|key\|code` are **refused outside `sealed`** (mirrors Messaging's CHECK, `m20261106_000004_realtime.rs:139-140`) |
| `sealed` | HPKE (RFC 9180, X25519/HKDF-SHA256/AES-256-GCM) to the service's public key `kid`. Plaintext is a JSON object: `code`, `token`, `address` (invitation invitee only). Allowed **only** for event types flagged `sealedAllowed` in the catalogue (#21-25) |
| Strings | ≤ 200 chars; control characters stripped |
| `recipients` | ≤ 200 refs: `cfmeet-user:<uuid>`, `meeting-email-principal:<uuid>`, `platform-principal:<uuid>`, `operator-group:<name>` |
| `delivery.idempotencyKey` | required for email categories that exist today; must use **today's key formats** (`otp:verify:…`, `otp:reset:…`, `device-step-up:…`, `password-changed:…`, `meeting-notification:…`, `devplat-access:…`) so the new ledger keeps the same dedupe behaviour |
| `producer` | must equal the authenticated service identity |
| `eventType` | must be in that producer's closed allowlist (as Developer Platform `catalog.rs:29-52` does) |
| CRITICAL priority | allowed only for `account.*` types from `cfmeet.meeting-service` (the Account authority) |
| `route` | required: the producer's decision (§9.2). `route.decisionId` is unique per producer (`uq_notif_event_inbox_decision`); I1 rejects any event whose decision is not `NEW` and supports `dryRun:true` probes for the reroute check (§9.5) |

**Versioning.** `schemaVersion` versions the envelope; `eventVersion` versions each type. The service accepts the current and previous `eventVersion` per type. A breaking change is a new contract entry in `packages/contracts/manifest.json`.

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

## 6. Data design (17 tables in the service + 5 producer-side tables)

All tables follow `docs/architecture/migration-conventions.md` (raw SQL `UP`/`DOWN`, history table `notifications_seaql_migrations`, prefix `notif_`). The server refuses to start with pending migrations. The full DDL for all 17 service tables follows the summary. The 5 producer-side tables used by the routing fence are specified in §9.2.

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
| 17 | `notif_preference_change_log` | 7-day log of preference writes made through the service, for rollback write-back to the Hub (Document 5 §6, 4.2) | PK `(account_ref, scope, changed_at)` | 7 d |

```sql
-- m20270101_000001_notifications_foundation (illustrative date)

CREATE TABLE notif_event_inbox (               -- 1. ingest dedupe
  producer        VARCHAR(64)  NOT NULL CHECK (producer ~ '^cfmeet\.[a-z][a-z0-9-]*$|^alertmanager$'),
  event_id        VARCHAR(64)  NOT NULL,
  request_digest  CHAR(64)     NOT NULL,
  event_type      VARCHAR(96)  NOT NULL,
  event_version   SMALLINT     NOT NULL,
  lane            VARCHAR(10)  NOT NULL CHECK (lane IN ('CRITICAL','HIGH','STANDARD')),
  state           VARCHAR(16)  NOT NULL CHECK (state IN ('PENDING','PROCESSED','REJECTED','SHADOWED')),
  reject_reason   VARCHAR(64),
  envelope        JSONB        NOT NULL CHECK (pg_column_size(envelope) <= 8192),  -- sealed part stays sealed
  received_at     TIMESTAMPTZ  NOT NULL DEFAULT now(), processed_at TIMESTAMPTZ,
  route_decision_id UUID NOT NULL,            -- producer decision (§9.2); route must be NEW
  routing_version INT NOT NULL, routing_protocol SMALLINT NOT NULL,
  lease_id UUID, lease_until TIMESTAMPTZ, attempts INT NOT NULL DEFAULT 0,
  PRIMARY KEY (producer, event_id)
);
CREATE UNIQUE INDEX uq_notif_event_inbox_decision ON notif_event_inbox (producer, route_decision_id);
CREATE INDEX ix_notif_event_inbox_due ON notif_event_inbox (lane, received_at) WHERE state = 'PENDING';

CREATE TABLE notif_notifications (             -- 2. one logical notification per recipient
  id              UUID PRIMARY KEY,
  recipient_ref   VARCHAR(80)  NOT NULL CHECK (recipient_ref ~ '^(cfmeet-user|meeting-email-principal|platform-principal):[0-9a-f-]{36}$|^operator-group:[a-z-]{1,40}$'),
  category        VARCHAR(40)  NOT NULL,
  lane            VARCHAR(10)  NOT NULL,
  dedupe_key      VARCHAR(200) NOT NULL,
  coalesce_key    VARCHAR(200), cancel_key VARCHAR(200),
  source_producer VARCHAR(64)  NOT NULL, source_event_id VARCHAR(64) NOT NULL,
  subject_kind    VARCHAR(32), subject_id UUID,
  template_key    VARCHAR(64)  NOT NULL,
  params          JSONB        NOT NULL CHECK (pg_column_size(params) <= 2048),  -- never codes/tokens
  policy_snapshot JSONB        NOT NULL,       -- decision, reasons, preference version (audit)
  state           VARCHAR(24)  NOT NULL CHECK (state IN ('ROUTED','SUPPRESSED','CANCELLED','CANCELLED_FOR_REROUTE','EXPIRED','SHADOW','HELD')),
  deliver_after   TIMESTAMPTZ, expires_at TIMESTAMPTZ,
  created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
  UNIQUE (recipient_ref, dedupe_key)
);
CREATE INDEX ix_notif_notifications_cancel ON notif_notifications (cancel_key) WHERE cancel_key IS NOT NULL;

CREATE TABLE notif_deliveries (                -- 3. one per (notification, channel, target)
  id              UUID PRIMARY KEY,
  notification_id UUID NOT NULL REFERENCES notif_notifications(id) ON DELETE CASCADE,
  channel         VARCHAR(16) NOT NULL CHECK (channel IN ('EMAIL','PUSH','IN_APP','OPERATOR','WEBHOOK')),
  lane            VARCHAR(10) NOT NULL,
  target_ref      VARCHAR(80) NOT NULL,        -- 'address' | push endpoint id | 'inbox'
  status          VARCHAR(20) NOT NULL CHECK (status IN
                  ('PENDING','DEFERRED','DISPATCHING','SENT','DELIVERED','FAILED','EXPIRED','CANCELLED','SUPPRESSED','SHADOW')),
  attempts        INT NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 64),
  next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  lease_id UUID, lease_until TIMESTAMPTZ,
  CHECK ((status = 'DISPATCHING') = (lease_id IS NOT NULL)),
  last_error_class VARCHAR(64),
  routing_version INT NOT NULL,                -- notif_routing.version at creation (fence evidence)
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(), completed_at TIMESTAMPTZ,
  UNIQUE (notification_id, channel, target_ref)
);
CREATE INDEX ix_notif_deliveries_due ON notif_deliveries (lane, next_attempt_at) WHERE status IN ('PENDING','DEFERRED');

CREATE TABLE notif_delivery_attempts (         -- 4. append-only
  delivery_id UUID NOT NULL REFERENCES notif_deliveries(id) ON DELETE CASCADE,
  attempt     INT  NOT NULL,
  started_at  TIMESTAMPTZ NOT NULL, finished_at TIMESTAMPTZ,
  outcome     VARCHAR(16) NOT NULL CHECK (outcome IN ('OK','RETRY','PERMANENT','BREAKER_OPEN')),
  error_class VARCHAR(64), http_status INT,
  PRIMARY KEY (delivery_id, attempt)
);

CREATE TABLE notif_inbox_items (               -- 5. user-visible history
  id              UUID PRIMARY KEY,            -- = notification id
  recipient_ref   VARCHAR(80) NOT NULL,
  category        VARCHAR(40) NOT NULL,
  template_key    VARCHAR(64) NOT NULL,
  params          JSONB NOT NULL CHECK (pg_column_size(params) <= 2048),
  action_kind     VARCHAR(32) NOT NULL, action_ref UUID,
  group_key       VARCHAR(200), group_count INT NOT NULL DEFAULT 1,
  silent          BOOLEAN NOT NULL DEFAULT false,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  read_at TIMESTAMPTZ, dismissed_at TIMESTAMPTZ, expires_at TIMESTAMPTZ NOT NULL,
  version         BIGINT NOT NULL DEFAULT 1
);
CREATE INDEX ix_notif_inbox_recipient ON notif_inbox_items (recipient_ref, created_at DESC, id DESC) WHERE dismissed_at IS NULL;
CREATE INDEX ix_notif_inbox_unread    ON notif_inbox_items (recipient_ref) WHERE read_at IS NULL AND dismissed_at IS NULL AND silent = false;
CREATE UNIQUE INDEX uq_notif_inbox_group ON notif_inbox_items (recipient_ref, group_key) WHERE group_key IS NOT NULL AND read_at IS NULL AND dismissed_at IS NULL;

CREATE TABLE notif_schedules (                 -- 6. reminders, quiet-hour deferrals, coalesce windows
  schedule_key VARCHAR(200) PRIMARY KEY,       -- e.g. reminder:<conference>:<start_epoch> (today's reminder_key)
  kind         VARCHAR(16) NOT NULL CHECK (kind IN ('REMINDER','QUIET_HOURS','DELIVER_AFTER','COALESCE')),
  release_at   TIMESTAMPTZ NOT NULL,
  cancel_key   VARCHAR(200),
  payload      JSONB NOT NULL CHECK (pg_column_size(payload) <= 2048),  -- ids only
  delivery_id  UUID REFERENCES notif_deliveries(id) ON DELETE CASCADE,
  state        VARCHAR(12) NOT NULL CHECK (state IN ('PENDING','FIRED','CANCELLED')),
  lease_id UUID, lease_until TIMESTAMPTZ,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ix_notif_schedules_due ON notif_schedules (release_at) WHERE state = 'PENDING';
CREATE INDEX ix_notif_schedules_cancel ON notif_schedules (cancel_key) WHERE cancel_key IS NOT NULL;

CREATE TABLE notif_routing (                   -- 7. routing fence per (category, channel)
  category VARCHAR(40) NOT NULL, channel VARCHAR(16) NOT NULL,
  mode     VARCHAR(8)  NOT NULL CHECK (mode IN ('OFF','SHADOW','ACTIVE')),
  fence_at TIMESTAMPTZ NOT NULL, version INT NOT NULL,
  canary_percent SMALLINT NOT NULL DEFAULT 0 CHECK (canary_percent BETWEEN 0 AND 100),
  changed_by_run VARCHAR(64) NOT NULL, changed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (category, channel)
);

CREATE TABLE notif_service_nonces (            -- 8. jti replay guard
  jti VARCHAR(64) PRIMARY KEY, issuer VARCHAR(64) NOT NULL, expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE notif_shadow_observations (       -- 9. temporary; dropped in Phase 8
  id BIGSERIAL PRIMARY KEY,
  category VARCHAR(40) NOT NULL, dedupe_key VARCHAR(200) NOT NULL,
  side VARCHAR(8) NOT NULL CHECK (side IN ('LEGACY','NS')),
  recipient_digest CHAR(64) NOT NULL,          -- keyed HMAC of the recipient ref
  decision VARCHAR(16) NOT NULL, render_digest CHAR(64),   -- subject+body hash for template parity
  observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (category, dedupe_key, side, recipient_digest)
);
```

```sql
CREATE TABLE notif_email_messages (            -- port of email_deliveries
  id               UUID PRIMARY KEY,           -- UUIDv5(service namespace, idempotency_key): deterministic, so a re-created row reuses the same Resend Idempotency-Key after a crash
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

CREATE TABLE notif_email_provider_events (     -- 11. port of email_delivery_events
  provider VARCHAR(16) NOT NULL, event_id VARCHAR(128) NOT NULL,
  event_type VARCHAR(48) NOT NULL, provider_message_id VARCHAR(128),
  message_id UUID,                             -- re-linked by a sweep if the event arrived before ACCEPTED
  occurred_at TIMESTAMPTZ, received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (provider, event_id)
);

CREATE TABLE notif_email_suppressions (        -- 12. bounce / complaint suppression
  recipient_digest CHAR(64) PRIMARY KEY,
  reason VARCHAR(16) NOT NULL CHECK (reason IN ('COMPLAINED','HARD_BOUNCE','MANUAL')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);  -- CRITICAL-lane messages are NOT suppressed (account recovery must still work); an operator alert is raised instead

CREATE TABLE notif_conversation_preferences (  -- 15. port of the Hub conversation notification fields
  account_ref VARCHAR(64) NOT NULL, conversation_id UUID NOT NULL,
  notifications VARCHAR(8) NOT NULL DEFAULT 'ALL' CHECK (notifications IN ('ALL','MENTIONS','MUTED')),
  muted_until TIMESTAMPTZ CHECK (muted_until IS NULL OR notifications = 'MUTED'),
  version BIGINT NOT NULL DEFAULT 1, source_version BIGINT, updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (account_ref, conversation_id)
);

CREATE TABLE notif_contact_preferences (       -- 16. port of communication_contact_aliases.notifications
  account_ref VARCHAR(64) NOT NULL, contact_ref VARCHAR(64) NOT NULL,
  notifications VARCHAR(8) NOT NULL DEFAULT 'ALL' CHECK (notifications IN ('ALL','MENTIONS','MUTED')),
  version BIGINT NOT NULL DEFAULT 1, source_version BIGINT, updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (account_ref, contact_ref)
);

CREATE TABLE notif_preference_change_log (     -- 17. rollback write-back source (7 days)
  account_ref VARCHAR(64) NOT NULL,
  scope VARCHAR(16) NOT NULL CHECK (scope IN ('ACCOUNT','CONVERSATION','CONTACT')),
  scope_ref VARCHAR(64),                       -- conversation id or contact ref
  changed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  new_value JSONB NOT NULL CHECK (pg_column_size(new_value) <= 2048),
  version BIGINT NOT NULL,
  PRIMARY KEY (account_ref, scope, changed_at)
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

Rules are evaluated top to bottom, and the first terminal rule wins. Rules 7–9 read the service's own preference tables. Rules 2, 4 and 6 ask the Hub through X4.

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
| Cross-account / cross-tenant leakage | `VerifiedAccount` filtering; 404 for foreign IDs; tenant scope on push endpoints and events; authorization-matrix test suite for U1–U9, I1–I12 and X1–X7 |
| Forged service events | `cf-service-auth`; `producer == issuer`; per-producer type allowlist; durable `jti` |
| Unauthorized recipient lookup | no lookup API; X1–X7 grantable to `cfmeet.notifications` only; admin APIs PII-free |
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

## 9. Routing fence protocol v1: one system per message

The old and new ledgers live in **different databases**:
- the old email ledger, `meeting_notifications` and `account_notification_outbox` are in the Meeting database (`rtc_db`, legacy `cosmicforge-db` container);
- `messaging_push_jobs` is in `cfmeet_messaging`;
- the new ledger is in `cfmeet_notifications`.

No transaction spans them. The fence therefore relies on one rule: **every routing decision is made, recorded and enforced inside the producer's own database transaction**, the same transaction that writes either the old-path row or the outbox row, and the service only ever acts on decisions that say `NEW`.

### 9.1 Invariants

| # | Invariant | Enforced by |
|---|---|---|
| F1 | Every routable message has exactly one decision row, keyed by its deterministic idempotency key | `notification_route_decisions` primary key, in the producer database |
| F2 | A decision is created in the same transaction as the old-path row (route `OLD`) **or** the outbox row (route `NEW`), never both | producer code + legacy-table guard triggers (§9.4) |
| F3 | A decision never changes silently. It changes only through an audited reroute that increments `decision_version` and records `reason` (§9.8) | trigger refusing any other update |
| F4 | The service sends nothing whose decision is not `NEW` | I1 rejects non-`NEW` routes; CRITICAL sends require an X6 lease and are reported complete through X7 |
| F5 | Responsibility moves to the service only when I1 returns `ACCEPTED` (committed). Before that, the producer may reroute unilaterally | §9.5 |
| F6 | No component running a pre-fence binary can write to an old-path table for a fenced category | guard triggers (§9.4) + release-tooling rollback floor (§9.6) |

**Definition used in this blueprint:** a *silent duplicate* is the same logical notification delivered by both systems without a recorded decision, lease or operator action that explains it. The protocol makes silent duplicates impossible by construction, and detects any violation within one hour (§9.10). Residual duplicates that remain possible are listed in §9.11; each one is recorded and counted.

### 9.2 Producer-side tables (5 new tables, additive)

These are created in the Meeting database (3) and the Messaging database (2). They are proposals only; no migration is authorized by this audit.

```sql
-- Meeting database and Messaging database (one copy each)
CREATE TABLE notification_routing_state (       -- runtime-readable routing switch (no container restart)
  category        VARCHAR(40) PRIMARY KEY,
  mode            VARCHAR(12) NOT NULL CHECK (mode IN ('LEGACY_ONLY','SHADOW','FENCED_OLD','FENCED_NEW')),
  canary_percent  SMALLINT NOT NULL DEFAULT 0 CHECK (canary_percent BETWEEN 0 AND 100),
  critical_fallback VARCHAR(8) NOT NULL DEFAULT 'AUTO' CHECK (critical_fallback IN ('AUTO','FORCED','OFF')),
  routing_version INT NOT NULL,
  protocol        SMALLINT NOT NULL DEFAULT 1,
  changed_by_run  VARCHAR(64) NOT NULL,
  changed_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notification_route_decisions (     -- one row per routable message (F1)
  idempotency_key  VARCHAR(160) PRIMARY KEY,    -- today's key formats (otp:verify:…, meeting-notification:…, event id for push)
  decision_id      UUID NOT NULL UNIQUE,
  category         VARCHAR(40) NOT NULL,
  route            VARCHAR(4)  NOT NULL CHECK (route IN ('OLD','NEW')),
  decision_version INT NOT NULL DEFAULT 1,
  routing_version  INT NOT NULL,                -- notification_routing_state.routing_version used
  reason           VARCHAR(32) NOT NULL CHECK (reason IN ('ROUTED','REROUTED_UNACKED','REROUTED_CANCELLED','REROUTED_CRITICAL_LEASE','AUTO_FAILOVER','REROUTED_POSSIBLE_DUPLICATE')),
  ns_acked_at      TIMESTAMPTZ,                 -- set when I1 returned ACCEPTED (F5)
  send_lease_until TIMESTAMPTZ,                 -- CRITICAL only (X6), Meeting DB clock
  lease_count      INT NOT NULL DEFAULT 0,      -- CRITICAL only: leases ever granted
  ns_completed_at  TIMESTAMPTZ,                 -- CRITICAL only: service reported the provider accepted it (X7); terminal
  decided_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at       TIMESTAMPTZ NOT NULL         -- purge after expiry + 30 d
);
-- trigger: UPDATE allowed only for (route, decision_version+1, reason REROUTED_*/AUTO_FAILOVER, ns_acked_at,
--          send_lease_until, lease_count+1, ns_completed_at); a row with ns_completed_at set can never be rerouted

-- Meeting database only
CREATE TABLE account_notification_outbox (      -- CRITICAL lane outbox (sealed payloads)
  id UUID PRIMARY KEY, decision_id UUID NOT NULL UNIQUE REFERENCES notification_route_decisions(decision_id),
  event JSONB NOT NULL,                          -- envelope; code sealed to the service key (HPKE)
  fallback_sealed BYTEA NOT NULL,                -- the same code sealed to Account's local fallback key (AES-256-GCM)
  state VARCHAR(12) NOT NULL CHECK (state IN ('PENDING','ACKED','SUPERSEDED','EXPIRED')),
  attempts INT NOT NULL DEFAULT 0, next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  lease_id UUID, lease_until TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at TIMESTAMPTZ NOT NULL
);
```

The Meeting `outbox_events` table gains the channel `NOTIFICATION` for non-CRITICAL Meeting events, and each such row references a `decision_id`. Messaging's outbox publication carries the decision in its event.

Counted as producer-side additive tables: Meeting `notification_routing_state`, `notification_route_decisions`, `account_notification_outbox`; Messaging `notification_routing_state`, `notification_route_decisions`. Producers without an old delivery path (Hub, Organizations, Calling, Developer Platform) need **no** decision table: everything they publish is new-only, so nothing can duplicate.

### 9.3 Decision algorithm (inside the producer transaction)

```
route_message(key, category, build_old_row, build_event):
  state    = SELECT * FROM notification_routing_state WHERE category = $category   -- cached ≤ 5 s, re-read on version change
  existing = SELECT * FROM notification_route_decisions WHERE idempotency_key = $key FOR UPDATE
  if existing:                          -- retry of the same message (resend, duplicate request, crash replay)
      return existing.route             -- sticky: follow the earlier decision; write nothing new
  route = OLD if state.mode in (LEGACY_ONLY, SHADOW, FENCED_OLD)
          NEW if state.mode = FENCED_NEW and canary(key, state.canary_percent)
          else OLD
  if category is CRITICAL and state.critical_fallback = FORCED: route = OLD
  INSERT notification_route_decisions(key, decision_id, category, route, routing_version=state.routing_version, reason=ROUTED)
  if route = OLD: build_old_row(decision_id)       -- email_deliveries / meeting_notifications / messaging_push_jobs
  else:           build_event(decision_id)         -- outbox_events NOTIFICATION / account_notification_outbox / Messaging publication
  -- SHADOW mode additionally writes a shadow event (state SHADOW) that the service may never send (I1 marks it SHADOWED)
```

Requirements:
- **Deterministic keys everywhere.** Categories that today use random or no keys get deterministic ones:
  - `NOTIFICATION` device notices: `device-linked:<link_id>`;
  - `MEETING_INVITATION`: `meeting-invitation:<invitation_id>`;
  - `ORGANIZATION_INVITATION`: `org-invitation:<invitation_id>:<token_version>`.
  
  This is part of N7.
- **Push keys.** The key is the Messaging outbox `event_id`. The decision covers every device fan-out for that event.
- **Canary is a pure function** of the key: `hash(key) mod 100 < canary_percent`. A retry therefore always yields the same route, even if it bypasses the existing-row check.

### 9.4 Legacy-table guard triggers (mixed-version protection in the database)

A `BEFORE INSERT` trigger is added to each old-path table:
- `email_deliveries` (keyed by `idempotency_key`);
- `meeting_notifications` (keyed by `dedupe_key`);
- `messaging_push_jobs` (keyed by `event_id`).

The trigger:
1. looks up the category's `notification_routing_state.mode`;
2. if the mode is `LEGACY_ONLY`, allows the insert (pre-fence behaviour; old binaries work unchanged);
3. otherwise requires a `notification_route_decisions` row for the key with `route = OLD`. If none exists, it **raises** `ROUTING_DECISION_REQUIRED`.

**Effect:** once a category leaves `LEGACY_ONLY`, a pre-fence binary (which writes old-path rows without decisions) fails loudly instead of creating a second copy. The release tooling makes sure such a binary is never running then (§9.6). The trigger is the second, independent line of defence.

The trigger is installed by the **fence-capable release's migration**, with every category initialised to `LEGACY_ONLY`. In that state its behaviour is a no-op.

### 9.5 Responsibility boundary (acknowledgement)

| State of the message | Who is responsible | Producer may reroute? |
|---|---|---|
| Decision `NEW`, outbox row PENDING (I1 never returned `ACCEPTED`) | producer | **yes, unilaterally**. In one producer transaction: decision → `OLD` (`REROUTED_UNACKED`), outbox row → `SUPERSEDED`, old-path row created. Zero duplicate risk: the service never committed the event |
| Decision `NEW`, `ns_acked_at` set | the service | only through §9.8 (two-phase cancel, or a CRITICAL lease) |
| Decision `OLD` | producer (old path) | no reroute to NEW (old path finishes it) |

**Crash safety of the acknowledgement:**
- The relay marks the row `ACKED` only after receiving `ACCEPTED`.
- If the relay crashes between the service commit and marking `ACKED`, it re-posts the event, and I1 answers `DUPLICATE` (`ACCEPTED` semantics). The relay must treat `DUPLICATE` as acknowledged.
- Unilateral reroute of an unacked row therefore requires one more step first. The producer calls I1 with the same event ID in **probe mode** (`dryRun:true`); `UNKNOWN` confirms the service never committed it.
- If the service is unreachable:
  - **CRITICAL rows** may still be rerouted under the lease rule of §9.8. The service can only send a CRITICAL email under a lease recorded in the producer's own decision row, so a decision that was never leased is provably unsent.
  - **STANDARD rows** wait: delay, never duplication.

### 9.6 Mixed-version deployments

| Situation | Protection |
|---|---|
| Producer replicas on different versions during a rolling release | A category may leave `LEGACY_ONLY` only through an activation operation. That operation **refuses** unless certify shows every running replica of the producer reporting `routingProtocol >= 1` in `/ready`. Pre-fence binaries do not report it. The guard trigger (§9.4) catches anything that slips through |
| Producer image rollback to a pre-fence version | **Rollback floor.** The coordinated release refuses to deploy a producer image without fence support while any category of that producer is not `LEGACY_ONLY`. The operator must first route every category back to `LEGACY_ONLY`, through routing operations (§9.8) that drain or reroute in-flight messages |
| Service replicas on different versions | The envelope carries `route.protocol`. Each service release supports protocols N and N−1. An unsupported protocol gets I1 503 (`PROTOCOL_UNSUPPORTED`), the producer retries, and another replica accepts. A protocol bump needs two releases (expand, then contract) |
| Producer and service disagree on mode (operator error) | Producer `FENCED_NEW` with service mode `OFF`: I1 answers `HELD`. The event is stored, never sent, and an alert fires; the operator fixes the mode and HELD events resume. Producer `FENCED_OLD` with service `ACTIVE`: the service receives nothing new for that category, so nothing duplicates. Certify fails on any disagreement (check "routing agreement") |
| Config cache staleness (≤ 5 s) | Harmless. Each decision records the `routing_version` it used. A decision is sticky once made, and both routes are valid during the transition window |

### 9.7 Retries and crashes

| Event | Outcome |
|---|---|
| User or business retry (resend verification, repeated invitation request) with the same key | existing decision found and followed (sticky); no second row in either system |
| Retry with a new key (for example a new code) | a new message with its own decision; never a duplicate of the old one |
| Producer crash before commit | nothing written (decision, old row and outbox row are one transaction) |
| Outbox relay crash after the service commit | re-post → `DUPLICATE` → treated as acknowledged |
| Service crash after the provider accepted, before commit | the row is re-claimed; Resend receives the **same** `Idempotency-Key` (UUIDv5 of the key) and returns the original result within Resend's idempotency window. SMTP has no idempotency: a residual duplicate is possible and counted (§9.11) |
| Service crash between lease grant (X6) and send | the lease lapses (≤ 30 s); the re-claim requests a new lease |
| Legacy email worker crash | unchanged existing behaviour (old ledger lease and retry; N0-4 fixes the lease length) |

### 9.8 Rollback and reroute

**Routing rollback** sets `notification_routing_state.mode` to `FENCED_OLD` (or `LEGACY_ONLY` once drained) through one operation, without a restart. It affects **only new decisions**. Messages already decided keep their route.

In-flight `NEW` messages are handled as follows:

| Message state | Path |
|---|---|
| Unacked (outbox PENDING) | unilateral reroute (§9.5) |
| Acked, service healthy | **drain in the service** (default; no action needed) |
| Acked, service unhealthy, STANDARD lane | **two-phase reroute:** (1) I11 `cancelForReroute` → the service marks never-dispatched items `CANCELLED_FOR_REROUTE` and reports the rest as `IN_FLIGHT_OR_SENT`; (2) for `CANCELLED` IDs only, one producer transaction: decision → `OLD` (`REROUTED_CANCELLED`), old-path row recreated from source data. `IN_FLIGHT_OR_SENT` items are never rerouted. If the service is unreachable, STANDARD items wait: delay, never duplication |
| Acked, CRITICAL lane (codes, security notices) | **lease-fenced reroute** (below) |

**CRITICAL lease protocol (X6/X7):**
1. Before every CRITICAL provider call, the service claims a lease through X6. In one row-locked Meeting transaction, Account checks `route = NEW`, `ns_completed_at IS NULL` and no live lease; it then sets `send_lease_until = now() + 30 s` and increments `lease_count`.
2. The service makes the provider call with a 20 s timeout, so it always finishes inside the lease.
3. Immediately afterwards it calls X7 `completeCriticalSends`, which sets `ns_completed_at`. Completed decisions are terminal and can never be rerouted.
4. Reroute rules, enforced by Account in the same row-locked transaction that writes the old-ledger row:

| Decision state | Reroute allowed? | Duplicate risk |
|---|---|---|
| `lease_count = 0` (never leased) | **yes**, automatic or manual (`REROUTED_CRITICAL_LEASE` / `AUTO_FAILOVER`) | **none**: the service never had permission to send, and any later X6 claim answers `REVOKED` |
| live lease (`send_lease_until > now()`) | no; wait for expiry | — |
| `ns_completed_at` set | **never** | — |
| leased at least once, lease expired, not completed (the service crashed during or after a provider call) | **not automatic.** Manual break-glass only (`critical_fallback = FORCED`), recorded `REROUTED_POSSIBLE_DUPLICATE` | at most one extra email carrying the **same** code; recorded and counted |

**Reroute capability per category:**

| Category | Reroute content source | Reroutable? |
|---|---|---|
| Verification / reset / step-up codes | `fallback_sealed` (same code, Account key) | yes (lease-fenced) |
| Security notices | re-rendered from the account event | yes (lease-fenced) |
| Meeting reschedule / cancel / reminder | re-rendered from the conference (X2 data is Meeting's own) | yes (two-phase) |
| Developer notices | re-rendered from Developer Platform data | yes (two-phase) |
| Invitation emails (token) | token never stored by the producer | **no.** Cancelled items are reported to the inviter as delivery FAILED (`emailDelivery`), and the inviter can resend, which mints a new token |
| Push wake-ups | ephemeral | no (they expire within 24 h) |
| In-app | no old path | n/a |

### 9.9 Reminder scheduling takeover (time partition)

The old reminder scan (`enqueue_due_reminders`) and the service scheduler must never both produce a reminder for the same start. The takeover therefore happens in two steps:
1. **Phase 4.3a.** Meeting's existing scan remains the only reminder *trigger*. Each reminder it finds is routed by the decision protocol (key = today's `reminder_key`, `reminder:<conference>:<start_epoch>`), so OLD reminders render in Meeting and NEW reminders render in the service.
2. **Phase 4.3b (scheduler takeover).** One operation records a boundary `B` (UTC) in both the Meeting routing state and `notif_routing`.
   - The Meeting scan handles only starts `< B`.
   - The service scheduler handles only starts `>= B`, scheduling from `conference.*` events.
   - `B` is chosen at least `lead + 24 h` in the future, so every start is owned by exactly one side by construction, independent of timing or clock skew within that margin.

### 9.10 Detection (cross-ledger audit)

| Check | Where | Frequency | Violation |
|---|---|---|---|
| Old-path rows whose decision is `NEW` (or missing, outside `LEGACY_ONLY`) | Meeting and Messaging DB (one SQL each) | hourly (certify extension) | any row |
| Decisions `NEW` acked but absent from the service | producer digest vs I8 route audit (per category and hour) | hourly | digest mismatch → list diff |
| Service rows whose decision is not in the producer's `NEW` set | I8 digest vs producer | hourly | any |
| HELD events | I5 | continuous (alert) | any |
| CRITICAL sends without a granted lease | service metric `notif_critical_send_without_lease_total` | continuous | > 0 |

Any violation pages through the operator path (D-9) and freezes the affected category (`FENCED_OLD`) automatically.

### 9.11 Residual duplicates (recorded, never silent)

| Residual | Bound | How it is recorded |
|---|---|---|
| SMTP provider: crash after the SMTP server accepted, before commit | one extra identical email per crash | `notif_email_messages.attempts > 1` with `last_error_class = COMMIT_LOST`; metric |
| Resend: retry after Resend's idempotency window | one extra identical email | same |
| CRITICAL lane: manual FORCED reroute of a leased-but-uncompleted decision (the service crashed mid-send) | at most one extra email with the **same** code | decision `REROUTED_POSSIBLE_DUPLICATE`; metric `account_critical_possible_duplicate_total` |
| Push: a wake-up retried after a provider timeout | one extra metadata-only wake-up; collapse ID replaces the visible notification | delivery attempts |

The old and new **ledgers never hold the same message**. That is invariant F2, checked hourly.

---

## 10. Reliability (Part 11)

### 10.1 Flow classes

| Class | Flows | Requirement | Design |
|---|---|---|---|
| R0 synchronous critical | sign-in, code verification, ringing, message send | must work with the service down | the service is never in these paths |
| R1 critical delivery | verification, reset and step-up codes; security notices | durable; p95 handoff to provider ≤ baseline + 5 s (baseline = Q17, outstanding); survives service restarts | Account transactional outbox; CRITICAL lane on both replicas; X6/X7 leases; AUTO and FORCED fallback (§10.4) until Phase 8 |
| R2 time-sensitive | call push, message wake-ups, missed calls, reminders | at-least-once, expire when late | CALL fast lane (≤ 1 s, best-effort); HIGH priority |
| R3 informational | org, contacts, developer, recordings | delay tolerated | STANDARD |

### 10.2 Failure behaviour

| Failure | Behaviour |
|---|---|
| Service outage (both replicas) | Producer outboxes hold events (Hub, Organizations, Calling, Messaging, Meeting). CRITICAL: AUTO failover to the old path after 60 s (§10.4). Ringing, sign-in and messaging are unaffected |
| One replica down | the other claims everything (`SKIP LOCKED`) |
| Service database outage | `/ready` 503; same as a service outage (the Meeting DB is a separate container, §10.4) |
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

### 10.4 Emergency authentication-email fallback (verified design)

The fallback is the **retained old Meeting email path**: `email_deliveries` plus the existing `EmailWorker` with its Resend/SMTP providers. It is fed by the routing protocol, not by a container restart.

**Two modes**, both set in `notification_routing_state` in the Meeting database:

| Mode | When | What happens |
|---|---|---|
| **AUTO** (default once a CRITICAL category is `FENCED_NEW`) | the Account outbox relay cannot get `ACCEPTED` from I1 for 60 s, or X6 has not granted a lease within 60 s of creation | the relay reroutes **never-leased** CRITICAL decisions to the old ledger (`AUTO_FAILOVER`). The code comes from `fallback_sealed`, so it is the same code, and there is no duplicate (§9.8). New CRITICAL decisions keep trying `NEW` first, each with its own 60 s budget |
| **FORCED** (manual break-glass, one coordinated-release operation, no restart) | operator decision | every new CRITICAL decision routes `OLD` immediately. Leased-but-uncompleted decisions may be rerouted with the recorded possible-duplicate reason |

A code that has expired is never sent by either path. Users can always request a new code, which creates a new key and a new decision.

**Fallback dependency matrix.** "Fallback works" means a code email reaches the provider through the old path.

| Failure | Sign-in / code verification | Code email via the service | Fallback | Evidence it is independent |
|---|---|---|---|---|
| One service replica down | unaffected | the other replica serves | not needed | `SKIP LOCKED` claims |
| **Both service replicas down** | unaffected (Account never calls the service synchronously) | unavailable | **AUTO after 60 s**: unacked or never-leased rows reroute; new rows reroute after their 60 s budget | the old path runs entirely inside the Meeting Service process and its database |
| **Service database (`cfmeet_notifications` in `cfmeet-core-db`) down** | unaffected for Meeting-local sign-in: the Meeting database is in the separate legacy `cosmicforge-db` container (certify 43670057279: meeting `database.host = postgres`, `rtc_db`; core services use `cfmeet-core-db`) | unavailable (I1 cannot commit; X6 leases are in the Meeting DB, but the service cannot run its claim loop) | **AUTO** as above | the old path needs only the Meeting DB, Redis and the provider. Note: `cfmeet-core-db` also hosts Platform Access, Hub, Messaging and Calling, so other features degrade; that is pre-existing and out of scope |
| Service up, provider down | unaffected | retries, then the provider breaker opens | **the same provider is down for both paths.** The fallback only helps if a *different* transport is configured for the old path (D-14: keep a working SMTP account on the legacy worker while Resend serves the service, or the reverse). Without D-14 the result is delay, not loss | — |
| Service partitioned from the Meeting Service (X6 unreachable) | unaffected | cannot obtain leases → sends nothing (safe) | AUTO after 60 s for never-leased rows | lease protocol |
| Meeting database down | **sign-in is down today as well** (pre-existing) | — | — | unchanged risk |
| Redis down | sign-in codes fail today (OTP state in Redis) | — | — | unchanged risk |
| Legacy worker not running or credentials missing | unaffected | unaffected | **fallback broken.** Prevented by the readiness invariants below | certify check |

**Fallback readiness invariants.** These are certified weekly and before every CRITICAL routing change:
1. The Meeting Service email worker is running with a configured provider. It keeps its **own** mounted credentials, which are not moved: the same Resend key is mounted in both containers, or SMTP per D-14.
2. A **fallback drill** (`email-acceptance` with `confirm=fallback`) sends a controlled acceptance email through the old path, with category `OPERATIONS_ACCEPTANCE`, and requires provider acceptance. With Resend and a webhook secret, it also requires `DELIVERED`.
3. `notification_routing_state.critical_fallback = AUTO` for every `FENCED_NEW` CRITICAL category.
4. The Account fallback key is mounted (`fallback_sealed` can be decrypted). A self-test runs at startup.
5. Alert rules for the CRITICAL lane are live and have delivered a test alert (D-9).
6. With Resend, the Meeting Service's own webhook endpoint stays registered at Resend (with its secret) for as long as the fallback exists, so fallback emails also produce delivery evidence. Both endpoints stay registered until Phase 8.

**Verification tests** (Document 7): ST-20, R-18, R-19, R-21, R-22, R-23 and gate A4.6.

The fallback stays in place until Phase 8 retirement, and is removed only after 90 days of the CRITICAL lane with zero AUTO or FORCED use (Document 5 §10).

---

## 11. Workers (canonical register)

**In the service (5 permanent + 1 temporary):**

| Worker | Lane | Cadence |
|---|---|---|
| W1 ingest processor | all | 100 ms wake + poll |
| W2 critical dispatcher (claims X6 leases, reports X7 completion) | CRITICAL | 100 ms |
| W3 standard dispatcher (email, push, in-app) | HIGH / STANDARD | 500 ms |
| W4 scheduler (reminders after takeover §9.9, deferrals, expiry, cancel) | — | 5 s |
| W5 retention and erasure | — | hourly |
| W6 shadow comparator (temporary; removed in Phase 8) | — | 60 s |

The CALL fast lane is a synchronous handler (I3), not a worker.

**Producer-side (1 new + 6 extended existing):**

| Worker | Owner | Change |
|---|---|---|
| PW1 Account notification outbox relay (AUTO failover, §10.4) | Meeting Service | **new** |
| Meeting outbox relay (`workers/outbox_relay.rs`) | Meeting Service | extended: channel `NOTIFICATION` with route decisions |
| Messaging outbox publisher | Messaging | extended: route decision + publication to the service |
| Hub outbox dispatcher | Communications Hub | extended: destination `NOTIFICATIONS` |
| Organizations outbox worker | Organizations | extended: subscriber `cfmeet.notifications` |
| Calling outbox publisher | Calling | extended: destination `notifications` + missed-call fact |
| Developer Platform worker | Developer Platform | extended: publication |

**Retained unchanged until Phase 8 (not counted as new work):**
- Meeting `EmailWorker`: the fallback path.
- Meeting notification worker: the reminder trigger until takeover, §9.9.
- Messaging push dispatcher: until Phase 4.1.

---

## 12. Shared packages

| Package | Change |
|---|---|
| `packages/service-auth` | identity `cfmeet.notifications`; new permissions (I1–I12, X1–X7) |
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
| Runbooks | outage → AUTO/FORCED fallback (§10.4); replay; provider credential rotation; HPKE key rotation |

---

## 14. Email provider architecture (preserved) and delivery evidence

### 14.1 What is preserved

The service's email channel is a **port** of the existing adapter architecture, not a redesign. **No new provider dependency is introduced** (brief Part 2 rule 7).

| Element today (Meeting Service) | In the service |
|---|---|
| `EmailProvider` trait with `ResendProvider`, `SmtpProvider`, `RecordingProvider` (tests), and no provider (`email_delivery.rs:270-540`) | the same trait and the same three adapters, moved with their unit tests |
| Provider selection `EMAIL_PROVIDER` = `resend` \| `smtp` \| `none`; unset → `smtp` if `SMTP_HOST` is set, else `none` (`config/email.rs:137-263`) | the same rules under `NOTIFICATIONS_EMAIL_PROVIDER`, with the same variable names prefixed `NOTIFICATIONS_` (`…_RESEND_API_KEY_FILE`, `…_SMTP_HOST`, …), file-based secrets only |
| Error classification (Resend 429/409/4xx/5xx; SMTP 535 → temporary authentication error) | identical |
| Svix-verified Resend webhooks | identical (public route §3.2) |
| `none` → no worker, mail stays queued | identical, plus mandatory expiry (codes never linger) |
| Release-tooling safe defaults (provider variables refused until activation) | identical pattern, new activation operations |

Provider choice remains **owner configuration**, not code. The Meeting Service keeps its own provider configuration throughout, because it is the fallback path (§10.4).

### 14.2 Development is independent of provider activation

| Track | Owner | Depends on |
|---|---|---|
| **N-track** (engineering N1–N10, Document 6) | engineering | `RecordingProvider` and test adapters only. **No production provider is required to build, test or deploy the service dark or in shadow** |
| **E-track** (provider activation on the existing Meeting path: Resend per `docs/operations/resend-email-owner-handoff.md`, or a working SMTP account) | owner | nothing in the N-track |
| **Moving email categories** (Phases 4.3–4.6) | both | E-track evidence gates (§14.3) |

### 14.3 Real delivery evidence gates

*Real delivery evidence* means one of the following for a real message to a controlled inbox:
- Resend: an `email.delivered` webhook event, verified by Svix;
- SMTP: a `250` acceptance **and** a confirmed receipt in an owner-controlled acceptance mailbox, recorded in a dated ops report.

Provider acceptance alone (`ACCEPTED`) is not delivery evidence.

| Gate | Required before | Evidence |
|---|---|---|
| E1 | any email category moves (4.3) | the old path delivers in production: `email-acceptance confirm=otp` PASS with delivery evidence (fixes S-2) |
| E2 | any email category moves (4.3) | the service path delivers the same provider's acceptance email with delivery evidence |
| E3 | security notices move (4.5) | ≥ 14 days of STANDARD categories from the service: delivered ratio ≥ the old path's baseline (Q1/Q17); bounce and complaint ratios ≤ baseline + 2σ; zero F2 violations |
| E4 | authentication codes move (4.6) | E3, plus `email-acceptance confirm=otp` through the service with delivery evidence at every canary step, plus a successful fallback drill (§10.4 invariant 2) within the previous 7 days |
