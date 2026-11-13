# Organizations Service — internal contract v1 (Blueprint Section 3 freeze)

**Owner:** `services/organizations` (`cfmeet.organizations`). **Decision record:** [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md). **Machine-readable contract:** `packages/contracts/organizations/internal/v1.openapi.json` (generated from this page; this page wins on conflict until that file exists).

All routes are under `/internal/v1`, require a valid `packages/service-auth` token (`X-CF-Service-Authorization`) whose issuer holds the stated permission in the **receiver's** trust bundle, and refuse replays (per-token nonce table). No route is reachable from public ingress.

## 1. Actor propagation

Operations performed for a human carry the acting user in `X-CF-Acting-User: <uuid>`. Only callers holding `organization.act-for-user` (initially `cfmeet.meeting-service`, which has just authenticated the human with the account authority) may set it; from any other caller the header is refused (`403 ACTOR_NOT_ALLOWED`). Organizations never accepts a user id, role, tenant or organization role from a request body as authority. Optional `X-CF-Acting-User-Verified-Email: <lowercased email>` is accepted from the same callers only, for email-bound invitation acceptance.

## 2. Envelope and errors

JSON, camelCase. Errors: `{"schemaVersion":1,"error":{"code","message","requestId"}}` with `Cache-Control: no-store`. Stable codes: `VALIDATION_ERROR` 400, `ORGANIZATION_NOT_FOUND` 404 (also for organizations the actor may not see — non-enumerating), `MEMBERSHIP_REQUIRED` 403, `PERMISSION_DENIED` 403, `ORGANIZATION_SUSPENDED` 409, `ORGANIZATION_ARCHIVED` 409, `LAST_OWNER` 409, `VERSION_CONFLICT` 409, `SLUG_TAKEN` 409, `INVITE_EXPIRED` 410, `INVITATION_INVALID` 404, `IDEMPOTENCY_CONFLICT` 409, `PROVISIONING_PENDING` 409, `RATE_LIMITED` 429, `ACTOR_NOT_ALLOWED` 403, `INTERNAL_AUTHENTICATION_FAILED` 401, `INTERNAL_PERMISSION_DENIED` 403, `SERVICE_UNAVAILABLE` 503; Section 6: `POLICY_RESTRICTED` 403, `TEAM_NOT_FOUND` 404, `TEAM_ARCHIVED` 409, `TEAM_NAME_TAKEN` 409. Writes accept `Idempotency-Key` (replay with the same body returns the stored response; a different body returns `IDEMPOTENCY_CONFLICT`). Updates take `expectedVersion` (optimistic concurrency).

## 3. Resources

**Organization**
```json
{"id":"uuid","name":"string 1-200","slug":"^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$",
 "lifecycleStatus":"ACTIVE|SUSPENDED|ARCHIVED","provisioningState":"NOT_REQUIRED|PENDING|IN_PROGRESS|READY|FAILED",
 "creationSource":"SELF_SERVICE|PLATFORM_ADMIN|IMPORTED","createdBy":"uuid|null","createdAt":"RFC3339","updatedAt":"RFC3339",
 "version":1,"archivedAt":"RFC3339|null",
 "profile":{"description":"≤2000|null","organizationType":"≤60|null","industry":"≤60|null","countryCode":"ISO-3166 alpha-2|null",
            "city":"≤120|null","operatingAddress":"≤500|null","logoRef":"≤500|null","publicContactEmail":"≤254|null",
            "legalRegistration":{"registeredName":"≤200|null","registrationNumber":"≤100|null","incorporatedOn":"YYYY-MM-DD|null","verification":"SELF_DECLARED|VERIFIED"}|null},
 "viewer":{"membershipId":"uuid","role":"OWNER|ADMIN|MANAGER|MEMBER|GUEST","status":"ACTIVE|SUSPENDED","capabilities":["..."]},
 "memberCount":3}
```
`createdAt` is the CF Meet workspace creation time and is never presented as an incorporation date. Legal registration is optional and `SELF_DECLARED` until verified by an operator.

