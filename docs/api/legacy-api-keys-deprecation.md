# Legacy meeting-service API keys — deprecation record and migration guide

**Findings:** PLT-004 (canonical), FE-013 (merged duplicate) — Blueprint v1.0 Section 15.
**Disposition:** `DEPRECATED_WITH_MIGRATION`. Nothing is removed in Section 15. Removal of
the legacy code path is Section 16 work (master register decision D-9: "only after measured
zero consumers, per-endpoint record").
**Status of this record:** recorded 2026-11-07 against `staging` `90c2397e`.

## 1. Decision

CF Meet has two unrelated API-credential systems. Platform Access is the **only** credential
authority; the legacy meeting-service API keys are deprecated.

| | Legacy meeting-service API keys | Platform Access credentials |
|---|---|---|
| Authority | `services/meeting-service` (`api_keys` table, `models/api_keys.rs`) | `services/platform-access` (ADR 0032; ADR 0055 §Decision: "There is no second credential authority") |
| Issued by | `POST /api/v1/api-keys` (user session) | Developer Platform `POST /v1/environments/{environmentId}/service-accounts/{serviceAccountId}/credentials` (`createCredential`), delegated to Platform Access |
| Format | 64 alphanumeric characters, **no prefix** (`handlers/api_keys.rs` `generate_api_key`) | `cfmk_<sbx\|test\|live>_<24 hex public id>_<64 hex secret>` (`identity/credential_format.rs`) |
| Storage | the key itself, plaintext, matched by equality (`middleware/api_key.rs`; PLT-002 / SEC-006 / DATA-003) | HMAC verifier only; raw secret returned once |
| Header | `Api-Key: <key>` | `Authorization: Bearer cfmk_…` at the API Gateway |
| Scope model | none: the key acts as its owner on the two `/api/v1/api/*` routes | per-credential scopes, validated against the application's allowed scopes (`identity/scopes.rs`) |
| Lifecycle | fixed 20-day expiry, fixed 100-request quota (non-atomic counter), revoke | expiry within organization policy, rotation with bounded overlap, revoke, suspend / resume, per-credential and per-service-account limits |
| Ingress | direct meeting-service routes on `api.cfmeet.com`; the gateway catalogue marks all six legacy operations `EXCLUDED` / `LEGACY_DIRECT_INGRESS` (`infra/api-gateway/route-manifest.json`) | API Gateway only (`PLATFORM_CREDENTIAL` auth mode; credential routes accept only `cfmk_` values) |

Why: ADR 0032 makes Platform Access the shared identity authority (tenants, applications,
environments, service accounts, credentials, scopes, principals). ADR 0055 forbids any second
credential authority (guard rule `IDENTITY_HIERARCHY_DUPLICATED`). The legacy keys predate
both, have no scopes, and store secrets in plaintext.

## 2. Operation mapping

The six legacy operations and their replacements (all replacements go through the API
Gateway; paths are relative to the gateway origin):

| Legacy operation | Replacement | Auth / scope |
|---|---|---|
| `GET /api/v1/api-keys` (`list_api_keys`) | `GET /v1/environments/{environmentId}/service-accounts/{serviceAccountId}/credentials` (`listCredentials`) | developer session or credential with `credentials.manage` |
| `POST /api/v1/api-keys` (`create_api_key`) | `POST …/service-accounts/{serviceAccountId}/credentials` (`createCredential`) — response `IssuedCredential` carries the secret once | as above |
| `GET /api/v1/api-keys/{id}` (`get_api_key`) | `GET /v1/environments/{environmentId}/credentials/{credentialId}` (`getCredential`) | as above |
| `DELETE /api/v1/api-keys/{id}` (`revoke_api_key`) | `POST /v1/environments/{environmentId}/credentials/{credentialId}/revoke` (`revokeCredential`); also `rotate`, `suspend`, `resume` | as above |
| `POST /api/v1/api/meetings` (`api_create_meeting`) | `POST /api/v2/meeting-spaces` (`v2_create_meeting_space`) then `POST /api/v2/meeting-spaces/{id}/conferences` (`v2_create_conference`); or `POST /api/v2/instant-meetings` (`v2_create_instant_meeting`) | `meetings.write` |
| `POST /api/v1/api/meetings/{id}/join` (`api_join_meeting`) | No machine "join as the key owner". Issue an invitation with `POST /api/v2/meeting-spaces/{id}/invitations` (`v2_create_invitation`, `meetings.write`); the participant resolves it (`POST /api/v2/invitations/{token}/resolve`, anonymous) and joins with the participant token (`POST /api/v2/conferences/{id}/join`) | `meetings.write` (issuer); participant token (joiner) |

Further machine operations available to `cfmk_` credentials on the meetings family
(`meetings.read` / `meetings.write`): conference start / end / cancel, participants,
attendance, waiting room admit / deny, invitation list / revoke — see the `meetings` routes
with `PLATFORM_CREDENTIAL` in `infra/api-gateway/route-manifest.json`.

### Scope mapping

A legacy key implicitly had "create and join meetings as me". The least-privilege
equivalent is a credential on a service account with:

| Need | Scope |
|---|---|
| create spaces / conferences / invitations, control conferences | `meetings.write` |
| read spaces, conferences, participants, attendance | `meetings.read` |
| manage credentials programmatically (optional; usually done from the console) | `credentials.manage` |

The project (application) must first be granted the scope (`POST /v1/projects/{projectId}/scopes`).

## 3. Migration steps for integrators

1. Create (or be invited to) a Developer Platform organization: `POST /v1/organizations`.
2. Create a project (`POST /v1/organizations/{id}/projects`) and use its SANDBOX environment
   (`GET /v1/projects/{id}/environments`).
3. Grant the project `meetings.write` (and `meetings.read` if needed).
4. Create a service account (`POST /v1/environments/{env}/service-accounts`).
5. Issue a credential with the needed scopes (`createCredential`); store the one-time
   secret in a secret manager.
6. Change calls: host → the API Gateway; header `Api-Key: …` →
   `Authorization: Bearer cfmk_…`; `/api/v1/api/meetings` → the v2 operations in §2.
7. Verify in SANDBOX, then request production access
   (`POST /v1/projects/{id}/production-access-requests`) and issue a `cfmk_live_…` credential.
8. Revoke the legacy key (`DELETE /api/v1/api-keys/{id}`).

The SDKs (`@cfmeet/platform-sdk`, `cfmeet_platform_sdk`) already target the gateway and
`cfmk_` credentials; they never supported legacy keys.

## 4. Sunset preconditions (all required before Section 16 removes anything)

| # | Precondition | Owner | Status |
|---|---|---|---|
| 1 | API Gateway and Developer Platform deployed and reachable on the decided public host (PLT-005; checklist `docs/operations/developer-platform-gateway-activation-checklist.md`) | user / release | OPEN |
| 2 | A developer console (or documented operator-assisted onboarding) exists so integrators can obtain `cfmk_` credentials without operator tooling (ORG-017) | frontend | OPEN |
| 3 | The web Developers page and API-keys settings stop advertising legacy keys (§6) | frontend | OPEN |
| 4 | Measured zero use of `/api/v1/api/*` and `/api/v1/api-keys*` for an agreed window (D-9 per-endpoint record), with known integrators notified | user / release | OPEN |
| 5 | Optional freeze before removal: refuse new legacy key creation, keep validation of existing keys until expiry (legacy keys expire after 20 days, so a freeze drains naturally) | backend (Section 16) | NOT STARTED |

## 5. What Section 16 will remove (not done now)

* meeting-service routes `/api/v1/api-keys*` (`routes/api_keys.rs`, `handlers/api_keys.rs`,
  `dto/api_keys.rs`) and `/api/v1/api/*` (`routes/api.rs`, `api_create_meeting`,
  `api_join_meeting`);
* `middleware/api_key.rs` (`api_key_middleware`, `combined_auth_middleware`);
* the `api_keys` table and `api_key_status` enum, by an additive-then-drop migration pair
  after an export (PLT-002 / DATA-003: plaintext secrets at rest — Section 16 owns the
  hashing-or-freeze step);
* the "API Keys" tag and `api_key_auth` security scheme from the meeting-service Swagger;
* the six `LEGACY_DIRECT_INGRESS` catalogue rows in `infra/api-gateway/route-manifest.json`.

## 6. Frontend corrections (owned by the frontend team)

| File | Problem | Correction |
|---|---|---|
| `apps/web/src/components/dashboard/DevelopersContent.tsx` (lines ~276–315) | documents `https://api.cosmicforge.io/api/v1/api/meetings` with `Api-Key: cf_live_…`; the host is wrong (production is `api.cfmeet.com`), the key format is wrong (legacy keys have no prefix), and the API is deprecated | short term: hide the page or replace it with a pointer to the gateway docs (`GET /docs`, `GET /discovery`); medium term: the developer console (ORG-017, `docs/phases/phase-06/developer-console-api-handoff.md`) |
| `apps/web/src/services/api-keys.service.ts`, `apps/web/src/hooks/useApiKeys.ts` | API-key settings use legacy `/api/v1/api-keys` | mark deprecated in the UI; replace with the console credential screens (`listCredentials`, `createCredential`, …) once PLT-005 is done |

The browser must never hold a `cfmk_` credential: the console uses the user session
(`DEVELOPER_SESSION`) and shows a newly issued secret once.
