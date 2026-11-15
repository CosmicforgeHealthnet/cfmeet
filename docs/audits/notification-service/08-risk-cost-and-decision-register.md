# Document 8 — Risk, Cost and Decision Register (revision 2: unified service)

## Classification codes

| Code | Meaning |
|---|---|
| **RET** | Retain where it is |
| **MIG** | Migrate into the unified service |
| **INT** | Integrate without moving (the service asks the owner) |
| **BLD** | Missing capability to build |
| **DUP** | Duplication to eliminate |
| **SEC** | Security issue to repair |
| **EXT** | External dependency |
| **OWN** | Owner decision required |

## Owner decision recorded

| ID | Decision | Date | Effect |
|---|---|---|---|
| **D-0** | **One notification system used by every service; current pipelines move into it** | 2026-11-15 | Option B adopted. Revision 1's orchestration-only design is superseded (Document 3 §3) |
| **D-13** | Blueprint approved (full consolidation); start N0, N1, N4 | 2026-11-15 | engineering started; report in Document 9 |
| **D-5, D-10, D-15, D-16** | approved (D-16: AUTO fallback enabled only after real recovery tests prove the old transport works) | 2026-11-15 | X5, event #15, I12 / event #31 in the N1 contracts; D-16 applies in N12 |
| **D-W** | excluded: customer webhooks stay with the Developer Platform | 2026-11-15 | N16 not planned |
| **D-12** | security-safe registration responses without exposing account existence | 2026-11-15 | implemented in N0-9 (Document 9) |

---

## 1. Findings register

*Finding IDs use `FN-nn` so they do not collide with the functional test IDs `F-nn` in Document 7.*

| ID | Finding | Class | Evidence | Section |
|---|---|---|---|---|
| FN-01 | Meeting email ledger, worker, Resend/SMTP providers, Svix webhooks | **MIG** | `services/meeting-service/src/services/email_delivery.rs`; release 43309857124 | N5, N11, N12 |
| FN-02 | Account code and security emails | **MIG** (delivery), **RET** (code generation and verification) | `handlers/auth.rs`; `account_devices.rs:1494-1758` | N12 |
| FN-03 | Meeting and organization invitation emails (token-bearing) | **MIG** (sealed token) | `handlers/v2/invitations.rs:38-104`; `handlers/v2/organizations.rs:935-1033` | N11 (4.4) |
| FN-04 | Meeting notice jobs and reminder scheduling | **MIG** (recipients via **INT** X2) | `notifications.rs:660-1071` | N11 (4.3) |
| FN-05 | Calling ringing, arbitration, signaling | **RET** (call control) | `calls/app/ringing.rs`; runs 41580265031, 41581639199 | — |
| FN-06 | Calling push hook (`NoPushTransport`) | **BLD** → service CALL lane | `services/calling/src/main.rs:453` | N9 |
| FN-07 | Messaging push registrations, jobs, dispatcher | **MIG** (entitlement via **INT** X3) | `realtime/push.rs` | N9 |
| FN-08 | Messaging APNs/FCM/Web Push adapters | **MIG** (ported with tests) | `push_adapters.rs` | N4, N9 |
| FN-09 | No push provider; no client registers | **EXT** + **OWN** (D-3) | certify 43670057279; evidence/06 | N9 |
| FN-10 | Organization/MANAGED push impossible | **BLD** (account-device registry) | `push.rs:172` | N9 |
| FN-11 | Hub notification preferences (categories, conversation and contact notification mute) | **MIG** (Hub endpoints become facades) | `preferences.rs`, `chat.rs`, `policy.rs:174-208` | N10 |
| FN-12 | Hub blocks, privacy, contacts | **RET** + **INT** (X4) | `relationships.rs`, `profiles.rs` | N6 |
| FN-13 | 4 of 7 categories unenforced; no quiet hours, channels or time zone | **BLD** | evidence/04 §2.2-2.3 | N3, N10 |
| FN-14 | Contact vs conversation mute semantics; MENTIONS as MUTED | **DUP** (semantic) | `policy.rs:180-207` | precedence rule 9 |
| FN-15 | No in-app inbox | **BLD** | evidence/06 §3 | N8 |
| FN-16 | Contact, org, missed-call, voicemail, developer notices missing | **BLD** | Document 2 §B | N7, N8 |
| FN-17 | Organizations `invitation.*`, `team.*` without subscriber | **INT** (new subscriber) | `outbox.rs:41-59` | N7 |
| FN-18 | Hub outbox destination CHECK | **INT** (additive) | `001_foundation.sql:107-115` | N7 |
| FN-19 | No per-participant missed fact; voicemail owners never told | **BLD** + **OWN** (D-10) | `history.rs:287-330`; `voicemail.rs:410-428` | N7 |
| FN-20 | Developer human notices absent; dead catalogue entries | **BLD** | `control/credentials.rs`, `control/production.rs` | N7, N11 |
| FN-21 | Customer webhooks | **RET** unless **OWN** D-W | Developer Platform runs 43173688215, 43337892242, 43488534526 | N16 |
| FN-22 | Four notification job models, three retry policies | **DUP** → one model with lanes | evidence/01 §9.18 | N3, N5 |
| FN-23 | Two device-push stores (Messaging + Hub mirror) | **DUP** → one registry | `communications/src/devices.rs:18-25` | N9, N15 |
| FN-24 | Two push hooks | **DUP** → one transport | evidence/02 §9 | N4, N9 |
| FN-25 | Monitoring not deployed; receivers empty | **EXT** + **OWN** (D-9); **prerequisite for N12** | `alertmanager.yml:19-66` | N13 |
| FN-26 | Operator alert delivery | **BLD** (optional operator channel) | — | N13 (4.7) |
| FN-27 | Telephony, SMS, Numbering, Privacy Relay disabled | **RET** (preserve) | certify integrations | — |
| FN-28 | `cf-service-auth`, `cf-webhooks`, `cf-migration-guard`, contracts tooling | **RET**/reuse | `packages/*` | N1, N2 |
| FN-29 | Calling `limit.*`/`usage.*` reach only the retired local transport | **INT** (Calling backlog; candidate for the operator or developer channel) | `webhooks/publication.rs:119-125` | later |
| FN-30 | Three producer outbox implementations (Hub, Organizations, Account) | **RET** (each owns its transaction); unified client helper `notification-client` | evidence/04 §7.2 | N1 |
| FN-31 | Dead code and stale docs (welcome email, `status_counts`, `EMAIL_QUEUE.md`, ADR 0049 §7) | hygiene | evidence/01 §9.20; evidence/02 §9 | N15 |
| FN-32 | No retention on `messaging_push_jobs`, `organization_outbox`, `calling_outbox_events` | **BLD** | evidence/02 G-8; evidence/04 §4.4 | N0 (push jobs) / owners |
| FN-33 | Old and new ledgers sit in different databases; no cross-database transaction exists | **BLD**: routing fence protocol v1 (producer-side sticky decisions, guard triggers, acknowledgement boundary, leases, audit) | Document 4 §9 | N7, N12 |
| FN-34 | The authentication-email fallback must survive both replicas or the service DB being down | **RET** (old Meeting path kept running with its own credentials) + **BLD** (AUTO/FORCED modes, drill) | Document 4 §10.4; certify 43670057279 (Meeting DB in a separate container) | N12 |
| FN-35 | Email-provider activation was coupled to service development in revision 2 | **corrected:** E-track separated; adapter architecture preserved (Document 4 §14) | — | E1–E4 |

