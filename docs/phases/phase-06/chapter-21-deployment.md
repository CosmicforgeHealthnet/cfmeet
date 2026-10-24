# Part VI Chapter 21 — deployment readiness and controlled rollout

**Result: DEPLOYMENT_READY — not deployed.** Staging only. No production deployment, VPS
change, production secret, live edge routing, telecom activation or customer cutover was
performed. `docker-compose.prod.yml`, `deploy-backend.yml`, `frontend.yml` and Vercel are
unchanged. Every step that changes a live environment is recorded as
**AWAITING_AUTHORIZED_CUTOVER** in [`backend-final-closure.md`](backend-final-closure.md).

Operator material: [`infra/deployment/part-vi/README.md`](../../../infra/deployment/part-vi/README.md)
(deployment units, flag mapping, 12-step release sequence, rollback per control).

## 21.1 Deployment units

| Unit (spec) | Implementation | Evidence | Status |
|---|---|---|---|
| developer-platform API | `services/developer-platform/Dockerfile` → `developer-platform`; own PostgreSQL (`provision-service-database.sh --service developer-platform`: owner + no-DDL runtime role); Ed25519 key / trust bundle files; `/health`, `/ready` (DB + migrations); graceful SIGTERM; `--check-config` | DP `startup.rs` (5 tests incl. runtime role and SIGTERM); preflight against a provisioned local database under the runtime role: all PASS | READY |
| developer-platform worker | same image → `developer-platform-worker`; `FOR UPDATE SKIP LOCKED` + leases; on SIGTERM finishes the send in flight and releases unsent claims | DP `webhooks.rs`, `worker.rs`; load profiles `worker-graceful-stop`, `worker-crash` | READY |
| migrations | same image → `developer-platform-migration up` (migrator role), one-shot before API / worker; re-runnable; read-only pending check at runtime | `schema_upgrade.rs` (pre-Part-VI → current for 6 services), `compose.part-vi.yml` `developer-platform-migrate` | READY |
| api-gateway | `services/api-gateway/Dockerfile` → `api-gateway`; no database; Redis quotas with bounded timeouts and reconnect; `/ready` 503 while Platform Access is unreachable; graceful SIGTERM; `--check-config` | GW `startup.rs`, `ingress.rs`, `quota.rs`; profiles `redis-outage`, `platform-access-outage` | READY |
| Developer Console | `apps/web` | — | BACKEND_NOT_APPLICABLE_FRONTEND (UI excluded; APIs ready: [`developer-console-api-handoff.md`](developer-console-api-handoff.md)) |
| TypeScript / Dart SDK CI | `.github/workflows/sdk-release.yml` (manual): builds and verifies both SDK artifacts; publication job gated by `SDK_PUBLICATION_ENABLED == 'true'` + explicit input + `sdk-release` environment + publishable manifests (fails closed today) | workflow review; local artifact build (TS tarball, Dart archive) | READY (publication AWAITING_AUTHORIZED_CUTOVER; Dart `LICENSE` decision open) |

Deployment manifest: [`compose.part-vi.yml`](../../../infra/deployment/part-vi/compose.part-vi.yml)
(`docker compose config` validates; every rollout control explicitly off; secrets only as
files / secret env files). Not wired into any workflow.

## 21.2 Feature flags and cutover controls

