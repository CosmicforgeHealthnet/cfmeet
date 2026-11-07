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
| 0 | 2 Account security, backups | 28 (18 complete, 8 partial) | `/api/v1/auth/*`, session bridge, Hub fan-out | `GET /api/v2/me/sessions`, `POST …/sessions/{id}/revoke`, `POST …/sessions/revoke-all`, `POST /api/v1/auth/oauth/google/exchange`, `GET /api/v2/admin/audit-events` | login, refresh, logout, recovery endpoints, OAuth callback | `account_security_migrations` history (state, sessions, audit, trigger, email index) | meeting-service, infra, certification | meeting-service 42308337872; comms-hub 42306789245; Part VI cert 42306794069; backup drill 42307503831; calling/messaging/numbering/relay/telephony/sms DR | see §5 | — | off-host backup target + age key (owner); frontend adoption of OAuth exchange | ENGINEERING_COMPLETE (partials listed) |
| 0 | 3 Authority & contract freeze | 4 | ADR 0004 service auth | contract `cfmeet.organizations.internal-api` 1.0.0 | service-auth: identity + 5 permissions | — | architecture | contracts 42315715205 / 42323140732 | a4d255bf | — | — | ENGINEERING_COMPLETE |
| I | 4 Organizations Service + migration | 1 (+ORG-001 family) | `/api/v2/me/organizations`, members routes (facades) | internal org API; `export_organizations` | legacy admin/member writes become facades in authoritative mode | new `organizations` DB (12 tables, own history) | organizations, meeting-service | organizations 42316792944; meeting-service 42323140454; Part VI cert 42323140460 | ab13bdbe | — | cutover: off-host backups, release, import, shadow zero-mismatch, owner approval | ENGINEERING_COMPLETE; cutover NOT started |
| I | 5 Lifecycle, invitations | 8 | members routes | 17 first-party organization routes (authoritative mode only) | — | invitations, audit, history (in organizations DB) | organizations, meeting-service | as Section 4 | ab13bdbe | — | invitation email = Section 11; web UI = Section 17 | ENGINEERING_COMPLETE (2 partial) |
| I | 6 Teams, roles, policy | 1 | — | teams, policies | — | teams, grants, policies | organizations | — | — | — | — | NOT_STARTED |
| I | 7 Provisioning | 2 | PA delegation | scopes resolve | — | scope mapping, saga | organizations, platform-access, calling | — | — | — | — | NOT_STARTED |
| I | 8 Workspace context | 0 | `/api/v2/me/organizations` rule, Organizations user-organizations / role-matrix / effective-policy reads, Hub personal capabilities (linked, not duplicated) | `GET /api/v2/me/workspaces`, `GET /api/v2/me/workspaces/{workspaceId}/capabilities` (v2 177 → 179) | relay allowlist +2 reads (`roles/capabilities`, `policies/effective`) | none | meeting-service | branch `blueprint/s08-workspaces` CI ([ops record](../operations/workspaces-section-8.md)) | — | — | — | IN_PROGRESS (built on branch; not merged to staging) |
| II | 9 Personal onboarding | 3 | Hub 73 ops | phone verification (gated) | profile | phone binding | meeting-service, communications | — | — | — | SMS provider | NOT_STARTED |
| II | 10 Meetings & calendar | 18 (7 complete, 5 partial; rest frontend) | v2 conferences/recurrence/invitations | `GET /api/v2/me/conferences`, 2× `calendar.ics` | reschedule, space list (paginated), creates (Idempotency-Key), recurrence edits, summary views | `meetings_calendar_migrations` (idempotency keys, agenda indexes) | meeting-service | meeting-service 42353311875; certification 42356374877 | 3620317a | — | — | ENGINEERING_COMPLETE (partials listed) |
| II | 11 Notifications | 2 (+2 external) | outboxes, Hub prefs | conditional | — | reminder jobs | meeting-service, Hub | — | — | — | push credentials | NOT_STARTED |
| III | 12 Calling | 19 (3 complete, 1 partial; externals gated) | `/v1/calls*`, `/v1/enterprise/*` | `GET /v1/enterprise/recordings` | auto-end, signaling revocation | none | calling | calling 42350889478; RTC 42350891700; enterprise 42350894085 | b4c9476a | — | PSTN, numbers, SMS (external); per-org scope after Section 7 | PARTIAL (lifecycle complete; per-organization enterprise scope pending Section 7 integration) |
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

### Section 2 — Account security, data protection and release readiness

**Verdict:** ENGINEERING_COMPLETE with listed partials; not deployed. Branch `blueprint/s02-account-security`, final CI-verified code commit `08298305`, promoted to `staging` by fast-forward.

