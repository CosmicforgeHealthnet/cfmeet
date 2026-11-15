# Document 2 — Existing Notification Inventory

*In this document "NS" means the proposed unified notification service. Revision 2 (owner decision D-0) updates the migration-action columns.*

This is a service-by-service inventory of notification producers, consumers, APIs, tables, workers, events, feature flags and production states. It reflects `main` at `6f3743f` (2026-11-15).

## Status vocabulary

These definitions follow Part 3.1 of the brief.

| Label | Meaning |
|---|---|
| **implemented** | Code on `main` is wired into a reachable path. |
| **configured** | Env/flags are wired and set in some environment. |
| **activated** | The flag or transport is ON in production. The evidence column cites the run or certify output. |
| **production-tested** | A dated production run exercised the behaviour. |
| **disabled** | Gated off. |
| **broken** | Evidence says it cannot work as deployed. |
| **deprecated** | Superseded or sunset. |
| **absent** | No code path exists. |

Production flag values come from certify run 43670057279 (`evidence/07-…summary.txt`) unless another run is named. Line numbers point to the evidence files, which carry full citations. Production row counts were **not measured**: the audit had no authorized database access. Each table lists the read-only query that measures it in Phase 0 (Document 5 §2).

---

## A. Service-by-service

### A.1 Meeting Service (also hosts the **Account** authority)

| Responsibility | Location | Produces | Consumes | Status |
|---|---|---|---|---|
| Unified email ledger, worker and providers | `src/services/email_delivery.rs` (`enqueue` L124-185, `EmailWorker` L599-875, `ResendProvider` L270-391, `SmtpProvider` L394-486) | provider sends; ledger rows | business enqueue calls | implemented; **activated on SMTP** (`EMAIL_PROVIDER=smtp`, certify 43670057279); Resend **not activated**; delivery **not production-evidenced** (diagnose 43317264046: 0 sends / 168 h) |
| Resend delivery webhooks | `handlers/email.rs::resend_events` L50-103; `verify_svix` L941-983 | status upgrades | Svix-signed provider events | implemented; route returns 404 until a webhook secret is set (not set in production) |
| Account emails (verification, reset, password changed) | `handlers/auth.rs` L78-163, L748-899, L967-1036; `services/email.rs` | `AUTH_VERIFICATION`, `AUTH_PASSWORD_RESET`, `SECURITY_PASSWORD_CHANGED` | user actions | implemented; transport **broken in production per ops report** (`CFMEET_FINAL_PRODUCTION_COMPLETION.md:121`) |
| Device step-up code, new-browser / new-primary-phone notices | `services/account_devices.rs` L1494-1571 (transactional), L1724-1743 (`notify_owner`, best-effort) | `SECURITY_DEVICE_STEP_UP` (literal), `NOTIFICATION` | device linking | implemented; flows exercised in production run 43666265601 (2026-11-15); email delivery not evidenced |
| Meeting invitation email (synchronous) | `handlers/v2/invitations.rs` L38-104 → `notifications.rs` L429-536 | `MEETING_INVITATION` | `sendEmail=true` requests | implemented; activated (`NOTIFICATIONS_EMAIL_ENABLED=true`); **likely broken for account holders** (missing Hub grant) |
| Organization invitation email (synchronous) | `handlers/v2/organizations.rs` L935-1033 → `notifications.rs` L540-626 | `ORGANIZATION_INVITATION` | relay of the Organizations response | implemented; activated; not production-tested; no preference check (transactional) |
| Durable meeting notices: reschedule, cancel, reminder | `notifications.rs` (`record_rescheduled` L660-688, `record_cancelled` L692-714, `enqueue_due_reminders` L718-755, `deliver` L916-1071) | `MEETING_NOTIFICATION` | conference service, in the same transaction | implemented; activated; **likely broken for account holders** |
| Developer production-access decision email | `handlers/email.rs::developer_access_decision` L195-260 | `DEVELOPER_PLATFORM` | ops script `infra/calling/production/developer_access.py:150-165` | implemented; **not activated** (`DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false`) |
| Per-conference realtime (waiting room, start, host/participant updates) | `handlers/v2/realtime.rs`, `services/realtime.rs`, `workers/outbox_relay.rs` | WS events | conference outbox | implemented; activated (`FEATURE_CONFERENCE_REALTIME_ENABLED=true`). This is in-meeting only, not a per-user notification channel |
| Account → Hub lifecycle events | `services/communications.rs` L470, L913, L936-975; table `account_communication_outbox` | `ACCOUNT_SESSION_ENDED`, `SESSION_REVOKED`, `STATUS_CHANGED` | — | implemented; activated. No max-attempts / dead letter |
| Platform public events (meeting, participant, recording) | `services/platform/events.rs` | Developer Platform webhooks | outbox `PUBLIC_EVENT` | implemented; activated (`MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION=developer-platform`) |
| Legacy customer webhooks `/api/v1/webhooks*` | `handlers/webhooks.rs` | none (never delivered) | — | **deprecated** (Sunset 2027-05-01) |
| Welcome email | `services/email.rs::send_welcome_email` L127 | — | — | implemented, **no caller** (dead code) |

**Workers** (`src/main.rs`):

| Worker | Wiring | Gate / cadence / safety |
|---|---|---|
| Email worker | L251-270 | Provider ≠ none. 1 s idle poll, batch 10, 2 msg/s per process, 90 s lease, `SKIP LOCKED`, no leader election |
| Notification worker | L274-288 | `NOTIFICATIONS_EMAIL_ENABLED`. 30 s, batch 20, 120 s lease |
| Conference scheduler | L308-310 | `FEATURE_MEETING_V2_ENABLED`. Does **not** create reminders |
| Account → Hub event loop | L218-230 | 500 ms |
| Outbox relay | L327-344 | 2 s |
| Platform event publisher | — | — |
| Legacy Redis email import | L240-250 | Once at boot |

