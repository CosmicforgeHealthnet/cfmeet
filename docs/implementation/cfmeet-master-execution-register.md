# CF Meet — master execution register (Blueprint v1.0)

Single register for the consolidated platform programme: one row per blueprint section, the release gates, the migration dependency map and the per-section records. Status words follow the blueprint: `ENGINEERING_COMPLETE`, `STAGING_CERTIFIED`, `PRODUCTION_VERIFIED`, `PUBLICLY_ACTIVE`, plus `IN_PROGRESS`, `NOT_STARTED`, `EXTERNAL_BLOCKED`, `DEFERRED_BY_OWNER`. A passed build is never reported as a deployment.

## 0. Baseline (Section 1 kickoff, 2026-11-06)

| Item | Value |
|---|---|
| `origin/main` | `586b88cb121d07ac0821f5d334820bae3868f6c2` (audit baseline; Sections 8–23 closure) |
| `origin/staging` | `0587b37a189b891d3a0b0d3d5851817bc1622f74` (= main + 10 audit documents) |
| Code drift since audit | none: `git diff 586b88cb 0587b37a` changes only `docs/audits/*` |
| Registered operations | 1,066 (inventory CSV re-read: 650 public, 166 internal, 94 delegated mirror, 100 admin/operator, 42 infrastructure, 9 provider callbacks, 5 WebSocket) — exact because no router changed |
| Audit findings | 182 register rows → 149 canonical + 1 informational (P0 4 · P1 48 · P2 66 · P3 31), all carried in [`audit-findings-traceability.csv`](audit-findings-traceability.csv) |
| Push to `staging` | runs CI only; every deploy workflow triggers on `main` or manual dispatch (checked in `.github/workflows/*`) |
| Production | untouched by this programme until a separately approved release gate |

## 1. Section register (blueprint Appendix F)

| Phase | Section | Findings (canonical) | Existing APIs reused | New APIs added | Modified APIs | Data migrations | Integration owners | Tests passed | Staging SHA | Production SHA | External blockers | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | 1 Baseline, ADRs, contracts | all 149 traced | inventory re-used | 0 | 0 | 0 | architecture | traceability generator self-checks (counts reconcile) | see §5 | — | — | ENGINEERING_COMPLETE (documentation) |
| 0 | 2 Account security, backups | 28 | `/api/v1/auth/*`, session bridge | planned: sessions, OAuth exchange, admin audit | auth flows | planned: account security epoch, sessions, audit, google_sub | meeting-service, infra | — | — | — | off-host backup target (owner choice) | NOT_STARTED |
| 0 | 3 Authority & contract freeze | 4 | — | — | — | — | architecture | — | — | — | — | NOT_STARTED |
| I | 4 Organizations Service + migration | 1 | `/api/v2/me/organizations` (facade) | internal org API | — | new DB | organizations, meeting-service | — | — | — | cutover needs §2 backups + release approval | NOT_STARTED |
| I | 5 Lifecycle, invitations | 8 | members routes (facade) | org lifecycle family | members | invitations, audit | organizations | — | — | — | — | NOT_STARTED |
| I | 6 Teams, roles, policy | 1 | — | teams, policies | — | teams, grants, policies | organizations | — | — | — | — | NOT_STARTED |
| I | 7 Provisioning | 2 | PA delegation | scopes resolve | — | scope mapping, saga | organizations, platform-access, calling | — | — | — | — | NOT_STARTED |
| I | 8 Workspace context | 0 | — | `/me/workspaces*` | — | — | organizations, meeting-service | — | — | — | — | NOT_STARTED |
| II | 9 Personal onboarding | 3 | Hub 73 ops | phone verification (gated) | profile | phone binding | meeting-service, communications | — | — | — | SMS provider | NOT_STARTED |
| II | 10 Meetings & calendar | 18 | v2 conferences/recurrence | agenda, ICS | reschedule/cancel | indexes | meeting-service | — | — | — | — | NOT_STARTED |
| II | 11 Notifications | 2 (+2 external) | outboxes, Hub prefs | conditional | — | reminder jobs | meeting-service, Hub | — | — | — | push credentials | NOT_STARTED |
| III | 12 Calling | 19 | `/v1/calls*`, `/v1/enterprise/*` | recordings list | lifecycle | — | calling | — | — | — | PSTN, numbers, SMS | NOT_STARTED |
| III | 13 Messaging | 14 | messaging APIs | — | relay PATCH/query/cursor | — | messaging, meeting-service | — | — | — | E2EE review #13 | NOT_STARTED |
| III | 14 Hub & revocation | 9 | Hub 73 ops | fan-out (internal) | context-aware reads | — | communications | — | — | — | — | NOT_STARTED |
| IV | 15 Developer Platform & gateway | 10 | DP 126 ops, gateway 572 routes | 0 | scope fix | — | developer-platform, api-gateway | — | — | — | deploy approval | NOT_STARTED |
| IV | 16 Consolidation & retirement | 15 | — | — | deprecations | — | contracts, sdks | — | — | — | consumer migration | NOT_STARTED |
| V | 17 Frontend handoff | 14 | — | — | — | — | apps/web, apps/mobile | — | — | — | — | NOT_STARTED |
| V | 18 Certification | 2 | — | — | — | — | certification | — | — | — | physical devices | NOT_STARTED |

Finding counts are canonical findings per section from the traceability CSV (column `blueprint_section`).

## 2. Owner decisions settled by the blueprint (audit implementation plan §0)

