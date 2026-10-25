# Part VI Chapter 20 — backend testing and certification

Staging only. Every suite below was re-run on the changed code of this closure pass
(nothing relies on earlier green results for changed code). The final SHA and CI run
links are in [`backend-final-closure.md`](backend-final-closure.md).

## A. Unit and service suites (local, PostgreSQL 16, Redis 7)

| Area | Suite | Result |
|---|---|---|
| Developer Platform (memberships / invitations, roles and scopes, project / environment mapping, Platform Access delegation, credentials, production approval / suspension, plans and entitlement precedence, webhooks signing / retry / replay / SSRF, usage immutability / ingestion / reconciliation, migration tooling) | `make developer-platform-test`, `make developer-platform-lint` | 120 passed; fmt + clippy clean |
| API Gateway (route allow-list, internal-route refusal, authentication context, signed context, header stripping, RPM / daily quotas, Redis degradation, limits, readiness, WebSockets) | `make api-gateway-test`, lint | 43 passed (incl. `--check-config` and SIGTERM startup tests) + `make api-gateway-integration` 1 passed; clean |
| Calling (incl. webhook migration export / rollback, legacy usage paths and retirement) | `make calling-test`, lint | all passed (one count assertion updated for the two new public paths); clean |
| Runtime-role startup change in Calling, Messaging, Numbering, Platform Access, Privacy Relay, SMS Gateway, Telephony | fmt + clippy `-D warnings` for each; full suites Calling 720, Messaging 250, Numbering 215, SMS Gateway 87, Platform Access 124, Privacy Relay 180, Telephony 165 (all passed); every service the certification stack starts (all of these except Telephony) runs under its runtime role | clean; 0 failed |
| Contracts | `make contracts-validate`, `make contracts-test`, `public_api.py --check`, `bundle_openapi.py --check`, `test_public_api.py` | 82 contracts / 269 examples / 343 counter-examples OK; 19 mutation tests; 499 public routes; bundles current |
| SDKs | `sdks/generate.py --check`; TypeScript `npm run typecheck && npm test`; Dart `dart analyze --fatal-infos`, `dart format` | 499 operations in parity; TS 143 passed; Dart analyze / format clean (Dart test: see §B SDK parity) |
| Boundaries, registry, secrets | `make service-boundaries`, `make secret-scan` | OK (62 responsibilities); clean |

## B. Cross-service journeys (real processes: every service binary, real Ed25519 service auth, one database per service, real Redis)

`make certification` (`certification/tests/`, serial, debug binaries):

| # | Journey | Test |
|---|---|---|
| 1 | External developer sandbox onboarding (organization → project → sandbox credential → Meeting / Calling / Messaging / Numbering / SMS through the gateway → signed webhook → usage) | `journeys.rs::external_developer_sandbox_journey`, `webhooks.rs::external_developer_signed_webhook_journey`, `meetings_platform.rs` |
| 2 | Production-access approval (operator approves selected capabilities; unapproved stays blocked) | `journeys.rs::production_approval_journey`, `chapters_10_13.rs::external_onboarding_to_production_journey` |
| 3 | Internal CosmicForge integration (same contracts, first-party scopes, no DB reads, events, usage allocation) | `journeys.rs::first_party_product_journey`, `chapters_10_13.rs::first_party_fixtures_journey` |
| 4 | Credential revocation (immediate at the gateway and services) | `journeys.rs::credential_revocation_journey`, `chapters_10_13.rs::credential_lifecycle_and_caller_quota_journey`, load profile `concurrent-authentication` (revoked under load → 401 on the next request) |
| 5 | Webhook failure, retry, dead letter, replay (same event id) | `webhooks.rs::webhook_failure_and_replay_journey`, `webhook_retry_after_is_honoured`, `webhook_ssrf_policy_journey` |
| 6 | Global quota aggregate across services; domain quotas by owners | `journeys.rs::quota_enforcement_journey`, load profile `multi-tenant-quota-race` |
| 7 | Multi-tenant isolation / IDOR | `journeys.rs::tenant_isolation_journey` |
| 8 | Meeting / Calling / Messaging WebSockets through the gateway | `journeys.rs::websocket_gateway_journey`, load profile `websocket-connect-reconnect-close` |
| 9 | Usage replay / duplicate / out-of-order → one fact per source id, correct rollups | `chapters_10_13.rs::usage_ledger_producers_journey`, `usage_migration.rs`, load profiles `usage-ingestion-throughput-and-replay`, `rollup-rebuild-under-load` |
| 10 | TypeScript / Dart SDK parity against the real gateway | CI `certification.yml` job `sdk-parity` (`sdks/testdata/parity-scenarios.json`; both SDKs against one `cfmeet-cert-stack`) |

Plus **NEW** in this pass: `schema_upgrade.rs`, `usage_migration.rs`,
`rollout_rehearsal.rs`, `load_resilience.rs` (two journeys), and the existing
`webhook_migration.rs`, `service_accounts.rs`, `webhooks.rs` (6), `meetings_platform.rs`
(2). Console interactions are exercised through their authenticated backend APIs (no
frontend test code).

