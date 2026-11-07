# Organization policy enforcement and per-organization Enterprise Calling — Blueprint v1.0 Section 12 follow-up

Closes the Section 8 gap (Meeting writes did not apply organization policy) and builds the
software for CAL-007 / ORG-012 / CAL-009 (one shared Calling credential and scope for every CF
Meet user, enterprise admin by environment allowlist, admin actions without the human actor).
Decision records: [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md)
§3–§6, [ADR 0058](../decisions/0058-first-party-organization-scopes-and-provisioning.md) and its
**Amendment A1** (this section). Nothing here is deployed, and nothing changes active production
configuration: every new path is off by default.

## 1. Meeting write paths enforce organization policy

**Rule.** In `ORGANIZATIONS_AUTHORITY_MODE=authoritative`, the Meeting write paths that act on an
organization apply the same decision that `GET /api/v2/me/workspaces/{id}/capabilities` reports.
There is one evaluator. `services/meeting-service/src/services/workspaces.rs::authoritative_organization_answer`
reads the Organizations membership list, `…/roles/capabilities` and `…/policies/effective`, and
returns the capabilities answer. `require_organization_capability` is that same answer plus a pure
decision, `decide`. The wiring is in `services/organization_policy.rs`.

| Action | Requirement |
|---|---|
| `POST /api/v2/meeting-spaces` with `organizationId`; `POST /api/v2/meeting-spaces/{id}/duplicate` when the copy keeps the organization | organization ACTIVE, `features.meetings`, `meetings.organization.create` (`meetings.hostCreation` tier) |
| `POST /api/v2/conferences/{id}/waiting-room/admit` in an organization's meeting | organization ACTIVE, `features.meetings`. Evaluated for the acting host. A host who is not a member of the organization is governed by meeting roles alone. |
| Join admission that relies on organization membership (`ORGANIZATION_ONLY`, internal waiting-room bypass) | organization ACTIVE, `features.meetings` for the joining member. Otherwise the membership grants nothing; invitation, passcode and public grants still apply. |

**Failure codes (stable).**
* `403 ORGANIZATION_POLICY_RESTRICTED` (new): the feature is off, or the policy removes a capability the role holds. The read reports this as `POLICY_RESTRICTED`.
* `403 PERMISSION_DENIED`: the role does not hold the capability.
* `409 ORGANIZATION_SUSPENDED`: the organization is not active.
* `503 SERVICE_UNAVAILABLE`: Organizations is unreachable, or its answer is malformed or incomplete during the decision. Never an allow.

During a complete Organizations outage, the request's directory already counts no membership. Creation therefore keeps its existing `403 PERMISSION_DENIED`, which the Section 4 test asserts, and membership-based admission grants nothing. `admit` reads the membership from Organizations itself, so an outage is `503`.

**Legacy and shadow mode:** no change, and no policy read is made.

**Tests:** see §5.

## 2. Per-organization Enterprise Calling

```text
human ─▶ Meeting  /api/v2/organizations/{id}/calling/{path}   (5 ops, gateway EXCLUDED FIRST_PARTY_ORGANIZATIONS)
         1 account authority: verified human, revocation checked
         2 Organizations: calling.enterprise.manage + features.calling  (same evaluator as §1)
         3 Organizations: POST scopes/resolve → verified tenant/application/environment
                          + serviceBindings.calling {serviceAccountId, credentialId}
         4 Platform Access: principals/ensure → member's NATIVE_CF_USER principal in THAT environment
                          (cached in communication_principal_links, keyed by environment)
         5 Calling customer route, no customer credential: Meeting token with
           calling-request.act-for-organization; delegated context = org triple + binding service
           account + initiating credential (id) + initiating principal
Calling: issuer == the permission's single grantee AND grant AND flag on
         → Platform Access credentials/delegated-authority (live, unchanged path)
         → tenant FIRST_PARTY with reserved slug org-<32 hex> (defence in depth)
         → principal resolves in the same triple, ACTIVE, NATIVE_CF_USER
         → PlatformAuthContext.acting_principal_id → enterprise_admin_audit.actor_principal_id
```

