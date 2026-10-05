# 10 — Failure injection and recovery report (Part M §29–§35, Part N §73, §80)

Environment: local certification container (PostgreSQL 16.14, Redis 7.0.15). Failures are
injected for real: PostgreSQL through a short-acquire-timeout pool pointed at an
unreachable / exhausted database, Redis through a TCP toggling proxy
(`tests/common/proxy.rs`) or an unreachable address, LiveKit / Egress through fault flags
of the mock RoomService (`fail_all`, `fail_create`, `fail_delete`, `fail_remove`,
`fail_update`, `delay_ms`, `fail_egress`, `reject_egress`, `egress_stop_noop`), object
storage through the in-memory store's failure switches, workers by abandoning a tick
mid-way and expiring leases.

## §29 PostgreSQL

| Requirement | Test(s) | Result |
|---|---|---|
| No state-changing request claims success during outage; 503 `SERVICE_UNAVAILABLE` | `platform_failure::l_f01_postgres_unavailable`, `rtc_failure::f1_postgres_down`, `collab_failure::f10_database_failure`, `profiles_h::h_f02_token_validation_database_failure` | PASS |
| No RTC credential, no recording start, no invitation consumption, no role grant | same + `l_f12_mixed_dependency_failure_no_false_success` | PASS |
| Safe retries converge after recovery | same (retry after recovery succeeds exactly once) | PASS |

## §30 Redis

| Requirement | Test(s) | Result |
|---|---|---|
| PostgreSQL authority continues; durable outbox persists; REST usable | `rtc_failure::f2_redis_down`, `platform_failure::l_f02_redis_unavailable`, `collab_failure::f05_redis_down_durable_state`, `recording_g::g_f09_*` | PASS |
| WebSocket degrades (explicit `degraded` frame, PostgreSQL tailing) | `system_e2e::r_e2e_redis_outage_and_recovery`, `collab_failure::f04_redis_down_reaction_and_chat` | PASS |
| Security never fails open | `appointment_revocation::rev01_strict_profiles_fail_closed_when_revocation_unverifiable`, `operations_i::i_s10_sink_outage_does_not_bypass_authorization` | PASS |
| Outbox / fanout converge after recovery | `r_e2e_redis_outage_and_recovery` | PASS |

## §31 LiveKit

| Provider unavailable during | Test(s) | Behaviour | Result |
|---|---|---|---|
| join | `rtc_failure::f3_livekit_down_on_join`, `platform_failure::l_f03_*` | `RTC_PROVIDER_UNAVAILABLE`, no fake credential | PASS |
| room creation | `rtc_failure::f4_room_creation_failure` | start fails honestly / retried; no LIVE without a room | PASS |
| permission update | `collab_failure::f02_permission_update_failure` | business state commits; durable command retries; grants re-asserted | PASS |
| remove | `failure_injection_m::m_f_livekit_unavailable_during_remove` (new in M) | REMOVED commits; `providerEnforcement` ≠ ENFORCED; credential refused; command retained (0 FAILED) and converges after recovery | PASS |
| screen-share stop | `collab_failure::f03_stop_share_failure` | retried until confirmed | PASS |
| conference end | `rtc_failure::f5_termination_failure`, `platform_failure::l_f04_*` | stays ENDING with `PENDING_PROVIDER_CONTROL`; ENDED only after confirmed absence | PASS |
| mute | `collab_failure::f01_livekit_mute_failure` | honest failure | PASS |
| breakout routing | `collab_failure::f06_breakout_routing_provider_down` | durable reroute command | PASS |
| no insecure fallback | `rtc_failure::f9_no_insecure_fallback` | — | PASS |

## §32 Object storage

| During | Test(s) | Result |
|---|---|---|
| recording finalization | `recording_g::g_f05_object_storage_unavailable`, `g_f06_provider_complete_but_object_missing` — never READY without the verified object | PASS |
| artefact access | `g_f07_signed_access_failure` | PASS |
| deletion / retention cleanup | `g_f08_retention_delete_failure`, `g_f11_retention_worker_crash_restart` — never DELETED while the object remains | PASS |

## §33 Recording provider

| Failure | Test(s) | Result |
|---|---|---|
| start / stop / timeout | `recording_g::g_f01`, `g_f02`, `g_f03`, `g_e2e_16_provider_failure_safe`, `platform_failure::l_f06_recording_provider_failure_isolated` (conference stays usable) | PASS |
| finalization / lost webhook | `g_f04_completion_callback_lost`, `g_e2e_25_reconciliation_after_lost_callback` | PASS |
| duplicate webhook | `g_s09_duplicate_recording_callback_deduped`, `g_c02`, `g_e2e_15_callback_replay_deduped` | PASS |

## §34 Workers (crash / restart; lease expiry / reclaim; no duplicate terminal operation)

