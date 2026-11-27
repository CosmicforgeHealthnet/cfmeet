# 01 — Meeting Service: notification behaviour inventory (evidence grade)

Audit scope: `services/meeting-service` (Rust/axum/SeaORM), its migrations, the OpenAPI export
`packages/contracts/openapi/meeting-service.json`, and the operations docs under `docs/operations/`.
Method: read-only source reading and grep. No repo file was modified, no git state changed, and no
production endpoint was called. I did not probe `/health`: the handler `handlers/root.rs::health_check`
(L92-101) returns only a static `{status, timestamp, service, version}`, so it carries no notification
evidence. No secret values are reproduced here.

Paths below are relative to `services/meeting-service/` unless they start with `docs/`, `infra/` or
`packages/`.

## 0. Classification vocabulary used

| Label | Meaning in this report |
|---|---|
| implemented | Code exists and is wired into a reachable code path |
| configured | Requires env/config. The repo shows the config is set in some environment (release tooling or ops docs) |
| activated | Docs or release tooling claim the flag or transport is ON in production (date and run cited) |
| production-tested | A dated ops report with a workflow run id shows the behaviour exercised in production |
| disabled | Gated off by default, with no evidence that it is on |
| broken | Evidence that it cannot work as deployed (missing grant, rejected transport, etc.) |
| deprecated | Explicitly marked deprecated or superseded in code or docs |
| absent | No code path exists |

**Headline production facts (from docs, not code):**
- 2026-11-12, `docs/operations/CFMEET_FINAL_PRODUCTION_COMPLETION.md` §1/§2/§6/§16:
  - The durable ledger was deployed (release run 43309857124, image `f2f96934…`). Post-release diagnose 43317264046 showed the ledger present with all counts 0.
  - `EMAIL_PROVIDER` is unset, so implicit SMTP is used (`smtp.gmail.com:587`).
  - Resend is NOT activated: "`activate-resend-email` was not run".
  - The report states: "OTP delivery is not yet recovered … Status: `IMPLEMENTED_PENDING_CONFIGURATION`".
  - Over a 168h log window it counted "0 SMTP `535`/BadCredentials, 0 drops after retries and **0 SMTP sends**".
  - `docs/operations/resend-email-owner-handoff.md` L3 calls the Gmail account "the rejected Gmail SMTP account".
- 2026-11-13, `docs/operations/CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md`:
  - L76 lists meeting-service with `NOTIFICATIONS_EMAIL_ENABLED=true` (SMTP).
  - L270: "Resend email: not activated; SMTP is in use."
  - `docs/operations/CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md` L248 says the same (`NOTIFICATIONS_EMAIL_ENABLED=true`, `EMAIL_PROVIDER=smtp`). L313 says invitations by email are "IMPLEMENTED_PENDING_ACTIVATION … email via SMTP".
- Repo search: no doc records a production run that delivered a meeting notification, reminder, invitation email or OTP email.

---

## 1. Email pipeline (unified email ledger)

### 1.1 Architecture

`src/services/email_delivery.rs` module docs (L1-28):
`business event → EmailService::enqueue → email_deliveries (QUEUED) → EmailWorker (lease FOR UPDATE SKIP LOCKED) → EmailProvider (Resend | SMTP | none) → provider webhooks (Svix) → DELIVERED/BOUNCED/COMPLAINED/FAILED`.

The module claims at L12: "The Meeting Service is the single email authority of CF Meet (no other service sends email)". This was checked by grep:
- No `lettre`, Resend or SMTP use was found in any other `services/*`.
- Organizations, Calling, Messaging, Communications and Developer Platform do not call meeting-service email endpoints.
- The only external caller of an email endpoint is the operator script `infra/calling/production/developer_access.py::notify_applicant` (L150-165). It reaches it over loopback `127.0.0.1:18081` with `INTERNAL_API_TOKEN`.

Entry points that write to the ledger:
- `src/services/email.rs::EmailService` (L26-192): `enqueue_template` L50, `send_verification_email` L77, `send_password_reset_email` L102, `send_welcome_email` L127, `send_password_changed_email` L148, `send_notification_email` L172.
- `src/services/notifications.rs` `impl NotificationMail for EmailService` (L103-122). It calls `email_delivery::enqueue` directly.
- `src/services/account_devices.rs::send_email_code` (L1494-1571). It calls `email_delivery::enqueue` inside the business transaction.
- `src/handlers/email.rs::acceptance_email` (L114) and `developer_access_decision` (L195).
- `email_delivery::import_legacy_redis_queue` (L896-926). This is a one-time migration of the old Redis list at startup (`main.rs` L240-250).

### 1.2 Tables (migration `migration/src/m20261112_000001_email_delivery_and_presence.rs`, history `lifecycle_delivery_migrations`, binary `lifecycle-delivery-migration`; readiness check `migration::lifecycle_delivery_ready`, enforced by a panic in `main.rs` L234-239)

**`email_deliveries`** (L23-60)

| Column | Type | Notes |
|---|---|---|
| id | UUID PK (`pk_email_deliveries`) | Also sent to Resend as `Idempotency-Key` |
| category | VARCHAR(48) NOT NULL | CHECK `^[A-Z][A-Z0-9_]{1,47}$` |
| idempotency_key | VARCHAR(160) NOT NULL | UNIQUE `uq_email_deliveries_idempotency` |
| recipient_hash | CHAR(64) NOT NULL | SHA-256 of trimmed, lowercased address (`recipient_hash()` L80) |
| recipient | TEXT NULL | **Plaintext address**. Nulled `retention_days` after `final_at` |
| recipient_name | TEXT NULL | Display name, nulled with recipient |
| subject | TEXT NOT NULL | **Never erased** (see §9) |
| html_body, text_body | TEXT NULL | Erased on a final state (OTP codes and invitation tokens live here until then) |
| status | VARCHAR(16) default 'QUEUED' | CHECK in (QUEUED, SENDING, ACCEPTED, DELIVERED, DELAYED, BOUNCED, COMPLAINED, FAILED, EXPIRED) |
| provider | VARCHAR(16) | `resend` / `smtp` / `recording` |
| provider_message_id | VARCHAR(128) | Partial UNIQUE index `uq_email_deliveries_provider_message (provider, provider_message_id)` |
| attempts | INT ≥0 | |
| next_attempt_at | TIMESTAMPTZ default now() | |
| lease_holder, lease_expires_at | VARCHAR(96), TIMESTAMPTZ | |
| last_error_class, last_error_status | VARCHAR(64), INT | |
| expires_at | TIMESTAMPTZ | Do-not-send-after |
| created_at, updated_at, accepted_at, delivered_at, final_at | TIMESTAMPTZ | |

Indexes:
- `ix_email_deliveries_due (next_attempt_at) WHERE status IN ('QUEUED','SENDING')`
- `ix_email_deliveries_recipient (recipient_hash, created_at)`
- `ix_email_deliveries_status (status, updated_at)`

No FKs.

**`email_delivery_events`** (L62-72): PK `(provider, event_id)` (dedupe of Svix ids). Columns: `event_type` VARCHAR(48), `provider_message_id`, `delivery_id` UUID (no FK), `occurred_at`, `received_at`. Index `ix_email_delivery_events_delivery`. No PII.

The same migration also creates `conference_presence`, which is unrelated to notifications.

**Legacy, deprecated:**
- `migration/src/m20260117_000001_create_email_jobs.rs` created `email_jobs` with an `email_job_status` enum that included `dead_letter`.
- `m20260323_000001_migrate_to_redis.rs` dropped it (L10, L38-40) in favour of the Redis list.
- `src/queues/redis_email.rs::RedisEmailQueue` (key `email:queue`, `MAX_RETRIES=3`) is now "kept only so messages left in it at upgrade time are imported once" (L1-5). Its only caller is `main.rs` L243.
- `services/meeting-service/docs/EMAIL_QUEUE.md` still describes the Redis/BRPOP design and is stale.

### 1.3 Enqueue API: `email_delivery::enqueue(conn, NewEmail)` (L124-185)