## 2. Security issues (SEC)

| ID | Issue | Severity | Fix | Status |
|---|---|---|---|---|
| S-1 | Missing Hub preference grant (meeting notices fail closed) | High | N0-1; disappears once preferences move (N10) | **fixed in code, pending release** (ffff88d; Document 9) |
| S-2 | OTP transport not recovered | High | E-track E1 (owner makes the existing Meeting adapters deliver, D-4); later the CRITICAL lane | open |
| S-3 | `/internal/operations/email/*` public reachability unverified | Medium | N0-6; routes retired in N15 | **fixed in code, pending release** (1bbfefd; Document 9) |
| S-4 | Caller-controlled `notify:false` | Medium | N0-5 | **fixed in code, pending release** (Messaging (Doc 9 §5); Document 9) |
| S-5 | Plaintext OTP in Redis | Medium | N0-8 | **fixed in code, pending release** (ba0af77; Document 9) |
| S-6 | Enumeration | Medium | N0-9 (+ D-12) | **fixed in code, pending release** (94d759f; Document 9) |
| S-7 | Erasure leaves push handles | Medium | N0-7; service erasure W5 | **fixed in code, pending release** (Messaging (Doc 9 §5); Document 9) |
| S-8 | Lease < batch, so duplicate sends are possible | Medium | N0-4; lease renewal in the service | **fixed in code, pending release** (05bfff0 + Messaging; Document 9) |
| S-9 | Security notices best-effort | Medium | N0-10; transactional Account outbox in N7 | **fixed in code, pending release** (c684d8c; Document 9) |
| S-10 | Calling endpoint survives logout; `voip` without implementation | Medium | N0-11 | **fixed in code, pending release** (567e7f2, 9696dd6; Document 9) |
| S-11 | Poison recipient | Medium | N0-3 | **fixed in code, pending release** (4246235; Document 9) |
| S-12 | No bounce suppression | Low/Medium | N0-12; `notif_email_suppressions` | **fixed in code, pending release** (4246235; Document 9) |
| S-13 | Push handles plaintext and shareable across accounts | Medium | sealed + `UNIQUE handle_digest` (N9) | design |
| S-14 | APNs/FCM see conversation IDs | Low (residual) | documented; prefer Web Push | accepted residual (owner) |
| S-15 | Org invitation token returned to the inviter | Low/Medium | Organizations backlog | open |
| S-16 | Signaling token in WS subprotocol header; ticket and reset code in URL | Low/Medium | web/Calling backlog | open |
| S-17 | `format!` column name in Hub resolve | Low | removed when preferences move | design |
| S-18 | Request binding at step 1 | Low | the service emits binding from day one | design |
| **S-19** | **New in revision 2:** the service now carries codes, invitation tokens and addresses | Medium (design) | HPKE sealing from producer to renderer; sealed at rest; erased at the final state; never logged; Account remains the only verifier; I6 refuses to replay expired codes | design (Document 4 §8) |

