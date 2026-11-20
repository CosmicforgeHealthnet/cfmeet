# Notification Service runbook

Operations guide for `services/notifications` (identity `cfmeet.notifications`, database `cfmeet_notifications`). Design: `docs/audits/notification-service/` (Documents 4–7). Implementation status: Document 10.

## 1. What runs where

| Item | Value |
|---|---|
| Containers | `cfmeet-notifications` (replica `notifications-1`, private TLS route `https://api.cfmeet.com:8456`) and `cfmeet-notifications-2` (replica `notifications-2`, private network only). 256 MiB each, read-only, no capability, no published port |
| Database | `cfmeet_notifications` in `cfmeet-core-db`; roles `notifications` (runtime) and `notifications_migrator` |
| Secrets (0400 files, mounted read-only) | `/opt/cfmeet-core/secrets/notifications-service-auth.pem` (signing key), `notifications-data.key` (master key: sealing at rest and keyed digests), trust bundle `coordinated/trust/notifications.trust.json`; provider files only after an activation |
| Workers per replica | W1 ingest, W2 email CRITICAL lane, W3 email STANDARD lane + push, W4 scheduler (also runs hourly retention; retention has no heartbeat of its own, so `/ready` stays green between runs). Replicas coordinate only through `FOR UPDATE SKIP LOCKED` claims and leases |
| Probes | `GET /health` (liveness), `GET /ready` (database, migrations, worker heartbeats, `routingProtocol`, provider kinds), `GET /metrics` (Prometheus text, private network) |

The service is never in a synchronous business path: sign-in, code verification, ringing, message send and webhook delivery keep working with both replicas down.

## 2. Dark state and routing

Every `(category, channel)` row of `notif_routing` starts `OFF`. While a category is `OFF` its events are **HELD** (stored, never sent, counted by `notif_held_events`). Modes are changed only by coordinated-release operations; each change bumps `version` and records the run id in `changed_by_run`.

| Mode | Effect |
|---|---|
| `OFF` | nothing is delivered; incoming events are HELD |
| `SHADOW` | full pipeline, `SHADOW` deliveries and shadow observations (I7), no provider call |
| `ACTIVE` | delivered |

Read the state with I8 (`GET /internal/v1/admin/routing`, operator console identity). Producer shadow copies (I1 `shadow: true`) are never delivered whatever the mode.

## 3. Alerts

| Alert | Meaning | First action |
|---|---|---|
| `NotificationsUnavailable` | no replica answers the scrape (independent path: Prometheus `up`, not the service) | §4 |
| `NotificationsReplicaDown` | one replica is down; the other claims all work | restart through `rollback` / the next release; check `docker logs` (IDs and classes only) |
| `NotificationsCriticalBacklog` | a CRITICAL event or email has waited more than 60 s | §5 |
| `NotificationsCriticalSendWithoutLease` | a CRITICAL email left without an Account lease | §5; freeze the category (`FENCED_OLD`) |
| `NotificationsHeldEvents` | events arrive for a category that is `OFF` | §6 |
| `NotificationsDeadLetters` | events parked after their retry budget | §7 |
| `NotificationsEmailBreakerOpen` | the email provider failed 5 times in a row | §8 |
| `NotificationsStandardBacklog` | STANDARD email waited more than 15 minutes | §8 |

## 4. Outage and fallback

Both replicas down, or the database down: producers keep their outbox rows; nothing is lost. CRITICAL codes are protected by the Account fallback (Document 4 §10.4): AUTO failover after 60 s reroutes never-leased decisions to the retained Meeting email path; FORCED is the break-glass operation (`critical_fallback = FORCED`, no restart). Verify the old path with `email-acceptance confirm=otp`. Do not delete HELD or PENDING events.

Recovery: restore the containers with `rollback` (previous images) or release again; `/ready` must report `migrations: current` and fresh heartbeats on both replicas. Then confirm the backlog gauges drain.

