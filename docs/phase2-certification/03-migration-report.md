# 03 — Migration certification report (Part M §27–§28, Part N §71, §76)

**Classification: DEVELOPMENT CERTIFICATION — PRODUCTION-LIKE SYNTHETIC DATASET.**
No production or sanitized production data was available or used; this is **not** a
production migration certification. The production migration must be re-run and
re-reported on the real (or sanitized) dataset in staging (11-staging-certification.md).

- Test: `services/meeting-service/tests/migration_certification.rs`
  (`migration_certification_synthetic_dataset`; default 3 000 meetings in the regular
  suite, `MIGRATION_CERT_MEETINGS=20000` for this report).
- Backfill code under test: `migration/src/phase2_backfill.rs` (shared by the
  `m20261003_000002` migration, the compatibility resolver and the admin endpoint).
- Environment: PostgreSQL 16.14, local 4-vCPU container.
- Raw JSON: `migration-report.json` produced in `$CFMEET_CERT_OUT` (CI artifact
  `certification-load`).

## Dataset (synthetic, deterministic)

| Category | Rows |
|---|---|
| Users | 4 000 (2 % inactive) |
| Legacy meetings | 20 000 — scheduled 4 000, ongoing 1 000, ended 12 000, cancelled 3 000 |
| Public / private | 14 000 / 6 000 |
| Instant (start = creation, no end) / scheduled window | 7 918 / 12 082 |
| Participant rows | 75 424 — registered 56 424, guests 19 000 |
| Rejoin rows (same user, second row) | 5 424 |
| Waiting rows (ongoing meetings) | 500 |
| Kicked rows | 11 824 |
| Guests sharing the display name "Guest" | yes (never merged) |
| Session logs | 22 154 (1 in 13 ended meetings has none) |
| Deliberately inconsistent meetings (`end_time <= start_time`) | 206 |
| Legacy codes / links | 20 000 distinct `meeting_identifier`s |
| Recurring / passcode / organization meetings, space memberships | **NOT APPLICABLE** — the legacy schema (`meetings`, `participants`, `session_logs`) has no representation for them; they exist only as Phase 2 v2 features |

## Procedure

1. Legacy schema only (14 pre-Phase-2 migrations); seed the dataset.
2. Apply the Phase 2 schema migration only.
3. **Partial run**: backfill the first 5 000 meetings one by one.
4. **Interruption**: for 2 500 of them delete the migrated participants and sessions
   (simulated crash mid-way).
5. **Concurrent resume**: the real migration (`Migrator::up`, which runs the backfill
   migration and all later schema) races **two** additional full backfill runners
   (advisory-lock serialized).
6. Verify all mappings; 7. idempotent re-run; 8. legacy reads, compatibility resolver and
   shadow comparison through the running service.

## Results (20 000 meetings)

| Item | Result |
|---|---|
| Input row counts | see dataset table |
| MeetingSpaces created | 20 000 (1 per legacy meeting; `mapping_failures` = 0, unmapped = 0) |
| ConferenceSessions created | 20 000 (origin `LEGACY`, `occurrence_key = legacy:<id>`) |
| Members mapped | 20 000 owners (`host_id → owner_user_id`); legacy has no membership table |
| Participants mapped | 75 424 / 75 424 legacy rows → 69 992 logical ConferenceParticipants (rejoins of a registered user collapse into one; guests are never merged) |
| Role / admission mapping | 50 992 registered logical participants checked against the documented rule (latest legacy row decides; host of the meeting → OWNER, other host → HOST, viewer → VIEWER, else PARTICIPANT; joined → ADMITTED, waiting → WAITING, kicked → REMOVED): **0 mismatches**; 12 992 OWNER |
| Participant sessions mapped | 74 924 historical ParticipantSessions = every non-waiting legacy row |
| Legacy ids mapped | 20 000 (`meeting_spaces.legacy_meeting_id`, `conference_sessions.legacy_meeting_id`, `legacy_participant_id`) |
| Legacy links preserved | 20 000 / 20 000 (`meeting_code = meeting_identifier`) |
| Status mapping | scheduled → CREATED 4 000; ongoing → LIVE 1 000; ended → ENDED 12 000; cancelled → CANCELLED 3 000 |
| Privacy mapping | private → `AUTHENTICATED` + waiting room `EVERYONE`; public → `PUBLIC`: 20 000 / 20 000 |
| Skipped records | 500 waiting rows get no ParticipantSession (they never joined) but are mapped as WAITING participants |
| Invalid records | 206 meetings with `end_time <= start_time` |
| Repairable exceptions | 206 → `scheduled_end_at` NULL (window dropped, meeting kept); 923 ended meetings without session logs → `started_at` NULL |
| Manual-review exceptions | 390 meetings owned by inactive accounts (mapped; owner cannot act until reactivated) |
| Duplicate prevention | 3 concurrent runners: 0 duplicate spaces, 0 duplicate conferences, 0 duplicate sessions |
| Partial-migration resume | 5 000 partially migrated, 2 500 interrupted → resumed clean (`is_clean`) |
| Idempotent re-run | inserted 0 / 0 / 0 / 0, refreshed 0 |
| Legacy reads after migration (`GET /api/v1/meetings/{id}` as owner) | 25 / 25 sampled OK; 5 / 5 inactive owners correctly denied |
| Compatibility resolver (`GET /api/v2/resolve/{code}` exact + lower-case) | 25 / 25 |
| Shadow comparison (legacy ↔ v2) | 200 sampled, **0 mismatches** |
| Timings | seed 3.0 s; partial run 98.7 s (5 000 single-meeting runs); concurrent resume 13.7 s; idempotent re-run 3.3 s |

The same certification at 3 000 meetings runs in the regular suite (CI) and passed.

## Additional migration evidence (permanent tests)

| Area | Test |
|---|---|
| Rollback / forward (down → up) | `migration_backfill::m02_rollback_and_reapply`; CI "Migration smoke (up / reset / up)" |
| Idempotent schema migrations | `db_invariants::i07_migrations_idempotent` |
| Legacy-managed conferences stay legacy-managed; legacy delete reflected | `migration_backfill::m03`, `m04` |
| Resolver code / participant mapping stability | `k_e2e_01_02_05_*` |
| Missing / inconsistent legacy metadata | `k_backfill_handles_missing_and_inconsistent_legacy_metadata` |
| Controlled v2 writes canonical; compat reads follow | `k_e2e_06_07_v2_writes_are_canonical_and_compat_reads_follow` |
| Shadow detects injected mismatches; v2 edits never silently overwritten | `k_e2e_08_shadow_comparison_detects_injected_mismatch` |
| Legacy writes default on; cannot overwrite v2 state; disable keeps reads + v2 | `legacy_retirement::*` |

## Backend migration capability vs production legacy-write retirement (§71)

- **Backend migration capability: PASS** (mapping, links, compatibility, idempotency,
  resume, concurrency — on synthetic data).
- **Production legacy-write retirement: NOT STARTED.** The current web frontend uses
  `/api/v1` meeting writes; `FEATURE_LEGACY_MEETING_WRITE_DISABLED` must stay off in
  production until consumers migrate (12-rollout-plan.md §6).

## Schema change made in Part M

`m20261005_000006_message_main_cursor_index` — one additive partial index justified by the
query-plan certification (06-load-concurrency-report.md §query plans). No other schema
change; no table dropped or altered.
