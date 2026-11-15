# Document 9 — Engineering Report: N0, N1 and N4

**Status:** `N0_N1_N4_ENGINEERING_IMPLEMENTED — MERGE_RELEASE_AND_PRODUCTION_VERIFICATION_PENDING`

| | |
|---|---|
| Authorization | Owner approval of the full-consolidation architecture (D-13), with D-5, D-10, D-15 and D-16 approved, D-W excluded and D-12 decided (security-safe registration responses) |
| Branch | `ccr-a48695ce-yy5erg`, synchronized with `main` @ `7f70ec7` before any change |
| Production at start | release 43706099426 on `7f70ec7`, success; no workflow run in progress; no open pull request |
| Production changes made | **none.** Nothing was merged, released, activated or routed. No provider was activated and no notification route was switched |
| Scope rule followed | each roadmap item is its own commit (N0-3 / N0-12 share one), so it can be reviewed, released and reverted on its own; no UI redesign; **no database migration in any service**, so every service can still be rolled back to its current image (see §4.1, §5) |
| Test evidence (full suites) | Meeting Service `cargo test --workspace`: **932 passed, 0 failed** (84 test binaries); Messaging suites listed in §5; release tooling 353 + controlled-OTP tests; contracts validator and all generator gates |

---

## 1. Summary

| Section | Engineering result | Production state |
|---|---|---|
| **N4** push transport port | done: APNs / FCM / Web Push adapters moved verbatim into `packages/push-transport`; Messaging re-exports them; **zero test edits** | unchanged until the next Messaging release; behaviour identical |
| **N1** contracts, permissions, `notification-client` | done: every interface of the canonical register frozen as `experimental` contracts; identity `cfmeet.notifications` and 12 permissions; producer client crate with HPKE sealing and the routing-fence helper; FE1 types | nothing served; no grant issued |
| **N0** repair existing paths | 11 engineering items done (N0-1, N0-3 … N0-12). N0-2 is the owner's E-track (E1) | each item takes effect only after merge + release; N0-1 and N0-6 additionally need their dispatched operation |

M1 (ENGINEERING_COMPLETE) is **not** claimed: it requires N1–N13. Per Document 6, N0's row-14 "Engineering" definition also requires *released and verified in production*; that step is the owner's (§6).

---

## 2. N4 — Push transport port

| Item | Result |
|---|---|
| Commit | `bc8cf04` |
| New package | `packages/push-transport` (`cf-push-transport`): `adapters` (verbatim `push_adapters.rs`), `provider` (`PushOutcome`, `PushProvider`, `LocalTestPush`), `config` (adapter configuration structs) |
| Source change in the moved code | one import path (`super::push::…` → `crate::provider::…`) |
| Messaging | re-exports every item at its previous path; build context, Dockerfile, CI triggers updated |
| Security properties moved unchanged | metadata-only payload whitelist, Web Push SSRF host allowlist (registration and send, no redirects), ES256 / RS256 constant-time signing, RFC 8291 vector |
| Tests | package: 6/6 (incl. RFC 8291 Appendix A). Messaging, **unchanged tests**: architecture 7/7, foundation 7/7, realtime 24/24, push_adapters 4/4, authorization_matrix 4/4; fmt + clippy `-D warnings`; licence and crypto-graph guards |
| CI | new `push-transport.yml`; `messaging.yml` triggers on the package; `dependency-security.yml` audits its lockfile |
| Rollback | revert the commit (pure refactor) |

---

## 3. N1 — Contracts, permissions, `notification-client`

