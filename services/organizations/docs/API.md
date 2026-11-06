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

The registry in `packages/service-auth/src/permissions.rs` refuses any trust bundle that
grants these elsewhere.

## Request rules (summary)

* Errors: `{"schemaVersion":1,"error":{"code","message","requestId"}}`, `Cache-Control: no-store`.
* Unknown body fields are refused; no user id, role or tenant is ever taken from a body.
* `Idempotency-Key` on create, admin create, invite and accept (24 h replay window).
* `expectedVersion` on organization PATCH (organization version), member PATCH
  (membership version) and ownership transfer (organization version).
* Bodies: 64 KiB (import: 32 MiB).
