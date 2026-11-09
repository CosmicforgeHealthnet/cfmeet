# Part VI deployment units and controlled rollout (Chapter 21)

**Status: DEPLOYMENT_READY, not deployed.** Nothing here is wired into a workflow;
`docker-compose.prod.yml`, `deploy-backend.yml`, the VPS, production secrets and live edge
routing are unchanged. Every step below that changes a live environment needs an
authorized change window.

| File | Purpose |
|---|---|
| [`compose.part-vi.yml`](compose.part-vi.yml) | Developer Platform API, worker, one-shot migration job and API Gateway; every rollout control off; secrets mounted as files / secret env files; `/ready` health checks |
| [`preflight.sh`](preflight.sh) | Read-only gate: `--check-config` of both binaries with the real env files, every control off (`--expect dark`), migrations current (`--migrator-env`: the migration CLI needs the migrator role), `/ready` probes |
| [`../postgres/provision-service-database.sh`](../postgres/provision-service-database.sh) | `--service developer-platform`: own database, migrator (owner) and runtime (no DDL) roles |
| [`docs/operations/developer-platform-gateway-activation-checklist.md`](../../../docs/operations/developer-platform-gateway-activation-checklist.md) | PLT-005 activation-readiness checklist: decisions (public host, base URLs), env vars, trust bundles, migrations, DNS/TLS, open blockers |

## Deployment units

| Unit | Image / binary | Requirements |
|---|---|---|
| Developer Platform API | `services/developer-platform/Dockerfile` → `developer-platform` | own PostgreSQL (`developer_platform` runtime role, no DDL: the migration state is read with a plain `SELECT`), Ed25519 key + trust bundle (files), Platform Access URL; `/ready` = 503 while the DB is unreachable or migrations pend; drains on SIGTERM |
| Developer Platform worker | same image → `developer-platform-worker` | same configuration; scale freely (claims use `FOR UPDATE SKIP LOCKED` + leases: several workers never deliver one delivery twice) |
| Migrations | same image → `developer-platform-migration up` | migrator role; before API / worker (they refuse pending migrations); re-runnable |
| API Gateway | `services/api-gateway/Dockerfile` → `api-gateway` | no database; Ed25519 key file; `rediss://` Redis (refused otherwise outside local / test), `API_GATEWAY_REDIS_TIMEOUT_MS`; `/ready` = 503 while Platform Access is unreachable |
| SDK artifacts | `.github/workflows/sdk-release.yml` (manual) | publication only with `SDK_PUBLICATION_ENABLED=true`, explicit input, environment approval, publishable manifests |

Domain services keep their own units; a family joins the gateway only through its
receiver-side `cfmeet.api-gateway` permissions (trust bundles).

## Rollout controls (spec §21.2 → configuration)

| Spec control | Configuration (default off) | Rollback |
|---|---|---|
| `DEVELOPER_PLATFORM_ENABLED` | Developer Platform `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`; gateway family `developer-platform` (`API_GATEWAY_DISABLED_DOMAINS`) | set false / disable the family |
| `PUBLIC_API_GATEWAY_ENABLED` | gateway `API_GATEWAY_PUBLIC_ROUTING_ENABLED` (+ `API_GATEWAY_WEBSOCKETS_ENABLED`); per family `API_GATEWAY_DISABLED_DOMAINS` | false = deny-all 404; or one family |
| `CENTRAL_WEBHOOKS_ENABLED` | Developer Platform `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` + `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` (`shadow` → `active`), gateway `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`, producers `*_PUBLIC_EVENTS_DESTINATION` | [`docs/phases/phase-06/webhook-migration-runbook.md`](../../../docs/phases/phase-06/webhook-migration-runbook.md) §5 |
| `CENTRAL_USAGE_ENABLED` | gateway `API_GATEWAY_CENTRAL_USAGE_ENABLED`; producers `CFMEET_USAGE_EXPORT_ENABLED` | gateway flag false (ledger facts are kept) |
| `PRODUCTION_ACCESS_WORKFLOW_ENABLED` | Developer Platform `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` | false: submit / cancel / activate 503, reads and operator decisions stay |
| `SDK_PUBLICATION_ENABLED` | repository / environment variable read by `sdk-release.yml` | unset |
| Legacy Calling webhooks | `CALLING_WEBHOOK_AUTHORITY` (`active` / `retired`), `CALLING_WEBHOOK_DELIVERY_ENABLED`, `CALLING_WEBHOOK_MUTATIONS_FROZEN` | `active` |
| Legacy Calling usage | `CALLING_LEGACY_USAGE_AUTHORITY` (`active` / `retired`) | `active` |

## Release sequence (each step: change, verify, stop on failure)

| # | Change | Verify |
|---|---|---|
| 1 | Provision the database and roles; mount secrets; build images | `preflight.sh --expect dark --migrator-env …` (config, keys, catalogue) |
| 2 | Run `developer-platform-migrate` | `developer-platform-migration status`: nothing pending |
| 3 | Start the Developer Platform API and worker (controls off) | `/ready` 200, `migrations: current` |
| 4 | Platform Access trust bundles: add issuers `cfmeet.developer-platform` (`platform-identity.read`) and `cfmeet.api-gateway` (`platform-credential.authenticate`) | Developer Platform `/internal/v1/gateway/context` refuses unsigned calls; gateway `/ready` `platformAccess: reachable` |
| 5 | Start the gateway (deny-all) | `/ready` 200, `routes: 0`, `quotas: shared` |
| 6 | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true`, then `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true` (+ WebSockets) | `/ready` routes > 400; public OpenAPI / `/discovery` served |
| 7 | Sandbox smoke (an operator-provisioned sandbox credential) | six families 200; webhook test endpoint; usage visible |
| 8 | Producers `CFMEET_USAGE_EXPORT_ENABLED=true` (shadow) | ledger reconciliations `MATCH`; ingestion health `OK` |
| 9 | Webhooks shadow (`DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=true`, mode `shadow`), migration import | runbook §4 steps 1–3 (`parity: true`, 0 DP sends) |
| 10 | `API_GATEWAY_CENTRAL_USAGE_ENABLED=true`; webhook takeover per runbook §4 steps 4–10; `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=true` | `/v1/usage` / `/v1/limits` contract responses; `/v1/webhooks` served by the DP |
| 11 | Monitoring and acceptance | readiness, ingestion health, delivery backlog, quota mode, error rates |
| 12 | Rollback (any time, independently) | table above; rehearsed by `certification/tests/rollout_rehearsal.rs` |

## Rehearsal evidence

`certification/tests/rollout_rehearsal.rs` runs this sequence from a fully dark stack on
the real processes and rolls back central usage, central webhooks, one family, the
production-access workflow, the Developer Platform API and public routing one at a time,
checking the others and every service stay intact. `certification/tests/usage_migration.rs`
and `webhook_migration.rs` rehearse the two data migrations including rollback.