| Item | Result |
|---|---|
| Commit | `9fba6e4` |
| Contracts (all `experimental`, contract-first) | `cfmeet.notifications.common`, `.event` (envelope + 31 types + `notifications.cancel`), `.inbox-changed-event`, `.http-api` (U1–U9 + Resend callback), `.internal-api` (I1–I12; I12 under D-15), and owner APIs X1–X7: `cfmeet.meeting-service.notification-owner-api` (X1, X2, X6, X7), `cfmeet.messaging.…` (X3), `cfmeet.communications.…` (X4), `cfmeet.organizations.…` (X5, D-5) |
| Counts vs canonical register | 9 U + 1 callback; 11 + 1 I; 7 X; 31 inbound (30 + 1 optional) + 1 control + 1 outbound — **all equal** |
| Fixtures | 10 examples, 14 counter-examples (forbidden data keys in any case, unsealed code, sealed part on a non-sealed type, wrong producer / priority, wrong legacy key format, missing route, operator alert to a user, control character, …); each counter-example is pinned to its **intended** rejection reason |
| Validator | `planned_owners` in the manifest: a not-yet-existing service may own contracts only if listed with an approval reference, only while `experimental`, and the entry is rejected once `services/<owner>` exists (5 mutation tests) |
| Coverage gate | `FROZEN_UNIMPLEMENTED` (experimental only, each naming its roadmap step) instead of a baseline; `BASELINE` stays empty (3 new unit tests) |
| `packages/service-auth` | identity `cfmeet.notifications`; 12 permissions — received by the service: `notifications-event.publish`, `-user.forward`, `-call-push.dispatch`, `-observability.read`, `-operations.replay` (sensitive), `-operations.reroute` (sensitive); granted only to the service: `account-contacts.read` (sensitive), `meeting-notification-recipients.read` (sensitive), `messaging-wakeups.authorize`, `communications-restrictions.read`, `organization-invitations.read`, `account-route-lease.claim`. `cfmeet.platform.common` 1.18.0. Existing least-privilege pins updated explicitly |
| `packages/notification-client` | envelope builder and validator (same rules as the contract plus size and issuer), HPKE RFC 9180 (X25519 / HKDF-SHA256 / AES-256-GCM, AAD = `producer|eventId|eventType`; opening only behind the `open` feature, which only the service will enable), routing fence helper: §9.3 decision, canary, §9.4 guard rule, §9.8 CRITICAL lease reroute |
| FE1 | `scripts/contracts/notification-types.mjs` generates types-only files for web and mobile from the user-facing definitions (`--check` in `contracts.yml`); nothing imports them before N8; no UI change |
| Tests | service-auth 54 unit + all integration (incl. contract conformance); validator 25/25; notification-client 18 (with `open`) / 16 (producer build); release, coverage and generator checks pass |
| Defects found by the tests and fixed | (1) the event key `voicemailId` contains the forbidden part `email` (voic**email**Id), so event #16 could never validate; renamed `voiceMessageId`. (2) the operator-only recipient rule targeted the wrong level. (3) a counter-example was rejected for the wrong reason |
| Spec correction | Document 4 §5.1's example used `data.codeTtlMinutes`, which its own forbidden-key rule (`code`) refuses; the contract uses `ttlMinutes` |
| Rollback | revert (no runtime effect) |

---

## 4. N0 — Repair the existing paths

