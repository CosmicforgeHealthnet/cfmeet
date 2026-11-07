# Workspace context — Blueprint v1.0 Section 8

Decision record: [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md)
§5 (workspace context is per request, never session-global; no `/switch-workspace`; unknown and
unauthorized organizations get the same non-enumerating denial), [ADR 0058](../decisions/0058-first-party-organization-scopes-and-provisioning.md)
(roles, policies, provisioning). Authority reads used: [`organizations-internal-v1.md`](../api/organizations-internal-v1.md) §4, §8.2–§8.3.

Code: `services/meeting-service/src/services/workspaces.rs` (sources, decisions, paging),
`handlers/v2/workspaces.rs`, the existing signed Organizations client in
`services/organizations_authority.rs` (two read routes added to its relay allowlist; no second
client). Tests: `tests/organizations_authority.rs` (`s8_*`, CI only — they need PostgreSQL) and
the unit tests in `services/workspaces.rs`.

Nothing is stored. There is no "active organization", no switch endpoint and no context token:
the client may *show* a current workspace, but every organization-scoped request still names its
organization and the owning service re-verifies it.

## API

| Method | Path | Change |
|---|---|---|
| GET | `/api/v2/me/workspaces?cursor&limit` | NEW — `personal` first, then every organization in which the caller holds an ACTIVE membership (ACTIVE or SUSPENDED organization), ordered by id; role, organization status, provisioning state, capability summary |
| GET | `/api/v2/me/workspaces/{workspaceId}/capabilities` | NEW — `workspaceId` = `personal` or an organization id; features plus every capability with `granted` and `reason` |

v2 operations: 177 → 179 (`tests/api_contract_matrix.rs`, `tests/openapi_contract.rs`). No existing
operation changed. `GET /api/v2/me/organizations` keeps its contract (ADR 0056 §10).

Gateway classification (`infra/api-gateway/public-routes.json`): both `PUBLIC_CUSTOMER`, exactly
like the comparable `GET /api/v2/me/organizations`. Like it, they are human-only: Platform Access
machine callers cannot call them (`services/platform/operations.rs` test).

Both answers carry `Cache-Control: no-store`.

### Response shapes

`WorkspacePage` `{items: Workspace[], nextCursor: string|null, authority}`; `Workspace`
`{id, kind: PERSONAL|ORGANIZATION, displayName, organizationId, slug, role, organizationStatus,
provisioningState, capabilities: {granted[], canCreateMeetings, canInviteMembers, canAdminister}}`.

`WorkspaceCapabilities` `{workspaceId, kind, authority, organizationId, role, organizationStatus,
provisioningState, policyRevision, features: {meetings, messaging, calling},
capabilities: [{capability, granted, reason}], personalCommunicationsCapabilities, evaluatedAt}`.

`reason`: `ALLOWED`, `CAPABILITY_NOT_GRANTED` (role does not hold it), `POLICY_RESTRICTED` (role holds
it, organization policy removes it), `ORGANIZATION_SUSPENDED`, `FEATURE_DISABLED` (needs the
Organizations Service, not authoritative here). `authority`: `ORGANIZATIONS_SERVICE` or
`LEGACY_DIRECTORY`.

`features.*` means "the workspace context does not disable it" (organization ACTIVE and not
switched off by organization policy). Owner services still apply their own policy (stricter wins).
Personal communications capabilities stay with the Communications Hub
(`GET /api/v2/communications/me/capabilities`); the personal answer only points there
(`personalCommunicationsCapabilities`) and duplicates none of them.

Personal capabilities: `meetings.personal.create` (every registered account) and
`organizations.create` (`ALLOWED` only when the Organizations Service is authoritative; the per-user
creation limit still applies at creation). Organization capabilities use the contract vocabulary
(`organization.*`, `meetings.organization.*`, `calling.enterprise.manage`,
`messaging.organization.moderate`).

## Behaviour by `ORGANIZATIONS_AUTHORITY_MODE`