## 5. CRITICAL lane

1. Check `notif_email_backlog_oldest_seconds{lane="CRITICAL"}` and `notif_event_backlog_oldest_seconds{lane="CRITICAL"}`.
2. If Account leases are refused (`notif_email_messages.last_error_class = LEASE_UNAVAILABLE`), the Account owner API (X6) is unreachable: nothing is sent, which is safe; Account's AUTO failover takes over after 60 s.
3. Any `notif_critical_send_without_lease_total` increase is a protocol violation: set the category to `FENCED_OLD` on the producer and `OFF` on the service, then investigate.

### Account CRITICAL lane

The Meeting Service (Account) side of codes and security notices (roadmap N12, `services/meeting-service/src/services/critical_notifications.rs`). In `LEGACY_ONLY` the CRITICAL categories use the old ledger exactly as before. Outside it, each message gets one sticky decision. Every CRITICAL enqueue point has a catalogue event (contract `cfmeet.notifications.event` 1.1.0): the device step-up code (`device-step-up:`), the device-linked notice, and the passwordless codes of PR #97 — the sign-in code (`otp:signin:`, `account.sign-in-code.requested`), the email-change code (`otp:emailchange:`, `account.email-change-code.requested`) and the step-up action code (`otp:stepup:`, `account.step-up-code.requested`). The sign-in code may be for an address with no account yet: it names one email principal (no account subject) and seals the code, the magic-link token and the address; the email-change code names the email principal of the new address (sealed) on behalf of the account, which X1 must report active; the step-up code goes to the account with `purpose` `DEACTIVATE_ACCOUNT` / `CHANGE_EMAIL`. They are routed by `AUTH_VERIFICATION` (sign-in, email change) and `SECURITY_DEVICE_STEP_UP` (step-up) like the other codes, and fail over exactly like them. A sign-in code whose magic link or request facts the catalogue cannot carry exactly (no link token, unparsable request time, a client label over 80 characters, an unmasked network) is decided `OLD` (`UNPUBLISHABLE`). A `NEW` decision is one `account_notification_outbox` row holding the envelope (code sealed to the service key) and `fallback_sealed` (the legacy email sealed to the Account fallback key, `CF_ACCOUNT_FALLBACK_KEY_FILE`).

**Account notices** (owner decision A, the N12 follow-up; contract `cfmeet.notifications.event` 1.3.0): the sixth CRITICAL category, `ACCOUNT_NOTICES` (fence migration `m20261118_000006_account_notices`, service routing row `ACCOUNT_NOTICES` / `EMAIL` from `m20261118_000001_account_notices`), carries the emails the Meeting Service used to send only through its legacy ledger: the welcome (`AUTH_WELCOME`, `welcome:`, `account.welcome.requested`), the new sign-in alert (`new-sign-in:`, `account.security.new-sign-in`), the email-change attempt notice (`email-in-use:`, `account.security.email-in-use`) and the two email-changed notices of a self-service or support-assisted change (`email-changed:old:` / `email-changed:new:`, `account.security.email-changed` with `side` `OLD` / `NEW`). Each names one email principal with its address sealed; the welcome, the alert and the email-changed notices also name the account (the subject X1 must report active, and whose name greets), the email-in-use notice names none. The undo token and the masked new address of the `OLD` notice, and the client label and masked network of the alert, are sealed too, never in data. The service rebuilds the undo link (`<url>/auth/email-change/undo?token=…`), the signed-in devices link (`<url>/settings/sessions`) and the welcome's link from `NOTIFICATIONS_APP_URL`, so before `ACCOUNT_NOTICES` leaves `LEGACY_ONLY` / `SHADOW` it must equal the Meeting Service's `APP_URL` (a Meeting `APP_URL` ending in `/` keeps every welcome on the legacy path, `UNPUBLISHABLE`). A suppressed recipient's welcome is refused as before; an alert whose request facts the catalogue cannot carry exactly, or a masked address that is not `x***@domain`, is decided `OLD` (`UNPUBLISHABLE`). In `LEGACY_ONLY` the ledger rows are unchanged.

