# ADR 0058 — Governed FIRST_PARTY organization scopes and the Organizations provisioning saga

**Status:** Accepted (Blueprint v1.0 Sections 6–7; staging only — provisioning is OFF by default and nothing here is a production change)
**Date:** 2026-11-07
**Builds on:** [ADR 0032](0032-platform-access-shared-identity-authority.md) (Platform Access is the only identity authority), [ADR 0056](0056-first-party-organizations-and-workspace-boundaries.md) §1, §4, §6
**Resolves:** the ADR 0056 §4 validation gate ("confirm that Platform Access can create FIRST_PARTY tenants for organizations through an authenticated path; otherwise add that capability to Platform Access")

## Context

ADR 0056 §4 decided that every READY first-party organization maps to one dedicated FIRST_PARTY tenant with one first-party application and one environment, recorded in `organization_platform_scopes` and marked READY only after Platform Access confirms it server-side. Platform Access offered two ways to create tenants:

* the Developer Platform delegation (`platform-identity.delegate`, Part VI §50) — hard-coded EXTERNAL, SANDBOX / TEST environments only, grantable only to `cfmeet.developer-platform`;
* operator administration (`platform-identity.administer`) — operator tooling only.

Neither may be used by the Organizations Service: widening the EXTERNAL delegation would let a customer-facing control plane mint first-party tenants, and granting operator administration to a service would break ADR 0032's least privilege.

## Decision

### 1. A dedicated, minimal Platform Access path (`platform-identity.provision-first-party`)

* New internal permission `platform-identity.provision-first-party`: receiver `cfmeet.platform-access`, grantable to `cfmeet.organizations` ONLY, sensitive. The handlers additionally require the authenticated identity to be `cfmeet.organizations` (defence in depth over the registry).
* Routes (`/internal/v1/organization-scopes/{organizationRef}/{capability}`):
  * `PUT` — create (201) or return (200) the organization's triple. Idempotency key = organization id + capability. The tenant (classification FIRST_PARTY, reserved slug `org-<32 hex>`), the application (`organization`), the PRODUCTION-kind environment and the immutable `platform_organization_scopes` row are created in ONE Platform Access transaction — there is never a half-created triple to resume; concurrent calls serialize on an advisory lock. A tenant already holding the reserved slug outside the registry is never adopted (409).
  * `GET` — read back with the server-side facts the caller verifies (`verified`: FIRST_PARTY, reserved slug, ownership chain, PRODUCTION; `active`).
  * `POST …/status` — suspend / resume the TENANT (every subordinate credential then fails authentication immediately); idempotent.
* `capability` is a closed vocabulary with one value, `primary` (ADR 0056 §4: one triple per organization carries Calling, Messaging and every other organization-scoped resource).
* ADR 0032 guarantees are unchanged: Platform Access remains the only tenant authority; this path issues no credential, creates no service account or principal, grants no scope (the application starts with none; scope grants for Calling / Messaging stay explicit operator decisions in Section 12/13) and can never reach an EXTERNAL tenant or a developer workspace. The organization's PRODUCTION environment is not a production-access activation, so the Developer Platform delegation cannot change it either.
* Every committed change writes the operator audit (`platform_access_audit`, actor `cfmeet.organizations`) with the organization id as an opaque reference (`detail.organizationRef`); new action `ORGANIZATION_SCOPE_PROVISIONED`.
* Migration `m20261121_000005_first_party_organization_scopes` (append-only table, composite foreign key tenant ⊃ application ⊃ environment, one scope per tenant).

### 2. The Organizations provisioning saga

* One workflow row per organization (`organization_provisioning`, stable key `org-scope:{organizationId}:primary`), enqueued by the worker for every ACTIVE organization in PENDING (imports included). Steps: `REQUESTED` → Platform Access `PUT` → `SCOPE_CREATED` (checkpoint: ids recorded, mapping VERIFYING) → Platform Access `GET` + server-side verification (+ tenant resumed if needed) → `VERIFIED` (mapping READY with `verified_at`, evidence and `mapping_version`; organization READY; `scope.mapping_changed` and `organization.provisioned` in the same transaction).
* Never a distributed transaction and never another service's database: each step is one signed call or one local transaction guarded by the workflow lease. A local failure abandons the step; the lease expires and the idempotent `PUT` returns the same triple.
* Failures: Platform Access unavailable → capped exponential backoff (5 s … 15 min), FAILED (`PLATFORM_ACCESS_UNAVAILABLE`) after 8 attempts; refusal → FAILED at once (`PLATFORM_SCOPE_CONFLICT`, `PLATFORM_ACCESS_REFUSED`); a read-back that does not verify → `SCOPE_VERIFICATION_FAILED`; a different triple than recorded, or a tenant mapped to another organization → `SCOPE_MISMATCH` (no split-brain mapping; the schema also refuses one tenant in two mappings). FAILED is explicit, carries the error class and waits for an operator retry (`POST /internal/v1/admin/organizations/{id}/provisioning/retry`, `organization.administer`).
* Compensation: a FAILED workflow marks any recorded tenant for suspension. Lifecycle: suspending or archiving an organization marks its mapping SUSPENDED and the desired tenant status SUSPENDED in the same transaction (fail closed locally — scope resolution refuses at once); reactivation restores verified mappings. A sync step applies the desired status to Platform Access with backoff and never gives up. Reconciliation re-verifies READY mappings every 6 h and records drift (`SCOPE_DRIFT`).
* `POST /internal/v1/organizations/scopes/resolve` returns the triple only when the organization is ACTIVE, provisioning READY, the mapping READY and the tenant status applied. Calling is NOT switched to it in this section.
* Provisioning is OFF by default (`ORGANIZATIONS_PROVISIONING_ENABLED=false`): organizations stay PENDING and `GET …/provisioning` reports `provisioningEnabled: false`. When enabled, Organizations signs with its own key (`CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`) and calls `CF_PLATFORM_ACCESS_BASE_URL`.