## 3. Risk register

| ID | Risk | L | I | Mitigation |
|---|---|---|---|---|
| K-1 | **Sign-in code email depends on a second service** | M | **H** | CRITICAL lane; 2 replicas; transactional Account outbox; X6/X7 lease protocol; **AUTO failover to the retained old path after 60 s and FORCED mode without restart** (verified for both replicas down and service DB down, Document 4 §10.4); fallback drill; alerting first (D-9); gates E3/E4; moved last |
| K-2 | Wider blast radius: one service for all notifications | M | H | lanes and per-lane budgets; per-category fences; old pipelines kept until Phase 8 |
| K-3 | Building on broken transports (S-1, S-2) | **H** | H | N0 first |
| K-4 | Producer outbox DEAD during a long outage | M | M | 503 + `Retry-After`; replay runbooks; Account outbox retries CRITICAL until code expiry |
| K-5 | Owner read APIs overloaded (X1–X4 per recipient) | M | M | batching (≤ 100), versioned caches |
| K-6 | E2EE metadata leakage | L | H | no content; X3 authorization; 5-key payload; tests ST-06, ST-17 |
| K-7 | Push activated in Messaging before N9 creates data to migrate | **H** (attempt observed: run 43702218850 on 2026-11-15 failed before change; fix `de7b863` on `main`) | L | supported copy path (Document 5 §6, 4.1) with Messaging route decisions; or activate providers in the service instead (D-1b) |
| K-8 | Ring latency regression | L | H | ringing stays in Calling; fast lane post-commit ≤ 1 s; R-16 / A4.1 gates |
| K-9 | Memory on a shared VPS | L | M | 2 × 256 MiB; gate 3,732 MiB margin; re-measure at Phase 6 |
| K-10 | Email re-certification in the new home (deliverability, sender reputation) | M | M | same sender domain and provider (Resend); acceptance run before any fence; drain window with both webhook endpoints |
| K-11 | Preference move loses or overwrites user choices | L | M | versioned copy + catch-up + < 60 s freeze + 7-day write-back log; M-09 |
| K-12 | Template drift changes what users receive | M | L | template parity test F-19 |
| K-13 | Scope creep into customer webhooks | M | M | D-W with a separate addendum, not before 90 days |
| K-14 | Shadow noise before N0 | **H** | M | shadow only after N0-1 |
| K-15 | UI changes without design approval | M | L | D-11 |
| K-16 | Mixed-version deployment writes to the old path after a category moved | L | H | activation preconditions (`routingProtocol` on every replica), guard triggers on old-path tables, release rollback floor (Document 4 §9.4, §9.6); tests M-11 … M-13 |
| K-17 | Crash or rollback creates a message in both ledgers | L | H | sticky decisions in the producer transaction, acknowledgement boundary, two-phase and lease-fenced reroute, hourly audit with auto-freeze (Document 4 §9); tests M-14 … M-19 |
| K-18 | Baselines taken while the transport is broken | **H** | M | email baselines start after gate E1; all Q1–Q18 OUTSTANDING until measured |
| K-19 | Fallback silently broken (legacy worker or its credentials removed during migration) | M | H | fallback readiness invariants certified weekly and before every CRITICAL change; drill (R-23, Document 4 §10.4) |

## 3a. Outstanding production baseline measurements

