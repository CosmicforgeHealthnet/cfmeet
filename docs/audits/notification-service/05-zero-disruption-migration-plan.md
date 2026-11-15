# Document 5 — Zero-Disruption Migration Plan (revision 2: full consolidation)

**Goal:** every notification pipeline moves into `cfmeet-notification-service` with no interruption of any notification that works today.

**Rules that hold throughout:**
- **Every message is routed to exactly one system at creation** (the routing fence, Document 4 §9).
- Old pipelines remain installed and switchable back until Phase 8.
- Every switch is one journaled coordinated-release operation on `main`, gated like every existing activation:
  - the exact SHA must be running;
  - health must pass;
  - changed containers are restored automatically on failure.

---

## 1. Functions that must never be interrupted

| # | Function | How the plan protects it |
|---|---|---|
| C1 | Sign-in, registration, code verification, password reset | code authority stays in Account. Code **emails** move last (4.6), through a transactional outbox, with break-glass fallback |
| C2 | Ringing, arbitration, recovery | stay in Calling; call push is post-commit and best-effort |
| C3 | Message send, ciphertext, realtime, receipts | stay in Messaging; only push moves |
| C4 | Device linking, step-up, revocation | Account authority unchanged; step-up email follows C1's rules |
| C5 | Organization access changes | the existing Meeting subscriber is untouched; the service is an additional subscriber |
| C6 | Customer webhooks | untouched unless D-W |
| C7 | Meeting notice emails | old worker keeps running until its fence; old jobs drain |

---

## 2. Phase 0: baseline (read-only)

### 2.1 Already measured (no further action)

| Fact | Value | Evidence |
|---|---|---|
| Push provider | `none` | certify 43670057279 |
| Email provider | `smtp` (implicit); Resend not activated | certify 43670057279; journal has no `activate-resend-email` |
| `NOTIFICATIONS_EMAIL_ENABLED` | `true` | certify 43670057279 |
| Organizations event delivery | DELIVERED 35, PENDING 59 (reconciliation rows), DEAD 0 | certify 43670057279 + readiness `outbox.pending 0` |
| Messaging outbox | pending 0, dead letter 0 | Messaging `/ready` in certify 43670057279 |
| Numbering / SMS outbox | pending 0, dead 0; SMS provider `none` | certify 43670057279 |
| Host | MemAvailable 5,076 MiB, disk free 141.1 GiB | certify 43670057279 |
| Running revisions | platform-access, organizations, numbering, privacy-relay, sms-gateway, telephony, developer-platform(+worker) at `b9dc15be`; messaging, calling, meeting (`cfmeet-production-backend`), api-gateway at `c6de964c`; communications at `8a64b480` | certify 43670057279 image labels |
| Release in progress at audit time | run 43696469230 (`6f3743f`, started 2026-11-15 08:51 UTC) | GitHub Actions |

### 2.2 Still to collect: authorized read-only queries

These must be run through a **new read-only `diagnose` extension**, or by the owner with the runtime role in a read-only session (`SET TRANSACTION READ ONLY`). They return counts and ages only; no addresses, content or handles.

| Q | Database | Query (counts only) | Answers |
|---|---|---|---|
| Q1 | meeting (`rtc_db`) | `SELECT category, status, count(*), min(created_at), max(created_at) FROM email_deliveries GROUP BY 1,2;` | email volume, failure mix, stuck QUEUED (OTP bodies retained) |
| Q2 | meeting | `SELECT last_error_class, count(*) FROM email_deliveries WHERE status IN ('QUEUED','FAILED','EXPIRED') GROUP BY 1;` | SMTP rejection evidence (S-2) |
| Q3 | meeting | `SELECT kind, status, last_error, count(*) FROM meeting_notifications GROUP BY 1,2,3;` | **confirms or refutes S-1**: look for `PREFERENCES_UNAVAILABLE` / FAILED |
| Q4 | meeting | `SELECT outcome, count(*) FROM meeting_notification_recipients GROUP BY 1;` | suppressed vs queued |
| Q5 | meeting | `SELECT count(*) FILTER (WHERE status='PENDING'), max(attempts) FROM account_communication_outbox;` | infinite-retry backlog |
| Q6 | messaging | `SELECT platform, enabled, count(*) FROM messaging_push_registrations GROUP BY 1,2;` | sizes the push move (expected 0: no data to copy) |
| Q7 | messaging | `SELECT status, count(*), max(created_at) FROM messaging_push_jobs GROUP BY 1;` | job growth (no retention) |
| Q8 | comm hub | `SELECT count(*), count(*) FILTER (WHERE NOT (direct_messages AND group_messages AND incoming_calls AND missed_calls AND contact_requests AND group_invitations AND meeting_invitations)) FROM communication_notification_preferences;` | how many users changed defaults |
| Q9 | comm hub | `SELECT notifications, count(*) FROM communication_conversation_preferences GROUP BY 1;` and the same on `communication_contact_aliases` | mute usage |
| Q10 | comm hub | `SELECT destination, state, count(*) FROM communication_outbox GROUP BY 1,2;` | Hub delivery health |
| Q11 | organizations | `SELECT destination, state, count(*) FROM organization_outbox GROUP BY 1,2;` | baseline |
| Q12 | calling | `SELECT status, count(*) FROM calling_outbox_events GROUP BY 1;` and `SELECT count(*) FROM calling_incidents WHERE status='OPEN';` | publication health |
| Q13 | developer platform | `SELECT status, count(*) FROM devplat_webhook_deliveries GROUP BY 1;` | webhook health |
| Q14 | trust bundle (file) | Is `communications-preferences.read` granted to `cfmeet.meeting-service` in the live Hub bundle? Read permissions only, never keys | **S-1 root cause** |
| Q15 | nginx (file) | Does any `location` block `^/internal/operations/`? `curl -s -o /dev/null -w '%{http_code}' https://api.cfmeet.com/internal/operations/email/deliveries/00000000-0000-0000-0000-000000000000` **without** a token, run by the owner (expected 404 from nginx; 404 from the app is also "no token"; distinguish by the `server`/body signature) | **S-3** |
| Q16 | host | `docker stats --no-stream` for the 13 containers; 7-day MemAvailable minimum | Phase 6 capacity gate |


