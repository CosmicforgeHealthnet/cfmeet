# Part VI Chapters 14–18 — backend completion and Developer Console API readiness

Staging only (base `4a69da152a525406419a6d0ba2b989213fd74921`). Nothing was merged to
`main`, deployed, or cut over. Chapters 14–17 consolidate Sections 50–57, so most rows
point at existing evidence ([`chapters-6-9.md`](chapters-6-9.md),
[`chapters-10-13.md`](chapters-10-13.md)). **NEW** marks what this pass added.

Test locations:

* DP = `services/developer-platform/tests/`
* GW = `services/api-gateway/tests/`
* CERT = `certification/tests/` (real processes)
* GEN = `scripts/contracts/test_public_api.py`

## Chapter 14 — data model

| Requirement | Status | Evidence |
|---|---|---|
| All 18 aggregates (memberships, invitations, project/environment profiles, production requests and decision history, plans, assignments, billing accounts, entitlement snapshots, API daily counters, webhook subscriptions/events/deliveries/attempts, usage facts, daily rollups, activity, support profiles) | COMPLETE | `devplat_*` tables in `services/developer-platform/migration/src/m2026110{1..5}*`, `m20261110*`, `m20261120*` |
| Platform Access UUIDs as references only, no cross-database FK, no identity hierarchy | COMPLETE | DP `foundation.rs::the_schema_owns_only_devplat_tables_and_no_identity_hierarchy`, `canonical_environment_references_are_immutable`; boundary guard `IDENTITY_HIERARCHY_DUPLICATED` |
| No secret, verifier, protected content or E2EE key columns; activity summaries refuse secret-like keys | COMPLETE | DP `foundation.rs` schema test; `ck_devplat_activity_events_summary`; webhook secrets sealed (`webhooks.rs::history_is_immutable_and_secrets_are_only_sealed`) |
| Immutable facts cannot be overwritten (activity, webhook events/attempts, API counters) | COMPLETE | DP `control_plane.rs` (activity UPDATE/DELETE/TRUNCATE refused), `webhooks.rs::history_is_immutable…` |
| Immutable Chapter 10–13 records (usage facts, snapshots, production transitions, usage rejections, reconciliations; production requests never deleted) | COMPLETE (**NEW test**) | DP `chapters_10_13.rs::chapter_14_15_immutability_pagination_and_activity_filters` |
| Idempotency and safe concurrent mutation (`devplat_idempotency_records`, versions, unique keys) | COMPLETE | DP `control_plane.rs` (idempotent replay, concurrent demotion), `webhooks.rs::concurrent_workers…` |
| Migrations compatible with deployed versions (additive; down refuses to drop history) | COMPLETE | DP `foundation.rs::migrations_apply_roll_back_and_reapply…`; **NEW** additive `m20261201_000012_event_rejections` |

## Chapter 15 — public Developer Platform API

| Family | Status | Evidence |
|---|---|---|
| Organizations and memberships (CRUD, invite/accept/revoke, role change, leave, close, ownership transfer) | COMPLETE | DP `control_plane.rs`; CERT `journeys.rs::tenant_isolation_journey` |
| Projects, environments, service accounts | COMPLETE | DP `control_plane.rs`; CERT `service_accounts.rs` |
| API credentials (secret once, rotate, revoke, suspend; **NEW** cursor pagination) | COMPLETE | DP `chapters_10_13.rs` (secret-once test plus **NEW** paging); CERT `credential_lifecycle_and_caller_quota_journey` |
| Scopes and grantability, limits and entitlements | COMPLETE | DP `chapters_10_13.rs` (registry/trust, quota precedence) |
| Production access (submit/read/cancel/activate, decisions; **NEW** cursor pagination) | COMPLETE | DP production test plus **NEW** paging; CERT `external_onboarding_to_production_journey` |
| Webhooks and delivery history, replay | COMPLETE | DP `webhooks.rs`; CERT `webhooks.rs` (6 journeys) |
| Usage queries/exports, limits | COMPLETE | DP usage tests; CERT `usage_ledger_producers_journey` |
| Activity history by tenant (**NEW**: `projectId` and `action` filters; foreign project → 404) | COMPLETE | DP **NEW** test above |
| Commercial profile and support reads/updates | COMPLETE | DP `control_plane.rs`, `chapters_10_13.rs::first_party_usage…` |
| Session vs credential auth, roles and scopes, tenant-safe 404, request/correlation ids, idempotency, operator routes separate | COMPLETE | DP `control_plane.rs`, `foundation.rs`; route classes (operator routes never PUBLIC_CUSTOMER) |

