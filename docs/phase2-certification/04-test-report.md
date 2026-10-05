# 04 — Backend test report (Part M, Part N §77, §93–§94)

## Environment

| Item | Version |
|---|---|
| OS / host | Linux 6.18.44, 4 vCPU Intel Xeon @ 2.80 GHz, 15 GiB RAM (cloud container) |
| Rust | rustc 1.97.0 (2d8144b78 2026-07-07), cargo 1.97.0 |
| PostgreSQL | 16.14 (Ubuntu 16.14-0ubuntu0.24.04.1) |
| Redis | 7.0.15 |
| LiveKit | `livekit-server` 1.8.4 (self-hosted, single node) |
| Egress | `livekit/egress` v1.14.1 (container) |
| Object storage | MinIO RELEASE.2026-08-04T00-00-00Z (`pgsty/minio` community build; S3 API) |
| WebRTC client | `rtcprobe` (Go 1.24.7, pion / server-sdk-go v2) |
| Docker | 29.6.2 (Egress / MinIO containers only) |
| Python | 3.11.15 (coverage report script) |

## Full regression

Command (in `services/meeting-service`, `TEST_DATABASE_URL` / `TEST_REDIS_URL` set,
`CARGO_INCREMENTAL=0`):

```
cargo test --workspace --no-fail-fast
```

Result: **exit 0 — 614 passed, 0 failed, 15 ignored** (52 test binaries, wall 7 m 39 s).
The 15 ignored are the explicitly-run certification suites (8 real-LiveKit, 5 load, 1
query-plan) and 1 pre-existing ignored doc-test; none is a required regression test that
was skipped. After this run, four lint-only edits (clippy) and a formatting-only edit were
made; the affected suites were re-run green (`--lib` 127, `idor_matrix` 1,
`db_invariants` 7, `migration_backfill` 12).

| Suite | Passed | Failed | Ignored |
|---|---|---|---|
| lib unit tests (`src/**`) | 127 | 0 | 0 |
| api_contract_matrix | 13 | 0 | 0 |
| **api_operation_coverage** (new) | 7 | 0 | 0 |
| api_v2_e2e | 16 | 0 | 0 |
| api_v2_roles | 11 | 0 | 0 |
| appointment_revocation | 5 | 0 | 0 |
| architecture_boundaries | 4 | 0 | 0 |
| collab_api / collab_concurrency / collab_e2e / collab_failure / collab_security | 9 / 12 / 25 / 10 / 18 | 0 | 0 |
| concurrency_v2 | 9 | 0 | 0 |
| **credential_replay** (new) | 8 | 0 | 0 |
| db_invariants | 7 | 0 | 0 |
| **failure_injection_m** (new) | 4 | 0 | 0 |
| **idor_matrix** (new) | 1 (61 attack attempts + 6 positive controls) | 0 | 0 |
| **invariants_certification** (new) | 4 | 0 | 0 |
| legacy_retirement / legacy_v1_join / legacy_v1_web_compat | 3 / 4 / 6 | 0 | 0 |
| livekit_certification (real provider) | — | — | 8 (run separately, 07 / 08) |
| load_certification | — | — | 5 (run separately, 06) |
| migration_backfill | 12 | 0 | 0 |
| **migration_certification** (new) | 1 (3 000-meeting dataset) | 0 | 0 |
| openapi_contract | 8 | 0 | 0 |
| operations_i | 39 | 0 | 0 |
| organization_domain_access | 5 | 0 | 0 |
| platform_concurrency / platform_failure / platform_security | 12 / 12 / 15 | 0 | 0 |
| profiles_h | 44 | 0 | 0 |
| query_plans | — | — | 1 (run separately, 06) |
| recording_g | 70 | 0 | 0 |
| recurrence_exceptions | 3 | 0 | 0 |
| **rollout_flags** (new) | 2 | 0 | 0 |
| rtc_concurrency / rtc_e2e / rtc_failure / rtc_outbox_realtime / rtc_security | 8 / 15 / 9 / 8 / 12 | 0 | 0 |
| security_v2 | 19 | 0 | 0 |
| **system_e2e** (new) | 17 | 0 | 0 |