## C. Gateway and contract certification

| Requirement | Evidence |
|---|---|
| Every customer route explicitly authorized; one gateway route per public operation; every routable public operation has a destination | `public_api.py --check` (default deny, 499 PUBLIC_CUSTOMER, 72 EXCLUDED with reasons); GW `routing.rs` catalogue parity |
| Internal / admin / provider routes unreachable externally | classes INTERNAL_SERVICE 33, OPERATOR_ADMIN 36, PROVIDER_CALLBACK 4 never routable (GW unit test over the whole catalogue); `ingress.rs` |
| No unresolved references, no duplicate operation ids / ambiguous routes | `public_api.py`, `bundle_openapi.py --check`, `test_public_api.py` |
| Status / header / request-id / correlation-id preservation; WebSocket upgrade; body / header / path / timeout limits | GW `ingress.rs`, `websocket.rs`, `platform_access_e2e.rs` |
| Cross-service quota aggregation | journey 6 |
| SDK parity | `sdks/generate.py --check` (499 operations both SDKs); GW `ingress.rs` (SDK operation count = manifest routes); CI sdk-parity |

## D. Security certification

| Check | Result |
|---|---|
| Tenant / project isolation, IDOR, privilege escalation, scope forgery, signed-context spoofing, operator-only approval, replay / duplicate processing, SSRF / redirects / DNS rebinding, production / sandbox separation, internal route exposure | journeys 2, 4, 5, 7; GW `ingress.rs` forgery tests; DP / CALL suites |
| Secret handling, redaction, logging | secret scan clean; no-plaintext tests (webhook migration); `--check-config` and startup tests never echo secrets |
| RustSec (`cargo audit` on all 20 lockfiles of the CI matrix, incl. the two Part VI services now added) | exit 0 on every lockfile with only the two pre-existing documented ignores: RUSTSEC-2023-0071 (`rsa`, no fixed release; not loaded by any RSA path) and RUSTSEC-2026-0381 (`webpki-roots` 0.26 shim, proven by `check_webpki_roots_shim.py`). Warnings only (unmaintained / yanked crates). **No new ignore added.** |
| Messaging crypto (MLS) remediation preserved | RUSTSEC-2026-0387 still closed (no `crypto-bigint 0.5`); messaging-crypto lockfiles audited without ignores |
| Supply chain | SBOM job covers every lockfile; image secret scan matrix now includes `developer-platform` and `api-gateway` |
| Independent MLS-provider assessment and MPL-2.0 obligations | **OPEN — external**: an independent review of CF Meet's own OpenMLS provider (ADR 0045) and confirmation of MPL-2.0 source-availability obligations for the shipped messaging crypto have not been performed by an independent party; recorded as a release gate |

## E. Load and resilience (measured)

Criteria were declared in the test before each run (TEST criteria for this environment,
not production SLOs — none is documented). Full reports:
[`evidence/gateway-load-resilience.json`](evidence/gateway-load-resilience.json),
[`evidence/webhook-usage-load-resilience.json`](evidence/webhook-usage-load-resilience.json).

Environment: isolated certification stack (10 service processes, PostgreSQL 16, Redis 7)
on one host — Intel Xeon @ 2.10 GHz, 4 logical CPUs, 15.7 GiB RAM, Linux 6.18; optimized
release binaries. These are floors for this shared host, **not capacity claims**.

