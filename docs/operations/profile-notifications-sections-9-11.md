# Personal profile and notifications — Blueprint v1.0 Sections 9 and 11

**Scope:** `services/meeting-service` (account authority, meeting notifications), a small
read-only addition to `services/communications` (preference read), one service-auth
permission. **Nothing is enabled by this change:** every notification email is off until
`NOTIFICATIONS_EMAIL_ENABLED=true`, the v1 status spelling is unchanged until
`ACCOUNT_STATUS_FORMAT=canonical`, phone verification and push stay provider-blocked.
Not deployed.

Code: `src/services/account_profile.rs`, `src/handlers/v2/account_profile.rs`,
`src/services/notifications.rs`, `src/config/notifications.rs`,
`src/templates/email/meeting.rs`; Hub `src/preferences.rs`. Tests:
`tests/profile_notifications.rs`, `tests/organizations_authority.rs`
(`s11_organization_invitation_email_from_the_authoritative_answer`), Hub
`tests/foundation.rs` (`meeting_notification_opt_outs_are_listed_for_the_account_only`), unit
tests in each module.

## 1. Section 9 — personal account profile

### Decisions

* **One writer.** Meeting Service (the account authority) owns the profile: `username`,
  `display_name`, `avatar_url`, `timezone`, `locale` live on `users`. The Communications Hub
  `communication_profiles.display_name` / `avatar_reference` stay a **projection**: the
  account relay already sends the account's display name (now the profile display name,
  falling back to the username) and now also the avatar reference on every
  account-authenticated Hub request, and the Hub refreshes its row when they differ
  (`ensure_profile`). The Hub backfill also uses the display name. No code writes the
  profile twice; contacts see a change after the user's next Hub request.
* **Display name is what people see.** `users::Model::presentation_name()` is used for the
  registered actor (participant names, realtime), the pre-join organizer, the
  Developer Platform session bridge and the Hub. The username stays the unique handle.
* **One email-verification rule.** `users::Model::email_verified()` replaces four copies:
  Google sign-in, a recorded `email_verified_at`, or an active account where verification is
  required to sign in. `email_verified_at` is written by the verification code and every
  Google sign-in path; it is backfilled only from the Section 2 verified Google binding
  (`account_security_state.google_linked_at`). Local accounts are not backfilled because
  `status = active` can also mean an administrator activated them. Effect: a user who
  verified with the code now counts as verified even where `REQUIRE_EMAIL_VERIFICATION` is
  off (only from this release on).
* **`deleted_at` is not added.** Deletion semantics belong to owner decision D-6 (ACC-011 /
  ACC-013); deactivation is reversible and keeps using `status`. The Google subject binding
  already exists (Section 2) and is not duplicated.
* **Avatar is a reference.** An `https://` URL (≤512 characters, no spaces or `@`, the Hub's
  existing rule). Upload through object storage is not part of this section (Section 17 /
  client work); clients must load avatars with a referrer-free, cookie-free request.

### Validation (one rule set for every writer)

| Field | Rule |
|---|---|
| username | trimmed, 3–50 of `[A-Za-z0-9_]` (registration rule; v1 used to check length before trimming and accepted any character), unique (`409 USERNAME_TAKEN` / v1 `409`) |
| displayName | trimmed, 1–64 characters, no control or invisible formatting characters (bidi overrides, zero-width) |
| avatarUrl | `https://…`, ≤512, no whitespace / `@` |
| timezone | IANA zone known to `chrono-tz` |
| locale | BCP 47 (`en`, `en-GB`, `es-419`, `zh-Hant-TW`), normalized; `_` accepted as `-` |

Database CHECK constraints mirror these rules. Profile changes write a `PROFILE_UPDATED`
account security event (field names only, never values).

### API

| Method | Path | Change |
|---|---|---|
| GET | `/api/v2/me/profile` | NEW — profile, canonical status, email verification, `phoneVerification: {status: PROVIDER_BLOCKED, required: false}` |
| PATCH | `/api/v2/me/profile` | NEW — partial update; `null` clears an optional field; unknown fields refused |
| PATCH | `/api/v1/users/me` | CHANGED — username validated with the registration rule after trimming |
| GET | `/api/v1/users/me`, login, register, Google sign-in/exchange | CHANGED — one status rendering (ACC-022) |