| Signal | Meaning | First action |
|---|---|---|
| `MeetingCriticalAutoFailover` (warning) | a never-leased code was rerouted to the old path (no `ACCEPTED` / no X6 lease within 60 s, or a refusal) | delivery continues; investigate the service (§4) |
| `MeetingCriticalCodeStuck` (critical) | an open decision is older than 120 s: leased without X7, `OFF`, or the relay stopped | `GET /internal/operations/notifications/critical` (loopback, `X-Internal-Token`) |
| `MeetingCriticalFallbackUnavailable` (critical) | a failover could not open `fallback_sealed` | set `critical_fallback = FORCED`, restore the key file, check `/health` `criticalFallbackKey: true` on both replicas |
| `MeetingCriticalDeadLetters` (critical) | the service refused an event while `critical_fallback = OFF` | restore `AUTO`; the user requests a new code |
| `MeetingCriticalPossibleDuplicate` (warning) | a break-glass reroute of a leased decision, or an X7 after a reroute | recorded (`REROUTED_POSSIBLE_DUPLICATE`); at most one extra email with the same code |

The service builds the sign-in magic link from `NOTIFICATIONS_APP_URL` exactly as Meeting builds it from `APP_URL` (`<url>/auth/magic?email=…&token=…`): before `AUTH_VERIFICATION` leaves `LEGACY_ONLY` / `SHADOW`, both must name the same web app origin, or the link in a service-sent email points elsewhere than the legacy one. While `AUTH_VERIFICATION` is `FENCED_NEW`, a sign-in code delivered by the service has no row in the Meeting ledger (`email_deliveries`): acceptance checks that look for `otp:signin:` rows there (the OTP journey of `email_operations.py`) find one only after a failover — check `notif_email_messages` (key `otp:signin:…`) on the service instead.

Rules the relay enforces (Document 4 §9.8, §10.4):
- **AUTO** (default): a `NEW` decision that was never leased fails over after 60 s, in one row-locked transaction that writes the legacy email (same code, same key). The service can never send it afterwards (X6 answers `REVOKED`).
- **FORCED** (break-glass, no restart): new CRITICAL messages are decided `OLD`; in-flight never-leased ones fail over at once.
- **OFF**: no automatic failover (refused events become dead letters and page).
- A **leased** decision is never failed over automatically. If the lease lapsed without X7 (the service crashed mid-send), first set `critical_fallback = FORCED`, then `POST /internal/operations/notifications/critical/reroute` with `{"decisionIds": [...]}` from the status list; each is recorded `REROUTED_POSSIBLE_DUPLICATE`. A live lease answers `WAIT_FOR_LEASE`, a completed decision `REFUSED`.
- Codes that expired are sent by neither path.

Fallback drill (readiness invariants 1, 2, 4): `POST /internal/operations/email/fallback-drill` with `{"to": "<acceptance inbox>", "reference": "<run id>"}` queues one `OPERATIONS_ACCEPTANCE` email through the exact failover path (sealed to and opened with the fallback key, sent by the legacy worker with its own credentials); poll `GET /internal/operations/email/deliveries/{id}` for `ACCEPTED`, and `DELIVERED` with the Resend webhook. 503 `FALLBACK_KEY_UNAVAILABLE` means no key is loaded.

## 6. HELD events

HELD means a producer routed `NEW` while the service mode is `OFF` (operator error or an activation in progress). Fix the mode with the activation operation, then replay the HELD events with I6 (`POST /internal/v1/admin/dead-letters/replay`, ids `event:<producer>:<eventId>`). CRITICAL items are never replayed (a code is re-requested by the user).

