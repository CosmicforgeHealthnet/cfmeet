# Part VI Chapter 23 — production acceptance matrix

Three acceptances are kept separate and never merged into one verdict:

* **Backend software** — implemented and proven on staging by automated tests against the
  real processes (this pass). `ACCEPTED` / `NOT ACCEPTED`.
* **Production release** — the same software operated in production (deployment, flags,
  data cutover, approvals). Nothing was deployed: `AWAITING_AUTHORIZED_CUTOVER` unless the
  area has no production step.
* **Frontend product** — the Developer Console experience (Chapter 18 UI), excluded from
  this backend scope: `NOT IN SCOPE (frontend)` where the area has a UI element.

The 17 areas of spec §23 plus load / recovery (required by §24) — 18 items.

| # | Area | Backend software | Evidence | Production release | Frontend product |
|---|---|---|---|---|---|
| 1 | Multi-tenant control plane | ACCEPTED | DP `control_plane.rs`; CERT `tenant_isolation_journey`, `external_developer_sandbox_journey`; activity history tenant-scoped | AWAITING_AUTHORIZED_CUTOVER (DP deployment) | NOT IN SCOPE (Console screens) |
| 2 | Platform Access authority | ACCEPTED | DP schema stores PA ids only (migration header + architecture test); credential operations delegated (`chapters-10-13.md`); boundary guard `IDENTITY_HIERARCHY_DUPLICATED` | n/a (property of the software) | — |
| 3 | API Gateway | ACCEPTED | one ingress for REST + realtime; INTERNAL / OPERATOR / PROVIDER classes never routable (GW catalogue test, `ingress.rs`); load profiles | AWAITING_AUTHORIZED_CUTOVER (edge routing, `API_GATEWAY_PUBLIC_ROUTING_ENABLED`) | — |
| 4 | Versioned contracts | ACCEPTED | `contracts-validate` (82 contracts), `public_api.py --check`, `bundle_openapi.py --check`, no unresolved refs / audience leakage | n/a | — |
| 5 | TypeScript SDK | ACCEPTED | 499 operations generated from public contracts only; TS tests; CI sdk-parity against the real gateway / services | AWAITING_AUTHORIZED_CUTOVER (npm publication) | — |
| 6 | Dart / Flutter SDK | ACCEPTED | core journey parity against the real gateway in CI; native crypto bridge keeps keys client-side; cross-SDK MLS interop | AWAITING_AUTHORIZED_CUTOVER (pub.dev publication; LICENSE decision) | — |
| 7 | Webhooks | ACCEPTED | project-scoped subscriptions, HMAC + timestamp, retries, dead letter / history, replay, endpoint health, SSRF; producers in Calling, Privacy Relay, Messaging, Numbering and SMS: CERT `webhooks.rs`; load fan-out, replay, DB outage (0 duplicates), worker stop / crash | AWAITING_AUTHORIZED_CUTOVER (`CENTRAL_WEBHOOKS_ENABLED`) | NOT IN SCOPE (Console webhook screens) |
| 8 | Credential lifecycle | ACCEPTED | sandbox / test / production separation, scopes, expiry / rotation / revocation, service accounts: CERT `credential_lifecycle_and_caller_quota_journey`, `credential_revocation_journey`, `service_accounts.rs`; revoked under load → 401 next request | AWAITING_AUTHORIZED_CUTOVER (production trust bundles) | NOT IN SCOPE (Console credential screens) |
| 9 | Quotas | ACCEPTED | global RPM / daily aggregate across families (CERT `quota_enforcement_journey`, load `multi-tenant-quota-race`); domain quotas stay with owners; Redis outage bounded | AWAITING_AUTHORIZED_CUTOVER (production Redis) | — |
| 10 | Sandbox | ACCEPTED | sandbox / test requests use deterministic adapters only; no live provider call anywhere in certification | n/a | — |
| 11 | Production approval | ACCEPTED | telecom / numbering production capabilities disabled until explicit operator approval; only approved capabilities activate: CERT `production_approval_journey`, `external_onboarding_to_production_journey`; workflow flag | AWAITING_AUTHORIZED_CUTOVER (`PRODUCTION_ACCESS_WORKFLOW_ENABLED`; live telecom activation) | NOT IN SCOPE (Console request screens) |
| 12 | Usage metering | ACCEPTED | 8 fact types from their owning producers, idempotent, correct rollups: CERT `usage_ledger_producers_journey`; load ingestion / replay / rebuild | AWAITING_AUTHORIZED_CUTOVER (`CENTRAL_USAGE_ENABLED`, producer exports) | NOT IN SCOPE (Console usage screens) |
| 13 | Internal CosmicForge | ACCEPTED | CERT `first_party_product_journey`, `first_party_fixtures_journey` — published contracts, trusted scopes, no DB coupling, no product-specific fork | AWAITING_AUTHORIZED_CUTOVER (first-party production onboarding) | — |
| 14 | External customer | ACCEPTED | CERT `external_onboarding_to_production_journey`: onboarding → sandbox → webhook verification → usage → production approval, no manual DB edit | AWAITING_AUTHORIZED_CUTOVER (customer cutover) | NOT IN SCOPE (Console onboarding UI) |
| 15 | Migration | ACCEPTED | Calling webhooks: `webhook_migration.rs` (no secret leakage, no duplicate delivery, rollback); usage visibility: `usage_migration.rs` (no double counting, credential continuity, rollback); schema upgrade from the pre-Part-VI baseline (`schema_upgrade.rs`) | AWAITING_AUTHORIZED_CUTOVER (production takeover per runbook) | — |
| 16 | Developer Console | NOT APPLICABLE (backend APIs ready: `developer-console-api-handoff.md`) | live control-plane APIs exist for every Console surface | — | NOT IN SCOPE — **not accepted**: the stale Developer Guide is not replaced by this work |
| 17 | Rollback | ACCEPTED | `rollout_rehearsal.rs` R1–R6: gateway (all / one family), central webhooks, central usage, workflow and DP API disabled independently without corrupting domain authority | AWAITING_AUTHORIZED_CUTOVER (production rehearsal in the change window) | — |
| 18 | Load / recovery | ACCEPTED (TEST criteria for this host; not capacity claims) | `chapter-20-certification.md` §E: 13 profiles PASS on release binaries; reports in `evidence/` | AWAITING_AUTHORIZED_CUTOVER (production SLOs not documented; capacity test on production-class infrastructure) | — |

Backend software: 17 ACCEPTED, 1 NOT APPLICABLE (Developer Console UI).
Production release: not accepted — nothing deployed.
Frontend product: not accepted — out of scope.