**Feature flags** (names and defaults):
- `EMAIL_PROVIDER` (unset → smtp if `SMTP_HOST` is set, else none)
- `NOTIFICATIONS_EMAIL_ENABLED` (false; production **true**)
- `NOTIFICATIONS_REMINDER_LEAD_MINUTES` (15)
- `NOTIFICATIONS_MAX_RECIPIENTS` (200)
- `NOTIFICATIONS_INVITE_EMAILS_PER_HOUR` (50)
- `EMAIL_MAX_ATTEMPTS` (8)
- `EMAIL_MAX_PER_SECOND` (2)
- `EMAIL_LEDGER_RETENTION_DAYS` (30)
- `ACCOUNT_DEVICE_EMAIL_STEP_UP` (true)
- `INTERNAL_API_TOKEN` (unset → internal email ops return 404)

Full table: evidence/01 §1.8.

### A.2 Messaging

| Responsibility | Location | Status |
|---|---|---|
| Push job fan-out from the outbox (`messaging.message.created`, kind `APPLICATION`, `notify != false`) | `src/realtime/fanout.rs:125-134`, `push.rs:240-283` | implemented; runs in production; jobs end `NOT_CONFIGURED` |
| Leased push dispatcher with dispatch-time reauthorization (device active, membership, history window, DIRECT block, Hub policy for personal scope) | `push.rs:319-534`; spawned `main.rs:408-411` | implemented; dormant (no provider) |
| APNs (`.p8`, HTTP/2), FCM HTTP v1, Web Push (RFC 8291/8292, VAPID) | `push_adapters.rs:132-683` | implemented; **not configured** (`MESSAGING_PUSH_PROVIDER=none`); tested only against fakes |
| VAPID public key route | `GET /v1/messaging/push/configuration` (`push.rs:66-81`), commit `42b509c` | implemented; `activate-web-push` has **no recorded run** (absent from the certify journal) |
| Push registration | `PUT/DELETE /v1/messaging/devices/{deviceId}/push-registration` | implemented; **no client calls it** |
| Realtime gateway (WS `/v1/realtime`, inbox events, receipts, typing, presence) | `src/realtime/*` | implemented; activated (readiness: broker redis, clientTokens enabled) |
| Read/delivery cursors, unread counts | `messages/receipts.rs`; `GET /v1/conversations/{id}/unread` | implemented; activated |
| Message requests (INVITED members) | `conversations/service.rs:764-777` | implemented; **no notification of a pending request** |
| Customer webhook relay (message/conversation) | `m20261114_000012_webhook_relay.rs`; `MESSAGING_PUBLIC_EVENTS_DESTINATION=developer-platform` | activated |
| Calls, mentions, reactions, Welcomes, membership changes → push | — | **absent** (single trigger only) |

**Workers:**

| Worker | Cadence | Notes |
|---|---|---|
| Outbox publisher | 250 ms + wake-up | Batch 500 |
| Push dispatcher | 1 s | Batch 200, 60 s lease, 8 attempts, 1 s → 5 min backoff, 24 h expiry |
| Receipt flush | 250 ms | — |
| Webhook relay | — | — |
| Identity feed poller | 2 s | — |
| Maintenance | 30 s | No push-job purge |

**Flags:**
- `MESSAGING_PUSH_PROVIDER` (none)
- `MESSAGING_PUSH_APNS_*`, `MESSAGING_PUSH_FCM_*`, `MESSAGING_PUSH_WEB_*` (refused by release tooling until activation, `coordinated_release.py:1243-1250`)
- `MESSAGING_E2EE_DEVICES_ENABLED` (**true**, run 43663108367)
- `MESSAGING_SESSION_DEVICES_ENABLED` (**true**)

Readiness in production: `outboxPending 0`, `outboxDeadLetter 0`.

### A.3 Calling

| Responsibility | Location | Status |
|---|---|---|
| Incoming-call alert via signaling WS (`call.incoming`), `PgNotifySignalBus`, post-commit publish | `calls/app/mod.rs:490-621`, `signaling/bus.rs`, `signaling/ws.rs` | implemented; activated; **production-tested** (41580265031, 41581639199) |
| Invitation recovery `GET /v1/calling/invitations`, `GET /v1/calling/history` | `signaling/http.rs:44-62` | production-tested |
| First-answer arbitration, `call.answered_elsewhere`, decline, ring-cancel, expiry (1 s sweep) | `calls/app/ringing.rs:139-838`; `call_answer_claims`, `call_answer_group_claims` | production-tested; race and mutation tests (`tests/app_races.rs`) |
| Push-invitation payload (`cfmeet.calling.push-invitation` v1) and `PushDispatcher` hook | `signaling/signals.rs:186-205`, `signaling/push.rs:19-33` | payload implemented; **transport absent** (`NoPushTransport`, `main.rs:453`, no env var) |
| Missed calls | derived at read time (`calls/app/history.rs:287-330`); metric `calling_missed_calls_total` | history production-tested; **notification absent**; no per-participant missed fact |
| Voicemail deposit | `VOICEMAIL_RECEIVED/READY` → outbox → `voicemail.*` webhooks | events implemented; **mailbox-owner notification absent** |
| Recording availability | `RECORDING_READY` → `recording.ready` webhook; `call.recording_started/stopped` compliance notice to every participant | notice production-tested; user notification absent |
| Enterprise ring groups, queues, IVR, transfers | realtime signals (Appendix A of evidence/03) | production-tested |
| Calling outbox → Developer Platform | `calling_outbox_events`; `CALLING_PUBLIC_EVENTS_DESTINATION=developer-platform` | activated. End-to-end Calling producer event **not driven** in acceptance (`CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:75`) |
| Legacy Calling webhooks | `webhooks/*`; `CALLING_WEBHOOK_AUTHORITY=retired`, `CALLING_WEBHOOK_DELIVERY_ENABLED=false` | **deprecated / retired** |
| Incidents (`calling_incidents`) | `calls/app/incidents.rs`; CLI `platform_admin` | implemented; **no alerting** |
| `limit.*` / `usage.*` notices | `webhooks/notices.rs` | implemented. Effectively **undelivered**: only the retired local transport carries them (`publication.rs:119-125`) |

