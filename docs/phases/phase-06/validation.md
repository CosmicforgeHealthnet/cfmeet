# Part VI §§1–5 — validation evidence

Local run on the staging working tree (PostgreSQL 16, no Docker for tests), 2026-10-23.

| Check | Command | Result |
|---|---|---|
| Developer Platform fmt + clippy (`-D warnings`) | `make developer-platform-lint` | pass |
| Developer Platform tests | `make developer-platform-test` | 26 passed (unit 6, foundation 11, architecture 4, startup 3, worker 2) |
| API Gateway fmt + clippy | `make api-gateway-lint` | pass |
| API Gateway tests | `cargo test` (routing 6, ingress 11, startup 2, unit 6) | 25 passed |
| Cross-service journey (real Platform Access, real Developer Platform, gateway binary) | `make api-gateway-integration` | 1 passed |
| `packages/service-auth` (core + `client`) incl. Part VI least-privilege registry test and contract conformance | `make service-auth-test` | pass |
| Contract validator / its mutation tests | `make contracts-validate`, `make contracts-test` | 70 contracts OK; 19 passed |
| Swagger bundles current | `python3 scripts/contracts/bundle_openapi.py --check` | 11 documents current |
| SDK generation current | `make sdks-check`; `tsc --noEmit` on `sdks/typescript` | pass |
| Service-boundary guard + mutation tests (incl. `GATEWAY_OWNS_DATA`, `IDENTITY_HIERARCHY_DUPLICATED`, `PRODUCT_ENDPOINT_IN_PLATFORM`) | `make service-boundaries` | pass |
| Operations registry | `check_operations_registry.py` | 60 responsibilities OK |
| Secret scan + tests | `make secret-scan` | clean |
| Per-service database isolation with the real Developer Platform migrations | `infra/deployment/postgres/verify-service-isolation.sh` | pass |
| Platform Access full suite (links the changed registry) | `make platform-access-test` | pass |
| Root compose unchanged and valid | `docker compose config --quiet` | pass |

## What the tests prove

* **Independent compilation / startup:** both services build in their own workspaces;
  real binaries refuse missing / invalid configuration (no secret echoed), the Developer
  Platform refuses pending migrations, the worker binary runs its own loop.
* **Database ownership and isolation:** only `devplat_*` tables; no identity-hierarchy,
  credential, content or E2EE columns; immutable canonical references; migrations up /
  reset / up; runtime role cannot run DDL or connect to another service's database; the
  gateway has no database (config, Cargo manifest and guard).
* **Platform Access integration:** credentials verified only by Platform Access (real
  process); revocation and an authority outage fail closed (401 / 503); throttling → 429;
  the Developer Platform re-validates every forwarded context with Platform Access.
* **Signed context:** upstream receives a gateway-issued token (≤ 60 s) whose delegated
  context equals the Platform Access chain — not the client's headers, query or body.
* **Forgery:** client `X-CF-Service-Authorization` refused (even a genuine gateway token);
  client `X-CF-*` / `X-Tenant-*` / `X-Forwarded-*` never reach the upstream; tokens from
  untrusted keys, tampered tokens and requests around the gateway are rejected upstream.
* **Default deny:** with defaults every customer path is 404 without contacting Platform
  Access; `/internal`, provider callbacks, operator / admin paths, Telephony / Platform
  Access upstreams can never be routed.
* **Correlation:** one correlation id across client response, gateway hop and Developer
  Platform; per-hop request ids.

## Not executed here

* Full test suites of the other domain services (Calling, Numbering, Privacy Relay,
  Telephony, SMS Gateway, Messaging, Meeting Service): their code is unchanged; the shared
  changes are additive (two identities, one permission, two widened grant lists). Their
  CI workflows run on push.
* Docker image builds of the new services (`docker` jobs in the new workflows).
* Dart SDK tests (no Dart toolchain in this environment); CI does not run them yet.