| | `authoritative` | `legacy` (default) / `shadow` |
|---|---|---|
| Source | Organizations Service, read with the verified acting user (`X-CF-Acting-User` from the account authority only) | meeting-service `organizations` / `organization_memberships` (shadow mode still serves the legacy answer; its background comparison is unchanged) |
| Listed organizations | ACTIVE memberships of ACTIVE or SUSPENDED organizations, every role including GUEST (`GET /internal/v1/users/{id}/organizations`) | ACTIVE memberships: ACTIVE organizations only when the request's organization directory verified them (the `/me/organizations` rule, fail closed), plus SUSPENDED organizations |
| Roles | OWNER, ADMIN, MANAGER, MEMBER, GUEST | OWNER, ADMIN, MEMBER |
| Capabilities | membership's effective capabilities ∩ the role's effective capabilities under the current policy (`…/roles/capabilities`); matrix-granted but removed = `POLICY_RESTRICTED`; features from `…/policies/effective` | only what the legacy routes implement: `organization.read` and `meetings.organization.create` for every verified member, `organization.members.read` / `.manage` for OWNER and ADMIN; everything else `FEATURE_DISABLED` |
| `provisioningState` | the organization's state (`NOT_REQUIRED`/`PENDING`/`IN_PROGRESS`/`READY`/`FAILED`; the same column the provisioning read reports — no tenant, application or environment id is exposed) | `null` (no provisioning) |
| `policyRevision` | the policy revision used | `null` |
| Personal `organizations.create` | `ALLOWED` | `FEATURE_DISABLED` |

Common rules: a SUSPENDED organization is listed with its status and grants nothing except what the
authority itself still grants its OWNERs (`organization.read`, so they can see it); its features are
all `false`, and no role-matrix or policy question is asked. ARCHIVED organizations and SUSPENDED or
REMOVED memberships are not workspaces. An organization of someone else, an unknown id and a
malformed id all answer `404 ORGANIZATION_NOT_FOUND` with the same message.

## Fail closed

Authoritative mode: if Organizations is unreachable, answers 5xx, refuses meeting-service's
credential, or returns a malformed / incomplete document (missing items, missing feature flag,
another organization's id, an unknown role), both routes answer `503 SERVICE_UNAVAILABLE` in the
standard error envelope — never a partial workspace list and never a capability. A contract refusal
(e.g. the membership ended between the reads → `404 ORGANIZATION_NOT_FOUND`) is relayed. If the
organization's status changes between the reads, nothing is granted. The personal capability read
needs no Organizations call and keeps answering.

## Client mapping (web / mobile — owned by the frontend team)

* Workspace switcher: `GET /api/v2/me/workspaces` (follow `nextCursor`). Keep the selected
  workspace id **client-side only**; never send it as a global header — there is nothing to switch
  on the server.
* Gate UI per workspace with `GET /api/v2/me/workspaces/{id}/capabilities` (`granted`; show
  `POLICY_RESTRICTED` as "turned off by your organization", `ORGANIZATION_SUSPENDED` as a suspended
  banner, `FEATURE_DISABLED` as unavailable). Re-read after any membership / policy change; never
  cache across sessions (`no-store`). The server still re-checks every operation.
* Organization-scoped calls keep naming the organization explicitly, e.g.
  `GET /api/v2/me/conferences?organizationId=…`, `POST /api/v2/meeting-spaces` with
  `organizationId`, `/api/v2/organizations/{id}/…`.
* Personal communications (contacts, public ID, privacy): keep using
  `GET /api/v2/communications/me/capabilities` (the personal answer links to it).
* A `404 ORGANIZATION_NOT_FOUND` for a remembered workspace means "no longer available": drop it
  and fall back to `personal`. A `503` means "retry"; do not fall back to stale capabilities.
* SDKs: TypeScript `v2MyWorkspaces` (paginated) / `v2MyWorkspaceCapabilities`; Dart equivalents
  (generated by `sdks/generate.py`).

## Left for later

* Owner-service enforcement of organization policy: the authoritative organization directory used
  by meeting admission / space creation still counts any ACTIVE non-GUEST membership of an ACTIVE
  organization as membership; it does not yet apply `meetings.hostCreation` or
  `features.meetings`. The capability read reports the authority's (policy-aware) answer. Wiring
  policy into meeting creation is a Meeting-side change for a later section (Section 10/12
  follow-up), as are Calling (Section 12) and Messaging (Section 13).
* Team context (`teams[]` from `memberships/resolve`) is not part of the workspace answer.
* Production: nothing here is deployed; `ORGANIZATIONS_AUTHORITY_MODE` stays `legacy` by default.
