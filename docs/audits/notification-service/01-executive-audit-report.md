# Document 1 — Executive Audit Report

**CF Meet Notification Service: audit and zero-disruption migration blueprint**

| | |
|---|---|
| Status | **AUDIT ONLY. No implementation is authorized.** |
| Repository | `CosmicforgeHealthnet/cfmeet`, `main` at `6f3743f806434a55bad8297f104d28a55e350e2e` (2026-11-15) |
| Production | `https://api.cfmeet.com` (single Contabo VPS, coordinated release path) |
| Production evidence | Read-only `certify` run [43670057279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43670057279) (2026-11-15 02:56 UTC, verdict `CERTIFIED_READ_ONLY`, artifact 14144111736). Release run history from GitHub Actions. Unauthenticated `GET https://api.cfmeet.com/health` = 200. Dated ops reports under `docs/operations/` |
| Raw evidence | `evidence/01…07` in this folder. Each claim there has a path, symbol and line |
| Changes made by this audit | Documentation files in `docs/audits/notification-service/` only. No code, configuration, migration, deployment or production write |

---

## 1. Bottom line

CF Meet does **not** need a large, fully consolidated notification platform. It does have a real, evidence-backed gap that no existing service can fill without breaking its own boundaries. That gap is:

- one place that decides who is notified, on which channel, under which preferences;
- one durable in-app notification history;
- one delivery record per notification.

**Recommendation: Option A, a narrow *notification orchestration service*** (`cfmeet-notification-service`). It consumes the existing transactional outboxes. It owns the in-app inbox, routing policy, scheduling and delivery tracking. It **reuses** the existing transports:
- Meeting Service's durable email ledger;
- Messaging's metadata-only push pipeline;
- Calling's realtime signaling.

Each stays where it is.

There is one deliberate exception, and it is evidence-driven. **Device push transport** has never carried a single production notification:
- `MESSAGING_PUSH_PROVIDER=none` in certify 43670057279.
- No web service worker exists, and no mobile push SDK exists.

It can therefore be given a single shared home *before* it goes live, at near-zero migration risk. That home is a shared crate plus an account-device-level push-endpoint registry. This is an owner decision (D-1 in Document 8), not an assumption.

Before any new service is built, **the defects in the existing notification paths should be repaired in their owning services** (Roadmap Section N0, 12 items). Two of them mean that today important notifications are very probably not being delivered at all:

1. **Account email (OTP, password reset) is not recovered.** Production still uses the implicit Gmail SMTP transport (`EMAIL_PROVIDER` reported as `smtp`, certify 43670057279). `docs/operations/CFMEET_FINAL_PRODUCTION_COMPLETION.md:121` records "OTP delivery is not yet recovered … `IMPLEMENTED_PENDING_CONFIGURATION`". The Resend activation (`activate-resend-email`) has never been run: it is absent from the deploy journal in certify 43670057279.
2. **Meeting notices and invitation emails to account holders very probably fail closed.** The Meeting Service reads Hub preferences with permission `communications-preferences.read`. No release tool grants that permission: `infra/calling/production/communications_release.py:178` grants Meeting only `communications-user.forward` and `communications-account.apply`, and the Hub bundle is rebuilt from scratch on each release (`:168`). On a failed read the job retries and then ends `FAILED` (`services/meeting-service/src/services/notifications.rs:994-996`). `docs/operations/profile-notifications-sections-9-11.md:221-227` states this consequence. This is static evidence. A read-only production query (Phase 0, Document 5 §2) must confirm it before repair.

---

## 2. What exists today (verified state)

