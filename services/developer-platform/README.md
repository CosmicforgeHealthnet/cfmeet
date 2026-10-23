# services/developer-platform — Developer Platform (Part VI foundation)

The developer / enterprise control plane of CF Meet ([ADR 0055](../../docs/decisions/0055-developer-platform-and-api-gateway-foundation.md)).
This is the §§1–5 **foundation**; the functional Developer Platform (Sections 50+) is not
implemented.

* **Authority:** Platform Access is the only authority for tenants, applications,
  environments, service accounts, API credential verifiers, scopes and principals. The
  Developer Platform stores their canonical ids only (`devplat_environment_refs`) and asks
  Platform Access (`platform-identity.read`) to re-validate every forwarded context. It
  never verifies a credential and never issues one.
* **Database:** its own PostgreSQL database, `devplat_` tables, history in
  `developer_platform_seaql_migrations`; the server refuses to start with pending migrations.
* **Binaries:** `developer-platform` (API), `developer-platform-worker` (own loop:
  heartbeats; Section 50+ jobs plug in), `developer-platform-migration`.
* **HTTP:** `GET /health`, `GET /ready`, `GET /openapi.json` (contract
  `cfmeet.developer-platform.internal-api`) and `/internal/v1/**` (service tokens only):
  `GET /internal/v1/gateway/context` — `developer-request.forward` (API Gateway only),
  signed delegated context required, behind `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`
  (default off).
* **Developer Console:** `apps/web/src/app/dashboard/developers/` stays the future console
  UI; it will call the published gateway API with the same contracts as every customer
  (see `docs/phases/phase-06/developer-console-boundary.md`).

```bash
make developer-platform-migrate   # services/developer-platform/.env
make developer-platform           # API
make developer-platform-worker    # worker
make developer-platform-test      # TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres
make developer-platform-lint
```