| Profile | Volume | Measured | Result |
|---|---|---|---|
| Mixed REST, 6 families, 16 tenants | 2,160 req at 72 req/s (90 % of the aggregate TEST quota), 64 in flight | 0 errors; p50 7.2 ms, p95 9.8 ms, p99 12.0 ms | PASS |
| Concurrent first authentication | 48 credentials cold, 96 warm | 0 errors; cold p95 209 ms, warm p95 115 ms; revoked under load → 401 next request | PASS |
| Quota race | 4 tenants × 150 attempts, 48-way concurrent, limit 40/min | admitted exactly 40 each; 440 × 429; bystander unaffected | PASS |
| WebSocket connect / reconnect / close | 20 concurrent × 3 rounds | 60/60 cycles; connect p95 44 ms | PASS |
| Redis outage (proxy cut, 8 s) | 80 requests | all 200/429, each bounded 256–262 ms; admitted 40 of 80 (never unlimited); mode `degraded`; shared again 1.23 s after restore | PASS (after fix) |
| Domain outage (Calling stopped 5 s) | 24 + 24 + 24 | Calling and Privacy Relay (same process) 502 `UPSTREAM_UNAVAILABLE`; four independent families 24/24 200; recovery 0.02 s | PASS |
| Platform Access outage (5 s) | 25 | 25 × 503 (fail closed), gateway `/ready` 503; recovery 0.02 s | PASS (after fix) |
| Webhook fan-out + retry backlog | 5 subs × 40 events, 25 % of one endpoint failing | 200/200 delivered exactly once, max queue 196, drained 27.7 s (30 s-class retry backoff) | PASS |
| Replay volume | 30 concurrent replays | 30 × 201, new delivery id / same event id, delivered 0.41 s | PASS |
| Usage ingestion + full replay | 20,000 facts, 500/batch, 4 concurrent | 4,355 facts/s; replay 7,061 facts/s, 0 recorded / 20,000 duplicates; exactly 20,000 in the ledger | PASS (after fix) |
| Rollup rebuild under ingestion | 10,000 concurrent facts, 3 rebuilds | all 200; 0 mismatching rollup rows | PASS |
| Developer Platform DB outage (10 s) | 99 deliveries in flight + 4,000 facts | `/ready` 503 during; ready 0 s after; 74 retried batches → exactly 4,000 facts; wave delivered on recovery; 0 duplicate deliveries | PASS (after fix) |
| Worker graceful stop (SIGTERM, as a container stop / rolling deploy) | backlog of 100, slow endpoint (sends in flight) | exits within its grace period; 0 deliveries left held; nothing sent while down; backlog completed 39.9 s after restart; 0 duplicates | PASS (after fix) |
| Worker crash (SIGKILL) | backlog of 100 | nothing lost (completed 72 s after restart); the one send interrupted mid-flight re-sent as the SAME delivery id (at-least-once, deduplicable by receivers) | PASS |

Final run: the code of this closure (every provisioned service under its runtime role),
release binaries, `CFMEET_LOAD_REPORT_DIR` → [`evidence/`](evidence/) (0 failed criteria).
`rollout_rehearsal.rs` passed in the same run.

Debug-build run (as CI runs it, `make certification`): same correctness criteria with a
relaxed latency bound — passed in the full local certification run (26 tests, 0 failed).

### Defects found by these tests and fixed

1. **Gateway / Redis** — quota calls had no timeout and the store was never reconnected
   after a startup outage (reported `local`): a Redis partition could stall requests.
   Fixed: bounded calls and connects (`API_GATEWAY_REDIS_TIMEOUT_MS`), background
   reconnect, honest `degraded` mode (`quota.rs` unit test + `redis-outage` profile).
2. **Gateway readiness** — reported ready while Platform Access was down. Fixed: 503
   `not_ready` (`ingress.rs` + `platform-access-outage` profile).
3. **Usage ingestion body limit** — 64 KiB for every internal route while exporters send
   200-fact batches (~130 KiB): any backlog of ≥ 98 facts (e.g. a historical backfill)
   was refused forever. Fixed: 1 MiB for the ingestion route only (DP test with a full
   500-fact batch; fails at the old limit).
4. **`/v1/usage` / `/v1/limits` contract** — see [`chapter-19-migration.md`](chapter-19-migration.md) §19.4.
5. **Duplicate webhook delivery across a Developer Platform database outage** — a send
   that succeeded while the database dropped could not record its outcome; the delivery
   was claimed again after its lease and the customer received it twice. Reproduced
   deterministically on the old binary. Fixed: the outcome write is retried within the
   delivery's lease (`webhooks/worker.rs` `record_sent`); `developer-platform-database-outage`
   now asserts 0 duplicates with 100 deliveries in flight.
6. **No SIGTERM handling** in the Developer Platform API, worker and API Gateway — a
   container stop killed the worker mid-send (duplicates after the lease) and the
   servers never drained. Fixed: graceful shutdown on SIGTERM / Ctrl-C; a stopping worker
   finishes the send in flight and releases its unsent claims at once (lease expiry; the
   attempt counter never decreases). DP `webhooks.rs`
   `a_stopping_worker_finishes_the_send_in_flight_and_releases_the_rest`, `startup.rs`
   SIGTERM assertions (DP and gateway), profile `worker-graceful-stop`.
7. **Services could not start under their least-privilege runtime role** — every Rust
   service (Developer Platform, Calling, Messaging, Numbering, Platform Access, Privacy
   Relay, SMS Gateway, Telephony) checked pending migrations with
   `MigratorTrait::get_pending_migrations`, which runs `CREATE TABLE IF NOT EXISTS` on the
   migration table and is refused (sqlstate 42501) for the no-DDL runtime role that
   `provision-service-database.sh` creates: startup failed and `/ready` stayed
   `unknown`. CI never started a service under that role (it only checked SQL
   privileges). Fixed: a read-only `pending_migrations` in each service's migration crate
   (startup and readiness), keeping the old fail-closed rules (pending → refuse; a
   migration applied but unknown to the binary → error). Regression: DP `startup.rs`
   `the_server_starts_and_is_ready_under_the_runtime_role_without_ddl`, and the
   certification stack now runs **every provisioned service under its own runtime role**
   (granted exactly as in production), so every journey and load profile exercises
   production privileges. Found by the Chapter 21 deployment preflight.