- Validates `to` with `config::email::valid_address` (L94-111): ≤254 chars, no whitespace or control characters, a domain containing `.`. An invalid address returns `ApiError::BadRequest`.
- Subject and name are header-cleaned (control characters removed, ≤200/≤120 chars, `<>"` stripped from the name).
- `idempotency_key`: the caller's key (≤160 chars), or `msg:{uuid}` when `None`.
- Insert uses `ON CONFLICT (idempotency_key) DO NOTHING RETURNING id`. On a duplicate it returns the existing id with `created=false` and increments `email_enqueue_duplicates`.
- It is generic over `ConnectionTrait`, so it can run inside a business transaction. Only `account_devices::send_email_code` uses it that way. All other callers enqueue after the business commit and treat failure as best effort (see §2).

**Idempotency keys by category** (from callers):

| Category const (`email_delivery::category`, L51-63) | Key | Expiry | Caller |
|---|---|---|---|
| AUTH_VERIFICATION | `otp:verify:{recipient_hash}:{sha256(code)[..12] hex}` | now+24h | `EmailService::send_verification_email` L77-99 |
| AUTH_PASSWORD_RESET | `otp:reset:{recipient_hash}:{digest}` | now+1h | L102-124 |
| AUTH_WELCOME | `welcome:{user_id}` | now+2d | L127-145. **No caller** (dead code) |
| SECURITY_PASSWORD_CHANGED | `password-changed:{user_id}:{unix_ts}` | now+2d | L148-169 |
| NOTIFICATION | none (random) | none | `send_notification_email` L172. Used by device notices |
| MEETING_INVITATION | none | none | `notifications.rs` L522-523 |
| ORGANIZATION_INVITATION | none | none | `notifications.rs` L613-614 |
| MEETING_NOTIFICATION | `meeting-notification:{job_id}:{sha256(lower(addr))}` | none | `notifications.rs` L1037-1038 |
| DEVELOPER_PLATFORM | `devplat-access:{request_id}:{decision}` | now+3d | `handlers/email.rs` L241-245 |
| OPERATIONS_ACCEPTANCE | `acceptance:{reference}` | now+1h | `handlers/email.rs` L147 |
| LEGACY_QUEUE | `legacy-redis:{job.id}` | now+1h | `import_legacy_redis_queue` L906 |
| `SECURITY_DEVICE_STEP_UP` (**string literal, not in the `category` module**) | `device-step-up:{link_id}:{code_digest[..16]}` | link `expires_at` | `account_devices.rs` L1548-1566 |

### 1.4 Worker: `EmailWorker` (L599-875)

- **Spawn:** `main.rs` L251-270. The worker starts only when `provider_from_config` returns `Some` (Resend or SMTP). With `EMAIL_PROVIDER=none` no worker runs ("emails stay queued", warning at L266). `spawn_email_worker` (L887) runs one tokio task per process. `workers/email.rs` is only a re-export.
- **Claim** `claim()` (L620-644):
  - Runs `UPDATE … SET status='SENDING', lease_holder, lease_expires_at = now()+90s, attempts=attempts+1` on rows `(status='QUEUED' AND next_attempt_at<=now()) OR (status='SENDING' AND lease_expires_at<now())`, `ORDER BY next_attempt_at LIMIT batch FOR UPDATE SKIP LOCKED`.
  - `LEASE_SECS=90` (L76). The holder id is `email-{uuid}` per process (L616).
- **Leader election:** none. Multi-instance safety relies on SKIP LOCKED and on lease checks in `finish`/`reschedule` (`WHERE id=$1 AND lease_holder=$2`, L666 and L695).
- **Rate limit:** `spacing = 1000/max_per_second` ms sleep between messages in a batch (L716, L743-745). This is per process, not global.
- **Expiry:** `expires_at <= now` leads to `finish("EXPIRED")` (L718-723). Erased content leads to `FAILED/CONTENT_UNAVAILABLE` (L724-742).
- **Outcomes:**
  - `Ok`: ACCEPTED, `provider_message_id` stored, content erased (L757-764).
  - `Permanent`: FAILED (L765-773).
  - `Temporary`: next attempt = `Retry-After` (clamped 1s..15min) or `backoff(attempts)`. If `attempts >= max_attempts` or the message would expire before the next attempt, it ends FAILED/EXPIRED. Otherwise it goes back to QUEUED (L774-811).
- **Backoff** `backoff()` (L592-597): 5s·2^(n-1), capped at 15 min, ±20% jitter (`BACKOFF_BASE_SECS=5`, `BACKOFF_CAP_SECS=900`, L77-78).
- **Terminal / dead-letter states:** FAILED, EXPIRED. ACCEPTED is also "final" (`FINAL_STATUSES` L66-74) and content is erased at that point. There is no separate DLQ table and no re-drive endpoint.
- **Retention** `purge()` (L818-843), run at most hourly from `run()` (L860-865):
  - Nulls `recipient`/`recipient_name` when `final_at < now - retention_days`.
  - Deletes rows when `final_at < now - 2×retention_days`.
  - Deletes `email_delivery_events` older than `2×retention_days`.
  - Non-final rows (QUEUED forever when no provider is configured) are never purged and their bodies are never erased.
- **Poll:** `run()` sleeps `poll_millis` only when idle (`busy` = claimed>0, L853-871).

### 1.5 Provider adapters

| Adapter | Symbol | Behaviour |
|---|---|---|
| Resend | `ResendProvider` L270-391 | `POST {RESEND_API_URL}/emails`, bearer key, `Idempotency-Key: <ledger id>` (L364), 20s timeout / 10s connect, UA `cfmeet-email-worker/1`. Body has `from`, `to` (`Name <addr>`), `subject`, `html`, `text`, `tags:[{category}]`, optional `reply_to` (`body()` L292-309). `classify()` L312-350: 429 → Temporary(Retry-After); 409 `concurrent_idempotent_requests` → Temporary(10s); other 409 → Permanent; 401/403 → Temporary `PROVIDER_CONFIGURATION`; 400/404/405/413/422 → Permanent `REJECTED`; 5xx → Temporary; other → Temporary `UNEXPECTED_STATUS`. Network/timeout errors → Temporary |
| SMTP (legacy) | `SmtpProvider` L394-486 | lettre; port 465 → implicit TLS, else STARTTLS; 30s timeout. Message-ID `<{id}@cfmeet.com>`. Multipart text+html. Permanent SMTP errors → Permanent `SMTP_REJECTED`, except a message containing "535" → Temporary `SMTP_AUTHENTICATION`. Transient errors → Temporary. **No idempotency at the provider** (re-claim after lease expiry can duplicate) |
| Recording (tests) | `RecordingProvider` L489-526 | In-memory |
| none | `provider_from_config` L529-540 | Returns `None`: no worker |
| Dev tool (bypasses ledger) | `src/bin/test_email.rs` (bin `test_email`, `Cargo.toml` L73-75) | Direct lettre SMTP send to `SMTP_USERNAME`. Prints host/username/from to stdout. Not used at runtime |

### 1.6 Provider callbacks and webhooks

- **Route:** `POST /api/v1/email/provider-events/resend` → `handlers/email.rs::resend_events` (L50-103), mounted in `routes/mod.rs` L92-95.
- **Auth:** Svix signature.
  - `email_delivery::verify_svix` (L941-983) computes HMAC-SHA256 over `{svix-id}.{svix-timestamp}.{body}` with the base64 secret after `whsec_`, checks the timestamp within ±300s and `svix-id` ≤128, and accepts any matching `v1,` signature. Verification uses `Mac::verify_slice` (constant-time).
  - Returns 404 when `RESEND_WEBHOOK_SECRET[_FILE]` is unset (`state.email_webhook_secret`), 413 above 256 KiB, 401 on a bad signature (counter `email_webhook_rejected`), 400 on malformed JSON.
- **Processing:** `apply_provider_event` (L998-1086).
  - Inserts into `email_delivery_events` `ON CONFLICT (provider,event_id) DO NOTHING`. A duplicate returns `{duplicate:true}`.
  - Resolves `delivery_id` by `(provider, provider_message_id)` at insert time.
  - Maps `email.delivered|delivery_delayed|bounced|complained|failed` to DELIVERED/DELAYED/BOUNCED/COMPLAINED/FAILED. Other event types (sent, opened, clicked) are recorded with no status change.
  - Monotonic: `rank()` (L986-995) only upgrades. QUEUED/SENDING=0 < ACCEPTED=1 < DELAYED=2 < DELIVERED=3 < BOUNCED/COMPLAINED/FAILED=4.