| Capability | Owner | State in production | Key evidence |
|---|---|---|---|
| Durable email ledger (`email_deliveries`), worker, Resend + SMTP adapters, Svix-verified provider webhooks | Meeting Service | **Implemented, activated on SMTP, not production-evidenced as delivering.** Resend built, not activated | `services/meeting-service/src/services/email_delivery.rs`; migration `m20261112_000001_email_delivery_and_presence`; release 43309857124; diagnose 43317264046 ("all counts 0", "0 SMTP sends" over 168 h) |
| Account/security emails (verification code, reset code, password changed, device step-up, new browser / new primary phone) | Meeting Service (Account authority) | Implemented. Transport not recovered | `handlers/auth.rs`, `services/account_devices.rs:1494-1758` |
| Meeting notices (reschedule, cancel, reminders) and invitation emails | Meeting Service (`meeting_notifications`) | Implemented, flag `NOTIFICATIONS_EMAIL_ENABLED=true` (certify 43670057279). **Very probably failing for account holders** (missing grant) | `notifications.rs`, migration `m20261108_000002_meeting_notifications` |
| Message push: outbox → push jobs → leased dispatcher; APNs, FCM, Web Push adapters; metadata-only payloads | Messaging | **Implemented, no provider configured** (`MESSAGING_PUSH_PROVIDER=none`). Every job ends `NOT_CONFIGURED`. Never production-tested | `services/messaging/src/realtime/push.rs`, `push_adapters.rs`; migration `m20261106_000004_realtime` |
| Incoming-call ringing, first-answer arbitration, answered-elsewhere | Calling | **Implemented, production-tested** (runs 41580265031, 41581639199). Realtime WebSocket only | `services/calling/src/calls/app/ringing.rs`, `signaling/ws.rs`, ADR 0008 |
| Call push (VoIP / background) | Calling | **Absent.** `NoPushTransport` hard-wired | `services/calling/src/main.rs:453` |
| Notification preferences (7 categories, conversation mute, contact mute) | Communications Hub | CRUD production-tested (runs 42189631003, 42191576906). Only 3 of 7 categories are read by anything | `services/communications/src/preferences.rs`, `policy.rs:173-208` |
| Customer webhooks (signed, retried, replayable) | Developer Platform | **Implemented, activated, production-tested** (runs 43173688215, 43184898911, 43337892242, 43488534526) | `services/developer-platform/src/webhooks/*`, `packages/webhooks` |
| Organization events (transactional outbox, ordered, dead-lettered) | Organizations | Activated (run 42710192324). Only subscriber: Meeting Service. 35 delivered, 0 dead (certify 43670057279) | `services/organizations/src/outbox.rs:41-59` |
| In-app notification inbox / bell / unread badge | — | **Absent everywhere** (backend and both apps) | evidence/06 §3 |
| Operator alerting | — | Rules written and CI-validated. **Nothing deployed, all Alertmanager receivers empty.** Only the TLS monitor notifies (GitHub issue #38) | `infra/monitoring/alertmanager/alertmanager.yml:19-66`, `docs/operations/monitoring-alerts.md:3-5` |
| Web push / mobile push clients | apps/web, apps/mobile | **Absent.** No service worker, no `expo-notifications`. iOS declares `voip` background mode with no implementation | evidence/06 §1; `apps/mobile/app.json:26-28` |

**SMS:** no service sends SMS, and no SMS OTP exists. The SMS Gateway has only a test adapter, refused in production. `SMS_GATEWAY_PROVIDER` is unset. This audit does not propose activating SMS.

---

## 3. Major findings

### 3.1 Fragmentation (what centralization should fix)

1. **Three unrelated notification pipelines, each with its own retry/lease/backoff model:**
   - email ledger: 90 s lease, 5 s·2ⁿ ±20 % backoff, cap 15 min;
   - meeting notification jobs: 120 s lease, 2ⁿ backoff, cap 900 s;
   - Messaging push jobs: 60 s lease, 1 s·2ⁿ backoff, cap 5 min.
   On top of these there are four outboxes that are near-duplicates (Hub, Organizations, Account, Messaging) plus Calling's.
2. **No home for notifications that are not email, push or a call ring.** Examples:
   - contact requests (polling only);
   - organization invitations to existing users (no "my invitations" endpoint);
   - missed calls (derived at read time, never announced);
   - voicemail deposit (mailbox owner never told);
   - role changes, suspensions, ownership transfers (silent);
   - developer credential lifecycle (webhook only, no human channel).
3. **Preferences without enforcement.** `incoming_calls`, `missed_calls`, `contact_requests` and `group_invitations` are stored by the Hub and read by nobody. Mute semantics are duplicated across contact mute (no expiry), conversation mute (`muted_until`) and account switches. `MENTIONS` behaves as `MUTED` even for MANAGED messages, where the server knows the mentions.
4. **Organization-scope messaging cannot receive push at all.** Registration requires an E2EE `messaging_devices` row, and organization clients use session devices (`push.rs:172`, `m20261202_000030_session_devices_inbox.rs:4-7`).
5. **No delivery monitoring.** There are no email/push/webhook dead-letter alerts in production and no notification metrics for meeting jobs (`notifications::status_counts` has no caller).

### 3.2 Correctly centralized already (do not disturb)

- **Email:** Meeting Service is the single email authority (grep-verified). Its ledger has exact-once enqueue semantics: `UNIQUE (idempotency_key)`.
- **Customer webhooks:** centralized in the Developer Platform. Calling's legacy authority is retired.
- **Ringing and answer arbitration:** in Calling, transactional, production-tested.
- **Preference storage:** a single writer (Communications Hub).
- **Push payload confidentiality:** a DB CHECK plus an adapter whitelist enforce metadata-only payloads.

### 3.3 Security issues to repair (independent of the new service)

| # | Issue | Severity | Evidence |
|---|---|---|---|
| S-1 | Meeting → Hub preference grant missing; notices fail closed | High (availability) | `communications_release.py:178`; `notifications.rs:994-996` |
| S-2 | OTP/account email transport not recovered | High (account recovery) | `CFMEET_FINAL_PRODUCTION_COMPLETION.md:121` |
| S-3 | `/internal/operations/email/*` mounted on the public router behind a single shared `INTERNAL_API_TOKEN`. Whether nginx blocks it publicly is **unverified** | Medium (verify) | `services/meeting-service/src/routes/mod.rs:96-107` |
| S-4 | Any sender (including device client tokens) can set `notify:false` and suppress recipients' pushes | Medium | `services/messaging/src/messages/mod.rs:270-272`, `e2ee.rs:236` |
| S-5 | Account OTPs stored plaintext in Redis, email in the key name, compared with `==` (device codes are hashed and constant-time) | Medium | `services/meeting-service/src/services/otp.rs:60-114` |
| S-6 | Enumeration: `verify-reset-token` answers 200/400 by reset state; `register` returns 409 | Medium | `handlers/auth.rs:913-946, 91-98` |
| S-7 | Principal erasure leaves push handles (`messaging_push_registrations`) | Medium (privacy) | `services/messaging/src/compliance/deletion.rs:258-265` |
| S-8 | Lease shorter than worst-case batch in the email worker (90 s vs 10 × 20–30 s) and the push dispatcher (60 s vs 200 × 10 s), so duplicate sends are possible with more than one instance | Medium | `email_delivery.rs:76,620-644`; `push.rs:401-421,529` |
| S-9 | Security notices (password changed, new device) are best-effort after commit, with no retry ledger | Medium | `handlers/auth.rs:876-882`; `account_devices.rs:769-777` |
| S-10 | Web logout does not revoke the browser's Calling endpoint | Medium | `apps/web/src/services/auth.service.ts:97-142` |
| S-11 | One invalid address in a meeting notice blocks every alphabetically later recipient | Medium (correctness) | `notifications.rs:1033-1067` |
| S-12 | No bounce/complaint suppression list | Low/Medium | `email_delivery.rs:998-1086` |

The full list is in Document 8.

---

## 4. Recommended architecture (summary)

```
 producers (unchanged business logic, existing outboxes)
   Meeting/Account ─┐  Organizations ─┐  Hub ─┐  Calling ─┐  Developer Platform ─┐
                    ▼                 ▼       ▼           ▼                      ▼
              POST /internal/v1/notification-events   (signed, idempotent, schema-versioned)
                                   │
                    ┌──────────────▼───────────────┐
                    │  cfmeet-notification-service │  policy · recipients · inbox · schedule ·
                    │  (Postgres queues, no Redis) │  delivery ledger · shadow comparator
                    └──┬───────────┬───────────┬───┘
     email (enqueue)   │           │ push      │ in-app
                       ▼           ▼           ▼
        Meeting email ledger   push transport   notif inbox (HTTP, polled; realtime nudge later)
        (unchanged authority)  (shared crate;
                                D-1 decides endpoint registry owner)

 NOT routed through the service: Calling ringing/arbitration/signaling · Messaging ciphertext,
 realtime and E2EE checks · Account OTP generation/verification · customer webhooks
```

Why Option A over full consolidation (Option B):
- **Duplicate control is exact.** Email handover uses deterministic idempotency keys into the *same* ledger `UNIQUE` constraint, so old and new can both enqueue during cutover and only one email exists. Option B would need cross-provider idempotency, which Resend honours only within its window and SMTP not at all.
- **Rollback is configuration-only.** No data moves for email.
- **Security-critical flows stay where their token authority is** (Meeting/Account).
- Option B moves the most production-sensitive code (email ledger, OTP) for no functional gain.

Option C, putting the inbox into the Communications Hub, was also evaluated. The Hub governs only the personal-communications scope (`packages/communications-policy/src/lib.rs:11-55`). Account security, organizations, meetings and developer notifications are outside its authority, so C is rejected. See Document 3 §5.

---

## 5. Scale of the work

| Item | Count | Detail |
|---|---|---|
| New public (user) APIs | **5** | inbox list, unread count, single update, bulk read, bulk dismiss. All via the Meeting relay `/api/v2/notifications/*` |
| Extended existing public APIs | 2 | Hub `GET/PUT /api/v2/communications/me/notification-preferences` (additive fields) |
| New internal APIs on the new service | 7 + 2 conditional | event ingest, user dispatch (Meeting relay), admin deliveries, admin dead-letter list, admin dead-letter replay, shadow report, channel-authority read. Conditional on D-1: push-endpoint upsert, push dispatch. Scheduling, cancellation and template resolution need **no** endpoint: they are expressed in the event contract or internal to the service (Document 4 §3.3) |
| New internal APIs on existing services | 3 | Meeting `enqueueEmailDelivery` + `getEmailDelivery` (service-auth); Hub `resolveNotificationPolicy` (batch) |
| New tables | **9** (+1 conditional) | Document 4 §6 |
| Existing-service additive migrations | 3 | Hub preference extension; Hub outbox destination CHECK; Calling outbox destination column/enum |
| Workers | **4 permanent + 1 temporary** | ingest processor, delivery dispatcher, scheduler, retention; shadow comparator (temporary) |
| Inbound event types consumed | 24 (initial catalogue) | Document 4 §5 |
| Outbound events | 1 | `notifications.inbox.changed` (nudge, optional) |
| Infrastructure | 1 container (≤ 256 MiB limit), 1 database in the existing `cfmeet-core-db`, **no Redis, no queue product, no new server** | Document 3 §7 |

---

## 6. Safest sequence (summary)

| Step | What happens |
|---|---|
| **N0** | Repair defects in the owning services: grant, poison recipient, leases, `notify`, erasure, OTP storage, enumeration, nginx verification. Owner activates Resend (already prepared). |
| **Phase 0** | Read-only baseline queries (Document 5 §2). |
| **Phase 1** | Service foundations, deployed **dark**: no producer is switched. |
| **Phase 2** | Shadow ingestion: NS is added as an *extra* outbox destination and computes but never sends. |
| **Phase 3** | In-app inbox for categories that have **no current channel**, so there is nothing to duplicate (contact requests, org invitations, missed calls, voicemail, role changes). |
| **Phase 4** | Channel-by-channel ownership transfer behind fenced authority switches. Order: meeting notices (reminder, reschedule, cancel; these carry no tokens) → developer human notices → non-call push → optional in-app copies of security notices. These **never** transfer: account security email, call ringing, and token-bearing invitation emails (the token minter keeps sending them). |
| **Phase 5** | History: reference, don't copy. |
| **Phase 6** | Canary. |
| **Phase 7** | Rollback (configuration). |
| **Phase 8** | Retirement only with evidence. |

---

## 7. Answers to the Part 17 questions (short form)

The full answers are in `README.md` §Final verdict.

1. **Standalone service or extend?** A standalone but **narrow** orchestration service, plus extending existing services for transports. Full consolidation would be less safe.
2. **Reusable without modification:**
   - the email ledger and its worker;
   - Calling ringing and arbitration;
   - Developer Platform webhooks;
   - Organizations outbox mechanics;
   - Hub preference storage;
   - `cf-service-auth`, `cf-webhooks`, `cf-migration-guard`.
3. **Migrate:**
   - meeting notice orchestration (recipient resolution, preference check, scheduling for reminders, reschedules and cancellations);
   - developer human notices;
   - in-app history;
   - non-call push orchestration.
4. **Stay:**
   - OTP/verification and security email (Account);
   - token-bearing meeting and organization invitation emails (the token minter);
   - ringing, signaling and arbitration (Calling);
   - message wake-up decisions, E2EE rechecks and ciphertext (Messaging);
   - customer webhooks (Developer Platform);
   - preference writes (Hub);
   - infrastructure alerting (monitoring stack, not NS).
5. **Without downtime?** Yes for email and in-app, because email stays on the same ledger. Push has no production traffic to interrupt.
6. **Biggest risks:**
   - duplicate or lost notices during handover;
   - E2EE metadata leakage;
   - per-recipient policy fan-out load (about 6 signed calls per Hub decision);
   - the unverified internal-route exposure;
   - building on broken transports before N0.
7. **Size:** 5 new public APIs, 7 (+2) internal on NS, 3 internal on existing services, 9 (+1) tables, 4 (+1) workers, 24 consumed event types, 1 emitted.
8. **Infrastructure:** about 256 MiB, 1 database, ~0 additional CPU at measured volumes. It fits the measured 5,076 MiB free memory (certify 43670057279) under the release gate.
9. **Sequence:** §6.
10. **Scope / definition of done:** Document 6.

`CFMEET_NOTIFICATION_SYSTEM_AUDIT_COMPLETE — IMPLEMENTATION_AWAITING_OWNER_APPROVAL`