An `active` **bundle** run of `activate-notifications-routing` (`user-facing-in-app-active`, `user-facing-push-active`, §11) releases the HELD events of its categories itself, with I6's rules: never CRITICAL, never a shadow copy, never past the envelope's own `delivery.expiresAt` (24 h for contact notices and missed calls, 72 h for voicemail and recordings), and only events received in the last 7 days. The run reports `heldReleased` and `heldLeft` (counts only). Events it leaves HELD are stale and stay undelivered; they are not deleted. A released item appears in the inbox at the time it is processed.

## 7. Dead letters and replay

I5 lists FAILED deliveries, REJECTED events and events parked after 30 attempts. I6 replays HELD events and failed PUSH deliveries. It refuses expired items, superseded ones, CRITICAL items, and email whose content was erased at its final state. Every replay is logged with the operator identity.

## 8. Email provider

The service uses its own provider configuration (`NOTIFICATIONS_EMAIL_PROVIDER`, file-based secrets); the Meeting Service keeps its own as the fallback path.

**Egress.** The replicas live on `cfmeet-core-private`, which is `--internal`. Their providers (Resend, the browser push services) are reached through `cfmeet-notifications-egress`, a non-internal bridge with inter-container traffic disabled. `activate-notifications-email` and `activate-notifications-push` attach it and refuse to finish unless every replica gets an HTTP answer from its providers.

**Symptom: `last_error_class = NETWORK` on every attempt.** The egress network is missing. Re-run the two activations. A CRITICAL message that was leased is never failed over automatically, so if this happens, move the CRITICAL categories to `LEGACY_ONLY` first (`activate-notification-fence`). `diagnose` (`notificationDiagnostics`) reports, as counts only:
- the email ledger by status and last error class;
- the Meeting CRITICAL outbox with lease, acknowledgement and completion flags. A breaker opens after 5 consecutive temporary failures for 60 s. Check the provider status page, then the ledger: `SELECT status, last_error_class, count(*) FROM notif_email_messages GROUP BY 1, 2`. Never print addresses, subjects or bodies (they are sealed).

## 9. Key rotation

| Key | Rotation |
|---|---|
| Data master key | add the new key as `NOTIFICATIONS_DATA_KEY_FILE` and keep the old one as `NOTIFICATIONS_DATA_KEY_PREVIOUS_FILE` (both replicas, one recreate each); new seals use the new key, old seals still open |
| HPKE key (`NOTIFICATIONS_HPKE_*`) | publish the new public key to producers first, then mount the new private key; keep the previous one until no envelope sealed to it can still arrive |
| Service signing key | the standard coordinated-release identity procedure |
| Provider credentials | replace the 0400 file through the activation operation; never in environment variables |

## 10. Retention and erasure

W5 (hourly): inbox items after `expires_at` (90 days), processed or rejected events after 30 days, notifications without pending work after 30 days, shadow observations after 30 days, the preference change log after 7 days, fired schedules after 7 days, disabled push endpoints after 30 days. Email: addresses erased 30 days after the final state, rows deleted after 60 days, non-final rows expired at `expires_at`. Account erasure removes inbox, endpoints, preferences and notifications, and erases ledger addresses (`workers::erase_account`).

## 11. Activations (coordinated release)

Each is a separate run of `cfmeet-coordinated-release.yml` on the current `main` head, journaled and reversible with `rollback`. None of them delivers anything: delivery is governed only by `notif_routing`.

