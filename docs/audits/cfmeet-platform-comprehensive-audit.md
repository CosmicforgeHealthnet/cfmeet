# CF Meet — Comprehensive Platform Audit: Executive Report

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.


## 1. Outcome in brief

- **The backend is broad and mostly well engineered, but CF Meet cannot yet deliver its intended user experience end to end.**
  - There are 12 backend services with 1,066 registered operations.
  - 70 of 211 product features have backend behaviour verified by CI tests against real databases or real stacks.
  - Only 1 of 20 user journeys passes end to end, and it is voicemail deposit.
- **Most failures sit between the backend and the clients, not inside the backend:**
  - The web meetings list shows the space's **creation date** instead of the scheduled date (MTG-004). This is the date/time problem the owner noticed. The schedule itself is stored and returned correctly.
  - The web pre-join screen reads fields the backend does not send, so **consent-gated and invite-only meetings cannot be joined** (FE-001).
  - The **mobile app's meeting features are broken end to end** (MTG-001/002/003). It sends legacy shapes the backend never had and calls 35 v2 paths that do not exist.
  - There is **no calendar or cross-space "my meetings" API**, and no invitation emails, reminders or change notifications (MTG-005/006).
- **Organizations are structurally incomplete:**
  - Three unrelated "organization" models exist (ORG-001).
  - Meeting organizations can be created only by a platform admin, and only with curl (ORG-002).
  - There is no organization detail or edit endpoint (ORG-004).
  - `created_by` is stored but never returned, and location is not stored anywhere (ORG-003).
  - Members are added without invitation or consent (ORG-005).
- **There are 4 P0 findings:**
  - No backup path for the account database (SEC-010).
  - Logout and password reset do not revoke tokens on v1 routes (ACC-002).
  - Google sign-in can take over a pending password account (ACC-004).
  - Mobile meetings are broken (MTG-001).
- **Backend engineering readiness and public product activation are different things.** The Sections 8–23 production deployments (Meeting, Calling, Messaging MANAGED, Communications, Platform Access, Scan Gateway) were not disturbed by this audit, and their verified scope is acknowledged. Several capabilities remain unactivated: the API Gateway and Developer Platform are built but not deployed, and PSTN, numbers, SMS, push and PRIVATE_E2EE are blocked by external dependencies.

## 2. Consolidated dashboard

