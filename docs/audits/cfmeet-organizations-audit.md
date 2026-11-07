# CF Meet — Organization Assessment

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.

## Lead-auditor verification and cross-checks

- **ORG-003 (why creation metadata is missing): verified at the API layer.** `services/meeting-service/src/handlers/v2/organizations.rs:15-23` builds `OrganizationResponse` from `id, name, slug, status, created_at` only. `created_by` is stored but never serialized. `created_at` is serialized but the web page does not render it. Location and creation source are not stored anywhere.
- **ORG-002: verified.** `services/meeting-service/src/services/organizations.rs:96-102` (`require_platform_admin`) refuses organization creation unless `actor.platform_admin`.
- **ORG-004: independently confirmed from the route inventory.** Out of 1,066 registered operations, meeting-service exposes exactly five organization routes:
  - `GET /api/v2/me/organizations` (gateway)
  - `GET /api/v2/organizations/:id/members`
  - `PUT /api/v2/organizations/:id/members/:user_id`
  - admin `POST /api/v2/admin/organizations`
  - admin `PATCH /api/v2/admin/organizations/:id`

  There is **no `GET /api/v2/organizations/:id`**, no profile edit, no invitation, no ownership transfer and no archive/delete route. By contrast, the Developer Platform exposes 22 public `/v1/organizations*` and `/v1/invitations/accept` operations: create, read, PATCH, close, ownership transfer, invitations, members, activity and policies. Platform Access exposes tenant creation and status as internal/admin operations. This route-level asymmetry is the core of ORG-001 and ORG-004.
- **Legal incorporation date vs workspace creation date.** `organizations.created_at` (meeting-service) and `devplat_organizations.created_at` record when the CF Meet workspace was created. No service models a legal incorporation date, registered address or operating location. The only legal-entity evidence in the platform is Numbering KYC documents.

### Severity reconciliation in the master register
- **ORG-001** — also reported as PLT-010, DATA-004
- **ORG-010** — consolidated into **MTG-016** in the register
- **ORG-012** — consolidated into **CAL-007** in the register



Audit-only. Repository @ `586b88cb` (staging == origin/main). No repo file was changed.
Tests were **not executed locally** (no Postgres server running; building would write into the repo
`target/`). "CI DB test" below means: the test exists, uses a real PostgreSQL (`TEST_DATABASE_URL`
set in `.github/workflows/backend.yml:93-132` / `developer-platform.yml:89-102`), and the Backend
workflow concluded `success` on `main` e717d018 (2026-11-06) per `gh run list` (read-only). The
audited commit 586b88cb itself only has a "Service boundaries" run recorded → treat CI evidence as
strong but REQUIRES_VALIDATION for this exact SHA.

## Scope inspected

- **meeting-service**: `migration/src/m20261004_000001_organizations.rs` (whole), `m20261005_000005_artifacts_profiles_operations.rs:163` (profile_policy), `m20261101_000001_platform_environment_meetings.rs` (platform_tenant_id on spaces), `src/models/organizations.rs`, `src/models/organization_memberships.rs`, `src/handlers/v2/organizations.rs` (whole), `src/services/organizations.rs` (whole), `src/dto/v2.rs:808-861`, `src/routes/v2.rs:490-507`, `src/routes/admin.rs` (no org routes), `src/services/actor.rs:170-205,300-345`, `src/services/meeting_space.rs:150-176,300-315,452,616-660,955-975`, `src/services/meeting_access.rs:313-345`, `src/services/collaboration/prejoin.rs:228-250`, `src/services/unified_messaging/relay.rs:668-717`, `src/services/communications.rs:405-440`, `src/handlers/v2/views.rs:40-62`, `src/swagger/mod.rs:195-199,465-472`, `tests/organization_domain_access.rs` (org01-org03, dom01-02), `tests/common/mod.rs` (DB harness).
- **developer-platform**: `migration/src/m20261102_000002_control_plane.rs:1-190,300-330`, `src/control/organizations.rs` (whole), `src/control/members.rs` (views, invitation, last-owner), `src/control/mod.rs:200-295`, `src/domain/rbac.rs:160-260`, `src/http/public.rs:40-120,400-540`, `src/http/operator.rs:25-45`, `src/platform_access/mod.rs:149-185`, `tests/control_plane.rs` (test list), `tests/common/mod.rs:262-370` (FakePlatformAccess).
- **platform-access**: `migration/src/m20261016_000001_identity_hierarchy.rs:104-124` (platform_tenants), `src/http/delegation.rs:1-20,82-290`.
- **calling enterprise**: `migration/src/m20261009_000020_enterprise_foundation.rs:1-20,49-200`, `m20261009_000021_enterprise_extensions.rs`, `src/enterprise/http.rs:1-120,356-420`, tests `enterprise_certification.rs`, `enterprise_extensions.rs`, `enterprise_queues.rs`, `enterprise_ring_groups.rs` (function lists / key test).
- **numbering**: `m20261016_000002_number_domain.rs:640-720` (dcn_assignments owner_kind ORGANIZATION), `m20261018_000004_kyc_eligibility.rs:128-167,250-256`, `src/acquisition/assignments.rs:178-190`.
- **messaging**: migrations (tenant/app/env scoping only, no org table). **communications**: `src/owners.rs`, migration (no org concept).
- **docs**: ADR 0018 (§"organization boundary is the platform tenant" L47-56), ADR 0032 (whole), ADR 0055 (no org/meeting-org statement), `architecture/service-data-ownership.md:19-30,40-50,95-110`, `architecture/platform-boundaries.md:46`, `phases/phase-06/developer-console-api-handoff.md:1-75`, PDR PDF (text-extracted: §4 routes L170-190, F-02/F-03 L205-220, D-07 L803, A.6 L1029-1043, A.32 L1975-1982).
- **gateway/contracts/SDK**: `infra/api-gateway/public-routes.json` (L6 reason, L2172, L2440, L2447), `route-manifest.json`, `packages/contracts/openapi/meeting-service.json`, `bundled/public/meetings/v1.openapi.json`, `sdks/typescript/src/generated/operations.ts:239-305,383`, `sdks/dart/lib/src/generated/operations.dart:351`.
- **web**: `src/services/v2/organizations.service.ts`, `src/hooks/v2/useOrganizations.ts`, `src/types/v2/index.ts:77-81,1499-1535`, `src/components/dashboard/OrganizationsContent.tsx` (whole), `src/app/dashboard/organizations/page.tsx`, `src/lib/dashboard-navigation.ts:6`, `src/components/meeting-spaces/CreateMeetingSpaceContent.tsx:20,50-57,124-133`, `src/components/dashboard/EditMeetingContent.tsx:38-40`, `src/components/messaging/MessagesContent.tsx:187`, `src/app/api/calling/[...path]/route.ts` (whole), `src/lib/calling/server.ts:20-108`, `src/lib/calling/resource-routes.ts`, `src/components/calling/CallsContent.tsx:182`, `tests/invitations-participants-v2.test.mjs:67-77`.
- **mobile**: grep of `apps/mobile` (only `features/meetings/v2/types.ts:19,146,183`, `components/space-card.tsx:44,57`).

### Organization / tenant concepts identified