| ID | Measures | Status (2026-11-15) | Blocks |
|---|---|---|---|
| Q1, Q2, Q17, Q18 | email ledger volume, error classes, p95 latency, drain size | **OUTSTANDING.** Take after E1, because today's SMTP path is reported broken | G-18, G-19, G-20, G-22, G-23; drain planning |
| Q3, Q4 | meeting notice outcomes | **OUTSTANDING** | confirming S-1 before N0-1 |
| Q5 | Account → Hub outbox backlog | **OUTSTANDING** | — |
| Q6, Q7 | push registrations and jobs | **OUTSTANDING** (hypothesis 0; Web Push activation in Messaging attempted) | 4.1 copy-path decision; G-10 |
| Q8, Q9 | preference and mute rows | **OUTSTANDING** | 4.2 sizing; G-15 |
| Q10–Q13 | outbox and webhook health | **OUTSTANDING** | G-04 |
| Q14 | live Hub trust bundle grant | **OUTSTANDING** | S-1 root cause |
| Q15 | public reachability of `/internal/operations/*` | **OUTSTANDING** | S-3 |
| Q16 | host capacity (7-day minimum) | **OUTSTANDING** (the 5,076 MiB from certify 43670057279 is a single sample) | G-01, Phase 6 |

## 4. Cost

| Item | Infrastructure cost |
|---|---|
| 2 replicas × 256 MiB + 1 DB in the existing Postgres | **€0** (gate on the single sample of 2026-11-15: 5,076 − 1,344 = 3,732 MiB ≥ 1,024; Q16 OUTSTANDING) |
| Queue | **€0** (Postgres) |
| Storage | < 2 GiB (Document 3 §7.2) |
| Email provider | Resend: already required for OTP recovery (D-4) |
| Push providers | Web Push and FCM: no vendor fee. APNs: Apple Developer Program (D-3). The owner confirms current pricing |
| Second server / managed queue | not justified |

**Relative engineering size:**

| Size | Sections |
|---|---|
| Small | N0 items, N1, N4, N13, N14 |
| Medium | N2, N6, N8, N10 |
| Large | N3, N5, N7, N9, N11, N12 |
| Separate audit | N16 |

Revision 2 is roughly 1.5–2× the engineering of revision 1, mainly from N5, N10 and N12.

## 5. Owner decisions

| ID | Decision | Recommendation | Blocks |
|---|---|---|---|
| D-0 | Unified service | **decided 2026-11-15** | — |
| D-1 | Push registry owner | **resolved by D-0**: the service, at account-device level | — |
| D-1b | If push is activated in Messaging before N9 | **either is supported.** The owner has begun `activate-web-push` in Messaging (run 43702218850 failed before change; fix on `main`). If it proceeds, N9 uses the copy path (Document 5 §6, 4.1). Activating in the service instead avoids the copy | N9 |
| D-2 | Call push path | **resolved by D-0**: the service CALL fast lane | — |
| D-3 | Activate push providers (VAPID / FCM / APNs incl. VoIP) | Web Push first | N9 acceptance |
| D-4 | **E-track:** make the existing Meeting adapters deliver (Resend via `activate-resend-email`, or a working SMTP account). Independent of service development | **now** (restores sign-in email today) | gate E1 → E2–E4 → moving email categories (N11, N12). **Does not block N1–N10** |
| D-5 | Organizations `resolveInvitationInvitee` (X5) | build | in-app invitation copy |
| D-6 | Inbox realtime nudge | polling first | — |
| D-7 | Organization notification policy | reserve slot | — |
| D-8 | Inbox retention / silent semantics | 90 d; record silently | N8 |
| D-9 | Deploy Prometheus/Alertmanager with real receivers | **yes; prerequisite for N12** | N12 |
| D-10 | Calling `CALL_PARTICIPANT_MISSED` fact | approve | missed-call notices |
| D-11 | UI design approval (inbox, preferences) | required | FE2, FE3 |
| D-12 | `register` 409 behaviour | uniform answer + email notice | N0-9 |
| D-13 | Approve the blueprint and the order | approve N0 + N1 + N4 first | all |
| **D-14** | Secondary email provider for the CRITICAL lane (keep the SMTP adapter as failover when Resend is down) | yes, if a working SMTP account exists; otherwise single provider + alerting | N12 |
| **D-W** | Move customer webhooks into the unified service | **not before 90 days of stable user channels**; separate audit addendum. The brief warns against merging them blindly. **Owner to confirm** whether "all notification delivery channels consolidate" includes customer webhooks; the blueprint keeps them out of scope until then | N16 |
| **D-15** | Operator alerts through the service (Phase 4.7) | yes, with the service's own health alerts on an independent path. **Owner to confirm** whether operator alerting counts as a "notification delivery channel" under the consolidation mandate (currently optional) | N13 |
| **D-16** | Enable AUTO failover for never-leased CRITICAL messages (default) in addition to manual FORCED | **yes** (zero duplicate risk by the lease protocol; removes the human from the first minutes of an outage) | N12 |
| **D-17** | Acceptance mailbox for SMTP delivery evidence (needed only if the owner uses SMTP rather than Resend with webhooks) | provide one owner-controlled mailbox | E1–E4 with SMTP |
