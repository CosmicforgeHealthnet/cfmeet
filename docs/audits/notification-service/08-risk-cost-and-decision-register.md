# Document 8 — Risk, Cost and Decision Register

Each finding is classified with one of the categories required by Part 16.

| Code | Category |
|---|---|
| **RET** | Existing functionality to retain |
| **MIG** | Functionality to migrate |
| **INT** | Integrate without moving |
| **BLD** | Missing capability to build |
| **DUP** | Duplication to eliminate |
| **SEC** | Security issue to repair |
| **EXT** | External dependency |
| **OWN** | Owner decision required |

Every row cites its evidence. Paths are repository paths. Runs are GitHub Actions run IDs on `CosmicforgeHealthnet/cfmeet`.

---

## 1. Findings register

| ID | Finding | Class | Evidence | Action / section |
|---|---|---|---|---|
| F-01 | Meeting `email_deliveries` ledger with `UNIQUE (idempotency_key)`, worker, Resend/SMTP, Svix webhooks | **RET** + **INT** | `services/meeting-service/src/services/email_delivery.rs:124-185, 599-875`; release 43309857124 | single email transport; NS enqueues via X1 (N8) |
| F-02 | Account OTP / reset / step-up / password-changed / new-device emails | **RET** | `handlers/auth.rs`; `services/account_devices.rs:1494-1758` | never moves (P4) |
| F-03 | Token-bearing meeting and organization invitation emails sent synchronously by the Meeting relay | **RET** | `handlers/v2/invitations.rs:38-104`; `handlers/v2/organizations.rs:935-1033` | token minter keeps sending; NS adds a token-free in-app copy |
| F-04 | Meeting durable notices (reschedule, cancel, reminder): recipient resolution, preference check, rendering | **MIG** (orchestration) / **RET** (job writer, reminder scan, recipients) | `notifications.rs:660-1071` | N8, Document 5 §6 (4.2) |
| F-05 | Calling ringing, arbitration, answered-elsewhere, signaling WS, recovery | **RET** | `calls/app/ringing.rs:139-838`; runs 41580265031, 41581639199 | untouched |
| F-06 | Calling push hook with `NoPushTransport` | **BLD** | `services/calling/src/main.rs:453`; `signaling/push.rs:19-33` | N10 (D-2) |
| F-07 | Messaging push pipeline and metadata-only payload enforcement | **RET** (decision + whitelist) + **INT** | `realtime/push.rs`, `m20261106_000004_realtime.rs:139-140` | Messaging keeps deciding; dispatch via NS (N9) |
| F-08 | Messaging APNs/FCM/Web Push adapters | **MIG** (to the shared crate, not deleted) | `push_adapters.rs:132-683` | N4 |
| F-09 | No push provider configured; no client registers | **EXT** + **OWN** | certify 43670057279 `MESSAGING_PUSH_PROVIDER=none`; evidence/06 §1 | D-3 |
| F-10 | Organization/MANAGED scope cannot register push (session devices) | **BLD** | `push.rs:172`; `m20261202_000030_session_devices_inbox.rs:4-7` | account-device registry (N9, D-1) |
| F-11 | Hub preferences: 7 categories, conversation mute, contact mute | **RET** (single writer) + extend | `services/communications/src/preferences.rs`, `chat.rs:354-419` | N5 |
| F-12 | 4 of 7 Hub categories stored but never read | **BLD** (enforcement) | evidence/04 §2.3 | NS precedence (Document 4 §7.2) |
| F-13 | No quiet hours, time zone, channel matrix or org policy | **BLD** | no DDL or grep hit (evidence/04 §2.2.1) | N5 (+ D-7) |
| F-14 | Contact mute vs conversation mute, with different semantics; `MENTIONS` treated as `MUTED` | **DUP** (semantic) | `001_foundation.sql:72-79`; `002_unified.sql:36-48`; `policy.rs:180-207` | precedence rule 9 defines one behaviour; storage kept |
| F-15 | No in-app inbox anywhere | **BLD** | evidence/06 §3 | N7 |
| F-16 | Contact requests, org changes, missed calls, voicemail deposit produce no notification | **BLD** | evidence/02 B.3–B.6 | N6 + N7 |
| F-17 | Organizations outbox: `invitation.*` / `team.*` have no subscriber despite the contract | **INT** | `services/organizations/src/outbox.rs:41-59`; `docs/events/organization-event-contracts.md` §2 | add the NS subscriber (N6) |
| F-18 | Hub outbox destination CHECK only allows CALLING/MESSAGING | **INT** | `001_foundation.sql:107-115` | additive CHECK (N5) |
| F-19 | Calling: no per-participant missed fact; voicemail owners never told | **BLD** + **OWN** | `history.rs:287-330`; `voicemail.rs:410-428` | additive `CALL_PARTICIPANT_MISSED` (D-10) |
| F-20 | Developer Platform human notices absent; catalogue entries never emitted | **BLD** | `control/credentials.rs:218-241`; `control/production.rs:304-341` | N11. Fix dead catalogue entries separately (Developer Platform backlog) |
| F-21 | Customer webhooks (Developer Platform) | **RET** | runs 43173688215, 43337892242, 43488534526 | out of NS scope |
| F-22 | Three notification job models + five outboxes with different retry policies | **DUP** | evidence/01 §9.18, evidence/04 §7.2 | job models: NS for new categories; outboxes stay per service (correct) |
| F-23 | Two device-push stores (Messaging + Hub mirror) | **DUP** | `services/communications/src/devices.rs:18-25` | one registry (N9); mirror retired (N14) |
| F-24 | Two push hooks (Messaging, Calling) | **DUP** | evidence/02 §9 Duplication | one transport crate (N4) |
| F-25 | Alert rules exist; Prometheus/Alertmanager not deployed; receivers empty | **EXT** + **OWN** | `infra/monitoring/alertmanager/alertmanager.yml:19-66`; `docs/operations/monitoring-alerts.md:3-5` | D-9 (stays in monitoring, not NS) |
| F-26 | Calling, Telephony and Messaging incident tables, CLI only | **RET** (monitoring domain) | `calls/app/incidents.rs` | no NS action |
| F-27 | Telephony, SMS, Numbering, Privacy Relay disabled | **RET** (preserve disabled) | certify 43670057279 integrations | no dependency introduced |
| F-28 | `cf-service-auth`, `cf-webhooks` (`retry`, `secret_box`), `cf-migration-guard`, contracts tooling | **RET** / reuse | `packages/*` | N1, N2, N9 |
| F-29 | `limit.*` / `usage.*` Calling notices only reach the retired local transport | **INT** (Calling backlog) | `webhooks/publication.rs:119-125` | outside NS; flagged for the Calling owner |
| F-30 | SMS Gateway / Privacy Relay default publish destination is the retired Calling ingest | **RET** (disabled) | evidence/03 §17.9 | note only |
| F-31 | Welcome email dead code; `notifications::status_counts` unused; stale docs (`EMAIL_QUEUE.md`, ADR 0049 §7, `outbox.rs:17-20`) | **DUP** / hygiene | evidence/01 §9.19-20; evidence/02 §9 | docs cleanup (no runtime effect) |
| F-32 | `messaging_push_jobs`, `organization_outbox`, `calling_outbox_events` have no retention | **BLD** (retention) | evidence/02 G-8; evidence/04 §4.4; evidence/03 §17.7 | N0 (push jobs); others to the owners' backlogs |

