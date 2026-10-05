# 01 — Phase 2 backend certification and closure: summary (Parts M–N)

Branch `staging`. Service `services/meeting-service` (Rust / Axum / SeaORM modular monolith).
Architecture unchanged: PostgreSQL is the authority, LiveKit enforces, object storage holds
bytes, Redis is support only, the outbox carries events, `ActorContext` is the identity
authority and `MeetingPermissionService` the permission authority.

## Outcome

| Area | Result |
|---|---|
| Full regression | **614 passed, 0 failed, 15 ignored** (the ignored are the separately-run certification suites + 1 pre-existing doc-test) |
| v2 API coverage | **136 / 136** operations have a success-path and an authorization / error-path test (measured at runtime) |
| OpenAPI | regenerated, byte-identical (187 operations / 136 v2) |
| clippy / rustfmt | 18 pre-existing warnings, **0 new**; 31-file documented fmt baseline, **0 new drift** (gate now blocking) |
| System E2E | M-E2E-01..11 + 6 recovery scenarios: **17 / 17 PASS** |
| Security | M-S1..15 PASS; IDOR matrix 61 / 61 rejected; credential replay 8 / 8; **1 high finding fixed (SEC-M-01); 0 critical / 0 high unresolved** |
| Migration | 20 000-meeting synthetic legacy dataset: 0 mapping failures, 20 000 / 20 000 links, 0 duplicates under 3 concurrent runners, partial resume clean, idempotent re-run, 0 / 200 shadow mismatches (**synthetic, not production**) |
| Failure injection | PostgreSQL, Redis, LiveKit (join, room creation, permission update, remove, screen-share stop, end), storage, recording provider, workers, WebSocket (drop, slow consumer, cursor too old): PASS; **1 reliability defect fixed (REL-M-01)** |
| Load (local, single 4-vCPU host) | burst join 25–500 with 0 errors (one conference: ~70–77 joins/s, p99 7.0 s at 500 → observed limit); WebSocket fanout 25–500 clients, 0 lost of 10 000 deliveries (p95 ≈ 2.2 s, bounded by the 2 s relay poll); churn 1 408 cycles 0 errors; scheduler 4 instances 0 duplicates; 10-min soak no leaks; query plans all indexed after **1 justified index (PERF-M-01)** |
| Real LiveKit (1.8.4) | 7 / 8 real tests PASS in the final run, incl. new real subscription + ban / reconnect and media load (clean envelope on this host: 6 publishers × 50 subscribers) |
| Real Egress + S3 | S3: PASS (final run). Egress capture: PASS in the J–L run; **final M / N re-run failed for an environmental reason** (Egress headless Chrome could not load its template after a sandbox restart) — **not re-certified** (08) |
| Flags / rollback | 12-combination flag matrix and 10-step rollback drill PASS (local) |
| Staging | **STAGING DEPLOYMENT CERTIFICATION — PENDING ENVIRONMENT** |
| Production | **PRODUCTION CANARY — NOT EXECUTED**; no production deployment; legacy-write cutover not enabled |

## Defects found and fixed on `staging` (each with a permanent regression)

1. **SEC-M-01 (High, security)** — an expired-but-still-active ParticipantSession could
   refresh / replay its media credential. Fixed in credential issuance (refusal + TTL cap),
   provider-join authority and the reconciler (`SESSION_EXPIRED`). (09)
2. **REL-M-01 (reliability)** — scheduler auto-end could be starved by ≥ 50 overrunning
   conferences that must not be auto-ended; eligibility moved into SQL. Deterministic
   regression fails 3 / 3 on the old code. (10)
3. **PERF-M-01 (performance)** — main-room chat pagination sequentially scanned
   `conference_messages`; additive partial index `idx_messages_main_cursor`
   (migration `m20261005_000006`): 37.9 ms → 0.05 ms at 300 000 messages. (06)

## Genuine remaining backend gaps

- Real Egress capture not re-certified after the sandbox restart (08); must be re-run in the
  manual CI workflow or staging.
- Staging certification (11), production canary and legacy-write retirement (12) not
  executed — they require an environment / explicit authorization.
- CI workflows were changed but have not yet executed on GitHub (they run on the next
  push / manual dispatch).

## Non-blocking hardening backlog

- Realtime delivery latency is bounded by the 2 s outbox relay poll (p95 ≈ 2.2 s); a
  LISTEN / NOTIFY or shorter poll would lower it.
- Single-conference join throughput (~70–77 joins/s on this host) is bounded by per-conference
  serialization in a 10-connection pool (hardcoded in `src/main.rs`); make the pool size
  configurable and shorten the locked section.
- Scheduler instances share the same bounded batch (safety, not throughput) — partition with
  `SKIP LOCKED` if scheduling volume grows.
- Multi-hour soak and production-scale media load on dedicated hosts (staging).

## Decision

**IMPLEMENTATION / CERTIFICATION READY — LOCAL**, with the real-Egress re-certification
outstanding. **NOT** "production rollout completed": staging certification is pending an
environment, and the production canary, legacy-write retirement, frontend migration and any
further phase require explicit instruction.