| Operation | Effect | Checks |
|---|---|---|
| `activate-notifications-owners` | grants X1/X2 (Meeting, with `NOTIFICATION_OWNER_API_ENABLED=true`), X3 (Messaging), X4 (Communications), X5 (Organizations) to `cfmeet.notifications`; sets `NOTIFICATIONS_AUTHORITIES`, the owner base URLs and the first-party scope on both replicas | each owner route answers 401 to an unsigned probe before (non-flagged owners) and after |
| `activate-notifications-relay` | grants I2 `notifications-user.forward` to the Meeting Service on both replicas; sets `CF_NOTIFICATIONS_BASE_URL` on the Meeting Service, which turns on U1-U9 under `/api/v2/notifications` (owner-approved 2026-11-16) | I2 answers 401 unsigned and the Meeting relay answers 401 without a session, before and after |
| `activate-notifications-email` | makes Resend the service's email provider on both replicas with the credentials already on the VPS: service-owned 0400 copies of `/opt/cfmeet-core/secrets/resend-api-key` and `resend-webhook-secret`, mounted read-only (`NOTIFICATIONS_RESEND_API_KEY_FILE`, `NOTIFICATIONS_RESEND_WEBHOOK_SECRET_FILE`); sender `CF Meet <no-reply@cfmeet.com>`. The Resend account keeps its one webhook on the Meeting Service, which hands events for messages it did not send to the service's Svix-verified callback (503 to the provider when that hop fails, so it retries). Nothing is sent until an EMAIL routing row is ACTIVE | needs E1 (`activate-resend-email`); the callback answers 404 or 401 before and 401 unsigned after |
| `activate-messaging-publications` | grants I1 `notifications-event.publish` to Messaging on both replicas; sets `CF_NOTIFICATIONS_BASE_URL` on Messaging, which starts its publication relay (nothing is published while `MESSAGE_WAKEUPS` is `LEGACY_ONLY`) | I1 answers 401 unsigned before and after; Messaging reports `routingProtocol` and has `messaging_notification_publications` |
| `activate-notification-fence` | changes one producer routing-fence row (`confirm`: `<service>-<category>-<mode>`, e.g. `messaging-message-wakeups-shadow`); modes `legacy-only`, `shadow`, `fenced-old`, `fenced-new` (canary 100: the legacy path is skipped for every new decision) | only for categories whose producer records decisions (`FENCE_ROUTABLE`), with `routingProtocol` and the producer's publication activation; `fenced-new` needs SHADOW or FENCED_OLD first and every Notification Service routing row the category is delivered on `ACTIVE` (a NEW decision is never left undelivered); any mode may return to `legacy-only`; `rollback` restores the previous mode (decisions already made stay sticky) |
| `activate-notifications-routing` | changes one `notif_routing` row (`confirm`: `<category>-<channel>-<mode>`), or, with `confirm` `user-facing-in-app-<mode>` / `user-facing-push-<mode>`, the IN_APP (or PUSH) row of every user-facing category in one run: `CONTACT_REQUESTS` (requests, accepts, direct adds), `GROUP_INVITATIONS` (added to a group), `MISSED_CALLS`, `VOICEMAIL` (voicemail, recordings), `MEETING_NOTIFICATION`, `ORGANIZATION_UPDATES` (invitations, role / membership / organization changes), `DEVELOPER_PLATFORM` (IN_APP only). An `active` bundle then releases the still-relevant HELD events of those categories (§6). EMAIL, OPERATOR and the CRITICAL categories are never bundled; `MESSAGE_WAKEUPS` and `CALL_INVITATIONS` keep their own sequences | the gates of §2 and Document 6 N14, for every row before any changes: CRITICAL `active` refused; EMAIL `active` needs a provider and E1; social `active` needs the owner APIs; a PUSH `active` bundle needs `activate-notifications-push`. `rollback` restores every row's previous mode |

`diagnose` (read-only) also emits `notificationDiagnostics`, counts only:
- per producer: fence modes, decisions by route and reason, publications by shadow, state, outcome and error code;
- in the service: inbox rows by producer, shadow and state, shadow observations, deliveries, and the number of routing rows not `OFF`.

It never reads ids, envelopes or recipients. Use it to evidence a SHADOW phase.

The U1-U9 routes are classified `EXCLUDED` / `FIRST_PARTY_APP_NOTIFICATIONS` in `infra/api-gateway/public-routes.json`: they are served on the Meeting Service ingress to the CF Meet apps' own signed-in users, never routed as a customer API. Without `CF_NOTIFICATIONS_BASE_URL` every U route answers 503 after the 401 session check.

