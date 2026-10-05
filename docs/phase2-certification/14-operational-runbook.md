# 14 — Operational runbook, dashboards and alerts (Part N §82–§84)

Scope: `services/meeting-service` (Phase 2 meeting domain). Authority model that every
procedure below respects:

- **PostgreSQL is the authority.** Never "fix" state by editing the RTC provider or Redis
  to match a belief; fix PostgreSQL through the API / workers, then let enforcement follow.
- **LiveKit enforces** what PostgreSQL decided (rooms, grants, removals, termination).
- **Object storage holds bytes**; `meeting_artifacts` holds the lifecycle.
- **Redis is support only** (fanout, rate-limit counters). Losing it degrades, never authorizes.
- **The outbox carries events** (`outbox_events`, channels `REALTIME` and `PROVIDER_COMMAND`).

Operator interfaces (all internal; never exposed through the public ingress):

| Interface | Purpose |
|---|---|
| `GET /internal/metrics` (header `x-internal-token: $INTERNAL_API_TOKEN`) | Prometheus exposition: `cfmeet_rtc_*_total`, `cfmeet_collab_*_total`, `cfmeet_ops_*_total`, `meeting_join_*`, `rtc_*` quality series |
| `GET /internal/rtc/metrics` | RTC control-plane counters (JSON) |
| `GET /internal/operations/conferences/health?state=UNHEALTHY` | Active conference health (no PII) |
| `GET /internal/operations/conferences/{id}/health` | One conference: sessions, drift, quality, provider node/region |
| `GET /internal/operations/incidents` | Open quality / provider incidents |
| `GET /api/v2/admin/compat/legacy-meetings/{id}/shadow` (platform admin) | Legacy ↔ v2 shadow comparison |
| `POST /api/v2/admin/compat/backfill` (platform admin) | Idempotent legacy → v2 backfill (re)run |
| `rtc_drift_events`, `worker_leases`, `outbox_events`, `conference_audit_events` tables | Read-only diagnosis |

Without `INTERNAL_API_TOKEN` set, every `/internal/*` operations route answers 404 (fail closed).

---

## JOIN FAILURES

**Identify** — `meeting_join_failure_total{outcome}` splits failures by outcome; denials are
excluded from the join SLI by design. Map the outcome / client error code:

| Symptom (error code) | Class | Meaning / action |
|---|---|---|
| `MEETING_ACCESS_DENIED`, `PERMISSION_DENIED`, `MEETING_PASSCODE_REQUIRED`, `INVITATION_*`, `PARTICIPANT_BANNED` | access denial | Correct behaviour. Check the space's `access_policy` / invitation state; never relax policy to "fix" a join. |
| `RATE_LIMITED` (+ `Retry-After`) | abuse control | Passcode brute-force limiter or collaboration limits. Wait for the window. |
| `MEETING_CAPACITY_REACHED` | capacity | `interactive_capacity` / `viewer_capacity` of the space reached. Raise capacity on the space (owner/host) or use the viewer pool for large audiences. |
| `SERVICE_UNAVAILABLE` (503), pool acquire timeouts in logs | database failure | PostgreSQL down or pool exhausted (production pool = 10 connections, 30 s acquire timeout, `src/main.rs`). No join claims success while PostgreSQL is unavailable. |
| `RTC_PROVIDER_UNAVAILABLE` (503), `rtcStatus=PROVIDER_UNAVAILABLE` | RTC provider failure | LiveKit unreachable; `cfmeet_rtc_provider_errors_total` rising. See LIVEKIT OUTAGE. |
| client timeout with no server error | network failure / timeout | Check ingress latency and `meeting_join_latency_seconds`; compare with `meeting_join_media_latency_seconds` (API OK, media path failing → network/TURN). |

`meeting_join_attempts_total` vs `meeting_join_success_total` gives the success rate;
`meeting_join_media_connected_total` confirms the provider actually connected the session.

## LIVEKIT OUTAGE

**Safe behaviour (certified: `rtc_failure::f3_livekit_down_on_join`, `collab_failure::*`,
`failure_injection_m::m_f_livekit_unavailable_during_remove`, `platform_failure::l_f03/l_f04`):**