- **Not in OpenAPI.** Confirmed: `packages/contracts/openapi/meeting-service.json` has no `/api/v1/email/*` path. `CFMEET_FINAL_PRODUCTION_COMPLETION.md` L279 says it is "Not a client API".
- **Status:** implemented. Configured only if the owner sets the webhook secret. There is no evidence the secret is set in production (the 2026-11-12 report says "No Resend key and no webhook secret").
- **Gaps:**
  - No suppression list. BOUNCED/COMPLAINED do not stop future sends to that `recipient_hash`.
  - An event that arrives before the worker's `finish()` commits `provider_message_id` is stored with `delivery_id NULL` and never reconciled (see §9).

### 1.7 Templates and localization

- `src/templates/email/base.rs`: shared layout (`wrap_html` L42 with hard-coded `<html lang="en">` L45, `wrap_text`, `button`, `code_block`, `escape_html` L16, `plain` L34).
- `src/templates/email/account.rs`: `verification_email` L16, `password_reset_email` L37, `welcome_email` L58, `password_changed_email` L76, `notification_email` L99 (generic; also used for acceptance, developer decision and device notices).
- `src/templates/email/meeting.rs::render(NotificationBody)` (L33-73): greeting, paragraphs, optional action button, footnote. Every value is HTML-escaped and control characters are dropped. Tests at L79-102.
- **Localization: absent.** English only. `users.locale` is stored (Section 9) but unused by any template. `docs/operations/profile-notifications-sections-9-11.md` L117-118 confirms: "localized templates (English only…)".
- No `List-Unsubscribe` header, no unsubscribe link, no ICS attachments.

### 1.8 Environment variables and flags (names and defaults only)

Source: `src/config/email.rs::EmailConfig::from_lookup` (L137-263) and `src/config/notifications.rs` (L43-62).

| Var | Default | Notes |
|---|---|---|
| `EMAIL_PROVIDER` | unset → `smtp` if `SMTP_HOST` set, else `none` | `resend`/`smtp`/`none|off|disabled`. Unknown values panic at startup (`main.rs` L63-64) |
| `RESEND_API_KEY_FILE` / `RESEND_API_KEY` | — | Required for resend. Must start `re_`, length ≥12 |
| `RESEND_API_URL` | `https://api.resend.com` | Must be https (or `http://127.0.0.1…`) |
| `RESEND_WEBHOOK_SECRET_FILE` / `RESEND_WEBHOOK_SECRET` | unset | Must start `whsec_`. Enables the webhook route |
| `SMTP_HOST`, `SMTP_PORT` (587), `SMTP_USERNAME`, `SMTP_PASSWORD[_FILE]` | — | Required for smtp |
| `SMTP_FROM_EMAIL`, `SMTP_FROM_NAME` | — | Fallback sender with smtp |
| `EMAIL_FROM_ADDRESS` / `EMAIL_FROM_NAME` | `no-reply@cfmeet.com` / `CF Meet` | Validated |
| `EMAIL_REPLY_TO` | unset | Validated |
| `APP_URL` | `http://localhost:3000` | Links in account emails |
| `EMAIL_MAX_ATTEMPTS` | 8 (1-20) | |
| `EMAIL_MAX_PER_SECOND` | 2 (1-100) | Per process |
| `EMAIL_WORKER_BATCH` | 10 (1-100) | |
| `EMAIL_WORKER_POLL_MS` | 1000 (200-60000) | |
| `EMAIL_LEDGER_RETENTION_DAYS` | 30 (1-365) | |
| `NOTIFICATIONS_EMAIL_ENABLED` | **false** | Master switch for meeting/org notification email and the notification worker |
| `NOTIFICATIONS_REMINDER_LEAD_MINUTES` | 15 (0 disables, ≤1440) | |
| `NOTIFICATIONS_MAX_RECIPIENTS` | 200 (1-1000) | |
| `NOTIFICATIONS_POLL_SECS` | 30 (≥5) | |
| `NOTIFICATIONS_INVITE_EMAILS_PER_HOUR` | 50 (1-1000) | Per account |
| `INTERNAL_API_TOKEN` | unset → internal email ops return 404 | `handlers/internal.rs::internal_token_ok` L85-98 |
| `ACCOUNT_DEVICE_EMAIL_STEP_UP` | true | `account_devices.rs::DevicePolicy` L66-68 |
| `CF_COMMUNICATIONS_BASE_URL` (+ `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`, `CF_PLATFORM_ACCESS_BASE_URL`, `CF_SERVICE_TRUST_BUNDLE_FILE`, `CFMEET_COMMUNICATION_*`) | unset → no preferences | `services/communications.rs::Bridge::from_env` L40-106 |
| `EMAIL_VERIFICATION_ENFORCEMENT`, `EMAIL_VERIFICATION_ENFORCE_FROM`, `EMAIL_VERIFICATION_GRACE_UNTIL`, `REQUIRE_EMAIL_VERIFICATION` | off | `account_security.rs::AuthPolicy` L98-109, L214-241 |
| `AUTH_CLIENT_IP_HEADER` | unset → no per-IP limits on auth email endpoints | `handlers/auth.rs::limit_by_client` L22-41 |

Release-tooling guard: `infra/calling/production/coordinated_release.py` L1274-1275 refuses `NOTIFICATIONS_EMAIL_ENABLED=true` without mail credentials. `first_party_feature_changes` (L1812-1820) enables it only if `mail_available()` (L299-302) is true.

### 1.9 Metrics and logging

- **Counters** in `src/services/ops_metrics.rs` L110-122 (label-free): `email_enqueued`, `email_enqueue_duplicates`, `email_provider_accepted`, `email_provider_temporary_failures`, `email_provider_permanent_failures`, `email_failed`, `email_expired`, `email_delivered`, `email_bounced`, `email_complained`, `email_webhook_rejected`, `email_webhook_duplicates`. They are exposed through `GET /internal/metrics` and `GET /internal/rtc/metrics` (`handlers/internal.rs` L100-112, `X-Internal-Token`).
- **No metrics for meeting notification jobs.** `notifications::status_counts` (L1169-1185) exists but has no caller outside tests.
- **Logs:**
  - The worker logs `delivery_id`, `category`, `provider`, `error_class`, `status` and `attempts`. It never logs the address or content (L761-808).
  - Auth handlers log `user_id` and `job_id` only (e.g. `handlers/auth.rs` L148, L151, L802, L805, L881, L1023).
  - Grep for tracing macros that interpolate `.email`, `to_email`, `code` or `token` found none in `src/`.
  - The legacy `RedisEmailQueue::enqueue` logs the job id only.
  - Exception: the dev binary `test_email.rs` prints the SMTP username and sender.

### 1.10 PII stored

| Store | PII |
|---|---|
| `email_deliveries.recipient`, `recipient_name` | Plaintext address and name. Kept 30 days after final (default), row deleted at 60 days. **Kept indefinitely for non-final rows** |
| `email_deliveries.subject` | Kept for the row lifetime. Can include meeting titles (`Invitation: {title}`, `Reminder: {title} starts at …`), organization names and time zones |
| `email_deliveries.html_body/text_body` | OTP codes, device step-up codes, invitation tokens (in the link fragment) and names, until a final state |
| `email_deliveries.recipient_hash`, `meeting_notification_recipients.recipient_digest` | Unsalted SHA-256 of the lowercased address. Pseudonymous only: guessable by dictionary |
| Redis `otp:verify:{email}`, `otp:reset:{email}`, `…:attempts:{email}`, `…:send_rate:{email}` (`services/otp.rs` L60-78) | **Plaintext lowercased address in key names, plaintext 6-digit code as value** |
| Redis `rate:{action}:{sha256(subject)[..32]}` (`rate_limiter.rs` L57-76) | Hashed |

---

## 2. Account and security notifications

