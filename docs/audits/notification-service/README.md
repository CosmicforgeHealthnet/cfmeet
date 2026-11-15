# CF Meet Unified Notification Service: Audit and Zero-Disruption Migration Blueprint

**Status:** `BLUEPRINT_APPROVED (D-13) — N0, N1, N4 ENGINEERING_IMPLEMENTED; MERGE, RELEASE AND PRODUCTION VERIFICATION PENDING`

**Engineering report:** [Document 9](09-engineering-report-n0-n1-n4.md) (2026-11-15). Owner decisions D-5, D-10, D-15, D-16 approved; D-W excluded; D-12 decided.

**Revision 2, corrected before coding (2026-11-15).** The owner confirmed full consolidation (D-0): one notification system that every service uses, with every current notification delivery pipeline moving into it. This correction pass leaves the target architecture unchanged. It does the following:
- reconciles every count;
- strengthens the routing fence;
- separates email-provider activation from development;
- verifies the authentication fallback;
- splits engineering completion from operational acceptance;
- reconciles against the latest `main`;
- records Q1–Q18 as outstanding.

| | |
|---|---|
| Audit base | `main` @ `6f3743f806434a55bad8297f104d28a55e350e2e` |
| Reconciled against | `main` @ `376339215233a55aae365e3bde9830da51778e0c` (§Reconciliation) |
| Production evidence | read-only certify [43670057279](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43670057279) (`CERTIFIED_READ_ONLY`); release history up to run 43702218850; `GET https://api.cfmeet.com/health` = 200; dated `docs/operations/*` |
| Changes made | this folder only (documentation). No production code, schema, secret or deployment change |
| Outstanding | production baseline queries Q1–Q18, **all OUTSTANDING** (Document 5 §2.2, Document 8 §3a) |

## Deliverables

| # | Document |
|---|---|
| 1 | [Executive Audit Report](01-executive-audit-report.md) |
| 2 | [Existing Notification Inventory](02-existing-notification-inventory.md) |
| 3 | [Current and Target Architecture](03-current-and-target-architecture.md) |
| 4 | [Backend Specification](04-notification-service-backend-specification.md): fence protocol §9, fallback §10.4, email providers §14 |
| 5 | [Zero-Disruption Migration Plan](05-zero-disruption-migration-plan.md) |
| 6 | [Implementation Roadmap](06-implementation-roadmap.md): milestones M1–M4 |
| 7 | [Testing and Acceptance Matrix](07-testing-and-acceptance-matrix.md) |
| 8 | [Risk, Cost and Decision Register](08-risk-cost-and-decision-register.md) |
| 9 | [Engineering Report: N0, N1, N4](09-engineering-report-n0-n1-n4.md) |
| — | [evidence/](evidence/): raw audit notes (line references pinned to `6f3743f`) and the certify summary |

---

## Canonical register

**Every count in the eight documents must equal this table.** The counting unit for APIs is one HTTP method on one path, which is one operationId. Health endpoints (`/health`, `/ready`) are not counted.

