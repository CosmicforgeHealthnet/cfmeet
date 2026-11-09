# Operator / admin plane — consolidation map (PLT-009)

**Finding:** PLT-009 (P2): the operator/admin plane is fragmented, partly unaudited, has no
UI, and its audit trails are write-only. Blueprint v1.0 Section 15.
**Section 15 scope (bounded, backend only):** every operator mutation in Platform Access and
the Developer Platform writes an audit record, and their operator audit trails become
readable through operator-only, paginated endpoints. No UI. The other admin planes are
**not** rewritten in Section 15; this document maps them and the target consolidation.
Inventory taken on `staging` `90c2397e` (operation counts from the handlers and from the
`OPERATOR_ADMIN` rows of `infra/api-gateway/route-manifest.json`).

## 1. Inventory

| # | Plane | Routes | Authorization | Audit — write | Audit — read |
|---|---|---|---|---|---|
| 1 | **Meeting Service v1** `/api/v1/admin/*` (`routes/admin.rs`, `handlers/admin.rs`) | 4: `GET users`, `PATCH users/{id}`, `GET meetings`, `GET logs` | CF Meet user session + `UserRole::Admin` (`require_admin`) | **None** for `PATCH /api/v1/admin/users/{id}` (`update_user` changes role / status without writing `account_security_events`; verified on `staging` `90c2397e`). Self role change is refused; an admin can still change their **own status** | `GET logs` reads meeting session logs, not admin actions |
| 2 | **Meeting Service v2** `/api/v2/admin/*` (`routes/v2.rs`) | 9: `audit-events`, chat-migration (status, transitions, parity-runs), compat (backfill, legacy shadow), organizations (list, create, PATCH) | session + platform admin (`UserRole::Admin` → `platform_admin`) | account events → `account_security_events` (Section 2, SEC-013); chat-migration transitions → `conference_chat_migration_transitions` (actor, approver); organization admin calls are relayed to the Organizations service, which audits them (below); `compat/backfill` writes no audit row | `GET /api/v2/admin/audit-events` (account security events; the read itself is audited `SECURITY_AUDIT_READ`) |
| 2a | **Organizations service** `/internal/v1/admin/organizations*` (`services/organizations/src/http/mod.rs`) | 3: list / create, PATCH, `provisioning/retry` | internal service auth, `organization.administer`; the relayed platform admin (`X-CF-Acting-User`) is the audit actor (`docs/api/organizations-internal-v1.md`) | every mutation writes an organization audit event + outbox event | per organization: `/internal/v1/organizations/{id}/audit-events` (`organization.audit.read`, OWNER / ADMIN) — no cross-organization operator read |
| 3 | **Developer Platform** `/internal/v1/operator/**` (`http/operator.rs`, `webhooks/migration/http.rs`) | 32 (22 control-plane + 10 webhook-migration) | `developer-platform.administer` (registry-grantable to `cfmeet.operator-console` only) + named human operator `x-cf-operator` | control-plane mutations → `devplat_activity_events` via `activity::record_operator` (append-only by trigger, same transaction); webhook migration → `devplat_webhook_migration_audit`. Section 15 closes the gap for the platform-level commands that only logged (plan publish / retire, rollup rebuild, API-operation backfill, stream reset): they now write `devplat_operator_audit` (append-only) in the command's transaction (backfill: after it returns, it is idempotent) | **Before Section 15:** none for operators (tenant members read their own `GET /v1/organizations/{id}/activity`); only `GET /operator/organizations/{id}` existed, no list / search. **Section 15 adds** `GET /internal/v1/operator/organizations` (list / search, paginated) and `GET /internal/v1/operator/activity` (operator activity across organizations, paginated), both `developer-platform.administer` + `x-cf-operator` |
| 4 | **Platform Access** `/internal/v1/admin/**` (`http/admin.rs`) + `platform-access-admin` CLI (`src/bin/platform-access-admin.rs`) | 26 operations on 23 paths (tenants, applications, environments, scopes, service accounts, credentials, principals); the CLI covers the same hierarchy plus identity hand-off | `platform-identity.administer` (operator console only) + `x-cf-operator`; the CLI connects directly to the Platform Access database and requires `--operator` on every mutating command | every `operator_*` mutation writes one `platform_access_audit` row in its transaction (`identity/operator.rs`); delegated Developer Platform calls are audited as `devplat.<actor>` (`http/delegation.rs`) | **Before Section 15:** none (no route or CLI selected from `platform_access_audit`). **Section 15 adds** `GET /internal/v1/admin/audit` (paginated, filterable, `platform-identity.administer` + `x-cf-operator`) |
| 5 | **Numbering** `/v1/admin/**` (`services/numbering/src/http/{catalog,kyc,assignments,stewardship}.rs`) | 25 (catalog 3, controls 2, KYC 5, number resources 11, quarantines 1, reconciliation 1, relay numbers 1, risk decisions 1) | platform credential with the privileged `numbers.admin` scope (FIRST_PARTY applications only, explicit operator grant) + `x-cf-operator` | per-aggregate event history: `number_lifecycle_events`, `number_control_events`, `number_assignment_history`, `number_port_events`, KYC case events; privileged evidence reads write `number_history_access` | partial: per-resource history (e.g. admin assignment history, KYC case); no unified operator audit query |
| 6 | **Communications** `/internal/v1/communications/admin/*` (`services/communications/src/api.rs`) | 3: `GET metrics`, `POST suspend`, `POST phone-recovery` (deferred handler) | internal service auth (all grantable to `cfmeet.operator-console` only): `communications-observability.read`, `communications-profile.suspend`, `communications-backfill.run` | `suspend` writes `communication_audit` (`store.audit_system`); retention deletes rows after 365 days | **none** (no route selects from `communication_audit`) |