Three of these queries carry extra weight in revision 2:

| Q | Why it matters more now |
|---|---|
| Q1/Q2 | size the drain window of the old email ledger and set the CRITICAL-lane baseline |
| Q6/Q7 | expected 0, so the push move needs no data copy |
| Q8/Q9 | **the row counts to copy** in Phase 4.2 (preferences) |

Two queries are added:

| Q | Query | Answers |
|---|---|---|
| Q17 | `SELECT category, percentile_disc(0.95) WITHIN GROUP (ORDER BY accepted_at - created_at) FROM email_deliveries WHERE accepted_at IS NOT NULL AND created_at > now() - interval '14 days' GROUP BY 1;` | p95 enqueue-to-provider latency per category (CRITICAL baseline) |
| Q18 | `SELECT count(*) FROM email_deliveries WHERE status IN ('QUEUED','SENDING');` | drain size at each fence |

**Exit criteria:**
- Q1–Q18 are recorded.
- S-1, S-2 and S-3 are confirmed or refuted.
- N0 is complete for the channels about to move.

---

## 3. Phase 1: foundations (dark)

The full schema (16 tables), I1–I9, U1–U9 (relay route off), workers and metrics. **2 replicas.** All routing is OFF; producers have no grants.

- **Acceptance:** certify shows both replicas healthy and zero traffic.
- **Rollback:** remove the containers.

---

## 4. Phase 2: shadow (all producers, no sends)

Each producer gets an **additional** publication to the service. Existing paths are untouched. The service computes recipients, preferences and channels and renders templates, then writes `SHADOW` rows. **It never calls a provider and never writes an inbox item.**

| Producer | Shadow publication | Compared against |
|---|---|---|
| Account | `account_notification_outbox` rows written alongside the current direct enqueue, with `shadow=true` (sealed code included; the service decrypts only to verify rendering, then discards) | old ledger row with the same idempotency key |
| Meeting | `outbox_events` channel `NOTIFICATION` for `conference.*` and invitation email requests | old `meeting_notifications` + ledger rows |
| Messaging | `message.created`, `member.invited` | old `messaging_push_jobs` (would-be wake-ups; today `NOT_CONFIGURED`) |
| Calling | missed, voicemail, recording facts | none (new) |
| Hub | `connection.*`; preference change feed (`NOTIFICATION_PREFERENCE`) mirrored to the service | Hub preference rows (copy verification) |
| Organizations | subscriber `cfmeet.notifications` | none (new) |
| Developer Platform | lifecycle and decision publications | ops-script email (key `devplat-access:<req>:<decision>`) |

**Shadow exit, per category:**
- ≥ 14 days or ≥ 1,000 events;
- capture ≥ 99.9 %;
- recipient, policy and rendering mismatch ≤ 0.1 %, all explained;
- for email categories, the rendered subject and body hash match the old template output;
- producer DEAD rows for the service destination = 0.

**Rollback:** set each producer's publication flag off.

---

## 5. Phase 3: in-app first

The in-app inbox goes live for categories with **no current channel**:
- contacts;
- org changes and the invitation copy;
- missed calls, voicemail, recordings;
- message requests;
- developer notices.

Meeting categories get an in-app copy while their email stays on the old path. There is nothing to duplicate (Document 2 §A.12: no inbox exists today).

**Rollback:** relay route off, IN_APP routing OFF.

---