### Communications Hub publications (`CONTACT_REQUESTS`)

The Hub (`services/communications`) publishes `communications.connection.requested` (to the addressee of a new contact request) and `communications.connection.connected` (to the requester when the addressee accepts) through its own outbox `communication_notification_publications` (migration history `communications_notification_publications_seaql_migrations`). The Hub has no legacy notification path for these events, so there is no producer routing fence: each envelope carries its own route decision (`routingVersion` 1) and delivery is governed only by `notif_routing` for `CONTACT_REQUESTS` (HELD while `OFF`; the service re-checks blocks and privacy through X4).

| Hub variable | Effect |
|---|---|
| `CF_NOTIFICATIONS_BASE_URL` | set: publications are written in the relationship transaction and relayed to I1, signed as `cfmeet.communications` for audience `cfmeet.notifications`. Unset (default): nothing is written or sent; the Hub behaves exactly as before |

Activation needs the grant on both Notification Service replicas: receiver `cfmeet.notifications`, issuer `cfmeet.communications`, permission `notifications-event.publish`, then `CF_NOTIFICATIONS_BASE_URL` on the Hub. No coordinated-release operation does this yet. Rows: `PENDING` → `ACKED` (ACCEPTED / DUPLICATE / HELD), `DEAD` (REJECTED or a contract 4xx), `SKIPPED` (no CF Meet account, a block, a request or connection that no longer holds), `EXPIRED` (not posted within 24 h); settled rows are purged after 30 days. Retries back off from 2 s to at most 5 min.

### Messaging "added to a group" notices (`GROUP_INVITATIONS`)

Messaging (`services/messaging/src/member_notifications.rs`) publishes catalogue #14 `messaging.member.invited` (the bell's "Added to a group", action `OPEN_CONVERSATION`) to the CF Meet account of a person **someone else** added to, or invited into, a multi-party conversation. Self-joins, accepted invitations, organization team snapshots and DIRECT conversations are not announced. Before this, Messaging published no member notice at all, so `GROUP_INVITATIONS` never received an event. There is no legacy path, so there is no routing fence (`routingVersion` 1): delivery is governed only by `notif_routing` for `GROUP_INVITATIONS`, and the service re-checks blocks and holds through X4 (`GROUP_INVITATION`) and the `groupInvitations` preference.

Rows live in `messaging_member_notifications`, a separate migration history (`messaging_member_notifications_seaql_migrations`, applied by `messaging-migration up` after the MLS self-join history). The previous image never reads it, so a rollback still starts. They are written in the membership transaction only while `CF_NOTIFICATIONS_BASE_URL` is set on Messaging, which `activate-messaging-publications` sets; the same I1 grant covers them. The envelope is ids only (`conversationId`, `eventSequence`) and is built at the first relay attempt from the same Platform Access account mapping as the wake-ups. A member who is no longer ACTIVE / INVITED, a conversation that is no longer ACTIVE, or a principal without a CF Meet account is `SKIPPED`. The rest of the lifecycle matches the other producers: `PENDING` → `ACKED` / `DEAD` / `SKIPPED` / `EXPIRED` (24 h), and settled rows are purged after 30 days (`messaging_member_notifications_total`).

### Calling publications (`MISSED_CALLS`, `VOICEMAIL`)

Calling (`services/calling`) publishes catalogue #15-#17 through its own outbox `calling_notification_publications` (migration history `calling_notification_publications_seaql_migrations`, applied by `calling-migration up` after the main history; the previous image's readiness is unchanged). Before this Calling notified no user of these facts (missed calls were derived for the call history only; voicemail / recording facts reach only customer webhooks and the Developer Platform), so there is no legacy path and no routing fence (`routingVersion` 1).

