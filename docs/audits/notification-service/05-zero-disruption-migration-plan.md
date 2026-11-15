# Document 5 — Zero-Disruption Migration Plan

This plan preserves every working notification path at every step. Rules that apply throughout:
- **Only one delivery authority is active per (category, channel) at a time** (Document 4 §9).
- Every authority change is a single, journaled coordinated-release operation on `main`.
- Every step is gated by the same rules as existing activations:
  - the release of the exact SHA must be the one running;
  - health checks must pass;
  - on failure, the containers the step changed are restored automatically (`docs/operations/coordinated-production-release.md`).

---

## 1. Functions that must never be interrupted

| # | Function | Owner | Why it is safe in this plan |
|---|---|---|---|
| C1 | Sign-in, registration, OTP issue/verify, password reset | Account (Meeting) | never routed through NS; the Account email path is unchanged |
| C2 | Call ringing, answer arbitration, answered-elsewhere, recovery | Calling | never routed through NS; the push fast path is post-commit and best-effort |
| C3 | Message send, ciphertext storage, realtime delivery, receipts | Messaging | never routed through NS |
| C4 | Device linking / step-up / revocation | Account | unchanged |
| C5 | Organization membership and access changes (offboarding) | Organizations → Meeting | the existing subscriber is untouched; NS is an *additional* subscriber |
| C6 | Customer webhooks | Developer Platform | untouched |
| C7 | Meeting notice emails (where currently working) | Meeting | stay with the legacy worker until a fenced, key-deduplicated handover |

---

## 2. Phase 0: baseline and risk inventory (read-only)

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
| Q6 | messaging | `SELECT platform, enabled, count(*) FROM messaging_push_registrations GROUP BY 1,2;` | **decides D-1 cost** (expected 0) |
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

**Phase 0 exit criteria:**
- Q1–Q16 are recorded in a dated ops report.
- S-1, S-2 and S-3 are confirmed or refuted.
- Volumes are known, so that Phase 2/6 thresholds can be derived from them (Document 7 §5).

### 2.3 Pre-requisite remediation (Roadmap N0)

These run in the owning services. Each is a normal coordinated release with its own rollback.
- Fix the Hub grant (S-1).
- Owner runs `activate-resend-email` + `email-acceptance confirm=otp` (S-2; already-built tooling).
- Poison recipient (S-11).
- Lease/batch bounds (S-8).
- `notify` restriction (S-4).
- Erasure of push handles (S-7).
- OTP hashing (S-5).
- Enumeration (S-6).
- nginx internal block (S-3).
- Push-job and outbox retention.

**NS work does not depend on N0 code, but Phase 4 does depend on S-1 and S-2 being fixed.** Do not migrate a channel whose current behaviour is broken: the baseline comparison would be meaningless.

---

## 3. Phase 1: service foundations (dark)

| Item | Detail |
|---|---|
| Deliverable | `services/notifications` (Roadmap N1–N3), DB `cfmeet_notifications`, migrations, I1–I7, U1–U5 behind the relay (relay route off), workers W1–W4, metrics, rules file |
| Production action | `provision` (DB + roles) → `release` (container starts; `/ready` passes) → **no grants to producers, no relay route, every authority OFF** |
| Switched producers | **none** |
| Acceptance | certify reports the container healthy; `notif_channel_authority` all OFF; no inbound traffic (`notif_events_ingested_total = 0`) |
| Rollback | stop/remove the container (no other service references it); the DB may remain (empty) |

---

## 4. Phase 2: shadow ingestion (no user-visible effect)

**Mechanism:** NS is added as an **additional destination** on each producer's existing outbox. The existing destinations are untouched:

