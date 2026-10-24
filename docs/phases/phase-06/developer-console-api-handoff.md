# Developer Console — backend API handoff (Part VI Chapter 18)

**Status: `BACKEND_API_READY` · `FRONTEND_IMPLEMENTATION_DEFERRED`.** The console UI
(`apps/web`) is not implemented or changed by this work.

## How the console calls the backend

* **Origin:** the public API Gateway only (`https://api.cfmeet.com`). The console never calls
  Platform Access, the Developer Platform or a domain service directly, and never reads
  their databases.
* **Authentication:** the existing CF Meet user session as `Authorization: Bearer <session>`
  (gateway auth mode `DEVELOPER_SESSION`; the Developer Platform verifies it through the
  Meeting Service session bridge). Platform credentials (`cfmk_…`) are machine credentials:
  **never** use or store one in the browser.
* **Contracts:** request and response schemas are in
  `packages/contracts/platform-developer/http/v1.openapi.json` (1.5.0). The `$defs` named
  below live in `control/v1.schema.json` (C) and `webhooks/v1.schema.json` (W). The
  customer-facing rendering is `GET /openapi/developer-platform.json` on the gateway.
  Typed clients are in `@cfmeet/platform-sdk` (`client.developerPlatform.*`) and
  `cfmeet_platform_sdk`.
* **Errors:** `{"error": {"code", "message", "requestId"}}`. Codes come from
  `platform-developer/common/v1.schema.json`. A resource of another organization is
  `404 NOT_FOUND` (it never confirms existence). Other codes: `403 PERMISSION_DENIED` for the
  wrong role, `401` for no or expired session, `409 VERSION_CONFLICT` / `INVALID_STATE`,
  `422 POLICY_VIOLATION` / `IDEMPOTENCY_KEY_REUSED`, and `429` with `Retry-After` from the
  gateway.
* **Correlation:** every response carries `X-Request-Id`; show it in error toasts for
  support.
* **Pagination:** list operations take `?limit=1..100&cursor=…` and return
  `{items, nextCursor}`. Treat cursors as opaque.
* **Idempotency:** creates accept `Idempotency-Key`. A replay returns the stored response
  with `Idempotent-Replayed: true`.
* **Roles** (from `services/developer-platform/src/domain/rbac.rs`): OWNER, ADMIN,
  DEVELOPER, ANALYST, BILLING. Hide actions the member's role cannot perform, but rely on
  the server answer: the role is re-checked on every call.

## Screen-to-endpoint matrix

All paths are under the gateway origin. "Roles" is who may call; "Schemas" names the
response and request `$defs`.

