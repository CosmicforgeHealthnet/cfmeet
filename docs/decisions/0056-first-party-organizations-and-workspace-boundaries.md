# ADR 0056 — First-party Organizations authority, workspace context and developer/technical boundaries

**Status:** Accepted (Blueprint v1.0, Section 1). Implementation proceeds section by section on `staging`; nothing here is a production change.
**Date:** 2026-11-06
**Scope:** CF Meet Consolidated Platform Architecture & Implementation Blueprint v1.0, Phase 0–I (decision register ADR-A01…A16)
**Supersedes (in part):** the "no organization/membership source exists elsewhere" note in `services/meeting-service/migration/src/m20261004_000001_organizations.rs:1-6`; the implicit single shared enterprise scope used by the web calling BFF (`apps/web/src/lib/calling/server.ts:83-104`).
**Resolves audit findings:** ORG-001 (+PLT-010, DATA-004) at the decision level; sets the target for ORG-002…018, CAL-007/ORG-012.

## Context

The 2026-11-06 audit (`docs/audits/`) found three unreconciled "organization" authorities (ORG-001):

| # | Concept | Owner today | Evidence |
|---|---|---|---|
| 1 | Meeting organization (`organizations`, `organization_memberships`, OWNER/ADMIN/MEMBER) | meeting-service | `m20261004_000001_organizations.rs:14-41`; 5 routes (`routes/v2.rs:490-507`) |
| 2 | Developer workspace (`devplat_organizations` keyed 1:1 to a Platform Access EXTERNAL tenant) | developer-platform | `m20261102_000002_control_plane.rs:3,10-12`; 22 `/v1/organizations*` ops |
| 3 | Technical tenant / application / environment | platform-access (ADR 0032) | `m20261016_000001_identity_hierarchy.rs:104-124` |

Calling enterprise configuration is keyed by the technical triple (ADR 0018: "the tenant is the organization boundary"), but the CF Meet web app runs every user through one shared triple and an environment allowlist (CAL-007). Meeting organizations have no technical scope, so they cannot hold enterprise calling, numbers or billing; developer workspaces never appear in the CF Meet app.

The blueprint fixes the target: one new **Organizations Service** as the first-party business-organization authority, migrated from meeting-service (not rebuilt), with the developer workspace and the technical tenant kept as separate concepts.

## Decisions

### 1. Three concepts, never interchangeable (ADR-A02, A03, A04)

| Concept | Identifier | Canonical owner | Roles |
|---|---|---|---|
| First-party business organization | `organizationId` (UUID, immutable) | **`services/organizations`** (new) | OWNER, ADMIN, MANAGER, MEMBER, GUEST + capability grants |
| Developer workspace (API customer) | `tenantId` of an EXTERNAL tenant (`devplat_organizations.tenant_id`) | developer-platform (unchanged) | OWNER, ADMIN, DEVELOPER, ANALYST, BILLING (unchanged) |
| Technical scope | tenant / application / environment / principal ids | platform-access (unchanged) | scopes and grants (unchanged) |

* No role is inherited across these concepts. An organization OWNER is not a developer-workspace OWNER, not an enterprise-calling administrator by default (that is a capability grant, `calling.enterprise.manage`), and never a Platform Access operator.
* A legal business that has both an organization and a developer workspace may later be linked only by an explicit, audited affiliation record. No such link is built in Phase I.
* Public naming: `/api/v2/organizations*` is the **business organization** (first-party app); `/v1/organizations*` stays the **API customer workspace** (developer platform). Documentation, Swagger tags and SDK descriptions must say which one they mean (Section 16).

### 2. Organizations Service boundary (ADR-A02, principle "one writer per domain")

* New service `services/organizations` with its own PostgreSQL database, its own migration history table, a runtime role without DDL and a migrator role with DDL, signed internal authentication (`packages/service-auth`, ADR 0004) and least-privilege ingress. It must pass `scripts/service-boundaries/check_service_boundaries.py`.
* It owns: organizations, profiles, memberships (with history), invitations, teams and team members, role/capability grants, organization policies (organization-level constraints only), organization ↔ technical-scope mappings, provisioning workflows, audit events and its outbox.
* It must not own: passwords or any account credential, meeting/conference/message/call/recording data, PBX configuration, developer-workspace roles, technical tenants or credential verifiers.
* **Migration, not reconstruction.** The existing meeting-service `organizations` and `organization_memberships` rows are imported with their UUIDs, timestamps and `created_by` preserved. Meeting-service remains the single writer until an explicit, gated cutover (Section 4); after it, meeting-service organization writes become authenticated facades over the new service and meeting-service keeps a read-only projection for its foreign keys (`meeting_spaces.organization_id`) and ORGANIZATION_ONLY admission. **At no time are there two active writers.**
* The production cutover is a release operation. It requires the Section 2 backup and restore evidence and a separate explicit release approval; engineering work stops at that boundary.

### 3. Human identity stays where it is (ADR-A05)

* The CF Meet human account (`users.id` in meeting-service) is the only human identity. Organizations stores `user_id` as an opaque reference; there is no second user, password or Google-identity table anywhere.
* **Ingress pattern (reused, not invented).** Public first-party organization routes are served under `/api/v2/...` by meeting-service, which authenticates the human with the account authority's own session checks (including the Section 2 revocation epoch) and forwards the request to Organizations with a short-lived signed service token that carries the verified acting `userId`. This is the same pattern meeting-service already uses for the Communications Hub relay (`/api/v2/communications/*`, 73 operations, production-verified). Organizations trusts only the signed actor, never a client-supplied user id, role or tenant.
* Developer Portal humans reach the Developer Platform through the existing session bridge (`/internal/v1/user-sessions/verify`), hardened in Section 2. Developer roles are checked by the Developer Platform, independently.