## Chapter 16 — route catalogue and versioning

| Requirement | Status | Evidence |
|---|---|---|
| Manifest generated from contracts plus explicit classifications; default deny; internal/operator/callback never public | COMPLETE | `scripts/contracts/public_api.py --check` (contracts.yml); GW `routing.rs`, `ingress.rs` |
| **NEW**: unique route ids (`domain.operationId`), operationId required | COMPLETE | `public_api.py`; GEN mutation cases |
| **NEW**: deprecation needs `x-cfmeet-replaced-by` naming a live public route (no dangling, self or chained replacements); carried into the manifest | COMPLETE | `public_api.py::check_replacements`; GEN tests; wired into contracts.yml |
| Per-domain and aggregate public OpenAPI, no unresolved references, no internal leakage | COMPLETE | `public_api.py --check`, `bundle_openapi.py --check`, Swagger render job |
| Gateway, OpenAPI, TypeScript and Dart parity (497 operations) | COMPLETE | `sdks/generate.py --check`; GW `routing.rs::the_generated_catalogue_is_in_parity…`; **NEW** GW `ingress.rs` (SDK operation count = manifest routes) |
| REST and WebSocket routing, authentication, quotas, header stripping | COMPLETE | GW `ingress.rs`, `platform_access_e2e.rs`; CERT `websocket_gateway_journey` |

## Chapter 17 — events and usage ingestion

| Requirement | Status | Evidence |
|---|---|---|
| Durable envelopes, authenticated producer, registered types, explicit `schema_version`, dedupe by producer + source id, safe projection | COMPLETE | DP `webhooks.rs::ingestion_dedupes_refuses_spoofing…` |
| **NEW**: conflicting event replays refused (`400 CONFLICTING_REPLAY`), recorded append-only, original kept | COMPLETE | DP `webhooks.rs::conflicting_replays_are_refused_recorded_and_surface_in_ingestion_health` |
| **NEW**: failed-ingestion monitoring, `GET /internal/v1/operator/ingestion/health` (OK/DEGRADED; per-producer usage and event rejections, reconciliation mismatches, stream watermarks; operator only) | COMPLETE | DP test above; CERT `usage_ledger_producers_journey` (DEGRADED after a conflicting usage replay, real processes) |
| Eight usage types from their owning producers (gateway, Meeting, Calling, Numbering, SMS, Messaging) | COMPLETE | CERT `usage_ledger_producers_journey` |
| Watermarks/recovery, exact attribution, no cross-environment aggregation, no duplicates on replay, rollups/rebuild, backfill, reconciliation, safe queries/exports | COMPLETE | CERT usage journey; DP usage and backfill tests; `packages/usage-facts` tests |
| Feature flags and rollback (`CFMEET_USAGE_EXPORT_ENABLED`, `API_GATEWAY_CENTRAL_USAGE_ENABLED`, both off by default) | COMPLETE | config tests; no production cutover performed |

## Chapter 18 — Developer Console (backend readiness only)

| Requirement | Status | Evidence |
|---|---|---|
| APIs, schemas, authorization, errors, pagination and secret handling for all ten console areas | **BACKEND_API_READY** | [`developer-console-api-handoff.md`](developer-console-api-handoff.md) |
| **NEW**: SDK discovery for quick-starts (`GET /discovery` → `sdks.packages[]`, generated by `sdks/generate.py` into `infra/api-gateway/sdk-discovery.json`, drift-checked) | COMPLETE | GW `ingress.rs::probes_docs_and_discovery…`; CERT onboarding journey |
| Console UI | **FRONTEND_IMPLEMENTATION_DEFERRED** | `apps/web` not modified |

## Operation counts

| Surface | Before → after |
|---|---|
| Public (gateway PUBLIC_CUSTOMER and SDK operations) | 497 → 497 (new filters and paging are parameters of existing operations) |
| Developer Platform internal / operator | 52 → 53 (+ `operatorIngestionHealth`) |
| Platform Access internal | 66 → 66 |

## Open blocker (unchanged, not suppressed)

RUSTSEC-2026-0387 (`crypto-bigint 0.5.5`) remains in the MLS dependency path of
`packages/messaging-crypto`, and therefore in Messaging, the Meeting Service, certification
and the wasm/web-harness builds. Dependency security stays red for those lockfiles; it is
not a passing production security gate. Details: [`chapters-10-13.md`](chapters-10-13.md).
