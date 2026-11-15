# Document 5 — Zero-Disruption Migration Plan (revision 2: full consolidation)

*Corrected 2026-11-15 (pre-coding review):*
- *the routing fence is now the protocol in Document 4 §9;*
- *email-provider activation is a separate track (Document 4 §14);*
- *the authentication fallback is verified (Document 4 §10.4);*
- *engineering completion and operational acceptance are separated (Document 6);*
- *reconciled against `main` @ `3763392`.*

**Goal:** every notification pipeline moves into `cfmeet-notification-service` with no interruption of any notification that works today.

**Rules that hold throughout:**
- **Every message is routed to exactly one system at creation**, by a sticky decision recorded in the producer's own database transaction (routing fence protocol v1, Document 4 §9). The decision is guarded by triggers on the old-path tables and audited hourly.
- Old pipelines remain installed and switchable back until Phase 8.
- Every switch is one journaled coordinated-release operation on `main`, gated like every existing activation:
  - the exact SHA must be running;
  - health must pass;
  - changed containers are restored automatically on failure.

---

## 1. Functions that must never be interrupted

| # | Function | How the plan protects it |
|---|---|---|
| C1 | Sign-in, registration, code verification, password reset | code authority stays in Account. Code **emails** move last (4.6), through a transactional outbox, with the verified AUTO/FORCED fallback to the old Meeting path (Document 4 §10.4) |
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
| Release in progress at audit time | run 43696469230 (`6f3743f`) **completed successfully** at 09:50 UTC | GitHub Actions |
| Web Push activation attempt (in Messaging) | run 43702218850 (10:03 UTC) **failed before any change**; tooling fix `de7b863` is on `main` (`activate_web_push`, `coordinated_release.py:1901-1932`). Not re-run at the time of writing | GitHub Actions; commit message of `de7b863` |
| `main` head at reconciliation | `3763392` (2 commits after the audit base; no notification code changed) | `git log 6f3743f..origin/main` |

### 2.2 Outstanding production baseline measurements (Q1–Q18)

**Status: all 18 are OUTSTANDING.** As of 2026-11-15 none has been run, and no document in this blueprint may treat an expected value as a fact. Statements such as "Q6 expected 0" are hypotheses to be confirmed. Every gate in Document 7 that uses a baseline `B(·)` stays **blocked** until the corresponding query is recorded in a dated ops report with its run ID.

These must be run through a **new read-only `diagnose` extension**, or by the owner with the runtime role in a read-only session (`SET TRANSACTION READ ONLY`). They return counts and ages only; no addresses, content or handles.