## 6. Phase 4: channel moves, in order of increasing risk

### 4.1 Push (lowest risk: no production traffic)

**Pre-conditions:**
- Q6 = 0, or the D-1b copy path is ready.
- The owner activates providers **in the service**: VAPID / FCM / APNs (D-3). Do not activate them in Messaging, which would create data to migrate.

**Switch** (`activate-notifications-push`):
1. Fence push routing.
2. Messaging stops creating `messaging_push_jobs` for events after the fence; earlier jobs finish or expire within 24 h.
3. The Hub and Messaging push-registration endpoints become facades to U8/U9.
4. Calling: `CALLING_PUSH_TRANSPORT=notifications`.
5. Delivery status is answered by the service.

**If Q6 > 0:** copy registrations into `notif_push_endpoints`, sealing handles and mapping Messaging device → account device through the Hub index. Dual-read until the 24 h job TTL passes.

**Duplicate control:** the fence (one creator per job) plus collapse IDs.

### 4.2 Preferences (single-writer switch)

1. **Copy:** Hub `communication_notification_preferences`, the `notifications`/`muted_until` columns of `communication_conversation_preferences`, and `communication_contact_aliases.notifications`. Each row records the Hub `version` in `source_version`.
2. **Catch-up:** apply Hub change-feed `NOTIFICATION_PREFERENCE` / `CONVERSATION_PREFERENCE` / `CONTACT` entries after the copy. The change feed already exists (`sync.rs:16-29`).
3. **Switch** (`activate-notifications-preferences`): the Hub endpoints become facades, so preference writes now go to the service. There is a **write freeze of under 60 s**: Hub PUTs answer 503 + `Retry-After: 5`. Web clients already retry `communicationsRequest` (`communications.service.ts:62-70`), and preference writes are rare (Q8).
4. **Verify:** row counts and per-row versions are equal. The Hub `policy/check` message-notification steps become unused (Messaging push has moved). Blocks and privacy stay in the Hub (X4).

**Rollback:** switch the facade back. Changes made in the service are written back to the Hub using the version columns. This is possible because the service keeps a change log for 7 days.

### 4.3 Standard email: developer notices, meeting reminders, changes and cancellations

**Pre-conditions:**
- N0-1 (Hub grant) and N0-3 fixed.
- **Resend activated** (N0-2) and **re-accepted from the service**: `email-acceptance` retargeted to the service ledger, with the Resend webhook endpoint added to the account.

**Switch per category:**
1. Fence the category's email routing.
2. Meeting stops creating old ledger rows and old notification jobs for new events in that category.
3. The service schedules reminders from `conference.*` events and resolves recipients through X2 at fire time.
4. Old PENDING jobs and QUEUED ledger rows drain through the old worker.

**Drain:** Q18 shows the backlog. Resend webhooks go to both endpoints until the old ledger has no non-final rows.

### 4.4 Invitation emails (token-bearing)

Meeting and the Meeting relay seal the token (and, for organization invitations, the address) to the service key, and emit `*.email-requested`.
- The `emailDelivery` field in the `v2_create_invitation` / `v2_create_organization_invitation` responses keeps its meaning: ACCEPTED when the event is committed, DISABLED as before.
- The fence is per category.

### 4.5 Security notices (password changed, device linked)

These move to the CRITICAL lane through the **transactional** Account outbox, which also fixes S-9 (notices are no longer lost after commit).

**Pre-conditions:**
- D-9: alerting deployed with a real receiver.
- CRITICAL-lane metrics live.
- 30 days of 4.3 stable.

### 4.6 Sign-in, verification, reset and device step-up codes (last)

**Pre-conditions:**
- 30 days of 4.5 stable.
- CRITICAL p95 handoff ≤ baseline (Q17) + 5 s in shadow.
- Both replicas healthy through at least one rolling release.
- Break-glass `ACCOUNT_EMAIL_FALLBACK=local` tested in production (the switch takes effect within one poll interval).

**Canary:** acceptance accounts → 10 % → 50 % → 100 %, chosen by account hash.

**Duplicate control:**
- The routing decision is made inside Account's transaction (old enqueue **or** outbox row, never both).
- The idempotency key format `otp:verify:<recipient>:<digest>` is kept, so a resend request collapses as today.

**What remains in Account forever:**
- code generation, hashing, expiry, attempt limits and verification;
- `/api/v2/me/security-events`.

### 4.7 Operator alerts (optional)

Deploy Alertmanager (D-9) with receiver 1 = the service's I10 (operator email) and receiver 2 = a direct path (the GitHub issue, as the TLS monitor uses). The service's **own** health alerts must use the direct path only.

### 4.8 Customer webhooks (only if D-W)

