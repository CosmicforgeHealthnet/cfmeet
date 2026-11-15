# CF Meet Notification Service: Audit and Zero-Disruption Migration Blueprint

**Status:** `CFMEET_NOTIFICATION_SYSTEM_AUDIT_COMPLETE — IMPLEMENTATION_AWAITING_OWNER_APPROVAL`

| | |
|---|---|
| Audited revision | `main` @ `6f3743f806434a55bad8297f104d28a55e350e2e` (2026-11-15) |
| Production evidence | read-only certify [43670057279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43670057279) (`CERTIFIED_READ_ONLY`); release/certify history; `GET https://api.cfmeet.com/health` = 200; dated `docs/operations/*` reports |
| Changes made | **this folder only** (documentation). No code, configuration, migration, secret, deployment or production write |
| Not done (needs authorized access) | database counts and live trust-bundle/nginx inspection, listed as Phase 0 queries Q1–Q16 (Document 5 §2.2) |

## Deliverables

| # | Document | Contents |
|---|---|---|
| 1 | [Executive Audit Report](01-executive-audit-report.md) | findings, risks, duplication, gaps, recommendation |
| 2 | [Existing Notification Inventory](02-existing-notification-inventory.md) | per-service producers/consumers, categories, 30 tables, existing APIs, event/job paths, production states |
| 3 | [Current and Target Architecture](03-current-and-target-architecture.md) | diagrams, Options A/B/C, ownership, dependency graph, infrastructure sizing |
| 4 | [Backend Specification](04-notification-service-backend-specification.md) | capabilities, APIs (EXISTING vs NEW), event contracts, DDL, preferences and precedence, threat model, reliability, workers |
| 5 | [Zero-Disruption Migration Plan](05-zero-disruption-migration-plan.md) | Phases 0–8, shadow rules, per-channel handover, rollback per stage, retirement evidence |
| 6 | [Implementation Roadmap](06-implementation-roadmap.md) | sections N0–N14 (14 fields each), dependency graph, frontend path |
| 7 | [Testing and Acceptance Matrix](07-testing-and-acceptance-matrix.md) | functional, reliability, security, migration, production gates, test vs real adapters |
| 8 | [Risk, Cost and Decision Register](08-risk-cost-and-decision-register.md) | classified findings, security repairs, risks, costs, owner decisions D-1…D-13 |
| — | [evidence/](evidence/) | raw audit notes per domain with path:line citations, plus the certify summary |

---

## Final verdict (Part 17)

**1. Do we genuinely need a standalone Notification Service, or would extending the current architecture be safer?**

Both, for different concerns.
- **Transports: extending is safer.** Email is already correctly centralized in the Meeting Service ledger (exact-once enqueue). Ringing is correctly in Calling. Customer webhooks are correctly in the Developer Platform.
- **Orchestration: a new narrow service is needed.** No existing service can own cross-domain recipient routing, preference enforcement, an in-app inbox and cross-channel delivery history without leaving its own authority. The Hub is limited to the personal-communications scope (`packages/communications-policy/src/lib.rs:11-55`). Meeting would take on Organizations, Developer Platform and Calling concerns.
- **Recommendation:** Option A (orchestration service). Full consolidation (Option B) and extending the Hub (Option C) are rejected for the reasons in Document 3 §4.

**2. Which existing functionality can be reused without modification?**
- The Meeting email ledger and its worker, provider adapters and Svix webhooks. The only changes are the N0 bug fixes and one new internal enqueue endpoint.
- Calling ringing, arbitration, signaling and recovery.
- Developer Platform webhooks.
- Organizations outbox mechanics.
- Hub preference storage. It receives additive fields only.
- Messaging's push decision logic and metadata whitelist. The adapters move unchanged into a shared crate.
- `cf-service-auth`, `cf-webhooks` (`retry`, `secret_box`), `cf-migration-guard`, the contracts tooling and the coordinated release framework.

**3. Which notification responsibilities should be migrated?**
- Orchestration of meeting notices (reschedule, cancel, reminder): channel selection, preferences and quiet hours, delivery tracking.
- Developer human notices.
- All new categories:
  - contact requests and acceptances;
  - organization invitation in-app copies and membership changes;
  - missed calls, voicemail, recordings;
  - message requests.
- Non-call push dispatch and the device push-target registry (if D-1 is approved).
- In-app history.

**4. Which responsibilities must remain with their existing services?**