| Q | Database | Query (counts only) | Answers | Status |
|---|---|---|---|---|
| Q1 | meeting (`rtc_db`) | `SELECT category, status, count(*), min(created_at), max(created_at) FROM email_deliveries GROUP BY 1,2;` | email volume, failure mix, stuck QUEUED (OTP bodies retained) | OUTSTANDING |
| Q2 | meeting | `SELECT last_error_class, count(*) FROM email_deliveries WHERE status IN ('QUEUED','FAILED','EXPIRED') GROUP BY 1;` | SMTP rejection evidence (S-2) | OUTSTANDING |
| Q3 | meeting | `SELECT kind, status, last_error, count(*) FROM meeting_notifications GROUP BY 1,2,3;` | **confirms or refutes S-1**: look for `PREFERENCES_UNAVAILABLE` / FAILED | OUTSTANDING |
| Q4 | meeting | `SELECT outcome, count(*) FROM meeting_notification_recipients GROUP BY 1;` | suppressed vs queued | OUTSTANDING |
| Q5 | meeting | `SELECT count(*) FILTER (WHERE status='PENDING'), max(attempts) FROM account_communication_outbox;` | infinite-retry backlog | OUTSTANDING |
| Q6 | messaging | `SELECT platform, enabled, count(*) FROM messaging_push_registrations GROUP BY 1,2;` | sizes the push move (expected 0: no data to copy) | OUTSTANDING |
| Q7 | messaging | `SELECT status, count(*), max(created_at) FROM messaging_push_jobs GROUP BY 1;` | job growth (no retention) | OUTSTANDING |
| Q8 | comm hub | `SELECT count(*), count(*) FILTER (WHERE NOT (direct_messages AND group_messages AND incoming_calls AND missed_calls AND contact_requests AND group_invitations AND meeting_invitations)) FROM communication_notification_preferences;` | how many users changed defaults | OUTSTANDING |
| Q9 | comm hub | `SELECT notifications, count(*) FROM communication_conversation_preferences GROUP BY 1;` and the same on `communication_contact_aliases` | mute usage | OUTSTANDING |
| Q10 | comm hub | `SELECT destination, state, count(*) FROM communication_outbox GROUP BY 1,2;` | Hub delivery health | OUTSTANDING |
| Q11 | organizations | `SELECT destination, state, count(*) FROM organization_outbox GROUP BY 1,2;` | baseline | OUTSTANDING |
| Q12 | calling | `SELECT status, count(*) FROM calling_outbox_events GROUP BY 1;` and `SELECT count(*) FROM calling_incidents WHERE status='OPEN';` | publication health | OUTSTANDING |
| Q13 | developer platform | `SELECT status, count(*) FROM devplat_webhook_deliveries GROUP BY 1;` | webhook health | OUTSTANDING |
| Q14 | trust bundle (file) | Is `communications-preferences.read` granted to `cfmeet.meeting-service` in the live Hub bundle? Read permissions only, never keys | **S-1 root cause** | OUTSTANDING |
| Q15 | nginx (file) | Does any `location` block `^/internal/operations/`? `curl -s -o /dev/null -w '%{http_code}' https://api.cfmeet.com/internal/operations/email/deliveries/00000000-0000-0000-0000-000000000000` **without** a token, run by the owner (expected 404 from nginx; 404 from the app is also "no token"; distinguish by the `server`/body signature) | **S-3** | OUTSTANDING |
| Q16 | host | `docker stats --no-stream` for the 13 containers; 7-day MemAvailable minimum | Phase 6 capacity gate | OUTSTANDING |


Three of these queries carry extra weight in revision 2:

| Q | Why it matters more now |
|---|---|
| Q1/Q2 | size the drain window of the old email ledger and set the CRITICAL-lane baseline |
| Q6/Q7 | hypothesis: 0 (unverified). If not 0, or if Web Push is activated in Messaging first, the copy path of 4.1 applies |
| Q8/Q9 | **the row counts to copy** in Phase 4.2 (preferences) |

**Baselines need a working transport.** Production SMTP is reported as failing (S-2), so Q1/Q2/Q17 taken *today* would describe a broken path. Email baselines `B(·)` are therefore taken over the **14 days after gate E1** (old path delivering with the owner-activated provider). Until then, every email gate stays blocked.

Two queries are added:

| Q | Query | Answers | Status |
|---|---|---|---|
| Q17 | `SELECT category, percentile_disc(0.95) WITHIN GROUP (ORDER BY accepted_at - created_at) FROM email_deliveries WHERE accepted_at IS NOT NULL AND created_at > now() - interval '14 days' GROUP BY 1;` | p95 enqueue-to-provider latency per category (CRITICAL baseline) | OUTSTANDING |
| Q18 | `SELECT count(*) FROM email_deliveries WHERE status IN ('QUEUED','SENDING');` | drain size at each fence | OUTSTANDING |

**Exit criteria:**
- Q1–Q18 are recorded in a dated ops report (status → MEASURED with the run ID).
- S-1, S-2 and S-3 are confirmed or refuted.
- N0 is complete for the channels about to move.

---

## 3. Phase 1: foundations (dark)