| Spec control | Configuration (default **off**) | Gate tested by | Independent rollback rehearsed |
|---|---|---|---|
| `DEVELOPER_PLATFORM_ENABLED` | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`; gateway family `developer-platform` | DP control-plane tests; rehearsal step 2 | R5 |
| `PUBLIC_API_GATEWAY_ENABLED` | `API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_WEBSOCKETS_ENABLED`, per family `API_GATEWAY_DISABLED_DOMAINS` | GW `ingress.rs` (deny-all 404) | R6 (all), R3 (one family) |
| `CENTRAL_WEBHOOKS_ENABLED` | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` + `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` (`shadow`/`active`), `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`, producers' public-event destinations | `webhook_migration.rs` (freeze, transfer, takeover, rollback) | R2 |
| `CENTRAL_USAGE_ENABLED` | `API_GATEWAY_CENTRAL_USAGE_ENABLED`; producers `CFMEET_USAGE_EXPORT_ENABLED` | `usage_migration.rs` (backfill, shadow, switch, rollback) | R1 |
| `PRODUCTION_ACCESS_WORKFLOW_ENABLED` | `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` (new in this pass) | DP `production_access_workflow_flag_gates_only_the_customer_flow` | R4 |
| `SDK_PUBLICATION_ENABLED` | repository / environment variable read by `sdk-release.yml` (new) | workflow condition; manifests fail closed | n/a (unset) |
| Legacy Calling webhooks | `CALLING_WEBHOOK_AUTHORITY`, `CALLING_WEBHOOK_DELIVERY_ENABLED`, `CALLING_WEBHOOK_MUTATIONS_FROZEN` | Calling suite, `webhook_migration.rs` | yes (runbook §5) |
| Legacy Calling usage | `CALLING_LEGACY_USAGE_AUTHORITY` (`active` / `retired`, new) | Calling `legacy_usage_views_can_be_retired_independently` | yes |

No control is switched on by this work: `preflight.sh --expect dark` refuses a release
whose configuration turns any control on.

## Rollout and rollback rehearsal (isolated, synthetic)

`certification/tests/rollout_rehearsal.rs` — real binaries, synthetic tenants and
subscriptions, per-run databases and test keys:

1. stack starts fully dark (gateway deny-all, DP API off, webhooks off, usage off,
   workflow off) — every service ready, migrations current;
2. DP API on — still unreachable externally;
3. public routing on — sandbox smoke across the families through the gateway;
4. production-access workflow on;
5. central webhooks (shadow → gateway) and central usage on;
6. rollbacks one at a time — R1 usage, R2 webhooks, R3 one family (SMS), R4 workflow,
   R5 DP API, R6 public routing — each checking the other controls and every domain
   service are unaffected; re-enabling restores service with no data loss.

Mutation check: breaking a flag's wiring makes the rehearsal fail. The two data
migrations (usage, webhooks) are rehearsed with rollback in `usage_migration.rs` and
`webhook_migration.rs`.

## Preflight (read-only gate)

[`preflight.sh`](../../../infra/deployment/part-vi/preflight.sh): `--check-config` of both
binaries with the deployment's env files, every control off, migrations current (migrator
env), `/ready` probes. Run against a locally provisioned database (synthetic keys):

```
PASS  developer-platform configuration valid
PASS  developer-platform rollout controls all off
PASS  api-gateway configuration valid
PASS  api-gateway rollout controls all off
PASS  developer-platform migrations current
PASS  developer-platform ready
```

The first run of this gate **failed**: the Developer Platform could not start under its
least-privilege runtime role (sqlstate 42501 reading migration state). The same defect
was in every Rust service; it is fixed and now covered by the certification stack running
every provisioned service under its runtime role (chapter 20, defect 7).

## Graceful shutdown

The DP API, DP worker and gateway now stop on SIGTERM (container stop / rolling deploy)
without dropping in-flight work: servers drain, the worker finishes the send in flight and
releases its other claims for immediate pickup. Asserted in the DP and gateway
`startup.rs` tests and the `worker-graceful-stop` load profile (0 duplicates, 0 held).

## 21.3 Third-party dependency position

Confirmed: no carrier, numbering provider, SMS provider, billing processor or external
SaaS was used to implement or certify Part VI; sandbox flows run on the deterministic
provider boundaries, and no live provider call was made.

## Awaiting authorized cutover

Production database and role provisioning, secret mounting, image publication to the
registry, Platform Access production trust-bundle entries, edge routing to the gateway,
each flag activation in production, Calling webhook takeover in production, SDK
publication, and `staging` → `main` promotion.