| Producer | Change (additive) | Revert |
|---|---|---|
| Organizations | `subscribers()` adds `cfmeet.notifications` for `invitation.*`, `membership.*`, `ownership.transferred`, `organization.suspended`; `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service,cfmeet.notifications` | remove the destination from the env; rows for NS already queued are left to DEAD-letter harmlessly or are purged by the operator |
| Hub | outbox CHECK allows `NOTIFICATIONS`; `scoped_event` writes a third row for `connection.requested/connected` | config flag `COMMUNICATIONS_NOTIFICATIONS_DESTINATION=off` |
| Calling | publication destination `notifications` for `VOICEMAIL_READY`, `RECORDING_READY`, the new `CALL_PARTICIPANT_MISSED` | `CALLING_NOTIFICATIONS_DESTINATION=off` |
| Messaging | publish `messaging.member.invited` to NS | flag off |
| Meeting | notification worker **also** posts each claimed job as a `meeting-service.notice.*` event **in addition** to its normal email path (shadow copy; mode SHADOW) | flag off |
| Developer Platform | lifecycle sweep / production decision **also** publishes to NS | flag off |

NS behaviour in SHADOW:
- It ingests, resolves recipients, evaluates policy, and writes `notif_notifications.state='SHADOW'`, `notif_deliveries.status='SHADOW'`.
- It writes `notif_shadow_observations(side='NS')` for categories that have a legacy counterpart (meeting notices).
- The Meeting worker writes `side='LEGACY'` for the same `dedupe_key`, with a keyed HMAC digest of the recipient ref, never the address.
- It **makes no transport call and creates no inbox items.**

**Shadow measurements** (I6, alert rules):

| Measure | Definition |
|---|---|
| Event capture | `ingested / emitted` per producer. Emitted is counted by the producer's outbox for destination NS |
| Missed | legacy observation without an NS observation after 10 min |
| Extra | NS observation without a legacy one (for categories with legacy) |
| Recipient mismatch | same key, differing recipient digest sets |
| Policy mismatch | same recipient, different allow/suppress |
| Lag | `occurredAt` → NS processed (p50/p95) |
| Duplicates | `DUPLICATE` outcomes are expected (producer retries). Duplicated notifications must be 0 (enforced by `UNIQUE`) |

**Phase 2 exit (per category):**
- ≥ 14 days of shadow, or ≥ 1,000 legacy events, whichever is longer.
- Capture ≥ 99.9 %.
- Mismatches ≤ 0.1 %, each explained.
- p95 lag below the baseline-derived bound (Document 7 §5).
- Producer outbox DEAD rows for the NS destination = 0.

---

## 5. Phase 3: in-app notifications first

**Why first:** the in-app inbox has **no legacy equivalent** (evidence/06 §3), so turning it on cannot duplicate or remove anything users receive today.

| Step | Action |
|---|---|
| 3.1 | Grant Meeting `notifications-user.forward`; enable the relay route `/api/v2/notifications/*`; add U1–U5 to `meeting-service.json` (mobile contract) |
| 3.2 | Set `notif_channel_authority(category, IN_APP)=ACTIVE` per category, in this order: CONTACT_REQUEST, CONTACT_ACCEPTED, ORG_INVITATION (token-free copy), ORG_MEMBERSHIP, CALL_MISSED, VOICEMAIL, RECORDING_READY, MESSAGE_REQUEST, DEVELOPER_*. These categories have **no current channel at all** |
| 3.3 | Web: bell + list behind a frontend flag (layout unchanged elsewhere; a new component in the existing header slot requires design approval, Part 2 rule 9). Mobile: list screen behind a flag |
| 3.4 | Meeting categories in-app: MEETING_CHANGE and MEETING_REMINDER get in-app ACTIVE while **email stays legacy** (in-app is an additional channel, not a replacement) |

**Existing realtime paths are preserved:**
- Calling signaling, Messaging realtime and conference realtime are untouched.
- The Hub `/activity` and `/sync` keep their semantics.

**Exit criteria:** see Document 7 §5 (A3).

---

## 6. Phase 4: channel-by-channel ownership transfer

Each sub-phase lists:
- the moment ownership changes;
- the mechanism that prevents double sending;
- what stays.

### 4.1 Organizations

| | |
|---|---|
| Moves | in-app only (Phase 3). Optional inviter notice on `invitation.accepted/declined` (new) |
| Stays | **the token-bearing invitation email** (Meeting relay, P4); membership/access processing in Meeting (C5) |
| Ownership change | none for email; IN_APP ACTIVE in Phase 3 |
| Duplicates | none possible (no legacy in-app; email not migrated) |

### 4.2 Meeting notifications (reschedule, cancel, reminder): the only true email handover

