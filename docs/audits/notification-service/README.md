# CF Meet Unified Notification Service: Audit and Zero-Disruption Migration Blueprint

**Status:** `CFMEET_NOTIFICATION_SYSTEM_AUDIT_COMPLETE — IMPLEMENTATION_AWAITING_OWNER_APPROVAL`

**Revision 2 (2026-11-15).** The owner decided (D-0) that CF Meet will have **one notification system that every service uses, and that all current notification pipelines move into it.** This revision replaces revision 1's orchestration-only recommendation. Revision 1 is kept as a superseded alternative in Document 3 §3.2.

| | |
|---|---|
| Audited revision | `main` @ `6f3743f806434a55bad8297f104d28a55e350e2e` |
| Production evidence | read-only certify [43670057279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43670057279) (`CERTIFIED_READ_ONLY`); release history; `GET https://api.cfmeet.com/health` = 200; dated `docs/operations/*` |
| Changes made | this folder only (documentation) |
| Needs authorized access | Phase 0 queries Q1–Q18 (Document 5 §2) |

## Deliverables

| # | Document |
|---|---|
| 1 | [Executive Audit Report](01-executive-audit-report.md) |
| 2 | [Existing Notification Inventory](02-existing-notification-inventory.md) |
| 3 | [Current and Target Architecture](03-current-and-target-architecture.md) |
| 4 | [Backend Specification](04-notification-service-backend-specification.md) |
| 5 | [Zero-Disruption Migration Plan](05-zero-disruption-migration-plan.md) |
| 6 | [Implementation Roadmap](06-implementation-roadmap.md) |
| 7 | [Testing and Acceptance Matrix](07-testing-and-acceptance-matrix.md) |
| 8 | [Risk, Cost and Decision Register](08-risk-cost-and-decision-register.md) |
| — | [evidence/](evidence/): raw audit notes with path:line citations, plus the certify summary |

---

## What moves and what stays

**Moves into `cfmeet-notification-service`:**
- the Meeting email ledger, worker, Resend/SMTP providers and Resend webhooks;
- all emails: sign-in, verification, reset and step-up codes (critical lane), security notices, meeting and organization invitations, meeting reminders, changes and cancellations, developer notices;
- meeting reminder scheduling;
- Messaging push registrations, jobs, adapters and dispatcher;
- the Calling push hook;
- Hub notification preferences (category switches, conversation and contact notification mute);
- all missing categories (contacts, organizations, missed calls, voicemail, recordings, developer);
- the in-app inbox and delivery history;
- optionally, operator alert delivery.

**Stays.** None of these is a notification system; your brief protects each of them:

| Stays where it is | Brief reference |
|---|---|
| Calling's realtime ringing, signaling and first-answer arbitration | Part 4.3 |
| Account's generation and verification of codes and tokens | Part 4.1 |
| Messaging's ciphertext, realtime delivery and membership/device authority. The service asks Messaging to authorize each wake-up | Part 4.4 |
| Business authority data | — |
| Customer webhooks, unless owner decision D-W | Part 4.7: "must not be merged blindly" |

---

## Final verdict (Part 17)

**1. Standalone service or extend?** A standalone unified service, by owner decision D-0. The evidence supports it:
- four separate notification pipelines with three retry models;
- preferences stored in one service and enforced in two others;
- many events with no notification home;
- no delivery monitoring.

**2. Reusable without modification:**
- Calling ringing and signaling;
- Messaging realtime and E2EE;
- producer outbox mechanics (Hub, Organizations, Calling, Messaging);
- `cf-service-auth`, `cf-webhooks`, `cf-migration-guard`, contracts tooling, the coordinated release.

The email ledger, push adapters and preference logic are **ported with their tests**: moved code, not new code.

**3. Migrate:** everything under "Moves into" above.

**4. Remain:** everything under "Stays" above.

**5. Without downtime?** Yes:
- each channel moves behind a routing fence, so every message lives in exactly one system;
- old pipelines drain and stay installed as fallbacks until Phase 8;
- the riskiest move (code emails) comes last, with a break-glass switch back to the old path.

**6. Biggest risks:**
- (a) sign-in code email now depends on a second service. Mitigated by the critical lane, 2 replicas, the transactional Account outbox (delay, never loss), alerting deployed first, the break-glass fallback, and moving it last.
- (b) A wider blast radius.
- (c) Re-certifying email deliverability and push in the new home.
- (d) The preference single-writer switch.
- (e) The service now carries codes and tokens (S-19), mitigated by HPKE sealing and erasure.
- (f) The pre-existing defects S-1, S-2 and S-3, which must be repaired first.

**7. Size:**

| Item | Count |
|---|---|
| New public APIs | **9**, plus 1 provider callback. Every existing endpoint is kept as a facade |
| New internal APIs on the service | **9**, plus 1 optional |
| New internal read APIs on existing services | **5** |
| New tables | **16** |
| Additive changes in existing services | 4 |
| Workers | **5**, plus 1 temporary |
| Inbound event types | **31** |
| Outbound events | 1 |
| New shared crate | 1 (`notification-client`) |

**8. Infrastructure:**
- 2 containers × 256 MiB and 1 database in the existing `cfmeet-core-db`;
- no Redis, no queue product, no new server;
- release gate 5,076 − 1,344 = 3,732 MiB ≥ 1,024 MiB;
- €0 additional infrastructure. Provider fees apply only for owner-activated providers.

**9. Safest sequence:**

| Step | What happens |
|---|---|
| N0 | Repairs, and the owner activates Resend in today's pipeline |
| Phase 0 | Read-only baseline |
| Phase 1 | Dark service |
| Phase 2 | Shadow from every producer |
| Phase 3 | In-app inbox for categories that have no channel today |
| Phase 4 | 4.1 push (no live traffic) → 4.2 preferences → 4.3 developer and meeting emails, including reminder scheduling → 4.4 invitation emails → 4.5 security notices → 4.6 sign-in, verification, reset and step-up code emails → 4.7 operator alerts (optional) → 4.8 customer webhooks (only with D-W) |
| Phase 6 | Canary |
| Phase 8 | Evidence-based retirement of the old pipelines |

**10. Scope and definition of done:** roadmap N0–N15 (N16 only with D-W). Done means:
- every channel has been at 100 % in the service for ≥ 30 days, and the critical lane for ≥ 60 days with zero fallback use;
- old pipelines are retired per Document 5 §10;
- Document 7 gates pass in production, with real-credential checks;
- S-1 … S-13 and S-19 are closed;
- no business action has a synchronous dependency on the service.

---

## Recommended first approvals

| Decision | Why it comes first |
|---|---|
| D-13: approve the blueprint | — |
| D-4: activate Resend now | restores sign-in emails today, independent of the new service |
| D-9: deploy alerting with real receivers | required before the critical lane moves |
| N0, N1 and N4 | can start in parallel |
| D-3: activate push providers **in the new service**, not in Messaging | avoids creating push data that would later need migrating |

**Do not implement anything until the owner approves.**
