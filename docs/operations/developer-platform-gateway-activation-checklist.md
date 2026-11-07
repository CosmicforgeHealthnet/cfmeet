# API Gateway and Developer Platform — activation-readiness checklist (PLT-005)

**Finding:** PLT-005 (P1, INFRASTRUCTURE): the API Gateway and the Developer Platform are
not deployed, yet the SDKs and the public OpenAPI advertise `https://api.cfmeet.com`.
**Disposition:** release step owned by the user. **Nothing here has been deployed or
changed in any environment.** This checklist adds to the existing material and does not
replace it:

* deployment units, rollout-flag mapping, 12-step release sequence, per-flag rollback:
  [`infra/deployment/part-vi/README.md`](../../infra/deployment/part-vi/README.md);
* manifest: [`infra/deployment/part-vi/compose.part-vi.yml`](../../infra/deployment/part-vi/compose.part-vi.yml)
  (every control off) and the read-only gate [`preflight.sh`](../../infra/deployment/part-vi/preflight.sh);
* original requirements: [`docs/phases/phase-06/deployment-integration.md`](../phases/phase-06/deployment-integration.md),
  [`chapter-21-deployment.md`](../phases/phase-06/chapter-21-deployment.md).

Owner: **user** = repository owner decision or authorized release action; **release** = the
release workflow / operator executing an approved change; **backend** = code change in this
repository. Status: `READY` (exists in the repo, verified by CI), `OPEN` (to do),
`DECISION` (needs an owner decision), `BLOCKER` (activation fails without it).

## 1. Decisions

| # | Item | Owner | Status |
|---|---|---|---|
| D1 | **Public hostname.** `api.cfmeet.com` today fronts meeting-service / calling through host nginx and the `cfmeet-core-tls` proxy (`docs/operations/tls-certificate-automation.md`; `infra/calling/production/production.py` reads `https://api.cfmeet.com/api-docs/calling.json`). Choose either (a) a new host for the gateway, or (b) a path-level cut-over on `api.cfmeet.com`. With (b) the legacy direct paths (`/api/v1/**`, `/swagger-ui/`, `/api-docs/**`, calling/meeting locations) keep their current upstreams, and `/internal`, provider callbacks and operator paths must never reach the gateway (deployment-integration.md §API Gateway 5) | user | DECISION |
| D2 | Base URL in generated artefacts must equal D1: `GATEWAY_URL` in `scripts/contracts/public_api.py` (line 52, `servers` of every public OpenAPI) and the quick-start `baseUrl` in `sdks/generate.py` (TypeScript and Dart quick-starts). Today both are `https://api.cfmeet.com`. If D1 = new host: change both, run `python scripts/contracts/public_api.py`, `python sdks/generate.py`, bump the affected contract versions in `packages/contracts/manifest.json` | backend (after D1) | OPEN |
| D3 | Console origin for CORS (`API_GATEWAY_CORS_ALLOWED_ORIGINS`) — the web app origin(s) that will call the gateway with a developer session | user | DECISION |

## 2. Build and release plumbing

| # | Item | Owner | Status |
|---|---|---|---|
| B1 | Image build matrix: `.github/workflows/cfmeet-core-images.yml` builds `api-gateway` but **not** `developer-platform` (matrix line 32). Add `developer-platform` (`services/developer-platform/Dockerfile`, which also ships `developer-platform-worker` and `developer-platform-migration`) | backend | OPEN |
| B2 | Deploy job: `developer-platform.yml` and `api-gateway.yml` validate only (no deploy job). A manual, staging-first deploy workflow mirroring `deploy-backend.yml` (no direct VPS access) | backend + user approval | OPEN |
| B3 | `preflight.sh --expect dark --migrator-env …` passes against the target environment's env files | release | OPEN |

## 3. Environment variables

Secrets are files (`*_FILE`), never inline values. Defaults are from `services/*/src/config`.

### Developer Platform (API and worker, same image)