| Event | Code path | Email? | Other side effects | Security-critical | Classification and evidence |
|---|---|---|---|---|---|
| Registration verification code | `handlers/auth.rs::register` L78-163: `generate_verification_code()` → `otp_service.store_code` (Redis) → `email_service.send_verification_email`. Enqueue failure is logged and registration still succeeds (L141-153). Audit `ACCOUNT_REGISTERED` | **Yes**, `AUTH_VERIFICATION` | — | Yes | Implemented. Activated: always on, no flag; transport is SMTP in prod. **Broken in prod per docs**: "OTP delivery is not yet recovered" (`CFMEET_FINAL_PRODUCTION_COMPLETION.md` §6, 2026-11-12, diagnose 43317264046). Pre-prod test `tests/email_delivery.rs::otp_journey_from_registration_to_session` (L345) |
| Resend verification | `resend_verification_email` L678-730. Uniform answer; per-account send limit `check_send_rate` 5/15min, exceeding it is silent | Yes | — | Yes | Same as above |
| Email verification (consume) | `verify_email` L188-240. `otp_service.verify_code`, 5 attempts per 15 min per email. Sets `status=Active`, `email_verified_at`. Audit `EMAIL_VERIFIED` | No (no welcome email) | — | Yes | Implemented |
| Welcome email | `EmailService::send_welcome_email` L127 | Never sent | — | No | **Implemented but not wired**. No caller. `CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5: "available … not triggered" |
| Password reset request | `forgot_password` L748-820. Only local accounts that are not inactive or suspended; uniform `RECOVERY_ACCEPTED`; 5 sends per 15 min per email. Audit `PASSWORD_RESET_REQUESTED` | Yes, `AUTH_PASSWORD_RESET` (code TTL 1h) | — | Yes | Implemented. Prod transport broken (as above) |
| Password reset complete | `reset_password` L836-899. Code verified, password updated; a DB trigger closes all sessions. `send_password_changed_email(..,"reset")` is best effort **after** commit (L876-882). Audit `PASSWORD_RESET`. `communications::credentials_ended` | Yes, `SECURITY_PASSWORD_CHANGED` | Hub account event via `account_communication_outbox` | Yes | Implemented. Notice is not transactional |
| Verify reset token | `verify_reset_token` L913-946. Returns 200/400 depending on whether a reset code exists in Redis | No | — | — | Implemented. **Enumeration oracle** (§9) |
| Password change | `change_password` L967-1036. Best-effort `send_password_changed_email(..,"changed")` after update (L1016-1024). Audit `PASSWORD_CHANGED` / `PASSWORD_CHANGE` failure | Yes | Hub event | Yes | Implemented |
| Login / new sign-in alert | `login` (L259+) records audit `LOGIN` only | **No** | — | Yes | **Absent**. Explicitly "Not created" in `CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5 ("new-sign-in alerts") |
| Failed login lockout | `rate_limiter.check_login_rate` (5 per 15 min per email) | No | — | — | Audit/limit only |
| Refresh-token reuse (suspicious session) | `account_security.rs::rotate_session` L618-690: audit `REFRESH_TOKEN_REUSE` (Denied) and session revoke | **No** | — | Yes | Audit-event only |
| Email-address change | grep for `change_email`, `email_change`, `new_email` returns nothing | — | — | Yes | **Absent** (no workflow). Doc §5 confirms |
| Device enrollment: link requested | `account_devices.rs::create_link` L1263. Audit `DEVICE_LINK_REQUESTED` | No | — | Yes | Audit only |
| Device link approved/rejected (by trusted device) | `approve_link` L1389, `reject_link` L1464. Audits `DEVICE_LINK_APPROVED/REJECTED` | No | — | Yes | Audit only |
| Device email step-up code | `send_email_code` L1494-1571. `six_digits()` L1164 (`rand::thread_rng`). Digest `code_digest()` L1168 = SHA-256(`cfmeet-device-link-email:v1|{link}|{code}`) stored in `account_device_links.email_code_digest`. 60s resend throttle (`EMAIL_RESEND_SECS`). Link TTL `ACCOUNT_DEVICE_LINK_TTL_SECS` (default 300). Enqueued **in the same txn**. Audit `DEVICE_STEP_UP_CODE_SENT`. Verified in `complete_link` with `ct_eq` (L1174); `MAX_LINK_ATTEMPTS=5`, shared counter `email_attempts` | Yes, category `SECURITY_DEVICE_STEP_UP` | — | Yes | Implemented. Activated by default (`ACCOUNT_DEVICE_EMAIL_STEP_UP` default true; prod value "default `true`" per `CFMEET_MULTI_DEVICE_E2EE_FINAL_REPORT.md` L70, 2026-11-15). Step-up itself was not shown exercised in prod (run 43666265601 used approvals). Test `tests/account_devices.rs::emailed_code` (L233) reads `text_body` from the ledger |
| Device link completed: new trusted browser | `complete_link` L1574-1758 → `notify_owner` (L769-777) "A new browser was linked…" (L1734-1743), best effort after commit | Yes, `NOTIFICATION` (no idempotency key) | — | Yes | Implemented. Exercised in prod run 43666265601 (2026-11-15, check 2: "second_browser_linked_by_trusted_device_approval"), but **delivery not evidenced** |
| Primary-mobile replacement / first mobile | `complete_link`: under the account lock, the old mobile is revoked (`DEVICE_REPLACED`), its sessions end, audit `PRIMARY_MOBILE_REPLACED`. Then `notify_owner` "A new phone is now your primary…" (L1724-1733) | Yes, `NOTIFICATION` | Hub/Messaging fan-out via `session_effects` | Yes | Implemented. Exercised in prod run 43666265601 (checks 3, 9, 10). Delivery not evidenced |
| Device revocation | `revoke_device` L1780-1860. Audit `DEVICE_REVOKED`, sessions end | **No** | Session fan-out | Yes | Audit only |
| Session revoke (single) | `handlers/v2/account_sessions.rs::revoke_my_session` L123. Audit `SESSION_REVOKED`. `communications::session_ended` queues `ACCOUNT_SESSION_ENDED` to the Hub | No | Hub outbox | Yes | Audit/event only |
| Sign-out everywhere | `revoke_all_my_sessions` L174 (audit `LOGOUT_ALL`), v1 `logout` path (`handlers/auth.rs` L617-623), `account_devices::revoke_all_devices` L1863 | **No** | Hub `SESSION_REVOKED` event (`communications::record_logout` L913) | Yes | Audit/event only |
| Account deactivation | `handlers/users.rs::deactivate_account` (L142; route `/api/v1/users/me/deactivate`). Re-auth required. Audit `ACCOUNT_DEACTIVATED`. `credentials_ended("ACCOUNT_DISABLED")` | **No** | Hub event, Platform Access mirror | Yes | Audit only |
| Admin status/role change, suspension, reactivation | `handlers/admin.rs` L280 (`ACCOUNT_STATUS_CHANGED`), L295 (`ACCOUNT_ROLE_CHANGED`) | **No** | — | Yes | Audit only |
| Account recovery (lost device) | Email step-up above | Yes | — | Yes | See step-up |
| Profile updated | `handlers/v2/account_profile.rs` L131, audit `PROFILE_UPDATED` | No | — | No | Audit only |
| Phone/SMS OTP | Design only (`profile-notifications-sections-9-11.md` L85-95) | — | — | — | **Absent** (EXTERNAL_BLOCKED / DEFERRED_BY_OWNER) |
| MFA | Not built (`account-security-section-2.md` L55) | — | — | — | Absent |

**OTP mechanics** (`src/services/otp.rs`):
- `generate_verification_code` (`services/auth.rs` L214-219) produces a 6-digit code via `rand::thread_rng` in `100000..1000000`.
- Codes are stored **plaintext** in Redis with TTL 24h (verification) or 1h (reset) (L10-14, `store_code` L81-84).
- `verify_code` (L87-114) increments `attempts`; above 5 per 15 min it returns 429. It then compares with `==` (not constant time) and deletes the code and attempts on success.
- The verification authority is the Meeting Service handler. The check is single-use.
- **Send limit:** `check_send_rate` (L123-134), 5 per 15 min per email per type.
  - `register` does not call it, but each registration needs a new address.
  - Per-IP limits apply only with `AUTH_CLIENT_IP_HEADER`.
- **Inconsistency:** device step-up codes are hashed and compared in constant time; account OTPs are not.

**Account lifecycle events to the Communications Hub (not user-facing notifications):**
- Table `account_communication_outbox`. Producers: `communications::record_session_ended` L470, `record_logout` L913, and DB triggers.
- Delivery: `deliver_due` L936-975 posts to Hub `/internal/v1/communications/account-events`, then mirrors status to Platform Access `/internal/v1/principals/account-status`.
- Driven by a 500 ms loop in `main.rs` L218-230, only when Communications is configured.
- Lease 120s, backoff 2^attempts (cap 2^10 s). **No max-attempts or dead-letter** (retries forever).

---

## 3. Meetings notifications