**Pre-conditions:**
- S-1 fixed and Q3 shows SENT jobs for account holders.
- S-11 fixed.
- Resend active (S-2).
- Phase 2 shadow exit met for MEETING_*.

The steps are ordered:

1. **4.2a Dual-enqueue window (ACTIVE-ACTIVE, safe by construction).**
   - Set `notif_channel_authority(MEETING_*, EMAIL)=ACTIVE` with `fence_at = T`.
   - The Meeting worker keeps sending.
   - NS enqueues through X1 with `legacyNoticeRef=<job id>`. Meeting computes the legacy idempotency key `meeting-notification:{job}:{sha256(lower(addr))}` for that recipient.
   - Whichever side enqueues first creates the single ledger row. The other gets `created=false`.
   - Metric: `email_enqueue_duplicates` rises by about one per recipient. This **proves** both sides agree.
   - Duration: ≥ 7 days.
2. **4.2b Legacy stops rendering.**
   - For jobs with `created_at >= T2`, the Meeting worker forwards the event only (`MEETING_NOTIFICATIONS_DELIVERY=notifications`).
   - Jobs created before `T2` are finished by the legacy worker.
   - Because both paths use the same key, overlap at the boundary is harmless.
3. **4.2c Preference source switch.** NS uses X3 (channels and quiet hours). The Meeting `HubPreferences` call becomes unused for these categories but stays in code.

**What stays in Meeting:**
- the job row written in the business transaction (`record_rescheduled` / `record_cancelled`);
- the reminder scan (`enqueue_due_reminders`);
- recipient resolution (`RECIPIENTS_SQL`);
- protected titles (`presented_title`).

NS takes over channel selection, preferences/quiet hours, templates for push/in-app, and delivery tracking. **Invitation emails (token-bearing) stay** in the Meeting synchronous path.

### 4.3 Messaging push

Pre-conditions:
- **D-1** decided.
- `packages/push-transport` extracted (no behaviour change, N4).
- Owner activates a push provider: `activate-web-push` or the APNs/FCM credentials. This is owner decision D-3, outside this plan's authority.

**If Q6 = 0** (no registrations, expected):
- Switch the Hub facade target to NS I8 in one release: `COMMUNICATIONS_PUSH_REGISTRY=notifications`.
- Messaging's dispatcher calls NS I9 with `pushTargetRef`s resolved through the Hub device index (`chat_device_ref` ↔ account device).
- Messaging keeps all rechecks: membership, history window, block and Hub policy.
- **There is no data to migrate.**

**If Q6 > 0** (Web Push activated in Messaging earlier, D-1b):
1. Copy registrations, handle by handle, into `notif_push_endpoints` (sealed).
2. Dual-read: Messaging resolves NS endpoints first, falling back to its own table.
3. Fence: push jobs created at or after `fence_at` are dispatched via NS; earlier ones finish in Messaging.
4. After 24 h (the job TTL) Messaging's own adapters go idle.

**Duplicate control:**
- `UNIQUE (event_id, device_id)` in Messaging.
- `(collapseId=eventId, pushTargetRef)` in NS.
- The fence decides the single dispatcher per job.

**Stays:** the wake-up *decision*, the payload whitelist, and E2EE rechecks in Messaging. Organization/MANAGED scope gains push only after D-1, because session devices map to account devices.

### 4.4 Account-security notifications

| | |
|---|---|
| Moves | **nothing for email.** Optional: `account.security.*` events create **in-app copies** (non-suppressible) |
| Stays | OTP, reset codes, step-up codes, password-changed and new-device emails (Account → ledger), and `/api/v2/me/security-events` |
| Reason | Part 4.1 and Part 11. A security notice must not depend on a non-essential service. N0 makes the existing notices transactional (S-9) |

### 4.5 Calling-related alerts

| Alert | Plan |
|---|---|
| Ring alert (push) | Calling `PushDispatcher` implementation calling I9 (D-2) **or** an in-Calling dispatcher using `packages/push-transport`. Enabled by Calling configuration `CALLING_PUSH_TRANSPORT=notifications\|local\|none` (default `none` = today). **The WS ring path is unchanged; ringing never waits** |
| Missed call, voicemail, recording ready | new in-app (Phase 3), plus push/email channels when the authority is ACTIVE. No legacy channel exists, so there are no duplicates |
| Recording consent notice, answered-elsewhere | stay realtime in Calling (compliance and latency) |