| Variable | Value / rule | Status |
|---|---|---|
| `CF_RUNTIME_ENVIRONMENT` | `staging` / `production` (forces HTTPS for every internal URL) | OPEN |
| `CF_SERVICE_ID` | `cfmeet.developer-platform` | READY (compose) |
| `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | Ed25519 key from `cf-service-keygen generate --kid developer-platform-<env>-<n>` | OPEN |
| `CF_SERVICE_TRUST_BUNDLE_FILE` | grants `developer-request.forward`, `developer-entitlement.read`, `developer-usage.record` to `cfmeet.api-gateway`; `developer-event.publish` to each producer that publishes (meeting-service, calling, privacy-relay, messaging, numbering, sms-gateway); `developer-platform.administer` to `cfmeet.operator-console` | OPEN |
| `DATABASE_URL` | runtime role `developer_platform` (no DDL); `DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS` (default 10) | OPEN |
| `DEVELOPER_PLATFORM_BIND_ADDRESS` | internal only (compose: `0.0.0.0:8098` on the internal network) | READY |
| `CF_PLATFORM_ACCESS_BASE_URL` | HTTPS | OPEN |
| `CF_MEETING_SERVICE_BASE_URL` | HTTPS; needed for console sessions (`user-session.verify`) | OPEN |
| `CF_CALLING_BASE_URL` | HTTPS; webhook migration only (`webhook-migration.export/import`) | OPEN |
| `CF_INTERNAL_HTTP_TIMEOUT_MS`, `CF_SERVICE_TOKEN_TTL_SECONDS` | defaults | READY |
| `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` | `false` until release step 6 | READY (off) |
| `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` | `false` | READY (off) |
| `DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL` | leave unset / off in staging and production | READY (off) |
| `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED`, `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | `false`, `shadow` | READY (off) |
| `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE` / `_ID` (+ `_PREVIOUS_*` during rotation) | 32 random bytes, base64; without it subscription routes answer 503 | OPEN |
| `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY_FILE` / `_ID` | only for the Calling webhook migration (runbook) | OPEN (when migrating) |
| `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN`, `…_REQUEST_TIMEOUT_MS`, `…_MAX_ATTEMPTS`, `…_AUTO_DISABLE_AFTER`, `…_ALLOW_PRIVATE_ENDPOINTS` (local only) | defaults | READY |
| `DEVELOPER_PLATFORM_WORKER_INTERVAL_MS`, `DEVELOPER_PLATFORM_WORKER_INSTANCE_ID` | per worker instance | OPEN |

### API Gateway (no database: `DATABASE_URL` is refused)

| Variable | Value / rule | Status |
|---|---|---|
| `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_ID=cfmeet.api-gateway` | as above | OPEN |
| `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | Ed25519 key; the gateway signs only (no trust bundle: it receives no internal call) | OPEN |
| `CF_PLATFORM_ACCESS_BASE_URL` | required, HTTPS | OPEN |
| `CF_DEVELOPER_PLATFORM_BASE_URL`, `CF_MEETING_SERVICE_BASE_URL`, `CF_CALLING_BASE_URL`, `CF_MESSAGING_BASE_URL`, `CF_NUMBERING_BASE_URL`, `CF_SMS_GATEWAY_BASE_URL` | HTTPS; an unset upstream makes its routes `SERVICE_UNAVAILABLE` | **BLOCKER** — see §7 R1 |
| `API_GATEWAY_REDIS_URL` | `rediss://` (TLS) — `redis://` is refused outside local / test; `API_GATEWAY_REDIS_KEY_PREFIX`, `API_GATEWAY_REDIS_TIMEOUT_MS` (20–5000, compose 250), `API_GATEWAY_QUOTA_FLUSH_INTERVAL_MS` | OPEN |
| `API_GATEWAY_BIND_ADDRESS`, `API_GATEWAY_UPSTREAM_TIMEOUT_MS`, `API_GATEWAY_MAX_BODY_BYTES`, `API_GATEWAY_MAX_HEADERS`, `API_GATEWAY_MAX_HEADER_BYTES`, `API_GATEWAY_WS_MAX_MESSAGE_BYTES` | defaults; the edge request-size limit must be ≥ `API_GATEWAY_MAX_BODY_BYTES` | READY |
| `API_GATEWAY_CORS_ALLOWED_ORIGINS` | from D3 | DECISION |
| `API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_WEBSOCKETS_ENABLED`, `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`, `API_GATEWAY_CENTRAL_USAGE_ENABLED`, `API_GATEWAY_DISABLED_DOMAINS` | all off / empty until the release sequence turns them on one at a time | READY (off) |

