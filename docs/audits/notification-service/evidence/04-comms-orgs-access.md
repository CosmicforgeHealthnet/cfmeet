# Evidence 04: notification behaviour in Communications Hub, Organizations, Platform Access, communications-policy and service-auth

Repo: `/home/user/cfmeet`, HEAD `6f3743f806434a55bad8297f104d28a55e350e2e` (2026-11-15). Read-only audit; nothing was changed or executed against any environment.

## 0. Classification vocabulary and evidence rules

| Status | Meaning in this report |
|---|---|
| implemented | The code exists and is reachable from a route or worker in this repo. |
| configured | A flag, env var, trust-bundle grant or release-tool setting exists that turns it on. |
| activated | An operations report with a date and run ID says it is on in production. |
| production-tested | A production acceptance or certify run with an ID exercised it. |
| disabled | Implemented, but off by default or by a missing grant or credential. |
| broken | The code path cannot succeed as configured. Static evidence only, never verified live. |
| deprecated | Marked deprecated in code or docs. |
| absent | No code exists. |

Production state is never inferred from code alone. Wherever I cite a production status, I give the dated document and the GitHub Actions run ID.

---

## 1. Executive findings (most important first)

1. **No service in scope sends a user-facing notification itself.** The Communications Hub, Organizations and Platform Access have no email, push, SMS or in-app notification transport. The only real notification senders are:
   - the Meeting Service (Account), which sends email through `email_deliveries` to Resend or SMTP;
   - Messaging's push adapters (APNs, FCM, Web Push), which are all **disabled**: `MESSAGING_PUSH_PROVIDER=none`, and there are no credentials.
   (Evidence: `docs/operations/CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5, "The Meeting Service is the only email sender", and "Communications Hub email (the Hub only owns preferences)". Also `docs/operations/CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:79`, where Messaging push is `none`.)
2. **The Hub owns a notification preference model, but only 3 of its 7 category switches are enforced anywhere:**
   - `direct_messages` and `group_messages` are enforced through `policy/check`, called by Messaging's push dispatcher. Push is disabled, so in effect they are not exercised in production.
   - `meeting_invitations` is enforced by the Meeting Service's email notifier, through `/notification-preferences/resolve`.
   - `incoming_calls`, `missed_calls`, `contact_requests` and `group_invitations` are **stored only**. No reader exists.
   - There are **no channels** (email, push or in-app per category), **no quiet hours / DND, no timezone, no digest, no read state**.
3. **Probably broken in production: the Meeting Service → Hub preference read.**
   - The Hub route requires the `communications-preferences.read` grant to `cfmeet.meeting-service`.
   - That grant appears in **no** release tool. `infra/calling/production/communications_release.py:178-180` grants Meeting only `communications-user.forward` and `communications-account.apply`. `coordinated_release.py:534-560` `GRANTS` has no row for it.
   - The Hub trust bundle is rebuilt from scratch on each Communications release (`communications_release.py:168`, `trusted_issuers=[]`).
   - The Meeting Service always uses `HubPreferences` when the Communications bridge is configured (`services/meeting-service/src/main.rs:193-203`). Notification email is on in production (`NOTIFICATIONS_EMAIL_ENABLED=true`, run 42730700547, 2026-11-10).
   - By design, a failed read fails closed. Meeting emails to account holders end `FAILED` after 8 attempts, and invitation emails report `emailDelivery: FAILED`. The source doc says so: `docs/operations/profile-notifications-sections-9-11.md:221-227`.
   - This is static evidence. The live bundle was not inspected.
4. **Hub "events" are durable fan-out records, not notifications.**
   - Contact requests, accept/decline, connection removal, blocks and privacy changes write three things:
     - an audit row;
     - a `communications.*` outbox event to Calling and Messaging;
     - a per-user change-feed row.
   - Calling and Messaging dedupe every event but act only on `credentialsNotBefore` (revocation) and `communications.timeline.changed` (Messaging realtime nudge). The rest are recorded and ignored.
   - A recipient learns of a contact request only by polling `/sync`, `/contact-requests/incoming` or `/bootstrap` counts.
5. **The Hub's inbound owner-event families (`/events/{calling|messaging|meetings|devices}`) have no producers in the repo.** The `communications-events.publish` permission is "defined, not yet granted in production" (`docs/api/communications-unified-handoff.md` §5). The Hub converges by polling owners (5 s sweep, re-check on read). `block-events` is **deprecated** (no producer).
6. **Organizations has a production transactional outbox, but its only subscriber is `cfmeet.meeting-service`, for 13 event types** (`services/organizations/src/outbox.rs:41-59`).
   - **`invitation.*` and `team.*` events have no subscriber**, despite the contract saying "Notification owner delivers transactional email" (`docs/events/organization-event-contracts.md` §2).
   - Organization invitation email is a synchronous side effect in the Meeting Service relay (`services/meeting-service/src/handlers/v2/organizations.rs:936-1023`), using the raw token from the Organizations response.
   - Membership, role, suspension and ownership events reach Meeting, which updates access only (`services/meeting-service/src/services/organization_events.rs:186-206`). **No user is notified.**
7. **Platform Access has no sessions, devices, typed user tokens or notifications.** It owns tenants, applications, environments, service accounts, credentials, scopes and principals, plus an append-only audit log and a pollable identity change feed.
   - Sessions, the revocation epoch (ADR 0057) and linked devices (ADR 0060) belong to the Account authority in `services/meeting-service`.
   - The only user-facing security notices are Account emails:
     - new primary mobile, or new trusted browser (`services/meeting-service/src/services/account_devices.rs:1724-1743`);
     - password changed (`handlers/auth.rs:878, 1020`).
   - These are not gated by Hub preferences.
8. **A new internal Notification Service can authenticate with `cf-service-auth` without new crypto, but it needs code changes:**
   - add `InternalPermission` variants and `REGISTRY` entries (`grantable_to` is a closed list);
   - update the contract schema (`packages/contracts/platform/common/v1.schema.json`);
   - add trust-bundle grants on each receiver;
   - change Hub handlers that hard-code the caller identity. For example, `preferences.rs:90` refuses any caller other than `meeting_service`.

---

## 2. Communications Hub (`services/communications`)

### 2.1 Process, workers and configuration

| Item | Evidence | Notes |
|---|---|---|
| Binary | `services/communications/src/main.rs:5-70` | Single process: HTTP listener and one background worker. |
| Worker tick | `main.rs:31-46` | Every 500 ms: `deliver_due()` (outbox). Every 60 s (`count%120`): `maintain()` (retention and contact-request expiry). Every 5 s (`count%10`): `reconcile_call_cards(20)`. |
| Config env | `src/config.rs:26-118` | Required: `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`, `CF_SERVICE_TRUST_BUNDLE_FILE`, `CF_PLATFORM_ACCESS_BASE_URL`, `CF_MEETING_SERVICE_BASE_URL`, `COMMUNICATIONS_INVITE_DIGEST_KEY_FILE`, `CFMEET_PUBLIC_APP_URL` (production must be `https://cfmeet.com`), `DATABASE_URL`. Optional: `CF_CALLING_BASE_URL`, `CF_MESSAGING_BASE_URL`, `COMMUNICATIONS_BIND_ADDRESS`. |
| Service token TTL | `config.rs:35-41` | 60 s. |
| Feature flags for notifications | **absent** | The Hub has no notification flags. All behaviour is live once the image is deployed. |
| Public surface | The Hub has **no public listener**. `/api/v2/communications/*` is mounted by the Meeting Service (`services/meeting-service/src/routes/mod.rs:86-89`). The Meeting Service calls `POST /internal/v1/communications/dispatch` with `Dispatch{method,path,query,body,idempotencyKey,account:VerifiedAccount}` (`packages/communications-policy/src/lib.rs:251-260`). Gateway entries such as `infra/api-gateway/route-manifest.json:742-765` use auth `MEETING_SESSION` and upstream `cfmeet.meeting-service`. | |

### 2.2 Preference model (exhaustive)

#### 2.2.1 Account-wide notification categories: `communication_notification_preferences`

DDL: `services/communications/migration/sql/002_unified.sql:74-83` (history `communications_unified_seaql_migrations`, migration `m20261106_000001_unified`, `migration/src/lib.rs:43-73`).

| Column | Type | Default | Constraint |
|---|---|---|---|
| `profile_id` | UUID PK | — | FK `communication_profiles(id)` |
| `version` | BIGINT | 1 | `CHECK (version>0)`; trigger `communication_guard_version` (`:83`) |
| `direct_messages` | BOOLEAN NOT NULL | true | |
| `group_messages` | BOOLEAN NOT NULL | true | |
| `incoming_calls` | BOOLEAN NOT NULL | true | |
| `missed_calls` | BOOLEAN NOT NULL | true | |
| `contact_requests` | BOOLEAN NOT NULL | true | |
| `group_invitations` | BOOLEAN NOT NULL | true | |
| `meeting_invitations` | BOOLEAN NOT NULL | true | |
| `updated_at` | TIMESTAMPTZ | now() | |

- **Absent row means all true.** See `chat.rs:516-523` `notification_view`: `get = r.map(..==true).unwrap_or(true)`.
- **Hard-coded response fields:**
  - `"securityEvents":true`. The comment at SQL `:74` says "Security/session events are always delivered", but no security event passes through the Hub.
  - `"payloadPolicy":"ROUTING_METADATA_ONLY"`.
- **Write path:** `State::put_notification_preferences` (`chat.rs:358-419`).
  - The full replacement body is `{expectedVersion, directMessages, groupMessages, incomingCalls, missedCalls, contactRequests, groupInvitations, meetingInvitations}`, with `deny_unknown_fields`.
  - It uses optimistic concurrency: version 0 inserts; otherwise `UPDATE … WHERE version=$9`, and a miss returns `Error::Version`.
  - It uses the idempotency op `notification-preferences.put`.
  - It writes a change-feed row with domain `NOTIFICATION_PREFERENCE` (`:403-410`).
  - It writes no audit row and no outbox event.
- **Read path:** `get_notification_preferences` (`chat.rs:354-357`). It is also embedded in `/bootstrap` (`sync.rs:111-123`) and in `/me/notification-delivery-status` (`devices.rs:526`).

**Missing from the model, by absence in DDL and grep:**
- channel dimension (email, push, in-app, SMS per category);
- quiet hours / DND (no grep hit repo-wide for `quiet_hours`, `do_not_disturb` or `dnd`);
- timezone. The Account profile has `users.timezone` (Meeting Service, `docs/operations/profile-notifications-sections-9-11.md` §1). The Hub does not use it.
- digest or frequency;
- per-organization or per-workspace preferences;
- security, account or organization categories;
- locale.

#### 2.2.2 Per-conversation preferences: `communication_conversation_preferences`

DDL `002_unified.sql:36-48`:

| Column | Type | Default | Constraint |
|---|---|---|---|
| `owner_profile_id` | UUID | — | FK profiles; PK part |
| `conversation_id` | UUID | — | Messaging reference, no FK; PK part |
| `notifications` | TEXT | `'ALL'` | `IN ('ALL','MENTIONS','MUTED')` |
| `muted_until` | TIMESTAMPTZ | NULL | `CHECK (muted_until IS NULL OR notifications='MUTED')` |
| `media_auto_download` | TEXT | `'WIFI'` | `IN ('ALWAYS','WIFI','NEVER')` |
| `version` | BIGINT | 1 | guard trigger |
| `created_at` / `updated_at` | TIMESTAMPTZ | now() | |

- **API:** `GET|PUT /api/v2/communications/conversations/{id}/preferences`. Dispatch is at `api.rs:571-578`, the handler at `chat.rs:64-151`.
- **Validation (`chat.rs:91-102`):** `mutedUntil` must be in the future and within 366 days, and is allowed only with `MUTED`.
- **Membership:** checked live with Messaging (`self.conversation(...)`, `chat.rs:70, 104`).
- **View (`chat.rs:20-44`):**
  - `effectiveNotifications` becomes `ALL` once `muted_until` has passed.
  - It lists device-local settings (`chatLock`, `theme`, `wallpaper`, `translation`, `galleryVisibility`).
  - It says disappearing messages are owned by Messaging.
- **Inbox overlay:** `GET /inbox` (`sync.rs:137-180`) returns `notifications` and `mutedUntil` per conversation.

#### 2.2.3 Per-contact mute: `communication_contact_aliases.notifications`

DDL `001_foundation.sql:72-79`: `notifications TEXT DEFAULT 'ALL' CHECK IN ('ALL','MUTED','MENTIONS')`, plus `alias`, `favorite`, and `favorite_rank` (added at `002_unified.sql:7`).
- Set with `PATCH /contacts/{contactId}` (`relationships.rs:299-372`).
- There is **no `muted_until`**, unlike the conversation mute. This is duplication with inconsistent semantics.

#### 2.2.4 Communication privacy (gates notification eligibility): `communication_privacy`

DDL `001_foundation.sql:25-35`:

| Column | Default |
|---|---|
| `public_id_discoverable` | true |
| `invitation_permissions` | `ENABLED` (or `DISABLED`) |
| `contact_request_permissions` | `EVERYONE` (or `NOBODY`) |
| `incoming_call_policy` | `CONNECTIONS` (or `NOBODY`) |
| `direct_chat_policy` | `CONNECTIONS` (or `NOBODY`) |
| `public_profile_visibility` | `PUBLIC` (or `CONNECTIONS` / `HIDDEN`) |
| `presence_visibility` | `CONNECTIONS` (or `NOBODY`) |

- API `GET|PUT /me/privacy` (`profiles.rs:214-290`).
- Writes audit, the outbox event `communications.privacy.changed` (`profiles.rs:271`), and the feed domain `PRIVACY`.

#### 2.2.5 How a notification decision is computed (the only enforcement engine)

`State::check_policy`, `services/communications/src/policy.rs:88-219`. It is reached through `POST /internal/v1/communications/policy/check` (`api.rs:309-330`). Only Messaging may ask about `MESSAGE_NOTIFICATION` or `GROUP_NOTIFICATION` (`api.rs:315-322`).

Steps for `MessageNotification` / `GroupNotification`:
1. **Scope validation.** `auth::validate_scope` calls Platform Access `GET /internal/v1/identity/context`.
2. **Actor and target profiles must exist**, else `NotPermitted`.
3. **Principals must be active in Platform Access** (`auth::principals_active`, `POST /internal/v1/principals/resolve`).
4. **Both accounts must be ACTIVE in the Hub and in Account** (`active_account`, `POST /internal/v1/accounts/communications/status` to Meeting, called twice). Otherwise `AccountInactive`.
5. **No operator hold** (`communication_operator_holds`).
6. **Optional credential epoch:** `credential_issued_at <= credentials_not_before` gives `CredentialRevoked`.
7. **No block.** The Hub block is checked, plus a live `domain_blocks` call to Calling and Messaging `/internal/v1/communications/blocks/check`.
8. **`decide()`** (`policy.rs:335-356`):
   - `MessageNotification` requires CONNECTED and recipient `direct_chat_policy==CONNECTIONS`.
   - `GroupNotification` is always `true` at this step.
9. **Recipient preference layers** (`policy.rs:174-208`):
   - **Contact mute.** The recipient's alias toward the sender must be `None` or `ALL` (`notification_preference_allows`, `:357-359`). `MENTIONS` counts as muted.
   - **Account switch.** `direct_messages` or `group_messages` must not be `false` (`:191-199`).
   - **Conversation mute**, if `conversation_id` is supplied. Suppressed when `notifications<>'ALL' AND (muted_until IS NULL OR muted_until>now())` (`:202-207`). `MENTIONS` again counts as muted, because ciphertext is opaque.
10. **Return:** `Decision{schemaVersion:1, allowed, reason (ALLOWED|NOT_PERMITTED|ACCOUNT_INACTIVE|CREDENTIAL_REVOKED), policyVersion}`.

**Cost:** one decision makes about **6 signed outbound calls** (2 to Platform Access, 2 to Account, 2 for blocks) plus several database queries. This matters if a central notification service calls it per recipient per message.

**Personal scope only.** Messaging returns `Ok(true)` (no preference check) when the scope is not the configured personal scope, or the context class is not DIRECT or GROUP (`services/messaging/src/realtime/push.rs:340-350`). **Organization and enterprise messaging have no preference layer.**

### 2.3 Who reads the preferences today

| Preference | Reader | Path | Status |
|---|---|---|---|
| `direct_messages`, `group_messages`, contact mute, conversation mute | Messaging push dispatcher `authorized()` → `PolicyClient::check` with `MessageNotification` / `GroupNotification` and `conversation_id` | `services/messaging/src/realtime/push.rs:319-396`; client `packages/communications-policy/src/lib.rs:162-191` | **implemented and configured**: Messaging holds `communications-policy.check` (`communications_release.py:180`). **Not exercised in production**, because push is disabled (`MESSAGING_PUSH_PROVIDER=none`: `CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:79`, 2026-11-13; `cfmeet-final-production-certification.md:169`). CI-tested (`services/communications/tests/unified.rs:1149-1195`, inside `private_owner_and_operator_routes_are_scoped_deduplicated_and_audited`). |
| `meeting_invitations` | Meeting Service `HubPreferences::meeting_opt_outs` → `POST /internal/v1/communications/notification-preferences/resolve` (`category:"MEETINGS"`, at most 100 refs per call) | `services/meeting-service/src/services/notifications.rs:207-258`; enforced at `:460-470` (invitation email) and `:994-1015` (jobs; `SUPPRESSED`); Hub `preferences.rs:37-101` | **implemented**. Hub route test: `tests/foundation.rs:985` `meeting_notification_opt_outs_are_listed_for_the_account_only`. **Grant missing from release tooling, see §1 item 3, so probably broken in production.** |
| `incoming_calls`, `missed_calls` | none (Calling has `NoPushTransport`, `services/calling/src/main.rs:453`; Calling's `communications.rs:89,168` only checks `DirectCall` privacy policy) | — | **stored only, not enforced**. |
| `contact_requests`, `group_invitations` | none. No contact-request or group-invitation notification exists. Messaging uses `GroupInvitation` only as a permission check (`services/messaging/src/membership/mod.rs:397,1224`). | — | **stored only, not enforced**. |
| Hub preference resolve categories | only `MEETINGS` maps to a column (`preferences.rs:43-46`); anything else returns `Validation` | | |
| Clients | `apps/web` and `apps/mobile` have **no** reference to `notification-preferences`, `notificationPreferences` or `delivery-status`. Only the generated SDKs (`sdks/typescript/src/generated/operations.ts`, `sdks/dart/lib/src/generated/operations.dart`) have them. The gap register `docs/audits/cfmeet-feature-gap-register.md:44` ACC-009 says "Hub … has no frontend consumer". | | **no UI consumer** |

`/notification-preferences/resolve` details (`preferences.rs`):
- References must be the canonical `cfmeet-user:<lowercase uuid>`, unique, 1 to 100 of them (`:30-34, 50-59`).
- Answer: `{"schemaVersion":1,"category":…,"optedOut":[refs]}`.
- An account with no profile or no stored row is never listed (default enabled).
- The caller identity must be `meeting_service` (`:90-92`). The permission is also checked in middleware (`auth.rs:74-77`).

### 2.4 Endpoints related to notifications, activity and feeds

Public paths go through the Meeting Service relay. Contract: `packages/contracts/communications/http/v1.openapi.json` v1.3.0. All operations use `bearer_auth` (Account session).

| Method / path | operationId | Handler (Hub) | Notes |
|---|---|---|---|
| GET `/api/v2/communications/me/notification-preferences` | `getNotificationPreferences` | `chat.rs:354` | |
| PUT `/api/v2/communications/me/notification-preferences` | `updateNotificationPreferences` | `chat.rs:358` | requires `Idempotency-Key` (`api::key`) |
| GET `/api/v2/communications/conversations/{id}/preferences` | `getConversationPreferences` | `chat.rs:64` | |
| PUT `/api/v2/communications/conversations/{id}/preferences` | `updateConversationPreferences` | `chat.rs:74` | |
| PATCH `/api/v2/communications/contacts/{contactId}` | `updateCommunicationsContact` | `relationships.rs:299` | contact `notifications` mute |
| GET `/api/v2/communications/me/notification-delivery-status` | `getNotificationDeliveryStatus` | `devices.rs:511-533` | Proxies Messaging `/internal/v1/communications/push-status`, strictly parsed (`PushStatus::parse`, `:549-572`). Reports `callRinging: "mobile push transport NOT_CONFIGURED"` and `physicalDeviceAcceptance:"NOT_RUN"`. |
| PUT / DELETE `/api/v2/communications/devices/{id}/push` | `registerCommunicationsDevicePush` / `removeCommunicationsDevicePush` | `devices.rs:315-376` | The push token passes through to Messaging `/internal/v1/communications/devices/push`. The Hub stores only `push_platform` and `push_registered_at`. |
| GET `/api/v2/communications/activity` | `listCommunicationsActivity` | `calls.rs:629-…` | Recent call and meeting-invitation cards across chats, from `communication_timeline_entries`. **Not a notification inbox**: no read/unread, no dismissal, no categories beyond `CALL` / `MEETING_INVITATION`. |
| GET `/api/v2/communications/inbox` | `getCommunicationsInboxOverlay` | `sync.rs:137` | Per-conversation mute and watermark overlay. Messaging owns unread counts (`sync.rs:133`). |
| GET `/api/v2/communications/sync`, `/events` | `syncCommunicationsChanges`, `listCommunicationsEvents` | `sync.rs:42-110` | Per-user change feed (domains `sync.rs:16-29`). `/events` adds a `domains` filter. Poll only. Retained 30 days; older cursors get `RESYNC_REQUIRED`. |
| GET `/api/v2/communications/bootstrap` | `getCommunicationsBootstrap` | `sync.rs:111-135` | Includes `incomingRequests` / `outgoingRequests` counts (`:115-116`), privacy and notification preferences. |
| GET `/api/v2/communications/contact-requests/incoming` | `listIncomingCommunicationsContactRequests` | `relationships.rs` `list_requests` | The only way a recipient sees a new request. |

Private internal contract: `packages/contracts/communications/internal/v1.openapi.json` v1.6.0, `cfServiceAuth`.

| Path | operationId | Permission |
|---|---|---|
| POST `/internal/v1/communications/notification-preferences/resolve` | `resolveNotificationPreferences` | `communications-preferences.read` |
| POST `/internal/v1/communications/policy/check` | `checkCommunicationsPolicy` | `communications-policy.check` |
| POST `/internal/v1/communications/principals/resolve` | `resolveCommunicationsPrincipal` | `communications-policy.check` |
| POST `/internal/v1/communications/dispatch` | `dispatchCommunicationsUser` | `communications-user.forward` |
| POST `/internal/v1/communications/organization-invitations/resolve` | `resolveOrganizationInvitee` | `communications-user.forward` |
| POST `/internal/v1/communications/contacts/names` | `resolveContactNames` | `communications-user.forward` |
| POST `/internal/v1/communications/account-events` | `applyCommunicationsAccountEvent` | `communications-account.apply` |
| POST `/internal/v1/communications/block-events` | `applyCommunicationsBlockEvent` | `communications-block.apply` (**deprecated**) |
| POST `/internal/v1/communications/backfill`, `/replay`; GET `/metrics` | … | `communications-backfill.run` |

**Contract drift:** the routes below are implemented in `api.rs:58-88` with permissions in `auth.rs:52-73`, but **not in the internal OpenAPI**. They are documented only in `docs/api/communications-unified-handoff.md:33` and `docs/operations/communications-sections-9-23-delivery.md`.
- `/events/{calling|messaging|meetings|devices}` (`communications-events.publish`)
- `/readiness`, `/admin/metrics` (`communications-observability.read`)
- `/reconciliation/range`
- `/admin/suspend` (`communications-profile.suspend`)
- `/verification/callback`, `/admin/phone-recovery`

### 2.5 Contact requests, connections, invitations, favorites, blocks: what each emits

Every row below writes **records only**. None produces a user notification.

| Action | Code | Audit (`communication_audit`) | Outbox event (to CALLING and MESSAGING) | Change-feed domain | User notification |
|---|---|---|---|---|---|
| Send contact request | `relationships.rs:20-108` (cooldown 24 h after decline/cancel `:88-91`; one pending per pair; 30-day expiry `001_foundation.sql:66`) | `CONNECTION_REQUESTED` | `communications.connection.requested` (`:99`) | CONTACT for both (`store.rs:322-332`) | **none** |
| Accept / decline / cancel | `relationships.rs:110-221` | `CONNECTION_REQUEST_{ACCEPTED,DECLINED,CANCELLED}` | `communications.connection.connected` or `.none` (`:211`) | CONTACT both | **none** |
| Request expiry (worker) | `events.rs:483-517` | `CONNECTION_REQUEST_EXPIRED` | `communications.connection.expired` | none | **none** |
| Remove contact | `relationships.rs:375-405` | `CONNECTION_REMOVED` | `communications.connection.revoked` | CONTACT both | none |
| Block / unblock | `relationships.rs:407-470` | yes | `communications.block.changed` (`:447`) | BLOCK (actor), CONTACT (other side) | none |
| Privacy change | `profiles.rs:214-290` | yes | `communications.privacy.changed` | PRIVACY | none |
| Profile activation | `profiles.rs:90-125` | yes | `communications.profile.activated` | PROFILE | none |
| Favorites / lists | `collections.rs:49-110, 132-366` | no | no | CONTACT / CONTACT_LIST | none |
| Connect invites (QR / link) | `invites.rs:35-211`; table `communication_invites` (SHA digest only, `001_foundation.sql:36-44`), at most 10 uses, `purpose='CONNECT'` | `INVITATION_*` | no | — | none (link shared out of band) |
| Profile report | `api.rs:687-…` | `PROFILE_REPORTED` | no | — | none (no moderator alerting) |

**Consumers of Hub outbox events.** Messaging `services/messaging/src/communications.rs:310-364` and Calling `services/calling/src/communications.rs:315-360`:
- Both dedupe on `event_id` into `messaging_communication_inbox` / `calling_communication_inbox`.
- They act only when `data.principalId` and `data.credentialsNotBefore` are present: they insert a revocation; Messaging also revokes `messaging_client_sessions` and disables `messaging_push_registrations` with `ACCOUNT_SESSION_REVOKED`.
- Messaging also acts on `communications.timeline.changed` (`:345-347, 367`), which publishes the realtime `timeline.changed` to members.
- All `connection.*`, `block.changed`, `privacy.changed` and `profile.activated` events are **delivered and ignored**.

### 2.6 Hub outbox (durable fan-out)

| Aspect | Evidence |
|---|---|
| Table | `communication_outbox` (`001_foundation.sql:107-115`). Columns: `seq BIGSERIAL PK`, `event_id`, tenant/app/env, `destination IN ('CALLING','MESSAGING')`, `aggregate_id`, `aggregate_version`, `event_type VARCHAR(80)`, `payload JSONB`, `state IN (PENDING,INFLIGHT,DELIVERED,DEAD)`, `attempts`, `next_attempt_at`, `lease_id`, `lease_until`, `delivered_at`, `created_at`, `UNIQUE(event_id,destination)`. |
| Writers | `store.rs:219-280`: `event`, `scoped_event` (both destinations), `messaging_event` (MESSAGING only; for timeline nudges). |
| Dispatcher | `events.rs:421-466` `deliver_due`. 20 rows per tick, `FOR UPDATE SKIP LOCKED`, 90 s lease. POSTs `{schemaVersion:1,eventId,aggregateId,version,type,data}` to `/internal/v1/communications/events` on the owner, with a `DelegatedPlatformContext` rebuilt from `communication_scope_authorities`. |
| Retry | Backoff `2^min(attempts,10)` s. DEAD after 12 attempts (`:422, 462`). Operator replay at `POST /internal/v1/communications/replay` (`events.rs:396-419`). |
| Retention | DELIVERED rows purged after 30 days. DEAD rows kept (`events.rs:469-481`). |
| Inbound dedupe | `communication_inbox_dedupe(source,event_id,request_digest)` (`001_foundation.sql:98-101`). A same id with a different body returns `Error::Idempotency` (`events.rs:66-83`). |
| Schema version | Envelope `schemaVersion:1`. Event type strings are unversioned. |

### 2.7 Device-related events in the Hub

The device table `communication_devices` is defined in `002_unified.sql:87-113` and extended in `003_device_sessions.sql`. Columns:
- `platform IN (IOS,ANDROID,WEB,DESKTOP)`;
- `chat_device_ref` / `call_endpoint_ref` (unique each);
- `push_platform IN (APNS,FCM,WEB_PUSH,LOCAL_TEST)`, `push_registered_at`;
- `state ACTIVE|REVOKED` (terminal, enforced by trigger `:104-113`);
- `revocation_reason IN (USER_REQUESTED,COMPROMISED,OPERATOR,OWNER_SERVICE,ACCOUNT_SESSION_ENDED)`;
- `acknowledged_sync_position` (monotonic);
- `account_session_ref VARCHAR(64)` (immutable once set, hex-64).

| Event | Code | Effect | Notification |
|---|---|---|---|
| Link device | `devices.rs:207-312` | audit `DEVICE_LINKED`, feed DEVICE | none |
| Revoke device (user) | `devices.rs:379-455` | Owner calls first (Messaging `devices/revoke`, Calling `endpoints/revoke`), then audit `DEVICE_REVOKED`, feed DEVICE. A bound session reference is returned to the Account relay only (`bound_session`, `:31-47`), and Account ends that sign-in. | none from Hub. Account records security event `SESSION_REVOKED trigger DEVICE_REVOKED` (ADR 0057 A1 decision 2). |
| `ACCOUNT_SESSION_ENDED` from Account | `events.rs:200-245`, `devices.rs:49-110` `end_session_devices` | Revokes bound devices at owners; audit `ACCOUNT_SESSION_ENDED` | none |
| Owner-side revocation | `devices.rs:125-197` `reconcile_devices` (on `GET /devices` and on link); `internal.rs:96-104` (owner event, **no producer**) | `OWNER_SERVICE` revoke, feed DEVICE | none |
| Account status / revocation epoch | `events.rs:84-197` | Profile `credentials_not_before`, outbox `communications.account.changed` (with or without a Hub profile, ACC-010) | none |

### 2.8 Hub tests touching notifications and events

- `services/communications/tests/foundation.rs:985` `meeting_notification_opt_outs_are_listed_for_the_account_only`.
- `tests/unified.rs`, `private_owner_and_operator_routes_are_scoped_deduplicated_and_audited`, at line 1022 (it covers the account-switch, conversation-mute and group notification policy, lines 1149-1195).
- `tests/unified.rs`:
  - `:776` `push_status_follows_the_messaging_owner_contract`
  - `:911` `change_feed_is_monotonic_bounded_and_reports_resync_instead_of_gaps`
  - `:1218` `owner_side_revocations_reconcile_and_outages_change_nothing`
  - `:1576` `devices_are_bound_to_the_account_session_that_linked_them`
  - `:2252` `call_cards_reach_parties_without_a_profile_and_timeline_changes_are_nudged_once`
- `devices.rs:580-627` unit tests (`PushStatus::parse`); `preferences.rs:103-116` unit test; `policy.rs:360-…` `decide` tests.
- **Production acceptance:**
  - `infra/calling/production/communications_acceptance.py:208-223` check `preferences_truthful_delivery_status_and_monotonic_sync`. It does a GET, then a PUT of the same values, and verifies the provider map.
  - This passed in stage run 42180933332 and release run 42189631003 (22/22), and in `communications-verify` 42191576906 (20/20), all on 2026-11-06 (`docs/operations/communications-sections-9-23-delivery.md:3-16`). Later live acceptance: "Communications 20/20", `docs/implementation/cfmeet-backend-closure-report.md` §0 (2026-11-09).
  - **Category:** preference CRUD is **production-tested**. Enforcement is not.

### 2.9 Hub classification summary

| Capability | Status | Evidence |
|---|---|---|
| Account-wide category preferences CRUD | implemented, activated, production-tested (CRUD only) | §2.8 |
| Per-conversation mute and expiry | implemented, activated | Released 2026-11-06 (delivery doc) |
| Per-contact mute | implemented, activated | Sections 4–7 (REUSE_ACTIVE, `communications-sections-9-23-delivery.md:170`) |
| Enforcement for DM / group push | implemented, configured; **effectively dormant** (push off) | §2.3 |
| Enforcement for meeting email | implemented; **probably broken** (grant absent) | §1 item 3 |
| Enforcement for calls, missed calls, contact requests, group invitations | **absent** (stored only) | §2.3 |
| Quiet hours / DND / timezone / channel matrix | **absent** | §2.2.1 |
| In-app notification inbox (read/unread) | **absent**; `/activity` is a timeline, `/sync` is a change feed | §2.4 |
| Delivery diagnostics | implemented, production-tested | §2.4 |
| Push registration facade | implemented; providers NOT_CONFIGURED | `profile-notifications-sections-9-11.md` §2 "Push — EXTERNAL_BLOCKED" |
| Owner event ingress `/events/*` | implemented, **disabled** (no producer, no grant) | handoff §5; delivery doc "Owner events versus owner reads" |
| `block-events` | **deprecated** | `events.rs:244-251` |
| Phone verification | disabled (`PROVIDER_BLOCKED`) | `internal.rs` readiness |

---

## 3. `packages/communications-policy` (`cf-communications-policy`)

File: `packages/communications-policy/src/lib.rs` (260 lines).

- **`Scope`** (`:11-55`). `from_env` reads `CF_COMMUNICATIONS_PERSONAL_TENANT_ID`, `..._APPLICATION_ID` and `..._ENVIRONMENT_ID`, and requires `CF_COMMUNICATIONS_BASE_URL` with them. Only this one first-party personal triple is governed. Every other scope is "not governed", so callers skip policy, including notification preferences.
- **`Action`**, a closed enum (`:57-70`): `DIRECT_CALL`, `DIRECT_CONVERSATION`, `MESSAGE_SEND`, `MESSAGE_NOTIFICATION`, `GROUP_NOTIFICATION`, `CONTACT_REQUEST`, `GROUP_INVITATION`, `PERSONAL_ACTIVITY`, `PRESENCE`, `PROFILE_DISCOVERY`. There is no call-notification, missed-call, meeting or security action.
- **`PolicyCheck`** (`:72-86`): `action`, `actorPrincipalId`, `targetPrincipalId`, `credentialIssuedAt`, `credentialPrincipalId`, `conversationId` (optional, for conversation mute).
- **`Decision`** / `DecisionReason` (`:87-102`).
- **`PolicyClient`** (`:111-192`).
  - `check()` fails closed: a non-200 or bad schema returns `Unavailable`; disallowed returns `Denied`.
  - `resolve_principal()` returns 403 or 404 as `Denied`.
  - It signs as a new operation to `cfmeet.communications` with the caller's delegated context.
- **`VerifiedAccount`** (`:225-241`) carries the `accountSessionRef` (SHA-256 domain-separated). **`Dispatch`** (`:251-260`) is the relay envelope.
- **Users:** Calling (`services/calling/src/main.rs:468-471`, `communications.rs`) and Messaging (`state.rs:28`, `push.rs`, `membership`, `conversations`, `realtime/connection.rs:1040`). The Meeting Service uses the `VerifiedAccount` and `Dispatch` types.
- **Tests:** `lib.rs:194-223` (closed vocabulary, optional session ref).
- **Gap:** the policy contract has no notion of a channel, a quiet period or a notification category beyond the two message actions.

---

## 4. Organizations (`services/organizations`)

### 4.1 Tables relevant to notifications

The DDL is in `services/organizations/migration/src/m20261106_000001_organizations_foundation.rs`.

| Table | Lines | Key columns and constraints |
|---|---|---|
| `organization_invitations` | `:139-166` | `token_hash CHAR(64)` (unique; SHA-256 of a 256-bit base64url token), `email_normalized`, `email_hint`, `role IN (ADMIN,MANAGER,MEMBER,GUEST)`, `status IN (PENDING,ACCEPTED,DECLINED,REVOKED,EXPIRED)`, `invited_by`, `invitee_user_id`, `expires_at` default now()+7 days, `decided_at`, `version`. Unique pending per (org, email). |
| `organization_memberships` | `:85-110` | `role IN (OWNER,ADMIN,MANAGER,MEMBER,GUEST)`, `status IN (ACTIVE,SUSPENDED,REMOVED)`, `joined_via IN (CREATOR,INVITATION,IMPORTED,OPERATOR,OWNERSHIP_TRANSFER)`, `version`. |
| `organization_audit_events` | `:168-194` | Append-only (triggers). `actor_kind`, `actor_user_id`, `actor_service`, `action` (dotted), `target_type/id`, `summary` (at most 4 KiB, no `email`, `token`, `tokenHash`, `password`, `secret` or `phoneNumber` keys). |
| `organization_outbox` | `:196-226` (aggregate list extended in `m20261107_000002…:150-157` with `team_member`) | `seq`, `event_id`, `destination ~ '^cfmeet\.[a-z][a-z0-9-]*$'`, `organization_id`, `aggregate_type`, `aggregate_id`, `aggregate_version`, `event_type VARCHAR(64)`, `payload JSONB` (same forbidden keys), `state PENDING/INFLIGHT/DELIVERED/DEAD`, `attempts`, `next_attempt_at`, `lease_id`, `lease_until` (CHECK consistent with INFLIGHT), `delivered_at`, `last_error_class`, `created_at`. `UNIQUE(event_id,destination)`. |
| `organization_idempotency_records`, `organization_service_nonces` | `:228-249` | Swept by the worker. |

### 4.2 Event production

- **Writer:** `store::emit` (`services/organizations/src/store.rs:531-575`). It runs in the caller's transaction and writes the envelope:
  - `eventId`, `eventType`, `schemaVersion:1`, `organizationId`, `aggregateType`, `aggregateId`, `aggregateVersion`, `occurredAt`;
  - `actor{kind,userId?,serviceId}`, `correlationId`, `causationId`, `payload`.
- **Rows written:** one `cfmeet.reconciliation` row, never dispatched, plus one row per subscriber.
- **Event types emitted (grep of `services/organizations/src`):**
  - organization: `created`, `profile_updated`, `provisioning_changed`, `provisioned`, `suspended`, `reactivated`, `archived`;
  - membership: `activated`, `role_changed`, `suspended`, `removed`;
  - `ownership.transferred`;
  - invitation: `created`, `accepted`, `declined`, `revoked`, `expired` (via `invitations.rs:56-95` `emit_invitation` and `worker.rs:36-80`);
  - team: `created`, `updated`, `archived`, `member_added`, `member_role_changed`, `member_removed`;
  - `policy.updated`, `scope.mapping_changed`.
- **Contract:** `docs/events/organization-event-contracts.md` §1-3. The rules are at-least-once, dedupe on `eventId`, monotonic `aggregateVersion`, and no email or token in events.

### 4.3 Subscribers (who receives what today)

From `outbox.rs:41-59` `subscribers()`:

| Event types | Subscriber |
|---|---|
| `organization.{created,profile_updated,provisioned,suspended,reactivated,archived}`, `membership.{activated,role_changed,suspended,removed}`, `ownership.transferred`, `policy.updated`, `scope.mapping_changed` | `cfmeet.meeting-service` only |
| `invitation.*`, `team.*`, `organization.provisioning_changed` | **none** (reconciliation record only). Unit test `outbox.rs:361-380` pins this. |

**Contract vs implementation gaps:**
- The contract lists Communications, Calling, Messaging and Platform Access as consumers. None of them subscribe.
- The contract says the invitation consumer obligation is "Notification owner delivers transactional email (Section 11)". **No subscriber exists.** Email happens synchronously in the Meeting relay instead (§4.5).

### 4.4 Dispatcher, worker, retry and idempotency

| Aspect | Evidence |
|---|---|
| Worker | `services/organizations/src/worker.rs:83-175`. Runs inside the API process every `ORGANIZATIONS_WORKER_INTERVAL_MS`. Steps: sweep nonces, sweep idempotency records older than 24 h, expire invitations (200 per pass), deliver the outbox (50 per pass), and optionally provisioning. |
| Claim | `outbox::claim_due` (`outbox.rs:199-239`). Configured destinations only, never the reconciliation destination. **Per-aggregate ordering**: a row is not claimed while an earlier undelivered row of the same aggregate and destination exists. `FOR UPDATE SKIP LOCKED`, 90 s lease. |
| Settle and retry | `settle` (`:298-329`). Backoff `2^clamp(attempts,0,10)` s (at most 1024 s). DEAD on a permanent 4xx (`classify`, `:104-134`; 408, 429 and `RATE_LIMITED` are retryable) or after `MAX_ATTEMPTS=12`. An expired INFLIGHT lease with 12 or more attempts becomes DEAD with `ATTEMPTS_EXHAUSTED` (`:209-215`). |
| Replay | `replay_dead` (`:243-261`), exposed at `POST /internal/v1/admin/event-delivery/replay`. Status at `GET /internal/v1/admin/event-delivery` (`http/mod.rs:177-178`, `http/outbox.rs`). |
| Transport | `HttpEventDispatcher` POSTs `/internal/v1/organization-events`, signed, continuing the correlation id (`:136-161`). `DisabledDispatcher` when there are no destinations (`:163-178`). |
| Config | `ORGANIZATIONS_EVENT_DESTINATIONS` (`config.rs:256-275, 365-371`; only `cfmeet.meeting-service` with `CF_MEETING_SERVICE_BASE_URL`, `config.rs:81`). Default empty, which means disabled. |
| Retention | **No deletion of `organization_outbox` rows anywhere.** Reconciliation rows stay PENDING forever, so the table grows without bound. Production shows `PENDING 52` reconciliation rows (`CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:244`). |
| Consumer side | Meeting `services/meeting-service/src/services/organization_events.rs`. Gated by `ORGANIZATION_EVENTS_ENABLED`, permission `meeting-organization-events.apply`. Dedupe in `organization_event_inbox`; fences in `organization_event_fences`. Effects (`:186-206`): membership events go to `organization_member_access` (offboarding worker); org, ownership, policy and scope events refresh the projection. **No user notification.** |
| Activation | **activated.** `activate-org-events` run 42710192324 (2026-11-09/10; `docs/operations/CFMEET_BACKEND_FINAL_CLOSURE.md:60`, `cfmeet-backend-closure-report.md` §0). `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` (`CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md:250`). Outbox DEAD 0, DELIVERED 31 (`CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:244`, 2026-11-13). Certify 43176010902, "Organization event delivery … ACTIVE" (`CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:132`). |
| Tests | `services/organizations/tests/worker.rs`:<br>`:55` `the_maintenance_pass_expires_invitations_and_sweeps_nonces_and_idempotency`<br>`:109` `outbox_leases_are_exclusive_and_failures_back_off`<br>`:214` `an_enabled_dispatcher_delivers_each_pending_event`<br>`:277` `a_consumer_receives_each_aggregate_in_version_order_and_replay_resumes_after_a_dead_letter`<br>`:445` `readiness_reports_delivery_state_and_excludes_reconciliation_records`<br>Also `outbox.rs:348-408` unit tests. |

### 4.5 Invitations: how they are delivered

1. **Organizations creates the invitation** (`services/organizations/src/http/invitations.rs:146-265`).
   - It returns the raw token once (`doc["token"]`, `:254`) and stores only the SHA-256.
   - An idempotent replay rotates the token (`:170-190`).
   - It emits `invitation.created`, which has no subscriber.
   - Organizations sends **no email**.
2. **The Meeting Service relay sends the email** (`services/meeting-service/src/handlers/v2/organizations.rs:936-1023`, `POST /api/v2/organizations/{id}/invitations`, operationId `v2_create_organization_invitation`).
   - It strips `sendEmail` and forwards the signed call.
   - Then, if `sendEmail && email_enabled`, it calls `notifications::send_organization_invitation` (`services/meeting-service/src/services/notifications.rs:540-625`). The link is `{APP_URL}/organizations/invitations#<token>` (fragment). The category is `ORGANIZATION_INVITATION`.
   - It is rate-limited by `NOTIFICATIONS_INVITE_EMAILS_PER_HOUR`.
   - **No Hub preference applies** (transactional, `notifications.rs:34-35`).
3. **Invitation by CF Meet ID.** The relay resolves the ID through the Hub's `POST /internal/v1/communications/organization-invitations/resolve` (`services/communications/src/api.rs:115-160`), applying discovery rules, block and visibility checks. The email address is never returned to the inviter.
4. **Accept or decline** requires the token (`POST /internal/v1/organization-invitations/accept|decline`). Accept also requires `X-CF-Acting-User-Verified-Email` equal to the invited address (`invitations.rs:395-505`).
5. **There is no "my pending invitations" endpoint** (route list in `http/mod.rs:58-183`). An invitee with an account learns of an invitation only by email or by an out-of-band link. There is no in-app notice.
6. **The inviter receives the raw token in the API response** (to share the link). Any inviter-side log or client storage therefore holds a bearer invitation secret. This is a security note.
7. **Status:** invitation email is implemented and gated (`IMPLEMENTED_PENDING_ACTIVATION` in `CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md:313`, 2026-11-12). Later Meeting runs `ORGANIZATIONS_AUTHORITY_MODE=authoritative` with `NOTIFICATIONS_EMAIL_ENABLED=true` (`CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:76`, 2026-11-13). I found no production run that specifically shows an organization invitation email being delivered. That makes it **activated (by configuration), not production-tested by evidence found**.

### 4.6 Membership, role, suspension and ownership changes

| Change | Code | Event | Notified? |
|---|---|---|---|
| Role or status update | `http/members.rs:96-270` (`change_membership`, `update`) | `membership.role_changed` / `.suspended` / `.removed` (+ audit) | **no**. Meeting updates access only. |
| Leave | `members.rs:273-328` | `membership.removed` (`membership.left` audit) | no |
| Ownership transfer | `members.rs:330-…` (emit at `:392`) | `ownership.transferred` | no |
| Organization suspend / reactivate / archive | `http/organizations.rs` | `organization.*` | no |
| Team membership | `http/teams.rs` | `team.*`, no subscriber | no |
| Invitation expiry | `worker.rs:36-80` | `invitation.expired`, no subscriber | no (inviter is not told) |

---

## 5. Platform Access (`services/platform-access`)

### 5.1 What it owns, and what it does not

**Owns:**
- tenants, applications, environments, service accounts, API credentials (with scopes, suspension and principal restriction), principals, organization scope bindings;
- the identity change feed;
- the access audit log;
- the delegation API (`http/delegation.rs`) and admin API (`http/admin.rs`).

Migrations (`services/platform-access/migration/src/`):
- `m20261016_000001_identity_hierarchy`
- `…000002_identity_change_feed`
- `…000003_access_operations`
- `m20261120_000004_credential_suspension_production_activation`
- `m20261121_000005_first_party_organization_scopes`
- `m20261122_000006_credential_usage_tenant_rename`
- `m20261123_000007_organization_service_bindings`

**Does not own:**
- human sessions, typed access/refresh tokens, the account revocation epoch, linked devices, device approval, primary-mobile replacement, sign-out-everywhere;
- user notifications. A grep for `notif`, `email`, `webhook` and `outbox` in `services/platform-access/src` hits only an unrelated mention in `identity/scopes.rs`.

All the "does not own" items live in the **Account authority (Meeting Service)**, per ADR 0057 and ADR 0060. Tables there include `account_sessions`, `account_security_state`, `account_security_events`, `account_devices`, `account_device_links`, `account_device_proof_nonces`, `account_session_media`, `account_communication_outbox` (`services/meeting-service/migration/src/m20261106_000001_account_security.rs`, `m20261115_000001_account_devices.rs`).

### 5.2 Platform Access tables relevant to revocation and audit

| Table | Evidence | Notes |
|---|---|---|
| `platform_principals` | `m20261016_000001…:361-437` | `kind` is one of 7 values (`NATIVE_CF_USER`, …). `status IN (ACTIVE,SUSPENDED,DEACTIVATED)`. `tombstoned_at`. `revision` bumped by trigger. |
| `platform_identity_changes` | `m20261016_000002…:25-97` | Append-only. `sequence BIGSERIAL`, `entity_kind` (8 kinds), `operation`, `entity_key`, `tenant_id`, `revision`, `snapshot` (no verifier keys allowed). Filled by AFTER triggers on every identity table. |
| `platform_access_audit` | `m20261016_000003…:22-62` | Append-only. A closed `action` list: `TENANT_*`, `APPLICATION_*`, `ENVIRONMENT_*`, `SERVICE_ACCOUNT_*`, `CREDENTIAL_CREATED/ROTATED/REVOKED/PRINCIPALS_RESTRICTED`, `SCOPE_*`, `PRINCIPAL_REGISTERED/STATUS_SET/TOMBSTONED`, `IDENTITY_IMPORTED`. Detail is at most 2 KiB with no secret keys. **Audit only, no notification.** |

### 5.3 Internal API (`services/platform-access/src/http/internal.rs:1-80`)

| Route | Permission |
|---|---|
| `POST /internal/v1/credentials/authenticate` | `platform-credential.authenticate` |
| `GET /internal/v1/credentials/{id}` | `platform-identity.read` |
| `POST /internal/v1/authorization/evaluate` | `platform-identity.read` |
| `GET /internal/v1/identity/context` | `platform-identity.read`. The Hub calls this on every scoped request (`auth::validate_scope`). |
| `POST /internal/v1/identity/changes` | `platform-identity.read`. Pageable feed (`identity/changes.rs`, at most 500 per page). |
| `GET /internal/v1/scopes` | `platform-identity.read` |
| `POST /internal/v1/principals/resolve` | `platform-principal.resolve` |
| `POST /internal/v1/principals/ensure` | `platform-principal.ensure` |
| `POST /internal/v1/principals/account-status` | `platform-principal.ensure` (ACC-010 mirror, `:840-900`) |
| `POST /internal/v1/principals/{id}/tombstone` | `platform-principal.register` |

### 5.4 How other services learn about revocation (complete map)

| Mechanism | Type | Producer → consumer | Evidence |
|---|---|---|---|
| Account session row and epoch check on every request | Synchronous check in the Account DB | Meeting verifiers (`account_security::authorize_user_token`) | ADR 0057 decisions 2-3 |
| `account_communication_outbox` → Hub `account-events` (`STATUS_CHANGED`, `SESSION_REVOKED`, `ACCOUNT_SESSION_ENDED`) | **Push** (durable outbox, retried) | Meeting → Hub | `services/meeting-service/src/services/communications.rs:936-960`; Hub `events.rs:84-245` |
| Hub `communications.account.changed {principalId, credentialsNotBefore}` | **Push** (Hub outbox) | Hub → Calling, Messaging | `events.rs:150-189`; consumers `messaging/src/communications.rs:348-358`, `calling/src/communications.rs:352-359` |
| Messaging `client-sessions/revoke-all` for the bridge principal | Push (Account outbox) | Meeting → Messaging | `communications-hub-section-14.md` ACC-010 row |
| Platform Access principal status mirror | Push (Account outbox worker → PA) | Meeting → Platform Access | `internal.rs:840-900`; test `tests/principal_account_status.rs:25` |
| Identity change feed | **Polling** | PA → Messaging (`identity/cache.rs:1-21`: any change flushes the cache; a failed poll disables caching), Calling (`platform/access.rs:446-457`) | |
| Fresh credential authentication / identity context | **Introspection-style** per request | PA ← Hub, Messaging, Calling, API Gateway | `internal.rs` |
| `policy/check` with `credentialIssuedAt` | Introspection | Hub ← Calling, Messaging | `policy.rs:132-146` |
| Hub device ↔ session binding | Push (`ACCOUNT_SESSION_ENDED`) and synchronous (device revoke ends session) | Meeting ↔ Hub | ADR 0057 A1; `devices.rs:31-110` |

**Residual window:** a Calling signaling credential stays valid up to its 600 s lifetime until the event is applied (ADR 0057 A1, "Remaining window").

### 5.5 Linked devices and E2EE (ADR 0060): notifications

- **Activated.** `docs/operations/CFMEET_MULTI_DEVICE_E2EE_FINAL_REPORT.md` (2026-11-15) verdict: `MULTI_DEVICE_E2EE_ACTIVATED — PRODUCTION_VERIFIED; INDEPENDENT_CRYPTOGRAPHIC_CERTIFICATION_NOT_CLAIMED`.
- **User notifications that exist (Account, email, not via the Hub):**
  - Device link completed: either "A new phone is now your primary CF Meet mobile" or "A new browser was linked to your CF Meet account" (`services/meeting-service/src/services/account_devices.rs:1724-1743`, `notify_owner` `:769-777`). It uses `EmailService::send_notification_email` (category `NOTIFICATION`, `services/email.rs:171-189`). It is **not gated** by `NOTIFICATIONS_EMAIL_ENABLED` or by Hub preferences. A failure is logged only (fire-and-forget after commit).
  - Email step-up code for a device link (`account_devices.rs:1494-1565`).
  - Password changed (`handlers/auth.rs:878, 1020`; category `SECURITY_PASSWORD_CHANGED`).
- **In-app:** `GET /api/v2/me/security-events` (`handlers/v2/account_devices.rs:409`, operationId `v2_list_my_security_events`) lists security events from `account_security_events`. This is a pull-only security activity feed, and the only in-app "activity" surface for account security.
- **No notification on these events:**
  - device revocation;
  - primary-mobile replacement for the *old* phone (it is signed out; the notice goes to the account email for the new device only);
  - sign-out-everywhere (`/api/v2/me/sessions/revoke-all`);
  - session revoke;
  - suspension or deactivation;
  - failed sign-ins;
  - new sign-in alerts (explicitly "Not created", `CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5);
  - email change.
- **Mobile push:** not configured ("Mobile push (APNs, FCM, Web Push): provider credentials are not configured", `CFMEET_MULTI_DEVICE_E2EE_FINAL_REPORT.md:175`).

### 5.6 Platform Access tests relevant to this report

- `services/platform-access/tests/change_feed.rs`: `:25` `every_identity_mutation_is_published_without_verifiers`, `:91` `updates_carry_increasing_revisions_and_pages_resume_from_the_cursor`, `:131` `a_rolled_back_mutation_publishes_nothing`.
- `tests/principal_account_status.rs`: `:25` `account_status_is_mirrored_idempotently_and_reversibly`, `:134` `another_scope_never_mirrors_this_scopes_principal`.

---

## 6. Service-to-service authentication (`packages/service-auth`, ADR 0004, SEC-012)

### 6.1 Token and transport (exact rules)

Sources: `docs/decisions/0004-internal-service-authentication.md` §2-8 and `packages/service-auth/README.md`.

- **Token:** EdDSA (Ed25519) compact JWS.
  - Header exactly `{"alg":"EdDSA","typ":"cf-service+jwt","kid"}`.
  - Closed claim set: `ver=1`, `iss`, `aud` (**exactly one** service), `runtime_environment` (`local|test|staging|production`), `iat`, `nbf`, `exp`, `jti` (128-bit), `request_id`, `correlation_id`, optional `delegated_context`, optional `bnd`/`htm`/`htu`.
  - TTL default 120 s, configurable 10–300 s, hard maximum 300 s, 30 s skew. Tokens over 4 KiB are refused. The Hub signs with 60 s (`config.rs:39`).
- **Headers:**
  - `X-CF-Service-Authorization: Bearer <jws>`;
  - `X-CF-Request-Id: req_<32hex>` (new per hop);
  - `X-CF-Correlation-Id: corr_<32hex>`.
  - The id headers must equal the signed claims. A request carrying a customer `Authorization` header as well is rejected.
- **Tokens carry no permissions.** Permissions are **receiver-authoritative**: a trust bundle per receiver and runtime environment lists issuers, up to 4 Ed25519 keys each, and the permissions granted (`packages/service-auth/src/trust.rs`, `MAX_KEYS_PER_ISSUER=4`, `MAX_ISSUERS=64`). Startup refuses wildcards, self-trust, unknown permissions, and permissions not grantable to the issuer (`trust.rs:326`).
- **Permission registry:** a closed `InternalPermission` enum plus `REGISTRY` of `PermissionDefinition{permission,name,receiver,grantable_to,sensitive,description}` (`packages/service-auth/src/permissions.rs:370-…`). `grantable_to` is a fixed list per permission. Examples:
  - `communications-preferences.read`: receiver Communications, grantable to Meeting Service only (`:397`);
  - `communications-policy.check`: grantable to Calling and Messaging (`:382`);
  - `communications-events.publish`: grantable to Calling, Messaging and Meeting Service (`:392`).
- **Service identities:** `ServiceId::parse` accepts any `cfmeet.<kebab>` name of at most 64 characters (`identity.rs:63-90`). Named constants exist for 14 services (`identity.rs:10-46`). There is no `cfmeet.notifications`.
- **Delegated context:** `DelegatedPlatformContext{tenant_id, application_id, application_environment_id, application_environment_kind, initiating_service_account_id, initiating_principal?, initiating_credential_id?}` (`delegated.rs:64-72`). It is signed inside the token. Receivers such as the Hub re-validate it against Platform Access (`GET /internal/v1/identity/context`) before touching scoped data.
- **Request binding (SEC-012)** (`docs/operations/service-token-request-binding.md`):
  - Claims `bnd=1`, `htm`, `htu` (canonical path; the query is not bound).
  - Receiver `SERVICE_AUTH_REQUEST_BINDING_MODE=off|accept|require` (default `accept`). Signer `SERVICE_AUTH_REQUEST_BINDING=off|emit` (default `off`).
  - Replay guard `SERVICE_AUTH_REPLAY_GUARD=on` (default) for POST, PUT, PATCH and DELETE. In-memory 100k entries, or Redis in Messaging.
  - **No release tool or ops doc sets `emit` or `require`.** The grep of `infra/` and `docs/operations` found only the runbook, so production is presumably at step 1 (`accept`, signers `off`). Classify as **implemented; step 1 of the rollout only; not verified**.
- **Durable nonces:** the Hub (`communication_service_nonces`, `auth.rs:80-90`, every method) and Organizations (`organization_service_nonces`) record every token id.
- **Client:** `InternalServiceClient` (feature `client`). Targets come from config by audience only, paths only under `/internal/v1/`, HTTPS outside local and test, no redirects, no proxies, bounded timeout. It signs each call afresh.

### 6.2 What a new internal Notification Service would need (derived from the code)

1. **Identity.** Choose `cfmeet.notifications` (or similar). Optionally add a constant to `packages/service-auth/src/identity.rs`. Generate one Ed25519 key per runtime environment (`cf-service-keygen generate --kid … --out …`). Configure:
   - `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`, `CF_SERVICE_TRUST_BUNDLE_FILE`;
   - peer base URLs.
2. **Outbound to existing services** (for example reading Hub preferences, Account profiles, Messaging push registrations, or Platform Access identity):
   - add new `InternalPermission` variants and `REGISTRY` rows whose `grantable_to` includes the new service, or extend existing rows;
   - update `packages/contracts/platform/common/v1.schema.json` (permission enum, verified by `tests/contract_conformance.rs`);
   - add the issuer entry with the public JWK and permissions to each receiver's trust bundle;
   - for production, add the grant to `infra/calling/production/coordinated_release.py` `GRANTS` (`:534-…`, which is checked against `permissions.rs` by `test_coordinated_release.py`). The Hub's bundle is generated by `communications_release.py:168-193`, from scratch, so it must also be added there or it is wiped.
3. **Hard-coded caller checks to change:**
   - Hub `preferences.rs:90` (`!= meeting_service`);
   - `api.rs:128, 174` (Meeting only);
   - `api.rs:315-322` (notification actions Messaging only);
   - `api.rs:246-264` (`principals/resolve` purpose by caller);
   - `internal.rs:62-71` (owner event families by caller).
4. **Delegated scope.** Hub routes require `delegated_platform_context` (`auth::delegated`). The service must sign with a valid `(tenant, application, environment, service account)` chain that Platform Access accepts. For personal scope, that is the `CF_COMMUNICATIONS_PERSONAL_*` triple and a service account registered in `communication_scope_authorities`.
5. **Inbound to the new service.** Each producer gets a new permission (receiver = notifications, for example `notifications-event.publish`) in `REGISTRY`, plus trust-bundle grants. The new service then verifies with `ServiceTokenVerifier::verify_request(method, original_path, headers)` and `require_permission`, and adds a durable `jti` table if it is multi-instance (as the Hub and Organizations do).
6. **Organizations events.** Add the new destination to `outbox::subscribers()` for the desired event types (for example `invitation.*`, `membership.*`). This is a code change. Then add it to `ORGANIZATIONS_EVENT_DESTINATIONS`, add a base URL entry (`config.rs:81` currently allows only the Meeting Service), and implement `POST /internal/v1/organization-events`.

---

## 7. Cross-cutting gaps, duplication and security concerns

### 7.1 Gaps

| # | Gap | Evidence |
|---|---|---|
| G1 | 4 of 7 Hub categories have no enforcement point and no delivery channel. | §2.3 |
| G2 | No channel matrix, quiet hours, DND, timezone, digest or locale in preferences. | §2.2.1 |
| G3 | No in-app notification inbox with read/unread anywhere in scope. `/activity` covers calls and meetings only; `/sync` is a cache-invalidation feed; `/me/security-events` (Account) is security only. | §2.4, §5.5 |
| G4 | Contact requests and accepted connections produce no notification and no realtime nudge; the client must poll. | §2.5 |
| G5 | Organization invitations: no subscriber for `invitation.*`, no "my invitations" endpoint, email only on explicit `sendEmail`. Membership, role, suspension and ownership changes are silent to users. | §4.3-4.6 |
| G6 | Organization and enterprise Messaging pushes have no preference check (scope not governed). | `push.rs:340-350` |
| G7 | Calls: `incoming_calls` and `missed_calls` stored, but Calling has `NoPushTransport` and no Hub check. | `calling/src/main.rs:453` |
| G8 | Security notices: only new device and password changed. Nothing for revocation, sign-out-everywhere, suspension or new sign-in. | §5.5 |
| G9 | The Hub owner-event ingress has no producers and no grants. Convergence is by polling (5 s sweep). | handoff §5; `main.rs:41-42` |
| G10 | Organizations outbox has no retention. Reconciliation rows stay PENDING forever. | §4.4 |
| G11 | Internal contract drift: Hub `/events/*`, admin, readiness and reconciliation routes are not in `internal/v1.openapi.json`. | §2.4 |
| G12 | Org event contract promises consumers (Hub, Calling, Messaging, Platform Access, notification owner) that are not implemented. | `organization-event-contracts.md` header vs `outbox.rs:41-59` |
| G13 | No preference UI in `apps/web` or `apps/mobile`. | §2.3 |

### 7.2 Duplication

- **Three overlapping mute layers with different semantics:**
  - contact mute (`communication_contact_aliases.notifications`, no expiry);
  - conversation mute (`communication_conversation_preferences`, with `muted_until`);
  - account category booleans.
  The `MENTIONS` value appears in two of them, but no layer can enforce it: the server cannot see message content, so `MENTIONS` behaves as `MUTED`.
- **Three outbox implementations with the same pattern** (lease, `SKIP LOCKED`, `2^n` backoff, DEAD at 12 attempts, replay):
  - Hub `communication_outbox`;
  - Organizations `organization_outbox`;
  - Meeting `account_communication_outbox`.
  Messaging has its own `messaging_outbox_events` / `messaging_push_jobs`, and Meeting has `meeting_notifications`.
- **Inbound-dedupe tables in every consumer:** Hub `communication_inbox_dedupe`, `messaging_communication_inbox`, `calling_communication_inbox`, Meeting `organization_event_inbox`.
- **Several revocation epochs:**
  - Account `account_security_state.credentials_not_before`;
  - Hub `communication_profiles.credentials_not_before`;
  - `messaging_communication_revocations`;
  - `calling_communication_revocations`;
  - Platform Access principal status.
- **Two device-notification paths:** the Hub device index (no notices), and Account `account_devices` (email notices).

### 7.3 Security observations

- **S1.** The Hub preference resolve returns only opted-out references for references the caller supplies. It does not enumerate accounts, and preference values and principals never leave the Hub. Good. But `notification_opt_outs` builds SQL with `format!("… n.{column} …")` (`preferences.rs:64-75`). This is safe today because `column` comes from a closed match (`:43-46`). Any future category added by string would be an injection risk.
- **S2.** **Fail-closed by design, but with the grant missing it fails closed in production** (§1 item 3). This is an availability issue, not a confidentiality one.
- **S3.** The organization invitation raw token is returned to the inviter's client in the API response, and an idempotent replay re-issues it (`invitations.rs:170-190, 254`). It is a bearer secret, although acceptance is bound to the verified email.
- **S4.** Device notices are fire-and-forget after commit (`account_devices.rs:769-777`). A queue failure loses the security notice silently, apart from a log line, and it has no retry ledger key (idempotency `None`, `email.rs:181-182`).
- **S5.** Request binding has not progressed past `accept` in any recorded rollout. Replay protection depends on the in-memory guard, except in the Hub, Organizations and Messaging-with-Redis.
- **S6.** The push token passes through the Hub to Messaging and is never stored there (`devices.rs:339-343`). Good. `/notification-delivery-status` returns per-device push state from Messaging verbatim (devices array). Make sure no handles leak, which depends on the Messaging contract.
- **S7.** Per-recipient `policy/check` fans out to about 6 signed calls. A central notification service calling it per message per recipient could cause a denial of service on Account and Platform Access. It needs batching or caching.

---

## 8. Dependency map (notification-relevant)

```
Client ─(Account bearer)→ Meeting Service relay /api/v2/communications/* ─(communications-user.forward)→ Hub /dispatch
Meeting notifier ─(communications-preferences.read [grant absent in tooling])→ Hub /notification-preferences/resolve
Meeting notifier → email_deliveries → Resend/SMTP           (NOTIFICATIONS_EMAIL_ENABLED=true, run 42730700547)
Messaging push worker ─(communications-policy.check)→ Hub /policy/check ─→ Platform Access (identity/context, principals/resolve)
                                                                     ├→ Meeting (accounts/communications/status ×2)
                                                                     └→ Calling, Messaging (blocks/check)
Messaging push adapters (APNs/FCM/WebPush)                  (MESSAGING_PUSH_PROVIDER=none → NOT_CONFIGURED)
Meeting account outbox ─(communications-account.apply)→ Hub /account-events → Hub outbox → Calling/Messaging /communications/events
Meeting account outbox ─(platform-principal.ensure)→ Platform Access /principals/account-status
Organizations outbox ─(meeting-organization-events.apply)→ Meeting /organization-events     (activated, run 42710192324)
Calling/Messaging ─poll→ Platform Access /identity/changes
```

## 9. Key file index

- Hub:
  - `services/communications/src/preferences.rs`
  - `src/chat.rs` (lines 20-151, 354-523)
  - `src/policy.rs` (88-219, 335-359)
  - `src/api.rs` (21-100, 309-330, 423-680)
  - `src/devices.rs` (31-110, 315-455, 511-572)
  - `src/events.rs` (84-251, 421-517)
  - `src/internal.rs` (55-125)
  - `src/sync.rs`
  - `src/relationships.rs`
  - `src/store.rs` (219-332)
  - `src/auth.rs` (16-118)
  - `src/config.rs`
  - `src/main.rs`
  - `migration/sql/001_foundation.sql`, `002_unified.sql`, `003_device_sessions.sql`, `004_timeline_realtime.sql`
  - `tests/foundation.rs:985`, `tests/unified.rs`
- Policy crate: `packages/communications-policy/src/lib.rs`
- Organizations:
  - `services/organizations/src/outbox.rs`, `src/worker.rs`, `src/store.rs:531-600`
  - `src/http/invitations.rs`, `src/http/members.rs`, `src/http/mod.rs`, `src/config.rs:81, 256-371`
  - `migration/src/m20261106_000001_organizations_foundation.rs`
  - `tests/worker.rs`
- Platform Access:
  - `services/platform-access/src/http/internal.rs`, `src/identity/changes.rs`
  - `migration/src/m20261016_00000{1,2,3}_*.rs`
  - `tests/change_feed.rs`, `tests/principal_account_status.rs`
- Service auth:
  - `packages/service-auth/src/{permissions.rs,trust.rs,identity.rs,delegated.rs,verifier.rs,binding.rs,replay.rs,client.rs}`
  - `README.md`
- Consumers and senders outside scope (cited):
  - `services/meeting-service/src/services/notifications.rs`, `src/handlers/v2/organizations.rs:903-1023`
  - `src/services/organization_events.rs`, `src/services/account_devices.rs:769, 1724-1743`, `src/main.rs:193-203`
  - `services/messaging/src/realtime/push.rs:319-396`, `src/communications.rs:310-364`
  - `services/calling/src/communications.rs:315-360`
- Docs:
  - `docs/api/communications-unified-handoff.md` (§3-6)
  - `docs/operations/profile-notifications-sections-9-11.md`
  - `docs/operations/communications-sections-9-23-delivery.md`
  - `docs/operations/communications-hub-section-14.md`
  - `docs/architecture/communications-device-and-block-authority.md`
  - `docs/events/organization-event-contracts.md`
  - `docs/api/organizations-internal-v1.md`
  - `docs/operations/service-token-request-binding.md`
  - `docs/decisions/0004-*`, `0057-*`, `0060-*`
- Release tooling: `infra/calling/production/communications_release.py:165-193`, `coordinated_release.py:530-560`, `communications_acceptance.py:208-223`