All meeting notification email is behind `NOTIFICATIONS_EMAIL_ENABLED` (default false; `config/notifications.rs` L3-5, L46). Production value is claimed true (SMTP) on 2026-11-13 (§0).

### 3.1 Implemented notifications (`src/services/notifications.rs`)

| Notification | Producer | Mode | Dedupe | Classification |
|---|---|---|---|---|
| Meeting invitation email | `handlers/v2/invitations.rs::create_invitation` (L38-104, `POST /api/v2/meeting-spaces/{id}/invitations`, opId `v2_create_invitation`) → `notifications::send_meeting_invitation` (L429-442) → `meeting_invitation_inner` (L444-536) | **Synchronous** after the invitation commits (the token is never stored). Link `{APP_URL}/invite#<token>` (L506). Response field `emailDelivery` = DISABLED / ACCEPTED / FAILED (`EmailDelivery` enum L279-286) | None (one email per request) | Implemented. Activated (flag claimed true 2026-11-13). Not production-tested. **Likely broken for account-holder invitees** (§3.4) |
| Organization invitation email | `handlers/v2/organizations.rs::create_organization_invitation` (L935-1033, opId `v2_create_organization_invitation`): strips `sendEmail`, relays signed to Organizations `/internal/v1/organizations/{id}/invitations`, then `notifications::send_organization_invitation` (L540-626) using the token from the authoritative answer. Link `{APP_URL}/organizations/invitations#<token>` (L600) | Synchronous | None. An idempotent replay rotates the token and **emails again** (doc L104) | Implemented. Activated (same flag). Requires `require_authoritative` mode when the email is on. Not production-tested. No preference check (transactional by design, L33-35) |
| Reschedule notice | `services/conference.rs::reschedule` (L933-1015) → `notifications::record_rescheduled` (L660-688) **in the same transaction** (L1003-1011) | Durable job `CONFERENCE_RESCHEDULED`. Only `origin=V2` with a scheduled start | `rescheduled:{conf}:{version}` | Implemented. Activated (flag). Not production-tested |
| Cancellation notice | `conference.rs::cancel` (L869-930) → `record_cancelled` (L692-714), same txn (L910-916) | Durable job `CONFERENCE_CANCELLED`. Only scheduled V2 occurrences | `cancelled:{conf}` | Implemented, same evidence |
| Reminder | Notification worker `tick` → `enqueue_due_reminders` (L718-755): V2 conferences in status CREATED/PREPARING/WAITING, space ACTIVE, `scheduled_start_at ∈ (now, now+lead]`, `LIMIT 200` (`REMINDER_SCAN_LIMIT`) | Durable job `CONFERENCE_REMINDER` | `reminder:{conf}:{start_epoch}` (`reminder_key` L757). A reschedule creates a new reminder | Implemented, same evidence. Lead default 15 min |

### 3.2 Durable job tables (`migration/src/m20261108_000002_meeting_notifications.rs`, history `profile_notifications_migrations`; `main.rs` L163-168 panics if not applied)

**`meeting_notifications`** (L24-53):
- Columns: `id` UUID PK; `kind` VARCHAR(32) CHECK in (CONFERENCE_RESCHEDULED, CONFERENCE_CANCELLED, CONFERENCE_REMINDER); `meeting_space_id` UUID NOT NULL; `conference_id` UUID NOT NULL; `dedupe_key` VARCHAR(200) **UNIQUE** (`uq_meeting_notifications_dedupe`); `actor_user_id` UUID NULL; `payload` JSONB (object, ≤2048 bytes); `status` VARCHAR(16) CHECK in (PENDING, SENT, SKIPPED, FAILED); `attempts`; `available_at`; `lease_id`; `lease_until`; `last_error` VARCHAR(64); `recipients_queued`; `recipients_suppressed`; `created_at`; `completed_at`.
- CHECK `(status='PENDING') = (completed_at IS NULL)`.
- **No FKs** to `meeting_spaces` or `conference_sessions`.
- Indexes: `idx_meeting_notifications_due (available_at) WHERE status='PENDING'`, `idx_meeting_notifications_completed`, `idx_meeting_notifications_conference (conference_id, created_at DESC)`.

**`meeting_notification_recipients`** (L55-61):
- PK `(notification_id, recipient_digest)`.
- FK `notification_id → meeting_notifications ON DELETE CASCADE`.
- `outcome` in (QUEUED, SUPPRESSED); `recorded_at`.
- No address is stored.

Payloads hold ids, times and a version only (`record_rescheduled` L679-684, `record_cancelled` L710, reminder L750).

### 3.3 Notification worker (`notifications::tick` L794-871, `spawn_worker` L1144-1166)

- **Spawned** in `main.rs` L274-288 only when `email_enabled`. It is independent of `FEATURE_MEETING_V2_ENABLED`. Cadence `NOTIFICATIONS_POLL_SECS` (default 30). Batch 20 (`NotificationConfig::batch`, not configurable). One task per process. No leader election.
- **Claim** (L801-811): `UPDATE … SET lease_id, lease_until=now()+120s, attempts+1 WHERE id IN (SELECT … status='PENDING' AND available_at<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY available_at LIMIT 20 FOR UPDATE SKIP LOCKED)`. `LEASE_SECS=120` (L75).
- **Deliver** `deliver()` (L916-1071):
  - Skips with `SPACE_GONE`, `SPACE_INACTIVE` or `CONFERENCE_GONE`.
  - Checks the notice is "still true": reminder requires the conference startable and the start unchanged and in the future; reschedule requires status CREATED and start == payload `to`; cancel requires status CANCELLED. Otherwise SKIPPED `SUPERSEDED`.
- **Recipients** `RECIPIENTS_SQL` (L892-914): space owner, ACTIVE members (account or EMAIL principal), ACTIVE unexpired unexhausted email-bound invitations (space-wide or that conference), REGISTERED webinar registrants that have accounts. Deduplicated by `lower(email)`, joined to `users`, `ORDER BY email LIMIT max+1`.
  - Then truncated to `max_recipients` with a warning (L974-977).
  - Then filters out the actor, inactive or suspended accounts and guests (L978-983).
