# Part VI backend final closure register (Chapters 19–24)

**Final decision: BACKEND_CERTIFIED_RELEASE_PENDING.**

The Part VI backend software is implemented and certified on `staging`. It is not
released: every production step is AWAITING_AUTHORIZED_CUTOVER, and external release
gates remain (§13–§15). The complete original Part VI (backend **plus** the Chapter 18
Developer Console UI) is **not** delivered: the UI was excluded from this scope.

## 1. Scope and constraints

Backend only, `staging` only. Not modified: `apps/web`, Developer Console pages and
components, browser assets, frontend API consumption, `frontend.yml`, Vercel, frontend
deployment workflows. Not performed: merge to `main`, force-push, rebase, production
deployment, VPS change, production secrets, live telecom / carrier traffic, live
provider calls, customer cutover, SDK publication, silent flag activation. Rehearsals
used synthetic tenants, per-run databases and test keys only.

## 2. Baseline and SHAs

| Item | Value |
|---|---|
| Starting `origin/staging` | `4d92f24820ab3f29fa41b6658ffb3bad9e83650d` (CI green) |
| `origin/main` (unchanged) | `454440c66a84cc643a446fdcb8fb14de30235463` (21 commits not in `staging`) |
| Part VI base | `5c2430c19449bbde28993dda2bd88fe1ffde9e92` |
| Commits of this pass | `d75e4b8` (Ch19), `2522f06` (Ch20), `1b16fc1` (Ch21 controls), `2d11ce8` (Ch20–21 defects, deployment readiness), `672f4ed` (Ch22–24, guard wording fix) and this CI-record commit (docs only) |

Reconciliation with `main`: [`baseline-and-branch-reconciliation.md`](baseline-and-branch-reconciliation.md).

## 3. Chapter results

| Stage | Chapter | Result | Record |
|---|---|---|---|
| 0 | Baseline | Baseline frozen; CI green; `main` divergence analysed; merge rehearsed (merged-tree checks incl. Meeting Service swagger 7/7 pass) | `baseline-and-branch-reconciliation.md` |
| 1 | 19 Migration and compatibility | PASS — `/v1/usage` `/v1/limits` contract defect fixed; schema upgrade; usage migration rehearsal | `chapter-19-migration.md` |
| 2 | 20 Testing and certification | PASS — 7 defects found and fixed; load / resilience measured | `chapter-20-certification.md` |
| 3 | 21 Deployment and rollout | DEPLOYMENT_READY (not deployed) — controls, preflight, rehearsal, graceful shutdown, runtime-role fix | `chapter-21-deployment.md`, `infra/deployment/part-vi/` |
| 4 | 22 A–Z workstreams | 21 BACKEND_COMPLETE, 4 BACKEND_COMPLETE_AWAITING_CUTOVER, 1 BACKEND_NOT_APPLICABLE_FRONTEND; 0 BLOCKED / PARTIAL / UNTESTED | `chapter-22-workstreams.md` |
| 5 | 23 Acceptance matrix | Backend software 17 ACCEPTED + 1 N/A (Console UI); production release not accepted (nothing deployed); frontend not in scope | `chapter-23-acceptance.md` |
| 6 | 24 Definition of Done | this register | §12 |

## 4. Public and internal surface

| Class (route catalogue, `public_api.py --check`) | Count |
|---|---|
| PUBLIC_CUSTOMER (gateway-routable, in the SDKs) | **499** |
| PUBLIC_INFRASTRUCTURE (health, discovery, docs) | 17 |
| INTERNAL_SERVICE (never routable) | 33 |
| OPERATOR_ADMIN (never routable) | 36 |
| PROVIDER_CALLBACK (never routable) | 4 |
| EXCLUDED with recorded reasons | 72 |

Internal (never externally routable) total: **73**. SDK parity: 499 operations in both
SDKs; bundles current (11 documents); 82 contracts validated.

## 5. Test and certification evidence

Local, on the final code (PostgreSQL 16, Redis 7, real binaries):

| Suite | Result |
|---|---|
| Developer Platform (all test targets) | 120 passed |
| API Gateway (`make api-gateway-test`: unit, ingress, routing, websocket, startup, central webhooks) + `make api-gateway-integration` | 43 + 1 passed |
| Runtime-role change — full suites: Calling / Messaging / Numbering / SMS Gateway / Platform Access / Privacy Relay / Telephony | 720 / 250 / 215 / 87 / 124 / 180 / 165 passed, 0 failed |
| fmt + clippy `-D warnings`: DP, gateway, Calling, Messaging, Numbering, Platform Access, Privacy Relay, SMS Gateway, Telephony, certification | clean |
| Cross-service certification (`certification/`, debug, serial; every provisioned service under its no-DDL runtime role) | 26 passed, 0 failed (4 unit + 22 journeys / rehearsals / load suites) |
| Contracts / manifest / bundles / SDK generation / route-catalogue tests | pass (§4) |
| Service boundaries + operations registry, secret scan | OK (62 responsibilities); clean |

