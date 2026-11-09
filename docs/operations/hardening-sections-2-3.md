# Sections 2–3 hardening (account security, data protection) — release and operations record

**Scope:** the findings still open after Blueprint v1.0 Section 2 ([account-security-section-2.md](account-security-section-2.md), [ADR 0057](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md)): PLT-002 (+SEC-006, DATA-003), SEC-009, SEC-015, SEC-016, SEC-017, SEC-018, ACC-016, ACC-025, ACC-026, DATA-010, DATA-012, DATA-017. Branch `blueprint/s02-s03-hardening`. **Production is not changed by this work**: everything below is staged for a release the owner approves. ACC-011 and ACC-013 (deletion/export, MFA) stay owner decision D-6 and are not touched.

## 1. Dispositions

| Finding | Disposition | What changed (evidence in §2–§8) |
|---|---|---|
| PLT-002 (P1) | ENGINEERING_COMPLETE | Legacy API keys stored as a SHA-256 verifier + 8-character lookup prefix, compared in constant time; atomic quota; existing rows backfilled additively, plaintext kept only as a read fallback until the cleanup step (§2.4) |
| DATA-003 | ENGINEERING_COMPLETE | Both secrets at rest found in the meeting-service schema (`api_keys.api_key`, `webhooks.secret`) get the same treatment; no other plaintext secret exists there (§2.3) |
| SEC-009 (P1) | ENGINEERING_COMPLETE | Legacy kick removes the participant from the LiveKit room server-side (v2 provider code reused); reconnects with an older credential are removed again by the verified webhook; legacy credential TTL 6 h → 15 min (config); no client `room_admin` |
| SEC-015 | ENGINEERING_COMPLETE | Every migration binary of every service refuses `down`/`fresh`/`refresh`/`reset` unless `APP_ENV` is non-production or `CF_MIGRATION_ALLOW_DESTRUCTIVE=<subcommand>` is set (§4). Remaining (not required by the finding): older data-bearing `down()`s that drop tables unconditionally |
| SEC-016 (P1) | PARTIAL: thresholds marked APPROVAL REQUIRED and the paging receiver are owner inputs; Prometheus/Alertmanager deployment is infrastructure | Alert rules + Alertmanager routing as code, promtool/amtool/policy validation in CI ([monitoring-alerts.md](monitoring-alerts.md)) |
| SEC-017 | EXTERNAL: inventory of served certificate lineages on the VPS | Documented only (§7); no repo-side change was clearly safe |
| SEC-018 | ENGINEERING_COMPLETE (repo); production cutover is an OWNER ACTION | meeting-service in least-privilege provisioning and isolation certification with separate migrator/runtime roles, CI proof, opt-in separate migration URL in release tooling, cutover + rollback SQL and runbook ([meeting-service-database-roles.md](meeting-service-database-roles.md)) |
| ACC-016 | OWNER_DECISION: whether and where to enforce email verification | Enforcement points built behind `EMAIL_VERIFICATION_ENFORCEMENT` (default `off`, behaviour unchanged) (§5.3) |
| ACC-025 | ENGINEERING_COMPLETE | Dead `combined_auth_middleware` and `check_email_verification` removed; passwords > 72 bytes refused (no silent bcrypt truncation); logout tolerant once the authoritative revocation is recorded |
| ACC-026 | PARTIAL: Hub profile reactivation (the Hub's deliberate terminal-deactivation trigger) | Self-deactivation needs the current password or a sign-in within 10 minutes; administrator reactivation (`PATCH /api/v1/admin/users/{id}` `status: active`) documented; both audited (§5.2) |
| DATA-010 | OWNER_DECISION: retention periods | No retention period for the open tables is decided in the repository docs, so no purge job was added; the policy table with proposed values is §6.1. The decided ones (Calling ADR 0030, Telephony, Privacy Relay, Messaging, Hub nonces, Section 11 notification jobs 30 days) were already implemented |
| DATA-012 | ENGINEERING_COMPLETE (documented canonical type and conversions; no unification was additive without new owner histories) | §6.2 |
| DATA-017 | ENGINEERING_COMPLETE (report only, never repairs) | Report-only reconciliation of the foreign ids held by the meeting-service (`reconcile_references`), Messaging (`messaging-reconcile-references`), Numbering (`numbering-reconcile-references`) and the Developer Platform (`developer-platform-reconcile-references`, plus the worker's periodic report with remote existence checks at Platform Access) (§6.3) |

## 2. Legacy secrets at rest (PLT-002, SEC-006, DATA-003)

### 2.1 Design

* New additive history `account_hardening_migrations` (`m20261109_000001_account_hardening`, binary `/app/account-hardening-migration`): nullable `api_keys.key_hash` (unique when present), `api_keys.key_prefix` (indexed), `webhooks.secret_sha256`; backfill of every existing row in SQL. No existing column changes; the previous image keeps starting (its readiness checks only the histories it knows).
* Verifier: `hex(SHA-256("cfmeet-legacy-api-key:v1|" + key))`. Keys are 64 random alphanumerics (~380 bits), so an unkeyed hash is a sufficient verifier and the backfill needs no server secret. The domain-separation strings are part of the stored format.
* Lookup: candidates by the non-secret prefix, verifier compared in constant time over every candidate (`services/legacy_credentials.rs`). Rows an older image writes after the migration (a rollback window) have no verifier: they are found once by the plaintext fallback and healed.
* New keys store a placeholder in `api_keys.api_key` (`U+0001` + `h:` + key id): unique, never a valid HTTP header value, so neither image's plaintext lookup can ever match it. New webhooks store a constant placeholder in `webhooks.secret`; legacy webhooks are never delivered (DATA-007), so the secret is never needed again and only a fingerprint is kept.
* Quota: one statement — `UPDATE api_keys SET used_count = used_count + 1 … WHERE id = $1 AND status = 'active' AND expires_at > $2 AND used_count < usage_limit RETURNING used_count` — so concurrent requests can neither lose increments nor exceed the limit (DB test: 20 concurrent calls against a limit of 5 → exactly 5 succeed).
* Reads show only the prefix (`api_key_masked` = 8 characters + `********`); before, the last four characters were shown.

### 2.2 API impact

`/api/v1/api-keys*` and `/api/v1/webhooks` keep their shapes. The key and the webhook secret are still returned once at creation and can never be shown again (already documented). Only the masked display format changed (contract text updated). The legacy key API stays deprecated in favour of Platform Access credentials ([legacy-api-keys-deprecation.md](../api/legacy-api-keys-deprecation.md)).

### 2.3 Inventory of secrets at rest in the meeting-service schema

| Column | State |
|---|---|
| `api_keys.api_key` | was plaintext → verifier (this change) |
| `webhooks.secret` | was plaintext → fingerprint (this change) |
| `users.password_hash` | bcrypt |
| `meeting_spaces.passcode_hash` | bcrypt |
| `meeting_invitations.token_hash`, `conference_participants.credential_hash` | SHA-256 of 256-bit bearer secrets |
| `account_sessions.current_refresh_id` | identifier inside a signed refresh token; useless without the signing key |
| OTP / reset codes, OAuth state, exchange codes | Redis with expiry (removed from PostgreSQL by `m20260323_000001_migrate_to_redis`) |
| `JWT_SECRET`, LiveKit, SMTP, Google client secret | environment / secret store, never in the database |

### 2.4 Cleanup (contract step, dated plan — NOT in this change)

After every running meeting-service image reads verifiers (this release on production, rollback window closed — proposed no earlier than **2026-12-15**, owner to confirm):

1. Encrypted pre-migration snapshot (`infra/deployment/backup/pre-migration-snapshot.sh`).
2. A new migration in a new history: `UPDATE api_keys SET api_key = chr(1) || 'h:' || replace(id::text, '-', '') WHERE left(api_key, 1) <> chr(1) AND key_hash IS NOT NULL;` and `UPDATE webhooks SET secret = chr(1) || 'sha256' WHERE left(secret, 1) <> chr(1) AND secret_sha256 IS NOT NULL;` (one-way; keys keep working through the verifier).
3. Remove the plaintext fallback from `legacy_credentials::find_api_key`.
4. Dropping the columns (or the whole legacy key API) belongs to the Section 16 retirement (D-9: measured zero consumers).

## 3. Legacy media eviction (SEC-009)

* `POST /api/v1/participants/{id}/kick` now also calls the v2 provider contract `RtcProvider::remove_participant` (`services/legacy_media.rs`; already-absent = success). The session log records `media_evicted`. A provider outage never undoes the kick (the database state stands; join refuses kicked participants).
* A kicked participant reconnecting with a credential issued before the kick: the verified LiveKit webhook (`participant_joined` for an unknown — i.e. legacy — room) finds the kicked legacy participant and removes it again (`rtc_webhook_events.outcome = LEGACY_REMOVED_EVICTED`).
* Legacy credentials live `LEGACY_RTC_TOKEN_TTL_SECS` (default **900**, bounded 60–21600; was a fixed 6 h). Connected participants are unaffected (LiveKit refreshes connected participants' tokens); a reload rejoins through the join endpoint, which issues a fresh credential.
* Hosts no longer get `room_admin` (nor record/create/list) in their client credential, the same rule as the v2 `LiveKitProvider::grants_for`. Checked read-only: neither `apps/web` nor `apps/mobile` uses a LiveKit admin API (no `RoomServiceClient`, no client-side remove/mute of others); host controls are server API calls.
* The preferred long-term fix is unchanged: `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` once legacy clients are gone (release decision).

## 4. Destructive migration subcommands (SEC-015)

Shared std-only crate `packages/migration-guard` (`cf-migration-guard`), called first by all 17 migration binaries (and the new one). `down`, `fresh`, `refresh`, `reset` exit 2 before any connection unless `APP_ENV` ∈ {dev, development, local, test, ci} or `CF_MIGRATION_ALLOW_DESTRUCTIVE=<the same subcommand>` (e.g. `CF_MIGRATION_ALLOW_DESTRUCTIVE=down organizations-migration down -n 1`). `up`, `status`, `generate`, help are never affected; `.env` files cannot unlock it; `staging` counts as production. CI smoke steps set `APP_ENV=test`; `scripts/migrations/assert_destructive_guard.sh` and `services/meeting-service/migration/tests/destructive_guard.rs` prove the refusal. Documented rollbacks that rely on `down -n 1` now show the override ([migration-conventions.md](../architecture/migration-conventions.md)).

## 5. Account security

### 5.1 ACC-025 hygiene

* Removed: `combined_auth_middleware` (accepted any JWT without blacklist, status or epoch checks; unused) and `check_email_verification` (unused; replaced by §5.3).
* Passwords: registration, reset and change accept 8–72 characters and refuse more than 72 bytes (bcrypt reads only 72); existing hashes still verify.
* Logout: the authoritative revocation (session row, or account epoch for pre-session tokens) runs first; the Redis blacklist afterwards is best effort, so a recorded logout is never reported as a failure (guest tokens, which have no session row, still fail closed).

### 5.2 ACC-026 deactivation

* `POST /api/v1/users/me/deactivate` takes an optional body `{ "current_password": "…" }`. It succeeds with the correct current password, or without one when the bearer token comes from a sign-in made within `ACCOUNT_REAUTH_MAX_AGE_SECS` (default **600**, bounded 60–3600) — a session token's `auth_time` (ADR 0057: never moved by refresh; the session is checked open). Otherwise `403 REAUTHENTICATION_REQUIRED` and nothing changes. A wrong password never falls back to the recent-sign-in rule. Google-only accounts re-authenticate by signing in again.
* Audit: `ACCOUNT_DEACTIVATED` (SUCCESS with `reauthentication` = PASSWORD / RECENT_SIGN_IN / NOT_REQUIRED, or DENIED); administrator changes `ACCOUNT_STATUS_CHANGED` (`from`, `to`, `reactivated`) and `ACCOUNT_ROLE_CHANGED`, actor ADMIN.
* Reactivation: `PATCH /api/v1/admin/users/{id}` with `status: active` (existing route, now audited and documented). The user signs in again (earlier sessions stay ended); Platform Access mirrors ACTIVE (Section 14). **Open:** a Communications Hub profile that was DEACTIVATED stays DEACTIVATED — `communication_profile_identity_guard` raises "deactivation is terminal". Reversing that is a Hub schema change (a fourth Hub history relaxing the trigger for an Account `STATUS_CHANGED` with a newer `sourceVersion`) plus a product decision that deactivation is not terminal in the Hub.
* `ACCOUNT_DEACTIVATION_REQUIRE_REAUTH=false` turns the requirement off (emergency lever only).
* **Frontend handoff:** Settings → Danger Zone should send `current_password` (password accounts) or ask the user to sign in again on `REAUTHENTICATION_REQUIRED`. Until then, deactivation works within 10 minutes of signing in.

### 5.3 ACC-016 email verification (OWNER_DECISION)

`EMAIL_VERIFICATION_ENFORCEMENT` = `off` (default, unchanged behaviour) | `communications` | `login`, effective only while the existing `EMAIL_VERIFICATION_REQUIRED` is true (its default, true, is unchanged):

| Point | `off` | `communications` | `login` |
|---|---|---|---|
| Communications Hub relay `/api/v2/communications/**` (profile activation, calls, devices) | allowed | `403 EMAIL_VERIFICATION_REQUIRED` without a verified address | same |
| Messaging bridge `/api/v2/messaging/**` (devices, client tokens, relay) | allowed | refused | refused |
| Password sign-in of a `pending_verification` account | allowed | allowed | `403 EMAIL_VERIFICATION_REQUIRED` after a correct password (no oracle for others) |
| Meetings (v1/v2) | allowed | allowed | sign-in refused, so effectively blocked |

"Verified" is the Section 9 rule (`users::Model::email_verified`): Google sign-in, a recorded verification, or an active account where verification is required. Owner steps: decide the level; before `login`, check how many accounts are pending; certification fixtures that use never-verified accounts must verify them first.

## 6. Data protection

### 6.1 DATA-010 retention policy (OWNER_DECISION — nothing is purged by this change)

| Table (service) | Growth | Proposed policy | Decided? |
|---|---|---|---|
| `outbox_events` (meeting) PUBLISHED / DEAD | per meeting event | delete PUBLISHED after 30 days, DEAD after 90 | no |
| `account_communication_outbox` (meeting) delivered | per account event | delete delivered after 30 days | no |
| `rtc_webhook_events` (meeting) | per provider callback | delete after 30 days (dedupe window ≪ 30 days) | no |
| `rtc_drift_events` (meeting) resolved | per incident | delete resolved after 365 days (as Calling incidents, ADR 0030) | no |
| `session_logs` (meeting, legacy) | per legacy join/leave | 365 days, or with legacy retirement | no |
| `chat_messages` / `conference_messages` (meeting) | per message | product decision (customer data; per-space `retention_policy_ref` exists, unused) | no |
| `account_sessions` (meeting) revoked / expired | per sign-in | delete 90 days after end | no |
| `account_security_events` (meeting) | per security event | append-only by trigger; 2555 days like Calling operator audit; purge needs a controlled trigger exception | no |
| `devplat_activity_events`, `devplat_usage_facts`, `devplat_webhook_attempts` (developer-platform) | append-only by trigger | billing/audit period decision | no |
| `platform_access_audit`, `platform_identity_changes` (platform-access) | append-only | audit period; the change feed must outlive every consumer's cursor | no |
| `meeting_notifications` (meeting) finished | — | 30 days | **yes** (Section 11; implemented) |
| Calling, Telephony, Privacy Relay, Messaging, Hub nonce/idempotency | — | per their ADRs | **yes** (implemented) |

When the owner decides, each row becomes a config-driven worker (period unset = off, dry-run first), batched with `SKIP LOCKED` like the Calling retention workers.

### 6.2 DATA-012 `credentials_not_before`

| Where | Type | Meaning |
|---|---|---|
| meeting `account_security_state`, `account_communication_state` | `TIMESTAMPTZ` | account epoch (authority) |
| Hub `communication_profiles` | `TIMESTAMPTZ` | projection |
| Hub → owners event `communications.account.changed.credentialsNotBefore` | integer Unix seconds | wire format |
| `calling_communication_revocations`, `messaging_communication_revocations` | `BIGINT` Unix seconds | owner copy (greatest wins) |

Canonical: **`TIMESTAMPTZ` at the authority, integer Unix seconds (UTC, truncated) on the wire and in owner copies.** Conversions: authority → wire `timestamp()` (truncates sub-seconds); owner check `credentials_not_before >= token issued-at seconds` refuses a credential issued in the same second as the epoch (inclusive, fail-closed); the account authority compares session tokens strictly and pre-session tokens inclusively (ADR 0057). A generated `TIMESTAMPTZ` column in Calling/Messaging would need a new owner history in each service for no behavioural gain, so it was not added.

### 6.3 DATA-017 reconciliation (report only)

`cargo run --bin reconcile_references` (or `backend::services::reference_reconciliation::report`) reads one READ ONLY snapshot and prints JSON: per foreign-id holder (Platform Access principals/environments/tenants, Messaging conversations) the rows, distinct ids and local-invariant anomalies — principal links without an account, chat owners / members / pending revocations using principals this service never mapped, a Platform Access environment seen with more than one tenant/application. Exit 1 when anomalies exist; nothing is repaired. `remoteVerification: NOT_PERFORMED` — existence at the owner (Platform Access `/internal/v1/identity/changes`, as Calling's projection consumes it; Messaging conversation reads) is the next step for the meeting-service.

The same report shape (`service`, `checks[]` with `name` / `owner` / `heldIn` / `rows` / `distinctForeignIds` / `anomalies` / `status`, total `anomalies`, `remoteVerification`; exit 0 consistent, 1 anomalies, 2-4 usage / connection / query failure) is served by:

| Service | Command | Local invariants checked (anomalies) | Remote |
|---|---|---|---|
| Messaging | `cargo run --bin messaging-reconcile-references` (`messaging::reference_reconciliation::report`) | an environment recorded under more than one tenant / application; a member in another environment than its conversation; a client session whose scope / principal differs from its device's; an SMS bridge whose scope differs from its conversation's; a Privacy Relay-controlled conversation still open after its authority was REVOKED (or ACTIVE while SUSPENDED). Inventory: member / owner / device principals, bridged Numbering number resources | NOT_PERFORMED |
| Numbering | `cargo run --bin numbering-reconcile-references` (`numbering::reference_reconciliation::report`) | an environment recorded under more than one tenant / application (14 holders); an ORGANIZATION-owned number whose owner reference is not the organization (tenant) in assignments, history and release requests; a PRINCIPAL owner reference equal to a tenant / application / environment id; history naming another owner than its assignment. Inventory: reservation / routing-grant principals, KYC subjects, Privacy Relay lease holders | NOT_PERFORMED |
| Developer Platform | `developer-platform-reconcile-references` (in the image) and the worker every `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS` (default 86400; 0 = off; logged, `devplat_worker_schedules`) | an environment recorded under more than one tenant / application; an environment profile whose tenant is not its project's; subscriptions, webhook events, quota restrictions, usage facts and credential lifecycle notices whose environment is known under another tenant / application; a LIVE production environment that is not a known environment of the request's project. Inventory: workspace member accounts (Meeting Service) | with service authentication configured: each organization, project and environment read at Platform Access (`DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT`, default 1000 of each): `MISSING` (404) / `MISMATCHED` (other parent) findings, `PARTIAL` when Platform Access does not answer |

Nothing is written by any of them (READ ONLY transactions; the Developer Platform worker records only its schedule row). An anomaly is for an operator to investigate with the owning service.

## 7. TLS (SEC-017) — documentation only

[tls-certificate-automation.md](tls-certificate-automation.md) covers the `api.cfmeet.com` lineage. Other names in the repository (`livekit.cfmeet.com`, `rtc.cfmeet.com`, `internal-docs.cfmeet.com`) may be served with their own certificates. Owner/operator step: run the read-only `private-tls-inspect` operation (lists every certificate consumer and lineage on the host), then extend the monitor to each lineage found. No repo change was made because the lineages that exist are not knowable from the repository.

## 8. Release (owner-approved; nothing here is done by this branch)

Order:

1. Encrypted pre-migration snapshot of the account database and a passed restore drill (`backup-and-restore.md`; the backup catalog now lists `account_hardening_migrations`).
2. `migration up` → `account-security-migration up` → `meetings-calendar-migration up` → `profile-notifications-migration up` → **`account-hardening-migration up`** → (`account-communications-migration up` where Communications is configured). `infra/deployment/backend_runtime.py` and `communications_release.py` already list the new binary in this order. The new image refuses to start until the history is applied.
3. Deploy the image. Configuration (all optional; defaults shown):

| Variable | Default | Effect |
|---|---|---|
| `LEGACY_RTC_TOKEN_TTL_SECS` | 900 | legacy LiveKit credential lifetime (60–21600) |
| `ACCOUNT_DEACTIVATION_REQUIRE_REAUTH` | true | ACC-026 requirement (emergency off switch) |
| `ACCOUNT_REAUTH_MAX_AGE_SECS` | 600 | recent-sign-in window (60–3600) |
| `EMAIL_VERIFICATION_ENFORCEMENT` | off | ACC-016 enforcement level (owner decision) |
| `CF_MIGRATION_ALLOW_DESTRUCTIVE` | unset | per-command override for `down`/`fresh`/`refresh`/`reset` |

4. Announce: legacy API key displays change to the prefix form; Danger Zone needs re-authentication (§5.2).
5. Verify: run `reconcile_references` read-only and record the counts; check `SELECT count(*) FROM api_keys WHERE key_hash IS NULL` is 0 after the migration.

Rollback: redeploy the previous image (it ignores the new nullable columns and the new history). Keys created by the new image do not work on the previous image (they have no plaintext) — they keep working again after roll-forward. `account-hardening-migration down` is guarded (SEC-015) and refuses once such keys exist. SEC-018 cutover and SEC-016 monitoring have their own runbooks.

## 9. Still open

* SEC-016: threshold approval, receiver choice and credentials, Prometheus/Alertmanager deployment (owner/infrastructure).
* SEC-018: production cutover (owner action, runbook ready); production DB/role names and PostgreSQL version unknown to the repository.
* SEC-017: lineage inventory on the VPS.
* ACC-026: Hub reactivation (Hub migration + product decision).
* ACC-016, DATA-010: owner decisions.
* DATA-017: remote existence checks for the meeting-service, Messaging and Numbering reports (the Developer Platform's are built).
* PLT-002/DATA-003: the dated cleanup step (§2.4).
* SEC-015 follow-up: data-refusing `down()`s for older migrations.
