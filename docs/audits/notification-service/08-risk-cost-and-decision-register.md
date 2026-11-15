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

---

## 1. Findings register

| ID | Finding | Class | Evidence | Section |
|---|---|---|---|---|
| F-01 | Meeting email ledger, worker, Resend/SMTP providers, Svix webhooks | **MIG** | `services/meeting-service/src/services/email_delivery.rs`; release 43309857124 | N5, N11, N12 |
| F-02 | Account code and security emails | **MIG** (delivery), **RET** (code generation and verification) | `handlers/auth.rs`; `account_devices.rs:1494-1758` | N12 |
| F-03 | Meeting and organization invitation emails (token-bearing) | **MIG** (sealed token) | `handlers/v2/invitations.rs:38-104`; `handlers/v2/organizations.rs:935-1033` | N11 (4.4) |
| F-04 | Meeting notice jobs and reminder scheduling | **MIG** (recipients via **INT** X2) | `notifications.rs:660-1071` | N11 (4.3) |
| F-05 | Calling ringing, arbitration, signaling | **RET** (call control) | `calls/app/ringing.rs`; runs 41580265031, 41581639199 | — |
| F-06 | Calling push hook (`NoPushTransport`) | **BLD** → service CALL lane | `services/calling/src/main.rs:453` | N9 |
| F-07 | Messaging push registrations, jobs, dispatcher | **MIG** (entitlement via **INT** X3) | `realtime/push.rs` | N9 |
| F-08 | Messaging APNs/FCM/Web Push adapters | **MIG** (ported with tests) | `push_adapters.rs` | N4, N9 |
| F-09 | No push provider; no client registers | **EXT** + **OWN** (D-3) | certify 43670057279; evidence/06 | N9 |
| F-10 | Organization/MANAGED push impossible | **BLD** (account-device registry) | `push.rs:172` | N9 |
| F-11 | Hub notification preferences (categories, conversation and contact notification mute) | **MIG** (Hub endpoints become facades) | `preferences.rs`, `chat.rs`, `policy.rs:174-208` | N10 |
| F-12 | Hub blocks, privacy, contacts | **RET** + **INT** (X4) | `relationships.rs`, `profiles.rs` | N6 |
| F-13 | 4 of 7 categories unenforced; no quiet hours, channels or time zone | **BLD** | evidence/04 §2.2-2.3 | N3, N10 |
| F-14 | Contact vs conversation mute semantics; MENTIONS as MUTED | **DUP** (semantic) | `policy.rs:180-207` | precedence rule 9 |
| F-15 | No in-app inbox | **BLD** | evidence/06 §3 | N8 |
| F-16 | Contact, org, missed-call, voicemail, developer notices missing | **BLD** | Document 2 §B | N7, N8 |
| F-17 | Organizations `invitation.*`, `team.*` without subscriber | **INT** (new subscriber) | `outbox.rs:41-59` | N7 |
| F-18 | Hub outbox destination CHECK | **INT** (additive) | `001_foundation.sql:107-115` | N7 |
| F-19 | No per-participant missed fact; voicemail owners never told | **BLD** + **OWN** (D-10) | `history.rs:287-330`; `voicemail.rs:410-428` | N7 |
| F-20 | Developer human notices absent; dead catalogue entries | **BLD** | `control/credentials.rs`, `control/production.rs` | N7, N11 |
| F-21 | Customer webhooks | **RET** unless **OWN** D-W | Developer Platform runs 43173688215, 43337892242, 43488534526 | N16 |
| F-22 | Four notification job models, three retry policies | **DUP** → one model with lanes | evidence/01 §9.18 | N3, N5 |
| F-23 | Two device-push stores (Messaging + Hub mirror) | **DUP** → one registry | `communications/src/devices.rs:18-25` | N9, N15 |
| F-24 | Two push hooks | **DUP** → one transport | evidence/02 §9 | N4, N9 |
| F-25 | Monitoring not deployed; receivers empty | **EXT** + **OWN** (D-9); **prerequisite for N12** | `alertmanager.yml:19-66` | N13 |
| F-26 | Operator alert delivery | **BLD** (optional operator channel) | — | N13 (4.7) |
| F-27 | Telephony, SMS, Numbering, Privacy Relay disabled | **RET** (preserve) | certify integrations | — |
| F-28 | `cf-service-auth`, `cf-webhooks`, `cf-migration-guard`, contracts tooling | **RET**/reuse | `packages/*` | N1, N2 |
| F-29 | Calling `limit.*`/`usage.*` reach only the retired local transport | **INT** (Calling backlog; candidate for the operator or developer channel) | `webhooks/publication.rs:119-125` | later |
| F-30 | Three producer outbox implementations (Hub, Organizations, Account) | **RET** (each owns its transaction); unified client helper `notification-client` | evidence/04 §7.2 | N1 |
| F-31 | Dead code and stale docs (welcome email, `status_counts`, `EMAIL_QUEUE.md`, ADR 0049 §7) | hygiene | evidence/01 §9.20; evidence/02 §9 | N15 |
| F-32 | No retention on `messaging_push_jobs`, `organization_outbox`, `calling_outbox_events` | **BLD** | evidence/02 G-8; evidence/04 §4.4 | N0 (push jobs) / owners |