Journey matrix: [`chapter-20-certification.md`](chapter-20-certification.md) §B.

## 6. Load and recovery (measured, release binaries, TEST criteria — not capacity claims)

Host: Intel Xeon 2.10 GHz, 4 vCPU, 15.7 GiB, Linux 6.18; 10 service processes, PostgreSQL
16, Redis 7 on one host. Reports: [`evidence/`](evidence/).

| Profile | Measured |
|---|---|
| Mixed REST (6 families, 16 tenants, 72 req/s, 2,160 requests) | 0 errors; p50 7.2 ms, p95 9.8 ms, p99 12.0 ms |
| Quota race (limit 40/min) | exactly 40 admitted per tenant |
| WebSockets connect / reconnect / close | 60/60 |
| Redis outage 8 s | each request bounded 256–262 ms, 40 of 80 admitted (never unlimited); shared again 1.23 s after restore |
| Calling outage / Platform Access outage | 502 for Calling only, others 200 / 503 fail-closed + `/ready` 503; recovery 0.02 s |
| Webhook fan-out + retry backlog | 200/200 delivered exactly once; drained 27.7 s |
| Usage ingestion + full replay | 4,355 facts/s; replay 7,061 facts/s recording 0 new; exactly one fact per source id |
| Rollup rebuild under ingestion | 0 mismatching rows |
| DP database outage 10 s (99 deliveries + 4,000 facts in flight) | `/ready` 503 → ready 0 s after; exactly 4,000 facts; 0 duplicate deliveries |
| Worker graceful stop / crash (backlog 100) | stop: 0 duplicates, 0 held; crash: nothing lost, the one interrupted send re-sent with the same delivery id |

## 7. Security and supply chain

RustSec: `cargo audit` exit 0 on all 20 lockfiles of the CI matrix; only the two
pre-existing documented ignores (RUSTSEC-2023-0071, RUSTSEC-2026-0381); **no ignore
added**. Secret scan clean. Tenant isolation, IDOR, signed-context forgery, internal
route exposure, SSRF, replay / duplicate, production / sandbox separation: journeys and
suites in chapter 20 §D. The Developer Platform stores no credential verifier or raw
secret (Platform Access is the only authority). Services now run under least-privilege
database roles in certification.

## 8. Migration and compatibility

Calling webhooks: export / import / shadow parity / takeover / rollback without secret
leakage or duplicate delivery (`webhook_migration.rs`, runbook). Usage: backfill without
double counting, shadow parity, switch, credential continuity, rollback
(`usage_migration.rs`); `/v1/usage` `/v1/limits` keep Calling's contract; legacy views at
`/v1/calling/usage` `/v1/calling/limits`. Schema upgrade from the pre-Part-VI baseline for
six services (`schema_upgrade.rs`).

## 9. Deployment readiness

DEPLOYMENT_READY: deployment units, compose manifest (controls off), read-only preflight
(all PASS against a provisioned database under the runtime role), graceful SIGTERM in DP
API / worker / gateway. Chapter 21.

## 10. Rollout and rollback

All six spec controls plus the two legacy Calling controls map to configuration, default
off; `preflight.sh --expect dark` refuses an accidental activation. Independent rollback
rehearsed on the real processes (R1–R6, `rollout_rehearsal.rs`).

## 11. Defects found and fixed in this pass

1. `/v1/usage` / `/v1/limits` answered outside Calling's published contract after the
   central-usage switch (would break SDK decoders).
2. Gateway quota calls to Redis unbounded; no reconnect after a startup outage.
3. Gateway reported ready while Platform Access was down.
4. Usage-ingestion body limit (64 KiB) refused every exporter batch ≥ 98 facts.
5. Duplicate webhook delivery across a Developer Platform database outage.
6. No SIGTERM handling in DP API / worker / gateway (duplicates on container stop).
7. No Rust service could start under its least-privilege runtime role (migration-state
   check needed DDL) — fixed in all eight; certification now runs services under it.

Details and regression tests: chapter 20 "Defects found by these tests and fixed".

## 12. Definition of Done (§24) — backend view