Before Parts M–N the suite was 550 passed / 7 ignored (Part L report). Parts M–N add 64
passing tests (20 unit + 44 integration) and 8 ignored certification tests.

## Static analysis, format, contract

| Check | Command | Result |
|---|---|---|
| clippy | `cargo clippy --workspace --all-targets` | exit 0; **18 warnings, all pre-existing** (legacy handlers / migrations / `livekit.rs`; identical file set to the pre-Phase-2 baseline); **0 new** |
| rustfmt | `scripts/certification/check-fmt.sh` | 31 documented baseline files with pre-existing drift (`scripts/certification/fmt-baseline.txt`); **0 new drift** |
| OpenAPI | `cargo run -q --bin export_openapi` vs `packages/contracts/openapi/meeting-service.json` | **identical** (187 operations, 136 `/api/v2`) |
| v2 operation coverage | `scripts/certification/route_coverage.py --strict` | **136 / 136** operations with success + error path (6 912 recorded requests) — [route-coverage.md](route-coverage.md) |
| Migration up / reset / up | CI step; locally `m02_rollback_and_reapply` (index down → G–I down → F down → up → down 8 → up) | PASS |

## Certification runs (separately executed)

| Run | Command | Result |
|---|---|---|
| Real LiveKit + WebRTC + Egress + S3 | `scripts/livekit-certification/run.sh` (EGRESS_BIN / MINIO_BIN shims) | 07 / 08 |
| Media load | `CERT_FILTER=real_livekit_m_media_load MEDIA_LOAD_PUBLISHERS=P MEDIA_LOAD_SUBSCRIBERS=S run.sh` | 06 §43 |
| Control-plane load | `scripts/certification/run-load.sh` (burst, fanout, churn, scheduler, soak) | 06 |
| Migration certification (20 000 meetings) | `MIGRATION_CERT_MEETINGS=20000 cargo test --test migration_certification` | 03 — PASS |
| Query plans | `cargo test --test query_plans -- --ignored` | 06 §42 — PASS |

## Defects found and fixed during M / N (permanent regressions)

| Id | Severity | Summary | Regression test |
|---|---|---|---|
| SEC-M-01 | High (security) | Expired session could refresh / replay its media credential | `credential_replay::replay_expired_session`, `refreshed_credential_is_capped_by_session_expiry` |
| REL-M-01 | Reliability | Scheduler auto-end starvation by ineligible overrunning conferences | `failure_injection_m::m_scheduler_auto_end_not_starved_by_ineligible_overruns` (fails 3 / 3 on the old code, passes 3 / 3 with the fix) |
| PERF-M-01 | Performance | Main-room chat pagination sequential scan | `query_plans::query_plans_at_representative_volume` (+ migration `m20261005_000006`) |

No test was deleted, ignored, weakened or had its expected security result changed. Two
expectations changed because the schema legitimately gained a migration
(`db_invariants::i07` count 21 → 22; `migration_backfill::m02` now also certifies the new
index's down / up). One rollback-drill expectation was corrected from "recordings list →
`RECORDING_DISABLED`" to the implemented, documented behaviour (capture start / stop →
`RECORDING_DISABLED`, existing records stay readable to authorised users and denied to
outsiders — assertions added, none removed).

## CI disk safety (post-closure fix)

The first GitHub run of the fast gate on `staging` (run 37344646914, commit `2c2e637`) failed
with `No space left on device (os error 28)` while linking test binaries — a runner storage
failure, not a test failure. Cause: one job ran check, clippy, ~50 test binaries, build and
two `cargo run`s into one `target/` with full debug info (locally ≈ 23 GB including split
`.dwp` debug packages) on a runner with ≈ 14 GB free. Fix (`.github/workflows/backend.yml`,
`phase2-backend-certification.yml`): gates split across jobs (fresh disk each), CI builds
without debug info or incremental state (`CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`; debug assertions and overflow checks
unchanged), per-job cache keys, unused runner toolchains removed in the heaviest jobs, disk
telemetry at checkpoints, Docker images pruned only after the provider certification, and
certification images pinned by digest. Measured with the CI settings: check + clippy + all
test binaries + build = 5.5 GB. No gate was removed or relaxed.