| Event | Written in the transaction of | Recipients |
|---|---|---|
| `calling.call.participant-missed` (`callId`, `callKind`) | the Call's terminal transition | each RECIPIENT participant whose outcome is MISSED (the call-history derivation; never answered, declined or answered elsewhere) |
| `calling.voicemail.ready` (`callId`, `voiceMessageId`) | the message becoming READY | the mailbox owners (principal, extension member, queue agents, ring-group principal members) |
| `calling.recording.ready` (`callId`, `recordingId`) | the recording becoming READY | the principals who took part (initiator, recipients who answered or connected) |

Recipients are CF Meet accounts only: `NATIVE_CF_USER` principals of a FIRST_PARTY tenant whose reference is `cfmeet-user:<uuid>`. Nothing is written for an EXTERNAL tenant; inside a FIRST_PARTY tenant a principal without an account or an inactive one is `SKIPPED` (`RECIPIENT_NOT_CF_MEET_ACCOUNT`, `RECIPIENT_INACTIVE`). Envelopes are ids only (no name, number, label, object key or media); missed calls carry a coalesce key per caller and 10-minute window.

| Calling variable | Effect |
|---|---|
| `CF_NOTIFICATIONS_BASE_URL` | set: publications are written and relayed to I1 every second, signed as `cfmeet.calling` for audience `cfmeet.notifications`; the service refuses to start unless the notification-publications history is applied. Unset (default): nothing is written or sent; Calling behaves exactly as before |

Activation needs the grant on both Notification Service replicas: receiver `cfmeet.notifications`, issuer `cfmeet.calling`, permission `notifications-event.publish`, then `CF_NOTIFICATIONS_BASE_URL` on Calling (after a Calling release that ran `calling-migration up`). The release tooling has no Calling publication activation yet. Rows: `PENDING` → `ACKED` (ACCEPTED / DUPLICATE / HELD), `DEAD` (REJECTED or a contract 4xx), `SKIPPED` (no CF Meet account, an inactive recipient, a message / recording no longer READY), `EXPIRED` (not posted within 24 h for missed calls, 72 h for voicemail / recordings); settled rows are purged after 30 days. Retries back off from 2 s to at most 5 min. Calling's incoming-call push goes to I3 only with `CALLING_PUSH_TRANSPORT=notifications` (see *Gap-audit closure* below); by default (`none`) ringing reaches devices through Calling's signaling and the recovery API.

### Gap-audit closure (17 November audit: C1, C2, S1, S15, M12, G19)

Everything below ships **off**. Nothing changes in production until an operator runs the steps in the table. No step touches SMTP, DNS or the email provider. The email channel of the meeting notices stays as it is.