### Other services (only when the matching step is reached)

| Service | Change | Status |
|---|---|---|
| Producers | `*_PUBLIC_EVENTS_DESTINATION` (messaging, numbering, sms-gateway, calling, …) and `CF_DEVELOPER_PLATFORM_BASE_URL` for central webhooks; `CFMEET_USAGE_EXPORT_ENABLED` for central usage | OPEN (release steps 8–10) |
| Calling | `CALLING_WEBHOOK_AUTHORITY`, `CALLING_WEBHOOK_DELIVERY_ENABLED`, `CALLING_WEBHOOK_MUTATIONS_FROZEN`, `CALLING_LEGACY_USAGE_AUTHORITY` stay at their current values until the webhook runbook | READY (unchanged) |

## 4. Trust bundles and service identities

Permissions come from the registry in `packages/service-auth/src/permissions.rs` (asserted
by its tests). Each receiver lists the grant in **its own** trust bundle.

| Receiver (trust bundle) | Issuer to add | Permissions | Status |
|---|---|---|---|
| Platform Access | `cfmeet.api-gateway` | `platform-credential.authenticate` | OPEN |
| Platform Access | `cfmeet.developer-platform` | `platform-identity.read` **and** `platform-identity.delegate` (the delegation API used for organizations, projects, environments, service accounts and credentials). `deployment-integration.md` §Developer Platform 4 lists only `platform-identity.read`; that line is incomplete | OPEN |
| Developer Platform | `cfmeet.api-gateway` | `developer-request.forward`, `developer-entitlement.read`, `developer-usage.record` | OPEN |
| Developer Platform | producers | `developer-event.publish` | OPEN (central webhooks) |
| Developer Platform | `cfmeet.operator-console` | `developer-platform.administer` | OPEN (operator tooling) |
| Meeting Service | `cfmeet.api-gateway` / `cfmeet.developer-platform` | `meeting-request.forward` / `user-session.verify` | OPEN |
| Calling | `cfmeet.api-gateway` / `cfmeet.developer-platform` | `calling-request.forward` / `webhook-migration.export`, `webhook-migration.import` | OPEN |
| Messaging, Numbering, SMS Gateway | `cfmeet.api-gateway` | `messaging-request.forward`, `numbering-request.forward`, `sms-request.forward` | OPEN |

A family joins the gateway only when its receiver grants the forward permission; until then
its routes refuse. That allows family-by-family activation.

## 5. Databases and migrations

| # | Item | Owner | Status |
|---|---|---|---|
| M1 | `infra/deployment/postgres/provision-service-database.sh --service developer-platform` (owner `developer_platform_migrator`, runtime `developer_platform`, CONNECT revoked from PUBLIC); `verify-service-isolation.sh` covers it | release | OPEN |
| M2 | Developer Platform migrations, as the migrator role, before API / worker (`developer-platform-migration up`; both processes refuse pending migrations), in order: `m20261101_000001_developer_platform_foundation`, `m20261102_000002_control_plane`, `m20261103_000003_api_usage`, `m20261104_000004_webhooks`, `m20261105_000005_webhook_migration`, `m20261110_000010_api_operation_usage`, `m20261120_000011_credentials_production_usage`, `m20261201_000012_event_rejections`, then the Section 15 additive migration `m20261202_000013_provisioning_name_sync_operator_audit` (`devplat_organization_provisioning`, `devplat_organizations.platform_name_sync_pending`, `devplat_operator_audit`). Re-confirm against `services/developer-platform/migration/src/lib.rs` at the release SHA. The worker needs `CF_PLATFORM_ACCESS_BASE_URL` (and its Platform Access trust entry) to run the organization reconciliation; without it the worker logs a warning and skips it | release | OPEN |
| M3 | Platform Access migrations (Platform Access is already a running service — apply with its own release): `m20261016_000001_identity_hierarchy`, `m20261016_000002_identity_change_feed`, `m20261016_000003_access_operations`, `m20261120_000004_credential_suspension_production_activation`, `m20261121_000005_first_party_organization_scopes`, then the Section 15 additive migration `m20261122_000006_credential_usage_tenant_rename` (table `platform_credential_usage`, audit action `TENANT_RENAMED`). Re-confirm in `services/platform-access/migration/src/lib.rs`. All are additive; rollback = previous image (the new table is ignored by it; the down migration refuses once a `TENANT_RENAMED` row exists) | release | OPEN |
| M4 | Pre-migration snapshot of the Platform Access database before M3 (SEC-011 policy) | release | OPEN |
| M5 | The gateway has no database | — | READY |