## 2. Security issues to repair (SEC)

| ID | Issue | Severity | Evidence | Fix (N0 item) | Status |
|---|---|---|---|---|---|
| S-1 | Meeting → Hub `communications-preferences.read` grant missing in release tooling; meeting notices to account holders fail closed | High (availability) | `infra/calling/production/communications_release.py:168,178`; `notifications.rs:994-996`; `docs/operations/profile-notifications-sections-9-11.md:221-227` | N0-1 (verify Q3/Q14 first) | open; static evidence |
| S-2 | OTP/account email transport not recovered (Gmail SMTP rejected; Resend not activated) | High | `CFMEET_FINAL_PRODUCTION_COMPLETION.md:121`; certify 43670057279 (`EMAIL_PROVIDER=smtp`, no `activate-resend-email` in the journal) | N0-2 (owner) | open |
| S-3 | Internal email operations on the public router behind a shared static token; public reachability unverified | Medium | `routes/mod.rs:96-107`; `handlers/internal.rs:85-98` | N0-6 (verify Q15) | open; unverified |
| S-4 | Caller-controlled `notify:false` suppresses others' pushes | Medium | `messages/mod.rs:270-272`; `e2ee.rs:236` | N0-5 | open (dormant while push is off) |
| S-5 | Plaintext OTPs in Redis, email in the key, `==` comparison | Medium | `services/otp.rs:60-114` | N0-8 | open |
| S-6 | Account enumeration through `verify-reset-token` and `register` 409 | Medium | `handlers/auth.rs:913-946, 91-98` | N0-9 (+ D-12 for `register`) | open |
| S-7 | Erasure leaves push handles | Medium (privacy) | `compliance/deletion.rs:258-265` | N0-7 | open |
| S-8 | Lease shorter than worst-case batch → duplicate sends (email, push) | Medium | `email_delivery.rs:76`; `push.rs:401-421,529` | N0-4 | open |
| S-9 | Security notices best-effort after commit | Medium | `handlers/auth.rs:876-882`; `account_devices.rs:769-777` | N0-10 | open |
| S-10 | Web logout leaves the Calling endpoint active; `voip` background mode declared without implementation | Medium | `apps/web/src/services/auth.service.ts:97-142`; `apps/mobile/app.json:26-28` | N0-11 | open |
| S-11 | Poison recipient blocks later recipients of a meeting notice | Medium | `notifications.rs:1033-1067` | N0-3 | open |
| S-12 | No bounce/complaint suppression | Low/Medium | `email_delivery.rs:998-1086` | N0-12 | open |
| S-13 | Push handles stored in plaintext; one handle can serve two accounts | Medium (privacy) | `m20261106_000004_realtime.rs:94-113` | NS registry seals and adds `UNIQUE handle_digest` (N9) | design |
| S-14 | APNs/FCM see `conversationId` and sequences in clear | Low (residual) | `push_adapters.rs:203-206,413-417` | documented residual; prefer Web Push; consider an opaque per-device conversation alias later | accepted residual (owner) |
| S-15 | Organization invitation raw token returned to the inviter's client and re-issued on idempotent replay | Low/Medium | `services/organizations/src/http/invitations.rs:170-190,254` | Organizations backlog (not NS) | open |
| S-16 | Signaling `cfsig_` token in the WS subprotocol header; conference ticket and reset code in the URL query | Low/Medium | evidence/06 §9.3 | web/Calling backlog | open |
| S-17 | Hub preference resolve builds SQL with `format!` on a column name (safe today: closed match) | Low | `preferences.rs:64-75` | N5 replaces it with a static match | design |
| S-18 | Request binding (SEC-012) still at step 1 (`accept`; no signer emits) | Low | `docs/operations/service-token-request-binding.md` | NS emits binding from day one | design |

