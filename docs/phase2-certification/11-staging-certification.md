# 11 — Staging certification (Part N §56)

## Classification

**STAGING DEPLOYMENT — EXECUTED 2026-10-06, FUNCTIONAL VALIDATION PASS** (see
[16](16-deployment-status.md)): an isolated staging environment now exists
(`https://api.staging.cfmeet.com`), deployed from `staging`, migrated on a restored copy of
production and validated functionally (35 / 35) with a rollback-compatibility run (15 / 15).
Steps 3–4 and 6–7 of the procedure below (external load, real media / Egress / bucket
certification, rollback drill, 24 h observation) were **not** run there: the host also runs
production, staging shares the production LiveKit project, and no object storage is
provisioned. The text below is the original pre-deployment record.

*Original classification (before a staging environment existed):*
**STAGING DEPLOYMENT CERTIFICATION — PENDING ENVIRONMENT**

No staging deployment of `services/meeting-service` was accessible from the certification
session: no staging URL, credentials, cluster access or deployment pipeline target was
provided, and none is configured in this repository (`.github/workflows` contains build /
test workflows only). Nothing in this package is staging evidence. All certification
evidence is **LOCAL** (single 4-vCPU / 15 GiB cloud container) unless a document says
otherwise. Local results are not relabelled as staging results.

## What was certified locally with real components (not staging)

| Component | Local certification |
|---|---|
| PostgreSQL | 16.14 (real) — all suites, migration certification (20 000 synthetic legacy meetings), query plans |
| Redis | 7.0.15 (real) — fanout, rate limiting, outage / recovery via a toggling proxy |
| LiveKit | self-hosted `livekit-server` 1.8.4 (real) + real WebRTC participants (`rtcprobe`, pion) |
| Egress | `livekit/egress` v1.14.1 container (real composite recording) |
| S3-compatible storage | MinIO RELEASE.2026-08-04 (`pgsty/minio` community build; the official image registry was not reachable from the container) |
| Feature flags | all combinations of §53 + rollback drill |
| Internal ops token | `/internal/*` exercised with `INTERNAL_API_TOKEN` (and 404 without it) |
| Data | synthetic only (no production or clinical data) |

## Staging-grade configuration (prepared)

Environment variables the staging deployment needs (values are environment secrets, never
committed):

| Variable | Purpose |
|---|---|
| `DATABASE_URL` | Production-like PostgreSQL 16 (restored from a sanitized snapshot or seeded with the synthetic generator in `tests/migration_certification.rs`) |
| `REDIS_URL` | Redis 7 |
| `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET` | Self-hosted LiveKit (and Egress registered to the same Redis) |
| object storage settings (`ObjectStorageConfig`: endpoint, region, bucket, access key, secret, path style, prefix) | S3-compatible bucket for recordings |
| `INTERNAL_API_TOKEN` | Operations endpoints / metrics scrape |
| `JWT_SECRET` | Auth |
| `FEATURE_*` | Stage 3 of 12-rollout-plan.md: all A–I on, `FEATURE_MEETING_V2_SHADOW_READ=true`, legacy writes **enabled** |

## Staging certification procedure (to execute when the environment exists)

1. Deploy this `staging` build; run migrations (`migration up`); confirm `migration status`.
2. Run the legacy backfill (`POST /api/v2/admin/compat/backfill`) on the production-like
   dataset; capture the report; compare with 03-migration-report.md expectations.
3. External load harness against the deployment:
   `LOAD_BASE_URL=https://<staging> LOAD_DATABASE_URL=… LOAD_JWT_SECRET=… scripts/certification/run-load.sh`.
4. Real provider certification against staging LiveKit / Egress / bucket
   (`scripts/livekit-certification/run.sh` with the staging endpoints).
5. M-E2E-01 … 11 by hand or with the system harness pointed at staging.
6. Rollback drill (13-rollback-plan.md) in staging.
7. 24 h observation of the dashboards in 14-operational-runbook.md.

Until these steps are executed in a real staging environment, stage 3 of the rollout plan
remains **NOT EXECUTED** and the production canary cannot start.