Both new operations are `EXCLUDED / FIRST_PARTY_APP_ACCOUNT` in the gateway manifest (like
`/api/v2/me/sessions`). Meeting v2 operations 181 → 183.

### ACC-022 — status contract

The database, the admin API and `/api/v2/me/profile` use `pending_verification`, `active`,
`inactive`, `suspended`. The v1 sign-in responses used `format!("{:?}")` and returned
`pendingverification`; the live web client compares against that spelling
(`LoginModal.tsx`, `useGoogleCallback.ts`) and mobile compares against `pending` (never
matched). All v1 responses now go through one function controlled by
`ACCOUNT_STATUS_FORMAT`: `legacy` (default, unchanged wire value) or `canonical`. The OpenAPI
descriptions state both. **Switch:** after the web and mobile clients accept
`pending_verification` (Section 17 handoff: compare against both spellings), set
`ACCOUNT_STATUS_FORMAT=canonical`.

### Phone verification — EXTERNAL_BLOCKED

No clean seam exists: the SMS Gateway sends customer / bridge SMS for provisioned numbers
(sender authorization, privacy relay bindings, deterministic provider only) and has no
verification-code API, and no provider is approved. Design (not built): Meeting Service
issues a 6-digit code (OTP store as for email, 10 minutes, 5 attempts, per-account and
per-number limits), stores `phone_e164` / `phone_verified_at` in a new additive history,
sends through a new SMS Gateway permission `sms-verification.send` (Meeting Service only,
template-only body, no customer number pool), off by default
(`PHONE_VERIFICATION_ENABLED=false`), reported `PROVIDER_BLOCKED` until a provider is
certified. Never required for existing accounts (ADR 0056 §8).

## 2. Section 11 — notifications

### What is sent (email only; D-4)

| Notification | Trigger | Delivery |
|---|---|---|
| Meeting invitation | `POST /api/v2/meeting-spaces/{id}/invitations` with `inviteeEmail` and `sendEmail: true` (signed-in users only; machine callers get `VALIDATION_ERROR`) | synchronous after commit; link `{APP_URL}/invite#<token>` (fragment: never sent to a server; the web invitation page already reads it); `emailDelivery` in the response |
| Organization invitation | `POST /api/v2/organizations/{id}/invitations` with `sendEmail: true` (authoritative mode) | the relay strips `sendEmail` (the Organizations contract refuses unknown fields), sends the signed call, and emails the token from the authoritative answer; link `{APP_URL}/organizations/invitations#<token>` (web page: Section 17). An idempotent replay rotates the token and emails the new link. Meeting stores no organization data (ADR 0056) |
| Reschedule notice | `POST /api/v2/conferences/{id}/reschedule` | durable job written in the reschedule transaction |
| Cancellation notice | `POST /api/v2/conferences/{id}/cancel` of a scheduled occurrence | durable job in the cancel transaction (system expiry of stale occurrences is not announced) |
| Reminder | worker, once per conference and scheduled start, `NOTIFICATIONS_REMINDER_LEAD_MINUTES` before the start | durable job; a reschedule produces a new reminder; materialized recurring occurrences are covered (the Section 10 materializer keeps 30 days of rows) |

`emailDelivery`: `DISABLED` (switched off), `ACCEPTED` (queued, subject to the recipient's
preferences — the same answer whether or not the address has an account, so the inviter
learns nothing about it), `FAILED` (temporary; the invitation exists, share the link).
Requested invitation emails are limited per account
(`NOTIFICATIONS_INVITE_EMAILS_PER_HOUR`, `429 RATE_LIMITED` before anything is created).

Not covered (documented gaps): series edits and `PUT …/recurrence/exceptions`
cancellations/reschedules, legacy v1 meetings, ICS attachments (the email links to the
meeting; `calendar.ics` needs a bearer token), localized templates (English only; the
profile locale is stored for a later template set).

### Recipients and content