| Screen | Method / path | Roles | Schemas |
|---|---|---|---|
| **Organization switcher** | `GET /v1/organizations` (paged) | any member | C `OrganizationList` |
| | `POST /v1/organizations` | any signed-in user | C `CreateOrganizationRequest` → `Organization` |
| | `GET`/`PATCH /v1/organizations/{id}`, `POST …/close` | read: all; update: OWNER, ADMIN; close: OWNER | C `Organization`, `UpdateOrganizationRequest` |
| **Members and roles** | `GET /v1/organizations/{id}/members` (paged) | all | C `MemberList` |
| | `PATCH`/`DELETE /v1/organizations/{id}/members/{memberId}` (self-removal = leave) | OWNER (self: any) | C `ChangeRoleRequest` → `Member` |
| | `GET`/`POST …/invitations`, `POST …/invitations/{id}/revoke`, `POST /v1/invitations/accept` | list: all; create/revoke: OWNER; accept: the invited user | C `CreateInvitationRequest` → `InvitationWithToken`, `InvitationList`, `AcceptInvitationRequest` |
| | `POST …/ownership-transfer` | OWNER | C `TransferOwnershipRequest` |
| **Projects** | `GET`/`POST /v1/organizations/{id}/projects` | read: all; create: OWNER, ADMIN | C `ProjectList`, `CreateProjectRequest` → `Project` |
| | `GET`/`PATCH /v1/projects/{id}`, `POST …/archive`, `…/suspend`, `…/resume` | read: all; manage: OWNER, ADMIN | C `Project`, `UpdateProjectRequest` |
| **Environments** (SANDBOX / TEST / PRODUCTION) | `GET`/`POST /v1/projects/{id}/environments` | read: all; create SANDBOX/TEST: OWNER, ADMIN | C `EnvironmentList`, `CreateEnvironmentRequest` → `Environment` (`kind`, `status`, `productionAccessState`) |
| | `GET /v1/environments/{id}`, `POST …/suspend`, `…/resume` | read: all; manage: OWNER, ADMIN (non-production only) | C `Environment` |
| **Service accounts** | `GET`/`POST /v1/environments/{id}/service-accounts`, `GET …/{sa}`, `POST …/{sa}/suspend`, `…/resume` | read: OWNER, ADMIN, DEVELOPER; manage: OWNER, ADMIN | C `ServiceAccountList`, `CreateServiceAccountRequest` → `ServiceAccount` |
| **API credentials** | `GET`/`POST /v1/environments/{id}/service-accounts/{sa}/credentials` (paged) | OWNER, ADMIN, DEVELOPER | C `CredentialList`, `CreateCredentialRequest` → **`IssuedCredential`** (one-time secret) |
| | `GET /v1/environments/{id}/credentials/{cid}`, `POST …/rotate`, `…/revoke`, `…/suspend`, `…/resume` | OWNER, ADMIN, DEVELOPER (revoke and suspend also while RESTRICTED) | C `Credential`, `RotateCredentialRequest` → **`IssuedCredential`**, `SuspendCredentialRequest` |
| | `GET`/`PUT /v1/organizations/{id}/credential-policy` | read: all; put: OWNER, ADMIN | C `CredentialPolicy`, `PutCredentialPolicyRequest` |
| **Scopes and entitlements** | `GET /v1/scopes` | all | C `ScopeRegistry` (`selfServiceGrantable`, `productionCapability`, `regulated`) |
| | `GET`/`POST /v1/projects/{id}/scopes`, `DELETE …/scopes/{scope}` | read: all; grant/withdraw: OWNER, ADMIN | C `ProjectScopes`, `GrantScopeRequest` → `ScopeGrant`, `ScopeWithdrawal` |
| | `GET /v1/environments/{id}/entitlements`, `GET /v1/environments/{id}/limits` | OWNER, ADMIN, DEVELOPER, ANALYST | C `Entitlements`, `EnvironmentLimits` (`limitingLevel`, `remaining`, `warnings`) |
| | `PUT /v1/environments/{id}/service-accounts/{sa}/limits`, `PUT …/credentials/{cid}/limits` | OWNER, ADMIN, DEVELOPER | C `QuotaRestrictionRequest` → `QuotaRestriction` (stricter only) |
| **Webhooks** | `GET`/`POST /v1/environments/{id}/webhooks` (paged) | read: OWNER, ADMIN, DEVELOPER, ANALYST; manage: OWNER, ADMIN, DEVELOPER | W `ConsoleWebhookList`, `CreateWebhookRequest` → **`ConsoleWebhookWithSecret`** |
| | `GET`/`PATCH`/`DELETE …/webhooks/{wid}`, `POST …/{wid}/rotate-secret` | as above | W `ConsoleWebhook`, `UpdateWebhookRequest`, `RotateWebhookSecretRequest` → **`ConsoleWebhookWithSecret`** |
| | `GET …/webhooks/{wid}/deliveries?status=` (paged), `GET /v1/environments/{id}/webhook-deliveries/{did}/attempts` (paged), `POST …/{did}/replay` | read: as above; replay: manage roles | W `ConsoleDeliveryList`, `WebhookAttemptList`, `WebhookDelivery` |
| **Usage, limits, history, export** | `GET /v1/environments/{id}/usage?from&to&usageType` | all roles | C `UsageSummary` (daily rollups and totals, `sandbox` flag) |
| | `GET …/usage/facts?from&to&usageType` (paged), `GET …/usage/export?format=csv\|json&granularity=daily\|facts`, `GET …/usage/warnings` | all roles | C `UsageFactList`, CSV / JSON download, `UsageWarnings` |
| **Production access** | `GET /v1/projects/{id}/production-access` | all | C `ProductionAccessStatus` |
| | `GET`/`POST /v1/projects/{id}/production-access-requests` (paged) | read: all; submit: OWNER | C `ProductionAccessRequestList`, `SubmitProductionAccessRequest` → `ProductionAccessRequest` (with `history`) |
| | `GET /v1/production-access-requests/{rid}`, `POST …/cancel`, `POST …/activate` | read: all; cancel/activate: OWNER | C `ProductionAccessRequest` |
| **API docs and SDK discovery** | `GET /discovery` (no auth) | public | families, per-domain OpenAPI links, **`sdks.packages[]`** (package, version, install, quick-start), pinned contracts |
| | `GET /openapi.json`, `GET /openapi/{domain}.json`, `GET /docs` (no auth) | public | aggregate / per-domain public OpenAPI 3.1 |
| **Activity** | `GET /v1/organizations/{id}/activity?projectId&action` (paged, newest first) | OWNER, ADMIN, ANALYST | C `ActivityList` (actor, action, target, outcome, request id, safe summary) |
| **Commercial** | `GET /v1/organizations/{id}/commercial`, `…/plan-assignments` (paged), `GET /v1/commercial-plans`, `PUT …/billing-account` | read: OWNER, ADMIN, BILLING; billing account: OWNER, BILLING | C `OrganizationCommercial`, `PlanAssignmentList`, `CommercialPlanList`, `PutBillingAccountRequest` → `BillingAccount` |
| **Support and incident contacts** | `GET`/`PUT /v1/organizations/{id}/support-profile` | read: all; put: OWNER, ADMIN | C `SupportProfile` (tier, reference, contact member ids, `productionStatus`, `lastIncident`), `PutSupportContactsRequest` |