| Service | Keeps |
|---|---|
| Account (Meeting) | OTP, verification, reset and step-up codes; security emails; token-bearing invitation emails (token minter) |
| Meeting | the business-transaction job writer, the reminder scan and recipient resolution |
| Calling | ringing, signaling, arbitration and compliance notices |
| Messaging | ciphertext, realtime and the wake-up decision with its E2EE rechecks |
| Hub | preference writes |
| Organizations | membership authority |
| Developer Platform | customer webhooks |
| Monitoring stack | infrastructure alerting |

**5. Can existing email, push, preferences and realtime systems be consolidated without downtime?**

Yes, under this plan, because nothing user-facing is switched off at any point.
- **Email:** the transport does not move. The handover dual-enqueues into the same ledger with deterministic keys, so it is exact-once by `UNIQUE (idempotency_key)`.
- **Push:** there is no production traffic (`MESSAGING_PUSH_PROVIDER=none`), so there is nothing to interrupt.
- **Preferences:** the single writer stays.
- **Realtime:** existing channels are untouched. The inbox starts with polling.

**6. What are the biggest architectural and security risks?**

Architectural:
- (a) Building on transports that are broken today: the OTP email transport (S-2) and the missing Hub grant (S-1).
- (b) Duplicate or lost meeting notices at handover. The deterministic key and fence mitigate this.
- (c) Per-recipient policy fan-out load (about 6 signed calls per Hub decision). The batch X3 call mitigates this.
- (d) Producer outbox dead-lettering during a long NS outage. Replay runbooks mitigate this.

Security:
- (e) Unverified public exposure of `/internal/operations/email/*` (S-3).
- (f) Caller-controlled push suppression (S-4).
- (g) Plaintext OTPs in Redis (S-5).
- (h) Push handles in plaintext that survive erasure (S-7, S-13).
- (i) E2EE metadata to APNs/FCM. This is a documented residual.

**7. How many new APIs, tables, workers and events are required?**

| Item | Count |
|---|---|
| New public user APIs | 5 (U1–U5) |
| Extended existing public APIs | 2 (Hub preferences GET/PUT, additive) |
| New internal APIs on NS | 7, plus 2 conditional on D-1 |
| New internal APIs on existing services | 3 (Meeting X1/X2, Hub X3) |
| New tables | 9, plus 1 conditional |
| Additive migrations in existing services | 3 |
| Workers | 4 permanent + 1 temporary |
| Consumed event types | 24, plus 1 control event |
| Emitted event types | 1 |
| Additive event in Calling | 1 (`CALL_PARTICIPANT_MISSED`, D-10) |

**8. How much additional infrastructure will the service require?**
- One container with a 256 MiB limit (expected resident memory under 64 MiB).
- One database in the existing `cfmeet-core-db`.
- No Redis, no managed queue, no new server. CPU use is negligible.
- It fits the measured headroom: 5,076 MiB available (certify 43670057279) against the release gate, which needs `MemAvailable − (256 + 832) ≥ 1,024` MiB.
- Additional infrastructure cost: €0. Provider fees apply only for owner-activated providers.

**9. What is the safest migration sequence?**

1. **N0:** repair the existing paths and activate Resend.
2. **Phase 0:** read-only baseline.
3. **Phase 1:** dark service.
4. **Phase 2:** shadow ingestion (additional outbox destinations, no sends).
5. **Phase 3:** in-app for categories with no current channel.
6. **Phase 4:** meeting notices handover (dual-enqueue, then fence) → developer notices → push registry and dispatch (D-1) → Calling push (D-2) → optional security in-app copies.
7. **Phase 6:** canary at 1 / 10 / 50 / 100 %.
8. **Phase 8:** evidence-based retirement.

Every stage has a configuration-only rollback (Document 5 §9).

**10. What is the exact implementation scope and definition of done?**
- **Scope:** roadmap sections N0–N14 (Document 6), limited to first-party users.
- **Out of scope:**
  - customer webhooks;
  - customer Messaging push;
  - SMS and telecom;
  - infrastructure alerting;
  - any change to E2EE, Calling, Meetings or Organizations business rules.
- **Programme definition of done:**
  - every category's channel authority is at 100 % NS ACTIVE (or deliberately left with its owner) for 30 days;
  - every Document 7 §5 gate passes in production, with real-credential checks recorded for each activated provider;
  - zero rollbacks open;
  - legacy rendering paths are retired only with the evidence in Document 5 §10;
  - Document 8 SEC items S-1 … S-12 are closed.

---

**Recommended first approvals:**
- **D-13:** approve the blueprint.
- **D-4:** activate Resend.
- **N0 + N1 + N4**, which can run in parallel and carry no migration risk.
- **D-1:** decide the push registry owner *before* any push provider is activated.

**Do not implement anything until the owner approves.**