| Metric | Result | Basis |
|---|---|---|
| Total backend services | **12** | `services/*`, each with its own router. No other backend service exists in the repo. |
| Total registered operations | **1,066** | Static extraction of every axum router (§6) |
| Total public APIs | **650** HTTP operations: 568 gateway-routed, 82 direct-only. Includes the 73 Communications Hub operations. | `visibility = PUBLIC` in the CSV |
| Total internal/private APIs | **269**: 166 internal service-to-service, 94 Messaging delegated mirror, 9 provider callbacks | CSV |
| Total admin/operator APIs | **100**: Developer Platform operator 32, Platform Access admin 26, Numbering admin 25, Meeting 14, Communications 3 | CSV |
| Total realtime interfaces | **5** WebSocket routes, 3 of them gateway-routed, plus 3 ticket/credential endpoints and the gateway WebSocket proxy | CSV, `api-gateway/src/ws.rs` |
| Infrastructure endpoints (health, metrics, docs) | 42 | CSV |
| Duplicate candidates | **25 groups**: 3 exact (same path, two services), 4 conflicting authorities, 1 legacy compatibility (23 v1 ops), 4 intentional facades, 4 internal variants, 9 name-only | [Duplication report](cfmeet-api-duplication-report.md) |
| Confirmed redundant APIs | **0 removable today. 31 after client migration:** 8 legacy developer-API routes and 23 v1 meeting write routes. The 10 Calling webhook/usage/limits copies also become redundant after the Part VI cut-over. | Duplication report |
| Missing APIs | **24 new endpoints justified** (about 28 if MFA is approved). The other gaps are closed by fixing or reusing existing endpoints. | [Implementation plan §3](cfmeet-recommended-implementation-plan.md#3-missing-api-estimate-evidence-backed) |
| Product features assessed | 211 across 6 domains, plus 39 frontend screens and 30 security controls as separate lenses | [Gap register §1](cfmeet-feature-gap-register.md) |
| Missing features | **26** | Gap register |
| Partially implemented features | **32** (plus 31 implemented-unverified, 29 frontend-only gaps, 7 contract mismatches) | Gap register |
| Incomplete data contracts | **21** canonical CONTRACT_MISMATCH findings (7 feature rows) | Gap register |
| Critical security findings | **2 P0 security** (ACC-002, ACC-004) **+ 1 P0 data protection** (SEC-010). 32 security-classified findings in total: P0 2, P1 15, P2 10, P3 5. | Gap register |
| P0 / P1 / P2 / P3 | **4 / 48 / 66 / 31** = 149 canonical findings (182 raw; 32 merged as duplicates; 1 informational) | Gap register §2 |
| User journeys | **20 traced** through code (0 executed live by this audit) | [Journey coverage](cfmeet-user-journey-coverage.md) |
| Passing / partial / failing journeys | **PASS 1 · PARTIAL 11 · FAIL 5 · BLOCKED_EXTERNAL 2 · NOT_TESTED 1** | Journey coverage |
| Backend/frontend integration gaps | **36** feature-level (29 frontend-only gaps, 7 contract mismatches). 54 findings owned by FRONTEND (37) or SHARED_CONTRACT (17). 35 mobile call sites target non-existent routes. 394 public or realtime operations have no frontend consumer. | [Frontend coverage](cfmeet-frontend-backend-coverage.md) |
| External dependency blockers | **6**: independent E2EE review (#13), push provider credentials, PSTN carrier, number provider, SMS carrier, key-transparency witnesses | §8 |

## 3. Readiness: backend engineering vs product activation

| Area | Backend engineering | Client delivery | Production activation |
|---|---|---|---|
| Meetings v2 (spaces, conferences, scheduling, recurrence, admission, recordings) | Strong: 20/37 features CI-verified against real PostgreSQL (LiveKit mocked) | Web: PARTIAL (MTG-004, FE-001, FE-007). Mobile: FAIL. | Deployed |
| Calendar, invitations, reminders | **Missing** (MTG-005/006/007) | Missing (`/dashboard/schedule` redirects to the list) | — |
| Organizations | Partial: admin-only creation, no detail, edit, invitations or audit | Read-only list; created date not shown | Deployed (limited) |
| Accounts and identity | Functional but with P0/P1 security debt (ACC-001…004, SEC-001) | Web works. Mobile logs credentials (ACC-012). | Deployed |
| Calling and enterprise calling | Strong: 29/42 CI-verified with real LiveKit, Asterisk/SIPp | Web PARTIAL (calls don't end for the callee, page-bound ringing, admin-gated settings). Mobile: none. | Deployed (Enterprise, recording, voicemail on; PSTN off) |
| Messaging | Strong backend | Web uses basic send/receive only; event re-sync broken; no realtime in production | MANAGED deployed; PRIVATE_E2EE BLOCKED_EXTERNAL (#13) |
| Communications Hub | Implemented (73 ops), production-verified per the closure report | **No client** | Deployed |
| Developer Platform and API Gateway | CI-verified | No console | **Not deployed** (PLT-005) |
| PSTN, numbers, SMS, Privacy Relay | Software-complete against simulators | Out of Phase 2 frontend scope | BLOCKED_EXTERNAL |

## 4. Top findings: what prevents the intended user experience

| Rank | ID | Severity | Finding | Why it matters |
|---|---|---|---|---|
| 1 | SEC-010 | P0 | No backup or restore path for the meeting-service (account authority), communications and developer-platform databases; nothing scheduled in the repo | One disk or VPS loss could permanently lose every account and meeting. Host-level snapshots outside the repo are unverified. |
| 2 | ACC-002 | P0 | v1 auth middleware and the developer session bridge ignore `credentials_not_before`; logout, password change and reset do not revoke old tokens there | An attacker keeps access for up to 7 days after the victim resets their password |
| 3 | ACC-004 | P0 | Google sign-in activates a pending password account matched by email and keeps the attacker's password | Account pre-hijacking |
| 4 | MTG-001 (+MTG-002/003, FE-004/005/006) | P0 | Mobile meetings: wrong v1 shapes, 35 non-existent v2 paths, live room with no media SDK, hard-coded LAN API URL | Mobile users cannot create, list, schedule or join meetings |
| 5 | MTG-004 | P1 | Web list and dashboard show the creation date under a calendar icon instead of the scheduled date/time | The owner-reported scheduling defect. Display only: the backend is correct. |
| 6 | FE-001 | P1 | Pre-join reads `requiresInvitation` / `consentRequirements`; the backend sends `access.*` / `consents.*` | Consent-gated (healthcare) and invite-only meetings cannot be joined |
| 7 | MTG-005, MTG-006 | P1 | No cross-space agenda or calendar API; no invitation emails, reminders or change notifications | There is no calendar, and invitees are never told about schedule changes |
| 8 | ORG-001…005 | P1 | Three organization models; admin-only creation with no tooling; no detail or edit endpoint; `created_by` never returned and location never stored; no invitations | The Organization experience cannot show who created it, when or where, and users cannot build one |
| 9 | CAL-001, CAL-002 | P1 | Connected calls never end when parties leave; only the initiator can end, hold or resume in the web | Ghost calls consume limits and block queue agents; callees cannot hang up |
| 10 | MSG-001, MSG-002, MSG-004 | P1 | Event re-sync parameter mismatch (400); relay lacks PATCH; no realtime for first-party users in production | Chats miss updates; edits and role changes are impossible; 15 s polling |
| 11 | ACC-001, ACC-003, SEC-001 | P1 | Token type confusion; revocation off when Communications is unconfigured; hard-coded JWT fallback without fail-fast | Defence-in-depth gaps in the authority every service trusts |
| 12 | PLT-005, PLT-003, PLT-002 | P1 | Gateway and Developer Platform not deployed although SDKs and docs point at them; legacy meeting webhooks are never delivered; legacy API keys are plaintext | The developer offering is not live, and the shipped legacy one is unsafe and misleading |
| 13 | CAL-007 (ORG-012) | P1 | All CF Meet users go through one shared calling credential and scope; enterprise admin is an environment allowlist | No per-organization enterprise calling |
| 14 | MSG-006 (ACC-009, FE-020) | P1 | The Communications Hub (73 ops) has no web or mobile client | Contacts, CF Meet ID/QR, invites, blocks, privacy and notification preferences are not usable |
| 15 | SEC-011, SEC-016 | P1 | Production migrations run without a pre-migration snapshot; no alert rules as code | Unrecoverable bad migrations; silent outages |

## 5. Domain summaries

| Domain | Status | Headline | Detail |
|---|---|---|---|
| Meetings, scheduling, calendar | Backend strong; clients and calendar weak | v2 scheduling, recurrence with DST, reschedule, cancel, join window and admission are CI-verified. Missing: calendar/agenda, notifications, ICS. Web display defects. Mobile broken. | [Meetings audit](cfmeet-meetings-calendar-audit.md) |
| Organizations | Incomplete | 13 of 32 features missing. Creation metadata stored but not exposed (`created_by`) or not rendered (`created_at`). Location, type, description and incorporation date not modelled. | [Organizations audit](cfmeet-organizations-audit.md) |
| Accounts, contacts, devices, notifications | Functional with security debt | 4 P0-candidate issues (2 confirmed P0 after reconciliation). No session model, MFA or deletion. The Hub has no client. | [Gap register §4](cfmeet-feature-gap-register.md#accounts-identity-contacts-devices-notifications) |
| Calling and enterprise calling | Backend strong; web partial | 29/42 CI-verified. Call termination and web controls broken. No push. One shared enterprise scope. | [Gap register §4](cfmeet-feature-gap-register.md#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| Telephony, numbering, SMS, privacy relay | External | Software-complete against simulators; carriers and providers absent; not deployed | same |
| Messaging, unified communications | Backend strong; first-party path weak | Relay and contract defects; minimal web UI; no realtime in production; PRIVATE_E2EE correctly gated | [Gap register §4](cfmeet-feature-gap-register.md#messaging-communications-hub-meeting-chat-realtime) |
| Platform: Developer Platform, gateway, SDKs, Swagger, admin | Built, not deployed | 572 gateway routes all resolve upstream; SDKs current; 3 webhook systems; 2 credential systems; fragmented operator plane | [Gap register §4](cfmeet-feature-gap-register.md#platform-developer-platform-gateway-sdks-swagger-admin) |
| Security, reliability, data | Newer services strong; account authority weak | Backups, migration safety, alerting, audit trail; 167 migrations, 17 history tables | [Data & architecture](cfmeet-data-and-architecture-gaps.md) |

## 6. API inventory and cross-checks

All registered routes were extracted statically from source (§10). Counts by service and visibility class:

| Service | Public | Realtime (WS) | Admin/operator | Internal service | Internal delegated mirror | Provider callback | Infrastructure (health/metrics/docs) | **Total** | Gateway-routed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| api-gateway | 0 | 0 | 0 | 0 | 0 | 0 | 6 | **6** | 0 |
| calling | 142 | 3 | 0 | 24 | 0 | 1 | 6 | **176** | 141 |
| communications | 73 | 0 | 3 | 11 | 0 | 1 | 2 | **90** | 73 |
| developer-platform | 83 | 0 | 32 | 8 | 0 | 0 | 3 | **126** | 74 |
| meeting-service | 193 | 1 | 14 | 5 | 0 | 2 | 4 | **219** | 130 |
| messaging | 96 | 1 | 0 | 20 | 94 | 0 | 4 | **215** | 96 |
| numbering | 52 | 0 | 25 | 17 | 0 | 1 | 4 | **99** | 52 |
| platform-access | 0 | 0 | 26 | 38 | 0 | 0 | 3 | **67** | 0 |
| privacy-relay | 5 | 0 | 0 | 24 | 0 | 0 | 2 | **31** | 0 |
| security-scan-gateway | 0 | 0 | 0 | 1 | 0 | 0 | 3 | **4** | 0 |
| sms-gateway | 6 | 0 | 0 | 4 | 0 | 2 | 4 | **16** | 6 |
| telephony | 0 | 0 | 0 | 14 | 0 | 2 | 1 | **17** | 0 |
| **Total** | **650** | **5** | **100** | **166** | **94** | **9** | **42** | **1066** | **572** |

**Cross-check results** (source code ↔ registered routes ↔ gateway ↔ served Swagger ↔ SDKs ↔ frontend):

| Check | Result |
|---|---|
| Contract operation without a registered handler | **0.** All 905 unique contract operations resolve to a handler. The 73 Communications operations match the Hub dispatch table one-to-one. The 5 Privacy Relay contact operations are served by Privacy Relay itself, not by Calling, and the web proxies them to the wrong service (CAL-013). |
| Registered route missing from every contract (Swagger) | 126. All are infrastructure, internal-mirror or provider routes, except 3: the two meeting-service Communications relay wildcards, which are documented as 73 concrete operations, and a CORS `OPTIONS` preflight. |
| Gateway manifest route with no upstream handler | **0 of 572.** There is no CI gate for this (INV-001). |
| Gateway route unusable by design error | 1: `POST /v1/privacy-relay/contact-access-grants` has a prose `requiredScope`, so it is always denied (PLT-001) |
| SDK coverage | TypeScript and Dart each hold exactly the 572 gateway operations; generation check passes (`sdks/generate.py --check`). Neither app uses the SDKs. |
| Swagger served | Three renderers (gateway, meeting-service, calling) publish contradictory "public" surfaces (PLT-006) |
| Frontend consumption | 266 operations consumed (261 public). 394 public or realtime operations are unused. Mobile calls 35 non-existent v2 paths. |
| Operations without any test reference | 200, including 13 meeting-service auth/API-key/webhook routes (INV-002) |

## 7. Architectural risks

1. **The account authority is the least hardened service.** Meeting-service owns accounts that every other service trusts, but uses symmetric JWTs with no token type, has revocation gaps, plaintext legacy secrets, naive timestamps, no audit table and no backup path.
2. **There is no organization authority.** Meetings, the Developer Platform with Platform Access, and Calling/Numbering each model "organization" differently (ORG-001). Enterprise Calling runs as one shared scope.
3. **Two meeting generations are live.** v1 and v2 coexist, and mobile is the last v1 writer, so legacy retirement is blocked.
4. **Part VI is built but undeployed.** The SDKs and public docs point at a gateway that is not serving.
5. **Device and revocation fan-out spans four registries** with no account-level revocation event (Journey H FAIL).
6. **No contract-parity gate for clients.** Every major client defect in this audit is drift that generated types or contract tests would have caught.

Details: [data & architecture assessment](cfmeet-data-and-architecture-gaps.md).

## 8. External dependencies (not software defects)

| Dependency | Blocks | Status |
|---|---|---|
| Independent E2EE cryptographic review and owner acceptance (#13) | PRIVATE_E2EE in production | The production validator correctly refuses activation |
| APNs / FCM / Web Push credentials | Incoming-call push, offline message push | Code exists for Messaging. Calling uses `NoPushTransport`. |
| PSTN carrier contract | Phone calls, SIP trunk, Privacy Relay numbers | Telephony not deployed; flags off |
| Number provider (and adapter) | Real numbers / DCNs | Synthetic provider only |
| SMS carrier (and adapter) | SMS, phone verification | Test provider only |
| Key-transparency witnesses | KT witness checkpoints | Not configured |

Issue #37 (Chromium web Messaging journey) is a **frontend** defect: a collapsed device panel hides the `recover-history` control. The backend jobs in the same workflow pass. It is recorded as FE-016 / MSG-014 and is **not** a backend failure.

## 9. Recommended workstreams (proposal; not executed)

There are 11 workstreams, gated by 9 owner decisions. Full detail: [implementation plan](cfmeet-recommended-implementation-plan.md).

| WS | Scope | Owner class | Complexity | Phase |
|---|---|---|---|---|
| WS-1 | Account authority security hardening (ACC-001…006, SEC-001/004/007/013, DATA-002) | BACKEND | MEDIUM | 1 |
| WS-2 | Data protection and release safety (SEC-010/011/015/016/017/018) | INFRASTRUCTURE | MEDIUM | 1 |
| WS-3 | Meetings web fixes (MTG-004, FE-001, FE-007, MTG-012/013/015/016) | FRONTEND | LOW–MEDIUM | 1 |
| WS-4 | Scheduling correctness, calendar/agenda API, notifications, ICS (MTG-005…011, 014, 018) | BACKEND + FRONTEND | MEDIUM–HIGH | 2 |
| WS-5 | Organizations completion (ORG-003…011) after D-1/D-2 | BACKEND + FRONTEND | MEDIUM | 3 |
| WS-6 | Mobile v2 migration, then legacy write retirement | FRONTEND | HIGH | 3 |
| WS-7 | Messaging first-party completeness (MSG-001…008, #37) | BACKEND + FRONTEND | MEDIUM | 2 |
| WS-8 | Calling lifecycle and web calling (CAL-001…017) | BACKEND + FRONTEND | MEDIUM | 2 |
| WS-9 | Sessions, devices, Communications Hub client, deletion/export | BACKEND + FRONTEND | MEDIUM–HIGH | 4 |
| WS-10 | Developer Platform rollout and legacy developer API retirement (PLT-001…009) | BACKEND + INFRASTRUCTURE | MEDIUM | 4 |
| WS-11 | Contract parity and end-to-end test gates (FE-015, INV-001/002) | SHARED_CONTRACT | MEDIUM | 2 (cross-cutting) |

## 10. Method, evidence and limitations

- **Scope:** every service, package, app, contract, gateway manifest, SDK, migration directory and workflow at the baseline commit, inventoried and assessed. Eight domain audits ran in parallel and were consolidated and re-verified by the lead auditor.
- **API inventory:** a static extractor followed each service's router composition from `axum::serve` (route, nest, merge, let-bindings, router parameters, nested router functions), plus the Communications dispatch table. It was cross-checked against every OpenAPI contract, the gateway route manifest and catalogue, both generated SDK operation tables, test-file string literals and 388 frontend call sites. Its accuracy was validated against the contracts: every documented operation resolved to an extracted route.
- **Lead-auditor re-verification** of high-severity claims in code: MTG-004, MTG-001, MTG-003 (route cross-check), FE-001, ORG-002, ORG-003, ORG-004 (route inventory), CAL-001 (participant-left handling), CAL-002, CAL-003, MSG-001, MSG-002, ACC-001, ACC-002, ACC-003, ACC-004, PLT-002, SEC-001, SEC-010, issue #37.
- **Severity reconciliation** (see the gap register footnote ¹):
  - ACC-001 lowered to P1: its marginal impact over refresh-token theft alone is small; the exploitable defect is ACC-002.
  - ACC-003 lowered to P1: it only applies when Communications is unconfigured, and Communications is deployed in production.
  - SEC-001 lowered to P1: `docker-compose.prod.yml:91` requires `JWT_SECRET`; the production runtime env is unverifiable from the repo.
  - Duplicates reported by more than one auditor were merged under one canonical ID.
- **Limitations:**
  - No test suite was executed. CI results were read (latest backend CI on `e717d018`, whose later commits change only docs, infra and workflows).
  - No production host was contacted, so production configuration values (for example `JWT_SECRET` strength, `FEATURE_LEGACY_MEETING_WRITE_DISABLED`, attachment policy, host-level backups) are marked REQUIRES_VALIDATION where they matter.
  - Tenant-isolation was sampled (about 300 single-id lookups), not exhaustively swept (SEC-020).
  - Test-reference counts are a path-literal heuristic.
- **Safety:** no application code, contract, migration, infrastructure or workflow was changed. Only the audit documents in `docs/audits/` were added.

## 11. Deliverables

| # | Document | Content |
|---|---|---|
| 1 | `cfmeet-platform-comprehensive-audit.md` | This report |
| 2 | [`cfmeet-feature-gap-register.md`](cfmeet-feature-gap-register.md) | 211 features, the 182-finding master register (149 canonical), full evidence for Accounts, Calling, Messaging, Platform and Security |
| 3 | [`cfmeet-api-inventory.csv`](cfmeet-api-inventory.csv) | 1,066 operations × 29 columns: handler and registration, visibility, auth, fields, contract, gateway, SDK, frontend, tests |
| 4 | [`cfmeet-api-duplication-report.md`](cfmeet-api-duplication-report.md) | 25 candidate groups, facades vs duplicates, consolidation proposal |
| 5 | [`cfmeet-user-journey-coverage.md`](cfmeet-user-journey-coverage.md) | 20 journeys with step traces and status |
| 6 | [`cfmeet-meetings-calendar-audit.md`](cfmeet-meetings-calendar-audit.md) | Scheduling, recurrence, timezone, calendar, Space vs Conference, field-level table |
| 7 | [`cfmeet-organizations-audit.md`](cfmeet-organizations-audit.md) | Organization models, lifecycle, metadata root causes, permissions |
| 8 | [`cfmeet-data-and-architecture-gaps.md`](cfmeet-data-and-architecture-gaps.md) | Service inventory, authority map, migrations, data-model proposals |
| 9 | [`cfmeet-frontend-backend-coverage.md`](cfmeet-frontend-backend-coverage.md) | Screens ↔ APIs, contract mismatches, ownership |
| 10 | [`cfmeet-recommended-implementation-plan.md`](cfmeet-recommended-implementation-plan.md) | Owner decisions, 11 workstreams, missing-API estimate (proposal only) |

## Appendix A — Backend service inventory

| # | Service | Path | Purpose | Canonical data ownership | Dependencies | Migrations | Tests | Documentation | Deployment status | Known restrictions |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | **Meeting Service** | `services/meeting-service` | Account and identity authority (users, auth, Google sign-in), legacy v1 meetings, v2 Meeting Spaces / Conferences / admission / recordings / transcripts, meeting organizations, legacy developer API (API keys, webhooks), first-party relays to Messaging and Communications | users, sessions (JWT), meetings (v1), meeting_spaces, conference_sessions, participants, invitations, organizations, organization_memberships, artifacts, account_communication_state | PostgreSQL, Redis, LiveKit, SMTP, Google OAuth, Communications, Messaging, Platform Access, Developer Platform (session bridge) | `migration/` 27 (3 history tables) | 61 test files, ~683 tests (`backend.yml`, real PostgreSQL; LiveKit mocked) | README, Swagger (utoipa) `src/swagger/mod.rs`, ADR 0002/0053 | PRODUCTION (`deploy-backend.yml`) | Legacy v1 writes retire behind `FEATURE_LEGACY_MEETING_WRITE_DISABLED` (off by default) |
| 2 | **Calling** | `services/calling` | Canonical call aggregate, endpoints and ringing, LiveKit media, signaling WS, enterprise calling (extensions, ring groups, queues, IVR, business hours, forwarding, voicemail, recording), webhooks authority (until Part VI cut-over), public Privacy Relay facade, usage/limits | calls, call_participants/sessions/legs/events, enterprise_*, voicemail_*, call_recordings, webhook_*, platform_* projection | PostgreSQL, LiveKit, object storage, Platform Access, Communications, Telephony, Numbering, Privacy Relay, Developer Platform | `migration/` 46 (+1, 2 histories) | 102 test files, ~730 tests (real PostgreSQL, real LiveKit, Asterisk/SIPp in CI) | README, ADRs 0003, 0006–0009, 0018–0027, 0030 | PRODUCTION (Enterprise Calling, recording, voicemail ON; public PSTN OFF) | No push transport (`NoPushTransport`); PSTN flags off |
| 3 | **Messaging** | `services/messaging` | Conversations, membership, MANAGED and MLS (PRIVATE_E2EE) messages, attachments with malware scanning, realtime gateway, search, moderation, compliance, key transparency, SMS bridge, backups | messaging_conversations, members, messages, revisions, attachments, devices, key_packages, mls_*, transparency_*, push_*, blocks, legal_holds | PostgreSQL, Redis, object storage, Security Scan Gateway, Platform Access, Communications, Calling, SMS Gateway, APNs/FCM (not configured) | `migration/` 29 (+1) | 38 test files, ~284 tests (real PostgreSQL/WS/Redis/MinIO/clamd) | README, ADRs 0045–0054, architecture/messaging-*.md | PRODUCTION (MANAGED); PRIVATE_E2EE blocked by production validator (#13) | E2EE device registration refused in production |
| 4 | **Communications Hub** | `services/communications` | Account-level communications: profiles, CF Meet ID/QR, contacts, requests, invites, blocks, privacy, devices, notification preferences, inbox/timeline, chat/contact-to-call, meeting share | communication_profiles, public_ids, connections, contact_*, devices, block_bridge, timeline_entries, outbox, audit | PostgreSQL, Calling, Messaging, Meeting Service, Platform Access (via signed service auth) | `migration/sql` 2 (2 histories) | 3 test files, ~38 tests (real PostgreSQL, simulated owners) + five-service certification job | docs/api/communications-unified-handoff.md, operations/communications-*.md | PRODUCTION (73 public ops served via Meeting relay; 22/22 release checks) | Phone operations deferred by owner |
| 5 | **Platform Access** | `services/platform-access` | Shared identity authority: tenants, applications, environments, service accounts, API credentials (peppered hashes), principals, scopes, delegation, audit | platform_tenants, applications, environments, service_accounts, api_credentials, principals, identity_changes, access_audit | PostgreSQL | `migration/` 4 | 14 test files, ~121 tests | README, ADR 0032, 0046 | PRODUCTION (image in closure report §3) | No public routes (internal/admin only); audit table write-only |
| 6 | **Developer Platform** | `services/developer-platform` | Developer/enterprise control plane: organizations, members, invitations, projects, environments, credentials, service accounts, webhooks (central), usage, quotas, plans, production access, operator API | devplat_organizations, workspace_members, invitations, billing, plan_*, usage_*, webhook_*, idempotency | PostgreSQL, Platform Access, Meeting Service (sessions), Calling (webhook migration) | `migration/` 8 | 9 test files, ~120 tests (real PostgreSQL, fake Platform Access) | README, ADR 0055, phase-06 docs | NOT DEPLOYED (no image/deploy job; PLT-005) | Console UI deferred (spec D-07) |
| 7 | **API Gateway** | `services/api-gateway` | Public customer ingress: manifest-driven routing of 572 operations, credential auth, quotas, WebSocket proxy, aggregate OpenAPI and discovery | stateless (Redis quotas) | Redis, all upstream services, Platform Access | — | 7 test files, ~44 tests | README, ADR 0055, infra/api-gateway/* | NOT DEPLOYED (Part VI compose "not wired into any workflow") | Central webhooks/usage flags off |
| 8 | **Telephony** | `services/telephony` | Executes PSTN calls: SIP/Asterisk ARI legs, bridges, provider callbacks, routes, captures | telecom_providers, routes, telephony_legs, bridges, provider_events, captures | PostgreSQL, Asterisk ARI, Calling, Numbering, Privacy Relay, carrier (none) | `migration/` 17 | 25 test files, ~165 tests (simulators, Asterisk/SIPp) | README, ADRs 0014–0017 | NOT DEPLOYED to production | No carrier contract (EXTERNAL) |
| 9 | **Numbering** | `services/numbering` | Number catalog, KYC/eligibility, inventory search/reservation, provisioning, ownership/assignment, routing grants, stewardship | number_resources, reservations, assignment_history, kyc_*, provider_*, routing_*, relay_leases | PostgreSQL, Platform Access, Developer Platform, number provider (synthetic only) | `migration/` 10 | 23 test files, ~215 tests | README, ADRs 0033–0040, 0043–0044 | No production deployment evidence in the repository; synthetic provider refused in protected environments | Real provider adapter not written |
| 10 | **SMS Gateway** | `services/sms-gateway` | SMS transport, delivery events, opt-out, Messaging SMS bridge | sms_transports, attempts, delivery_events, optout_blocks, bridge_* | PostgreSQL, Messaging, Numbering, Calling, Platform Access, SMS carrier (test provider only) | `migration/` 4 | 10 test files, ~87 tests | README, ADR 0042 | No production deployment evidence in the repository; test provider refused in protected environments | No real carrier adapter |
| 11 | **Privacy Relay** | `services/privacy-relay` | Communication contexts, relay sessions, protected contacts (Contact Vault), temporary relay numbers and cooldown | communication_contexts, privacy_relay_sessions, protected_contacts, contact_fingerprints, relay_number_* | PostgreSQL, Calling, Messaging, Numbering, Developer Platform | `migration/` 17 | 40 test files, ~180 tests | README, ADRs 0010–0012 | NOT DEPLOYED to production | Needs carrier and real numbers |
| 12 | **Security Scan Gateway** | `services/security-scan-gateway` | Shared authenticated malware-scan service (ClamAV) issuing signed, bound verdicts | stateless (in-memory replay cache) | ClamAV engines | — | 1 test file, ~20 tests | README, operations/shared-security-scanning.md | PRODUCTION (`scan-gateway-verify` PASS) | Single-instance replay cache |

| Package / app | Path | Role |
|---|---|---|
| contracts | `packages/contracts` | OpenAPI and JSON-Schema authority; `bundled/` Swagger bundles; manifest + validator |
| service-auth | `packages/service-auth` | Signed internal service-to-service authentication (ADR 0004) |
| messaging-crypto | `packages/messaging-crypto` | MLS / E2EE client core (Rust → WASM / FFI) |
| object-storage | `packages/object-storage` | Protected object storage and presigning (ADR 0023) |
| webhooks | `packages/webhooks` | Webhook signing and secret box |
| usage-facts | `packages/usage-facts` | Usage fact model shared by producers and Developer Platform |
| communications-policy | `packages/communications-policy` | Shared communications policy evaluation |
| scan-gateway-contract | `packages/scan-gateway-contract` | Scan verdict contract |
| SDKs | `sdks/typescript`, `sdks/dart` | Generated from the public gateway contracts (572 operations each); not used by either first-party app |
| Web app | `apps/web` | Next.js client: meetings v2, conference room, messaging (MANAGED + E2EE client), calling, organizations page, settings |
| Mobile app | `apps/mobile` | Expo / React Native client: auth and a legacy meeting flow (largely broken, see MTG-001) |
| Certification | `certification/` | Cross-service certification harness (Rust) |
| Infrastructure | `infra/` | API gateway manifests, deployment scripts, backup/restore, LiveKit, telephony edge, TLS |
| CI/CD | `.github/workflows` (75 workflows) | Per-service CI, certification, production release/verify workflows (deployments are GitHub Actions only) |
