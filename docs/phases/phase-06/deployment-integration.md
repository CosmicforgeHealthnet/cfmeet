# Part VI deployment integration requirements (recorded, NOT performed)

Nothing in Part VI §§1–5 deploys. `frontend.yml`, Vercel, `deploy-backend.yml`, the
production VPS, production secrets and live Nginx routing are unchanged. Before the
gateway or the Developer Platform reach staging, the following must exist:

## Developer Platform
1. Database provisioned with `infra/deployment/postgres/provision-service-database.sh --service developer-platform`
   (owner `developer_platform_migrator`, runtime `developer_platform`; CONNECT revoked from
   PUBLIC). `verify-service-isolation.sh` already covers it.
2. Migrations applied by `developer-platform-migration up` as the migrator role before the
   API / worker start (both refuse pending migrations).
3. Secrets (files, never inline): Ed25519 signing key (`cf-service-keygen generate --kid developer-platform-<env>-<n>`),
   its trust bundle granting `developer-request.forward` to `cfmeet.api-gateway` only.
4. Platform Access trust bundle: add issuer `cfmeet.developer-platform` with
   `platform-identity.read` only.
5. Network: internal only (no public route, no Nginx location). HTTPS between services
   outside local / test (enforced by `packages/service-auth`).
6. Processes: `developer-platform` and `developer-platform-worker` (separately scalable).

## API Gateway
1. No database. Secrets: Ed25519 signing key file only (no trust bundle).
2. Platform Access trust bundle: add issuer `cfmeet.api-gateway` with
   `platform-credential.authenticate` only.
3. Upstreams: `CF_DEVELOPER_PLATFORM_BASE_URL` (HTTPS). Each further domain service joins
   only with its own receiver-side permission for `cfmeet.api-gateway` (Section 51).
4. Redis for shared quotas: `rediss://` (TLS) URL when quotas are implemented.
5. Edge: a NEW public host / location in front of the gateway (TLS termination, request
   size limit ≥ `API_GATEWAY_MAX_BODY_BYTES`, `X-Forwarded-*` are ignored by the gateway).
   Existing Nginx locations of the domain services stay as they are until Section 51
   migrates them deliberately; `/internal`, provider callbacks and operator paths must
   never be proxied to the gateway's public host.
6. Flags stay `false` until the Section 50/51 acceptance; enabling is a configuration
   change, not a code change.

## CI / release
* `developer-platform.yml`, `api-gateway.yml` validate on push / PR (no deploy job).
* A future deploy workflow must be manual, staging first, mirroring `deploy-backend.yml`'s
  manual-only policy.