| Item | Defect | Commit | What changed | Regression evidence | Activation |
|---|---|---|---|---|---|
| N0-1 | S-1 | `ffff88d` | `communications-preferences.read` added to the Hub's incoming grants (`HUB_INCOMING`, rebuilt on every Hub release) and as `GRANTS` row + operation `activate-meeting-notice-preferences` (merged bundle, Hub only recreated, restored on failure) | release test: grant present in both, valid against `permissions.rs`; the test registry parser now also reads single-line entries (it skipped them before). 353 release-tooling tests pass | next Hub release, or dispatch the operation |
| N0-2 | S-2 | — | **E-track (owner):** E1 `activate-resend-email` / working SMTP, proven by `email-acceptance confirm=otp` | — | owner |
| N0-3 | S-11 | `4246235` | typed `MailError`; an address the ledger refuses (or a suppressed one) is recorded as not sent and the notice continues; only a queue outage retries | poison + suppressed recipient → the third recipient is emailed in the same pass, notice finished, nothing more later | release |
| N0-4 | S-8 | `05bfff0` (email), `869b9d0` (push) | email worker renews its lease on every held row before each send and skips rows re-claimed elsewhere | 2 workers, 0.8 s provider, 2 s lease, 5 messages → each sent once; **with renewal disabled the same test sees 3 duplicates** | release |
| N0-5 | S-4 | `3850f98` | Messaging: `notify:false` honoured only for delegated / import callers | §5 | release |
| N0-6 | S-3 | `1bbfefd` | public `api.cfmeet.com` refuses `/internal/operations/*`, `/internal/metrics`, `/internal/rtc/metrics` with a marked 404; operation `activate-public-internal-denial` proves each denial through the public host and restores on failure | verified with a real nginx: operator routes denied and marked; `/internal/rtc/livekit/webhook`, `/internal/transcription/events` and API traffic pass | dispatch the operation (or next Hub release) |
| N0-7 | S-7 | `df6493b` | Messaging: principal erasure deletes push registrations and jobs | §5 | release |
| N0-8 | S-5 | `ba0af77`, tooling in `94d759f` | codes stored as `salt:SHA-256(salt‖code)`, keys from SHA-256 of the address, constant-time compare; previous-image codes honoured once until expiry | real-Redis key inspection; unit tests; shared format vector Rust / Python / certification | release |
| N0-9 | S-6 + D-12 | `94d759f` | `verify-reset-token`: 200 only for the correct code, one 400 otherwise, shared attempt limit. `register`: same 201 for an existing address, nothing created, owner notified (≤ 1/h); 409 only for a taken username | updated tests (they encoded the defects); new assertions on answer shape, no second account, notice queued, 429 after 5 probes | release |
| N0-10 | S-9 | `c684d8c` | password-changed and device notices enqueued in the same transaction as the change; device key `device-linked:<link id>` | failure injection: notice refused → change rolled back, old password works; otherwise both commit | release |
| N0-11 | S-10 | `567e7f2`, `9696dd6` | web logout revokes this browser's Calling endpoint (existing `DELETE /v1/calling/endpoints/{id}`, bounded 3 s, never blocks logout); mobile drops `voip` from `UIBackgroundModes` | web 10/10, mobile 3/3 (2 fail when `voip` is put back) | web deploy / next app build |
| N0-12 | S-12 | `4246235` | enqueue refuses non-security mail to a recipient with a complaint or two bounces (`SUPPRESSED`); verification, reset, password-changed, device notices, step-up codes and acceptance mail are never suppressed | through the signed Resend webhook: complaint / second bounce → refused; security categories still queued | release |
| Retention | FN-32 | `79c3bbe` | Messaging: purge of finished push jobs | §5 | release |

### 4.1 Deliberate deviations from the blueprint wording (with reasons)

| Item | Blueprint wording | Implemented | Reason |
|---|---|---|---|
| N0-6 | `location ^~ /internal/ { return 404; }` | only the Meeting operator routes | The repository documents that LiveKit may post Meeting's signed webhook `/internal/rtc/livekit/webhook` through the public host, and Q15 (the live site file) is outstanding. A blanket rule could stop meeting room events. Signed provider callbacks stay reachable |
| N0-3 | recipient marked `INVALID_ADDRESS` | recorded with the existing outcome `SUPPRESSED` + metric `meeting_notification_invalid_recipients` | Every Meeting migration history fails closed on unknown versions; a CHECK change would stop the previous image from starting after a rollback. The distinct value comes with the service ledger (N5) |
| N0-12 | suppression list | derived from the ledger's own `BOUNCED` / `COMPLAINED` rows (existing index) | no schema change (same rollback reason); suppression lasts as long as ledger rows are retained; the service's `notif_email_suppressions` (N5) replaces it |

### 4.2 Consequences the owner should know before releasing

| Topic | Effect |
|---|---|
| N0-8 rollback | after a rollback to the previous image, codes issued by the new image cannot be read; the user requests a new code. Attempt / send-rate counters restart once at release |
| N0-8 tooling | `email-acceptance` (also the post-check of `activate-resend-email`), app acceptance and the certification harness used to **read** plaintext codes from Redis; they now replace an issued code with a controlled one in the new format and still read the old format, so E1 keeps working before and after the release |
| D-12 residual | the uniform registration answer hides account existence only while email-verification enforcement covers login (`EMAIL_VERIFICATION_ENFORCEMENT`); with it off, a follow-up login still distinguishes a new account |
| N0-9 behaviour | the web reset flow now shows "Invalid or expired reset code" at the code step for a wrong code (previously only at the final step) |
| N0-11 | endpoints registered before the release are revoked at logout only after the page has re-registered once; "log out everywhere" revokes only this browser's endpoint |