**Membership** `{"id","organizationId","userId","role","status":"ACTIVE|SUSPENDED|REMOVED","joinedVia":"CREATOR|INVITATION|IMPORTED|OPERATOR|OWNERSHIP_TRANSFER","createdBy","createdAt","updatedAt","removedAt","version"}`

**Invitation** `{"id","organizationId","role","status":"PENDING|ACCEPTED|DECLINED|REVOKED|EXPIRED","inviteeEmailHint":"a***@example.com","invitedBy","createdAt","expiresAt","decidedAt"}` — the raw token is returned once at creation as `token` and only its SHA-256 is stored; tokens never appear in URLs or logs.

**Roles → capabilities (default-deny)**

| Capability | OWNER | ADMIN | MANAGER | MEMBER | GUEST |
|---|---|---|---|---|---|
| `organization.read` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `organization.members.read` | ✓ | ✓ | ✓ | ✓ | — |
| `organization.profile.write` | ✓ | ✓ | — | — | — |
| `organization.members.invite` | ✓ | ✓ | ✓ | — | — |
| `organization.members.manage` | ✓ | ✓ (not OWNER/ADMIN targets) | — | — | — |
| `organization.ownership.transfer` | ✓ | — | — | — | — |
| `organization.lifecycle.manage` (suspend/reactivate/archive) | ✓ | — | — | — | — |
| `organization.audit.read` | ✓ | ✓ | — | — | — |
| `organization.policies.manage` | ✓ | ✓ | — | — | — |
| `organization.teams.manage` | ✓ | ✓ | ✓ (own teams, Section 6) | — | — |
| `meetings.organization.create` | ✓ | ✓ | ✓ | ✓ | — |
| `meetings.organization.manage` | ✓ | ✓ | — | — | — |
| `calling.enterprise.manage` | ✓ | ✓ | — | — | — |
| `messaging.organization.moderate` | ✓ | ✓ | — | — | — |

A SUSPENDED or ARCHIVED organization grants no capability except `organization.read` to its OWNERs (so they can see the status). A non-ACTIVE membership grants nothing. The organization policy (§8.2) can only REMOVE a capability this matrix grants (decision reason `POLICY_RESTRICTED`); it never grants one and never restricts OWNER administration.

## 4. Operations

| Method | Path | Permission | Purpose |
|---|---|---|---|
| POST | `/internal/v1/organizations` | `organization.write` | Create (actor becomes OWNER); body `{name, slug, profile?}`; `creationSource=SELF_SERVICE`; per-actor creation limit (5 per 24 h) |
| GET | `/internal/v1/organizations/{id}` | `organization.read` | Detail for the actor (404 when not a member), or the authoritative snapshot when no actor header (service reads) |
| PATCH | `/internal/v1/organizations/{id}` | `organization.write` | `{expectedVersion, name?, slug?, profile?}` |
| GET | `/internal/v1/users/{userId}/organizations` | `organization.read` | The user's memberships (ACTIVE and SUSPENDED organizations with status; REMOVED excluded) |
| GET | `/internal/v1/organizations/{id}/members?cursor&limit` | `organization.read` | Requires `organization.members.read` for the actor |
| PATCH | `/internal/v1/organizations/{id}/members/{membershipId}` | `organization.write` | `{expectedVersion, role?, status?}` with last-owner and no-self-escalation rules |
| POST | `/internal/v1/organizations/{id}/members/me/leave` | `organization.write` | Self-leave (refused for the last OWNER) |
| POST | `/internal/v1/organizations/{id}/ownership-transfer` | `organization.write` | `{toMembershipId, expectedVersion}`; atomic: target becomes OWNER, actor becomes ADMIN |
| POST | `/internal/v1/organizations/{id}/invitations` | `organization.write` | `{email, role}`; returns invitation + one-time `token` |
| GET | `/internal/v1/organizations/{id}/invitations?status` | `organization.read` | |
| POST | `/internal/v1/organizations/{id}/invitations/{invitationId}/revoke` | `organization.write` | Idempotent |
| POST | `/internal/v1/organization-invitations/accept` | `organization.write` | `{token}`; requires `X-CF-Acting-User-Verified-Email` equal to the invited email |
| POST | `/internal/v1/organization-invitations/decline` | `organization.write` | `{token}` |
| POST | `/internal/v1/organizations/{id}/suspend` · `/reactivate` · `/archive` | `organization.write` | Lifecycle (OWNER, or operator with `organization.administer`) |
| GET | `/internal/v1/organizations/{id}/audit-events?before&limit` | `organization.read` | Requires `organization.audit.read` |
| GET | `/internal/v1/organizations/{id}/provisioning` | `organization.read` | Truthful state, step, failure class |
| POST | `/internal/v1/organizations/memberships/resolve` | `organization.read` | `{userId, organizationIds?}` → active memberships with role, capabilities, organization status (service authorization; no actor header needed) |
| POST | `/internal/v1/organizations/authorization/evaluate` | `organization.read` | `{userId, organizationId, capability}` → `{allowed, reason, organizationStatus, role}` |
| GET | `/internal/v1/admin/organizations?cursor&limit&status` | `organization.administer` | Operator listing |
| POST | `/internal/v1/admin/organizations` | `organization.administer` | Operator create (`creationSource=PLATFORM_ADMIN`, optional `ownerUserId`) |
| POST | `/internal/v1/migration/import` | `organization.migrate` | Idempotent import of a meeting-service export (§5) |
| GET | `/internal/v1/migration/reconciliation` | `organization.migrate` | Counts, checksum, last import, mismatches |
| GET | `/health`, `/ready`, `/openapi.json` | none (not routed publicly) | Infrastructure |