- **Preferences:** `notifier.preferences.meeting_opt_outs(accounts)`. On error the result is `Retry("PREFERENCES_UNAVAILABLE")` (L994-997).
- **Per recipient:**
  1. `INSERT meeting_notification_recipients … ON CONFLICT DO NOTHING`. If the row exists, skip (handled before).
  2. If opted out, record SUPPRESSED.
  3. Otherwise compose the email (`compose` L1074-1127; time in the recipient's profile tz, else the space tz, else UTC: `format_time` L343-353) and enqueue with key `meeting-notification:{job}:{digest}`.
  4. On enqueue error: delete the ledger row, flush counters, return `Retry("MAIL_QUEUE_UNAVAILABLE")` (L1047-1067).
- **Settle** (L838-858): backoff `2^attempts` s capped at 900. FAILED after `MAX_ATTEMPTS=8` (L74). Settlement is guarded by `lease_id`.
- **Purge:** deletes up to 500 rows per tick with `completed_at < now-30d` (`RETENTION_DAYS` L76). This only runs while `email_enabled` (tick returns early otherwise, L796-798).
- **Delivery semantics:** at most once per recipient per job. The digest row is written before enqueue. A crash between the two drops that recipient silently.
- **Protected titles:** `presented_title` (L323-339) returns "CF Meet meeting" for HEALTHCARE / HIGH_SECURITY security profiles, HEALTHCARE meeting type and `HealthcareAppointment` access.

### 3.4 Preferences (ACC-020)

- `PreferenceSource` trait (L158-165). Implementations:
  - `NoPreferenceStore` (default in `state.rs` L90-93; everything is allowed).
  - `StaticPreferences` (tests).
  - `HubPreferences` (L208-262): signed `POST /internal/v1/communications/notification-preferences/resolve` (`HUB_PREFERENCES_PATH` L213) with `{"category":"MEETINGS","accountReferences":[cfmeet-user:<uuid>…]}` in chunks of 100. Any non-200 response or parse error returns `PreferencesUnavailable`.
- Wired in `main.rs` L195-204 whenever Communications is configured.
- **Production grant gap (strong evidence of breakage, inferred from repo config):**
  - The Hub's permission `communications-preferences.read` exists (`packages/service-auth/src/permissions.rs` L70, L397).
  - The production Hub trust-bundle builder `infra/calling/production/communications_release.py` L178 grants meeting-service only `['communications-user.forward','communications-account.apply']`. It does **not** grant `communications-preferences.read`. `docs/operations/profile-notifications-sections-9-11.md` L222-228 says the grant was "deliberately not changed".
  - The doc states the consequence (L225-228): "a notice with any account-holder recipient waits and ends FAILED and invitation emails to account holders report FAILED (fail closed)".
  - `communications_release.py` L310 sets `CF_COMMUNICATIONS_BASE_URL` for meeting-service, so `HubPreferences` is active in production.
  - **Classification:** meeting notices and invitation emails to account holders = activated but expected broken (fail-closed). Only emails to addresses without an account would be sent. This is not verified against production.

### 3.5 Absent or other meeting notification behaviour

| Behaviour | Status | Evidence |
|---|---|---|
| Accept/decline of meeting invitation → notify inviter | **Absent** | No producer. `POST /api/v2/invitations/{token}/resolve` (opId `v2_resolve_invitation`) has no notify call |
| Org invitation accept/decline → notify inviter | **Absent** | Relay only (opIds `v2_accept_organization_invitation`, `v2_decline_organization_invitation`) |
| Recurrence exception cancel/reschedule (`PUT …/recurrence/exceptions`) | **Absent** (documented gap) | `services/recurrence.rs::set_exception` (L803) updates rows and emits `conference.cancelled` (L902) but never calls `record_*`. Doc L115-116 |
| Series edit / series end / space archive cancelling occurrences | **Absent** | `recurrence.rs::set` L722, `end` L966, `stop_series_for_archive` L364: no notification calls |
| System expiry of stale occurrences | **Absent by design** | `calendar::expire_stale` (called by `ConferenceScheduler::tick` L66) emits `conference.cancelled` only. Docs comment at `notifications.rs` L690-691 |
| Legacy v1 meetings (`meetings` table) | **Absent** | Doc L116. `record_*` require `origin == V2` (L667, L698) |
| "Meeting starting" / host started | Realtime only | `conference.started` event to WebSocket subscribers of that conference (`meeting_events.rs` L37). No email or push |
| Waiting room (join request, admit/deny) | Realtime only | `admission.rs` L1118 emits `participant.requested_join` to the conference outbox. Audience filtering via `Audience` (`meeting_events.rs` L263-279) |
| Host/participant updates (role change, removed, banned, muted, spotlight) | Realtime only | `meeting_events.rs` L46-79 |
| Member added to a space (email-bound) | **Absent** | `handlers/v2/members.rs`: no email |
| Webinar registration confirmation | **Absent** (email). Realtime/outbox event `registration.created` | `services/specialized.rs` L659. Registrants with accounts receive reschedule/cancel/reminder notices. Registrant-typed emails are never mailed (RECIPIENTS_SQL joins `users`) |
| Recording / transcript availability | Realtime plus Developer Platform public event (environment-owned spaces only). No email or in-app | `artifacts/service.rs` L721-729. `services/platform/events.rs` L13-21 maps to `meeting-service.recording.ready` etc. |
| Calendar (ICS) change notifications / attachments | **Absent** | ICS is download-only (`GET /api/v2/conferences/{id}/calendar.ics`, `/meeting-spaces/{id}/calendar.ics`). No email attachment, no subscription feed (`meetings-calendar-section-10.md` L110-122, L168-169) |
| Push (APNs/FCM/WebPush) | **Absent** in meeting-service. EXTERNAL_BLOCKED | `config/notifications.rs` L5-6. Doc §"Push" L165-179 (adapters live in Messaging, disabled) |
| SMS | **Absent** | — |
| Legacy customer webhooks (`/api/v1/webhooks`) | **Deprecated, never delivered** | `handlers/webhooks.rs` L1-14 (`DELIVERY_NOT_DELIVERED`) |
| Developer Platform integrator events | Implemented (separate concern) | `services/platform/events.rs` (outbox channel `PUBLIC_EVENT`, `PublicEventPublisher`). Only for platform-environment-owned spaces. Enabled by `MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION` (`main.rs` L129-138) |

---

## 4. In-app notification store, feed, unread counts, preferences, realtime

- **In-app notification store or feed: absent.** No table or endpoint for a user notification inbox, no per-user unread counts. Unread counts referenced in `services/unified_messaging/relay.rs` L225 and `handlers/v2/unified_messaging.rs` L629 belong to the Messaging service (relayed).
- **Notification preferences:** not stored in meeting-service. They are owned by the Communications Hub (`services/communications/src/preferences.rs`), read via `HubPreferences`, and only for category `MEETINGS` (`meetingInvitations`). Clients reach Hub preference APIs through the generic relay `/api/v2/communications/*` (`communications::public_routes` L145-151, nested at `routes/mod.rs` L86-89).
- **Realtime transport** (per conference, in-meeting only; not a per-user notification channel):
  - `POST /api/v2/conferences/{id}/realtime/ticket` (opId `v2_realtime_ticket`): 60s ticket.
  - `GET /api/v2/conferences/{id}/realtime` (opId `v2_realtime_subscribe`): WebSocket.
  - `GET /api/v2/conferences/{id}/events` (opId `v2_replay_events`): cursor replay, `MAX_REPLAY=500`.
  - Handlers in `src/handlers/v2/realtime.rs` (L65-247). Service in `src/services/realtime.rs` (Redis pub/sub channel `cfmeet:rt:conference:{id}`, `conference_channel` L49).
  - Source rows: `outbox_events` (REALTIME channel), relayed by `workers/outbox_relay.rs` (2s, crash-safe leases, dead-letter FAILED after `outbox::MAX_ATTEMPTS`; `services/outbox.rs` L18, L100-136).
  - Gated by `FEATURE_CONFERENCE_REALTIME_ENABLED` (`main.rs` L327-344). Prod claimed on (2026-11-13 report L76).
- No SSE. No LiveKit data-channel notifications in meeting-service code paths reviewed.

---

## 5. Notification-related HTTP endpoints

The operationId column comes from `packages/contracts/openapi/meeting-service.json`; "—" means the route is not in OpenAPI.

| Method | Path | Handler | Auth | operationId | Public/internal |
|---|---|---|---|---|---|
| POST | /api/v1/auth/register | `handlers/auth.rs::register` | none | register | Public. Sends verification email |
| POST | /api/v1/auth/resend-verification | `resend_verification_email` | none | resend_verification_email | Public |
| POST | /api/v1/auth/verify-email | `verify_email` | none | verify_email | Public |
| POST | /api/v1/auth/forgot-password | `forgot_password` | none | forgot_password | Public. Sends reset email |
| POST | /api/v1/auth/reset-password | `reset_password` | none | reset_password | Public. Sends security notice |
| POST | /api/v1/auth/verify-reset-token | `verify_reset_token` | none | verify_reset_token | Public |
| POST | /api/v1/auth/change-password | `change_password` | bearer | change_password | Public. Sends security notice |
| POST | /api/v2/me/device-links/{linkId}/email-code | `handlers/v2/account_devices.rs::send_email_code` (`routes/v2.rs` L712-713) | bearer | v2_send_device_link_email_code | Public. Sends step-up code |
| POST | /api/v2/me/device-links/{linkId}/complete | `complete_link` | bearer | v2_complete_device_link | Public. Sends device notice |
| POST | /api/v2/meeting-spaces/{id}/invitations | `handlers/v2/invitations.rs::create_invitation` | bearer (platformCredential allowed but `sendEmail` refused for machine callers, L63-68) | v2_create_invitation | Public |
| POST | /api/v2/organizations/{id}/invitations | `handlers/v2/organizations.rs::create_organization_invitation` | bearer | v2_create_organization_invitation | Public, relays to Organizations |
| POST | /api/v2/conferences/{id}/reschedule | conference handler → `ConferenceService::reschedule` | bearer | v2_reschedule_conference | Public. Writes notice job |
| POST | /api/v2/conferences/{id}/cancel | → `ConferenceService::cancel` | bearer / participant_token / platformCredential | v2_cancel_conference | Public. Writes notice job |
| GET/WS | /api/v2/conferences/{id}/realtime, /events, POST …/realtime/ticket | `handlers/v2/realtime.rs` | bearer / participant_token | v2_realtime_subscribe / v2_replay_events / v2_realtime_ticket | Public |
| GET | /api/v2/me/security-events | account sessions handler | bearer | v2_list_my_security_events | Public (self-service audit trail; closest thing to a security feed) |
| POST | /api/v1/email/provider-events/resend | `handlers/email.rs::resend_events` | Svix HMAC (`RESEND_WEBHOOK_SECRET`) | — | Public provider callback (not in OpenAPI) |
| POST | /internal/operations/email/acceptance | `handlers/email.rs::acceptance_email` | `X-Internal-Token` (404 otherwise) | — | Internal ops. Arbitrary `to` with fixed template |
| GET | /internal/operations/email/deliveries/{id} | `delivery_status` | `X-Internal-Token` | — | Internal ops. PII-free status (`DeliveryStatus` L190-201) |
| POST | /internal/operations/email/developer-access-decision | `developer_access_decision` | `X-Internal-Token` | — | Internal. **Only email relay used by another component**: the operator script `infra/calling/production/developer_access.py` (not the developer-platform service). `deny_unknown_fields`, fixed template, reason code `[A-Z0-9_]{1,64}` |
| GET | /internal/metrics, /internal/rtc/metrics | `handlers/internal.rs` | `X-Internal-Token` | — | Internal (email counters) |
| ANY | /api/v2/communications/* | `communications::relay` | bearer | — | Public relay to the Hub (Hub owns notification preferences) |

**Email relay for other services:** none found. Calling, Organizations, Developer Platform and Communications do not ask meeting-service to send email. The organization-invitation email is produced by meeting-service's own client-facing relay from Organizations' response. It is not an Organizations → Meeting request.

**Exposure note (inference):**
- `/internal/operations/email/*` are mounted on the same router as public routes (`routes/mod.rs` L96-107).
- The production nginx explicitly blocks only `location ^~ /internal/v1/accounts/communications/ { return 404; }` (`communications_release.py` L391). That implies other `/internal/*` paths on `api.cfmeet.com` reach meeting-service through the default location.
- If so, these endpoints are Internet-reachable and protected solely by `INTERNAL_API_TOKEN`. `CFMEET_FINAL_PRODUCTION_COMPLETION.md` L47 asserts "none is public", which refers to the token, not the network.
- This needs verification on the VPS nginx config, which is not in the repo.

---

## 6. Background workers and schedulers (startup wiring in `src/main.rs`)

| Worker | Symbol | Wiring | Interval / batch | Concurrency / locking | Gate |
|---|---|---|---|---|---|
| Legacy Redis email import | `email_delivery::import_legacy_redis_queue` | `main.rs` L240-250 (once at boot) | up to 10,000 jobs | RPOP is atomic | always |
| Email worker | `EmailWorker::run` / `spawn_email_worker` | L251-270 | poll 1000 ms idle, batch 10, 2 msg/s per process, purge hourly | 1 task per process; SKIP LOCKED + 90s lease + lease-holder guard; no leader election | provider ≠ none |
| Notification worker | `notifications::spawn_worker` | L274-288 | 30 s, batch 20, purge 500/tick | 1 task per process; SKIP LOCKED + 120s lease; dedupe keys make reminder creation idempotent across instances | `NOTIFICATIONS_EMAIL_ENABLED` |
| Conference scheduler | `workers/conference_scheduler.rs::ConferenceScheduler` (`spawn_conference_scheduler` L153) | L308-310 | `CONFERENCE_SCHEDULER_POLL_SECS` (default 30, min 5; `config/features.rs` L186, L250) | Multi-instance safe (materialize with SKIP LOCKED / ON CONFLICT; `end` idempotent; `expire_stale` SKIP LOCKED) | `FEATURE_MEETING_V2_ENABLED` |
| Communications account-event delivery | inline loop calling `communications::deliver_due` | L218-230 | 500 ms, batch 20 | SKIP LOCKED + 120s lease; infinite retries | Communications configured |
| Outbox relay (realtime and RTC commands) | `workers/outbox_relay.rs` | L327-344 | 2 s | Leases; FAILED dead-letter | realtime or rtc control flag |
| Platform public event publisher | `services/platform/events.rs` | L129-138 | config | Leases | platform destination |

**Conference scheduler and notifications:**
- The scheduler does **not** create reminders. Its docs say: "Notifications/reminders are never authorization dependencies" (L16-17).
- Reminders are created by the notification worker (`enqueue_due_reminders`).
- The scheduler's `expire_stale` cancellations are deliberately not announced.
- Reminders for recurring series depend on the scheduler materializing occurrences (`recurrence.materialize_due`, L64; "keeps 30 days of rows"). With `FEATURE_MEETING_V2_ENABLED=false`, non-materialized occurrences get no reminders.

**Timezone handling:**
- Scheduling and reminder windows are UTC instants (`scheduled_start_at > now AND <= now+lead`).
- Display uses `chrono_tz`: recipient `users.timezone`, else `meeting_spaces.timezone`, else UTC (`format_time` L343-353).
- Org invitation expiry uses the recipient tz, else UTC (L590-598).

---

## 7. Dependencies

| Dependency | Used for | Failure behaviour |
|---|---|---|
| PostgreSQL (meeting DB) | Ledger, notification jobs, recipient ledger, device links, audit, Hub outbox | Required. Startup panics if migrations are missing (`main.rs` L163-168, L234-239) |
| Redis | OTP codes and attempt/send-rate counters (`OtpService`), invite-email budget (`rate_limiter.check_action_rate("notify:invite-email", …)` L417-425), login rate limits, legacy `email:queue` import, realtime pub/sub | Required at startup (`main.rs` L77-81 `expect`). Budget check error → invitation email `FAILED` (fail closed, `invitations.rs` L78). OTP store error → 500 after the user row exists |
| Resend HTTPS API (`api.resend.com`) | Provider | Configured only with key. Not active in prod as of 2026-11-13 |
| SMTP relay (Gmail in prod) | Provider (implicit) | Reported rejected / OTP not recovered (§0) |
| Communications Hub | Preference reads (`HubPreferences`); account-event outbox | Unavailable → notices retry → FAILED; invitation email FAILED |
| Platform Access | Principal status mirror (account events) | Retried |
| Organizations Service | Authoritative org invitation (token source) | Relay error → no email |

---

## 8. Tests covering notification behaviour

| Test | Path | Asserts |
|---|---|---|
| `ledger_is_idempotent_and_the_worker_delivers_once` | `tests/email_delivery.rs` L88 | Duplicate key returns the same id with `created=false`. One provider send. A second pass claims 0. Status ACCEPTED with provider and `accepted_at` |
| `temporary_failures_retry_with_backoff_and_permanent_ones_stop` | L117 | Temporary → QUEUED attempt 1, not due until backoff, then ACCEPTED. Permanent → FAILED with content erased. 403 exhausted (max 3) → FAILED, `last_error_status=403` |
| `expired_messages_are_not_sent_and_abandoned_leases_are_recovered` | L173 | `expires_at` passed → EXPIRED, nothing sent. Expired SENDING lease re-claimed, attempts=2. Provider id == ledger id |
| `provider_webhooks_are_authenticated_deduplicated_and_monotonic` | L211 | Unsigned/bad → 401. Valid → DELIVERED. Duplicate event → `duplicate:true`. Late DELAYED doesn't downgrade. COMPLAINED applied. No secret → 404 |
| `otp_journey_from_registration_to_session` | L345 | Register → one AUTH_VERIFICATION row → provider gets the 6-digit code → content erased → wrong 400 / right 200 / reuse 400 → login → forgot/reset → security notice |
| `developer_access_decision_notice_is_queued_once` | L461 | Internal token required. Idempotent per decision |
| Unit: `svix_signatures_are_verified`, `resend_errors_are_classified`, `resend_body_has_both_parts_and_a_category_tag`, `backoff_grows_and_is_capped`, `ranks_never_downgrade_delivered`, `recipient_hash_is_case_insensitive` | `src/services/email_delivery.rs` L1105-1200 | As named |
| Unit: config validation | `src/config/email.rs` L284-343 | Provider defaults, Resend key format, file secret, sender and webhook-secret validation, address validation |
| `s11_meeting_invitation_email_only_on_request_with_the_token_in_the_fragment` | `tests/profile_notifications.rs` L292 | Email only with `sendEmail`. Token in the `#` fragment |
| `s11_invitation_email_is_disabled_by_default_and_never_queued` | L381 | `DISABLED` with the default config |
| `s11_reschedule_and_cancel_notices_are_durable_once_and_exclude_the_actor` | L425 | Actor excluded. Subject escaping. A second tick sends nothing |
| `s11_protected_meeting_titles_never_reach_an_email` | L531 | "CF Meet meeting" title |
| `s11_reminders_once_per_start_respect_preferences_and_retry_on_unknown_preferences` | L580 | One reminder per start. Opted-out member suppressed. Concurrent passes don't duplicate. New start → new reminder. Preferences unavailable → retry then deliver. Cancelled → SKIPPED |
| `s11_a_queue_outage_retries_without_emailing_anyone_twice` | L711 | Queue failure → retry. No duplicates after recovery |
| `s11_organization_invitation_email_from_the_authoritative_answer` | `tests/organizations_authority.rs` L3278 | `sendEmail` consumed by the relay. `emailDelivery` ACCEPTED/DISABLED. Non-boolean refused |
| Device step-up | `tests/account_devices.rs` (helper `emailed_code` L233, journey around L428) | Step-up code read from `email_deliveries.text_body` with category `SECURITY_DEVICE_STEP_UP`. Completes mobile replacement |
| Unit: notifications | `src/services/notifications.rs` L1191-1257, `config/notifications.rs` L74-90, `templates/email/meeting.rs` L79-102 | tz formatting, digest normalization, RecordingMail/StaticPreferences, off-by-default, HTML escaping |
| Release tooling | `infra/calling/production/test_email_operations.py` L35-38, `test_coordinated_release.py` L418-424, L580-583 | Refuses `NOTIFICATIONS_EMAIL_ENABLED` without mail credentials |

**Not tested (found by reading):**
- Password-changed or device notices when enqueue fails.
- A poison recipient address in RECIPIENTS_SQL.
- A webhook arriving before ACCEPTED is committed.
- Multi-instance email lease expiry with SMTP.
- The Hub grant missing (403) path. It is simulated only as "unavailable".

---

## 9. Gaps, duplication, bugs and security concerns

**Bugs and correctness**

1. **Poison recipient blocks a meeting notice.** `notifications::deliver` (L1033-1067) maps any `email_delivery::enqueue` error to `MAIL_QUEUE_UNAVAILABLE`, including `BadRequest` for an address that fails `valid_address` (e.g. no `.` in the domain).
   - The job returns `Retry` on the first failing recipient, every attempt.
   - Recipients are processed in `ORDER BY email`, so everyone sorted after the bad address is never emailed.
   - The job ends FAILED after 8 attempts.
   - RECIPIENTS_SQL only checks `position('@') > 1`.
2. **Webhook/finish race.** `apply_provider_event` resolves `delivery_id` from `provider_message_id` at event insert time (L1030-1035). `provider_message_id` is written in `finish()` after the provider call returns (L655-678).
   - A fast `email.delivered` webhook can arrive before that commit. The event is stored with `delivery_id NULL` and is never re-linked.
   - Status stays ACCEPTED.
3. **Lease shorter than worst-case batch time.**
   - Email lease is 90s (L76). A batch of 10 with a 20s Resend or 30s SMTP timeout plus rate spacing can exceed 90s.
   - With more than one instance, a second worker re-claims rows still in the first worker's batch. The first worker's `finish` silently no-ops (`WHERE lease_holder=$2`).
   - With SMTP (no provider idempotency) the result is duplicate sends. With Resend it is deduplicated only within Resend's idempotency window.
   - The rate limit (`EMAIL_MAX_PER_SECOND`) is per process, not global.
4. **No email worker with `EMAIL_PROVIDER=none`.**
   - QUEUED rows never expire and never have their bodies erased (`purge` only touches `final_at` rows). OTP codes and invitation tokens persist in plaintext in Postgres indefinitely.
   - `expires_at` is only enforced when a worker claims the row.
5. **Notification purge stops when the feature is switched off.** `tick` returns before the purge when `email_enabled=false` (L796-798).
6. **Truncation before filtering.** RECIPIENTS_SQL `LIMIT max+1` plus truncation happens before removing the actor, ineligible accounts and opted-out recipients (L966-983). Alphabetically late recipients are dropped deterministically and logged only with a warning.
7. **At-most-once by design.** The digest row is committed before enqueue (separate statements, no transaction, L1006-1046). A crash between them loses that recipient silently.
8. **Category drift.** `SECURITY_DEVICE_STEP_UP` is a literal (`account_devices.rs` L1551), not in `email_delivery::category`. Device notices use the generic `NOTIFICATION` category, so security notices cannot be told apart from generic mail in the ledger.

**Security**

9. **Security notices are best-effort and non-transactional.** Password-changed (`handlers/auth.rs` L876-882, L1016-1024) and device-link notices (`notify_owner` L769-777) are enqueued after commit. Failure is only logged, and there is no outbox to retry from. Device step-up is the only transactional security email (L1548).
10. **Missing security-critical notifications:** new sign-in or new device sign-in alert, refresh-token reuse alert, device revocation notice, sign-out-everywhere notice, account deactivation/suspension/reactivation notice, email change (no flow exists), and a "device link requested" warning to the owner.
11. **OTP storage.** Account OTPs are plaintext in Redis with the plaintext email in the key name, and compared with `==` (`otp.rs` L83, L106). Device codes are hashed and constant-time (inconsistent).
12. **Enumeration.**
    - `verify_reset_token` (`handlers/auth.rs` L913-946) returns 200 vs 400 depending on whether a reset is pending for an email. Combined with `forgot-password` this reveals account existence, which contradicts the ACC-014 uniform-answer intent.
    - `register` returns 409 for an existing email (L91-98).
    - Per-IP limits only apply when `AUTH_CLIENT_IP_HEADER` is set (L22-41).
13. **Internal email endpoints are likely Internet-reachable** (§5 exposure note).
    - `acceptance_email` sends to an arbitrary address with a fixed template for anyone holding `INTERNAL_API_TOKEN`.
    - `internal_token_ok` (`handlers/internal.rs` L85-98) leaks the token length via an early length check (minor).
    - The token is a single shared static secret read from env on each call.
14. **No suppression list.** BOUNCED/COMPLAINED are recorded but not enforced on future sends (reputation risk). There is no `List-Unsubscribe` for meeting notifications.
15. **Subjects are retained.** `subject` (meeting titles, organization names) is never erased and stays up to 60 days. Bodies are erased.
16. **Unsalted SHA-256 address digests** (`recipient_hash`, `meeting_notification_recipients.recipient_digest`) are pseudonymous only.
17. **Hub preference grant missing in production tooling** (§3.4). Meeting notices and invitation emails to account holders fail closed.

**Duplication and drift**

18. Two independent durable-job systems for email with different retry policies:
    - `meeting_notifications`: backoff 2^n capped at 900s, 8 attempts, 120s lease.
    - `email_deliveries`: 5s·2^n ±20% capped at 900s, 8 attempts, 90s lease.
    - A third, `account_communication_outbox`, has no max attempts.
19. Stale docs:
    - `config/notifications.rs` L4-5 and `docs/operations/profile-notifications-sections-9-11.md` L161-163 still say the transport is "Redis `email:queue` and `EmailWorker` (`SMTP_*`)". It has been superseded by the ledger (`notifications.rs` L3-5).
    - `services/meeting-service/docs/EMAIL_QUEUE.md` describes the removed Redis design.
20. Dead code:
    - `EmailService::send_welcome_email` and `AUTH_WELCOME` have no caller.
    - `RedisEmailQueue::enqueue/pop/retry/len` are unused (only `pop_nowait` is used for import).
    - `notifications::status_counts` is not exposed.
    - `src/bin/test_email.rs` is a direct SMTP sender that bypasses the ledger.
21. No email localization. `users.locale` is unused.
22. No notification for recurrence-exception, series-edit, archive, legacy v1, member-add, webinar-registration or accept/decline events (§3.5). Recording-ready reaches integrators (platform events) but not end users.

**Implications for a centralized Notification Service** (facts only):
- Meeting-service is today the de facto single email authority.
- Templates are rendered by the producer and stored as full HTML/text in the ledger.
- Category, idempotency key and expiry are supplied per producer.
- Recipient resolution and preference checks happen inside meeting-service at delivery time.
- Preferences live in the Communications Hub.
- There is no channel abstraction beyond email (no push, SMS or in-app).
- Realtime is per-conference only.