### 4.6 Developer Platform

| | |
|---|---|
| Moves | human notices: credential expiring/expired, production-access decision. These go through NS email + in-app to organization admins / the applicant |
| Duplicate control (production-access decision email) | today it is only sent by the ops script (`developer_access.py::notify_applicant` → Meeting `/internal/operations/email/developer-access-decision`, key `devplat-access:<req>:<decision>`). NS uses X1 with the **same key** (`devplat-access:<req>:<decision>`), so a double run is exact-once at the ledger. The script call is removed only in Phase 8 |
| Stays | customer webhooks (`credential.*` webhooks keep firing) |

### 4.7 Transactional email orchestration (general)

| | |
|---|---|
| Moves | only NS-owned categories reach the ledger via X1 |
| Stays | the ledger, worker, providers, Svix callbacks and retention: **the Meeting Service remains the single email authority** |

The Option B migration of the ledger is explicitly **not** planned.

---

## 7. Phase 5: compatibility and historical data

| Data | Treatment | Rationale |
|---|---|---|
| `email_deliveries` / events | **left with the owner**; NS stores `transport_ref` = delivery id | contains OTP bodies until final; must not move |
| `meeting_notifications` / recipients | **left**; read by the shadow comparator only; legacy rows purge naturally (30 d) | no user-visible history today |
| Push registrations | **reference** (Q6 = 0) or **copy sealed** (Q6 > 0); Messaging rows disabled, not deleted, until Phase 8 | handles are sensitive, so a single store is the goal |
| Hub preferences | **left** (single writer); NS reads | no conflicting writer |
| Account security events | **left**; optional in-app copies only for new events (no backfill) | — |
| Calling history / voicemail / recordings | **left**; NS items reference IDs; no backfill of past missed calls | avoids surprising "new" notifications |
| Organizations invitations | **left**; no backfill | — |
| E2EE state (MLS, devices, ciphertext) | **never touched** | — |

There is **no historical backfill** into the inbox. The inbox starts empty at activation. This avoids a burst of stale notifications and moves no sensitive data.

---

## 8. Phase 6: controlled cutover

| Control | Specification |
|---|---|
| Feature flags | `notif_channel_authority` per (category, channel) + producer flags. Each changes only through an `activate-notifications-*` operation that refuses if the exact release SHA is not running |
| Canary | 1. Internal accounts allowlist (`NOTIFICATIONS_CANARY_ACCOUNT_REFS`; the acceptance accounts used by `communications-verify` / `organization-acceptance`) for ≥ 48 h. 2. 10 % by `hash(accountRef) mod 100 < 10` for ≥ 72 h. 3. 50 %. 4. 100 %. For email categories, accounts outside the canary keep the legacy-only path (NS SHADOW for them) |
| Health checks | NS `/ready`; dead letters 0; oldest pending < 2 min; breaker closed; shadow/legacy mismatch ≤ 0.1 % |
| Acceptance gates | Document 7 §5, per stage |
| Single authority | the authority table + legacy flags are set by the **same** operation; certify reports both sides and fails on disagreement (new certify check) |
| Provider idempotency limits | Resend `Idempotency-Key` = ledger id (`email_delivery.rs:364`), honoured only within Resend's idempotency window; SMTP has none. **Hence dedupe happens before the provider, at the ledger.** APNs/FCM/Web Push collapse IDs only replace *visible* notifications; they are not idempotency. Hence fencing per job |

---

## 9. Phase 7: rollback (per stage)