## 5. Migration export format (meeting-service → Organizations)

Produced read-only by meeting-service `export_organizations` (no source write), imported by Organizations preserving every id and timestamp:

```json
{"schemaVersion":1,"source":"cfmeet.meeting-service","exportedAt":"RFC3339",
 "organizations":[{"id","name","slug","status":"ACTIVE|SUSPENDED","createdBy":"uuid|null","createdAt","updatedAt","profilePolicy":{}}],
 "memberships":[{"id","organizationId","userId","role":"OWNER|ADMIN|MEMBER","status":"ACTIVE|SUSPENDED|REMOVED","createdBy":"uuid|null","createdAt","updatedAt","removedAt":"RFC3339|null"}],
 "counts":{"organizations":0,"memberships":0},
 "checksum":"hex sha256"}
```

**Checksum (both sides compute it identically):** SHA-256 over UTF-8 lines joined with `\n` (trailing `\n` after each line):
* one line per organization, sorted by `id`: `org|{id}|{sha256hex(name)}|{slug}|{status}|{createdBy or -}|{createdAt as unix microseconds}`
* then one line per membership, sorted by `id`: `mem|{id}|{organizationId}|{userId}|{role}|{status}|{createdAt as unix microseconds}`

**Test vector** (asserted by both the Meeting Service export and the Organizations import tests): one organization `id=00000000-0000-4000-8000-000000000001`, `name="Acme Health"`, `slug=acme-health`, `status=ACTIVE`, `createdBy=null`, `createdAt=2026-10-04T12:00:00Z` (unix µs `1791115200000000`) and one membership `id=00000000-0000-4000-8000-0000000000b1`, `organizationId=00000000-0000-4000-8000-000000000001`, `userId=00000000-0000-4000-8000-0000000000a1`, `role=OWNER`, `status=ACTIVE`, same `createdAt`, give the lines `org|00000000-0000-4000-8000-000000000001|<sha256hex("Acme Health")>|acme-health|ACTIVE|-|1791115200000000\n` and `mem|00000000-0000-4000-8000-0000000000b1|00000000-0000-4000-8000-000000000001|00000000-0000-4000-8000-0000000000a1|OWNER|ACTIVE|1791115200000000\n`, checksum **`1a212f81add323e1e9bed715699c0ba8c97b2dc22e211c84a05bca42d0908e78`**. Ids are lower-case hyphenated UUIDs; sorting by id equals sorting the UUID bytes.

