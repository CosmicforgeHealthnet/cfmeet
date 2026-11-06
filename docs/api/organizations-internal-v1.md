# Organizations Service — internal contract v1 (Blueprint Section 3 freeze)

**Owner:** `services/organizations` (`cfmeet.organizations`). **Decision record:** [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md). **Machine-readable contract:** `packages/contracts/organizations/internal/v1.openapi.json` (generated from this page; this page wins on conflict until that file exists).

All routes are under `/internal/v1`, require a valid `packages/service-auth` token (`X-CF-Service-Authorization`) whose issuer holds the stated permission in the **receiver's** trust bundle, and refuse replays (per-token nonce table). No route is reachable from public ingress.

## 1. Actor propagation

Operations performed for a human carry the acting user in `X-CF-Acting-User: <uuid>`. Only callers holding `organization.act-for-user` (initially `cfmeet.meeting-service`, which has just authenticated the human with the account authority) may set it; from any other caller the header is refused (`403 ACTOR_NOT_ALLOWED`). Organizations never accepts a user id, role, tenant or organization role from a request body as authority. Optional `X-CF-Acting-User-Verified-Email: <lowercased email>` is accepted from the same callers only, for email-bound invitation acceptance.

## 2. Envelope and errors

JSON, camelCase. Errors: `{"schemaVersion":1,"error":{"code","message","requestId"}}` with `Cache-Control: no-store`. Stable codes: `VALIDATION_ERROR` 400, `ORGANIZATION_NOT_FOUND` 404 (also for organizations the actor may not see — non-enumerating), `MEMBERSHIP_REQUIRED` 403, `PERMISSION_DENIED` 403, `ORGANIZATION_SUSPENDED` 409, `ORGANIZATION_ARCHIVED` 409, `LAST_OWNER` 409, `VERSION_CONFLICT` 409, `SLUG_TAKEN` 409, `INVITE_EXPIRED` 410, `INVITATION_INVALID` 404, `IDEMPOTENCY_CONFLICT` 409, `PROVISIONING_PENDING` 409, `RATE_LIMITED` 429, `ACTOR_NOT_ALLOWED` 403, `INTERNAL_AUTHENTICATION_FAILED` 401, `INTERNAL_PERMISSION_DENIED` 403, `SERVICE_UNAVAILABLE` 503. Writes accept `Idempotency-Key` (replay with the same body returns the stored response; a different body returns `IDEMPOTENCY_CONFLICT`). Updates take `expectedVersion` (optimistic concurrency).

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

A SUSPENDED or ARCHIVED organization grants no capability except `organization.read` to its OWNERs (so they can see the status). A non-ACTIVE membership grants nothing.

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

Import rules: refuse a different `schemaVersion`, a checksum mismatch or a count mismatch; reject memberships whose organization is absent; map source `status` to `lifecycleStatus` unchanged, `creationSource=IMPORTED`, `provisioningState=PENDING`, `joinedVia=IMPORTED`; re-import of identical rows is a no-op; a row that differs from an already-imported one is reported, not overwritten, until the cutover gate (Section 4 step 4) explicitly allows a re-sync. Every import writes an audit event and returns `{imported, unchanged, conflicts[], checksum}`.

## 6. Events

Outbox events follow [`docs/events/organization-event-contracts.md`](../events/organization-event-contracts.md). Delivery targets and transport are added per consumer in Sections 7–14; until then the outbox records and the reconciliation read are the source.