**Production flags:**
- `CALLING_NEW_CALLS_ENABLED`, `CALLING_APP_CALLING_ENABLED`, `ENTERPRISE_CALLING_ENABLED`, `VOICEMAIL_ENABLED`, `CALL_RECORDING_ENABLED`: all true.
- All PSTN flags: false.

### A.4 Communications Hub

| Responsibility | Location | Status |
|---|---|---|
| Account-wide category preferences (7 booleans) | `communication_notification_preferences`; `chat.rs:354-419` | implemented; activated; CRUD production-tested (42189631003, 42191576906) |
| Conversation preferences (ALL/MENTIONS/MUTED + `muted_until`) | `communication_conversation_preferences`; `chat.rs:64-151` | activated |
| Contact mute (ALL/MUTED/MENTIONS, no expiry) | `communication_contact_aliases.notifications` | activated |
| Policy decision engine (only notification enforcement) | `policy.rs:88-219`; `POST /internal/v1/communications/policy/check` | implemented; called only by the Messaging push dispatcher → dormant |
| Meeting opt-out resolve | `preferences.rs:37-101`; `POST /internal/v1/communications/notification-preferences/resolve` | implemented; **grant to Meeting absent in release tooling** |
| Notification delivery status | `devices.rs:511-533` | implemented; production-tested; reports push `NOT_CONFIGURED` |
| Device push facade (pass-through to Messaging) | `devices.rs:315-376` | implemented; no client caller |
| Contact requests, connections, blocks, privacy | `relationships.rs`, `profiles.rs` → audit + outbox + change feed | implemented; **no user notification** (poll only) |
| Activity timeline `/activity`, change feed `/sync` | `calls.rs`, `sync.rs` | implemented. Neither is an inbox (no read state) |
| Inbound owner events `/events/*` | `internal.rs` | implemented; **disabled** (no producer, no grant) |
| `block-events` | `events.rs:244-251` | **deprecated** |

**Workers:** outbox dispatch every 500 ms (20 rows, 90 s lease, DEAD after 12); maintenance every 60 s; call-card reconcile every 5 s.

**Flags:** none for notifications.

### A.5 Organizations

| Responsibility | Status |
|---|---|
| Transactional outbox: per-aggregate ordering, 12 attempts, DEAD, replay (`src/outbox.rs`, `worker.rs:83-175`) | activated (run 42710192324). Certify 43670057279: DELIVERED 35, PENDING 59. Readiness `outbox.pending 0`, so the 59 PENDING are the never-dispatched `cfmeet.reconciliation` rows |
| Subscribers | `cfmeet.meeting-service` only, 13 event types. `invitation.*`, `team.*` have **no subscriber** (`outbox.rs:41-59`) |
| Invitation email | sent by the Meeting relay, not Organizations (A.1) |
| Role, suspension, ownership, removal notifications | **absent** (Meeting updates access only) |
| "My pending invitations" endpoint | **absent** |

**Flags:** `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service`, `ORGANIZATIONS_PROVISIONING_ENABLED=true`, `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true`.

### A.6 Developer Platform

| Responsibility | Status |
|---|---|
| Customer webhooks: HMAC v1, sealed secrets, SSRF guard, 8 attempts with 30 s → 6 h backoff, auto-disable after 50 failures, replay | implemented; activated (`DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE=active`); production-tested (43184898911, 43337892242, 43488534526; certify developer-acceptance 30/30) |
| Credential lifecycle notices (`credential.expiring_soon/expired`) | **webhook only**; activated; production-tested |
| Catalogue entries `credential.created/rotated/revoked`, `production_access.*` | in the catalogue, **never emitted** (dead) |
| Workspace invitation | token returned to the inviter; **no notification** |
| Production access approve/reject email | via the Meeting endpoint from an ops script; **not activated** |
| Developer security alerts | **absent** |
| Webhook dead-letter monitoring | **absent** (no metric, no alert) |

### A.7 Platform Access

It owns tenants, applications, environments, credentials, scopes, principals, the identity change feed and the audit log. It has **no notifications, sessions or devices**: those belong to Account in the Meeting Service (ADR 0057, 0060). Revocation propagates through:
- the Account outbox → Hub → Calling/Messaging (push);
- a principal-status mirror;
- the change feed (polled by Calling and Messaging);
- per-request credential authentication.

Status: implemented and activated. Not a notification producer today.

### A.8 API Gateway

It is default-deny from `infra/api-gateway/route-manifest.json`, which has 578 public routes. Notification-related routes:
- the Hub preference, delivery-status, device-push and inbox routes (upstream Meeting, `MEETING_SESSION`);
- Messaging `push/configuration` and `push-registration`;
- the webhook families (Calling `/v1/webhooks*` re-pointed to the Developer Platform; `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED=true`).

No `/v1/notifications` family exists. In production nginx, `/api/v2/communications/*` goes **directly to the Meeting Service**, not through the gateway. `API_GATEWAY_WEBSOCKETS_ENABLED=false`.

### A.9 Telephony, SMS Gateway, Numbering, Privacy Relay