Import rules: refuse a different `schemaVersion`, a checksum mismatch or a count mismatch; reject memberships whose organization is absent; map source `status` to `lifecycleStatus` unchanged, `creationSource=IMPORTED`, `provisioningState=PENDING`, `joinedVia=IMPORTED`; re-import of identical rows is a no-op; a row that differs from an already-imported one is reported, not overwritten, until the cutover gate (Section 4 step 4) explicitly allows a re-sync. Every import writes an audit event and returns `{imported, unchanged, conflicts[], checksum}`.

## 6. Events

Outbox events follow [`docs/events/organization-event-contracts.md`](../events/organization-event-contracts.md). Delivery targets and transport are added per consumer in Sections 7–14; until then the outbox records and the reconciliation read are the source.

## 7. Implementation clarifications (Sections 4–5, `services/organizations`)

Where §1–§6 leave a choice open, the implementation (and `packages/contracts/organizations/internal/v1.openapi.json`) does the following. None widens access.

* **Actor requirement.** Mutations performed for a human (create, PATCH, member changes, leave, transfer, invitations, accept / decline, OWNER lifecycle) require `X-CF-Acting-User` (400 `VALIDATION_ERROR` without it). Service reads without an actor are allowed for organization detail, members, provisioning, user organizations, resolve and evaluate; invitations and audit events always need an actor.
* **Detail visibility.** An actor sees an organization only through a membership that grants `organization.read`: non-members, removed memberships and unknown ids get 404 `ORGANIZATION_NOT_FOUND`; a SUSPENDED membership gets 403 `MEMBERSHIP_REQUIRED`; a non-OWNER of a suspended / archived organization gets 409 `ORGANIZATION_SUSPENDED` / `ORGANIZATION_ARCHIVED`. An unknown membership or a membership of another organization is also 404 `ORGANIZATION_NOT_FOUND`.
* **Lifecycle.** OWNERs suspend an ACTIVE organization (reason `OWNER_REQUEST`) and may reactivate or archive only an organization they suspended; an operator (or import) suspension is lifted only by an operator. ARCHIVED is terminal. Repeating a transition already in effect is idempotent. Operators act with `organization.administer` and no acting user on `/organizations/{id}/suspend|reactivate|archive`, or on the added `PATCH /internal/v1/admin/organizations/{id}` `{status, expectedVersion?}`, which the Meeting Service platform-admin facade relays for `PATCH /api/v2/admin/organizations/{id}`. A relayed platform administrator (`X-CF-Acting-User`, only from `organization.act-for-user` callers) is recorded as `createdBy` / the audit actor of operator operations (`actor.kind=OPERATOR`).
* **Roles.** OWNERs assign any role (several OWNERs are allowed); ADMINs manage and assign MANAGER / MEMBER / GUEST only. Nobody raises their own role or changes their own status (leave instead); self-demotion is allowed subject to the last-owner rule. Invitations grant a role below the inviter's (OWNER: ADMIN…GUEST, ADMIN: MANAGER…GUEST, MANAGER: MEMBER / GUEST); OWNER is never granted by invitation. Ownership transfer requires an ACTIVE, non-OWNER target; `expectedVersion` is the organization version and the transfer increments it.
* **Versions.** Organization `version` changes with name / slug / profile / lifecycle and ownership transfer; membership changes bump the membership version once per emitted event (a role + status change bumps it twice, restrictive change first), so consumers can apply events by `aggregateVersion`.
* **Invitations.** Re-inviting an email revokes (or expires) its pending invitation and issues a new one. An idempotent replay of invitation creation re-issues a fresh token for the same still-pending invitation (the raw token is never stored, so the original cannot be returned); the response shape is the Invitation plus `token`. Accepting with a mismatched or missing verified email is 403 `PERMISSION_DENIED` and leaves the invitation pending; a user who already has a non-removed membership keeps it unchanged.
* **Slugs.** New and changed slugs follow §3; imported legacy slugs keep the Meeting Service rule (`^[a-z0-9][a-z0-9-]*$`, ≤ 100) unchanged.
* **Import.** The import route accepts bodies up to 32 MiB. `imported` / `unchanged` count rows (organizations + memberships); the report also carries per-entity tallies and `runId`. Conflict kinds: `ORGANIZATION_DIFFERS`, `MEMBERSHIP_DIFFERS` (with field names), `SLUG_TAKEN` (another organization holds the slug), `MEMBERSHIP_DUPLICATE`, `ORGANIZATION_NOT_IMPORTED`. Orphan memberships refuse the whole import (nothing is written). Reconciliation recomputes the checksum over the imported rows in their current state, so it is directly comparable with a fresh export.
* **Infrastructure codes.** Unknown routes / methods and oversized bodies answer `NOT_FOUND`, `METHOD_NOT_ALLOWED`, `PAYLOAD_TOO_LARGE` (and `INTERNAL_ERROR`) in the same envelope.