The full schema (17 tables), I1–I11, U1–U9 (relay route off), workers and metrics. **2 replicas.** All routing is OFF; producers have no grants.

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
- Q6 measured (OUTSTANDING).
- Messaging route decisions and guard trigger deployed (`notification_routing_state` category `MESSAGE_PUSH` in the Messaging database, Document 4 §9.2).
- Provider status (D-3/D-1b). As of `main` @ `3763392`, the owner has attempted `activate-web-push` **in Messaging** (run 43702218850, failed before change). If it is activated there before Phase 4.1, take the **copy path** below. The blueprint supports both; activating in Messaging only adds the copy step.

**Switch** (`activate-notifications-push`):
1. Set `MESSAGE_PUSH` to `FENCED_NEW` (canary percentage first). Each `message.created` event gets a sticky decision keyed by its `event_id`: `OLD` → `messaging_push_jobs`; `NEW` → publication to the service. The guard trigger refuses any push job without an `OLD` decision.
2. Jobs decided `OLD` finish or expire within 24 h in Messaging.
3. The Hub and Messaging push-registration endpoints become facades to U8/U9.
4. Calling: `CALLING_PUSH_TRANSPORT=notifications`.
5. Delivery status is answered by the service.

**Copy path (Q6 > 0, or Web Push activated in Messaging first):**
1. Copy registrations into `notif_push_endpoints`, sealing handles and mapping Messaging device → account device through the Hub index.
2. Registration writes go through the facade to the service from the switch onward, with the same <60 s write freeze as 4.2.
3. Messaging keeps its copy read-only until Phase 8.

**Duplicate control:** one decision per `event_id` (Document 4 §9.3). Jobs never exist in both systems. Collapse IDs cover provider-level retries only.

### 4.2 Preferences (single-writer switch)

1. **Copy:** Hub `communication_notification_preferences`, the `notifications`/`muted_until` columns of `communication_conversation_preferences`, and `communication_contact_aliases.notifications`. Each row records the Hub `version` in `source_version`.
2. **Catch-up:** apply Hub change-feed `NOTIFICATION_PREFERENCE` / `CONVERSATION_PREFERENCE` / `CONTACT` entries after the copy. The change feed already exists (`sync.rs:16-29`).
3. **Switch** (`activate-notifications-preferences`): the Hub endpoints become facades, so preference writes now go to the service. There is a **write freeze of under 60 s**: Hub PUTs answer 503 + `Retry-After: 5`. Web clients already retry `communicationsRequest` (`communications.service.ts:62-70`), and preference writes are rare (Q8).
4. **Verify:** row counts and per-row versions are equal. The Hub `policy/check` message-notification steps become unused (Messaging push has moved). Blocks and privacy stay in the Hub (X4).

**Rollback:** switch the facade back. Changes made in the service are written back to the Hub using the version columns. This is possible because the service keeps a change log for 7 days.

### 4.3 Standard email: developer notices, meeting reminders, changes and cancellations

**Pre-conditions:**
- N0-1 (Hub grant) and N0-3 fixed.
- **Delivery evidence gates E1 and E2 passed** (Document 4 §14.3). E1: the old path delivers, using whichever provider the owner activates in the E-track. E2: the service path delivers with the same provider.
- **The provider is activated by the owner (E-track); it is not part of service development.**

**Switch per category (4.3a):**
1. Set the category to `FENCED_NEW` with a canary percentage. Every message gets a sticky decision in the Meeting transaction that creates it (old ledger row **or** `NOTIFICATION` outbox row, never both, Document 4 §9.3).
2. Old-ledger rows decided `OLD` drain through the old worker.
3. Reminders: Meeting's existing scan remains the only trigger and routes each reminder by its `reminder_key` decision.

**Reminder scheduler takeover (4.3b):** after 30 days of 4.3a, the time-partition boundary `B` is set (Document 4 §9.9). The service scheduler owns starts `>= B`; the Meeting scan owns starts `< B`.

**Drain:** Q18 (OUTSTANDING) shows the backlog. Resend webhooks go to both endpoints, and the old endpoint stays registered until Phase 8 because it also serves the fallback (Document 4 §10.4, invariant 6).

### 4.4 Invitation emails (token-bearing)

