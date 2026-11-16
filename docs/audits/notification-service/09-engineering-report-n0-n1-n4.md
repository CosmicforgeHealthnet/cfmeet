# Document 9 — Engineering Report: N0, N1 and N4

**Status:** `CFMEET_NOTIFICATION_N0_N1_N4_PRODUCTION_CLOSED` — caveat `EMAIL_PROVIDER_E1_PENDING_OWNER_ACTIVATION` (N0-2 / E1 is the owner's external email-provider activation; see §7.7)

*Sections 1–5 record the engineering as delivered on the branch (2026-11-15). Section 7 records the merge, release, activation, acceptance and certification evidence (2026-11-16) and supersedes every earlier "pending" statement.*

| | |
|---|---|
| Authorization | Owner approval of the full-consolidation architecture (D-13), with D-5, D-10, D-15 and D-16 approved, D-W excluded and D-12 decided (security-safe registration responses) |
| Branch | `ccr-a48695ce-yy5erg`, synchronized with `main` @ `7f70ec7` before any change |
| Production at start | release 43706099426 on `7f70ec7`, success; no workflow run in progress; no open pull request |
| Production changes made (during engineering) | none on the branch. **Since then:** merged as PR #71 and released on 2026-11-16 (§7). Still no provider activated and no notification route switched |
| Scope rule followed | each roadmap item is its own commit (N0-3 / N0-12 share one), so it can be reviewed, released and reverted on its own; no UI redesign; **no database migration in any service**, so every service can still be rolled back to its current image (see §4.1, §5) |
| Test evidence (full suites) | Meeting Service `cargo test --workspace`: **932 passed, 0 failed** (84 test binaries); Messaging suites listed in §5; release tooling 353 + controlled-OTP tests; contracts validator and all generator gates |

---

## 1. Summary

| Section | Engineering result | Production state |
|---|---|---|
| **N4** push transport port | done: APNs / FCM / Web Push adapters moved verbatim into `packages/push-transport`; Messaging re-exports them; **zero test edits** | **released** (Messaging on `163be40`); Web Push activation intact; no provider added |
| **N1** contracts, permissions, `notification-client` | done: every interface of the canonical register frozen as `experimental` contracts; identity `cfmeet.notifications` and 12 permissions; producer client crate with HPKE sealing and the routing-fence helper; FE1 types | **merged**, checks green; nothing served; no grant issued (by design until N2+) |
| **N0** repair existing paths | 11 engineering items done (N0-1, N0-3 … N0-12). N0-2 is the owner's E-track (E1) | **11 items released and verified in production**; N0-1 and N0-6 activated. N0-2 / E1 `PENDING_OWNER_EMAIL_PROVIDER_ACTIVATION` |

M1 (ENGINEERING_COMPLETE) is **not** claimed: it requires N1–N13. N0's row-14 "Engineering" definition (*released and verified in production*) is now met for the 11 engineering items (§7). N0-2 is not reported as completed and email delivery is **not** operationally accepted.

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

## 6. What remained before N0 was "repaired" (historical, 2026-11-15; resolved in §7)

| Step | Owner | Notes |
|---|---|---|
| Review and merge this branch to `main` | owner | **done**: PR #71 (§7.1) |
| Coordinated `release` of Meeting, Messaging, web | owner | **done**: run 43784033241 (§7.2) |
| `activate-meeting-notice-preferences` (N0-1) | owner | **done**: run 43788212474. Q3 `SENT` still waits on E1 (§7.8) |
| `activate-public-internal-denial` (N0-6) | owner | **done**: run 43788404790 (Q15 completed) |
| E1 (N0-2) | owner | **still open**: `PENDING_OWNER_EMAIL_PROVIDER_ACTIVATION` (§7.7) |
| Q1–Q18 baselines | owner / operations | partly measured (§7.8); not blockers |
| Document 8 statuses S-1, S-3 … S-12 | — | **repaired: released and verified in production** (Document 8 §2) |

---

## 7. Production closure (2026-11-16)

**Verdict:** `CFMEET_NOTIFICATION_N0_N1_N4_PRODUCTION_CLOSED`
**Separate caveat:** `EMAIL_PROVIDER_E1_PENDING_OWNER_ACTIVATION`

Every run below was inspected on 2026-11-16 (job conclusions, uploaded release artifacts and the production journal reported by `certify`). No release was repeated: the only commits after the release SHA are tests and web UI (§7.2).

### 7.1 Merge

| | |
|---|---|
| Pull request | [#71](https://github.com/CosmicforgeHealthnet/cfmeet/pull/71) "Notification Service N0/N1/N4: existing-path repairs, contracts, shared push transport", merged 2026-11-15T15:51:13Z into `main` |
| Merge commit | `386d99325391e90dde75f932ca4b715b307cc670` |
| Push CI on the merge commit | 29 workflows success, including Contracts and service auth, Notification client package, Push transport package, Messaging, Meeting service, Mobile, Frontend, Dependency and image security, Communications Hub. *Service boundaries* and *Messaging realtime ingress* were **cancelled** there because a newer push superseded them. Both then passed on later `main` commits (ingress 43732441499 on `1db753e`; Service boundaries 43783995930 on `163be40` and 43788811854 on `e2ec012`) |
| Follow-ups merged before release | `a8e401e`, `1db753e` (N0 auth/OTP production acceptance on isolated fixtures); PR #72 (`163be40`, messaging journey); PR #73 (`e2ec012`, live web-logout Calling revocation check) |

### 7.2 Release

| | |
|---|---|
| Release run | [43784033241](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43784033241) `release` on `163be40b67d43219b4c7932a1437c5e110d4ab1e`, success (journal `43784033241-1 done`) |
| Images | built by 43784783573 (Build CF Meet core service images), revision label verified |
| Services replaced (13) | platform-access, organizations, messaging, calling, communications, numbering, privacy-relay, sms-gateway, telephony, meeting-service, developer-platform, developer-platform-worker, api-gateway (scan gateway re-verified, acceptance PASS) |
| Resource gate | MemAvailable 5,033 MiB; required 832; remaining after release 4,201 MiB; shortfall 0 |
| Pre-release backup | 16 databases, manifest `/opt/cfmeet-core/backups/43784033241-1/manifest.json`; restore-tested: organizations, platform-access, meeting-service (PASS) |
| Post-release verification | 30 checks PASS (13 running, 11 migration histories, internal-not-public, CORS, swagger, provider features off, Organizations event delivery); violations: none |
| Web | N0-9 / N0-11 web changes ship with the web app; the live web logout revocation is proven in §7.4 |
| Current `main` vs release SHA | `163be40..e2ec012` changes only `apps/web/tests/messaging-production.browser.mjs`. `386d993..e2ec012` changes no service, package or contract code (web UI + acceptance tooling only). **No further release is needed** |

### 7.3 Activations

| Operation | Run | Result |
|---|---|---|
| `activate-meeting-notice-preferences` (N0-1) | [43788212474](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43788212474) | success; Communications trust bundle changed, Hub recreated; regression (Meetings health, Messaging/Calling/Platform TLS readiness, both LiveKit keys) PASS. `certify` shows grant `activate-meeting-notice-preferences<-meeting-service:ok` |
| `activate-public-internal-denial` (N0-6) | [43788404790](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43788404790) | success; `/internal/metrics`, `/internal/rtc/metrics`, `/internal/operations/email/acceptance`, `/internal/operations/email/deliveries/{id}` all denied through the public host. Re-checked live 2026-11-16T02:38Z: `GET https://api.cfmeet.com/internal/metrics` → 404 with `x-cfmeet-ingress: denied`; `/health` 200 |

### 7.4 Production acceptance

| Run | Workflow / operation | Result |
|---|---|---|
| [43788587332](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43788587332) | `organization-acceptance` (on `163be40`) | success. **N0 auth acceptance** (`notificationAuthAcceptance`): duplicate registration same public response, fresh OTP + wrong-code denial, hashed OTP verifies and is single-use, new OTP stored as salted hash without address key, legacy OTP compatible and single-use, reset code single-use + new-password login, uniform reset denial for unknown address / wrong code, reset revokes old session and logout revokes new session, login and original account preserved: all PASS. Organization acceptance: 18/18 PASS |
| [43788769304](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43788769304) | Meetings production acceptance | `MEETING_ACCEPTANCE: PASS` (participant identity, reconnect, Q&A restore, realtime events, cleanup) |
| [43788941438](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43788941438) | Calling production / `messaging-web-verify` on `e2ec012` (real website, real sign-in, Chromium) | success; 13/13 PASS including `contact_chat_send_receive_encrypted`, `offline_recipient_receives_queued_messages`, `calling_endpoint_registered_by_web_app` and **`web_logout_revokes_registered_calling_endpoint`** (N0-11) |
| [43789219191](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43789219191) | `diagnose` (read-only) | success; used for the email evidence in §7.7 (not a substitute for `certify`) |

### 7.5 Final certification

| | |
|---|---|
| Run | [43792702821](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43792702821) `certify`, dispatched from `main` with `release_sha=e2ec012bd63ffbb195efd45d4e240bb600cfa035` |
| Verdict | **`CERTIFIED_READ_ONLY`**, failures: none |
| Topology | all 13 services on image revision `163be40b67d43219b4c7932a1437c5e110d4ab1e`, registry tag and container label match the journaled release, revision on `main` |
| Per service (13/13 PASS) | container running, no restarts, readiness, database connectivity with service credentials, every migration of the revision applied, anonymous request rejected (401), service-auth identity and grants without problems, service-to-service TLS reachability |
| Platform | core identity PASS; provider safe defaults PASS (no violations); Organizations event delivery PASS (76 delivered, 0 failures, oldest undelivered 0 s); public smoke from VPS PASS |
| Public API | health, CORS (`cfmeet.com`, `www.cfmeet.com`), 9 OpenAPI documents, swagger, TLS (35 days remaining), user API requires session: PASS. 25 internal/operator/callback routes probed on the public host: 0 answered 2xx |
| Resources | MemAvailable 5,039 MiB; disk free 139.2 GiB; no OOM kill, no restart |
| Integrations unchanged | Web Push (VAPID) **enabled** (activation 43706099426 intact); `MESSAGING_PUSH_PROVIDER=none` (no APNs/FCM); SMS, telephony, numbering, privacy relay disabled; Meeting `EMAIL_PROVIDER=smtp`. No notification route switched: no Notification Service exists and `notifications` remains in `planned_owners` |
| Not covered by `certify` | authenticated journeys (read-only by design); covered by the acceptance runs in §7.4 |

Required CI on `e2ec012`: Frontend 43788811873, Service boundaries 43788811854, Unified Messaging integration journeys 43788811828, all success. The path-filtered workflows were last triggered on `386d993` / `1db753e` / `305bb0f`, all success, and no later commit touches their paths.

### 7.6 Section status

| Section | Status | Evidence |
|---|---|---|
| **N0** | **closed** for N0-1, N0-3 … N0-12 and push-job retention: released on `163be40`, N0-1 and N0-6 activated, auth / OTP / reset / logout behaviour accepted in production (§7.4), certified (§7.5). **N0-2 not completed** (E1, §7.7) | §7.2–7.5 |
| **N1** | **closed**: contracts (`experimental`), service-auth identity and 12 permissions, `notification-client` (HPKE, routing fence), `apps/web/src/types/generated/notifications.ts` and `apps/mobile/lib/api/generated/notifications.ts` merged; Contracts 43776158134 and Notification client 43732183046 success | §3, §7.1 |
| **N4** | **closed**: `packages/push-transport` merged; Messaging imports `cf_push_transport` and re-exports at the old paths; Push transport 43732183009 and Messaging 43732183024 success; Messaging released and certified; Web Push still enabled, no provider added | §2, §7.5 |

Per-item production status:

| Item | Production status |
|---|---|
| N0-1 meeting-notice permission grant | activated (43788212474); grant certified |
| N0-2 email transport | **`PENDING_OWNER_EMAIL_PROVIDER_ACTIVATION`** |
| N0-3 poison recipient | released; regression test evidence in §4. Live outcome not observable until E1 (all sends fail SMTP auth) |
| N0-4 email + push lease renewal | released (Meeting, Messaging) |
| N0-5 `notify:false` authorization | released (Messaging) |
| N0-6 internal-route protection | activated (43788404790); re-checked live |
| N0-7 push registration and job erasure | released (Messaging) |
| N0-8 salted OTP hashes | released; accepted in production (43788587332) |
| N0-9 registration / reset-token security | released; accepted in production (43788587332) |
| N0-10 transactional security notices | released; notices are enqueued (ledger shows `SECURITY_PASSWORD_CHANGED:QUEUED`); delivery waits on E1 |
| N0-11 Calling endpoint revocation on logout | released; accepted on the live website (43788941438) |
| N0-12 bounce / complaint suppression | released; live effect needs the Resend webhook (E1) |
| Push-job retention (FN-32) | released (Messaging) |

### 7.7 Email provider (E1): outstanding, owner-only

Status: **`PENDING_OWNER_EMAIL_PROVIDER_ACTIVATION`**. This mission deliberately did **not** configure Resend, change SMTP credentials, modify DNS, create sender addresses or run `activate-resend-email`.

- Owner-confirmed: `cfmeet.com` registered and DNS configured; Resend credentials not yet installed on the VPS; intended sender `no-reply@cfmeet.com`; organization invitation emails to be designed separately.
- Live state (`diagnose` 43789219191): `emailProvider: smtp (implicit)`, `smtp.gmail.com:587`, Resend key not mounted, no Resend webhook; 71 SMTP `535` authentication rejections in the 7-day log window; ledger (24 h): FAILED 50, QUEUED 11, last error class `SMTP_AUTHENTICATION` ×42.
- Consequence: OTP, reset, security and meeting-notice email are **not delivered** today. N0-2 is not completed and email delivery is **not** operationally accepted. G-18 … G-23 stay BLOCKED until E1.
- To close E1 (owner): install the Resend credentials, then `activate-resend-email`, then `email-acceptance confirm=otp` with delivery evidence. This does not block N2.

### 7.8 Baseline measurements Q1–Q18

| ID | Status | Evidence / note |
|---|---|---|
| Q1 | OUTSTANDING (pre-E1 24 h snapshot only) | diagnose: FAILED 50 / QUEUED 11 by category. Formal baseline after E1 |
| Q2 | OUTSTANDING (S-2 evidence obtained) | diagnose: `SMTP_AUTHENTICATION` ×42 (24 h). Formal baseline after E1 |
| Q3, Q4 | OUTSTANDING | meeting-notice outcomes not queried; `SENT` cannot be shown before E1 |
| Q5 | OUTSTANDING | — |
| Q6, Q7 | OUTSTANDING | — |
| Q8, Q9 | OUTSTANDING | — |
| Q10 | OUTSTANDING | — |
| Q11 | **COMPLETED** | certify 43792702821: `organization_outbox` 76 DELIVERED to `cfmeet.meeting-service`, 125 `cfmeet.reconciliation` PENDING (history records, never dispatched by design), 0 failures |
| Q12, Q13 | OUTSTANDING | — |
| Q14 | **COMPLETED** | grant present in the live Hub bundle after N0-1 (43788212474; certify grant list) |
| Q15 | **COMPLETED** | 43788404790 + live check 2026-11-16T02:38Z (marked 404) |
| Q16 | OUTSTANDING (single samples) | 5,033 MiB (release), 5,039 MiB (certify); the 7-day minimum has not been taken |
| Q17, Q18 | OUTSTANDING | after E1 |

### 7.9 Compatibility and rollback limitations

- **No database migration** was added by N0/N1/N4, so every service can still return to its previous image with `rollback` (previous containers recorded by release 43784033241).
- **N0-8:** after a rollback, codes issued by the new image cannot be read and users must request a new code; attempt and send-rate counters restarted once at release.
- **D-12 residual:** `diagnose` shows `EMAIL_VERIFICATION_ENFORCEMENT` unset, so a follow-up login can still distinguish a new account from an existing one (see §4.2).
- **N0-11:** endpoints registered before the release are revoked at logout only after the page has registered once more. "Log out everywhere" revokes only this browser's endpoint. The mobile `voip` removal takes effect with the next app build.
- **N0-5:** external customer backends can no longer suppress pushes with `notify:false`.
- **N0-12 / N0-3:** live effect is only observable once a provider accepts mail (E1).
- **Retention:** after 30 days the push-job gauges reflect only recent jobs.

### 7.10 Next

**N2 (service skeleton) is ready to begin** under D-13. It has not been started. When it creates `services/notifications`, it must remove `notifications` from `planned_owners` in `packages/contracts/manifest.json`. E1 stays an independent owner track.