## 2. Security issues (SEC)

| ID | Issue | Severity | Fix | Status |
|---|---|---|---|---|
| S-1 | Missing Hub preference grant (meeting notices fail closed) | High | N0-1; disappears once preferences move (N10) | open |
| S-2 | OTP transport not recovered | High | N0-2 (owner activates Resend now); later the CRITICAL lane | open |
| S-3 | `/internal/operations/email/*` public reachability unverified | Medium | N0-6; routes retired in N15 | open |
| S-4 | Caller-controlled `notify:false` | Medium | N0-5 | open |
| S-5 | Plaintext OTP in Redis | Medium | N0-8 | open |
| S-6 | Enumeration | Medium | N0-9 (+ D-12) | open |
| S-7 | Erasure leaves push handles | Medium | N0-7; service erasure W5 | open |
| S-8 | Lease < batch, so duplicate sends are possible | Medium | N0-4; lease renewal in the service | open |
| S-9 | Security notices best-effort | Medium | N0-10; transactional Account outbox in N7 | open |
| S-10 | Calling endpoint survives logout; `voip` without implementation | Medium | N0-11 | open |
| S-11 | Poison recipient | Medium | N0-3 | open |
| S-12 | No bounce suppression | Low/Medium | N0-12; `notif_email_suppressions` | open |
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
| K-1 | **Sign-in code email depends on a second service** | M | **H** | CRITICAL lane; 2 replicas; transactional outbox (delay, never loss); alerting deployed first (D-9); break-glass fallback until Phase 8; migrated last after 60+ days of stable lanes |
| K-2 | Wider blast radius: one service for all notifications | M | H | lanes and per-lane budgets; per-category fences; old pipelines kept until Phase 8 |
| K-3 | Building on broken transports (S-1, S-2) | **H** | H | N0 first |
| K-4 | Producer outbox DEAD during a long outage | M | M | 503 + `Retry-After`; replay runbooks; Account outbox retries CRITICAL until code expiry |
| K-5 | Owner read APIs overloaded (X1–X4 per recipient) | M | M | batching (≤ 100), versioned caches |
| K-6 | E2EE metadata leakage | L | H | no content; X3 authorization; 5-key payload; S-06 tests |
| K-7 | Push activated in Messaging before N9 creates data to migrate | M | L | activate providers in the service (D-3); D-1b copy path |
| K-8 | Ring latency regression | L | H | ringing stays in Calling; fast lane post-commit ≤ 1 s; R-16 / A4.1 gates |
| K-9 | Memory on a shared VPS | L | M | 2 × 256 MiB; gate 3,732 MiB margin; re-measure at Phase 6 |
| K-10 | Email re-certification in the new home (deliverability, sender reputation) | M | M | same sender domain and provider (Resend); acceptance run before any fence; drain window with both webhook endpoints |
| K-11 | Preference move loses or overwrites user choices | L | M | versioned copy + catch-up + < 60 s freeze + 7-day write-back log; M-09 |
| K-12 | Template drift changes what users receive | M | L | template parity test F-19 |
| K-13 | Scope creep into customer webhooks | M | M | D-W with a separate addendum, not before 90 days |
| K-14 | Shadow noise before N0 | **H** | M | shadow only after N0-1 |
| K-15 | UI changes without design approval | M | L | D-11 |

## 4. Cost

| Item | Infrastructure cost |
|---|---|
| 2 replicas × 256 MiB + 1 DB in the existing Postgres | **€0** (gate: 5,076 − 1,344 = 3,732 MiB ≥ 1,024) |
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
| D-1b | If push is activated in Messaging before N9 | avoid; activate providers in the service | N9 |
| D-2 | Call push path | **resolved by D-0**: the service CALL fast lane | — |
| D-3 | Activate push providers (VAPID / FCM / APNs incl. VoIP) | Web Push first | N9 acceptance |
| D-4 | Activate Resend now (existing tooling, today's pipeline) | **now** | N0-2, N5, N11, N12 |
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
| **D-W** | Move customer webhooks into the unified service | **not before 90 days of stable user channels**; separate audit addendum. The brief warns against merging them blindly | N16 |
| **D-15** | Operator alerts through the service (Phase 4.7) | yes, with the service's own health alerts on an independent path | N13 |