Meeting and the Meeting relay seal the token (and, for organization invitations, the address) to the service key, and emit `*.email-requested`.
- The `emailDelivery` field in the `v2_create_invitation` / `v2_create_organization_invitation` responses keeps its meaning: ACCEPTED when the event is committed, DISABLED as before.
- The fence is per category, with deterministic keys `meeting-invitation:<invitation_id>` and `org-invitation:<invitation_id>:<token_version>`.
- Invitations are **not reroutable** (Document 4 §9.8). A cancelled delivery is reported as `emailDelivery: FAILED` so the inviter can resend.

### 4.5 Security notices (password changed, device linked)

These move to the CRITICAL lane through the **transactional** Account outbox, which also fixes S-9 (notices are no longer lost after commit).

**Pre-conditions:**
- D-9: alerting deployed with a real receiver.
- CRITICAL-lane metrics live.
- Operational acceptance of 4.3 (30 days).
- **Gate E3** (≥ 14 days of real STANDARD delivery evidence from the service).
- X6/X7 leases and AUTO fallback deployed and drilled (Document 4 §10.4).

### 4.6 Sign-in, verification, reset and device step-up codes (last)

**Pre-conditions:**
- Operational acceptance of 4.5 (30 days).
- **Gate E4:** `email-acceptance confirm=otp` through the service with delivery evidence, plus a successful fallback drill within 7 days.
- CRITICAL p95 handoff ≤ `B(Q17)` + 5 s in shadow. Q17 must be MEASURED first.
- Both replicas healthy through at least one rolling release.
- **Fallback verified in production** (Document 4 §10.4):
  - AUTO failover exercised by stopping both service replicas in a maintenance window: the code email is delivered through the old path within 60 s + delivery time;
  - FORCED mode switched on and off without a restart;
  - all readiness invariants certified.

**Canary:** acceptance accounts → 10 % → 50 % → 100 %, chosen by account hash.

**Duplicate control:**
- The routing decision is made inside Account's transaction (old enqueue **or** outbox row, never both). It is sticky per key, and the guard trigger is installed on `email_deliveries`.
- The service sends only under an X6 lease and reports X7 completion. Reroute follows the lease table (Document 4 §9.8).
- The idempotency key format `otp:verify:<recipient>:<digest>` is kept, so a resend request collapses as today.

**What remains in Account forever:**
- code generation, hashing, expiry, attempt limits and verification;
- `/api/v2/me/security-events`.

### 4.7 Operator alerts (optional)

Deploy Alertmanager (D-9) with receiver 1 = the service's I12 (operator email) and receiver 2 = a direct path (the GitHub issue, as the TLS monitor uses). The service's **own** health alerts must use the direct path only.

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
| Routing state | producer `notification_routing_state` (Meeting and Messaging databases) and service `notif_routing`, both set by **one** journaled operation and read at runtime (no container restart). Certify fails on any disagreement ("routing agreement") and on any audit violation (Document 4 §9.10) |
| Activation preconditions | every running replica of the producer and the service reports `routingProtocol >= 1`; the guard triggers are installed; the rollback floor is recorded (Document 4 §9.6) |
| Canary | acceptance accounts → 10 % → 50 % → 100 %. The canary is a pure function of the idempotency key (`hash(key) mod 100 < canary_percent`), so a retry always takes the same route |
| Gates | Document 7 §5 (baseline gates stay blocked until the matching Q is MEASURED) |
| Provider idempotency limits | Resend `Idempotency-Key` (UUIDv5 of the key) is honoured only within Resend's window; SMTP has none; push collapse IDs replace visible notifications but are not idempotency. **Hence the decision is made in the producer transaction, before any provider is involved** |

---

## 9. Phase 7: rollback (per stage)

The universal mechanism is to **route new decisions back to the old system** by setting the category to `FENCED_OLD`: one operation, no restart. Existing decisions are sticky. In-flight `NEW` messages either drain in the service (default), or are rerouted only through the audited paths of Document 4 §9.8:
- unilateral for unacked messages;
- two-phase I11 for acked STANDARD messages;
- lease-fenced for CRITICAL.