| Stage | Trigger (any one) | Restoration path | In-flight treatment | Reconciliation | Duplicate prevention | Preferences | DB compatibility | Monitoring before resuming |
|---|---|---|---|---|---|---|---|---|
| P1 dark | NS crash loop, resource gate breach | `rollback` restores the previous container set (NS removed) | none (no traffic) | none | n/a | untouched | NS DB unused; other DBs unchanged | 24 h stable `/ready` |
| P2 shadow | producer DEAD rows for the NS destination > 0; producer lag up; NS ingest 5xx > 1 % for 10 min | set each producer's NS destination off (one operation) | queued NS-destination rows: left to DEAD (harmless) or purged by the operator via existing replay/admin tools | shadow data discarded | n/a (no sends) | untouched | additive CHECK/subscriber changes remain (inert) | producer outbox lag back to baseline |
| P3 in-app | inbox API error > 1 %; wrong-recipient report; mismatch spike | relay route off; IN_APP authority OFF (items stay, invisible) | pending IN_APP deliveries → CANCELLED | none needed | none (no legacy) | untouched | items retained or purged by retention | root cause fixed; authorization-matrix tests pass |
| P4.2 meeting email | email failure rate > baseline + 2σ; missed recipients in shadow > 0.1 %; complaint/bounce spike | set `MEETING_NOTIFICATIONS_DELIVERY=legacy` and authority SHADOW (one operation). The Meeting worker resumes rendering for all jobs | NS deliveries in `DISPATCHING` finish (same key → no duplicate); `PENDING` → CANCELLED | the legacy worker processes all PENDING `meeting_notifications`; the recipient ledger (`meeting_notification_recipients`) prevents re-sends; ledger `UNIQUE` prevents duplicates with NS-enqueued rows | **deterministic key**: the legacy and NS enqueues of the same (job, recipient) collapse | Hub unchanged (single writer) | no schema reversal needed | shadow mismatch ≤ 0.1 % for 7 days |
| P4.3 push | invalid-handle surge, payload validation failure, provider auth errors, wrong-device delivery | Hub facade target back to Messaging; Messaging dispatcher back to local adapters | jobs at or after the fence and not yet sent are re-created by Messaging fan-out (`UNIQUE (event, device)`) or expire (24 h) | copy endpoints back if needed (sealed → plaintext only inside Messaging) | fence + collapse ID; residual duplicate wake-up accepted (metadata only) | untouched | NS endpoint table retained | provider error rate at baseline |
| P4.5 call push | any ringing regression (none expected: decoupled); push failure rate > 5 % | `CALLING_PUSH_TRANSPORT=none` | none (no queue) | none | n/a | untouched | none | 72 h canary clean |
| P4.6 developer | wrong recipient; email failures | authority OFF; the script path still exists until Phase 8 | PENDING → CANCELLED | same key with the script path | ledger key | untouched | none | 7 days clean |

**Universal rule:** a rollback never deletes NS data and never reverses a migration in an existing service. Every additive change stays inert when switched off.

---

## 10. Phase 8: legacy retirement (evidence required)

| Candidate | Retire only when (evidence) | Never retire |
|---|---|---|
| Meeting notification worker's *rendering* path (`deliver` composing emails) | 30 consecutive days at 100 % NS authority; 0 rollbacks; `email_enqueue_duplicates` attributable to legacy = 0 for 14 days; Q3 shows no PENDING legacy-rendered jobs; consumers inventory confirms no other caller | the job *writer* (`record_*`) and the reminder scan (business-transaction guarantee) |
| Meeting `HubPreferences` call for MEETINGS | no caller (code search + metric) for 30 days | — |
| `/internal/operations/email/developer-access-decision` + script email call | NS developer notices ACTIVE for 30 days; the script updated to stop calling | the email ledger itself |
| Messaging local push adapters (`push_adapters.rs` usage in Messaging) | all dispatch via NS for 30 days; Messaging registrations disabled and empty | Messaging's wake-up decision, payload whitelist, `messaging_push_jobs` (or replace with an NS reference later, separate decision) |
| `messaging_push_registrations` rows | copied and verified; 30 days unused; then delete handles (privacy win) | — |
| Hub `push_platform` mirror | NS registry authoritative for 30 days | Hub device index |
| `notif_shadow_observations` and the comparator | all categories ACTIVE ≥ 30 days | — |
| Calling `NoPushTransport` | a real transport configured and certified | Calling ringing/signaling |

**Explicitly not proposed for removal:**
- Meeting Service email delivery (ledger, worker, providers);
- Messaging push adapters (moved into a shared crate, not deleted);
- Calling signaling.

Each retirement is its own reviewed change with a consumer inventory: callers, queues, APIs, callbacks.