Resolved at delivery: the space owner, active members (account or email-bound), active
unexhausted email-bound invitations for the space or that conference, and REGISTERED webinar
attendees with an account — deduplicated by address, never the person who made the change,
never inactive / suspended / guest accounts, at most `NOTIFICATIONS_MAX_RECIPIENTS`.
Unverified addresses typed by anonymous registrants are never emailed. Times are shown in the
recipient's profile time zone (Section 9), otherwise the meeting's zone. HEALTHCARE /
HIGH_SECURITY / HEALTHCARE meeting type / appointment titles become "CF Meet meeting" for
everyone (stricter than the calendar file, which shows managers the title). Every
interpolated value is HTML-escaped; control characters are dropped from subjects and text.
No passcode, provider id or other secret is ever included; only invitation emails contain a
token, and only in the fragment.

### Preferences (ACC-020)

The recipient's Hub preference `meetingInvitations` governs every meeting email
(invitations, changes, reminders). Meeting Service reads it with the new private Hub route
`POST /internal/v1/communications/notification-preferences/resolve` (permission
`communications-preferences.read`, grantable only to the Meeting Service, also checked by
caller identity in the handler; account references in, opted-out references out, ≤100 per
call; Communications contract 1.2.0 → 1.3.0). Without Communications configured no
preference exists. When Communications is configured but unreachable a job is retried with
backoff (never sent against an unknown preference) and an invitation email reports
`FAILED`. Organization invitations are transactional (explicitly requested by an
administrator) and no Hub category covers them. Suppressed recipients are recorded
(`SUPPRESSED`) and counted.

### Durability and idempotency

`meeting_notifications` holds one job per notice with a unique `dedupe_key`
(`rescheduled:<conference>:<version>`, `cancelled:<conference>`,
`reminder:<conference>:<start epoch>`), so retries, restarts and concurrent workers create
it once. Jobs are leased (`FOR UPDATE SKIP LOCKED`, 120 s) and settled only with the lease.
`meeting_notification_recipients` records each recipient (SHA-256 of the lowercased address;
no address is stored) **before** the email is queued, so a job is delivered at most once per
recipient; when the queue refuses an email the entry is released and the job retried
(backoff 2^n s ≤ 15 min, FAILED after 8 attempts). A notice is sent only while still true
(a reminder for a moved, started or cancelled occurrence, or a reschedule that was superseded,
is SKIPPED). Finished jobs are purged after 30 days.

Transport: the existing `EmailService` queue (Redis `email:queue`) and `EmailWorker`
(`SMTP_*`). If notification email is enabled without SMTP, emails stay queued (warning at
start-up). Tests use the in-process `RecordingMail` transport only.

### Push — EXTERNAL_BLOCKED (CAL-003, MSG-011, ACC-020 push part)

What exists, all disabled: Messaging's APNs / FCM HTTP v1 / Web Push adapters with
invalid-handle disabling and retry (`services/messaging/src/realtime/push*.rs`, certified
against local fakes only; `MESSAGING_PUSH_PROVIDER=none` default; activated per platform by
`MESSAGING_PUSH_APNS_KEY_FILE`/`_KEY_ID`/`_TEAM_ID`/`_TOPIC`,
`MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE`, `MESSAGING_PUSH_WEB_VAPID_KEY_FILE`/`_SUBJECT`;
unset adapters end jobs `NOT_CONFIGURED`); device push registration through the Hub
(`PUT /devices/{id}/push` → Messaging); Calling's `NoPushTransport`
(`services/calling/src/main.rs`) — the Hub reports "mobile push transport NOT_CONFIGURED".
This section adds no push code and does not touch device registries (Section 14 owns
device/revocation work). Activation needs: Apple and Google developer accounts and keys, a
VAPID key pair, a Calling `PushDispatcher` reusing Messaging's adapters (APNs VoIP / PushKit
for iOS), a Meeting reminder push channel through the same registry, client registration
(service worker, `expo-notifications`) and physical-device certification.

## 3. Migration

NEW additive history `profile_notifications_migrations` (binary
`profile-notifications-migration`, `ProfileNotificationsMigrator`):

1. `m20261108_000001_account_profile` — nullable `users` columns `display_name`,
   `avatar_url`, `timezone`, `locale`, `email_verified_at`, `profile_updated_at` (no default:
   catalog-only change), CHECK constraints added `NOT VALID` then validated, Google backfill.