| Service | Notification behaviour | Events that could feed notifications | Production |
|---|---|---|---|
| Telephony | none | `telephony_outbox` leg events (to Calling only); `telephony_incidents` | **disabled** (`TELEPHONY_ENABLED=false`, `TELEPHONY_SIP_EDGE=none`) |
| SMS Gateway | none. Only the `deterministic` test adapter, refused in production | `sms-gateway.message.*` | **disabled** (`SMS_GATEWAY_PROVIDER` unset; readiness `provider: none`) |
| Numbering | none | `number.*` (public subset → Developer Platform); KYC status is internal only | **disabled** (`NUMBERING_PROVIDERS` unset) |
| Privacy Relay | none | `privacy-relay.session.*`, `context.*` | **disabled** (`PRIVACY_RELAY_ENABLED=false`) |

**This blueprint preserves every disabled telecom setting. No dependency on these four services is introduced.**

### A.10 Security Scan Gateway

It scans Messaging attachments only, and is deployed and production-tested (runs 42102547990, 42113919022). It has **no incident notifications** and no alert rules. Numbering KYC uploads do not pass through it. This is not a Notification Service concern beyond "never forward unscanned attachments".

### A.11 Infrastructure monitoring

- Prometheus rules exist: `meeting-service.rules.yml` (15 alerts) and `messaging.rules.yml` (6). They are CI-validated by `promtool` and `amtool`.
- Prometheus and Alertmanager are **not deployed**. Every Alertmanager receiver is empty (`unconfigured-pager/ticket/info`).
- The only live operator notification is the TLS certificate monitor, which comments on GitHub issue #38 (verified run 42189603354).

**Classification:** this stays in infrastructure monitoring and does **not** belong in the end-user Notification Service (Document 3 §6).

### A.12 Web and mobile apps

| | Web (`apps/web`) | Mobile (`apps/mobile`) |
|---|---|---|
| Push registration | absent (no SW, no manifest, no `pushManager`) | absent (no `expo-notifications` / FCM / APNs / PushKit / CallKit) |
| System notification | `new Notification` for incoming calls when the tab is hidden (`IncomingCallLayer.tsx:70-72`) | none |
| Realtime | 3 WebSockets: Calling signaling, Messaging `/v1/realtime`, Conference | none (polling only) |
| In-app notification centre | absent. Unread/mention badges only inside the Messages page | absent |
| Preference UI | per-contact All/Mentions/Muted, privacy | static "Push notifications: not available yet" (`app/(settings)/communication.tsx:23-27`) |
| Hub notification endpoints called | none of `me/notification-preferences`, `notification-delivery-status`, `conversations/{id}/preferences`, `devices/{id}/push` | none (mobile route contract `meeting-service.json` has no Hub routes; `tests/routes.test.ts` enforces this) |
| Risks | Calling endpoint survives logout; `cfsig_` token in the WS subprotocol header; reset code in the URL | `voip` background mode declared without implementation (App Store risk); custom-scheme deep links |

---

## B. End-to-end inventory by notification category (Part 4)

Legend for the channel columns:
- **E** = email
- **P** = device push
- **R** = realtime signal
- **I** = in-app persistent
- **W** = customer webhook
- **A** = audit/event only
- **—** = nothing

### B.1 Account and security (Part 4.1)