**No message is ever in both ledgers** (invariant F2).

| Stage | Trigger | Restoration | In-flight | Reconciliation | Duplicates | Preferences | DB compatibility | Resume after |
|---|---|---|---|---|---|---|---|---|
| P1 | crash loop, resource gate | remove containers | none | none | n/a | n/a | additive only | 24 h stable |
| P2 | producer DEAD > 0, lag | publication flags off | queued rows DEAD (harmless) | discard shadow | n/a (SHADOW is never sent) | n/a | additive only | lag at baseline |
| P3 | inbox errors, wrong recipient | relay off | items hidden | — | none | — | — | matrix tests pass |
| 4.1 push | invalid-handle surge, wrong device, auth errors | `MESSAGE_PUSH` → `FENCED_OLD`; facades point back; `CALLING_PUSH_TRANSPORT=none` | NEW-decided wake-ups finish or expire in the service (24 h); unacked ones reroute to Messaging jobs | copy endpoints back if any existed | one decision per `event_id` | — | — | provider errors at baseline |
| 4.2 prefs | version mismatch, write errors | facade back to the Hub | — | write back changes by version from `notif_preference_change_log` (7 days) | — | **preserved** (write-back) | Hub columns intact | counts and versions equal |
| 4.3 / 4.4 email | failure rate > `B` + 2σ, audit violation, complaints | category → `FENCED_OLD` | NEW decisions drain in the service; if the service is unhealthy, two-phase I11 reroute for reroutable categories; invitations reported `emailDelivery: FAILED` | hourly audit (Document 4 §9.10) | sticky decisions; F2 | — | — | 7 days clean shadow |
| 4.3b reminder takeover | missed reminders | boundary `B` moved forward (later starts return to the Meeting scan) | reminders already scheduled in the service for starts `>= B_old` fire there unless cancelled through `cancelKey` | partition is disjoint | by construction | — | — | 7 days clean |
| 4.5 / 4.6 critical | CRITICAL oldest pending > 60 s, failure above baseline, **any** user report of a missing code | `critical_fallback = FORCED` (immediate, no restart), then category → `FENCED_OLD` | never-leased decisions reroute automatically; leased-and-uncompleted ones only manually (recorded possible duplicate); completed ones are terminal | lease audit | X6/X7 lease protocol | — | — | root cause + 14 days shadow |
| 4.7 operator | — | remove receiver 1 | — | — | — | — | — | — |

---

## 10. Phase 8: retirement (evidence required)

Retirement is a separate milestone after **operational acceptance** (Document 6 §Milestones). It is never part of engineering completion.

| Retire | Evidence required | Keep forever |
|---|---|---|
| Meeting email worker, providers, `email_deliveries` writes, Resend webhook route, `/internal/operations/email/*` | operational acceptance of every email category; **90 days of CRITICAL lane with zero AUTO or FORCED fallback use**; old ledger has 0 non-final rows; code search shows no old-path enqueue caller; owner sign-off; then 60 more days for retention to expire | Account code authority |
| Route decision tables and guard triggers | all categories retired from the old path; decisions past expiry + 30 d | — |
| Meeting notification worker (`meeting_notifications`, recipients) | 30 days with no new rows after the scheduler takeover; drain complete | conference business logic |
| Messaging push dispatcher, adapters, `messaging_push_jobs`, `messaging_push_registrations` | 30 days with no new jobs; registrations empty or copied | Messaging authority (exposed through X3) |
| Hub notification preference columns and `policy/check` notification actions | 60 days as a facade with no direct reads | Hub blocks, privacy, contacts |
| Calling `NoPushTransport` | transport certified | ringing/signaling |
| Shadow tables and comparator | all categories operationally accepted | — |

**Immediate removal of the Meeting email delivery or the Messaging push adapters is not proposed.** Each retirement is its own reviewed change with a consumer inventory.