2. `m20261108_000002_meeting_notifications` — `meeting_notifications`,
   `meeting_notification_recipients`.

The previous image selects and inserts explicit column lists and never reads the new tables,
so it keeps starting after `up` (rollback = previous image; `profile-notifications-migration
down` drops the columns and tables if ever required). The new image refuses to start without
the history (`profile_notifications_ready`). Wired into: Dockerfile (build + copy),
`infra/deployment/backend_runtime.py`, `infra/calling/production/communications_release.py`
(after meetings calendar; `binaries[1:]` semantics kept), `certification/src/stack.rs` and
`personal_stack.rs`, `infra/deployment/backup/catalog.conf` (+ `tests/unit.sh`),
`docs/operations/backup-and-restore.md`, `tests/common/mod.rs` and the migration test suites.

## 4. Configuration (all defaults keep today's behaviour)

| Variable | Default | Effect |
|---|---|---|
| `NOTIFICATIONS_EMAIL_ENABLED` | `false` | master switch: invitation emails on request, notices, reminders, worker |
| `NOTIFICATIONS_REMINDER_LEAD_MINUTES` | `15` (0 = no reminders, ≤1440) | reminder lead time |
| `NOTIFICATIONS_MAX_RECIPIENTS` | `200` (1–1000) | recipients per notice |
| `NOTIFICATIONS_POLL_SECS` | `30` (≥5) | worker cadence |
| `NOTIFICATIONS_INVITE_EMAILS_PER_HOUR` | `50` (1–1000) | requested invitation emails per account and hour |
| `ACCOUNT_STATUS_FORMAT` | `legacy` | `canonical` returns `pending_verification` on v1 |

## 5. Release steps (owner-gated; production is not changed by this branch)

1. Encrypted pre-migration snapshot and restore drill (backup runbook).
2. Migrations in order: `migration up` → `account-security-migration up` →
   `meetings-calendar-migration up` → `profile-notifications-migration up` →
   (`account-communications-migration up` where Communications is configured). The release
   tooling does this.
3. Deploy the image with the defaults above (no behaviour change except: v1 username edits
   follow the registration rule; display names appear where set; code-verified users count as
   verified).
4. Communications: deploy the Hub image with the new route; when notifications are to use Hub
   preferences add `communications-preferences.read` to the Meeting Service grants of the
   Hub's trust bundle (`communications_release.py`, `incoming['meeting-service']`) —
   deliberately **not** changed by this branch. Without the grant, a notice with any
   account-holder recipient waits and ends FAILED and invitation emails to account holders
   report FAILED (fail closed); only notices and invitations addressed purely to addresses
   without an account are sent.
5. Enable `NOTIFICATIONS_EMAIL_ENABLED=true` only after confirming SMTP sender reputation and
   with the owner's approval. **Never** test against production SMTP with real addresses.
6. `ACCOUNT_STATUS_FORMAT=canonical` after the clients accept both spellings.

## 6. Findings

| Finding | Disposition |
|---|---|
| ACC-021 | FIXED (backend): profile fields, one validation rule set, v2 read/update, Hub projection. Avatar upload: Section 17 / object storage follow-up |
| ACC-022 | FIXED (backend, switch gated): one rendering, canonical on v2/admin, `ACCOUNT_STATUS_FORMAT` for v1; client constants in the Section 17 handoff |
| DATA-016 | FIXED: `email_verified_at`, `timezone`, `locale`, `avatar_url`, `display_name` (+ safe backfill); Google subject from Section 2; `deleted_at` DEFERRED_BY_OWNER (D-6) |
| Phone verification | EXTERNAL_BLOCKED (design above) |
| MTG-006 | FIXED (backend, disabled by default): invitation emails, reschedule/cancel notices, reminders, organization invitation emails, preferences, idempotency. Gaps listed in §2 |
| ACC-020 | PARTIAL: the meeting preference is enforced for every channel built here; push registration/transport EXTERNAL_BLOCKED; other categories (calls, contact requests) belong to Calling / Hub (Section 14) |
| CAL-003, MSG-011 | EXTERNAL_BLOCKED (inventory and activation inputs above) |
