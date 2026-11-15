# Document 1 — Executive Audit Report

**CF Meet Unified Notification Service: audit and zero-disruption migration blueprint (revision 2)**

| | |
|---|---|
| Status | **AUDIT ONLY. No implementation is authorized.** |
| Revision | **2 (2026-11-15).** The owner chose full consolidation: one notification system used by every service, with all current notification pipelines moving into it. Revision 1 recommended a narrower orchestration service and is kept as a superseded alternative (Document 3 §3.2) |
| Repository | `CosmicforgeHealthnet/cfmeet`, `main` at `6f3743f806434a55bad8297f104d28a55e350e2e` (2026-11-15) |
| Production | `https://api.cfmeet.com` (single Contabo VPS, coordinated release path) |
| Production evidence | read-only `certify` run [43670057279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43670057279) (2026-11-15 02:56 UTC, `CERTIFIED_READ_ONLY`, artifact 14144111736); release run history; unauthenticated `GET https://api.cfmeet.com/health` = 200; dated ops reports under `docs/operations/` |
| Raw evidence | `evidence/01…07` in this folder. Each claim there has a path, symbol and line |
| Changes made by this audit | Documentation files in `docs/audits/notification-service/` only |

---

## 1. Bottom line

**Target: `cfmeet-notification-service` becomes the single notification authority of CF Meet.** Every service hands it notification requests. It is the only component that:
- evaluates notification preferences;
- chooses channels;
- stores and sends email;
- stores push registrations and sends push;
- keeps the in-app inbox;
- records delivery history.

These existing pipelines **move into it**:

| Today | Owner today | In the unified service |
|---|---|---|
| Durable email ledger, email worker, Resend/SMTP adapters, Resend delivery webhooks | Meeting Service | email channel (ledger, worker, providers, webhooks) |
| Account emails: verification and reset codes, device step-up codes, password-changed and new-device notices | Meeting Service (Account) | **critical lane**. Account still generates and verifies the codes |
| Meeting invitation, organization invitation, reschedule, cancellation and reminder emails, including reminder scheduling | Meeting Service | meeting categories + scheduler |
| Developer production-access decision email | Meeting endpoint called by an ops script | developer categories |
| Push registrations, push jobs, APNs/FCM/Web Push adapters, push dispatcher | Messaging | push channel |
| Call push hook (`PushDispatcher`, today `NoPushTransport`) | Calling | push channel, fast lane |
| Notification preferences: 7 categories, conversation notification mute, contact notification mute | Communications Hub | preference authority (Hub endpoints kept as compatibility facades) |
| Voicemail, recording, missed-call, contact-request, org-membership and developer-credential notices | none (missing today) | new categories |
| In-app notification inbox | none | inbox |
| Operator alert delivery (Alertmanager receivers are empty today) | none | operator channel (optional phase) |

**What does not move.** None of the items below is a notification system, and your brief protects each of them:

| Item | Why it stays |
|---|---|
| Calling's realtime ring signaling, first-answer arbitration and answered-elsewhere signals | Part 4.3: Calling owns low-latency ringing, and the notification service must not become a synchronous dependency for establishing a call. Call **push** does move |
| Account's generation, storage, expiry and verification of codes and tokens | Part 4.1: "token generation, expiry and verification authority must remain secure". Only the *delivery* of the email moves |
| Messaging's ciphertext, MLS, realtime message delivery and its membership/device authority | Part 4.4. The service asks Messaging to authorize each wake-up and never sees content |
| Business authority data (memberships, conversations, call participants, mailboxes) | the service asks the owning service; it never copies authority |
| Customer webhooks (Developer Platform) | Part 4.7: "different products and must not be merged blindly". Joining them is an **owner decision (D-W)**, offered as an optional final phase |

Consolidation is possible **without downtime**:
- every channel moves behind a fence, so each message is routed to exactly one system at the moment it is created;
- old paths stay installed as instant fallbacks until retirement is proven;
- the riskiest move, sign-in code email, comes **last**.

---

## 2. Two defects must be repaired first (Roadmap N0)

1. **Account email (OTP, password reset) is not recovered.** Production still uses the implicit Gmail SMTP transport (`EMAIL_PROVIDER` reported as `smtp` in certify 43670057279). `docs/operations/CFMEET_FINAL_PRODUCTION_COMPLETION.md:121` records "OTP delivery is not yet recovered". `activate-resend-email` has never run: it is absent from the certify journal.
2. **Meeting notices and invitation emails to account holders very probably fail closed.** The Meeting Service needs `communications-preferences.read`, and no release tool grants it (`infra/calling/production/communications_release.py:168,178`). On a failed read the job retries and then ends `FAILED` (`services/meeting-service/src/services/notifications.rs:994-996`). This is static evidence; Phase 0 query Q3 confirms or refutes it.