---

## 5. Messaging items (N0-4 push part, N0-5, N0-7, push-job retention)

| Item | Commit | What changed | Regression evidence |
|---|---|---|---|
| N0-5 (S-4) | `3850f98` | `notify:false` is honoured only for delegated callers: requests on `/internal/v1/delegated/**` (peer service holding `messaging-application.delegate`, first-party scope confirmed by Platform Access) and the legacy Meeting import. Every `/v1` caller is ignored: end-user client tokens, gateway-forwarded credentials **and external customers' own backend credentials**. The field stays accepted (contract unchanged); ignored attempts counted in `messaging_notify_suppression_ignored_total` | `realtime::notify_false_is_honoured_only_for_delegated_callers`: client token and customer credential with `notify:false` → 1 job each; delegated → 0; fails with the fix disabled |
| N0-4 (S-8), push part | `869b9d0` | the dispatcher renews each job's lease (fenced by `lease_token`) before authorization and again before the provider call, and skips a job re-claimed elsewhere; authorization and provider calls capped at 20 s (a third of the 60 s lease); `lease_lost` count + `messaging_push_lease_lost_total` | `realtime::a_slow_provider_never_causes_duplicate_pushes_across_dispatchers`: 3 jobs → 3 sends; 4 sends with the fence disabled |
| N0-7 (S-7) | `df6493b` | principal erasure deletes the principal's push jobs and registrations (environment-scoped, one transaction, registrations locked first); device rows kept as before | `compliance::principal_erasure_removes_push_registrations_and_jobs`: 2 / 4 → 0 / 0, other principal untouched; fails without the fix |
| Retention (FN-32) | `79c3bbe` | hourly bounded purge (≤ 5,000) of final-state jobs older than 30 days; pending jobs and registrations never touched | `compliance::push_job_retention_purges_only_old_finished_jobs` |

Tests on this branch (all four items applied): realtime 26, compliance 12, delegated_mirror 2, integrations 12, architecture 7, migrations 3, push_adapters 4, messaging 15, notification_authorization 5, query_plans 1, **authorization_matrix 4/4 in one run** (an earlier combined run in the delegated worktree showed one unexplained failure that passed alone; treat as a possible flake); fmt and clippy `-D warnings` clean; migration-order guard unchanged.

**Deviation:** the retention change was first written with an additive migration (partial index on `completed_at`). It was removed: Messaging's readiness fails closed on applied migrations it does not know, so the previous image would no longer become ready after an image rollback. The purge runs hourly without an index; the table is small until push moves to the service (N9).

**Behaviour changes to note:**
- N0-5: an external customer's backend can no longer suppress pushes with `notify:false` (strict reading of "never for client tokens"). If a customer relied on it for migration writes, the owner may want delegated-style treatment for that case later.
- N0-4: a push call hanging beyond 20 s is retried instead of waited on (adapters time out at 10 s anyway).
- Retention: after 30 days, the `messaging_push_jobs{status}` gauge, `push_statuses` in `messaging_release.py` and the owner status view's dead count reflect only recent jobs.

---

## 6. What remains before N0 is "repaired" (Document 6 row 14)

| Step | Owner | Notes |
|---|---|---|
| Review and merge this branch to `main` | owner | no pull request was opened (not requested) |
| Coordinated `release` of Meeting, Messaging, web | owner | images are rebuilt from `main`; the previous image remains the rollback target (`rollback`) |
| `activate-meeting-notice-preferences` (N0-1) | owner | then Q3: notices for account holders reach `SENT` |
| `activate-public-internal-denial` (N0-6) | owner | Q15 evidence is produced by the operation itself |
| E1 (N0-2) | owner | `activate-resend-email` + `email-acceptance confirm=otp` with delivery evidence |
| Q1–Q18 baselines | owner / operations | still OUTSTANDING; not blockers for this engineering |
| Document 8 statuses S-1, S-3 … S-12 | — | set to **fixed in code, pending release** (Document 8 §2); "repaired" after production verification |

Next authorized engineering after this report: **N2** (service skeleton) can start; it must remove `notifications` from `planned_owners` when it creates `services/notifications`.