## 8. Sections 6–7 (teams, policies, technical scope) — contract 1.1.0

Decision record: [ADR 0058](../decisions/0058-first-party-organization-scopes-and-provisioning.md). Machine-readable: `cfmeet.organizations.internal-api` 1.1.0.

### 8.1 Teams

**Team** `{"id","organizationId","name":"1-120","description":"≤500|null","parentTeamId":"uuid|null","status":"ACTIVE|ARCHIVED","createdBy","createdAt","updatedAt","archivedAt","version","memberCount"}` — **TeamMember** `{"id","organizationId","teamId","membershipId","userId","role":"TEAM_MANAGER|TEAM_MEMBER","status":"ACTIVE|REMOVED","addedBy","createdAt","updatedAt","removedAt","version"}`.

* Teams are organization-scoped, flat by default, with an optional parent in the same organization (no cycles, depth ≤ 5, ≤ 500 ACTIVE teams). Names are unique among ACTIVE teams (case-folded). Users join through an ACTIVE organization membership and may be in several teams; removing an organization membership ends its team memberships (`MEMBERSHIP_ENDED`).
* OWNER / ADMIN create, change, re-parent and archive any team and assign TEAM_MANAGER — only to organization MANAGERs, ADMINs and OWNERs. A MANAGER manages only teams they are TEAM_MANAGER of (rename, add / remove TEAM_MEMBERs, step down) and creates a team only when `delegatedAdministration.managersMayCreateTeams` is true (becoming its TEAM_MANAGER). Nobody raises their own team role; everyone leaves a team themselves. A TEAM_MANAGER row grants nothing once the organization role no longer holds `organization.teams.manage`.
* Archival (OWNER / ADMIN) is refused while ACTIVE child teams exist and ends every ACTIVE team membership. Team-member history is append-only; every mutation writes an audit event and an outbox event (`team.*`).

| Method | Path | Permission | Purpose |
|---|---|---|---|
| GET / POST | `/internal/v1/organizations/{id}/teams` | `organization.read` / `organization.write` | list (`status`, cursor) / create (`Idempotency-Key`) |
| GET / PATCH | `/internal/v1/organizations/{id}/teams/{teamId}` | read / write | detail / `{expectedVersion, name?, description?, parentTeamId?}` (`null` detaches) |
| POST | `/internal/v1/organizations/{id}/teams/{teamId}/archive` | write | `{expectedVersion?}`; idempotent |
| GET / POST | `/internal/v1/organizations/{id}/teams/{teamId}/members` | read / write | list / `{membershipId \| userId, role}` |
| PATCH | `/internal/v1/organizations/{id}/teams/{teamId}/members/{teamMemberId}` | write | `{expectedVersion, role}` |
| POST | `/internal/v1/organizations/{id}/teams/{teamId}/members/{teamMemberId}/remove` | write | idempotent |

Reads need `organization.members.read` for an actor (policy-aware); service reads without an actor are allowed.

### 8.2 Policies

`GET /internal/v1/organizations/{id}/policies` → `{schemaVersion, organizationId, revision, policy, updatedBy, updatedAt, legacyMeetingProfilePolicy}`; `PATCH` `{expectedRevision, policy}` merges a partial document (compare-and-swap; stale → 409 `VERSION_CONFLICT`; unknown keys / bounds → 400). Requires `organization.policies.manage`; `delegatedAdministration` is OWNER-only. An absent policy is revision 0 with the defaults below (= the §3 matrix). `GET …/policies/effective` returns the organization-level constraints owner services combine with their own policy (**stricter wins**) plus lifecycle and provisioning state.