Migrating a broken pipeline would make its baseline meaningless, so neither may be migrated before repair. Ten further N0 items are listed in Document 6.

---

## 3. What exists today (verified state)

| Capability | Owner | State in production | Key evidence |
|---|---|---|---|
| Durable email ledger (`email_deliveries`), worker, Resend + SMTP, Svix webhooks | Meeting | activated on SMTP; delivery not evidenced; Resend built, not activated | `services/meeting-service/src/services/email_delivery.rs`; release 43309857124; diagnose 43317264046 |
| Account/security emails | Meeting (Account) | implemented; transport not recovered | `handlers/auth.rs`; `services/account_devices.rs:1494-1758` |
| Meeting notices + invitation emails | Meeting | `NOTIFICATIONS_EMAIL_ENABLED=true`; very probably failing for account holders | `notifications.rs` |
| Message push pipeline + APNs/FCM/Web Push | Messaging | implemented; `MESSAGING_PUSH_PROVIDER=none`; no client registers | `realtime/push.rs`, `push_adapters.rs` |
| Ringing + arbitration | Calling | production-tested (41580265031, 41581639199) | `calls/app/ringing.rs` |
| Call push | Calling | absent (`NoPushTransport`, `main.rs:453`) | — |
| Notification preferences | Hub | CRUD production-tested; 3 of 7 categories read | `preferences.rs`, `policy.rs:173-208` |
| Customer webhooks | Developer Platform | live, production-tested | `services/developer-platform/src/webhooks/*` |
| Org events | Organizations | live; only subscriber is Meeting | `services/organizations/src/outbox.rs:41-59` |
| In-app inbox | — | absent | evidence/06 |
| Operator alerting | — | rules written; nothing deployed | `infra/monitoring/alertmanager/alertmanager.yml` |
| Web/mobile push clients | apps | absent | evidence/06 §1 |

Full inventory: Document 2.

---

## 4. Major findings

### 4.1 Why one system is justified (evidence)

| # | Finding | Evidence |
|---|---|---|
| 1 | **Four notification pipelines with four retry models.** Email ledger: 90 s lease, 5 s·2ⁿ backoff. Meeting jobs: 120 s, 2ⁿ. Messaging push: 60 s, 1 s·2ⁿ. Hub/Org/Account outbox fan-out on top. No common history, metrics or alerting | evidence/01 §9.18; evidence/02 §1.6 |
| 2 | **Preferences are stored in one service and enforced (partly) in two others.** 4 of 7 categories are enforced nowhere. Mute semantics differ between contact and conversation. Organization scope is not governed at all | evidence/04 §2.3 |
| 3 | **Many events have no notification home**: contact requests, org changes, missed calls, voicemail, developer notices | Document 2 §B |
| 4 | **Push has two unfinished hooks** (Messaging adapters, Calling `NoPushTransport`), and **organization users cannot register push at all** | `push.rs:172`; `calling/src/main.rs:453` |
| 5 | **No delivery monitoring anywhere** | `monitoring-alerts.md:3-5` |

### 4.2 Security issues to repair (also in Document 8)

| ID | Issue | Severity |
|---|---|---|
| S-1 | Missing Hub preference grant | High |
| S-2 | OTP transport not recovered | High |
| S-3 | `/internal/operations/email/*` on the public router behind one shared token; public reachability unverified | Medium |
| S-4 | Caller-controlled `notify:false` | Medium |
| S-5 | Plaintext OTP in Redis | Medium |
| S-6 | Enumeration | Medium |
| S-7 | Erasure leaves push handles | Medium |
| S-8 | Leases shorter than worst-case batches, so duplicate sends are possible | Medium |
| S-9 | Best-effort security notices | Medium |
| S-10 | Calling endpoint survives web logout; `voip` declared without implementation | Medium |
| S-11 | One poison recipient blocks a meeting notice | Medium |
| S-12 | No bounce suppression | Low/Medium |

Consolidation **fixes S-8, S-9, S-12 and S-13 by design** in the new service (Document 4 §8). The others are repaired first, in N0.

---

## 5. Recommended architecture (summary)

```
 every producer (unchanged business logic) ──transactional outbox──▶  POST /internal/v1/notification-events
     Account (critical lane) · Meeting · Organizations · Hub · Messaging · Calling · Developer Platform · Alertmanager*

                      ┌────────────────────────────────────────────────────────────┐
                      │                 cfmeet-notification-service                 │
                      │ ingest · recipients (asks owners) · preferences (owner) ·   │
                      │ scheduler · inbox · delivery ledger · lanes:                │
                      │   CRITICAL (codes, security) · CALL (fast path) · STANDARD  │
                      │ channels: EMAIL (Resend/SMTP) · PUSH (APNs/FCM/WebPush) ·   │
                      │           IN_APP · OPERATOR* · WEBHOOK* (D-W)               │
                      └────────────────────────────────────────────────────────────┘
 Calling ──post-commit ≤1s──▶ CALL lane   (ringing itself stays on Calling's WebSocket)
 Messaging ◀──authorizeWakeups── NS        (membership/E2EE checks stay in Messaging)
 clients ──/api/v2/notifications/* (Meeting relay)──▶ inbox · preferences · push registration
```