There is **no operator console**. `cfmeet.operator-console` (`packages/service-auth/src/identity.rs`)
is used only by tests and `certification/` to sign calls.

## 2. Section 15 changes (Platform Access + Developer Platform)

| Change | Endpoint / mechanism | Authorization |
|---|---|---|
| Platform Access audit read | `GET /internal/v1/admin/audit` — `platform_access_audit`, newest first, keyset cursor, limit 1–200, filters `tenantId`, `targetKind`, `targetId`, `actor`, `action`, `correlationId` | `platform-identity.administer` + `x-cf-operator` |
| Developer Platform organization list / search | `GET /internal/v1/operator/organizations` — newest first, keyset cursor, `query` (display-name substring, literal, or exact organization id), `state` | `developer-platform.administer` + `x-cf-operator` |
| Developer Platform operator activity read | `GET /internal/v1/operator/activity` — OPERATOR rows of `devplat_activity_events` (scope ORGANIZATION) plus `devplat_operator_audit` (scope PLATFORM), newest first, keyset cursor, filters `organizationId`, `operator`, `action` (exact or `family.` prefix) | `developer-platform.administer` + `x-cf-operator` |
| Write coverage | every operator mutation in both services writes exactly one audit / activity row in the mutation's transaction | — |

Exact parameters and schemas: `packages/contracts/platform-access/internal/v1.openapi.json`
and `packages/contracts/platform-developer/internal/v1.openapi.json` at the release SHA.

## 3. Remaining fragmentation (not changed in Section 15)

| Gap | Plane | Proposed owner / section |
|---|---|---|
| `PATCH /api/v1/admin/users/{id}` writes no audit row; admins can change their own status | 1 | meeting-service account authority (Section 2 follow-up, or Section 16 when v1 admin retires). Minimal fix: write `SecurityEvent` `ADMIN_USER_UPDATED` (actor, target, before/after role/status) and refuse self status change |
| `compat/backfill` writes no audit row | 2 | meeting-service; retires with the compatibility layer (Section 16) |
| No cross-organization operator audit read | 2a | Organizations service: an operator-only `GET /internal/v1/admin/audit-events` when the console is built |
| No unified numbering audit query | 5 | Numbering: an operator read over its event tables (console phase) |
| `communication_audit` is write-only | 6 | Communications: an operator read (console phase) |
| Six planes, five authorization models (session role, platform admin, internal permission + `x-cf-operator`, privileged customer scope, internal permission only) | all | console phase below |

## 4. Target consolidation

1. **One operator console, a separate app** (never part of the customer web app; product
   spec: "Do not expose internal/admin migration APIs in a user-facing screen"). It signs
   as `cfmeet.operator-console` and always sends a named human operator.
2. **One authorization model:** internal service auth with a per-service `*.administer`
   permission granted only to `cfmeet.operator-console`, plus `x-cf-operator`. Planes fold
   in as follows:

   | Plane | Target |
   |---|---|
   | Platform Access, Developer Platform | already on the target model; consumed as they are |
   | Organizations service | already on the model (`organization.administer`); add the operator audit read |
   | Meeting v2 `/api/v2/admin/*` | keep as the session-based facade until the console exists, then call the owning internal APIs (Organizations, account security) directly from the console |
   | Meeting v1 `/api/v1/admin/*` | retire (Section 16, legacy retirement after zero use); users / roles move to the account-security admin surface |
   | Numbering `/v1/admin/**` | keep the `numbers.admin` scope (it is a governed first-party credential path); the console holds that credential server-side |
   | Communications admin | already internal; add the audit read |

3. **One audit read convention:** each service keeps its own audit table (no cross-service
   audit database — service boundaries, ADR 0005), exposing `GET …/audit` with cursor
   pagination, newest first, operator-only. The console federates the reads.
4. **Build order:** console shell + Platform Access / Developer Platform (Section 15
   endpoints) → Organizations → meeting account admin → numbering → communications.

Explicitly out of scope now: rewriting the numbering, communications or meeting-service
admin planes.
