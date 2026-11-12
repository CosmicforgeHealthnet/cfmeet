# CF Meet — final production completion

Date: 2026-11-12 · Environment: production (`api.cfmeet.com`, Contabo VPS) · Baseline: [CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md](CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md)

Status vocabulary: `ACTIVE_VERIFIED` (production evidence cited) · `IMPLEMENTED_PENDING_CONFIGURATION` · `PENDING_OWNER_APPROVAL` · `PENDING_EXTERNAL_ASSESSMENT` · `DEFERRED_BY_OWNER` · `FAILED`.

## 1. Executive summary

| Item | Status | Evidence |
|---|---|---|
| CI failures A–E on PR #65 (secret scan, Developer Platform image, real-service journeys, SDK parity, Chromium on ubuntu 26.04) and the Rust 1.100 `redis` break | Fixed | §17 |
| PR #65 → `staging` · PR #66 → `main` | Merged | #65 merge `21976ecd90dfe2a2927b19d495b248014b2beaa9`; #66 merge `1e9364ce6590f6e8f4b2ba68da5bc2b13d03046f` |
| Production release (Meeting Service and Developer Platform; additive migration `lifecycle_delivery_migrations`) | `ACTIVE_VERIFIED` | release [43309857124](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43309857124) at `f2f9693489bd1289935b695d5d83e6db4d22eb2f`; 30/30 verification checks |
| Instant conferences end after 10 empty minutes | `ACTIVE_VERIFIED` | 15 stale empty instant conferences ended `EMPTY_TIMEOUT`; 0 LIVE instant conferences without a session for over 10 minutes (§7) |
| Archived spaces keep no LIVE conference | `ACTIVE_VERIFIED` | `liveInArchivedSpaces = 0`; `swy-hayb-mzk` ENDED (§8) |
| Durable email ledger, worker and Resend adapter | Deployed, Resend inactive: `IMPLEMENTED_PENDING_CONFIGURATION` | the ledger exists and is empty; SMTP is still the transport; no Resend key is mounted (§6) |
| Developer Platform production access (named reviewers, no self-approval, manual only) | Deployed, submissions disabled: `PENDING_OWNER_APPROVAL` | `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false`; developer acceptance 30/30 (§9) |
| Telecom (SMS/phone OTP, PSTN/SIP, numbers, Privacy Relay) | `DEFERRED_BY_OWNER` | unchanged safe defaults |
| Full-platform `certify` at the `main` head | See §20 | — |

## 2. Starting production state

Before this release ([inspect 43291592091](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43291592091) and [diagnose 43302325314](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43302325314)):

- **Release.** All 13 containers ran release `4904ee33`. During this work a parallel stream released `9684346e` (Q&A restore and participant identity, all 13 services, [run 43301863683](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43301863683)). That release is the base this release was scoped against.
- **Mail.**
  - `EMAIL_PROVIDER` is unset, so SMTP is used implicitly: `smtp.gmail.com:587`, a Gmail username and sender, and a password configured.
  - No Resend key and no webhook secret.
  - Over the 168-hour window of the then-current container logs, `diagnose` counted 0 SMTP `535`/BadCredentials, 0 drops after retries and 0 SMTP sends. Those containers had been recreated by the releases, so earlier `535` failures are not in the window.
  - The legacy Redis `email:queue` held 0 jobs.