| Item | Count | Identifiers | Defined in |
|---|---|---|---|
| New public user APIs | **9** | U1–U9 | Doc 4 §3.1 |
| Existing public operations kept as facades | **11** | Hub notification-preferences GET/PUT, conversation preferences GET/PUT, contacts PATCH (`notifications`), delivery-status GET, device push PUT/DELETE; Messaging push-registration PUT/DELETE, push/configuration GET | Doc 4 §3.1 |
| New public provider callback | **1** | POST `/api/v1/notifications/provider-events/resend` | Doc 4 §3.2 |
| New internal APIs on the service | **11 + 1 optional** | I1–I11; I12 (optional, D-15) | Doc 4 §3.3 |
| New internal APIs on existing services | **7** | X1–X7 | Doc 4 §3.4 |
| Inbound event types | **31** (30 core + 1 optional) | Doc 4 §5.2 #1–#31 (#31 optional) | Doc 4 §5.2 |
| Control events | **1** | `notifications.cancel` | Doc 4 §5.2 |
| Outbound events | **1** | `notifications.inbox.changed` | Doc 4 §5.3 |
| New tables in the service | **17** | tables 1–17 | Doc 4 §6 |
| New producer-side tables | **5** | Meeting ×3, Messaging ×2 | Doc 4 §9.2 |
| Changes to existing tables | **6** | Meeting `outbox_events` channel; Hub outbox CHECK; Calling outbox destination + fact; guard triggers ×3 | Doc 4 §9.2, §9.4 |
| Workers in the service | **5 permanent + 1 temporary** | W1–W5; W6 | Doc 4 §11 |
| Producer-side workers | **1 new + 6 extended** | PW1; six owner relays/publishers | Doc 4 §11 |
| Test cases | **87** | F-01–F-23, R-01–R-23, ST-01–ST-20, M-01–M-21 | Doc 7 |
| Production acceptance measures | **28** | G-01–G-28 | Doc 7 §5 |
| Delivery-evidence gates | **4** | E1–E4 | Doc 4 §14.3 |
| Baseline measurements | **18, all OUTSTANDING** | Q1–Q18 | Doc 5 §2.2 |
| Roadmap sections | **17 engineering + E-track** | N0–N16; E1–E4 | Doc 6 |
| Containers | **2** (≤ 256 MiB each) | — | Doc 3 §7.2 |

---

## Reconciliation against the latest `main`

`git log 6f3743f..origin/main`: 2 commits.

| Commit | Change | Effect on the blueprint |
|---|---|---|
| `de7b863` | `activate-web-push` derives the VAPID public key as PEM (run 43702218850 failed before any change) | **Relevant.** The owner is activating Web Push **in Messaging**. The blueprint already supports this through the push copy path (Doc 5 §6, 4.1; D-1b). Risk K-7 is raised to High likelihood. Line references at or after `coordinated_release.py:1891` shift by about +6 (`activate_web_push` is now lines 1901-1932). The eight documents cite only lines before 1891 (unchanged) |
| `3763392` | web production-journey diagnostics (`apps/web/tests/messaging-production.browser.mjs`) | none |

**Production state since the audit:**
- Release 43696469230 (`6f3743f`) completed successfully.
- No `activate-resend-email` run, so the email provider is unchanged.
- No successful `activate-web-push`, so push is still unconfigured.

No notification code path cited by the documents changed.

---

## Owner-approved boundaries (verified in every document)

| Boundary | Where it is enforced |
|---|---|
| Calling retains live ringing, signaling and first-answer arbitration; only call **push** moves (I3, ≤ 1 s, post-commit) | Doc 3 §2.1, Doc 4 §1 P1, Doc 5 §1 C2 |
| Account retains code generation, storage, expiry and verification; only the email delivery moves (CRITICAL lane, X6/X7 leases) | Doc 4 §9.8, §10.4; Doc 5 §6 4.6 |
| Messaging retains E2EE, ciphertext, realtime and membership/device authority; it authorizes every wake-up (X3) | Doc 4 §1 P5, §3.4 X3 |
| All notification delivery channels consolidate: email, push (message, call, other), in-app, and preferences | Doc 1 §1, Doc 3 §2.1 |
| **To be confirmed by the owner:** whether operator alerting (D-15, currently optional) and customer webhooks (D-W, currently out of scope) count as "notification delivery channels" under the mandate | Doc 8 §5 |

---

## Final verdict (Part 17)

1. **Standalone service?** Yes: a unified service (D-0). The evidence is four pipelines with three retry models, preferences enforced outside their owner, many events with no notification home, and no delivery monitoring.
2. **Reusable without modification:**
   - Calling signaling;
   - Messaging realtime and E2EE;
   - producer outbox mechanics;
   - `cf-service-auth`, `cf-webhooks`, `cf-migration-guard`, contracts tooling, the coordinated release.

   The email adapters (`EmailProvider`: Resend, SMTP, recording, none), the ledger, the push adapters and the preference logic are **ported with their tests, preserving the architecture** (Doc 4 §14.1).