| Key | Default | Effect |
|---|---|---|
| `memberDirectory.visibility` (`MEMBERS`/`MANAGERS`/`ADMINS`) | `MEMBERS` | minimum role for `organization.members.read` |
| `memberDirectory.searchable` | `true` | colleague-discovery hint (Communications Hub keeps its own privacy rules) |
| `guests.invitationsAllowed` | `true` | GUEST invitations created / accepted |
| `meetings.hostCreation` (tier) | `MEMBERS` | minimum role for `meetings.organization.create` |
| `features.{meetings,messaging,calling}` | `true` | a disabled feature removes its capabilities for every role |
| `retention.hintDays` | `null` | 1–3650; a hint, owner retention applies |
| `delegatedAdministration.managersMayInvite` | `true` | MANAGER `organization.members.invite` |
| `delegatedAdministration.managersMayCreateTeams` | `false` | MANAGERs create teams |
| `delegatedAdministration.adminsMayManagePolicies` | `true` | ADMIN `organization.policies.manage` |

The Meeting Service `profile_policy` JSON is carried verbatim as `legacyMeetingProfilePolicy` (the Meeting Service still enforces it). There is no separate role-grant table in this version (ADR 0058 §3). A stored policy that no longer parses fails requests closed (503).

### 8.3 Authorization reads

* `GET /internal/v1/organizations/{id}/roles/capabilities` — every capability, each role's matrix capabilities and `effectiveCapabilities` under the current policy and status, and the team roles (UIs).
* `memberships/resolve` adds `policyRevision` and `teams[] {teamId, role}`; `authorization/evaluate` accepts `teamId` and returns `teamId` / `teamRole` — with `organization.teams.manage` a MANAGER is allowed only as that team's TEAM_MANAGER. Capabilities everywhere (`viewer`, user organizations, resolve, evaluate) are policy-aware.

### 8.4 Technical scope (Section 7)

* The provisioning saga (ADR 0058) maps each ACTIVE organization to one Platform Access FIRST_PARTY tenant + first-party application + PRODUCTION environment through `platform-identity.provision-first-party`, verifies it server-side and only then records the mapping READY and the organization `provisioningState: READY` (`organization.provisioned`, `scope.mapping_changed`). It is **off by default** (`ORGANIZATIONS_PROVISIONING_ENABLED=false`): organizations then stay `PENDING`.
* `GET …/provisioning` adds `provisioningEnabled`, `nextAttemptAt` and `scopes[]` (mapping state / version, ids, verification time, desired / applied tenant status, sync retries).
* `POST /internal/v1/organizations/scopes/resolve` `{organizationId, capability?: "primary", userId?}` (`organization.read`) returns `{organizationId, capability, tenantId, applicationId, environmentId, mappingVersion, verifiedAt}` only when the organization is ACTIVE, provisioning READY, the mapping READY and its tenant status applied in Platform Access; otherwise 409 `PROVISIONING_PENDING` / `ORGANIZATION_SUSPENDED` / `ORGANIZATION_ARCHIVED`. With `userId` (or an acting user) the user must hold an ACTIVE membership (404 otherwise). Calling is not switched to it yet.
* `POST /internal/v1/admin/organizations/{id}/provisioning/retry` (`organization.administer`) — operator retry of a FAILED / cancelled workflow (idempotent; READY and in-flight workflows unchanged; archived 409).
* Suspending / archiving an organization suspends its mapping and tenant (fail closed locally at once, Platform Access applied with retries); reactivation restores a verified mapping once the tenant is resumed.

### 8.5 Service bindings (Section 12 follow-up, contract 1.2.0; CAL-007 / ORG-012 / CAL-009)

Per-organization Enterprise Calling runs in the organization's own triple. Provisioning creates no service account, credential or scope grant (ADR 0058 §1), so an operator creates them in Platform Access (an explicit operator decision) and records their **identifiers** here. Organizations stores ids only — never a credential value — and does not verify them: Platform Access re-verifies the credential against the triple on every forwarded request (`/internal/v1/credentials/delegated-authority`), so a wrong binding can only refuse.