This is not recommended before every user channel has been stable for 90 days. If approved:
- move `devplat_webhook_*` tables and the worker into the service as a WEBHOOK channel;
- the Developer Platform keeps the subscription API as a facade;
- use the same fence pattern (deliveries created after the fence go to the service; earlier ones finish in the Developer Platform);
- keep signing secrets sealed under a re-keyed `secret_box`.

This requires its own audit addendum before approval.

---

## 7. Phase 5: historical data

| Data | Treatment |
|---|---|
| Old email ledger (`email_deliveries`, events) | **not copied** (it holds code bodies until final). It drains, then expires by its own retention (60 d). Delivery-status lookups during the overlap query both (I4 reads the service ledger; the old `/internal/operations/email/deliveries/{id}` stays until Phase 8) |
| `meeting_notifications` / recipients | not copied; drain + 30 d retention |
| Push registrations | none expected (Q6); otherwise a sealed copy |
| **Preferences** | **copied** (authoritative data move, §4.2); Hub columns kept read-only until Phase 8 |
| Security events, call history, voicemail, recordings, org invitations | stay with their owners; inbox items reference IDs; no backfill |
| E2EE state | never touched |

---

## 8. Phase 6: controlled cutover

| Control | Specification |
|---|---|
| Flags | `notif_routing` (category, channel) + producer routing configuration, set by one operation; certify fails on disagreement |
| Canary | acceptance accounts → 10 % → 50 % → 100 % by `hash(accountRef)` for per-account categories; per-category fences otherwise |
| Gates | Document 7 §5 |
| Provider idempotency limits | Resend `Idempotency-Key` honoured only within Resend's window; SMTP has none; push collapse IDs replace visible notifications but are not idempotency. **Hence the fence decides before any provider is involved** |

---

## 9. Phase 7: rollback (per stage)

The universal mechanism is to **route new messages back to the old system**. In-flight messages finish in the system that created them. Nothing is ever in both.

| Stage | Trigger | Restoration | In-flight | Reconciliation | Duplicates | Preferences | DB compatibility | Resume after |
|---|---|---|---|---|---|---|---|---|
| P1 | crash loop, resource gate | remove containers | none | none | n/a | n/a | additive only | 24 h stable |
| P2 | producer DEAD > 0, lag | publication flags off | queued rows DEAD (harmless) | discard shadow | n/a | n/a | additive only | lag at baseline |
| P3 | inbox errors, wrong recipient | relay off | items hidden | — | none | — | — | matrix tests pass |
| 4.1 push | invalid-handle surge, wrong device, auth errors | Messaging resumes job creation; facades point back; `CALLING_PUSH_TRANSPORT=none` | the service finishes or expires its jobs (24 h) | copy endpoints back if any existed | fence | — | — | provider errors at baseline |
| 4.2 prefs | version mismatch, write errors | facade back to the Hub | — | write back changes by version from the 7-day change log | — | **preserved** (write-back) | Hub columns intact | counts and versions equal |
| 4.3 / 4.4 email | failure rate > baseline + 2σ, mismatches, complaints | per-category fence back to the old Meeting path | the service ledger drains its own rows | none (rows live in one ledger) | fence | — | — | 7 days clean shadow |
| 4.5 / 4.6 critical | CRITICAL oldest pending > 60 s, failure above baseline, **any** user report of a missing code | `ACCOUNT_EMAIL_FALLBACK=local` (immediate) then fence back | service CRITICAL rows drain; codes in them stay valid until their TTL | none | routing decided in Account's transaction | — | — | root cause + 14 days shadow |
| 4.7 operator | — | remove receiver 1 | — | — | — | — | — | — |

---

## 10. Phase 8: retirement (evidence required)

| Retire | Evidence required | Keep forever |
|---|---|---|
| Meeting email worker, providers, `email_deliveries` writes, Resend webhook route, `/internal/operations/email/*` | 60 days at 100 % with no fallback use (`ACCOUNT_EMAIL_FALLBACK` never set); old ledger has 0 non-final rows; code search shows no `email_delivery::enqueue` caller; then 60 more days for retention to expire | Account code authority |
| Meeting notification worker (`meeting_notifications`, recipients) | 30 days with no new rows; drain complete | conference business logic |
| Messaging push dispatcher, adapters, `messaging_push_jobs`, `messaging_push_registrations` | 30 days with no new jobs; registrations empty or copied | Messaging authority (exposed through X3) |
| Hub notification preference columns and `policy/check` notification actions | 60 days as a facade with no direct reads | Hub blocks, privacy, contacts |
| Calling `NoPushTransport` | transport certified | ringing/signaling |
| Shadow tables and comparator | all categories at 100 % for 30 days | — |
| Break-glass fallback in Account | 90 days of CRITICAL lane with zero fallback use, and owner sign-off | — |

**Immediate removal of the Meeting email delivery or the Messaging push adapters is not proposed.** Each retirement is its own reviewed change with a consumer inventory.