* **Scope.** Calling runs in the organization's provisioned scope: the Section 7 tenant, application and environment. Limits and rate windows are per organization credential (CAL-009).
* **Enterprise admin rights.** These come from the Organizations capability `calling.enterprise.manage`, which OWNER and ADMIN hold. A policy with `features.calling=false` removes it for every role. The environment allowlist plays no part.
* **Human actor.** Calling's `GET /v1/enterprise/audit` now carries `actorPrincipalId`. It names the member's principal, and Platform Access maps it to `cfmeet-user:<account id>`. Its value is `null` for plain platform credentials.
* **Allowlist.** The relay serves only the organization's Enterprise administration surface: `v1/enterprise/**`, `v1/webhooks*`, `v1/webhook-deliveries/{id}/replay` and `v1/calling/usage|limits`. These are exactly the routes the web adapter gates with `CALLING_ADMIN_PRINCIPAL_IDS`. Personal calls, endpoints and blocks keep the governed personal scope (ADR 0056 §4). Privacy Relay administration is not an organization capability and is not relayed.
* **Answers.**
  * Calling's own 2xx and 4xx answers are relayed unchanged.
  * Calling `401` means the forward was not accepted: the flag is off, the trust bundle is missing, or the credential was revoked. Calling `401`, Calling 5xx and an unreachable peer all become `503 SERVICE_UNAVAILABLE`.
  * A missing binding, or Platform Access refusing the principal, is `409 ORGANIZATION_CALLING_DISABLED`.
  * A scope that is not yet READY is relayed as `409 PROVISIONING_PENDING`.
* **PRIVATE_E2EE and Messaging** are untouched.

## 3. Flags and configuration (defaults keep current behaviour)

| Where | Flag | Default | Effect |
|---|---|---|---|
| meeting-service | `ORGANIZATION_CALLING_SCOPE` | `shared` | `shared`: nothing is constructed, and the relay answers `409 ORGANIZATION_CALLING_DISABLED`. The shared-credential web path stays the one in use. `organization`: the relay is live. It requires `ORGANIZATIONS_AUTHORITY_MODE=authoritative`, `CF_CALLING_BASE_URL`, `CF_PLATFORM_ACCESS_BASE_URL`, `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID` and `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`; `CF_CALLING_TIMEOUT_MS` is optional (1000–30000, default 10000). Anything missing stops startup. |
| calling | `CALLING_ORGANIZATION_FORWARDING_ENABLED` | `false` | Accepts organization forwards from Meeting. When off, a Meeting token gets the same `401` as any non-gateway issuer. |
| trust bundles | Calling receiver: `cfmeet.meeting-service` → `calling-request.act-for-organization` | not granted | Release step. Certification bundles are built from the registry. |
| meeting-service, Section 4 | `ORGANIZATIONS_AUTHORITY_MODE` | `legacy` | §1 enforcement runs only in `authoritative` mode. |

## 4. Data and contracts

* **Migrations.** All are additive and appended last.
  * Calling `m20261201_000048_enterprise_audit_acting_principal` adds a nullable `actor_principal_id` column. Down drops it.
  * Organizations `m20261108_000003_scope_service_bindings` adds a new table, `organization_scope_service_bindings`, keyed by (organization, capability, service = `calling`). It stores ids only, with `bound_by` and `version`. Down drops it.
  * meeting-service has no migration. It reuses `communication_principal_links`, which is keyed by environment.
* **Contracts.**

  | Contract | Old → new | Change |
  |---|---|---|
  | `cfmeet.calling.http-api` / `-bundled` / `cfmeet.calling.public-api` | 1.15.0 → 1.16.0 | `actorPrincipalId` |
  | `cfmeet.calling.enterprise` | 1.0.0 → 1.1.0 | |
  | `cfmeet.platform.common` | 1.14.0 → 1.15.0 | permission `calling-request.act-for-organization`: receiver Calling, grantable to `cfmeet.meeting-service` only, sensitive |
  | `cfmeet.organizations.internal-api` / `-bundled` | 1.1.0 → 1.2.0 | `GET`/`PUT`/`DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling` (`organization.administer`); `serviceBindings` on `scopes/resolve` |
  | meeting-service OpenAPI (1.2.0, unversioned per section) | — | +5 operations, v2 operations 183 → 188; 2 new error codes |

* **Relay allowlist.** Meeting's Organizations allowlist gains `POST /internal/v1/organizations/scopes/resolve`.

## 5. Tests

* **Unit (run locally).**
  * meeting-service: `workspaces::write_paths_enforce_exactly_the_capabilities_read`, plus `organization_calling::*` for the allowlist, query bounds, scope parsing and the mode default.
  * Calling: middleware decision tests and the flag parse.
  * service-auth: the permission is grantable to Meeting only.