## 3. Risk register

L = likelihood, I = impact (H/M/L).

| ID | Risk | L | I | Mitigation | Owner of mitigation |
|---|---|---|---|---|---|
| K-1 | Duplicate meeting emails during handover | L | M | same deterministic key into one ledger `UNIQUE`; M-03, M-05 tests; dual-enqueue window proves agreement | N8 |
| K-2 | Lost notices during handover (crash between recipient digest and enqueue; fence gaps) | M | M | fence by `created_at`; legacy finishes pre-fence jobs; shadow "missing" alert; N0-3 | N8, N12 |
| K-3 | Building on broken transports (S-1, S-2) makes the baselines meaningless | **H** | H | N0 first; Phase 4 gated on the fixes | owner + N0 |
| K-4 | Producer outbox DEAD-letters during a long NS outage | M | M | 503 + `Retry-After`; replay runbooks for Organizations, Hub and Calling; NS dedupe makes replay safe | N12 |
| K-5 | Per-recipient policy calls overload Account / Platform Access | M | M | X3 batch + 60 s versioned cache | N5 |
| K-6 | E2EE metadata leakage through new categories | L | H | NS never handles message content; Messaging keeps the decision and whitelist; S-06 test | N9 |
| K-7 | Push activated in Messaging before D-1, creating handles to migrate | M | L | decide D-1 now; D-1b copy path specified | owner |
| K-8 | Calling ring latency regression from the push integration | L | H | post-commit, ≤ 1 s, configuration-gated; R-16, A4.5 gates | N10 |
| K-9 | Memory pressure on a shared VPS (other products use about 4.5 GiB) | L | M | 256 MiB limit; release resource gate; re-measure at Phase 6 | N2, N13 |
| K-10 | Scope creep into customer webhooks or infrastructure alerting | M | M | explicit out-of-scope (Document 3 §6, Document 4 header) | reviewers |
| K-11 | UI changes without design approval (Part 2 rule 9) | M | L | FE2/FE3 gated by D-11 | owner |
| K-12 | Unverified S-3 exposure exploited before the fix | L | H | Q15 immediately; N0-6 | owner |
| K-13 | Wrong-recipient in-app items (authorization bug) | L | H | authorization-matrix suite; 404-for-foreign; canary accounts first | N7 |
| K-14 | Shadow comparison noise (legacy failing for S-1) hides real mismatches | **H** before N0 | M | run shadow only after N0-1 | N6 |