| Worker | Test(s) | Result |
|---|---|---|
| scheduler | `platform_failure::l_f08_scheduler_crash_recovery`, `rtc_concurrency`, `load_scheduler` (4 instances racing, see 06), **new regression** `failure_injection_m::m_scheduler_auto_end_not_starved_by_ineligible_overruns` | PASS (after fix REL-M-01) |
| auto-end (v2 scheduled end) | `platform_concurrency::l_c07_two_auto_end_workers`, `load_scheduler` | PASS |
| legacy auto-start / auto-end | `api_v2_roles::r11_scheduler_auto_end_and_outbox_relay`, `load_scheduler` (instances racing on due legacy meetings: no duplicate start / end logs) | PASS |
| outbox relay | `rtc_failure::f7_outbox_worker_crash`, `collab_failure::f08_outbox_relay_crash`, `recording_g::g_f10_*`, `rtc_concurrency::c8_parallel_outbox_workers` | PASS |
| conference reconciler | `rtc_failure::f8_reconciler_crash`, `platform_failure::l_f09_*`, `operations_i::i_f06_*`, `rtc_concurrency::c6_two_reconcilers` | PASS |
| breakout closer | `collab_failure::f07_countdown_worker_restart` | PASS |
| artefact finalizer / retention | `platform_failure::l_f10_artifact_worker_crash_recovery`, `recording_g::g_f11_*` | PASS |

### REL-M-01 (reliability defect, fixed) — scheduler auto-end starvation

- **Found by**: the Part M scheduler load run (`load_certification::load_scheduler`) — not a
  duplicate, but an incomplete batch led to inspection of the candidate query.
- **Defect**: the v2 auto-end query selected `LIVE` conferences past `scheduled_end_at`
  with `LIMIT 50` and only then applied each space's policy (`autoEnd`, grace) in Rust. 50
  or more overrunning conferences that must keep running (auto-end disabled, or within a
  long grace period) occupied every batch, so due conferences were never auto-ended.
- **Fix**: eligibility (`autoEnd`, `autoEndGraceMinutes` with the `SchedulePolicy` defaults)
  is evaluated in SQL and candidates are ordered by `scheduled_end_at`
  (`src/workers/conference_scheduler.rs`, `AUTO_END_DUE`); the Rust-side check remains.
- **Proof**: `m_scheduler_auto_end_not_starved_by_ineligible_overruns` (60 ineligible
  overruns + 1 due conference) fails on the previous code and passes with the fix
  (04-test-report.md); the full regression and `load_scheduler` were re-run.

## §35 Network / WebSocket

| Case | Test(s) | Result |
|---|---|---|
| socket drop / reconnect / cursor replay | `system_e2e::r_e2e_websocket_replay_and_resync`, `rtc_e2e::e13_realtime_reconnect_recovery`, `collab_e2e::f25_realtime_recovery`, `platform_failure::l_f07_*` | PASS |
| slow consumer | `failure_injection_m::m_f_ws_slow_consumer_isolated_and_gap_free` (300 events; fast consumer unaffected; slow consumer later receives the identical gap-free stream); load: 500 clients incl. 25 slow (06) | PASS |
| cursor too old (beyond window / purged history) | `failure_injection_m::m_f_ws_cursor_too_old_requires_resync` (socket and REST replay) | PASS |
| canonical refetch | same + `r_e2e_websocket_replay_and_resync` | PASS |
| provider media continues while WebSocket down | `r_e2e_websocket_replay_and_resync` (session stays CONNECTED) | PASS |

## Provider reconciliation evidence (§80)

| Drift | Real `livekit-server` evidence | Mock / system evidence | Action |
|---|---|---|---|
| Lost room | `real_livekit_reconciliation`: room terminated out of band → `LIVE_ROOM_MISSING` → re-created (`CERT reconcile LIVE_ROOM_MISSING: PASS`) | `r_e2e_callback_loss_room_loss_orphans` | repair |
| Orphan room (ended conference) | — | `rtc_e2e::e9_termination_failure_and_reconciliation` | repair (terminate) |
| Orphan room (no conference) | — | `rtc_e2e::e14_provider_drift_reconciliation` | escalate (`RTC_DRIFT_ESCALATED`) |
| Orphan participant | `real_livekit_m_waiting_subscription_ban` (banned identity reconnecting → evicted, absent at provider) | `r_e2e_callback_loss_room_loss_orphans`, `operations_i::i_e2e_09_*` | repair (evict) |
| Stale CONNECTED session | `real_livekit_part_i_presence_and_mapping` (disconnect state) | `operations_i::i_e2e_10_stale_connected_session_reconciled` | repair |
| Expired session | — | `credential_replay::replay_expired_session` (`SESSION_EXPIRED`) | repair (expire + evict) |
| Permission drift | `real_livekit_full_lifecycle` / Part F (grants follow PostgreSQL) | `rtc_e2e::e14_*` | repair (push grants) |
| Breakout drift | — | `collab_failure::f06_*`, `collab_security::s10_stale_breakout_credential` | repair (reroute / evict) |
| Recording drift | Part G real Egress | `recording_g::g_e2e_25_reconciliation_after_lost_callback` | repair (finalize / fail) |
| Two reconcilers | — | `operations_i::i_e2e_15_two_reconcilers_no_duplicate_destructive_repair`, `rtc_concurrency::c6` | idempotent |
