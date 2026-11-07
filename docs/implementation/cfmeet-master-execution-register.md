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
| I | 6 Teams, roles, policy | 1 | Organizations internal API, role matrix | teams (5) + team members (4), `GET/PATCH …/policies`, `GET …/policies/effective`, `GET …/roles/capabilities` (internal) | resolve returns `teams[]`/`policyRevision`; evaluate takes `teamId` | Organizations migration 000002 (teams, team members + history, policies) | organizations | organizations 42360994274; certification 42360985275 | f4586b55 | — | — | ENGINEERING_COMPLETE (first-party relays for teams/policies are Section 8/17 consumers) |
| I | 7 Provisioning | 3 | PA tenant/application/environment model | PA `PUT/GET /internal/v1/organization-scopes/{organizationRef}/{capability}`, `POST …/status`; Organizations `POST …/scopes/resolve`, `POST /internal/v1/admin/organizations/{id}/provisioning/retry` | — | PA migration 000005 (`platform_organization_scopes`); Organizations 000002 scope sync columns | organizations, platform-access | platform-access 42360989040; organizations 42360994274; contracts 42360990612; boundaries 42360987229; developer-platform 42360992334; certification 42360985275 | f4586b55 | — | enable = release step (`ORGANIZATIONS_PROVISIONING_ENABLED=false` default; trust-bundle entry for Organizations) | ENGINEERING_COMPLETE (saga + PA delegation); Calling consumption = Section 12 follow-up |
| I | 8 Workspace context | 0 | `/api/v2/me/organizations` rule, Organizations user-organizations / role-matrix / effective-policy reads, Hub personal capabilities (linked, not duplicated) | `GET /api/v2/me/workspaces`, `GET /api/v2/me/workspaces/{workspaceId}/capabilities` | relay allowlist +2 reads (`roles/capabilities`, `policies/effective`) | none | meeting-service | meeting-service 42368605691; organizations 42368608014; certification 42368616582; boundaries 42368612983; gateway 42368619694; SDK TS 42368623856 / Dart 42368628223 (branch `01001824`) | e118c10e | — | — | ENGINEERING_COMPLETE (Meeting create/admit do not yet enforce organization policy in authoritative mode) |
| II | 9 Personal onboarding | 3 (+phone, external) | `/api/v1/users/me`, Hub profile projection through the account relay, Section 2 Google binding | `GET/PATCH /api/v2/me/profile` | v1 username rule, one v1 status rendering (`ACCOUNT_STATUS_FORMAT`, legacy default) | `profile_notifications_migrations` (nullable `users` profile columns, `email_verified_at`) | meeting-service | see §5 | — | — | SMS/verification provider (phone EXTERNAL_BLOCKED) | ENGINEERING_COMPLETE (backend; `deleted_at` D-6; avatar upload and client status constants Section 17) |
| II | 10 Meetings & calendar | 18 (7 complete, 5 partial; rest frontend) | v2 conferences/recurrence/invitations | `GET /api/v2/me/conferences`, 2× `calendar.ics` | reschedule, space list (paginated), creates (Idempotency-Key), recurrence edits, summary views | `meetings_calendar_migrations` (idempotency keys, agenda indexes) | meeting-service | meeting-service 42353311875; certification 42356374877 | 3620317a | — | — | ENGINEERING_COMPLETE (partials listed) |
| II | 11 Notifications | 2 (+2 external) | email queue/worker, Organizations relay, Hub preferences | Hub internal `POST /internal/v1/communications/notification-preferences/resolve` (`communications-preferences.read`) | invitation create (`sendEmail`/`emailDelivery`, meeting and organization), reschedule/cancel write notices | `profile_notifications_migrations` (`meeting_notifications`, recipient ledger) | meeting-service, communications | see §5 | — | — | APNs/FCM/Web Push credentials (push EXTERNAL_BLOCKED) | ENGINEERING_COMPLETE (backend, off by default: `NOTIFICATIONS_EMAIL_ENABLED=false`) |
| III | 12 Calling | 19 (3 complete, 1 partial; externals gated) | `/v1/calls*`, `/v1/enterprise/*` | `GET /v1/enterprise/recordings` | auto-end, signaling revocation | none | calling | calling 42350889478; RTC 42350891700; enterprise 42350894085 | b4c9476a | — | PSTN, numbers, SMS (external); per-org scope after Section 7 | PARTIAL (lifecycle complete; per-organization enterprise scope pending Section 7 integration) |
| III | 13 Messaging | 14 (6 complete, 2 partial; rest frontend / E2EE review) | messaging APIs, relay, account sessions | Meeting `PATCH /api/v2/messaging/relay/{path}`, `POST /api/v2/messaging/session-tokens`; Messaging `POST /v1/messaging/session-devices`, `…/revoke` (EXCLUDED first-party) | relay cursor/query parity, conversation list inbox summary (`orderBy`, title/avatar, unread/mention counts), send mentions, idempotency before rate limits | Messaging `m20261202_000030_session_devices_inbox` | messaging, meeting-service | messaging 42368522195; meeting-service 42368524444; certification 42368541566; Hub 42368539574; contracts 42372100330; boundaries 42372102371 (branch `4c257004`; Unified journeys only #37 Chromium) | 51c5903a | — | E2EE review #13; `MESSAGING_SESSION_DEVICES_ENABLED` at release | ENGINEERING_COMPLETE (backend; MSG-018 client work and organization conversations open) |
| III | 14 Hub & revocation | 9 (6 complete, 2 partial; frontend handoff) | Hub 73 ops, `/api/v2/me/sessions*`, account outbox, Hub outbox, PA `ensure` permission | PA `POST /internal/v1/principals/account-status` (internal); Hub event kind `ACCOUNT_SESSION_ENDED` | Hub capabilities (MANAGED), inbox (membership), timeline (late members), device link/revoke (session binding); Messaging capability `managed` | Hub third history `m20261108_000001_device_session_binding` | communications, meeting-service, messaging, platform-access, certification | Hub 42382649453 (incl. real-process Section 14 journey); meeting-service 42380694053; messaging 42380689605; platform-access 42383272508; calling 42381291169; contracts 42380696601; boundaries 42380698662; comms e2e 42381295760; SDK TS 42381300570 / Dart 42381302729; Unified journeys 42381298118 (only #37 Chromium) | — | — | deploy order PA + Hub before Meeting | ENGINEERING_COMPLETE (backend; Hub clients are Section 17) |
| IV | 15 Developer Platform & gateway | 10 (6 complete, 1 partial, 1 deprecated, 1 external, 1 frontend) | DP 126 ops, gateway 572 routes, PA tenant/audit APIs | PA `GET /internal/v1/delegation/tenant-slugs/{slug}`, tenant rename, `GET /internal/v1/admin/audit`; DP operator organization and activity reads | provisioning compensation + worker reconcile, credential last-use / `expiringSoon`, gateway scope pre-check (147 annotations), per-token rate limit (`API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE`, default 0 = off) | PA `m20261122_000006_credential_usage_tenant_rename`, DP `m20261202_000013_provisioning_name_sync_operator_audit` | developer-platform, platform-access, api-gateway, messaging, numbering, sms-gateway | DP 42382692657; PA 42382695273; gateway 42382697686; contracts 42382700368; boundaries 42382702983; SDK TS 42382705646 / Dart 42382707935; certification 42382710242; messaging 42382712633; numbering 42382715126; SMS 42382717312; meeting-service 42382719384 (branch `c53b6084`) | ffd4c4c5 | — | gateway/DP deployment (PLT-005, internal TLS), provisioning enablement, developer console UI (ORG-017) | ENGINEERING_COMPLETE (backend) |
| IV | 16 Consolidation & retirement | 15 | contract tooling, SDK generator | CI gates: gateway upstream handlers (INV-001), migration naming/order (DATA-014); Dart models | label/bundle/catalogue coverage 572 → 779 ops (PLT-007) | none | contracts, sdks | see §5 | 782c3c86, 90c2397e, f07b1b39, 1a767988, 781a5624 | — | consumer migration (PLT-003/MTG-017), frontend SDK adoption | IN_PROGRESS |
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
| 7 | meeting-service | agenda index `conference_sessions(scheduled_start_at)` (Section 10, `meetings_calendar_migrations`); profile columns + reminder/notification jobs (Sections 9/11, `profile_notifications_migrations`) | separate additive histories | — | LOW |
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

### Sections 6–7 — Teams, roles, policies and first-party provisioning

**Verdict:** ENGINEERING_COMPLETE on `staging` (`e5e96153`, merged at `f4586b55`). **Not active:** provisioning is off by default (`ORGANIZATIONS_PROVISIONING_ENABLED=false`, organizations stay PENDING). Decisions: [ADR 0058](../decisions/0058-first-party-organization-scopes-and-provisioning.md) (no `organization_role_grants` table — delegated-admin policy flags cover it; provisioning/scope events versioned per workflow and mapping).

- Section 6: teams with TEAM_MANAGER/TEAM_MEMBER (only org MANAGER+ can manage a team, only OWNER/ADMIN appoint, no self-escalation, ≤5-level hierarchy, append-only history, audit + outbox; organization removal ends team memberships); policy document with compare-and-swap `expectedRevision` that can only remove capabilities (never OWNER administration; delegated-admin section OWNER-only), `POLICY_RESTRICTED` on removed capabilities, effective constraints for owner services (stricter wins), legacy `profile_policy` carried as `legacyMeetingProfilePolicy`.
- Section 7: Platform Access permission `platform-identity.provision-first-party` (grantable only to `cfmeet.organizations`, sensitive, identity re-checked in handlers); idempotent FIRST_PARTY tenant + application + PRODUCTION environment per organization in one transaction (slug never taken over); Organizations saga PENDING → IN_PROGRESS → READY | FAILED (checkpoints, backoff ≤15 min, FAILED after 8 attempts), READY only after server-side confirmation, one mapping per tenant, suspension propagated with local fail-closed refusal, 6-hourly drift re-check, operator retry, scope resolve only for READY + applied tenant status. No credentials, service accounts or grants are created.
- Tests (CI): Organizations `teams` 4, `policies` 3 (full role × status × two-organization matrix), `provisioning` 7 (every listed failure mode against an in-memory PA); PA `organization_scopes` (idempotency incl. concurrency, caller restriction, isolation, slug); real-process certification journey `organizations_provision_first_party_scopes_through_the_real_platform_access`.

Release steps to enable (owner-gated): PA migration 000005, Organizations migration 000002, Organizations signing key in PA's trust bundle with the new permission, then `ORGANIZATIONS_PROVISIONING_ENABLED=true`. After drift/mismatch FAILED states an operator investigates before retry.

### Section 8 — Workspace context

**Verdict:** ENGINEERING_COMPLETE (`01001824`, merged at `e118c10e`). [Operations record](../operations/workspaces-section-8.md). Personal plus every ACTIVE membership, per-request capabilities with reasons (`ALLOWED`, `CAPABILITY_NOT_GRANTED`, `POLICY_RESTRICTED`, `ORGANIZATION_SUSPENDED`, `FEATURE_DISABLED`), same 404 for unknown/foreign ids, 503 when Organizations fails, no switch endpoint. Gap: in authoritative mode Meeting create/admit do not yet apply organization policy (`meetings.hostCreation`, `features.meetings`), so the read can report `POLICY_RESTRICTED` while Meeting still allows the action — tracked for the Section 12/10 policy-enforcement follow-up.

### Section 13 — Messaging

**Verdict:** backend ENGINEERING_COMPLETE (`4c257004`, merged at `51c5903a`). [Operations record](../operations/messaging-section-13.md); ADR 0049 Amendment A1 (MANAGED realtime through session devices: realtime-only, refused on PRIVATE_E2EE, bound to the account session, ≤300 s tokens renewed through Meeting, revoked with the session; production refusal of E2EE devices unchanged). Closed: MSG-001, MSG-002, MSG-003, MSG-004, MSG-007, MSG-019. Partial: MSG-018 (contract documents ack-cursor resume; web change and receipt coalescing open), MSG-015 (touched docs fixed). Release: `MESSAGING_SESSION_DEVICES_ENABLED` is not set by the production scripts; rollback needs `messaging-migration down 1`. Meeting v2 operations 179 → 181 after merging with Section 8.

### Section 15 — Developer Platform and API Gateway

**Verdict:** backend ENGINEERING_COMPLETE (`c53b6084`, merged at `ffd4c4c5`; all 12 workflows green on the branch SHA, merge tree identical).

- ORG-014: developer-organization provisioning keeps a provisioning row; a failed Platform Access step is compensated and the worker reconciles leftovers through PA `GET /internal/v1/delegation/tenant-slugs/{slug}`.
- ORG-013: PA tenant rename endpoint; Developer Platform syncs display names (sync flag recorded per organization).
- PLT-008 (partial): `platform_credential_usage` records last authentication; credential reads return `lastAuthenticatedAt` and `expiringSoon`. The expiry webhook is not built.
- PLT-009: PA `GET /internal/v1/admin/audit`, Developer Platform operator organization and activity reads, `devplat_operator_audit`; [operator/admin consolidation map](../operations/operator-admin-consolidation-map.md).
- PLT-011: 147 `x-cfmeet-required-scope` annotations (messaging contract 1.16.0, numbering 1.13.0, SMS gateway 1.3.0); [gateway scope pre-check](../api/gateway-scope-precheck.md). Messaging session-device operations are EXCLUDED from the gateway.
- PLT-012: optional per-token limit `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` (default 0, off); stale config comment fixed.
- PLT-004 / FE-013: DEPRECATED_WITH_MIGRATION — [legacy API key deprecation plan](../api/legacy-api-keys-deprecation.md); the web developer page change is frontend.
- PLT-005: EXTERNAL — [activation checklist](../operations/developer-platform-gateway-activation-checklist.md). Blocker: the configured Developer Platform URL is http, while service auth requires https; the internal-TLS decision and deployment approval belong to the owner. Nothing deployed.
- ORG-017: developer console UI handed to the frontend ([handoff](../phases/phase-06/developer-console-api-handoff.md)).

Release notes: both migrations are additive. The PA down migration refuses once a `TENANT_RENAMED` audit row exists (covered by `certification/tests/schema_upgrade.rs`). Staging CI for Sections 8 and 13 on `4b8f6a8b`: meeting-service 42376217380, certification 42376217511, contracts 42376217455, boundaries 42376217324.

### Sections 15–16 — items closed on staging by the lead

- PLT-001 (`c401d2ca`): the prose gate scope on `POST /v1/privacy-relay/contact-access-grants` is removed (Calling checks each requested scope); the generator rejects any gate scope outside the Platform Access registry (mutation-tested).
- INV-001 (`782c3c86`): `scripts/contracts/check_gateway_upstreams.py` proves every gateway route resolves to an upstream axum route or the Communications dispatch table; mutation tests; runs in Service boundaries (no path filter).
- PLT-007 (`90c2397e`, `f07b1b39`): Developer Platform / Gateway contracts labelled; bundles, label checks and the private catalogue cover 20 contracts (779 operations, was 572); generators refuse unknown arguments; new bundles registered. The meeting-service runtime tag patch is now a no-op and is removed with PLT-006.
- PLT-013 / MTG-020 (Dart part, `1a767988`): typed Dart models per public domain; the TS optional-body fix (`V2OptionalJson` operations) is pending in meeting-service utoipa.
- DATA-014 (`781a5624`): migration naming and append-order guard with a frozen baseline; [migration conventions](../architecture/migration-conventions.md) also record the DATA-011 target rule (code change pending).

### Section 14 — Communications Hub and revocation

**Verdict:** backend ENGINEERING_COMPLETE (branch `blueprint/s14-hub-revocation`, code `7489a566`). [Operations record](../operations/communications-hub-section-14.md); [ADR 0057 amendment A1](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md) (devices belong to sign-ins; account-wide ends reach every owner); [device and block authority decision](../architecture/communications-device-and-block-authority.md); [Hub client handoff §11](../api/communications-unified-handoff.md).

* FIXED: ACC-010 (Hub fans revocations out by principal without a profile; Account queues Messaging revoke-all on every credential end; Platform Access principal mirrors the current account status), ACC-008 (device ↔ account-session binding both ways; surface = existing sessions + devices APIs), MSG-016 (membership-filtered inbox; `block-events` deprecated), MSG-010 (real-process journey `hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`; it found and fixed a missing Calling launch credential in the personal stack, so Hub-launched calls had never run against real Calling in CI), Section 13 follow-up (MANAGED capability with session-device realtime).
* PARTIAL: DATA-005 / DATA-006 (authority decided and safe read paths in place; deprecated stores removed only by later contract / legacy-retirement steps), MSG-009 (late-member meeting cards; chat lines client-composed by decision; no realtime nudge, no call cards for members without a profile at launch).
* REUSED + handoff: MSG-006 (+ACC-009, FE-020).

Contracts: communications-internal 1.3.0, communications http/public/swagger 1.2.0, messaging communications-internal 1.2.0, platform-access internal-api 1.8.0, identity 1.4.0 (bundles and SDKs regenerated). Meeting v2 operations unchanged (181). Release: PA and Hub (migration first) before Meeting; no new configuration or trust-bundle grant.

### Sections 9 and 11 — Personal profile and notifications

**Verdict:** backend ENGINEERING_COMPLETE on branch `blueprint/s09-s11-profile-notifications`; nothing enabled. [Operations record](../operations/profile-notifications-sections-9-11.md). Section 9: the Account owns the profile (display name, avatar reference, time zone, locale, `email_verified_at`; one validation rule set; `GET/PATCH /api/v2/me/profile`, EXCLUDED first-party); the Hub profile is a projection refreshed by the existing relay; one email-verification rule and presentation name; one v1 status rendering behind `ACCOUNT_STATUS_FORMAT` (legacy default). Section 11: meeting and organization invitation emails on request (token only in the URL fragment), durable reschedule/cancel notices and reminders over the existing email queue with a per-recipient digest ledger, Hub `meetingInvitations` preference enforced through a new Meeting-only private read (Communications internal contract 1.4.0, after Section 14's 1.3.0). Closed: ACC-021, ACC-022 (switch gated), DATA-016 (`deleted_at` D-6), MTG-006. Partial: ACC-020. EXTERNAL_BLOCKED: phone verification, CAL-003, MSG-011. Release: `profile-notifications-migration up` after meetings calendar; grant `communications-preferences.read` to the Meeting Service in the Hub trust bundle before enabling `NOTIFICATIONS_EMAIL_ENABLED`. Meeting v2 operations 181 → 183. CI evidence: recorded with the merge.

### Section 10 — Meetings, scheduling and calendar

**Verdict:** ENGINEERING_COMPLETE for the backend scope (`3620317a`). Rules and client mapping: [meetings-calendar-section-10.md](../operations/meetings-calendar-section-10.md). New operations: agenda (`PUBLIC_CUSTOMER`), two ICS downloads (EXCLUDED `CALENDAR_FILE_DIRECT_INGRESS` — the gateway grammar has no file extensions). Meeting-service v2 operations 174 → 177; public gateway routes 572 → 574 together with Section 12. Remaining: MTG-006 reminders/notifications (Section 11), web display fixes MTG-004/FE-001 and forms (Section 17).

### Section 12 — Calling lifecycle (first pass)

**Verdict:** lifecycle ENGINEERING_COMPLETE (`b4c9476a`); the per-organization enterprise scope (CAL-007/ORG-012) waits for Section 7 provisioning. Auto-end rules (participation 60 s grace, idle 600 s, max 8 h; config-validated), reaper under row locks, no media-room resurrection, `GET /v1/enterprise/recordings` (Calling contract 1.15.0; operations 146 → 147), signaling sockets close (4401) on endpoint revocation. Client party rules: ADR 0009 amendment. Owner decisions left: CAL-018 decline-everywhere, CAL-019 attended transfer.