## 6. Edge, DNS and TLS

| # | Item | Owner | Status |
|---|---|---|---|
| E1 | DNS record for the D1 host (new host) or nginx location plan (path cut-over) | user | DECISION |
| E2 | TLS certificate for the D1 host. For a new host, add a lineage and extend the certificate monitor (`infra/calling/production/certificate_monitor.py`, runbook `tls-certificate-automation.md`); for `api.cfmeet.com` the existing lineage applies | release | OPEN |
| E3 | Edge proxy: TLS termination, body limit ≥ `API_GATEWAY_MAX_BODY_BYTES`, WebSocket upgrade for realtime families only when `API_GATEWAY_WEBSOCKETS_ENABLED`; the gateway ignores `X-Forwarded-*` | release | OPEN |
| E4 | Never proxy `/internal/**`, provider callbacks or operator paths to the gateway host; the Developer Platform and Platform Access have no public location | release | OPEN |

## 7. Readiness risks found while writing this checklist

| # | Risk | Owner | Status |
|---|---|---|---|
| R1 | `compose.part-vi.yml` sets `CF_DEVELOPER_PLATFORM_BASE_URL: http://developer-platform:8098` while requiring `CF_RUNTIME_ENVIRONMENT` = staging / production. `packages/service-auth/src/client.rs` (`InternalTargets::insert`) refuses `http` base URLs outside local / test (`RuntimeEnvironment::allows_insecure_transport`), so the gateway fails `--check-config` with that value. The Developer Platform serves plain HTTP on 8098, so internal TLS (a TLS-terminating sidecar or internal proxy with a certificate trusted by the gateway) must be provided and the URL changed to `https://…`. Not changed here (deployment configuration is release-owned) | release (+ backend if a TLS listener is preferred) | **BLOCKER** |
| R2 | `services/api-gateway/.env.example` / `config/mod.rs` said shared quotas were "future … nothing is enforced yet"; corrected in Section 15 (PLT-012), which also adds the optional per-token ceiling `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` (default 0 = off) | backend | DONE (Section 15) |
| R3 | The developer console does not exist (ORG-017); until it does, sandbox credentials are issued by operators (release step 7) | frontend | OPEN |

## 8. Activation, smoke checks and rollback

Follow the 12-step sequence in `infra/deployment/part-vi/README.md`; stop on the first
failed check. Minimum smoke after each step:

| Step | Check |
|---|---|
| DP started (dark) | `/ready` 200 with `migrations: current`; `/internal/v1/gateway/context` refuses an unsigned call |
| Gateway started (dark) | `/ready` 200, `platformAccess: reachable`, `routes: 0`, `quotas: shared` (not `degraded`) |
| Routing on | `/ready` routes > 400; `GET /discovery`, `GET /openapi.json`, `GET /docs` served with `servers[0].url` = D1 host |
| Sandbox | an operator-issued `cfmk_sbx_…` credential: `GET /v1/developer-platform/context` 200; one call per enabled family 200; a credential without the route scope → 403 at the gateway; over-quota → 429 with `Retry-After` |
| Console session | `GET /v1/organizations` with a CF Meet session → 200 |

Rollback is per control (README "Rollout controls" table): turning a flag off or adding a
family to `API_GATEWAY_DISABLED_DOMAINS` is a configuration change; public routing off =
deny-all 404. Existing meeting / calling traffic on `api.cfmeet.com` is unaffected as long
as D1 does not move their locations. Rehearsed by `certification/tests/rollout_rehearsal.rs`.
