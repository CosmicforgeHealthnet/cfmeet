# Phase 2 backend certification and closure package (Parts M–N)

Service: `services/meeting-service`. Branch: `staging`. All evidence is **local** (a single
4-vCPU cloud container with real PostgreSQL, Redis, self-hosted LiveKit, Egress and an
S3-compatible store) unless stated otherwise. Staging and production steps are prepared
but **NOT EXECUTED**.

| # | Document | Content |
|---|---|---|
| 01 | [Summary](01-summary.md) | Outcome, findings, gaps, decision |
| 02 | [Requirements matrix](02-requirements-matrix.md) | Coverage audit, wave audit, DoD §57–§74, invariants, architecture questions, deliverables |
| — | [Route coverage](route-coverage.md) | Generated: every v2 operation → success / error-path evidence |
| 03 | [Migration report](03-migration-report.md) | 20 000-meeting synthetic legacy dataset |
| 04 | [Test report](04-test-report.md) | Commands, counts, suites, environment |
| 05 | [System E2E](05-system-e2e-report.md) | M-E2E-01..11 + recovery scenarios |
| 06 | [Load / concurrency](06-load-concurrency-report.md) | Burst join, fanout, churn, scheduler, query plans, media load, soak |
| 07 | [RTC provider certification](07-rtc-provider-certification.md) | Real `livekit-server` + WebRTC |
| 08 | [Recording / storage certification](08-recording-storage-certification.md) | Real Egress + S3-compatible storage |
| 09 | [Security review](09-security-review.md) | M-S1..15, IDOR, replay, findings |
| 10 | [Failure / recovery](10-failure-recovery-report.md) | §29–§35, reconciliation evidence |
| 11 | [Staging certification](11-staging-certification.md) | PENDING ENVIRONMENT |
| 12 | [Rollout plan](12-rollout-plan.md) | Flag audit, combinations, stages, gates, canary, legacy-write retirement |
| 13 | [Rollback plan](13-rollback-plan.md) | Levers, certification, drill |
| 14 | [Operational runbook](14-operational-runbook.md) | Runbooks, dashboards, alerts |
| 15 | [OpenAPI / realtime compatibility](15-openapi-realtime-compatibility.md) | Contract parity, realtime protocol, consumers |

Reproduce:

```sh
cd services/meeting-service
cargo test --workspace --no-fail-fast                       # full regression
python3 scripts/certification/route_coverage.py --strict    # v2 operation coverage
scripts/certification/run-load.sh                           # load, migration report, query plans
scripts/livekit-certification/run.sh                        # real LiveKit (+ Egress / S3 with EGRESS_BIN / MINIO_BIN)
```

CI: `.github/workflows/backend.yml` (fast gate, `main` + `staging`) and
`.github/workflows/phase2-backend-certification.yml` (manual full certification with artifacts).