| Method | Path | Permission | Purpose |
|---|---|---|---|
| GET | `/internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling` | `organization.administer` | the binding (404 `NOT_FOUND` when none; 404 `ORGANIZATION_NOT_FOUND` for an unknown organization) |
| PUT | same | `organization.administer` | `{serviceAccountId, credentialId, expectedVersion?}` (non-nil UUIDs; `expectedVersion` 0 = none yet) → the binding; repeating the recorded ids is a no-op (same version, nothing audited); stale `expectedVersion` 409 `VERSION_CONFLICT`; no recorded triple 409 `PROVISIONING_PENDING`; archived 409 `ORGANIZATION_ARCHIVED` |
| DELETE | same | `organization.administer` | idempotent removal `{organizationId, capability, service, removed}` (allowed for archived organizations) |

**ServiceBinding** `{"organizationId","capability":"primary","service":"calling","serviceAccountId","credentialId","version","boundBy":"uuid|null","createdAt","updatedAt"}` — `boundBy` is the relayed acting operator (`X-CF-Acting-User`, only from `organization.act-for-user` callers), else `null`. Changes write `scope.service_binding_set` / `scope.service_binding_cleared` audit events (actor `OPERATOR`; ids only). No outbox event: consumers read the binding through `scopes/resolve` on every request.

`POST /internal/v1/organizations/scopes/resolve` adds `serviceBindings: {"calling": {serviceAccountId, credentialId, version} | null}` to its 200 answer; every refusal is unchanged (a suspended or archived organization, or a scope that is not READY, never exposes a binding). Table `organization_scope_service_bindings` (migration `m20261108_000003_scope_service_bindings`, additive; PK organization + capability + service; `service` CHECK `calling`).

### 8.6 Automatic service bindings (ADR 0058 Amendment A2, contract 1.5.0)

With `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true` (requires provisioning), no operator step is needed. After the scope is verified, the provisioning worker asks Platform Access (`platform-identity.bind-organization-services`, `PUT /internal/v1/organization-scopes/{org}/primary/service-bindings/{calling|messaging}`) to create the service account and credential inside the organization's own triple:

- `calling` gets `calls.read`, `enterprise.config.read` and `enterprise.config.write`; `messaging` gets `messages.read`.
- The credential secret is never returned.
- Organizations reads each binding back and verifies the same triple, `verified`/`active`, the exact scopes and matching ids. Only then does it record the ids (`source: AUTOMATIC`, `verifiedAt`, `credentialExpiresAt`).
- The organization becomes READY only after both bindings are recorded. Workflow steps: `SCOPE_VERIFIED` → `BINDING_CALLING_VERIFIED` → `BINDING_MESSAGING_VERIFIED` → `VERIFIED`.

**Failures** set the workflow to FAILED with one of `SERVICE_BINDING_CONFLICT`, `SERVICE_BINDING_VERIFICATION_FAILED`, `PLATFORM_ACCESS_UNAVAILABLE` (after backoff) or `PLATFORM_ACCESS_REFUSED`. They are recoverable with the existing operator retry, and retries are idempotent on both sides.

**Reconciliation** backfills bindings for already-READY organizations and renews credentials that expire within 30 days. Drift removes the local binding and fails the workflow (`SERVICE_BINDING_DRIFT`).

**Archiving, or an operator DELETE of an AUTOMATIC binding,** marks the local row revoked in the same transaction, so resolve stops returning it at once. A durable sync then revokes it at Platform Access, retrying until it is applied. Automation never recreates a revoked binding.

**Operator bindings** (`PUT`, `source: OPERATOR`) are never overwritten.

**Operator rotation:** `POST /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}/rotate` (`organization.administer`, audited `scope.service_binding_rotated`).

Audit actions (ids only): `scope.service_binding_provisioned`, `scope.service_binding_rotated`, `scope.service_binding_revoked`, `scope.service_binding_revocation_applied`. Migration `m20261113_000005_automatic_service_bindings` is additive.