### 4. Technical-scope cardinality for organizations (ADR-A06)

* **Decision:** each READY first-party organization maps to **one dedicated Platform Access tenant** (classification FIRST_PARTY) with **one governed first-party application and one environment** created for it. Enterprise Calling, organization Messaging and any other organization-scoped technical resource for that organization are provisioned under that triple.
* Why: Calling and Messaging already isolate every row by `(tenant_id, application_id, environment_id)` and certify cross-scope denial (`services/calling/tests/enterprise_extensions.rs` cross-scope 404, ADR 0018). A dedicated tenant reuses that proven boundary, gives per-organization limits (fixes CAL-009 concentration) and avoids inventing a second isolation key. It is never acceptable to fall back to one shared enterprise scope for two organizations.
* The mapping lives in `organization_platform_scopes` (Organizations Service) and is marked READY only after Platform Access confirms the scope server-side. Platform Access stays the tenant authority; Organizations stores ids and verification evidence only.
* Validation gate (Section 7): confirm that Platform Access can create FIRST_PARTY tenants for this purpose through an authenticated delegation path (today the Developer Platform delegation creates EXTERNAL tenants). If it cannot, Section 7 adds that capability **to Platform Access** rather than writing tenants anywhere else.
* Personal communication keeps the existing governed personal first-party scope (the scope the Communications release issued its launch credential for). It is not an organization and is never shared with one.

### 5. Workspace context is per request, never session-global (ADR-A01, A16; blueprint principle 4)

* The client may show "Personal" or an organization as the current workspace. The server never stores a global mutable "active organization".
* Every organization-scoped request names the organization (path or query). Each owning service re-verifies, per request, through Organizations: account active, organization ACTIVE, membership ACTIVE, capability granted, provisioning READY where a technical scope is needed. Unauthorized or unknown organization ids return the same non-enumerating denial.
* `POST /switch-workspace` is not built. If short-lived context tokens are ever introduced they must be audience-bound, non-escalating and revoked on membership removal.

### 6. Authorization, events and fail-closed behaviour (blueprint §3)

* Organizations exposes signed internal operations for membership resolution, capability evaluation and scope resolution. Owners (Meeting, Calling, Messaging, Hub) call them or keep projections fed by versioned events.
* Event delivery is at-least-once from a transactional outbox; consumers dedupe by `eventId`, apply by monotonic `aggregateVersion`, and reconcile periodically. A lagging or unavailable projection must never grant more access than the authority: removal and suspension fail closed.
* Every privileged mutation records the verified acting human, the organization and the service credential used.

### 7. Calendar, contacts, notifications (ADR-A07, A08)

* Calendar stays in meeting-service v2 (agenda query, ICS). No calendar service.
* Contacts, public ID/QR, relationship and privacy stay in the Communications Hub (73 operations reused). No contacts service.
* Notifications reuse existing outboxes and providers; no generic notification service unless Section 11 proves an owner gap.

### 8. Phone verification, realtime and external gates (ADR-A09, A10, A12)

* Phone binding is built provider-neutral and **off by default**. Until a real SMS/verification provider is approved and certified, its status is `PROVIDER_BLOCKED`; the deterministic test provider stays refused in staging and production, and no existing no-phone account loses access. This keeps the Sections 8–23 decision (phone DEFERRED) truthful while preparing the agreed product gate.
* MANAGED Messaging realtime may use a separately authenticated session device that can never acquire PRIVATE_E2EE privileges. This requires an ADR amendment before implementation (Section 13).
* PRIVATE_E2EE (issue #13), push (APNs/FCM/Web Push), PSTN, numbers and SMS remain provider/approval gated and are reported as such.

### 9. Backups before authority migration (ADR-A13)

* No live authority migration (Organizations cutover, account-table security migrations) runs in production without a verified, encrypted backup held off the production VPS and a passed isolated restore drill. Local same-host copies are not reported as disaster recovery. The off-host storage target is an owner choice (no AWS by default); until it is provided, the cutover gate stays closed.

### 10. Compatibility and retirement (ADR-A15)

* Existing routes stay working. `GET /api/v2/me/organizations` and `/api/v2/organizations/{id}/members*` keep their contracts and become facades over the new authority. Legacy routes are retired only with a per-endpoint deprecation record and measured zero consumers (Section 16).

## Consequences

* One new service and database to operate, back up and certify. Accepted by the blueprint as the only new major service.
* Meeting-service gains a relay to Organizations (same shape as the Communications relay) and a read-only organization projection.
* Platform Access may need a FIRST_PARTY tenant-provisioning path for organizations (Section 7).
* The web calling BFF's shared credential and admin allowlist become a legacy path to retire once per-organization scopes are READY (Section 12).

## Alternatives rejected

* **Make the Developer Platform organization canonical.** It is a projection of an EXTERNAL API-customer tenant with developer roles and billing; using it for colleagues, meetings and calendars would merge customer-facing collaboration with API control-plane roles (blueprint ADR-A03).
* **Keep extending meeting-service organizations.** It would keep business-organization authority inside the meeting and account service, which already carries the least-hardened authority (audit §7.1), and would not give organizations a clean provisioning and event boundary.
* **One shared first-party tenant with a per-organization environment.** Also isolating, but enterprise calling limits, operator views and suspension are tenant-level in Platform Access and Calling; per-environment isolation would need new rules in several services. Kept only as a fallback if Section 7 proves dedicated tenants infeasible, with an equivalent isolation proof.
