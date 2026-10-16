# 13 — Rollback plan, rollback certification and drill (Part N §54–§55)

## Principle

Rollback is **routing + feature flags + deployment version**, never destructive database
reversal. Phase 2 schema is additive (legacy `meetings` / `participants` / `session_logs`
are read, never altered or dropped); v2 rows stay in PostgreSQL when features are turned
off and are picked up again on re-enable. Flags are read at startup, so every rollback
step is a configuration change plus restart / redeploy.

## Rollback levers (narrowest first)

| Lever | Effect | Use when |
|---|---|---|
| Disable one capability flag (`FEATURE_COLLABORATION_V2_ENABLED`, `FEATURE_RECORDING_V2_ENABLED`, `FEATURE_TRANSCRIPTION_V2_ENABLED`, `FEATURE_INTELLIGENCE_V2_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_HEALTHCARE_PROFILE_ENABLED`, `FEATURE_QUALITY_TELEMETRY_ENABLED`) | That surface answers `FEATURE_DISABLED` / `RECORDING_DISABLED`; records untouched | One capability unhealthy |
| `FEATURE_CONFERENCE_REALTIME_ENABLED=false` | Realtime endpoints disabled; events keep accumulating durably for replay | Realtime fanout misbehaving |
| `FEATURE_WAITING_ROOM_V2_ENABLED=false` | Management API disabled; existing waiting policies still enforced (fails closed — nobody admitted by fallback) | Waiting-room management defect |
| Narrow `MEETING_V2_WRITE_ALLOWLIST` | Stops new v2 writes outside the cohort | Canary regression |
| `FEATURE_MEETING_V2_ENABLED=false` (routing rollback) | All `/api/v2` meeting routes `FEATURE_DISABLED`; `/api/v1` unaffected | Systemic v2 problem |
| Redeploy previous version | Previous binary; additive schema stays compatible | Code regression |
| `FEATURE_RTC_PROVIDER_CONTROL_V2=false` | v2 joins issue **no** credential (`PROVIDER_CONTROL_DISABLED`). There is **no** fallback to legacy client-trusted authorization | Only together with routing rollback; never as a way to "unblock" joins |

Not a lever: dropping v2 tables, deleting legacy tables, or re-enabling legacy
client-trusted RTC authorization (all prohibited).

## Rollback certification (§54)

| Requirement | Evidence | Result |
|---|---|---|
| Schema remains additive | `migration_backfill::m02_rollback_and_reapply` (down/up), `db_invariants::i07_migrations_idempotent`, CI migration up / reset / up, `migration_certification` (legacy rows read-only) | PASS |
| Legacy resolver operational | `migration_backfill::k_e2e_01_02_05_*`, `migration_certification` (25/25 resolved exact + case-insensitive at 3 000 and 20 000 meetings) | PASS |
| v1 reads remain | `legacy_v1_web_compat`, `legacy_v1_join`, `migration_certification` (25/25 legacy reads after migration; inactive owners denied) | PASS |
| v2 can be disabled safely | `rollout_flags::n_rollback_drill` (v2 off → `FEATURE_DISABLED`, `/health` OK, records intact), `n_flag_combination_matrix` | PASS |
| RTC control-plane failure does not fall back to insecure authorization | `rtc_failure::f9_no_insecure_fallback`, drill (provider control off → `PROVIDER_CONTROL_DISABLED`, no credential) | PASS |
| New create / join can be disabled | v2 off / allowlist (`api_v2_roles`, flag matrix) | PASS |
| Provider rooms reconciled / cleaned after rollback | drill step 8 (reconciler non-destructive on valid state), `system_e2e::r_e2e_callback_loss_room_loss_orphans`, `rtc_e2e::e9_termination_failure_and_reconciliation`, `rtc_e2e::e14_provider_drift_reconciliation` | PASS |
| Recording jobs not orphaned | drill (Egress completion delivered while recording disabled → READY after re-enable, object verified) | PASS |
| Outbox remains consistent | drill (after re-enable: 0 FAILED, 0 PENDING after relay ticks) | PASS |

## Rollback drill (§55) — executed, non-production

Test: `services/meeting-service/tests/rollout_flags.rs::n_rollback_drill`. Environment: the
local certification container (PostgreSQL 16.14, Redis 7.0.15, mock LiveKit RoomService /
Egress, in-memory object store) — **not** staging, not production. The service is
"redeployed" with different flags over the **same** database, provider and object store
(`TestApp::reconfigure`).

| Step | Action | Outcome |
|---|---|---|
| 1 | Enable the full v2 feature set (Parts A–I) | Service up |
| 2 | Create a LIVE meeting; member connected; owner connected; a guest WAITING | Sessions CONNECTED at the provider |
| 3 | Exercise collaboration (chat message, OPEN poll) and recording (RECORDING via Egress) | Committed |
| 4 | Simulate an unhealthy v2 feature set | — |
| 5 | Redeploy with collaboration, recording, transcription, realtime and waiting-room management disabled (provider control kept on) | Restarted |
| 6 | Existing records valid | Participant / message / open-poll / artefact counts identical; conference LIVE; both sessions CONNECTED; polls `FEATURE_DISABLED`; recording start / stop `RECORDING_DISABLED`; recordings list still readable to the owner (1 item) and denied to an outsider |
| 7 | No unsafe fallback | Admit answers `FEATURE_DISABLED`; a new guest still lands in WAITING with no credential |
| 7b | Provider evidence while disabled | Egress completion callback accepted (200) |
| 8 | Reconcile provider state | No orphan / missing-room drift; room and member still present at the provider |
| 8b | Deeper rollback: provider control off | Join → `PROVIDER_CONTROL_DISABLED`, no credential |
| 8c | Routing rollback: v2 off | `/api/v2` → `FEATURE_DISABLED`; `/health` 200 |
| 9 | Re-enable everything | — |
| 10 | Canonical state intact | Conference LIVE; poll still OPEN; waiting guest admitted (200); recording READY with verified object; outbox: 0 FAILED, 0 PENDING |

Result: **PASS** (local). A staging drill is required before the production canary
(rollout gate; see 12-rollout-plan.md) and is **NOT EXECUTED** (PENDING ENVIRONMENT).

## Production rollback runbook (prepared, NOT EXECUTED)

1. Declare the incident; note the triggering gate / metric.
2. Apply the narrowest lever above (config change + rolling restart).
3. Verify: `/health` OK; error codes on disabled surfaces are the stable `FEATURE_DISABLED`
   family; `cfmeet_rtc_provider_errors_total` and join failures stop rising.
4. Let workers converge: outbox PENDING → 0; reconciler drift repaired; no ENDING older
   than its window.
5. Record the outcome; re-enable only after the root cause is fixed and the stage's gates
   hold again in the lower environment.