| Audit decision | Settled by blueprint as | Where |
|---|---|---|
| D-1 organization authority | New Organizations Service, migrated from meeting-service; DP and PA stay separate | ADR 0056 |
| D-2 who creates organizations | Governed self-service creation (rate limits, idempotency, abuse checks) | Section 5 |
| D-3 calendar scope | In-app agenda + ICS export; Google/Outlook sync is a later optional integration | Section 10 |
| D-4 notification channels | Email now; push built provider-neutral, reported NOT_CONFIGURED until credentials exist | Section 11 |
| D-5 MANAGED realtime without E2EE device | Allowed via a separately controlled session device, after an ADR amendment | Section 13 |
| D-7 Part VI rollout | Controlled deployment through GitHub workflows after Section 2 bridge hardening | Section 15 |
| D-8 mobile scope | Full Personal + Organization parity; admin tools web-first | Section 17 |
| D-9 legacy retirement | Only after measured zero consumers, per-endpoint record | Section 16 |
| **D-6 deletion/export/MFA** | **Not settled.** ACC-011 and ACC-013 stay `DEFERRED_BY_OWNER (proposed)` until the owner decides | traceability CSV |
| Off-host backup storage target | **Not settled.** Blueprint requires encrypted off-host copies (no AWS by default); the target and its credentials are an owner input | Section 2 |

## 3. Release gates (checked before any `main`/production promotion)

1. Section verdict `ENGINEERING_COMPLETE` with CI run links for every touched workflow on the exact commit.
2. No implementable P0/P1 finding owned by the section left OPEN in the traceability CSV.
3. Contracts regenerated (OpenAPI export, bundled Swagger, TS/Dart SDK `--check`), gateway manifest parity, service-boundary check green.
4. For data migrations: additive or expand/contract only; migration smoke in CI; pre-migration snapshot step present in the release workflow; restore drill receipt for the affected database.
5. For authority moves (Section 4 cutover): off-host encrypted backup + isolated restore receipt; shadow-read comparison with zero permission differences; written rollback that cannot produce two writers.
6. Negative authorization tests for every new boundary (cross-organization, forged ids, revoked sessions, suspended organization).
7. External gates reported truthfully (`NOT_CONFIGURED`, `PROVIDER_BLOCKED`, `BLOCKED_EXTERNAL`).
8. Explicit release approval from the owner for `main` and production. Standing VPS rules apply (no test emails through production SMTP, no load tests, no restarts of unrelated containers, never print secrets).

## 4. Migration dependency map

| Order | Database | Migration (planned) | History table | Depends on | Risk |
|---|---|---|---|---|---|
| 1 | meeting-service | account security: `users.credentials_not_before`, token sessions, `account_security_events`, `users.google_sub` (unique, nullable), email normalization index **only after duplicate detection** | base `seaql_migrations` (must exist in default test DBs; today the epoch lives only in the separate Communications history) | — | MEDIUM (email index needs a duplicate report first) |
| 2 | organizations (new) | schema: organizations, profiles, memberships, invitations, audit, outbox | own history | 1 (actor epoch) | LOW (new DB) |
| 3 | organizations | import of meeting-service organizations/memberships (ids preserved) | data job, idempotent | 2; production run needs backups + approval | HIGH at cutover |
| 4 | meeting-service | organization projection + mutation guard flag | base | 3 | MEDIUM |
| 5 | organizations | teams, grants, policies, scope mappings, provisioning | own history | 2 | LOW |
| 6 | platform-access | FIRST_PARTY tenant delegation path if missing | PA history | ADR 0056 §4 validation | MEDIUM |
| 7 | meeting-service | agenda index `conference_sessions(scheduled_start_at)`, reminder jobs | base | — | LOW |
| 8 | messaging / calling | none planned beyond owner-local changes found in Sections 12–13 | owner histories | — | — |

## 5. Section records

### Section 1 — Baseline reconciliation, ADRs and contract proposals

**Verdict:** ENGINEERING_COMPLETE (documentation and contract proposals only, as the blueprint's release rule requires).

Delivered:
- [`audit-findings-traceability.csv`](audit-findings-traceability.csv): all 182 register rows; 149 canonical findings + 1 informational each with blueprint section, accountable workstream, proposed disposition, verification class, external/owner gate and closure-blocking flag; merged duplicates point to their canonical id. Reconciles to the audit exactly (P0 4, P1 48, P2 66, P3 31). 48 canonical P0/P1 findings are implementable closure blockers; 4 P0/P1 are gated (MSG-013, CAL-003 external; ACC-011, ACC-013 owner decision).
- [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md): organization authority, developer/technical separation, human identity and ingress pattern, technical-scope cardinality, workspace context, events and fail-closed rules, calendar/contacts/notification ownership, phone/realtime/external gates, backups-before-migration, compatibility.
- [`../architecture/cfmeet-target-authority-map.md`](../architecture/cfmeet-target-authority-map.md): single writer per entity, context flow, current → target gaps.
- [`../api/proposed-v2-delta.md`](../api/proposed-v2-delta.md): every proposed operation checked against the 1,066-operation inventory. Exact matches only for the two routes reused as facades (`GET /api/v2/me/organizations`, `GET /api/v2/organizations/{id}/members`); Developer Platform `/v1/organizations*` similarities recorded as a different authority.
- [`../events/organization-event-contracts.md`](../events/organization-event-contracts.md): envelope and event catalogue (frozen in Section 3).
- This register: section ledger, settled and open owner decisions, release gates, migration dependency map.

Reused: the 10 audit artefacts, the inventory CSV (still exact), ADR 0004/0005/0018/0032/0055, `service-data-ownership.md`, the boundary checker.
Not done in Section 1 (by design): handlers, migrations, contract files. Route recount is unchanged because no router changed; the count is recomputed from source at the end of each implementing section.