- **Ledger and migrations.** The Meeting database had no `lifecycle_delivery_migrations` history.
- **Diagnose failure.** The pre-release `diagnose` stopped before the conference-lifecycle section. Its ledger query relied on `CASE WHEN to_regclass(...)`, which PostgreSQL cannot use to hide a missing table, because it resolves relations at parse time. This was fixed in [CosmicforgeHealthnet/cfmeet#67](https://github.com/CosmicforgeHealthnet/cfmeet/pull/67) and promoted by [CosmicforgeHealthnet/cfmeet#68](https://github.com/CosmicforgeHealthnet/cfmeet/pull/68). The lifecycle state of `swy-hayb-mzk` before the release therefore comes from the owner's report: LIVE, no participants, space archived on 2026-11-12 00:23. The post-release record in §7 and §8 shows how it was ended.
- **Resources.** `memAvailable` was 5272 MiB, leaving 4440 MiB after the release (gate: 832 MiB needed plus a 1024 MiB margin).

## 3. Work completed per service

### Meeting Service

| Area | What was built | Where |
|---|---|---|
| Unified email delivery | Durable PostgreSQL ledger `email_deliveries` (unique idempotency key, recipient SHA-256, status, attempts, lease, provider id, error class, expiry, timestamps). It replaces the volatile Redis `email:queue`, whose `BRPOP` lost a job when a worker died and which dropped jobs after 3 tries. A one-time import moves jobs left in the Redis list into the ledger. | `services/meeting-service/src/services/email_delivery.rs`, migration `m20261112_000001_email_delivery_and_presence` (own history `lifecycle_delivery_migrations`, binary `lifecycle-delivery-migration`) |
| Provider interface | `EmailProvider` with three implementations: `ResendProvider` (HTTPS `POST /emails`, ledger id as `Idempotency-Key`, error classification), `SmtpProvider` (legacy; `535` is classed as an operator-fixable temporary error) and `RecordingProvider` (tests). The provider is chosen by `EMAIL_PROVIDER` (`resend` / `smtp` / `none`). The configuration fails closed: a selected provider with missing or malformed settings stops startup. | `src/config/email.rs`, `email_delivery.rs` |
| Worker | Claims due rows `FOR UPDATE SKIP LOCKED`, which is safe with several instances. A 90 s lease lets another worker re-claim a message after a crash. Rate limit is `EMAIL_MAX_PER_SECOND` (default 2). Retries use exponential backoff (5 s → 15 min, ±20 % jitter) and honour `Retry-After`. Up to `EMAIL_MAX_ATTEMPTS` (8) attempts. A message is never sent after its `expires_at` (an expired code). Content is erased when a row is final; addresses are erased after 30 days and rows deleted after 60. | `EmailWorker` |
| Provider webhooks | `POST /api/v1/email/provider-events/resend`. Svix signature (HMAC-SHA256 over `id.timestamp.body`, ±5 min). Duplicate event ids are ignored via `email_delivery_events`. Status never moves backwards (DELIVERED is never set back to DELAYED). Returns 404 when no secret is configured. | `handlers/email.rs` |
| Templates | One CF Meet layout: logo, title, primary action, footer. Responsive and accessible, with a plain-text alternative. Every interpolated value is escaped; this was not the case before. Codes are shown large. | `src/templates/email/{base,account,meeting}.rs` |
| Security notices | "Your CF Meet password was changed" after a reset or a change. | `handlers/auth.rs` |
| Operations | `POST /internal/operations/email/acceptance`, `GET /internal/operations/email/deliveries/{id}` (no PII) and `POST /internal/operations/email/developer-access-decision`. All need `X-Internal-Token` and none is public. | `handlers/email.rs` |
| Empty instant conferences | `EmptyConferenceSweeper`, run every 30 s. Presence comes from the provider (LiveKit participants, recorders excluded). A PENDING session younger than 2 minutes counts as someone joining. The durable empty interval is stored in `conference_presence`. The end is re-checked under the conference row lock, so a racing join wins. The end itself uses the canonical path. | `src/workers/empty_conference.rs`, `ConferenceService::end_if_still_empty` |
| Archive | `MeetingSpaceService::archive` ends every LIVE conference in the archive transaction. It locks every unfinished conference of the space, so a racing start waits and is then refused. A repeated archive repairs spaces archived earlier, and the sweeper repairs any LIVE conference left in an archived space. | `meeting_space.rs`, `ConferenceService::end_live_for_archive` |
| Shared end path | `request_end_locked`: LIVE → ENDING, stop recordings, audit, `conference.ending` and the provider terminate command, all in one transaction. ENDED and `conference.ended` follow provider confirmation. Host end, archive and the empty timeout all use it. | `conference.rs` |
| Gradual email verification | `EMAIL_VERIFICATION_ENFORCE_FROM` / `EMAIL_VERIFICATION_GRACE_UNTIL` limit `login` enforcement to new accounts, with an optional end of the grace period for existing ones. | `account_security.rs` |
| Metrics | Label-free counters, for example `email_enqueued`, `email_provider_accepted`, `email_failed`, `email_delivered`, `email_bounced`, `email_webhook_rejected`, `conferences_empty_ended`, `conferences_archive_ended`, `conference_presence_checks_unknown`. | `ops_metrics.rs` (`/internal/metrics`) |

### Developer Platform

| Area | What was built |
|---|---|
| Explicit reviewer roles | `DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`. When set, only these named operators may review, approve, reject, suspend or reinstate; anyone else gets `422 POLICY_VIOLATION`. |
| Submission safety | Without at least one named reviewer, `…_PRODUCTION_ACCESS_WORKFLOW_ENABLED=true` is downgraded to off: submissions stay disabled and the service still starts. `…_PRODUCTION_AUTO_APPROVAL=true` is ignored with a warning (manual approval only). |
| Review tool | `developer-platform-operator` (in the Developer Platform image). It signs as `cfmeet.operator-console` and sends `x-cf-operator` and `x-cf-operator-user-id`. Supported commands: `list`, `get`, `review`, `approve`, and `reject` (a reason code is mandatory). |

### Release tooling (`infra/calling/production`)

| Operation | Purpose |
|---|---|
| `diagnose` | Read-only report of mail configuration and failure evidence, the email ledger, the conference lifecycle, the reported meeting code and the Organizations inventory. |
| `activate-resend-email` | Switches the Meeting Service to Resend only after Resend accepts a controlled acceptance email (and reports it delivered, when the webhook is configured). Any failure restores the previous container. |
| `email-acceptance` (`confirm=otp`) | Repeatable acceptance run and the public OTP journey. |
| `activate-production-access-review` and `developer-access-review` | Create the operator-console identity and its grant, name the reviewers, enable submissions, and run audited decisions with an email to the applicant. |
| Release scope | Replaces only the images whose build inputs changed. Every other service keeps its container, after a check that it runs exactly the image recorded for the previous release. |

## 4. Resend architecture and provider configuration

```text
business event → EmailService (backend-owned template, category, idempotency key)
  → email_deliveries (QUEUED)                         [PostgreSQL, durable]
  → EmailWorker (lease, rate limit, backoff, expiry)
  → ResendProvider (HTTPS, Bearer key from a 0400 file, Idempotency-Key = ledger id)
  → ACCEPTED (provider id)            ← HTTP 2xx only: "accepted", not "received"
  → Resend webhook (Svix-signed) → DELIVERED | DELAYED | BOUNCED | COMPLAINED | FAILED
```

- Sender: `CF Meet <no-reply@cfmeet.com>`. `EMAIL_FROM_ADDRESS` and `EMAIL_FROM_NAME` are validated: no CR/LF and no display-name injection.
- The key is read from a file (`RESEND_API_KEY_FILE`) and is never held in an environment variable.
- Resend is a replaceable transport. Templates, OTPs, recipient rules, retries and idempotency stay in CF Meet.
- The Meeting Service is the only email sender in CF Meet. No other service sends email, so no event can be emailed twice by two services.
- DNS and the owner steps are in [resend-email-owner-handoff.md](resend-email-owner-handoff.md). The public DNS checked on 2026-11-12 shows Hostinger nameservers and no MX, SPF, DKIM or DMARC records yet for `cfmeet.com`.

## 5. Email notification inventory

| Category | Trigger | Idempotency | Expiry |
|---|---|---|---|
| `AUTH_VERIFICATION` | register, resend-verification | address hash + code digest | 24 h (code TTL) |
| `AUTH_PASSWORD_RESET` | forgot-password | address hash + code digest | 1 h |
| `SECURITY_PASSWORD_CHANGED` | reset-password, change-password | user + timestamp | 2 days |
| `AUTH_WELCOME` | available (`send_welcome_email`), not triggered | user | 2 days |
| `MEETING_INVITATION` | v2 invitation with `sendEmail` | per request | — |
| `MEETING_NOTIFICATION` | reschedule / cancel notices, reminders (`NOTIFICATIONS_EMAIL_ENABLED`) | job + recipient digest (plus the existing per-recipient ledger) | — |
| `ORGANIZATION_INVITATION` | organization invitation relay with `sendEmail` | per request | — |
| `DEVELOPER_PLATFORM` | production-access approve / reject (operator console) | request + decision | 3 days |
| `OPERATIONS_ACCEPTANCE` | release acceptance | run reference | 1 h |
| `LEGACY_QUEUE` | jobs found in the old Redis list at upgrade | legacy job id | 1 h |

Not created, because no product workflow exists for them:
- email-address change;
- new-sign-in alerts;
- Communications Hub email (the Hub only owns preferences);
- Developer Platform invitations, which are accepted in the console by the invited verified address.

Credential lifecycle warnings stay customer webhooks.

## 6. OTP recovery evidence

Production after the release ([diagnose 43317264046](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317264046)):
- `emailLedger` is present (all counts 0).
- `emailProvider` is `smtp (implicit)`: the existing transport, preserved as instructed.
- `resendKeyMounted` is `false`.

Every new email is now queued durably in the ledger and sent by the worker through SMTP. If Gmail keeps rejecting the credentials, messages stay in the ledger with a temporary error class. They are retried and expire with their code, and are no longer silently dropped from Redis.

OTP delivery is not yet recovered. Recovery needs the owner's Resend setup ([resend-email-owner-handoff.md](resend-email-owner-handoff.md)), then `activate-resend-email` and `email-acceptance confirm=otp`. Status: `IMPLEMENTED_PENDING_CONFIGURATION`.

Pre-production evidence: `tests/email_delivery.rs::otp_journey_from_registration_to_session` runs the whole journey against PostgreSQL and Redis:
1. Register.
2. The ledger holds one `AUTH_VERIFICATION` message.
3. The provider receives the 6-digit code.
4. The message content is erased.
5. A wrong code gets 400, the right code gets 200, and a reused code gets 400.
6. Login returns tokens.
7. Forgot-password sends a reset code; the reset succeeds and the security notice is sent.

Further tests cover retries, permanent rejection, attempts running out, expiry, lease recovery, idempotency and webhook authentication.

## 7. Instant-meeting lifecycle corrections

Production after the release (Meeting Service started at 22:07:50 UTC; [diagnose 43317264046](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317264046) at 22:10 UTC):

| Measure | Value |
|---|---|
| `emptyTimeoutEnded24h` (only the new sweeper writes `EMPTY_TIMEOUT`) | **15** |
| `liveInstantWithoutActiveSessionOver10Min` | **0** |
| `liveByKind` | `{}` (no LIVE conference left) |
| `endingConferences` | 0 (every provider termination confirmed) |

The occupied case, the reset on rejoin, `conference.ended` and the Home/agenda state are verified by the real-PostgreSQL tests listed below. They run in the Meeting Service CI of the released head.

In production, the [meetings acceptance run 43317535902](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317535902) passed after this release: a real two-participant LiveKit room with hands, spotlight and reconnect. That confirms the replaced Meeting Service still runs live meetings. It does not exercise the 10-minute timer. No production meeting was deliberately held occupied, or empty, for 10 minutes. The production evidence for the timer is the 15 real stale conferences it ended.

Rules as built:
- The interval starts at creation.
- A join clears it, and a departure starts a fresh one.
- 10 continuous minutes (`CONFERENCE_EMPTY_TIMEOUT_SECS`, bounded 120 s to 24 h) leads to `EMPTY_TIMEOUT`.
- A conference with someone present is never ended.
- Scheduled and recurring conferences are not affected.
- There is no plan-based limit.
- When presence is unknown (the provider is unreachable), nothing changes.

Restart safety: the interval is stored in `conference_presence`. When it is first observed, it starts at the latest evidence of activity: the conference start, the last observed occupancy, or the last session establishment or disconnection. A restart therefore never shortens it.

Tests (`tests/lifecycle_completion.rs`, real `LiveKitProvider` against the mock RoomService):
- `never_joined_instant_conference_ends_after_ten_minutes`
- `occupied_conference_is_never_ended_and_leaving_starts_a_fresh_interval`
- `a_join_racing_the_sweeper_resets_the_interval`
- `scheduled_conferences_are_not_subject_to_the_empty_rule`

## 8. Archiving lifecycle corrections

Production: `liveInArchivedSpaces = 0`. `swy-hayb-mzk` (space ARCHIVED at 2026-11-12 00:23:22 UTC) has conference status ENDED:
- `endingRequestedAt` 22:07:47.87;
- `endedAt` 22:07:47.99;
- reason `ADMIN`, the archive-repair path;
- 0 active sessions.

The sweeper repaired it on its first pass after the Meeting Service started.

Invariant: **an archived meeting space has zero LIVE conferences.**

It is enforced in four places:
1. The archive transaction (LIVE → ENDING, committed with `ARCHIVED`).
2. Start refuses an archived space; the row locks make a racing start wait.
3. A repeated archive repairs the space.
4. The sweeper repairs any LIVE conference whose space is ARCHIVED.

Provider room termination runs after commit. It is retried durably by the outbox relay and the reconciler, and while it is pending the conference stays ENDING, so joins and credentials are refused.

Tests:
- `archiving_a_space_ends_its_live_conference_with_participants_connected`: the provider room is terminated, the conference is ENDED, `conference.ending` and `conference.ended` are emitted, history is kept, and new conferences are refused.
- `unauthorized_archive_changes_nothing`
- `concurrent_archives_end_once`
- `sweeper_repairs_live_conferences_of_archived_spaces`

## 9. Developer Platform activation

Production state:
- `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false`.
- No reviewer configured: `vars.CFMEET_PRODUCTION_ACCESS_REVIEWERS` is not set, so production-access submissions stay disabled, as instructed.
- After the release, developer acceptance passed 30/30 in [run 43317264046](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317264046): console, credentials limited to their scopes, gateway scope enforcement, a forged credential refused, tenant isolation, a signed webhook delivered, and a revoked credential refused.
- The release's own regression checks passed.

Enabling submissions is `activate-production-access-review`, after the owner names reviewers. Status: `PENDING_OWNER_APPROVAL`.

Manual approval only:
- The `production` environment of GitHub Actions gates who can dispatch.
- The Developer Platform checks that the operator is a named reviewer and that the operator is not a member of the requesting organization.
- Every decision is audited in `devplat_activity_events` (existing).
- The applicant is emailed through the unified ledger.

## 10. Organizations activation outcome

Inventory from `diagnose` 43317264046:
- Legacy dataset: 0 organizations, 0 memberships, 0 duplicates, 0 orphans.
- Organizations service: 0 organizations, memberships, invitations or import runs.
- Authority mode: `legacy`.

The runbook has no empty-dataset path. The import requires:
- `offsite-backup-verified`;
- a backup less than 24 hours old with restore tests;
- the off-host restore-drill record.

The shadow phase requires 7 days of zero-mismatch evidence, or a period the owner sets (runbook §5). The tooling does not enforce the 7 days mechanically; `org-cutover-authoritative` needs the owner's `quiesced` + `offsite-backup-verified`.

With a verified-empty dataset there is nothing to migrate. But the import gate still requires a verified off-host backup, and that cannot be honestly satisfied without the owner's age key. Bypassing it would weaken an irreversible-operation safeguard, so no special path was added.

What the owner can decide without paid infrastructure:
- Create the age key pair. It is free, and the off-host target is the existing GitHub artifact store.
- Set a shorter shadow period for an empty dataset, as runbook §5 allows ("or a period the owner sets").

## 11. API Gateway completion

- **WebSocket proxy.** Already fully implemented and tested:
  - Code: `services/api-gateway/src/ws.rs`.
  - Behaviour: upstream handshake first, refusals relayed, subprotocol preserved, bounded frames, close and ping relay.
  - Tests: `tests/websocket.rs`.
  - Routes: three (conference realtime, calling signaling, messaging realtime), with credentials passed through to the owning service.

  It stays off (`API_GATEWAY_WEBSOCKETS_ENABLED=false`). The live clients use the direct routes, and redirecting them needs compatibility evidence, as the owner decided. Status: `IMPLEMENTED_PENDING_CONFIGURATION`.
- **Meeting developer API.**
  - 21 machine-callable operations, `meetings.read` / `meetings.write`.
  - Environment-scoped: a meeting outside the caller's environment is a 404.
  - Join, media, chat and artefacts stay human-only.
  - The gateway manifest already carries them.

  Missing is the production wiring:
  - a `meeting-request.forward` grant;
  - `CF_MEETING_SERVICE_BASE_URL` on the gateway;
  - `MEETING_PLATFORM_ACCESS_ENABLED`;
  - an edge route for the machine operations that does not move the first-party `/api/v2` traffic.

  This is an edge-topology decision. Status: `PENDING_OWNER_APPROVAL`.

## 12. Recording and notification improvements

- **Meeting recording v2** (`FEATURE_RECORDING_V2_ENABLED=false`). Lifecycle, consent (explicit under restrictive, healthcare and high-security profiles; notice-only in the base profile), single-object SigV4 signed playback URLs (≤ 900 s), deletion and retention all exist. Retention days come from the meeting profile: 30 base, 7 restrictive and healthcare, 14 for high-security recordings. Activation needs a Meeting-specific artefact bucket and credentials (`ARTIFACT_STORAGE_*`), isolated from the Calling bucket, plus the owner's retention decision. Status: `PENDING_OWNER_APPROVAL` (retention), `IMPLEMENTED_PENDING_CONFIGURATION` (storage).
- **Notifications.** All meeting and organization notification emails now go through the ledger, with categories and idempotency. Recipient preferences (Communications Hub `meetingInvitations`) still apply.

## 13. Security and privacy evidence

- Secrets:
  - the Resend key and webhook secret are installed only from GitHub environment secrets through SSH stdin into 0400 files, mounted read-only and never printed;
  - `diagnose` prints presence, domains and counts only.
- Logs and data:
  - logs carry the delivery id and category only, never an address, code or body;
  - message content is erased at the final state;
  - the ledger key is a digest of the code, never the code itself.
- Escaping: templates escape every interpolated value, which fixes HTML injection through usernames in the former verification, reset and welcome templates. Subjects and names are stripped of control characters.
- Webhooks: signature verified in constant time (HMAC `verify_slice`), a ±5 minute window, a 256 KiB body cap and replay-safe dedupe.
- Internal operations are hidden (404) without `X-Internal-Token` and are not routed publicly.
- Production access:
  - manual approval only, enforced twice (Developer Platform configuration and release tooling);
  - named reviewers;
  - self-approval refused;
  - `developer-platform.administer` granted only to `cfmeet.operator-console`, as the permission registry requires.
- Telecom exclusions are unchanged (safe defaults enforced by release, verify and certify).

## 14. API and Swagger changes

| Change | Contract impact |
|---|---|
| `POST /api/v1/email/provider-events/resend` (new) | Provider callback, signature-authenticated. Not a client API and not in the client Swagger. |
| `POST /api/v2/meeting-spaces/{id}/archive` | Behaviour: ends LIVE conferences. The response schema is unchanged. |
| `POST /api/v2/instant-meetings` | Behaviour: an empty conference auto-ends after 10 minutes (`terminationReason: EMPTY_TIMEOUT`, an existing enum value). |
| Developer Platform operator decisions | A new refusal for operators who are not named reviewers: `422 POLICY_VIOLATION` (an existing code). |

The OpenAPI documents (`packages/contracts`) were deliberately **not** regenerated in this release:
- Eight service images copy that directory.
- Changing it would replace Calling, Messaging, Organizations, Numbering, SMS Gateway, API Gateway and the Developer Platform for a documentation-only difference.
- The Calling swap waits for zero active calls, which would put the Meeting fix behind live calls.

The behaviour is documented in [the frontend handover §12](../frontend/cfmeet-frontend-handover.md#12-final-production-completion-changes-2026-11-12). The Swagger description update (archive and instant-meeting notes) is listed as follow-up F-1 in §18.

## 15. Frontend handover changes

[cfmeet-frontend-handover.md §12](../frontend/cfmeet-frontend-handover.md) covers:
- Resend-backed email states and OTP UX (delivery is asynchronous, use the latest code, resend timing);
- the instant-conference lifecycle and `EMPTY_TIMEOUT`;
- `conference.ending` / `conference.ended` handling without a refresh;
- archived-space behaviour;
- the gradual verification policy and the `EMAIL_VERIFICATION_REQUIRED` handling;
- production-access request states;
- capabilities that are still gated.

## 16. Staging and production deployment evidence

| Step | Result |
|---|---|
| PR [#65](https://github.com/CosmicforgeHealthnet/cfmeet/pull/65) → `staging` | merged `21976ecd90dfe2a2927b19d495b248014b2beaa9` (final head `9359d070`, all checks green) |
| PR [#66](https://github.com/CosmicforgeHealthnet/cfmeet/pull/66) `staging` → `main` | 192 checks green, 1 skipped by design; merged `1e9364ce6590f6e8f4b2ba68da5bc2b13d03046f` |
| `main` head released | `f2f9693489bd1289935b695d5d83e6db4d22eb2f`. It is `1e9364ce` plus two docs/acceptance-workflow commits from the parallel stream; no image build inputs changed. |
| Release | [43309857124](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43309857124): gate (green main CI for the exact SHA) → images → backups of 16 databases (restore-tested: meeting-service, organizations, platform-access) → migrations → swap → verify |
| Release scope | base `9684346e` → changed `meeting-service`, `developer-platform`. Containers replaced: `meeting-service`, `developer-platform`, `developer-platform-worker`. **Kept (not restarted)**: platform-access, organizations, messaging, calling, communications, numbering, privacy-relay, sms-gateway, telephony, api-gateway. Each kept container was checked to run exactly the `9684346e` image. |
| Images | `ghcr.io/cosmicforgehealthnet/cfmeet-meeting-service:f2f96934…@sha256:fb153c003624add0e6c23ac4f50a87ee9ff55bc984af87d0e4323f4a764b0531` · `ghcr.io/cosmicforgehealthnet/cfmeet-developer-platform:f2f96934…@sha256:7ad9f984c8af34bdbdd9524ed6a08a6aecf26a85606d969c286a72a2f19b8e60` |
| Migrations | All Meeting histories applied in order. New: `lifecycle_delivery_migrations` 1 applied (`m20261112_000001_email_delivery_and_presence`). Developer Platform: 10 applied (`m20261203_000014_credential_lifecycle_notices`). |
| Verification | 30/30 checks PASS, 0 violations. `REGRESSION: Meetings health, Messaging/Calling/Platform TLS readiness, both LiveKit keys PASS` |
| Rollback readiness | The journal records the previous container of each replaced service. `rollback` restores them, with backups `/opt/cfmeet-core/backups/43309857124-1/`. |
| Post-release diagnose | [43317264046](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317264046) |
| Post-release meetings acceptance | [43317535902](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43317535902) success |
| Resend | Not activated: `activate-resend-email` was not run, and the SMTP configuration is preserved. |

## 17. Test results

CI failures on PR #65 and their root causes:

| # | Failure | Root cause | Fix |
|---|---|---|---|
| A | Secret scan | The test fixture used a realistic Svix-format secret (base64 of a dummy string); nothing real was exposed | Synthetic `whsec_fixture-not-a-secret-…` fixture; scanner unchanged |
| B | Developer Platform image check | The image's exact `/app` binary allowlist did not list the new `developer-platform-operator` (the private-key check itself passed) | Allowlist updated |
| C/D | Real-service journeys and SDK parity | The Developer Platform refused to start with the workflow on and no named reviewer | Downgrade to "submissions disabled" with a warning instead of a startup error; the certification stack names an explicit test reviewer |
| E | Chromium/Playwright | `ubuntu-latest` moved to 26.04, where the pinned Playwright browser dependencies do not install | Browser jobs pinned to `ubuntu-24.04` |
| — | Meeting Service compile | Rust 1.100 (never-type fallback) breaks `redis` 0.25.4 (`!: FromRedisValue`) | `redis` 0.25.5 |
| — | Backup drill | `pg_restore` 17 against PostgreSQL 16 emits `SET transaction_timeout` | `restore.sh` renders to SQL and drops only that setting when the client is newer |
| — | `diagnose` before the migration | §2 | PR #67 |

Final results:
- PR #65 head `9359d070`: all checks green, including journeys run 43283416967.
- PR #66 head `21976ecd`: 192 checks green.
- Main push CI for `1e9364ce`/`f2f96934`: green (gate passed in release 43309857124).
- PR #67 head `236e5ae7`: 104 checks green.
- Release tooling: 274 offline tests OK.

## 18. Remaining external dependencies

- **Resend.** Domain `cfmeet.com` verified in Resend, with Hostinger DNS records (`send` MX/TXT, `resend._domainkey`, `_dmarc`). The production secrets `CFMEET_RESEND_API_KEY` and optionally `CFMEET_RESEND_WEBHOOK_SECRET`, and the variable `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT`.
- **Off-host backup age key** for the Organizations import gate (§10).
- **Meeting artefact bucket** for recording v2 (§12).
- **F-1:** regenerate the Swagger descriptions for the archive and instant-meeting behaviour notes in a contracts release.

## 19. Owner-only actions

1. Resend: follow [resend-email-owner-handoff.md](resend-email-owner-handoff.md) §1–6, then run `activate-resend-email`, then `email-acceptance` with `confirm=otp`.
2. Name production-access reviewers in `vars.CFMEET_PRODUCTION_ACCESS_REVIEWERS`, then run `activate-production-access-review`.
3. Revoke the Gmail app password after Resend has been stable for a few days.
4. Organizations: create the age key and decide the shadow period (§10).
5. Recording v2 retention and storage decision (§12).

## 20. Final production verdict

_Completed after the release._