| # | Concept | Table(s) & key columns | Owner service | APIs | Relationship |
|---|---|---|---|---|---|
| 1 | **Meeting organization** ("CF account organization") | `organizations(id, name ≤200, slug UNIQUE, status ACTIVE/SUSPENDED, created_by→users, created_at, updated_at, profile_policy JSONB)`; `organization_memberships(org, user, role OWNER/ADMIN/MEMBER, status ACTIVE/SUSPENDED/REMOVED, created_by, created_at, updated_at, removed_at)` — `m20261004_000001_organizations.rs:14-41` | meeting-service | `POST /api/v2/admin/organizations`, `PATCH /api/v2/admin/organizations/{id}`, `GET /api/v2/organizations/{id}/members`, `PUT /api/v2/organizations/{id}/members/{userId}`, `GET /api/v2/me/organizations` | Authority for ORGANIZATION_ONLY meetings, meeting-space `organization_id` FK, messaging colleague directory, pre-join org name. Not linked to any platform tenant. |
| 2 | **Developer organization / workspace** | `devplat_organizations(tenant_id PK, display_name ≤120, state INVITED/ACTIVE/RESTRICTED/SUSPENDED/CLOSED, state_reason, support_email, technical_contact_member_id, created_by VARCHAR actor ref, created_at, updated_at, closed_at, version)`; `devplat_workspace_members(role OWNER/ADMIN/DEVELOPER/ANALYST/BILLING, status ACTIVE/REMOVED, joined_via CREATOR/INVITATION/OPERATOR)`; `devplat_invitations` (sha-256 token, email-bound, 7-day TTL) — `m20261102_000002_control_plane.rs:30-140` | developer-platform | `/v1/organizations/**` (22 public ops) + `/operator/organizations/**` | A **profile + lifecycle projection keyed 1:1 to a Platform Access tenant** (comment L3: "tenant = organization"). Migration comment L10-12: members are "not Meeting Service organization members". |
| 3 | **Platform tenant** | `platform_tenants(id, slug, display_name ≤200, classification FIRST_PARTY/EXTERNAL, status ACTIVE/SUSPENDED, billing_currency, revision, created_at, updated_at)` — `m20261016_000001_identity_hierarchy.rs:104-124` | platform-access (ADR 0032) | internal `/internal/v1`, delegation `POST /delegation/tenants`, `GET /delegation/tenants/{id}`, `POST …/status` (`delegation.rs:82-90`) | Identity authority. Calling holds a read-only projection (ADR 0032 §6). devplat org = delegated EXTERNAL tenant. |
| 4 | **Enterprise calling organization** | No own table: every `enterprise_*` row carries `(tenant_id, application_id, environment_id)` FK to `platform_environments` (`m20261009_000020_enterprise_foundation.rs:1-6`); "members" are platform **principals** (department/ring-group/queue members) | calling | `/v1/enterprise/**` (`enterprise/http.rs:57-230`), platform-credential + `enterprise.config.*` scopes | ADR 0018 L47-56: "The platform tenant / application / environment triple is the organization security boundary … No overlapping organization identity is created." Separate from #1. |
| 5 | **Numbering organizational owner / KYC subject** | `dcn_assignments.owner_kind IN ('PRINCIPAL','ORGANIZATION')`, `owner_ref` = authenticated `tenant_id` (`assignments.rs:184`); `number_kyc_cases.subject_kind IN ('INDIVIDUAL','ORGANIZATION')` with evidence types incl. `BUSINESS_REGISTRATION`, `ADDRESS` (`kyc_eligibility.rs:163,254-256`) | numbering | `/v1/numbers/**` (customer), KYC endpoints | Organization = platform tenant (#3). The **only** place legal-entity evidence (business registration, address, country) is modelled — per KYC case, not as an org profile. |
| 6 | Messaging / Communications scoping | `messaging_*` tables keyed `(tenant_id, application_id, environment_id)`; communications hub has no org table | messaging / communications | — | Use the platform tenant triple. In the CF Meet app, the *colleague directory* is computed by meeting-service from concept #1 (`relay.rs:670-717`). |
| 7 | Meeting space platform scope | `meeting_spaces.platform_tenant_id/application_id/environment_id` (`m20261101_000001…:35-45`) alongside `meeting_spaces.organization_id` (FK → #1) | meeting-service | machine-caller `/api/v2/meeting-spaces` | Same table carries two independent org axes (#1 and #3) with no consistency rule between them. |

Conclusion: **three distinct organization authorities** (#1 meeting org, #2/#3 developer org = platform tenant, #4 enterprise = tenant triple) and **two independent membership/role systems** (#1 OWNER/ADMIN/MEMBER over CF Meet users; #2 OWNER/ADMIN/DEVELOPER/ANALYST/BILLING over CF Meet users; plus #4 principal-based enterprise membership). No mapping table, link column or ADR reconciles #1 with #2/#3/#4. The only explicit statements are negative (devplat migration L10-12; `public-routes.json:6` "never the developer organization authority"; ADR 0018 L55).

## Feature matrix

Abbreviations: MS = meeting-service, DP = developer-platform, PA = platform-access. "CI DB test" as defined above.

| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present / missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Org creation (MS) | Create org; PDR A.32: platform-admin "privileged engineering tools, do not expose in public meeting frontend" | `services/organizations.rs:144-186`; admin gate `:96-102,151-152` | `POST /api/v2/admin/organizations` | self-service create (if product wants it); admin UI | name, slug, status, created_by, created_at present | **None** — `organizationsService.create` defined (`organizations.service.ts:24-30`) but never called; no `/admin` app route exists | org03 asserts non-admin → 403 (`organization_domain_access.rs:211-222`) — CI DB test | PARTIALLY_IMPLEMENTED (API only, no tooling) | Product decision D-07; at minimum ship an operator/admin tool |
| Org creation (DP) | Any signed-in human creates a workspace & becomes OWNER | `control/organizations.rs:83-149` (delegates EXTERNAL tenant to PA, then one txn: org row + OWNER member + activity) | `POST /v1/organizations` (idempotent `public.rs:401-422`) | — | created_by (actor ref), created_at present | **None** (console deferred: `developer-console-api-handoff.md:3-4`) | `control_plane.rs:62,136` real DB, **fake PA** | IMPLEMENTED_UNVERIFIED (cross-service) | Decide if this is "the" organization (ORG-001) |
| Org profile retrieval (MS) | View org details | none for single org; list only `services/organizations.rs:318-341` | `GET /api/v2/me/organizations` | `GET /api/v2/organizations/{id}`, admin list-all | — | `OrganizationsContent.tsx:107-118` (name, slug, status, role) | org01 asserts list (CI DB) | PARTIALLY_IMPLEMENTED | Add GET detail (ORG-004) |
| Org profile retrieval (DP) | | `control/organizations.rs:209-215` (+PA classification) | `GET /v1/organizations[/{id}]` | — | | none | DB test, fake PA | IMPLEMENTED_UNVERIFIED | — |
| created_at | Workspace creation timestamp | MS model `organizations.rs:17`; handler maps it `handlers/v2/organizations.rs:21` | in `OrganizationResponse.createdAt` (`dto/v2.rs:841`), Swagger schema (`openapi/meeting-service.json` OrganizationResponse required) | — | `created_at TIMESTAMPTZ NOT NULL DEFAULT now()` (`m20261004…:20`) | web type has it (`types/v2/index.ts:1519`) but **not rendered** (`OrganizationsContent.tsx:114-118`) | not asserted by any test | FRONTEND_ONLY_GAP | Render as "Created on CF Meet" (ORG-003) |
| created_by | Creator identity | MS: stored `services/organizations.rs:165`; DP: stored `control/organizations.rs:116` | **not exposed** (MS `handlers/v2/organizations.rs:15-23`; DP `ORG_VIEW` L28-32 omits it) | expose `createdBy` (+ display name) | MS `created_by UUID → users ON DELETE SET NULL` (L19); DP `created_by VARCHAR(100)` | none | none | PARTIALLY_IMPLEMENTED (storage only) | Add to DTO + resolve name (ORG-003) |
| Creation source | Self-service vs operator vs import | — | — | — | MS: **missing**; DP: `joined_via` on member only; activity event `organization.created` | none | — | MISSING | Add `creation_source` if required |
| Registered / operating location | Country/region/address | — | — | all | **missing in every org table** (MS, DP, PA). Only `devplat_billing_accounts.country` (billing) and numbering KYC evidence `ADDRESS` | none | — | MISSING | Data-model change (ORG-003) |
| Legal incorporation date | Legal entity date | — | — | all | **missing** everywhere (KYC evidence `BUSINESS_REGISTRATION` is a document type, not a field) | none | — | MISSING / PRODUCT_DECISION | Do not conflate with created_at |
| Organization type | e.g. healthcare/business | — | — | all | missing (space-level `security_profile` exists, not org) | none | — | MISSING | Product decision |
| Description | | — | — | all | missing (MS & DP org; DP project has description) | none | — | MISSING | Add column + PATCH |
| Status | ACTIVE/SUSPENDED (MS); 5-state (DP) | MS `set_status` `services/organizations.rs:221-236`; DP `close` L273-314, operator L319-391 | `PATCH /api/v2/admin/organizations/{id}`; `POST /v1/organizations/{id}/close`; `POST /operator/organizations/{id}/state` | MS archive/delete | MS CHECK (L18) | web renders status (L117) but suspended orgs are never returned (`mine` filters via verified orgs) | org02 CI DB test (suspension denies) | COMPLETE_VERIFIED (backend, CI DB; lifecycle partial) | ORG-008 |
| Owner | Show owner | MS: OWNER role memberships; DP: OWNER members | via member list (managers only) | owner field on org response | no owner column (derived) | role shown only for caller | org03 | PARTIALLY_IMPLEMENTED | Add owner summary to detail |
| Member count | | — | — | count field | derivable | none | — | MISSING | Add aggregate to detail |
| Member invitation & acceptance (MS) | Invite by email, accept | **none**; direct add `services/organizations.rs:241-298` | `PUT /api/v2/organizations/{id}/members/{userId}` (direct add by UUID) | invite/accept/decline/revoke | no invitation table | UUID text box `OrganizationsContent.tsx:156-164` | org03 (direct add) | MISSING (invitation) | ORG-005 |
| Member invitation & acceptance (DP) | | `control/members.rs:325-400,513-615` (token once, email-bound, 7-day TTL) | `POST/GET /v1/organizations/{id}/invitations`, `…/revoke`, `POST /v1/invitations/accept` | email delivery (token returned to inviter only) | `devplat_invitations` | none | `control_plane.rs:196` real DB | IMPLEMENTED_UNVERIFIED | reuse for MS? (ORG-001/005) |
| Roles & permissions (MS) | OWNER/ADMIN/MEMBER; admins manage MEMBERs only; no self-change | `services/organizations.rs:125-141,249-270` | PUT member | custom permissions | role CHECK (L29) | role select `OrganizationsContent.tsx:166-177` (client guards mirror server) | org03 asserts admin cannot mint admin / touch owner / self-promote; stranger 403 — CI DB | COMPLETE_VERIFIED (backend, CI) | — |
| Member listing (MS) | managers list members | `services/organizations.rs:300-315` | `GET /api/v2/organizations/{id}/members` | user names/emails in response | — | `OrganizationsContent.tsx:132-151` shows raw UUIDs | org03 (member → 403) | PARTIALLY_IMPLEMENTED | ORG-006 |
| Member removal | | MS: status REMOVED via PUT (`:277,281-283`); DP: `DELETE …/members/{id}` (`members.rs:210-268`, last-owner guard) | PUT (MS), DELETE (DP) | MS "leave organization" | `removed_at` | status select (web) | MS: no explicit REMOVED test found beyond org02 "Removed membership"; DP test L488 | IMPLEMENTED_UNVERIFIED (MS) | ORG-008 |
| Profile editing | rename etc. | MS: **none** (PATCH = status only, `dto/v2.rs:823-825`); DP: `control/organizations.rs:218-268` (displayName, supportEmail, optimistic version) | DP `PATCH /v1/organizations/{id}` | MS PATCH name/slug/description | — | none | DP DB test | MISSING (MS) | ORG-004 |
| Settings / org policy | org-wide profile restrictions | read in `profiles.rs:950-953`, `specialized.rs:262-265`, `prejoin.rs` | **no write API** | `PUT /api/v2/organizations/{id}/profile-policy` | `organizations.profile_policy JSONB DEFAULT '{}'` (`m20261005…:163`) | none | profiles_h tests (space-level) | PARTIALLY_IMPLEMENTED | ORG-009 |
| Ownership transfer | | MS none; DP `members.rs:269-316` | DP `POST …/ownership-transfer` | MS | — | none | DP L488 | MISSING (MS) | ORG-008 |
| Org meetings (create) | meeting with optional organization (PDR F-03) | `meeting_space.rs:305-314,452` (verified member only), ORGANIZATION_ONLY validation `:153-156`; admission `meeting_access.rs:331-341` | `POST /api/v2/meeting-spaces` (`organizationId`) | — | `meeting_spaces.organization_id` FK RESTRICT | **no org option** in `CreateMeetingSpaceContent.tsx:124-133`; Edit offers 4 policies only (`EditMeetingContent.tsx:38-40`); mobile none | org01 CI DB (create + join + non-member denied) | FRONTEND_ONLY_GAP | ORG-010 |
| Org calendar / org meeting list | see org's meetings | none — `list_for_actor` owner/member only (`meeting_space.rs:616-660`) | — | `GET /api/v2/organizations/{id}/meeting-spaces` (or filter) | — | none | — | MISSING | ORG-011 |
| Org ↔ Messaging | colleagues = org co-members | `relay.rs:670-717`; resolve/name `handlers/v2/unified_messaging.rs:272,304,335` | `GET /api/v2/messaging/directory`, `/resolve` | — | — | `MessagesContent.tsx:187` | unified_messaging tests | IMPLEMENTED_UNVERIFIED (visibility inconsistency ORG-006) | — |
| Org ↔ Calling / Enterprise | org admin configures enterprise calling for org | none for MS org; web uses fixed tenant (`lib/calling/server.ts:83-88`) + env allowlist (`route.ts:58-68`) | `/v1/enterprise/**` (tenant-scoped) | mapping MS org → tenant/env | — | `CallingEnterpriseHome.tsx`, resource pages | calling enterprise tests (tenant-scoped) | MISSING (relationship) | ORG-012 |
| Suspension | | MS `set_status`; DP close/operator | see Status | — | | none (no admin UI) | org02 CI DB | IMPLEMENTED_UNVERIFIED (UI) | — |
| Archive / deletion | | MS none (FK RESTRICT on memberships & spaces); DP CLOSED terminal + no-delete trigger (L185) | DP close | MS archive/delete | | none | DP L845 | MISSING (MS) | ORG-008 |
| Audit history | who changed what | MS: `tracing::info!` only (`services/organizations.rs:184,234,296`); DP `devplat_activity_events` append-only + `GET /v1/organizations/{id}/activity` | DP only | MS audit API | MS **no audit table** | none | DP L1370 | MISSING (MS) | ORG-007 |
| Swagger | | MS `swagger/mod.rs:195-199,465-472` (all 5 ops, schemas) | | | | | `openapi_contract.rs` | COMPLETE_VERIFIED (contract test) | — |
| SDK | | TS/Dart: MS **only** `v2MyOrganizations` (`operations.ts:383`); DP full org family (`operations.ts:239-305`) | | MS member ops intentionally EXCLUDED (`public-routes.json:2440,2447`) | | | | NOT_REQUIRED (by gateway policy) | — |
| Mobile org experience | | — | — | — | — | none (only `ORGANIZATION_ONLY` label in `space-card.tsx:44,57`) | — | FRONTEND_ONLY_GAP | ORG-016 |

## Findings

### ORG-001 — Three unreconciled "Organization" authorities and two independent membership/role systems
- Service/module: meeting-service (`organizations`), developer-platform (`devplat_organizations` + platform-access tenant), calling enterprise (tenant triple), numbering (tenant as ORGANIZATION owner).
- Expected: One organization identity per customer (or an explicit, documented mapping) so that an organization's members, meetings, calling configuration, numbers and billing refer to the same entity; one source of truth for membership/roles.
- Actual: Meeting orgs (UUID, OWNER/ADMIN/MEMBER) and developer orgs (= PA tenant UUID, OWNER/ADMIN/DEVELOPER/ANALYST/BILLING) are created, administered and joined independently. No column, mapping table or ADR links a meeting org to a tenant. Calling enterprise and Numbering use the tenant triple only. Both families are published on the same public gateway under `/organizations` paths (`/api/v2/organizations/*` and `/v1/organizations/*`). A CF Meet user who self-creates `POST /v1/organizations` gets an org that never appears in the in-app **Organizations** page, does not unlock ORGANIZATION_ONLY meetings or messaging colleagues; conversely a meeting org has no tenant, so it cannot hold calling/numbering/billing.
- Evidence: `services/developer-platform/migration/src/m20261102_000002_control_plane.rs:3` ("tenant = organization"), `:10-12` ("Not Platform Access principals, not Meeting Service organization members"); `services/meeting-service/migration/src/m20261004_000001_organizations.rs:1-6` ("No organization/membership source exists elsewhere in the monorepo" — now stale since DP exists); `docs/decisions/0018-enterprise-calling-architecture.md:47-56`; `infra/api-gateway/public-routes.json:6` (`MEETING_DIRECTORY_NOT_PUBLIC_FAMILY`: "never the developer organization authority"); `meeting_spaces` holds both `organization_id` and `platform_tenant_id` (`m20261101_000001_platform_environment_meetings.rs:35-45`). grep: `grep -rli "organization\|org_id" services/*/src services/*/migration`.
- API/Swagger refs: MS `v2_create_organization`, `v2_my_organizations`…; DP `createOrganization`, `listMembers`, `createInvitation` (`sdks/typescript/src/generated/operations.ts:239-305`).
- Affected frontend workflow: Organizations page, Developer console (deferred), Enterprise calling, Messaging colleagues.
- Owner class: PRODUCT_DECISION (+ SHARED_CONTRACT)
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Customers will see two different "organizations" with different members and roles; enterprise features cannot be sold per meeting-org; long-term data-integrity risk (diverging membership).
- Recommended remedy: Write an ADR choosing the canonical customer organization. Lowest-risk option consistent with ADR 0032: make the PA tenant/DP organization canonical and give `organizations` (MS) a nullable, immutable `platform_tenant_id` (unique) as a delegated projection, or keep MS orgs as "meeting directories" and rename them in UI/API docs ("Meeting directory"/"Team") to avoid conflation. Do not dual-write membership without one authority; if unified, MS membership becomes a projection of DP members (feed like ADR 0032 §6).
- Dependencies: product D-07; ADR 0032/0055; developer console frontend.
- Complexity: HIGH
- Regression risk: HIGH (ORGANIZATION_ONLY admission, messaging directory, spaces FK).
- Confidence: CONFIRMED

### ORG-002 — End users cannot create a meeting organization; platform-admin-only creation has no tooling
- Service/module: meeting-service organizations; apps/web.
- Expected: Either (a) users create their own org (self-service, as DP does), or (b) platform-admin-only creation with an operator/admin tool. PDR A.32 (PDF text L1975-1978): `POST /api/v2/admin/organizations` = "Privileged engineering tools; do not expose in public meeting frontend"; PDR D-07 (L803) leaves org admin UI scope **open**.
- Actual: Creation is platform-admin-only (`users.role == Admin` → `platform_admin`, `actor.rs:337`) enforced at `services/organizations.rs:96-102,151-152`; route `routes/v2.rs:492-494`. There is no admin console in `apps/web/src/app` (dirs: `(auth) (room) api calling dashboard invite join register …`), `organizationsService.create/update` are dead code, and `routes/admin.rs` has no org routes. So an org can only be created with curl + an admin bearer token. A normal user's Organizations page shows "You do not currently have an active, verified organization membership" (`OrganizationsContent.tsx:87-91`) with no path forward.
- Evidence: test `tests/organization_domain_access.rs:211-222` asserts an org OWNER gets 403 creating an org ("only platform admins create organizations"). `grep -rn "organizationsService\.\(create\|update\)" apps/web/src` → only definitions.
- API/Swagger refs: `v2_create_organization` (Swagger doc "Requires a platform administrator bearer token", handler L37-41).
- Affected frontend workflow: Journey B step 1; onboarding of any organization.
- Owner class: PRODUCT_DECISION (then FRONTEND or BACKEND)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: Organizations cannot be provisioned without engineering involvement; no self-serve B2B onboarding.
- Recommended remedy: Intent evidence points to **platform-admin-only for meeting orgs** (code, test, PDR). If that stands: build a privileged admin tool (separate console or operator CLI) using existing endpoints + new `GET /api/v2/admin/organizations` (list) — no new create endpoint needed. If self-service is desired, prefer DP `POST /v1/organizations` (already self-service, idempotent, audited) and resolve ORG-001 rather than opening MS creation to users.
- Dependencies: ORG-001, D-07.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-003 — Creation metadata and location missing from the user-facing Organization view (root cause per field)
- Service/module: meeting-service DTO/handler; apps/web OrganizationsContent; all org schemas.
- Expected: Org profile shows "Created on CF Meet <date> by <name>", creation source and registered/operating location; legal incorporation date distinct from workspace creation date.
- Actual — root cause per field:
  - **created_at**: stored (`m20261004…:20`), exposed (`handlers/v2/organizations.rs:21`, `dto/v2.rs:841`, Swagger required field), typed in web (`types/v2/index.ts:1519`) → **missing frontend rendering only** (`OrganizationsContent.tsx:114-118` renders name, slug, status, role).
  - **created_by**: stored and set (`m20261004…:19`; `services/organizations.rs:165`) → **missing API exposure** (`org()` mapper `handlers/v2/organizations.rs:15-23` omits it; `OrganizationResponse` has no field). Also a bare user UUID (FK `ON DELETE SET NULL`), so a display-name resolution is needed. DP stores `created_by` (`control_plane.rs:37`) but `ORG_VIEW` (`control/organizations.rs:28-32`) also omits it.
  - **creation source**: **missing storage** (MS); DP only has activity event `organization.created` and member `joined_via`.
  - **registered / operating location**: **missing storage, API and UI** in MS `organizations`, DP `devplat_organizations` and PA `platform_tenants` (grep `country|address|location` in those migrations → only `devplat_billing_accounts.country` L308, a billing attribute).
  - **legal incorporation date**: not modelled anywhere. `created_at` is the **CF Meet workspace creation date** (server `Utc::now()`, `services/organizations.rs:159-167`) and must not be labelled as incorporation date. Legal-entity evidence exists only as Numbering KYC evidence kinds `BUSINESS_REGISTRATION`/`ADDRESS` per KYC case (`kyc_eligibility.rs:254-256`), keyed to the tenant, not to the meeting org.
- Evidence: as above; `grep -n "created_by\|created_at" services/meeting-service/src/handlers/v2/organizations.rs`.
- API/Swagger refs: `OrganizationResponse` (`packages/contracts/openapi/meeting-service.json`, required: id,name,slug,status,createdAt).
- Affected frontend workflow: Organizations page; Journey B step 2.
- Owner class: FRONTEND (created_at) + BACKEND (created_by exposure; location/source storage) + PRODUCT_DECISION (location/legal fields)
- Classification: FRONTEND_ONLY_GAP (created_at) / PARTIALLY_IMPLEMENTED (created_by) / MISSING (source, location, incorporation date)
- Severity: P1
- Business impact: Users cannot see when/who created the workspace or where the organization operates; compliance/trust context missing.
- Recommended remedy: (1) Render `createdAt` now ("Created on CF Meet"). (2) Add `createdBy {userId, displayName}` to `OrganizationResponse` — additive, non-breaking; reuse existing `users` lookup. (3) Additive migration: `organizations ADD COLUMN creation_source TEXT NOT NULL DEFAULT 'PLATFORM_ADMIN'`, `country_code CHAR(2) NULL`, `operating_region TEXT NULL`, optional `legal_incorporated_on DATE NULL` (explicitly separate). Nullable columns → low migration risk. If ORG-001 makes DP canonical, add these to `devplat_organizations` instead.
- Dependencies: ORG-004 (detail endpoint), ORG-001.
- Complexity: LOW (1-2) / MEDIUM (3)
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-004 — No organization detail endpoint and no profile editing in meeting-service
- Service/module: meeting-service.
- Expected: `GET /api/v2/organizations/{id}` (members/managers/admin), `PATCH` for name/description/settings by OWNER/ADMIN; admin list of all orgs.
- Actual: Only list-of-mine (`GET /api/v2/me/organizations`). `PATCH /api/v2/admin/organizations/{id}` accepts **only** `status` (`dto/v2.rs:821-825`, `deny_unknown_fields`) and is platform-admin-only. Name/slug are immutable through the API. No `GET` list for platform admins (they cannot even enumerate orgs). Response lacks updatedAt, createdBy, owner, member count, description, type, location.
- Evidence: `routes/v2.rs:490-507` (5 routes total); `services/organizations.rs` has no `get`/`update_profile`.
- API/Swagger refs: Swagger tag "Organizations (v2)" — 5 operations.
- Affected frontend workflow: Organizations page detail; admin tooling.
- Owner class: BACKEND
- Classification: MISSING
- Severity: P1
- Business impact: Org owners cannot correct their organization name or view details; admins cannot operate the directory.
- Recommended remedy: New endpoints justified (none exists): `GET /api/v2/organizations/{id}` (active members; 404 for non-members to avoid existence leaks), `PATCH /api/v2/organizations/{id}` (OWNER/ADMIN; name, description, optimistic `version` — add `version` column), `GET /api/v2/admin/organizations` (paged). Reuse DP's version/activity pattern.
- Dependencies: ORG-003, ORG-007.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-005 — Meeting-org membership is added unilaterally by raw user UUID; no invitation/acceptance or consent
- Service/module: meeting-service `upsert_member`; web OrganizationsContent.
- Expected: Invite by email/username, invitee accepts/declines; revoke; expiry (as DP `devplat_invitations` does).
- Actual: An OWNER/ADMIN `PUT /api/v2/organizations/{id}/members/{userId}` immediately creates an ACTIVE membership for any active registered user (`services/organizations.rs:284-293`, `insert_membership` L188-219) with no consent. Effects for the target user: they gain ORGANIZATION_ONLY meeting access, appear (username + principal id) in every co-member's messaging colleague directory (`relay.rs:693-699`) and see all co-members. The UI requires typing a raw UUID (`OrganizationsContent.tsx:156-164`, regex L18) and there is no user search API for org managers. The validation error "userId must reference an active registered user" (L204) also lets a manager probe whether a UUID is an active account.
- Evidence: above; no invitation table in `m20261004_000001_organizations.rs`.
- API/Swagger refs: `v2_upsert_organization_member`.
- Affected frontend workflow: Journey B step 3.
- Owner class: BACKEND (+ FRONTEND)
- Classification: MISSING
- Severity: P1
- Business impact: Unusable member onboarding (UUIDs), privacy exposure of users added without consent.
- Recommended remedy: Add `organization_invitations` (copy DP design: email-bound, SHA-256 token, 7-day TTL, PENDING/ACCEPTED/REVOKED/EXPIRED) and endpoints `POST /api/v2/organizations/{id}/invitations`, `POST /api/v2/organization-invitations/accept`, `…/revoke`; restrict direct PUT add to platform admins or require PENDING status. Email delivery can reuse meeting-service `email_jobs`. If ORG-001 unifies on DP, reuse DP invitations instead.
- Dependencies: ORG-001.
- Complexity: MEDIUM
- Regression risk: MEDIUM (existing direct-add tests org01-03 rely on PUT).
- Confidence: CONFIRMED

### ORG-006 — Member list is identity-less and member visibility is inconsistent with the messaging directory
- Service/module: meeting-service organizations + unified_messaging relay.
- Expected: Managers see member names/emails; a consistent rule for who may see co-members.
- Actual: `OrganizationMemberResponse` has only `userId` (`dto/v2.rs:846-855`); UI prints raw UUIDs (`OrganizationsContent.tsx:138`). `list_members` is OWNER/ADMIN-only (`services/organizations.rs:307-309`; org03 asserts MEMBER → 403), yet any ACTIVE member can list up to 200 co-members with usernames via `GET /api/v2/messaging/directory` (`relay.rs:670-717`). The directory query does not filter `users.status`, so inactive/suspended accounts still appear as colleagues.
- Evidence: `relay.rs:693-699` (`WHERE m.organization_id = ANY($1) AND m.status = 'ACTIVE' AND u.id <> $2` — no `u.status` predicate).
- API/Swagger refs: `v2_list_organization_members`; messaging directory.
- Affected frontend workflow: Organizations page, Messaging "New conversation".
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Managers cannot tell who members are; policy inconsistency (not cross-tenant).
- Recommended remedy: Add `displayName`/`email` (managers) to member response (join `users`); decide whether members may list members and align both endpoints; add `u.status NOT IN ('inactive','suspended')` to directory query.
- Dependencies: none.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-007 — No durable audit history for meeting-org lifecycle and membership changes
- Service/module: meeting-service organizations.
- Expected: Append-only audit of create/status/membership changes with actor, before/after, time (DP has `devplat_activity_events`; Calling has `enterprise_admin_audit`).
- Actual: Only `tracing::info!` log lines (`services/organizations.rs:184,234,296`). `organization_memberships` row is mutated in place (role/status overwritten, L274-279), so prior roles are lost; no outbox event.
- Evidence: `grep -rn -i "organization" services/meeting-service/src/services/*.rs | grep -i "audit\|outbox\|event"` → nothing.
- API/Swagger refs: none.
- Affected frontend workflow: admin review; compliance.
- Owner class: BACKEND
- Classification: MISSING
- Severity: P2
- Business impact: Cannot answer "who gave X admin rights / suspended this org"; weak for healthcare/enterprise customers.
- Recommended remedy: Add `organization_audit_events` (append-only trigger) written in the same txn as each mutation, plus `GET /api/v2/organizations/{id}/audit` (OWNER/ADMIN/platform admin).
- Dependencies: ORG-004.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-008 — Meeting-org lifecycle incomplete: no archive/delete/close, no ownership transfer, no last-owner protection, suspended orgs invisible to members
- Service/module: meeting-service organizations.
- Expected: Close/archive/delete with retention; ownership transfer; an org always keeps ≥1 OWNER (DP enforces `LAST_OWNER`, `members.rs:3-6,184,234`); members see their org is suspended.
- Actual: status CHECK only `ACTIVE`/`SUSPENDED` (`m20261004…:18`); FKs `ON DELETE RESTRICT` (L27, L46) and no delete endpoint. No transfer endpoint. Orgs can be created without an owner (`owner_user_id` optional, `services/organizations.rs:179-182`); OWNERs can demote/remove other OWNERs and platform admins can remove the last OWNER (no count check in `upsert_member`). `mine()` filters by `ctx.verified_organizations` which requires `o.status='ACTIVE'` (`services/organizations.rs:44-49,330`), so a suspended org silently disappears from the member's list — the web status label (`OrganizationsContent.tsx:117`) can never show SUSPENDED. Membership leave (self-removal) is forbidden for non-admins (L255-259).
- Evidence: above.
- API/Swagger refs: `v2_update_organization` (status only).
- Affected frontend workflow: Organizations page; offboarding.
- Owner class: BACKEND (+ PRODUCT_DECISION for deletion/retention)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Orphaned/ownerless orgs; users cannot leave orgs; confusing disappearance on suspension.
- Recommended remedy: Add `CLOSED`/`ARCHIVED` status (CHECK change — expand migration), last-owner guard with row locks (port DP logic), `POST /api/v2/organizations/{id}/ownership-transfer`, `DELETE /api/v2/me/organizations/{id}` (leave), and return suspended orgs in `/me/organizations` with status.
- Dependencies: ORG-007.
- Complexity: MEDIUM
- Regression risk: MEDIUM (ORGANIZATION_ONLY admission semantics on status change).
- Confidence: CONFIRMED

### ORG-009 — Organization profile policy (org settings) has storage and readers but no write API
- Service/module: meeting-service profiles/specialized.
- Expected: Org OWNER/ADMIN (or platform admin) sets org-wide restrictions that bind every space (Part H).
- Actual: `organizations.profile_policy JSONB NOT NULL DEFAULT '{}'` (`m20261005_000005…:163`) is read in `profiles.rs:950-953`, `specialized.rs:262-265` and set to `{}` at creation (`services/organizations.rs:168`). No handler writes it (`grep -rn profile_policy src` → only space-level writes in `specialized.rs:271`). Org settings can only be changed via direct SQL.
- Evidence: above.
- API/Swagger refs: space-level `GET/PATCH /api/v2/meeting-spaces/{id}/profile-policy` only.
- Affected frontend workflow: org settings (absent).
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Org-wide recording/transcription restrictions cannot be configured by customers.
- Recommended remedy: `GET/PATCH /api/v2/organizations/{id}/profile-policy` reusing the space policy validator + version column; audit via ORG-007.
- Dependencies: ORG-004, ORG-007.
- Complexity: MEDIUM
- Regression risk: MEDIUM (tightening policy affects live spaces).
- Confidence: CONFIRMED

### ORG-010 — Web cannot create or edit organization meetings although backend supports them
- Service/module: apps/web meeting-spaces; (mobile).
- Expected: PDR F-03 (PDF L215-218) "…capture policies and **optional organization**"; select org + ORGANIZATION_ONLY.
- Actual: Backend accepts `organizationId` only for verified members (`meeting_space.rs:305-314,452`) and validates ORGANIZATION_ONLY needs an org (`:153-156`); org01 CI DB test covers create/join/deny. Web create form offers only PUBLIC/PUBLIC_PASSCODE/AUTHENTICATED/INVITE_ONLY and never sends `organizationId` (`CreateMeetingSpaceContent.tsx:50-57,124-133`); Edit form allows only those four (non-listed policy is shown read-only, `EditMeetingContent.tsx:38-40`). Mobile has types only (`features/meetings/v2/types.ts:146,183`).
- Evidence: `grep -rn "organizationId\|ORGANIZATION_ONLY" apps/web/src --include=*.tsx` → only list label in `MeetingsContent.tsx:179,192`.
- API/Swagger refs: `CreateMeetingSpaceRequest.organizationId` (`dto/v2.rs:110-111`).
- Affected frontend workflow: Journey B step 5.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: The core value of an organization (org-restricted meetings) is unreachable in the product UI.
- Recommended remedy: Add organization selector (from `useMyOrganizations`) + ORGANIZATION_ONLY / NON_MEMBERS_ONLY / EXTERNAL_ONLY options to create/edit; no backend change.
- Dependencies: none.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### ORG-011 — No organization calendar / org meeting listing; org managers have no authority over org-scoped spaces
- Service/module: meeting-service meeting_space.
- Expected: Members see org meetings; org OWNER/ADMIN can oversee/transfer org meetings (e.g. when an employee leaves).
- Actual: `list_for_actor` returns owned or member spaces only (`meeting_space.rs:638-655`); no filter/endpoint by organization. Space permissions don't consult org role. A space keeps `organization_id` after its owner leaves the org; ORGANIZATION_ONLY admission still uses org membership, but the org cannot manage or reassign it.
- Evidence: above; `grep -n organization src/services/meeting_space.rs`.
- API/Swagger refs: `GET /api/v2/meeting-spaces`.
- Affected frontend workflow: Dashboard F-02 ("Owned/member spaces" — PDR lists `me/organizations` as input but no org list).
- Owner class: BACKEND (+ PRODUCT_DECISION)
- Classification: MISSING
- Severity: P2
- Business impact: No org-level schedule; offboarding leaves meetings ungoverned.
- Recommended remedy: `GET /api/v2/organizations/{id}/meeting-spaces` (members: visible spaces; managers: all), optional org-admin override permission; keep existing per-space visibility rules (`space_is_visible`).
- Dependencies: ORG-004.
- Complexity: MEDIUM
- Regression risk: MEDIUM (permission matrix).
- Confidence: CONFIRMED

### ORG-012 — No Organization ↔ Enterprise Calling relationship in the CF Meet product; enterprise admin is a deployment-wide env allowlist
- Service/module: apps/web calling BFF; calling enterprise; meeting-service orgs.
- Expected: An organization's OWNER/ADMIN configures that organization's extensions, ring groups, queues; enterprise data isolated per organization.
- Actual: The web BFF authenticates every user to Calling with **one** server credential (`lib/calling/server.ts:93-104`) and requires each user's identity to match a single fixed `CALLING_TENANT_ID/APPLICATION_ID/ENVIRONMENT_ID` (`:83-88`). Enterprise routes are allowed only for principals listed in env `CALLING_ADMIN_PRINCIPAL_IDS` (`app/api/calling/[...path]/route.ts:58-68,236-238`). Hence all meeting orgs share one enterprise configuration (extensions/ring groups/queues/voicemail/recordings) and enterprise admins are not derived from org roles. Calling itself is correctly tenant-isolated (`tests/enterprise_certification.rs:1-6`), but meeting orgs are not tenants (ADR 0018 L47-56), so per-organization isolation does not exist at product level.
- Evidence: above; `grep -rn CALLING_ADMIN_PRINCIPAL_IDS apps/web/src`.
- API/Swagger refs: `/v1/enterprise/**` (`services/calling/src/enterprise/http.rs:57-230`).
- Affected frontend workflow: Journey G; `/calling/enterprise/*` pages.
- Owner class: SHARED_CONTRACT (+ PRODUCT_DECISION)
- Classification: MISSING
- Severity: P1
- Business impact: Cannot offer enterprise calling to more than one customer organization from the CF Meet app; allowlisted admins see/modify every user's enterprise configuration and voicemail/recording metadata within the shared environment.
- Recommended remedy: Resolve ORG-001 first; then map each customer organization to its own PA tenant/application/environment and have the BFF obtain a delegated context per organization (no shared credential), with enterprise admin derived from the canonical org role (OWNER/ADMIN). Short term: document the single-tenant limitation and keep the allowlist small.
- Dependencies: ORG-001; Calling/Platform Access delegated user context.
- Complexity: HIGH
- Regression risk: HIGH
- Confidence: CONFIRMED

### ORG-013 — Developer-org display name drifts from Platform Access tenant display name
- Service/module: developer-platform organizations; platform-access.
- Expected: Rename propagates to the identity authority or PA name is documented as immutable/internal.
- Actual: `PATCH /v1/organizations/{id}` updates only `devplat_organizations.display_name` (`control/organizations.rs:232-249`); `DelegationCall` has no rename variant (`platform_access/mod.rs:149-185`); `platform_tenants.display_name` keeps the creation name (`identity_hierarchy.rs:107`). Operator/PA views and Calling's projection show the stale name.
- Evidence: above.
- API/Swagger refs: `updateOrganization`.
- Affected frontend workflow: future developer console, operator tools.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Confusing names in operator/audit contexts.
- Recommended remedy: Add `PATCH /delegation/tenants/{id}` (display_name) or declare PA display_name internal.
- Dependencies: none. Complexity: LOW. Regression risk: LOW.
- Confidence: CONFIRMED

### ORG-014 — Developer-org creation can orphan a Platform Access tenant
- Service/module: developer-platform `organizations::create`.
- Expected: Tenant creation and local org row are atomic or compensated.
- Actual: PA `CreateTenant` is committed first (`control/organizations.rs:91-100`), then the local txn (`:107-147`). A DB failure/crash after delegation leaves an EXTERNAL tenant with no `devplat_organizations` row and no owner. No compensation or reconciliation exists (`grep -rni "orphan\|compensat" src` → none). Idempotent replay (`public.rs:409-419`) only helps when the client sent an `Idempotency-Key` and the record was stored.
- Evidence: above.
- API/Swagger refs: `createOrganization`.
- Affected frontend workflow: developer console org creation.
- Owner class: BACKEND
- Classification: IMPLEMENTED_UNVERIFIED
- Severity: P2
- Business impact: Unowned tenants in the identity authority; quota/billing noise.
- Recommended remedy: Use a deterministic tenant slug/idempotency key so retries re-bind, or add a reconciliation job that suspends PA tenants with no devplat row after N minutes.
- Dependencies: none. Complexity: MEDIUM. Regression risk: LOW.
- Confidence: REQUIRES_VALIDATION (failure mode inferred from code order; not reproduced)

### ORG-015 — Wrong error code and existence oracle on meeting-org endpoints
- Service/module: meeting-service organizations.
- Expected: `ORGANIZATION_NOT_FOUND`; non-members get the same answer for existing and non-existing orgs (DP returns 404 for both, `control/mod.rs:213-225`).
- Actual: `org_not_found()` uses `ErrorCode::MeetingNotFound` → `"MEETING_NOT_FOUND"` (`services/organizations.rs:69-71`, `error.rs:122`). `list_members`/`upsert_member` look up the org first (404) then check authority (403), so a stranger can distinguish existing org ids (low impact: UUIDv4).
- Evidence: above.
- API/Swagger refs: `v2_list_organization_members` documents only 403.
- Affected frontend workflow: error messaging.
- Owner class: BACKEND
- Classification: CONTRACT_MISMATCH
- Severity: P3
- Business impact: Misleading errors; minor enumeration.
- Recommended remedy: Add `OrganizationNotFound` error code; return 404 for non-members.
- Dependencies: none. Complexity: LOW. Regression risk: LOW (clients matching on code).
- Confidence: CONFIRMED

### ORG-016 — Mobile app has no organization experience
- Service/module: apps/mobile.
- Expected: Parity with web Organizations page (list my orgs, roles; managers manage members) and org option in meeting creation, unless product scopes it out.
- Actual: No screens/services; only `ORGANIZATION_ONLY` label/icon (`features/meetings/v2/components/space-card.tsx:44,57`) and `organizationId` types.
- Evidence: `grep -rli organization apps/mobile --include=*.ts* | grep -v node_modules`.
- API/Swagger refs: `GET /api/v2/me/organizations` (public, SDK).
- Affected frontend workflow: mobile Journey B.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Mobile-only users cannot see or use organizations.
- Recommended remedy: Add a read-only org list first (public SDK op exists); manager features after ORG-005.
- Dependencies: ORG-005, ORG-010. Complexity: MEDIUM. Regression risk: LOW.
- Confidence: CONFIRMED

### ORG-017 — Developer organization console (self-service orgs, invitations, ownership transfer) is backend-only
- Service/module: developer-platform; apps/web.
- Expected: Console UI per `docs/phases/phase-06/developer-console-api-handoff.md` screen matrix.
- Actual: Status line L3: `BACKEND_API_READY · FRONTEND_IMPLEMENTATION_DEFERRED`; `grep -rln "/v1/organizations" apps/web/src` → nothing. DP org view also omits `createdBy`/`technicalContactMemberId` (`ORG_VIEW` L28-32).
- Evidence: above.
- API/Swagger refs: DP org family (`operations.ts:239-305`).
- Affected frontend workflow: developer onboarding; Journey G step 1.
- Owner class: FRONTEND (+ PRODUCT_DECISION re ORG-001)
- Classification: DEFERRED_BY_OWNER
- Severity: P2
- Business impact: Customers cannot self-provision tenants/credentials needed for enterprise calling/numbering.
- Recommended remedy: Build console after ORG-001 decision to avoid two org switchers in one app.
- Dependencies: ORG-001. Complexity: HIGH. Regression risk: LOW.
- Confidence: CONFIRMED

### ORG-018 — Concurrency/validation edge cases in meeting-org membership upsert
- Service/module: meeting-service `upsert_member`.
- Expected: Concurrent adds of the same user → deterministic 409/200; updates re-validate the target account.
- Actual: SELECT-then-INSERT without lock (`services/organizations.rs:260,284-286`); a concurrent duplicate hits `uq_org_membership_current` and `insert_membership` propagates the DbErr unmapped (no `is_unique_violation` mapping, unlike `create` L172-177) → likely 500. The update branch (L273-280) does not re-check that the target user is active/non-guest.
- Evidence: above.
- API/Swagger refs: `v2_upsert_organization_member` documents 200/403 only.
- Affected frontend workflow: double-submit on Organizations page (button disabled while pending mitigates).
- Owner class: BACKEND
- Classification: IMPLEMENTED_UNVERIFIED
- Severity: P3
- Business impact: Rare 500s.
- Recommended remedy: Map unique violation → 409 CONFLICT or retry as update; `SELECT … FOR UPDATE` on org row.
- Dependencies: none. Complexity: LOW. Regression risk: LOW.
- Confidence: REQUIRES_VALIDATION

## Journeys

### Journey B — Create organization → retrieve details → invite members → manage permissions → schedule an organization meeting

| Step | Operation (METHOD path → handler) | Status | Evidence / notes |
|---|---|---|---|
| B1 Create organization | `POST /api/v2/admin/organizations` → `handlers/v2/organizations.rs:48-59` → `services/organizations.rs:144-186` | PARTIAL | Platform-admin only (`:151-152`); end user 403 (test org03 L211-222). No web/admin UI (ORG-002). Alternative `POST /v1/organizations` (DP, self-service) creates a **different** entity not visible on the Organizations page (ORG-001). |
| B2 Retrieve details | `GET /api/v2/me/organizations` → `handlers/v2/organizations.rs:142-155` → `services/organizations.rs:318-341` | PARTIAL | No `GET /organizations/{id}` (ORG-004). Response: id, name, slug, status, createdAt + role; web renders name/slug/status/role only; createdAt not rendered; createdBy/location absent (ORG-003). |
| B3 Invite members | `PUT /api/v2/organizations/{id}/members/{userId}` → `handlers/v2/organizations.rs:119-131` → `services/organizations.rs:241-298` | PARTIAL | Direct add by UUID, no invitation/acceptance (ORG-005). Web form `OrganizationsContent.tsx:155-206`. |
| B4 Manage permissions | same PUT (role OWNER/ADMIN/MEMBER, status) + `GET /api/v2/organizations/{id}/members` → `services/organizations.rs:300-315` | IMPLEMENTED (backend verified in CI) | org03 asserts admin cannot mint admins/touch owners, no self-promotion, member cannot list (CI DB test). Fixed role set only; members shown as UUIDs (ORG-006). |
| B5 Schedule org meeting | `POST /api/v2/meeting-spaces` with `organizationId` + `accessPolicy: ORGANIZATION_ONLY` → `meeting_space.rs:452` (`verify_organization` `:305-314`), then conferences; admission `meeting_access.rs:331-341` | PARTIAL | Backend covered by org01 (CI DB: member creates & joins, non-member/foreign org 403). **Web cannot set organization or ORGANIZATION_ONLY** (ORG-010); no org calendar (ORG-011); mobile none. |

**Overall: PARTIAL.** Backend chain (admin-created org → direct add → roles → org-restricted meeting → join) is exercised by DB integration tests `org01`/`org03` (run in CI, not executed by this audit). The journey is not completable by an end user in web or mobile: B1 has no UI and is admin-only, B2 lacks a detail view/metadata, B3 lacks invitations, B5 has no UI. Not PASS — no end-to-end UI evidence.

### Journey G — Enterprise: configure enterprise identity → manage members/extensions → queue/ring group → internal call → history and admin review

| Step | Operation (METHOD path → handler) | Status | Evidence / notes |
|---|---|---|---|
| G1 Configure enterprise identity | Customer path: `POST /v1/organizations` → `POST /v1/organizations/{id}/projects` → environments → service accounts → credentials (DP `http/public.rs:45-216`, delegated to PA `delegation.rs:82-90`); Calling projects identity (ADR 0032 §6). Enterprise policy `POST /v1/enterprise/policies` (`enterprise/http.rs:61-62`, scope `enterprise.config.write` L356). CF Meet app path: fixed tenant/app/env from env vars (`lib/calling/server.ts:83-88`), users linked to principals via `GET /api/v2/messaging/me` (`communications.rs:136`, `unified_messaging/identity.rs:145`). | PARTIAL | DP flow backend-only, console deferred (ORG-017); DP tests use FakePlatformAccess. In-app: single shared tenant, not per organization (ORG-012). Meeting orgs play no role. |
| G2 Manage members/extensions | `POST/GET /v1/enterprise/extensions` (`enterprise/http.rs:85-87`), `PUT /v1/enterprise/departments/{id}/members` (`:80-81`) | IMPLEMENTED (Calling CI DB tests) | `tests/enterprise_extensions.rs:21,256` (namespace, cross-scope 404). "Members" are PA principals, not org members. Web: generic resource pages (`lib/calling/resource-routes.ts`) gated by `CALLING_ADMIN_PRINCIPAL_IDS` (`route.ts:58-68`). |
| G3 Queue / ring group | `POST /v1/enterprise/queues` (`:102`), `PUT …/queues/{id}/members` (`:108-109`), `POST /v1/enterprise/ring-groups` (`:89-90`), `PUT …/ring-groups/{id}/members` (`:99-100`) | IMPLEMENTED (Calling CI DB tests) | `enterprise_queues.rs:138-863` (FIFO, offers, overflow, races), `enterprise_ring_groups.rs:123-677`. Web resource page for ring groups (`resource-routes.ts:103-110`) and queues. |
| G4 Internal call | `POST /v1/calls` with `targetExtension` (`calls/http.rs`; test `enterprise_extensions.rs:335-380`: ENTERPRISE_INTERNAL, no telephony, idempotent) | IMPLEMENTED (Calling CI DB test) | Web dial-by-extension `CallsContent.tsx:182`; BFF forces `initiatorPrincipalId` and own endpoint (`route.ts:200-211`). Real-media end-to-end not evidenced here. |
| G5 History & admin review | `GET /v1/calls` (`calls/http.rs:49`, test `call_history.rs`), `GET /v1/enterprise/audit` (`enterprise/http.rs:83`), recordings access-audit | IMPLEMENTED_UNVERIFIED (UI) | Web: calls list (principal-filtered by BFF `route.ts:201-202`); audit via `resource-routes.ts:262` (admin allowlist only). Admin review is not organization-scoped. |

**Overall: PARTIAL.** Calling's enterprise backend (extensions, ring groups, queues, internal extension calls, audit) is well covered by DB-backed tests and is tenant-isolated, but (a) enterprise identity provisioning has no customer UI (developer console deferred; DP tests fake Platform Access), (b) in the CF Meet app all users share one fixed tenant/environment and enterprise administration is an env-var allowlist, and (c) there is no relationship between a CF Meet organization and an enterprise calling scope (ORG-012). No end-to-end evidence across DP → PA → Calling → web was found.

## Counts

Feature matrix rows: 32, by primary classification:
- COMPLETE_VERIFIED: 3 (MS status/suspension enforcement — org02; MS roles & permissions — org03; Swagger contract — openapi_contract) — all CI DB/contract tests, not executed by this audit
- IMPLEMENTED_UNVERIFIED: 6 (DP creation, DP retrieval, DP invitations — real DB but fake Platform Access; MS member removal; org↔messaging directory; suspension UI)
- PARTIALLY_IMPLEMENTED: 6 (MS creation, MS retrieval, created_by, owner, member listing, org settings/profile_policy)
- FRONTEND_ONLY_GAP: 3 (created_at rendering, org meeting creation UI, mobile)
- MISSING: 13 (creation source, location, incorporation date, org type, description, member count, MS invitation, MS profile editing, MS ownership transfer, org calendar, org↔calling, MS archive/deletion, MS audit)
- NOT_REQUIRED: 1 (SDK/public exposure of MS member ops — excluded by gateway policy)
- CONTRACT_MISMATCH / EXTERNAL_DEPENDENCY / DEFERRED_BY_OWNER: 0 rows (CONTRACT_MISMATCH and DEFERRED_BY_OWNER appear as findings ORG-001/ORG-015 and ORG-017)

Findings by severity (18 total):
- P0: 0
- P1: 7 (ORG-001, ORG-002, ORG-003, ORG-004, ORG-005, ORG-010, ORG-012)
- P2: 8 (ORG-006, ORG-007, ORG-008, ORG-009, ORG-011, ORG-014, ORG-016, ORG-017)
- P3: 3 (ORG-013, ORG-015, ORG-018)