| Item | What the service does now | Producer switch (default) | Operator steps |
|---|---|---|---|
| **C1** incoming-call push | I3 rings a first-party recipient's endpoints with `{v, type: call}`: APNs priority 10 / VoIP for `APNS_VOIP`, FCM `HIGH`, Web Push urgency high; the TTL is what is left of the invitation (≤ 300 s); collapse id `cfmeet-call`. **I3c** `POST /internal/v1/push/call-invitations/cancellations` (contract 1.3.0, same permission) sends `{v, type: call-cancel}` with the same collapse id and a 60 s TTL when the ring ends unanswered on that endpoint (answered elsewhere, cancelled, expired, the Call ended). It never sends a VoIP push for a cancellation (`SKIPPED_VOIP`). No number, name or call id is accepted or forwarded | Calling `CALLING_PUSH_TRANSPORT` (`none`) | 1. grant receiver `cfmeet.notifications`, issuer `cfmeet.calling`, permission `notifications-call-push.dispatch` on both Notification Service replicas; 2. set `CF_NOTIFICATIONS_BASE_URL` and `CALLING_PUSH_TRANSPORT=notifications` on Calling; 3. `activate-notifications-routing` `CALL_INVITATIONS-PUSH-active`. Until 3, I3 / I3c answer `NOT_CONFIGURED` and send nothing |
| **C2** missed call, voicemail, recording | already routed (`MISSED_CALLS`, `VOICEMAIL`: in-app + push). Recipients: the callee; the mailbox owners; the participants who connected (a subset of Calling's retrieval rule, `was_participant`) | Calling `CF_NOTIFICATIONS_BASE_URL` (unset) | grant `notifications-event.publish` to `cfmeet.calling`; set `CF_NOTIFICATIONS_BASE_URL` on Calling; `activate-notifications-routing` `MISSED_CALLS-IN_APP/PUSH-active`, `VOICEMAIL-IN_APP/PUSH-active` |
| **S1** organization chats | an organization conversation's wake-up is routed like a personal one (`MESSAGE_WAKEUPS`); the per-conversation mute (`notif_conversation_preferences`, by conversation id) applies to it; payloads are ids only (never text, so nothing of an E2EE conversation) | Messaging `MESSAGING_ORGANIZATION_WAKEUPS` (`off`); needs `CF_NOTIFICATIONS_BASE_URL`, `CF_COMMUNICATIONS_PERSONAL_*` and the notification-fence history, or Messaging refuses to start | after `activate-messaging-publications` and `MESSAGE_WAKEUPS-PUSH-active`, set `MESSAGING_ORGANIZATION_WAKEUPS=on` on Messaging. Then an organization chat's wake-up is always routed `NEW` (published, never legacy jobs) in every fence mode. Audiences above 40 other members stay on the legacy path, which reaches nobody in an organization scope (see the report) |
| **S15** Hub switches | `incomingCalls` (and the push channel of `INCOMING_CALLS`) gates I3 / I3c (`PREFERENCE_OFF`); `missedCalls`, `contactRequests`, `groupInvitations` gate their categories (precedence rule 7: no push or email, silent inbox item). The values come from the N10 copy of the Hub rows | — | the N10 preference copy must be running (or the facade switched to `SERVICE`) for Hub changes to reach the service |
| **M12** meeting notices | `meeting-service.conference.{scheduled→reminder, rescheduled, cancelled}` route to email + **in-app + push**, under `meetingReminders` / `meetingInvitations` and the channel matrix; a reminder push expires at the meeting start | — | `activate-notifications-routing` `MEETING_NOTIFICATION-IN_APP-active` and `MEETING_NOTIFICATION-PUSH-active` (the rows are seeded `OFF`; no migration) |
| **G19** organization archived / reactivated | catalogue #39 / #40 (event contract 1.4.0, common 1.3.0): `ORGANIZATION_UPDATES`, in-app + push, templates `org.organization.archived` / `org.organization.reactivated`, action `OPEN_ORGANIZATION` | Organizations `ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED` (`false`) | Organizations owner: move `services/organizations/migration/pending/m20261118_000007_lifecycle_notification_types.rs` into the migration history, release it, then turn the switch on (Organizations refuses to start with it on before that migration). Requires `ORGANIZATION_UPDATES-IN_APP/PUSH-active` |

App-side work for C1 (not part of the backend):
- **iOS**: register a PushKit VoIP token (`APNS_VOIP`) through U8. On a `{type: call}` VoIP push, report the call to CallKit right away (PushKit requires this), then recover the invitation from Calling (`GET /v1/calling/invitations`) with the device's own signaling credential. Cancellations never arrive as VoIP pushes. The app ends the CallKit call when signaling or recovery shows the invitation ended. A regular APNs endpoint receives `call-cancel` and removes any ringing notification.
- **Android**: handle the FCM high-priority data message `{type: call}` with a full-screen-intent / `CallStyle` notification and recover the invitation. On `{type: call-cancel}` (same collapse key), dismiss the ringing UI and re-check signaling.
- **Web**: the service worker shows the ring notification for `{type: call}` (topic `cfmeet-call`). On `{type: call-cancel}` it closes that notification (same topic), then re-checks the invitation.