| Category | Security-critical | E | P | R | I | A | Status / evidence |
|---|---|---|---|---|---|---|---|
| Registration verification code | **yes** | ✔ | — | — | — | ✔ | implemented; transport not recovered. `handlers/auth.rs:78-163` |
| Resend verification | yes | ✔ | — | — | — | — | same |
| Login security alert (new sign-in) | yes | — | — | — | — | ✔ | **absent** (audit `LOGIN` only; "Not created", `CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5) |
| Password reset code | **yes** | ✔ | — | — | — | ✔ | implemented; transport not recovered |
| Password changed / reset completed | yes | ✔ | — | — | — | ✔ | implemented; best-effort after commit |
| Email change | yes | — | — | — | — | — | **absent** (no workflow) |
| Device link requested / approved / rejected | yes | — | — | — | — | ✔ | audit only |
| Device email step-up code | **yes** | ✔ | — | — | — | ✔ | implemented; transactional |
| New trusted browser / new primary phone | yes | ✔ | — | — | ✔(pull) | ✔ | implemented; best-effort; `/api/v2/me/security-events` is pull-only |
| Old phone replaced (notice to the old device) | yes | — | — | — | — | ✔ | absent |
| Device revocation | yes | — | — | — | — | ✔ | audit only |
| Sign-out everywhere | yes | — | — | — | — | ✔ | audit/event only |
| Suspicious session (refresh-token reuse) | yes | — | — | — | — | ✔ | audit only |
| Deactivation / recovery / admin suspension | yes | — | — | — | — | ✔ | audit only |
| Phone/SMS OTP, MFA | — | — | — | — | — | — | absent (excluded by owner) |

**OTP authority:** codes are generated and verified only by the Meeting Service.
- Verification code: 24 h TTL. Reset code: 1 h TTL.
- 5 attempts per 15 min; single use.
- Stored in Redis (`services/otp.rs`).
- The email ledger carries the code in `text_body` only until a final state. The body is then erased.

**This authority must not move** (Document 5 §4.4).

### B.2 Meetings (Part 4.2)

| Category | E | R | I | W | Status |
|---|---|---|---|---|---|
| Invitation (space/conference) | ✔ (on request, synchronous) | — | — | — | implemented; likely failing for account holders |
| Invitation accepted/declined → inviter | — | — | — | — | **absent** |
| Reminder (lead 15 min) | ✔ (durable) | — | — | — | implemented; likely failing for account holders |
| Reschedule | ✔ (durable, V2 only) | — | — | — | implemented; same |
| Cancellation | ✔ (durable, V2 only) | ✔ (`conference.cancelled`) | — | — | implemented; same |
| Recurrence-exception / series edit / archive / system expiry | — | ✔ (cancel event) | — | — | **absent** (email) |
| Meeting starting | — | ✔ (`conference.started`, in-meeting subscribers) | — | ✔ (`meeting.started`) | realtime and webhook only |
| Waiting room join request / admit | — | ✔ | — | — | realtime only |
| Host/participant updates | — | ✔ | — | ✔ (`participant.*`) | realtime only |
| Webinar registration confirmation | — | ✔ (`registration.created`) | — | — | **absent** (email) |
| Recording available | — | ✔ | — | ✔ (`recording.ready`, environment-owned spaces) | user notification absent |
| Calendar (ICS) change | — | — | — | — | **absent** (ICS is download only) |

### B.3 Calling (Part 4.3)

| Category | R | P | I | W | Status |
|---|---|---|---|---|---|
| Incoming voice/video call | ✔ `call.incoming` | ✗ (`NoPushTransport`) | — | ✔ `call.incoming` | ringing production-tested; background alert absent |
| Answered on another device | ✔ `call.answered_elsewhere` | — | — | ✔ | production-tested |
| Missed call | — (derived) | — | history only | `call.no_answer` (Call-level) | notification absent |
| Voicemail | caller only `call.voicemail_started` | — | owner list API | ✔ `voicemail.*` | owner notification absent |
| Recording availability | compliance notice ✔ | — | list API | ✔ `recording.ready` | user notification absent |
| Enterprise ring group / queue offer | ✔ | — | — | ✔ (call.*) / queue facts internal | production-tested |
| Transfer / participant changes | ✔ (transfer) | — | — | ✔ (transfer), participant internal | production-tested |

### B.4 Messaging (Part 4.4)

| Category | PRIVATE_E2EE | MANAGED | Status |
|---|---|---|---|
| New DM / group message | P (metadata only; dormant), R ✔ | R ✔; **P impossible** (session devices) | push dormant |
| Mention | no distinct push | no distinct push; mentions stored for counts | absent |
| Reply / reaction | — | — | absent |
| Group invitation / message request | — (INVITED member receives nothing) | — | absent |
| Membership change | R (inbox signals) | R | realtime only |
| Conversation mute | Hub conversation mute (personal scope) | **none** (org scope not governed) | partial |
| Read/delivery status | cursors ✔ | cursors ✔ | implemented. Push ≠ delivery (`push.rs:15`) |
| Offline delivery | DB event stream + replay | same | implemented |
| Multi-device sync | read-cursor sync, no clear-on-read for push | same | partial |

**E2EE safeguards already in place:**
- the payload is built in SQL from IDs and sequences;
- a DB CHECK refuses content-like keys (`m20261106_000004_realtime.rs:139-140`);
- the adapter whitelist allows 5 keys only;
- dispatch-time device and membership recheck.

**Residual exposure:** APNs and FCM see `conversationId` and the sequence numbers in clear. Web Push encrypts them.

### B.5 Communications and contacts (Part 4.5)

Contact requests, accept/decline, cancellation, expiry, removal, blocks, privacy changes, favourites and connect invites all produce **audit rows, outbox events (consumed and ignored by Calling and Messaging) and change-feed rows. They produce no notifications.** A recipient learns of a request only by polling `/sync`, `/contact-requests/incoming` or the `/bootstrap` counts.

### B.6 Organizations (Part 4.6)

| Event | Notified? |
|---|---|
| Invitation created | email only via the Meeting relay when `sendEmail` is set; no in-app notice; no subscriber |
| Invitation accepted/declined/revoked/expired | no |
| Membership role changed / suspended / removed / left | no (access change only) |
| Ownership transferred | no |
| Organization suspended/reactivated/archived | no |
| Team changes | no (no subscriber) |

### B.7 Developer Platform (Part 4.7)

**Human notifications (absent except one inactive email):** workspace invitation, credential created/rotated/revoked, production-access requested/approved/rejected, suspension, security alerts.

**Customer webhooks (live):** 72 catalogue types. 5 of them are dead entries (never emitted).

**These are different products and must stay separate.**

### B.8 Infrastructure and operational alerts (Part 4.8)

| Mechanism | Status | Belongs in |
|---|---|---|
| Prometheus rules (Meeting outbox dead letters, Messaging dead letters, …) | written, not deployed | infrastructure monitoring |
| Alertmanager receivers | empty | infrastructure monitoring |
| Calling/Telephony/Messaging incidents tables | DB + CLI | infrastructure monitoring (an incident → alert bridge is optional later) |
| TLS certificate monitor → GitHub issue #38 | **live** | infrastructure monitoring |
| Disk capacity journal check | journal only | infrastructure monitoring |
| Deployment results | GitHub Actions | CI/CD |

---

## C. Database inventory (Part 5.1)

"Rows" is not measured; Document 5 §2 Q-n gives the query. "Ret." is the retention behaviour.

| # | Table | Owner (DB) | Key columns / keys / dedupe | Ret. | Sensitive fields | Dependencies | Migration risk |
|---|---|---|---|---|---|---|---|
| 1 | `email_deliveries` | Meeting (`rtc_db` in `cosmicforge-db`) | PK `id`; **`UNIQUE idempotency_key`**; partial UNIQUE `(provider, provider_message_id)`; status ∈ 9 states; lease cols; `expires_at` | recipient nulled +30 d after final, row deleted +60 d; **non-final rows never purged** | plaintext `recipient`, `recipient_name`, `subject` (meeting titles), bodies with OTP/tokens until final | worker, webhooks | **Migrate (revision 2):** superseded by `notif_email_messages`. Not copied; the table drains behind the fence and expires by retention (Document 5 §7) |
| 2 | `email_delivery_events` | Meeting | PK `(provider, event_id)` | +60 d | none | Resend webhook | none |
| 3 | `meeting_notifications` | Meeting | PK `id`; **`UNIQUE dedupe_key`**; kind ∈ {RESCHEDULED, CANCELLED, REMINDER}; status ∈ {PENDING, SENT, SKIPPED, FAILED}; lease | completed rows purged after 30 d (only while the flag is on) | none (ids, times) | notification worker | **Migrate:** reminder scheduling and notices move to the service scheduler; the table drains and is retired (Document 5 §10) |
| 4 | `meeting_notification_recipients` | Meeting | PK `(notification_id, recipient_digest)`, FK cascade | with parent | unsalted SHA-256 email digest | — | as above |
| 5 | `account_communication_outbox` | Meeting | lease, backoff; **no max attempts** | not documented | principal ids | Hub | untouched |
| 6 | `account_security_events` | Meeting | security event log | per Account policy | user/session ids | `/me/security-events` | **source** for security notices (read, never moved) |
| 7 | `outbox_events` (conference) | Meeting | channel REALTIME/PUBLIC_EVENT; FAILED dead letter | — | — | realtime relay, platform events | untouched |
| 8 | `webhooks` (legacy) | Meeting | — | — | — | — | deprecated; drop later per its own sunset plan |
| 9 | `messaging_push_registrations` | Messaging | PK `id`; **`UNIQUE device_id`** (FK `messaging_devices`); platform; `push_handle` VARCHAR(4096); `enabled`, `disabled_reason` | **none** (disabled handles kept) | **plaintext push handles** (Web Push `auth` secret) | dispatcher, Hub facade | **Migrate:** superseded by `notif_push_endpoints` (sealed); copied only if Q6 > 0 |
| 10 | `messaging_push_jobs` | Messaging | **`UNIQUE (event_id, device_id)`**; status ∈ 6; lease; payload CHECK metadata-only | **none** (unbounded) | none | dispatcher | add retention (N0) |
| 11 | `messaging_outbox_events` | Messaging | `evt_` id; per-aggregate order; DEAD_LETTER after 12 | not swept | content-key CHECK | publisher | untouched |
| 12 | `messaging_read_cursors`, `messaging_received_cursors` | Messaging | — | — | — | receipts | untouched (read state ≠ notification read) |
| 13 | `messaging_communication_inbox` | Messaging | PK `event_id` + digest | — | — | Hub events | untouched |
| 14 | `communication_notification_preferences` | Hub | PK `profile_id`; 7 booleans; `version` | life of profile | preference data | Messaging policy, Meeting resolve | **Migrate:** copied to `notif_preferences` (Phase 4.2); the Hub endpoints become facades; columns kept read-only until Phase 8 |
| 15 | `communication_conversation_preferences` | Hub | PK `(owner_profile_id, conversation_id)`; `notifications`, `muted_until` | — | — | policy | **Migrate** `notifications`/`muted_until` to `notif_conversation_preferences` (Phase 4.2); other columns stay in the Hub |
| 16 | `communication_contact_aliases.notifications` | Hub | — | — | — | policy | **Migrate** to `notif_contact_preferences` (Phase 4.2); alias, favourite and rank stay in the Hub |
| 17 | `communication_devices` | Hub | `chat_device_ref`, `call_endpoint_ref`, `push_platform`, `push_registered_at`, `account_session_ref` | REVOKED terminal | — | device index | used as the device mapping source |
| 18 | `communication_outbox` | Hub | `UNIQUE (event_id, destination)`; **destination CHECK IN ('CALLING','MESSAGING')** | delivered +30 d; DEAD kept | ids | Calling, Messaging | additive CHECK change to add `NOTIFICATIONS` (Phase 2) |
| 19 | `communication_timeline_entries` | Hub | activity cards | — | — | `/activity` | untouched |
| 20 | `organization_outbox` | Organizations | `UNIQUE (event_id, destination)`; per-aggregate order; DEAD after 12 | **never deleted** | none (forbidden keys) | Meeting consumer | add NS as a subscriber (code + config) |
| 21 | `organization_invitations` | Organizations | `token_hash` unique; status; `expires_at` | — | `email_normalized` | — | **never read by NS directly** (event payload refs only) |
| 22 | `calling_outbox_events` | Calling | `evt_` UNIQUE; `source_event_id` FK; DEAD_LETTER; destination outcomes | **no sweep** | none (projection whitelist) | publisher | add a notifications destination (Phase 2) |
| 23 | `call_invitations`, `call_answer_claims`, `call_answer_group_claims`, `call_endpoints` (`push_target_ref`) | Calling | arbitration uniqueness | — | — | ringing | **never touched** |
| 24 | `voicemail_mailboxes`, `voicemail_messages` | Calling | `owner_type` ∈ PRINCIPAL/EXTENSION/QUEUE/RING_GROUP | per mailbox | audio refs | — | recipients resolved by Calling (Document 4 §4.3) |
| 25 | `calling_incidents` | Calling | `UNIQUE dedupe_key WHERE OPEN` | resolved +365 d | — | CLI | monitoring, not NS |
| 26 | `devplat_webhook_subscriptions/_events/_deliveries/_attempts` | Developer Platform | delivery state machine; `UNIQUE (subscription_id, event_id)` | never deleted | sealed secrets | worker | **untouched** (customer webhooks ≠ notifications) |
| 27 | `devplat_credential_lifecycle_notices` | Developer Platform | PK `(credential_id, event_type, threshold_days, expires_at)` | append-only | — | sweep | source of the developer human notice (Phase 4) |
| 28 | `devplat_activity_events`, `devplat_invitations` | Developer Platform | — | — | invitation digest | — | sources for developer notices |
| 29 | `platform_identity_changes`, `platform_access_audit`, `platform_principals` | Platform Access | append-only feed | — | — | Calling/Messaging pollers | read-only source for principal status |
| 30 | Redis `otp:*`, `rate:*`, `email:queue` (legacy) | Meeting | — | TTL | **plaintext OTP, email in key** | auth | stays; hashing fix in N0 |

No two of these tables duplicate each other's function. Similar names, such as the three `*_outbox` tables, serve different aggregates and destinations.

---

## D. API inventory (Part 5.2)

These are **EXISTING APIs only**. The migration action uses: Preserve / Delegate (keep path, change backend) / Replace (new path, old kept compatible) / Deprecate.

**Revision 2 (unified service, D-0):** every public contract below is kept. Where a notification side effect sits behind an endpoint (account emails, invitations, notices, push registration, preferences), that side effect is **delegated to the unified service** behind a fence (Document 5 §6). The endpoint itself stays as it is, or becomes a facade.

| Service | Method & path | operationId | Auth | Consumer | Prod state | Action |
|---|---|---|---|---|---|---|
| Meeting | POST `/api/v1/auth/register` | register | none | web, mobile | activated | **Preserve** (Account) |
| Meeting | POST `/api/v1/auth/resend-verification` | resend_verification_email | none | web | activated | Preserve |
| Meeting | POST `/api/v1/auth/verify-email` | verify_email | none | web | activated | Preserve |
| Meeting | POST `/api/v1/auth/forgot-password` | forgot_password | none | web, mobile | activated | Preserve |
| Meeting | POST `/api/v1/auth/reset-password` | reset_password | none | web | activated | Preserve |
| Meeting | POST `/api/v1/auth/verify-reset-token` | verify_reset_token | none | web | activated | Preserve. **Fix enumeration** (N0) |
| Meeting | POST `/api/v1/auth/change-password` | change_password | bearer | web | activated | Preserve |
| Meeting | POST `/api/v2/me/device-links/{linkId}/email-code` | v2_send_device_link_email_code | bearer | web | activated | Preserve |
| Meeting | POST `/api/v2/me/device-links/{linkId}/complete` | v2_complete_device_link | bearer | web | activated | Preserve. Its notice is emitted as an event in Phase 4 (optional) |
| Meeting | GET `/api/v2/me/security-events` | v2_list_my_security_events | bearer | web | activated | Preserve (the security feed stays with Account) |
| Meeting | POST `/api/v2/meeting-spaces/{id}/invitations` | v2_create_invitation | bearer | web | activated | Preserve the contract (`emailDelivery` kept); **email delegated** to the service with a sealed token (Phase 4.4) |
| Meeting | POST `/api/v2/organizations/{id}/invitations` | v2_create_organization_invitation | bearer | web | activated | Preserve the contract; **email delegated** with the token and address sealed (Phase 4.4); in-app copy from `invitation.created` |
| Meeting | POST `/api/v2/conferences/{id}/reschedule`, `/cancel` | v2_reschedule_conference, v2_cancel_conference | bearer | web | activated | Preserve; notice **delegated** |
| Meeting | GET/WS `/api/v2/conferences/{id}/realtime`, `/events`, POST `…/realtime/ticket` | v2_realtime_* | bearer/participant | web | activated | Preserve (in-meeting) |
| Meeting | POST `/api/v1/email/provider-events/resend` | — (not in OpenAPI) | Svix HMAC | Resend | configured-off | Preserve |
| Meeting | POST `/internal/operations/email/acceptance`, GET `/deliveries/{id}`, POST `/developer-access-decision` | — | `X-Internal-Token` | ops scripts | implemented | **Verify not publicly reachable** (N0-6). Replaced by the service's own acceptance and diagnostics; retired in Phase 8 |
| Meeting | ANY `/api/v2/communications/*` | (Hub contract ids) | bearer | web | activated | Preserve (relay) |
| Meeting | `/api/v1/webhooks*` | — | bearer | none | deprecated | Deprecate (existing sunset plan) |
| Hub | GET/PUT `/api/v2/communications/me/notification-preferences` | getNotificationPreferences / updateNotificationPreferences | bearer | SDK only | prod-tested | **Delegate:** facade over the service's U6/U7 (Phase 4.2), same schema |
| Hub | GET/PUT `/api/v2/communications/conversations/{id}/preferences` | get/updateConversationPreferences | bearer | SDK only | activated | Preserve |
| Hub | PATCH `/api/v2/communications/contacts/{contactId}` | updateCommunicationsContact | bearer | web | activated | Preserve |
| Hub | GET `/api/v2/communications/me/notification-delivery-status` | getNotificationDeliveryStatus | bearer | SDK only | prod-tested | **Delegate:** answered from the service's delivery data (Phase 4.1) |
| Hub | PUT/DELETE `/api/v2/communications/devices/{id}/push` | register/removeCommunicationsDevicePush | bearer | none | implemented | **Delegate:** facade over U8/U9 (Phase 4.1) |
| Hub | GET `/api/v2/communications/activity`, `/inbox`, `/sync`, `/events`, `/bootstrap` | … | bearer | web (partly) | activated | Preserve. **Not replaced** by the NS inbox (different semantics) |
| Hub (internal) | POST `/internal/v1/communications/notification-preferences/resolve` | resolveNotificationPreferences | service (`communications-preferences.read`) | Meeting | implemented, grant missing | Grant fix (N0-1). **Deprecated** after Phase 4.2 (preferences live in the service); retired in Phase 8 |
| Hub (internal) | POST `/internal/v1/communications/policy/check` | checkCommunicationsPolicy | service | Messaging | dormant | Preserve |
| Messaging | PUT/DELETE `/v1/messaging/devices/{deviceId}/push-registration` | registerPush / unregisterPush | customer credential / client token | none (SDK) | implemented | **Delegate:** facade over the service with the tenant scope (Phase 4.1); contract unchanged |
| Messaging | GET `/v1/messaging/push/configuration` | getPushConfiguration | customer credential | none | implemented | Preserve |
| Messaging (internal) | POST `/internal/v1/communications/devices/push`, `/push-status`, `/devices/verify`, `/devices/revoke` | apply…DevicePush, read…PushStatus, … | service | Hub | implemented | Preserve |
| Calling | POST/GET/DELETE `/v1/calling/endpoints` | register/list/revokeEndpoint | platform credential | web | activated | Preserve; `pushTargetRef` gets meaning under D-1 |
| Calling | GET `/v1/calling/signaling`; GET `/v1/calling/invitations`, `/history`; POST accept/decline | openSignalingChannel, recoverInvitations, … | signaling credential | web | prod-tested | **Preserve, never routed via NS** |
| Calling | GET `/v1/calls?missed=true` | listCalls | platform credential | web | prod-tested | Preserve (history is authoritative for missed calls) |
| Calling | GET `/v1/enterprise/voicemail/messages*`, `/recordings*` | listVoicemailMessages, … | platform credential | web | prod-tested | Preserve |
| Calling | `/v1/webhooks*` | createWebhook, … | platform credential | — | retired (re-pointed to the Developer Platform) | Preserve the re-pointing |
| Developer Platform | `/v1/webhooks*`, `/v1/environments/{id}/webhooks*`, replay | … | platform credential / developer session | customers, console | prod-tested | **Preserve**; joins the unified service only if owner decision D-W |
| Organizations (internal) | POST `/internal/v1/organizations/{id}/invitations`; GET/POST `/internal/v1/admin/event-delivery[/replay]` | … | service | Meeting relay, ops | activated | Preserve |

**Inconsistencies found:**
- Hub internal routes (`/events/*`, `/admin/*`, `/readiness`, `/reconciliation/range`) are missing from `communications/internal/v1.openapi.json`.
- The Meeting internal email routes are absent from OpenAPI.
- `pushTargetRef` is documented but never set.
- Two mute models (contact vs conversation) have no defined precedence.
- `MENTIONS` is unenforceable for PRIVATE_E2EE.
- The web's hand-written Hub types say 1.2.0, but the served contract is 1.3.0.

**Insecure authorization:** the shared static `INTERNAL_API_TOKEN` on public-router internal email routes (verify, N0-6), and the caller-controlled `notify` (N0-4).

**No public endpoint is proposed for deletion.**

---

## E. Event and job inventory (Part 5.3)

| Path | Business action → transaction | Event / outbox | Worker | Recipient resolution | Preferences | Channel → transport | Evidence of delivery | UI |
|---|---|---|---|---|---|---|---|---|
| Account OTP | register/forgot → user row + Redis code | direct ledger enqueue (after commit) | email worker | the address itself | none (security) | email → SMTP (Resend later) | ledger status; webhook DELIVERED (when configured) | web form |
| Device step-up | link → `account_device_links` + ledger row (same txn) | ledger | email worker | account email | none | email | ledger | web |
| Meeting notice | reschedule/cancel → conference row + `meeting_notifications` (same txn); reminder by worker scan | `meeting_notifications` (`dedupe_key`) | notification worker → ledger | `RECIPIENTS_SQL` in Meeting | Hub resolve (fail closed) | email | `meeting_notification_recipients` + ledger | none |
| Meeting / org invitation | create → (Organizations) → synchronous email | none (synchronous) | — | the invitee address | Hub resolve (meeting only) | email | response `emailDelivery` | web toast |
| Message push | send → message + outbox (same txn) | `messaging_outbox_events` → `messaging_push_jobs` (`UNIQUE event,device`) | publisher → dispatcher | ACTIVE members' push-registered devices | Hub `policy/check` (personal scope) | push → APNs/FCM/Web Push (none configured) | job status | none |
| Call ring | create call → invitations (txn) | post-commit `pg_notify` | none (synchronous) | endpoints of the participant | Hub `DirectCall` privacy (not a preference) | WS signal; push hook → `NoPushTransport` | invitation status | web call layer |
| Calling public events | any call event → `call_events` + `calling_outbox_events` (deferred trigger) | outbox | Calling worker (1 s, 10 attempts, DEAD → incident) | — | — | Developer Platform ingest → webhook | Developer Platform deliveries | customers |
| Org events | org mutation → `organization_outbox` (same txn) | per-subscriber rows, ordered | Organizations worker (90 s lease, 12 attempts) | Meeting | — | internal HTTP | DELIVERED state | — |
| Hub events | contact/privacy/account → `communication_outbox` | destinations CALLING / MESSAGING | Hub worker (500 ms) | — | — | internal HTTP | DELIVERED | — |
| Credential expiry | DP sweep (hourly) | `devplat_credential_lifecycle_notices` | Developer Platform worker | the environment's subscriptions | — | webhook | deliveries + attempts | console |

**Schema versions:**
- Hub envelope `schemaVersion:1`.
- Organizations `schemaVersion:1`.
- Messaging and Calling use `cfmeet.platform.event-envelope` with `schema_version`.
- The Calling push payload is `cfmeet.calling.push-invitation` v1.

**Identity:** `evt_<32hex>` for Messaging and Calling; UUID `eventId` for the Hub and Organizations.

**Ordering:** per-aggregate in Messaging, Organizations and Calling (per call); none in Hub delivery beyond per-destination sequence.

**Replay:**
- Organizations `/admin/event-delivery/replay`;
- Hub `/replay`;
- Calling `replay_dead_lettered_outbox` (CLI);
- Developer Platform delivery replay.

**Critical paths that centralization could disrupt (never route through NS):**
1. call ring → arbitration → `answered_elsewhere`;
2. OTP generation and verification;
3. Messaging ciphertext persistence and realtime fan-out;
4. Organization access changes (offboarding);
5. customer webhooks.