Delivered: [ADR 0057](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md), [account-security-section-2.md](../operations/account-security-section-2.md) (behaviour, release prerequisites, verification), [backup-and-restore.md](../operations/backup-and-restore.md) (SEC-010/011 tooling, drill, owner actions).

CI evidence (all success): Meeting service [42308337872](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42308337872) (incl. 12 new account-security DB tests, bridge test, v2 operation coverage, migration smoke, OpenAPI parity, Docker); Communications Hub five-service stack [42306789245](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306789245); Part VI certification [42306794069](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306794069); Contracts [42306773987](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306773987); Service boundaries [42306778036](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306778036); API Gateway [42306785093](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306785093); SDK TypeScript [42306781536](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42306781536); Backup and restore drill [42307503831](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42307503831); service DR suites using the changed backup scripts: Calling 42307503651, Messaging 42307509370, Numbering 42307513884, Privacy Relay 42307519155, Telephony 42307523598, SMS Gateway 42307527751.

Partials and gates: per-address limits need `AUTH_CLIENT_IP_HEADER`; unique case-insensitive email index after the duplicate review; off-host target and age key custody (owner) — until then backups are LOCAL_ONLY; pre-migration snapshot to be inserted into release workflows at release review; `JWT_SECRET` length must be confirmed before the new image is deployed; OAuth exchange mode switched on after the frontend adopts it. Open in scope for later sections: ACC-008/010 (Section 14), ACC-012/019 (Section 17), PLT-002/DATA-003 (Section 16), SEC-015/016/017/018 (release hardening, Section 18), ACC-011/013 (owner decision D-6).

API delta (meeting-service): +5 operations (4 v2, 1 v1); before 219 → after 224 registered; all classified `EXCLUDED/FIRST_PARTY_APP_ACCOUNT` in the gateway manifest (first-party only).

### Sections 3–5 — Authority freeze, Organizations Service, lifecycle

**Verdict:** ENGINEERING_COMPLETE on `staging` (`a4d255bf` service, `ab13bdbe` meeting-service integration). **Not active for users:** meeting-service defaults to `ORGANIZATIONS_AUTHORITY_MODE=legacy`; the production cutover follows [organizations-cutover-runbook.md](../operations/organizations-cutover-runbook.md) and needs owner approval at each gate.

- Section 3: [internal contract](../api/organizations-internal-v1.md) (with §7 implementation clarifications and the shared export checksum vector), `packages/contracts/organizations/internal/v1.openapi.json`, service-auth identity `cfmeet.organizations` and permissions `organization.{read,write,act-for-user,administer,migrate}` (least-privilege grants asserted by tests), event catalogue.
- Section 4: `services/organizations` (own DB, runtime/migrator split, replay protection, outbox, audit, import CLI/route, reconciliation); meeting-service read-only `export_organizations`, legacy/shadow/authoritative modes, fail-closed directory, read-only projection, write facades, cutover runbook.
- Section 5: create, detail, profile edit, members, invitations (hashed, email-bound), accept/decline/revoke, leave, ownership transfer, suspend/reactivate/archive, audit read, provisioning read — internal API plus 17 first-party relay routes.

API delta: meeting-service v2 operations 157 → 174 (+17 relays, all EXCLUDED from the public gateway as `FIRST_PARTY_ORGANIZATIONS`, admin list OPERATOR_ADMIN); new Organizations internal API (see contract). Open: events have no delivery targets yet (Section 7); `organizations` backup catalog slot to be activated with drill fixtures before cutover; manager-facing member names (ORG-006).

### Section 10 — Meetings, scheduling and calendar

**Verdict:** ENGINEERING_COMPLETE for the backend scope (`3620317a`). Rules and client mapping: [meetings-calendar-section-10.md](../operations/meetings-calendar-section-10.md). New operations: agenda (`PUBLIC_CUSTOMER`), two ICS downloads (EXCLUDED `CALENDAR_FILE_DIRECT_INGRESS` — the gateway grammar has no file extensions). Meeting-service v2 operations 174 → 177; public gateway routes 572 → 574 together with Section 12. Remaining: MTG-006 reminders/notifications (Section 11), web display fixes MTG-004/FE-001 and forms (Section 17).

### Section 12 — Calling lifecycle (first pass)

**Verdict:** lifecycle ENGINEERING_COMPLETE (`b4c9476a`); the per-organization enterprise scope (CAL-007/ORG-012) waits for Section 7 provisioning. Auto-end rules (participation 60 s grace, idle 600 s, max 8 h; config-validated), reaper under row locks, no media-room resurrection, `GET /v1/enterprise/recordings` (Calling contract 1.15.0; operations 146 → 147), signaling sockets close (4401) on endpoint revocation. Client party rules: ADR 0009 amendment. Owner decisions left: CAL-018 decline-everywhere, CAL-019 attended transfer.