### 3. Section 6 decisions recorded here

* **No `organization_role_grants` table.** The fixed roles plus the typed policy's delegated-administration flags (`managersMayInvite`, `managersMayCreateTeams`, `adminsMayManagePolicies`; OWNER-only) cover delegated administration without a second grant model. A policy can only remove capabilities the role matrix grants and never restricts OWNER administration; per-capability grants (e.g. `calling.enterprise.manage` to a non-admin) are deferred until a concrete owner need exists.
* Organization policies are organization-level constraints; owner services combine them with their own policy, stricter wins (`GET …/policies/effective`). The legacy Meeting `profile_policy` is carried verbatim.

## Consequences

* Platform Access gains one sensitive permission, three routes, one table and one audit action. Organizations gains its first outbound dependency (Platform Access only).
* Enabling provisioning in an environment requires: an Organizations signing key and its public key in Platform Access's trust bundle with `platform-identity.provision-first-party`, Platform Access migration 000005, Organizations migration 000002 and `ORGANIZATIONS_PROVISIONING_ENABLED=true`. That is a release operation, not part of this section.
* Scope grants for the organization's application and the Calling / Messaging switch-over to per-organization scopes remain Sections 12–13.

## Alternatives rejected

* **Widen the Developer Platform delegation to FIRST_PARTY / PRODUCTION.** Mixes the customer-facing workspace control plane with first-party organizations (ADR 0056 §1) and gives the Developer Platform a capability it does not need.
* **Grant `platform-identity.administer` to Organizations.** Operator-wide administration for a service; violates least privilege.
* **Create the triple step by step through separate calls from Organizations.** Leaves half-created triples in Platform Access on any failure; the single transaction in Platform Access makes partial success impossible on that side.

## Amendment A1 — Calling consumption of organization scopes (Blueprint v1.0 Section 12 follow-up, 2026-11)

**Resolves (software):** CAL-007, ORG-012, CAL-009 — see [`org-policy-calling-scope-section-12.md`](../operations/org-policy-calling-scope-section-12.md). Off by default; nothing changes in production until the release steps there.