## 4. Cost estimate

| Item | Infrastructure cost | Basis |
|---|---|---|
| NS container (256 MiB limit) + DB in the existing Postgres | **€0** (existing VPS headroom) | MemAvailable 5,076 MiB; gate 5,076 − (256 + 832) = 3,988 ≥ 1,024 (certify 43670057279; `coordinated_release.py:166-190`) |
| Queue | **€0** (Postgres) | same pattern as every service |
| Storage | negligible (< 1 GiB at 10k users × 20 notifications/month × 90 d) | Document 3 §7.2 |
| Email provider | as already planned for OTP recovery (Resend, D-4); NS adds only low-volume notices | `docs/operations/resend-email-owner-handoff.md` |
| Push providers | Web Push and FCM: no vendor fee. APNs needs an Apple Developer Program membership (D-3) | provider terms; the owner must confirm current pricing; no purchase authorized |
| Second server / managed queue | **not justified** by any measurement | — |

**Engineering effort**, in relative size only. The audit does not estimate calendar time without a team baseline.

| Size | Sections |
|---|---|
| Small | N0-x items each; N1; N4; N12 |
| Medium | N2; N5; N7; N11 |
| Large | N3; N6 (6 producers); N8; N9 |
| Medium plus a mobile project | N10 |

---

## 5. Owner decisions required (OWN)

| ID | Decision | Options | Recommendation | Blocks |
|---|---|---|---|---|
| **D-1** | Who owns the device push-target registry? | (a) NS at account-device level, via the existing Hub facade. (b) Keep Messaging's E2EE-device registry and add a second one for other categories. (c) Messaging serves as a push service for all | **(a)**. It fixes org/MANAGED push and Calling `pushTargetRef`, gives one sealed store, and costs ~0 to migrate today (Q6 expected 0) | N9, N10 |
| D-1b | If Web Push is activated in Messaging before NS exists | allow, with a later sealed copy (Document 5 §6, 4.3) / wait | allow; do not delay user value | — |
| **D-2** | Call push path | (a) Calling → NS fast path (≤ 1 s, best-effort). (b) Calling-local dispatcher using `packages/push-transport` + a replicated endpoint read model | (a) for simplicity; (b) if zero runtime coupling for calls is preferred | N10 |
| **D-3** | Activate push providers (VAPID / FCM / APNs incl. VoIP) | activate per platform / defer | activate Web Push first (encrypted payloads, no vendor fee) | N9 real-device acceptance |
| **D-4** | Activate Resend (existing tooling) | — | **now** (restores OTP; independent of NS) | N0-2, N8 |
| **D-5** | Organizations read: invitation → invitee account ref (for the in-app copy) | build / skip the in-app invitation copy | build (read-only, Organizations-owned) | Phase 3 ORG_INVITATION |
| D-5b | Organizations "accept by invitation id for the matching verified email" (in-app acceptance) | Organizations business-rule change | defer (outside NS) | — |
| D-6 | Realtime nudge for the inbox | polling (30 s, ETag) / Messaging realtime frame / NS WebSocket (gateway WS disabled today) | polling first; revisit with measured usage | — |
| D-7 | Organization-level notification policy | none / Organizations-owned policy | reserve the slot (precedence rule 5); build later | — |
| D-8 | Inbox retention and "category off" semantics | 90 d default; silent recording vs no recording | 90 d; record silently (auditable, not counted) | N7 |
| D-9 | Deploy Prometheus/Alertmanager and set receivers (paging) | — | yes (independent of NS; covers existing dead-letter rules) | operator alerting |
| D-10 | Approve the additive Calling event `CALL_PARTICIPANT_MISSED` (ADR 0025 rule, computed in Calling) | approve / derive in NS from history reads | approve (keeps Calling authoritative; no business-rule change) | missed-call notices |
| D-11 | UI design approval: bell, inbox and preferences screens (web/mobile) | — | required before FE2/FE3 (Part 2 rule 9) | FE2, FE3 |
| D-12 | `register` 409 for existing emails (enumeration vs UX) | uniform answer + email notice / keep 409 | uniform answer; email the existing account holder instead | N0-9 |
| D-13 | Approve this blueprint and the order N0 → N1 … → N14 | — | approve N0 + N1 + N4 first | everything |