- Joins answer `RTC_PROVIDER_UNAVAILABLE` — no fake credential, no insecure fallback.
- Removal / ban / role change / lock / end still **commit in PostgreSQL** (business state wins);
  the response reports `providerEnforcement: PENDING_PROVIDER_CONTROL` and the durable
  `PROVIDER_COMMAND` outbox rows retry until the provider is back.
- Ending a conference leaves it `ENDING`; the reconciler terminates the room and confirms
  absence before `ENDED`.

**Response** — do not disable `FEATURE_RTC_PROVIDER_CONTROL_V2` to "unblock" joins: with it
off, v2 joins issue **no** media credential (`PROVIDER_CONTROL_DISABLED`); there is no
fallback to legacy client-side authorization. If new meetings must be stopped, disable
creation by routing / `FEATURE_MEETING_V2_ENABLED=false` (restart required).

**Recovery** — once LiveKit is reachable: outbox relay drains provider commands
(`cfmeet_rtc_outbox_published_total` rises, `outbox_events` PENDING `PROVIDER_COMMAND` → 0);
reconciler repairs `LIVE_ROOM_MISSING`, evicts stale identities and re-pushes grants
(`cfmeet_rtc_drift_repaired_total`). Verify with
`SELECT count(*) FROM outbox_events WHERE channel='PROVIDER_COMMAND' AND status='PENDING'`.

## STUCK ENDING CONFERENCE

**Inspect** — `SELECT id, status, updated_at FROM conference_sessions WHERE status='ENDING' AND updated_at < now() - interval '5 minutes'`;
then `GET /internal/operations/conferences/{id}/health` and `rtc_drift_events` for that id.

**Reconciler behaviour** — ENDING + room exists → terminate → confirm absence → `ENDED`
(repair). Provider unreachable → stays ENDING (never marked ENDED without confirmed absence).

**Safe operator action** — repeat `POST /api/v2/conferences/{id}/end` (idempotent; retries
termination). Never set `status='ENDED'` by SQL: that would leave a live provider room.

## ORPHAN RTC ROOM

**Detection** — reconciler compares provider `cf-*` rooms with conferences. Room of an
ENDED / CANCELLED / FAILED conference → terminated automatically (repair). A `cf-*` room
with **no** conference at all → `RTC_DRIFT_ESCALATED` (never guessed).

**Manual action allowed** only for escalated rooms after confirming in PostgreSQL that no
conference / breakout / backstage maps to the name (`conference_sessions.rtc_room_name`,
`backstage_room_name`, `breakout_rooms.rtc_room_name`). Then delete the room with the
LiveKit CLI / RoomService.

## ORPHAN PARTICIPANT

**Detection** — provider participant without an active `participant_sessions` row, or a
session past `expires_at`, or a removed / banned participant's identity.

**Safe eviction** — automatic: reconciler evicts (repair, `cfmeet_rtc_stale_sessions_evicted_total`);
a webhook `participant_joined` for an identity without authority returns
`STALE_SESSION_EVICTED` and removes it. Operators should not remove identities whose
session is valid in PostgreSQL.

## REDIS OUTAGE

**Expected degradation** (certified: `rtc_failure::f2_redis_down`, `collab_failure::f04/f05`,
`system_e2e::r_e2e_redis_outage_and_recovery`): REST and PostgreSQL authority unaffected;
WebSocket subscribers receive `{"type":"degraded","reason":"REALTIME_FANOUT_UNAVAILABLE"}` and
tail PostgreSQL (1 s poll); transient reactions are dropped (`cfmeet_collab_reactions_dropped_total`);
rate limiting falls back to per-process counters. Security never fails open.

**Recovery** — automatic: the relay resumes Redis publishing; clients resume from their
cursor (gap-free sequences) or are told `resync_required` and refetch canonical state.

## OUTBOX BACKLOG

**Metrics** — `cfmeet_rtc_outbox_published_total`, `cfmeet_rtc_outbox_retried_total`,
`cfmeet_rtc_outbox_dead_lettered_total`, `cfmeet_rtc_realtime_publish_failures_total`.

