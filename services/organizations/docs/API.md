# Organizations Service API

The API is the frozen internal contract
[`docs/api/organizations-internal-v1.md`](../../../docs/api/organizations-internal-v1.md)
(§7 records the implementation's resolution of open points). Its machine-readable form is
[`packages/contracts/organizations/internal/v1.openapi.json`](../../../packages/contracts/organizations/internal/v1.openapi.json)
(`cfmeet.organizations.internal-api`), served unchanged at `GET /openapi.json`.

## Callers and permissions

| Permission (receiver `cfmeet.organizations`) | Grantable to | Used for |
|---|---|---|
| `organization.read` | Meeting Service, Calling, Messaging, Communications | detail / snapshot, members, user organizations, provisioning, resolve, evaluate, audit (actor) |
| `organization.write` | Meeting Service | every mutation for an acting user |
| `organization.act-for-user` (sensitive) | Meeting Service | `X-CF-Acting-User`, `X-CF-Acting-User-Verified-Email` |
| `organization.administer` (sensitive) | operator console, Meeting Service (platform-admin facade) | admin listing / create / lifecycle |
| `organization.migrate` (sensitive) | operator console, Meeting Service | import, reconciliation |

Outbound (Section 7, only when provisioning is enabled): `platform-identity.provision-first-party`
(receiver `cfmeet.platform-access`, grantable to `cfmeet.organizations` only) for
`PUT` / `GET` `/internal/v1/organization-scopes/{organizationId}/primary` and `POST …/status`.

Sections 6–7 routes (contract §8): teams and team members, `GET/PATCH …/policies`,
`GET …/policies/effective`, `GET …/roles/capabilities`, `POST /internal/v1/organizations/scopes/resolve`
and `POST /internal/v1/admin/organizations/{id}/provisioning/retry`.

Section 12 follow-up (contract §8.5, 1.2.0; CAL-007 / ORG-012 / CAL-009):
`GET` / `PUT` / `DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling`
(`organization.administer`) record the Platform Access service account and credential **ids**
an operator created inside the organization's own triple for Enterprise Calling;
`scopes/resolve` answers them as `serviceBindings.calling` (`null` when none). Organizations
never stores a credential value and never verifies the ids — Platform Access re-verifies the
credential against the triple on every forwarded request.

ADR 0058 Amendment A2 (1.5.0): with `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED` the provisioning
saga creates, verifies and records the Calling and Messaging bindings itself
(`source: AUTOMATIC`; `verifiedAt`, `credentialExpiresAt`) and the organization is READY only
once both are verified; `POST …/service-bindings/{calling|messaging}/rotate`
(`organization.administer`) rotates an automatic binding's credential.

The registry in `packages/service-auth/src/permissions.rs` refuses any trust bundle that
grants these elsewhere.

## Request rules (summary)

* Errors: `{"schemaVersion":1,"error":{"code","message","requestId"}}`, `Cache-Control: no-store`.
* Unknown body fields are refused; no user id, role or tenant is ever taken from a body.
* `Idempotency-Key` on create, admin create, invite and accept (24 h replay window).
* `expectedVersion` on organization PATCH (organization version), member PATCH
  (membership version), ownership transfer (organization version), team PATCH / archive
  (team version) and team-member PATCH (team-membership version); `expectedRevision` on
  policy PATCH (compare-and-swap).
* Bodies: 64 KiB (import: 32 MiB).