| §24 item | Status |
|---|---|
| `main` synchronized before development; Part VI commits certified on staging | Part VI base synchronized; `main` has since moved 21 commits (Swagger split) — merge rehearsed, AWAITING_AUTHORIZED_CUTOVER |
| `services/developer-platform` own DB, no duplicate authority | MET |
| `services/api-gateway` tested public ingress, approved operations only | MET (software); live edge AWAITING_AUTHORIZED_CUTOVER |
| Platform Access sole identity / verifier authority | MET |
| Orgs / projects / environments / memberships / service accounts / credentials operable through the API with isolation | MET (API); Console UI NOT IN SCOPE |
| Plans and billing references without a payment engine | MET |
| TypeScript SDK complete and certified; Dart SDK complete for the required surface | MET; publication AWAITING_AUTHORIZED_CUTOVER |
| Public docs generated from contracts, no internal routes | MET |
| Central webhooks signed / retried / replayable from all required domains | MET |
| Calling webhooks migrated or retired through a proven plan | MET (proven plan + tooling); production takeover AWAITING_AUTHORIZED_CUTOVER |
| Unified usage facts and rollups for all master-required types | MET |
| Global quotas across services; authoritative limits view | MET |
| Sandbox / production separation; no live side effects from sandbox | MET |
| Production approval enforced | MET |
| Internal and external use the same contracts | MET |
| Static developer dashboard replaced by the live Console | **NOT MET — frontend, excluded** |
| Migrations, journeys, isolation, parity, webhook reliability, usage idempotency, SDK, load / recovery green | MET locally; CI in §16 |
| Closure register with no PARTIAL / ABSENT / UNTESTED software requirement in §§50–57 | MET for backend (chapter 22) |

## 13. Remaining backend blockers

None inside the backend software scope.

## 14. External and commercial follow-ups (release gates)

* Independent review of CF Meet's own OpenMLS provider (ADR 0045) and confirmation of
  MPL-2.0 source obligations for the shipped messaging crypto — not performed by an
  independent party.
* SDK licensing decision (Dart `LICENSE`; TypeScript `private`) before publication.
* `main` → `staging` reconciliation merge (rehearsed; plan in the baseline record).
* Production SLOs are not documented; load figures here are TEST floors on one host.
* Developer Console UI (Chapter 18) — frontend workstream.

## 15. Production approvals — AWAITING_AUTHORIZED_CUTOVER

Production database and roles (`provision-service-database.sh`), secrets, image
publication, Platform Access production trust bundles, deployment of DP API / worker /
migrations / gateway, edge routing to the gateway, each control activation (public
routing, DP API, central webhooks, central usage, production-access workflow), Calling
webhook takeover, producer usage exports, SDK publication (`SDK_PUBLICATION_ENABLED`),
live telecom / numbering activation for approved customers, `staging` → `main`
promotion.

## 16. Final certified code SHA and CI

Certified code: **`672f4edfaec652e0c03698f787c587ae9b761aa0`** (the commit after it only
records this section). GitHub Actions on that SHA (push to `staging`): **17 of 17
triggered workflows green** (contracts and SDK workflows are path-filtered and were not
triggered: no contract or SDK change after `d75e4b8`, where they passed) — Part VI cross-service certification, Developer Platform service, API
Gateway service, Calling service, Calling RTC certification, Enterprise calling
certification, Messaging, Unified Messaging integration journeys, Communications
end-to-end journeys, Numbering service, SMS Gateway service, Privacy Relay service,
Telephony service, Telephony SIP certification, Platform Access service, Dependency and
image security (RustSec, SBOM, image builds incl. `developer-platform` and
`api-gateway`), Service boundaries.

History of red CI during this pass, all resolved:

| SHA | Failure | Cause | Resolution |
|---|---|---|---|
| `d75e4b8` | Part VI certification | journeys still asserted the old (non-contract) `/v1/usage` shape | assertions updated in `2d11ce8` |
| `1b16fc1` | Part VI certification | same as above (fix not yet pushed) | `2d11ce8` |
| `1b16fc1` | Unified Messaging integration journeys (web browser test, `CRYPTO_WORKER_CLOSED` at logout) | frontend test, code untouched by this pass | not reproduced: green on `2d11ce8` and `672f4ed` |
| `2d11ce8` | Calling, Developer Platform, Messaging, SMS Gateway (repository guards / Calling architecture test) | a doc comment quoting `CREATE TABLE IF NOT EXISTS` read as a foreign table | comment reworded in `672f4ed` |