1. **Organizations stores a Calling service binding, never a credential.** `organization_scope_service_bindings` holds the ids of ONE service account and ONE credential in the organization's own FIRST_PARTY triple (operator route `PUT/GET/DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling`, `organization.administer`, audited); `scopes/resolve` returns them as `serviceBindings.calling`. The service account, credential and Calling scope grants are created by a Platform Access operator — §1 above is unchanged: the provisioning path still issues no credential and grants no scope. A binding outside the triple is refused by Platform Access at use (fail closed).
2. **Meeting forwards organization Enterprise administration to Calling without a customer credential.** New internal permission `calling-request.act-for-organization` (receiver Calling, grantable to `cfmeet.meeting-service` only, sensitive). Meeting's signed delegated context names the organization triple, the bound service account, the credential (id only) and the acting member's NATIVE_CF_USER principal (ensured in the organization's environment). Calling accepts it only with `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`, re-verifies the credential live through the existing `credentials/delegated-authority` path (the Part VI §51 gateway mechanism), requires a FIRST_PARTY tenant and a principal of the same triple, and records the principal as `actor_principal_id` on enterprise audit.
3. **Enterprise administration is an Organizations capability.** Meeting requires `calling.enterprise.manage` and `features.calling`, evaluated by the same code as the workspace capabilities read (ADR 0056 §5–§6); Organizations unavailable → 503. The web adapter's deployment-wide principal allowlist becomes a legacy path, retired by the frontend cutover.
4. **Alternatives rejected:** storing per-organization credential secrets in Meeting (secret sprawl); a credential-less "organization authority" in Platform Access (would bypass the credential-bound scopes, limits and revocation Calling relies on); widening `calling-request.forward` to Meeting (that grant means "the gateway authenticated a customer secret").

## Amendment A2 — Automatic organization service bindings (owner decision, 2026-11-13)

**Replaces:** the manual operator step of Amendment A1 §1 and of the final activation runbook §4.4 ("an operator creates a service account and credential inside the organization's triple, then records them") for every organization created after `activate-organization-bindings`. The operator routes stay as break-glass.

1. **A second, equally narrow Platform Access path (`platform-identity.bind-organization-services`).** Sensitive, receiver `cfmeet.platform-access`, grantable to `cfmeet.organizations` ONLY; the handlers also require the authenticated identity to be `cfmeet.organizations`. Routes `PUT | GET | DELETE /internal/v1/organization-scopes/{organizationRef}/{capability}/service-bindings/{service}` and `POST …/{service}/rotate`. The binding is created ONLY inside the triple recorded in `platform_organization_scopes` for that organization (never a caller-supplied tenant, never EXTERNAL, never a developer workspace). The service vocabulary is closed, and so is each scope set:

   | service | service account | credential scopes |
   |---|---|---|
   | `calling` | `org-calling` | `calls.read`, `enterprise.config.read`, `enterprise.config.write` |
   | `messaging` | `org-messaging` | `messages.read` |

   `PUT` grants exactly those scopes to the organization's application, creates the service account and issues the credential in ONE transaction under an advisory lock, and records `platform_organization_service_bindings`. The credential **secret is discarded at issuance and never returned**: nobody holds it. Consumers only name the credential id, and Platform Access re-verifies it live at every use (Amendment A1 §2). `PUT` is idempotent and self-healing: it renews a credential that expires within 30 days or no longer authenticates. A same-named service account outside the registry is never adopted (409). `rotate` issues a successor and revokes the predecessor. `DELETE` revokes the credential and the service account; it is terminal, and a later `PUT` answers 409. Every change is written to `platform_access_audit` with actor `cfmeet.organizations` and `detail.organizationRef`.
2. **ADR 0032 / §1 guarantees kept.** Platform Access remains the only identity authority and the only holder of credential material. Organizations gets no operator administration (`platform-identity.administer` is never granted to a service). The scope set cannot be widened by the caller. Suspending the tenant (§2) still disables every binding at once.
3. **The saga gains binding steps.** With `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true`, a VERIFIED scope does not make the organization READY by itself. The workflow then `PUT`s each binding, reads it back, and verifies server-side facts: the same triple, `verified` and `active`, the exact scope set, and matching ids. It then records the ids in `organization_scope_service_bindings` (`source = AUTOMATIC`, `verified_at`, `credential_expires_at`). Only when both bindings are recorded does the organization become READY. Failures use the existing backoff, FAILED classes and operator retry (`SERVICE_BINDING_CONFLICT`, `SERVICE_BINDING_VERIFICATION_FAILED`, `PLATFORM_ACCESS_UNAVAILABLE`, `PLATFORM_ACCESS_REFUSED`). Retries are idempotent on both sides.
4. **Lifecycle.**
   - **Reconcile** (every 6 h) backfills missing bindings of READY organizations, renews credentials through `PUT` and records drift (`SERVICE_BINDING_DRIFT`).
   - **Archive** removes the local automatic bindings in the archive transaction (fail closed: `scopes/resolve` stops returning them immediately). A durable sync then `DELETE`s them at Platform Access with retries until applied.
   - **Suspension** is covered by tenant suspension (§2).
   - **Operator rotation:** `POST /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{service}/rotate`.
   - **Operator bindings** (`source = OPERATOR`) are never overwritten by automation.
5. **Messaging policy baseline.** A new organization environment starts from Messaging's platform defaults, under which MANAGED conversations are unavailable. On first organization Messaging use, the Meeting Service applies the first-party baseline through its existing `messaging-application.delegate` authority: GROUP and DIRECT MANAGED-only, attachments on, Messaging's managed scan still required. It is applied only over a never-set policy (`expectedVersion: 0`), and never enables PRIVATE_E2EE.
6. **Activation.** Coordinated-release operation `activate-organization-bindings` (after `activate-provisioning`). It grants the permission in the Platform Access bundle and sets the flag. `activate-enterprise-calling` now requires it.

**Alternatives rejected:**
- Widening `platform-identity.provision-first-party` to issue credentials: it would mix tenant provisioning with credential issuance under one grant.
- Granting `platform-identity.administer` to Organizations: operator-wide authority, which violates least privilege.
- Returning the credential secret to Organizations for storage: secret sprawl, and nothing needs it.