* **DB integration (CI, PostgreSQL).** meeting-service `tests/organizations_authority.rs`:
  * `s12_organization_meeting_creation_follows_the_capabilities_read`: create and duplicate are allowed by default, refused by policy, refused by feature, refused for a GUEST and refused when suspended, and the OWNER is still allowed when only MEMBER is restricted.
  * `s12_organization_meeting_creation_fails_closed_when_the_policy_read_fails`: answers 503 and nothing is created.
  * `s12_admission_and_admit_follow_the_organization_meetings_feature`: covers allow, deny and 503 for join and admit.
  * `s12_legacy_mode_meeting_writes_are_unchanged`.
  * `s12_organization_calling_uses_the_organization_scope_and_the_acting_member`: all five methods succeed, with verified tokens and the org triple, credential and principal; no `Authorization` header; the principal is ensured once.
  * `s12_organization_calling_admin_rights_come_from_organizations`: MEMBER denied, feature off denied, non-allowlisted paths, non-member or malformed ids, unbound, pending and bad bodies, and nothing forwarded.
  * `s12_organization_calling_fails_closed`.
  * `s12_organization_calling_is_disabled_by_default`.

  Calling `tests/organization_forwarding.rs`: accepted plus the audit principal, flag off, missing principal or credential, non-org or non-FIRST_PARTY tenant, foreign principal, and other issuers. Organizations `tests/provisioning.rs::calling_binding_is_operator_only_versioned_audited_and_carried_by_resolve`.

## 6. Release (owner / operator, not done here)

Order matters; each step is reversible on its own.

1. **Deploy the images** with the defaults. Run Calling migration 000048 and Organizations migration 000003. Behaviour does not change.
2. **Section 4 / 7 gates**, owner decisions already tracked:
   * Organizations cutover to `authoritative`, with backups and the restore drill;
   * `ORGANIZATIONS_PROVISIONING_ENABLED=true`, with the Organizations key in the Platform Access trust bundle;
   * organizations reach `provisioningState=READY`.
3. **Per organization (operator, Platform Access operator console / `platform_admin`).** This is an **owner decision** under ADR 0058 §1: scope grants are explicit operator decisions.
   * In the organization's FIRST_PARTY triple, create one service account (e.g. `calling-enterprise`) and one credential.
   * Grant the Calling enterprise scopes the organization buys, and set explicit limits (CAL-009).
   * The credential secret is never needed by CF Meet: Meeting forwards the credential id only. Discard it, or keep it sealed for break-glass use.
4. **Bind (operator).** Call `PUT /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling` with `{serviceAccountId, credentialId}`. A wrong binding fails closed at use: Platform Access refuses a credential outside the triple.
5. **Trust bundles.** Grant `cfmeet.meeting-service` the permission `calling-request.act-for-organization` in Calling's receiver bundle. Make sure Meeting holds `platform-principal.ensure` (it already does for Messaging) and `organization.read` (already granted).
6. **Enable on staging first.**
   * Calling: `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`.
   * Meeting: `ORGANIZATION_CALLING_SCOPE=organization`, plus `CF_CALLING_BASE_URL` and `CF_PLATFORM_ACCESS_BASE_URL`.
   * Certify with one test organization. Do not use copied production meetings or real numbers.
7. **Frontend cutover (frontend team, `apps/`).** For organization workspaces, call `/api/v2/organizations/{id}/calling/{path}` instead of the BFF's shared-credential `/v1/enterprise/*` proxy. Gate the UI on `calling.enterprise.manage` from the capabilities read. Then remove `CALLING_ADMIN_PRINCIPAL_IDS` from the enterprise surface. The shared credential remains only for the personal scope and the legacy path until it is retired.

**Rollback.**
* Set `ORGANIZATION_CALLING_SCOPE=shared`; the relay goes off immediately.
* Set `CALLING_ORGANIZATION_FORWARDING_ENABLED=false`; Calling refuses forwards.
* Clearing a binding (`DELETE …/service-bindings/calling`) disables one organization.
* Suspending the organization suspends its tenant (Section 7), so every forward fails authentication.
* Schema rollback (only if required): `organizations-migration down 1` and Calling `down 1`. Both are additive-only drops.

## 7. Still open

* **Member calls in the organization scope:**
  * extension dialing;
  * endpoint registration in the organization triple;
  * an ownership guard equivalent to the BFF's `ownCall` / `ownEndpoints`.

  Only the administration surface is relayed. Personal calls stay in the personal scope by design. Organization member calling needs a product decision and a separate slice.
* **Per-organization Platform Access credential and Calling scope grants:** an operator / owner decision for each organization (step 3). Until it is made, the relay answers `409 ORGANIZATION_CALLING_DISABLED` for that organization.
* **Frontend adoption** (step 7), owned by the frontend team. The shared credential and env allowlist stay live until then (CAL-007 PARTIAL).
* **Admit by a non-member co-host** of an organization's meeting is governed by meeting roles only. Organizations answers policy reads only for members.
* **Slug check:** the `org-` slug check in Calling is defence in depth. Platform Access does not reserve the prefix against operator-created tenants. The authority is the Platform Access credential re-verification plus the Organizations mapping.
* **`actor_principal_id` lifecycle:** it has no foreign key, and its behaviour under principal tombstoning and retention is not addressed. It follows the audit table's retention.
* **Messaging (Section 13):** per-organization scope for Messaging is not part of this slice.