Items marked * are optional phases.

---

## 6. Scale of the work

| Item | Count |
|---|---|
| New public (user) APIs | **9**: inbox ×5, preferences GET/PUT, push registration PUT/DELETE. Existing Hub, Messaging and Meeting endpoints stay as compatibility facades |
| New public provider callback | 1 (Resend events to the new service) |
| New internal APIs on the new service | **9**: ingest, user dispatch, call fast path, 6 operator APIs. +1 optional Alertmanager receiver |
| New internal APIs on existing services | **5**: Account `resolveAccountContacts`; Meeting `resolveConferenceRecipients`; Messaging `authorizeWakeups`; Hub `checkCommunicationRestrictions`; Organizations `resolveInvitationInvitee` |
| New tables (new service) | **16** (Document 4 §6) |
| Additive tables/migrations in existing services | 4: Account `account_notification_outbox` (new table, sealed critical-lane payloads); Meeting `outbox_events` gains channel `NOTIFICATION`; Hub outbox destination CHECK; Calling outbox destination + missed-call fact |
| Workers | **5 permanent + 1 temporary**: ingest, critical dispatcher, standard dispatcher, scheduler, retention; shadow comparator |
| Inbound event types | **31** (Document 4 §5) |
| Outbound events | 1 (`notifications.inbox.changed`) |
| Infrastructure | **2 containers** (2 replicas for the critical lane, ≤ 256 MiB each), 1 database in the existing `cfmeet-core-db`, **no Redis, no new server**. The release gate passes: 5,076 − (512 + 832) = 3,732 MiB ≥ 1,024 MiB |

---

## 7. Safest sequence (summary)

| Step | What happens |
|---|---|
| **N0** | Repair the existing paths; the owner activates Resend in today's Meeting pipeline |
| **Phase 0** | Read-only baseline queries |
| **Phase 1** | Unified service deployed dark (full schema) |
| **Phase 2** | Shadow: every producer also sends copies; the service computes and never sends |
| **Phase 3** | In-app inbox for categories with no channel today |
| **Phase 4**, in order of increasing risk | 4.1 push (no live traffic today) → 4.2 preferences (Hub becomes a facade) → 4.3 developer and meeting emails, including reminder scheduling → 4.4 invitation emails → 4.5 security notices → 4.6 **sign-in/verification/reset/step-up code emails last**, with an emergency fallback to the old Meeting path kept until retirement → 4.7 operator alerts (optional) → 4.8 customer webhooks (only if D-W) |
| **Phase 5** | History: preferences and push registrations copied; email history left to expire in the old ledger |
| **Phase 6** | Canary |
| **Phase 7** | Rollback (route new messages back to the old system; in-flight messages finish where they are) |
| **Phase 8** | Retire the old pipelines only with evidence |

---

## 8. Answers to the Part 17 questions (short form)

Full answers are in `README.md`.

1. **Standalone service?** Yes. Owner-selected: one unified service that all services use.
2. **Reusable without modification:** Calling ringing; Messaging realtime and E2EE; Organizations, Hub, Calling and Messaging outbox mechanics; `cf-service-auth`; `cf-webhooks`; `cf-migration-guard`. The email ledger, push adapters and preference model are **moved code**, ported with their tests.
3. **Migrate:** everything in the §1 "moves" table.
4. **Stay:** ringing/signaling and arbitration; code generation and verification; ciphertext and membership authority; business data. Customer webhooks stay unless D-W.
5. **Without downtime?** Yes. Per-channel fences, old paths kept as fallbacks, critical lane last.
6. **Biggest risks:**
   - sign-in email now depends on a second service (mitigated by 2 replicas, the critical lane, deployed alerting, a break-glass fallback, and moving it last);
   - wider blast radius;
   - re-certifying email and push in the new home;
   - preference single-writer switch;
   - S-1/S-2/S-3 before migration.
7. **Size:** 9 public, 9 (+1) internal on the service, 5 internal on existing services, 16 tables, 5 (+1) workers, 31 event types.
8. **Infrastructure:** about 512 MiB limit in total (2 × 256), 1 database, negligible CPU. €0 additional infrastructure.
9. **Sequence:** §7.
10. **Scope / definition of done:** Document 6.

`CFMEET_NOTIFICATION_SYSTEM_AUDIT_COMPLETE — IMPLEMENTATION_AWAITING_OWNER_APPROVAL`