**Diagnosis**:
```sql
SELECT channel, status, count(*), min(available_at) FROM outbox_events
 WHERE status <> 'PUBLISHED' GROUP BY 1, 2;
SELECT event_type, attempts, last_error FROM outbox_events
 WHERE status = 'FAILED' ORDER BY created_at DESC LIMIT 20;
```
Rising PENDING with flat `published` → relay not running (check the process / logs
"outbox relay tick failed"). FAILED rows are dead letters (`Dead` results such as
"participant is not part of this conference") — inspect, do not blindly re-queue.

**Worker restart** — the relay is stateless; restarting the service resumes from
PostgreSQL. Rows are claimed with `FOR UPDATE SKIP LOCKED`, so several instances are safe
(certified `rtc_concurrency::c8_parallel_outbox_workers`).

## RECORDING FAILURE

1. **Provider state** — `meeting_artifacts.provider_ref` (egress id); compare with the
   Egress service (`ListEgress`).
2. **Artifact state** — `SELECT status, failure_code, failure_detail, updated_at FROM meeting_artifacts WHERE conference_id=...`.
   `FAILED` is terminal for that capture; the conference stays usable (certified
   `recording_g::g_f01..g_f04`, `platform_failure::l_f06`).
3. **Object storage** — confirm the bucket is reachable and the object at `storage_key`
   exists; READY is only set after size + SHA-256 verification.
4. **Retry** — start a new recording (`POST /api/v2/conferences/{id}/recordings`); never
   flip a FAILED artefact back by SQL.

## ARTIFACT STUCK PROCESSING

**Diagnosis** — `SELECT id, status, updated_at FROM meeting_artifacts WHERE status IN ('STOPPING','PROCESSING') AND updated_at < now() - interval '30 minutes'`.
Metrics: `cfmeet_ops_artifact_finalize_retries_total`, `cfmeet_ops_artifact_integrity_failures_total`,
`cfmeet_ops_artifact_missing_total`.

**Storage integrity** — the finalizer verifies object existence, size and checksum; a
missing object (`g_f06_provider_complete_but_object_missing`) ends `FAILED`, never `READY`.

**Finalizer** — the artefact worker retries with backoff under a worker lease; a lost
completion callback is recovered by reconciliation against the provider
(`g_e2e_25_reconciliation_after_lost_callback`).

## WORKER FAILURE

All workers (scheduler, reconciler, outbox relay, breakout closer, artefact finalizer /
retention) are restart-safe: leases in `worker_leases` expire and are reclaimed; every
terminal operation is a conditional update, so overlap produces no duplicate effect
(certified `platform_failure::l_f08..l_f10`, `rtc_concurrency::c6`, `load_scheduler`).
Check `SELECT name, holder, expires_at FROM worker_leases` — an expired lease is normal and
is reclaimed on the next tick.

## QUALITY INCIDENT

`GET /internal/operations/conferences/health?state=UNHEALTHY` → per-conference health
(join failure rate, reconnects, RTT / jitter / loss buckets) and `providerNode` /
`providerRegion` from provider callbacks. Correlate several unhealthy conferences on the
same `providerNode` → SFU node issue; a single participant's poor network does not raise
an incident (bounded labels, no per-participant alerting).

## MIGRATION MISMATCH

**Shadow metrics** — `cfmeet_ops_shadow_reads_total`, `cfmeet_ops_shadow_mismatches_total`,
`cfmeet_ops_shadow_failures_total` (with `FEATURE_MEETING_V2_SHADOW_READ=true`).

**Diagnosis** — `GET /api/v2/admin/compat/legacy-meetings/{legacyId}/shadow` lists diffs.
Legacy-managed drift (admission state, role, access policy) is repaired by an idempotent
`POST /api/v2/admin/compat/backfill`; a v2-edited value is **reported, never silently
overwritten** (`k_e2e_08_shadow_comparison_detects_injected_mismatch`).

**Safe rollback / escalation** — keep legacy writes enabled (they are the source for
legacy-origin rows); escalate persistent mismatches to engineering with the shadow output.
No destructive rollback of v2 tables.

---

## Dashboards (§83)

The repository has no dashboard-as-code; the existing stack is Prometheus exposition at
`/internal/metrics`. Panels (PromQL; `rate` windows are suggestions):