## Sensitive data rules for the frontend

* **One-time secrets:** these responses carry `secret` (or `signingSecret`) exactly once:
  `IssuedCredential` from create/rotate credential, and `ConsoleWebhookWithSecret` from
  create webhook/rotate secret.
  * Show the secret once, with copy-to-clipboard and an explicit "I have stored it"
    confirmation.
  * Never persist it: no `localStorage`, `sessionStorage`, IndexedDB, logs, analytics,
    URLs or crash reports. Clear it from component state when the dialog closes.
  * Responses carry `Cache-Control: no-store`.
  * A repeated request with the same `Idempotency-Key` returns the metadata with
    `secretRedacted: true`, never the secret. If the secret was lost, tell the user to
    rotate.
* Credential lists and metadata show the masked form and `publicId` only.
* Webhook endpoints may be shown, but never echo delivery payloads beyond what the delivery
  APIs return.
* Support contacts are member ids. Resolve names from the member list; there are no email
  addresses or phone numbers.
* PRODUCTION environments cannot be created from the console (`422 POLICY_VIOLATION`). They
  appear only after `POST …/production-access-requests/{rid}/activate` on an APPROVED
  request.

## Backend limitations (recorded)

* Usage daily rollups update as producers export. A domain's usage appears after its
  exporter runs (off by default: `CFMEET_USAGE_EXPORT_ENABLED`). `/v1/usage` and
  `/v1/limits` are served by the Developer Platform only while
  `API_GATEWAY_CENTRAL_USAGE_ENABLED` is on.
* Webhook console routes are session-only. Machines use the `/v1/webhooks*` machine API.
* Operator decisions (production review/approval, incidents, tiers) are operator-console
  APIs under `/internal/v1/operator/**`. They are never available from the customer
  console.
* There is no ticketing system. The support profile is routing metadata only.