3. **Migrate:** every delivery pipeline listed in Doc 1 §1.
4. **Remain:** see the boundaries table.
5. **Without downtime?** Yes. Each message has a sticky route decision in its producer's transaction; old-path tables are guarded; rollback is a no-restart routing change; old paths stay as fallbacks until evidence-based retirement.
6. **Biggest risks:**
   - sign-in email's dependence on the service (verified fallback, Doc 4 §10.4);
   - mixed-version or crash duplicates (fence protocol, Doc 4 §9);
   - baselines taken on a broken transport (E1 first);
   - re-certifying email and push in the new home;
   - the pre-existing defects S-1, S-2 and S-3.
7. **Size:** canonical register above.
8. **Infrastructure:**
   - 2 × 256 MiB containers and 1 database in the existing Postgres;
   - no Redis, no queue product, no new server;
   - €0 additional infrastructure;
   - the release gate was checked on a single memory sample, and Q16 is OUTSTANDING.
9. **Sequence:** N0 and the E-track in parallel → Phase 0 → 1 → 2 → 3 → 4.1 push → 4.2 preferences → 4.3a/b standard email → 4.4 invitations → 4.5 security notices → 4.6 codes → (4.7) → (4.8) → retirement.
10. **Definition of done:**
    - **M1 ENGINEERING_COMPLETE:** N1–N13 built, tested, deployed and verified, including fallback verification.
    - **M2 OPERATIONALLY_ACCEPTED** per category: 30 days, or 60 days for CRITICAL.
    - **M3 PROGRAMME_ACCEPTED.**
    - **M4 LEGACY_RETIRED** (Doc 6 §Milestones).

---

## Implementation-readiness verdict

**`READY_TO_START_ENGINEERING (N0, N1, N4) — PRODUCTION_MIGRATION_BLOCKED_ON_EVIDENCE_AND_DECISIONS`**

| Area | Verdict | Basis |
|---|---|---|
| Target architecture | **stable** (D-0; unchanged by this pass) | Docs 1, 3, 4 |
| Interface completeness | **ready to freeze in N1**, once the four interface-shaping decisions below are made | canonical register |
| Duplicate safety across ledgers | **specified**: no silent duplicates by construction; residuals recorded (Doc 4 §9.11) | fence protocol v1; tests M-10 … M-21 |
| Authentication fallback | **specified and testable**: works with both replicas down and with the service DB down (separate Meeting DB container) | Doc 4 §10.4; tests R-19, R-21, R-22, R-23 |
| Provider independence | **ready**: N-track needs no provider; E-track is the owner's | Doc 4 §14.2 |
| Production baselines | **not available**: Q1–Q18 OUTSTANDING; email baselines must follow E1 | Doc 5 §2.2 |
| Production migration (Phase 4) | **blocked** until E1/E2 (owner provider activation with delivery evidence), Q-measurements, N0-1/N0-3, and D-9 (alerting, before CRITICAL) | Docs 5, 6, 7 |

**Unresolved design decisions:**

*Decide before the N1 contract freeze (they change interfaces):*

| Decision | What it affects |
|---|---|
| D-5 | Organizations `resolveInvitationInvitee` (X5); otherwise the in-app invitation copy is dropped |
| D-10 | Calling `CALL_PARTICIPANT_MISSED` fact (event #15) |
| D-15 | operator alert channel in scope (I12, event #31) |
| D-W | whether customer webhooks are inside the consolidation mandate; the blueprint keeps them out until confirmed |
| D-16 | AUTO failover enabled by default (affects relay PW1 behaviour and the X6 contract use) |

*Decide before specific phases:*

| Decision | Needed before |
|---|---|
| D-13 | blueprint approval: start of any engineering |
| D-4 / E-track | provider activation with delivery evidence: Phase 4.3 |
| D-17 | acceptance mailbox, only if SMTP is used: E1 with SMTP |
| D-14 | secondary provider for real fallback diversity: Phase 4.6 |
| D-9 | alerting deployment: N12 / Phase 4.5 |
| D-3 and D-1b | push providers, and where Web Push is activated: Phase 4.1 |
| D-11 | UI design: FE2 / FE3 |
| D-12 | `register` 409 behaviour: N0-9 |
| D-6, D-7, D-8 | defaults proposed; non-blocking |

**Approved 2026-11-15 (D-13): N0, N1 and N4 implemented — see Document 9. No live notification-pipeline migration has been made.**