| Panel | Query |
|---|---|
| Join success rate | `sum(rate(meeting_join_success_total[5m])) / sum(rate(meeting_join_attempts_total[5m]))` |
| Join failure rate by outcome | `sum by (outcome) (rate(meeting_join_failure_total[5m]))` |
| Join latency p50/p95 | `histogram_quantile(0.95, sum by (le) (rate(meeting_join_latency_seconds_bucket[5m])))` |
| Join → media connected latency | `histogram_quantile(0.95, sum by (le) (rate(meeting_join_media_latency_seconds_bucket[5m])))` |
| Active / unhealthy conferences | `/internal/operations/conferences/health` (JSON; scrape via exporter or SQL `count(*) WHERE status='LIVE'`) |
| Participant reconnects | `rate(rtc_reconnects_total[5m])` |
| RTC provider failures | `rate(cfmeet_rtc_provider_errors_total[5m])`, `rate(cfmeet_rtc_room_create_failures_total[5m])`, `rate(cfmeet_rtc_room_terminate_failures_total[5m])` |
| Provider drift | `rate(cfmeet_rtc_drift_detected_total[15m])`, `rate(cfmeet_rtc_drift_repaired_total[15m])`, `increase(cfmeet_rtc_drift_escalated_total[1h])`, `sum by (kind) (rate(rtc_provider_drift_total[15m]))` |
| Outbox backlog | SQL `count(*) WHERE status='PENDING' AND available_at < now() - interval '1 minute'`; `rate(cfmeet_rtc_outbox_retried_total[5m])`, `increase(cfmeet_rtc_outbox_dead_lettered_total[1h])` |
| Reconciler activity | `rate(cfmeet_rtc_reconciler_runs_total[15m])` |
| Recording failures | `increase(cfmeet_ops_recording_failed_total[1h])`, `rate(cfmeet_ops_recording_provider_errors_total[15m])` |
| Recording processing latency | SQL `percentile_cont(0.95) WITHIN GROUP (ORDER BY finalized_at - ended_at)` over READY artefacts of the last day |
| Transcription failures | `increase(cfmeet_ops_transcription_failed_total[1h])` |
| WebSocket | `cfmeet_rtc_ws_connections_total`, `rate(cfmeet_rtc_ws_rejected_total[5m])`, `rate(cfmeet_rtc_ws_resync_required_total[5m])` |
| Worker failures | log-based ("tick failed") + `worker_leases` expired > 2 poll periods |
| Legacy retirement | `rate(cfmeet_ops_legacy_writes_rejected_total[5m])`, `cfmeet_ops_shadow_mismatches_total` |

## Alerts and severity (§84)

Thresholds marked **APPROVAL REQUIRED** must be set by product / operations; the values
shown are starting points for discussion, not approved SLAs.

| Severity | Condition | Starting point |
|---|---|---|
| **CRITICAL** | Widespread join failure: join success rate (denials excluded) below target | **APPROVAL REQUIRED** (e.g. < 90 % for 5 min) |
| **CRITICAL** | Database unavailable: `/health` failing or 503 `SERVICE_UNAVAILABLE` sustained | 2 min |
| **CRITICAL** | RTC provider unavailable for new joins: `RTC_PROVIDER_UNAVAILABLE` outcomes dominate | **APPROVAL REQUIRED** |
| **CRITICAL** | Large stuck ENDING backlog: ENDING older than 10 min | **APPROVAL REQUIRED** (count) |
| **CRITICAL** | Persistent outbox failure: PENDING older than 5 min growing, or dead letters increasing | 10 min |
| **CRITICAL** (only where recording is business-critical) | Systemic recording / storage failure: `recording_failed` / `recording_requested` ratio | **APPROVAL REQUIRED** |
| **WARNING** | Join p95 latency above target | **APPROVAL REQUIRED** |
| **WARNING** | Drift escalations (`cfmeet_rtc_drift_escalated_total` increase) | any in 1 h |
| **WARNING** | Shadow mismatches during migration observation | **APPROVAL REQUIRED** |
| **WARNING** | WebSocket resync rate spike / Redis degraded | 15 min |
| **WARNING** | Artefacts stuck STOPPING / PROCESSING > 30 min | any |
| **INFO** | Reconciler repairs (`drift_repaired`), legacy writes rejected (once retirement starts), worker lease takeovers | informational |

Individual participant network problems (high RTT / loss for one session) are **not**
alerted; they surface in per-conference health only.
