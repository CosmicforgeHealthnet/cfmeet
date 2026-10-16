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
| Real Egress + S3 | **PASS — re-certified in GitHub CI** (run 37384968304: 8 / 8 real-provider tests, real Egress recording → READY, real S3 / MinIO, artefact lifecycle) after the earlier local M / N environment failure (08) |
| Flags / rollback | 12-combination flag matrix and 10-step rollback drill PASS (local) |
| Staging | **PASS (2026-10-06)** — isolated staging deployment on a restored production copy: migration 14 → 22, backfill 12 / 12 and 37 / 37, functional validation 35 / 35, rollback compatibility 15 / 15 (16). Load, real media, recording and 24 h observation were not run there |
| Production | **MIGRATED, PRE-CUTOVER PASS (2026-10-06)** — 22 migrations, backfill 12 / 12 and 37 / 37 with 0 failures, Phase 2 backend running on `127.0.0.1:18081`, pre-cutover certification 18 / 18 (16). Public `api.cfmeet.com` is still routed to the legacy backend until the frontend compatibility deployment is confirmed; legacy-write cutover not enabled |

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

4. **CAP-M-01 (correctness, load certification)** — interactive capacity counted every
   participant ever ADMITTED (ADMITTED persists after leaving as the rejoin authorization),
   so a room that had admitted 1 000 distinct participants over its lifetime refused new
   entrants while nearly empty (surfaced by the CI churn run). Interactive capacity is now
   **current logical occupancy**: ADMITTED + ≥ 1 live session, multiple devices count once,
   the last live session ending releases the seat; admission history / reconnect unchanged,
   race-safe under the conference row lock. Regression suite `capacity_occupancy` (A–K). (06)
5. **QP-M-02 (test defect)** — the query-plan seed put every Q&A row into one conference,
   so the Q&A lookup selected 100 % of a tiny table and a sequential scan was the correct
   plan; the seed now carries representative Q&A history and the existing
   `idx_questions_conference` is used. No index added. Plan-failure diagnostics added. (06)

## Genuine remaining backend gaps

- None in the backend implementation. The public Nginx cutover waits for the frontend
  compatibility deployment (16) — a deployment dependency. The staged canary and
  legacy-write retirement of 12 and the staging load / media / observation items of 11
  remain separate operational decisions.
- The dedicated VIEWER audience pool (`viewerCapacity`) still counts the admitted audience
  (deliberately unchanged in the CAP-M-01 fix); whether it should also become current
  occupancy is a product decision.

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

**IMPLEMENTATION / CERTIFICATION READY**: the complete Phase 2 certification workflow is green
in GitHub CI (run 37416638738, commit `3be8f81`): full suite 622 / 0 / 15 + 136 / 136 v2
coverage, OpenAPI parity + migration up / reset / up, real LiveKit / WebRTC / Egress / S3
(8 / 8) + media load, and the full control-plane load suite (burst, fanout, churn,
scheduler, 600 s soak, migration, query plans) — 04, 06, 08.

**Update 2026-10-06 — PHASE 2 BACKEND — COMPLETE.** The backend was deployed to an isolated
staging environment, the production database was migrated (22 migrations) and the Phase 2
backend passed the production pre-cutover certification (16). Public traffic is still served
by the legacy backend until the frontend compatibility deployment is confirmed: a cutover
dependency, not unfinished backend implementation. Legacy-write retirement, the frontend
migration to `/api/v2` and any further phase still require explicit instruction.
