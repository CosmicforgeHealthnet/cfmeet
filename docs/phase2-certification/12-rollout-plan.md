# 12 — Feature flags, rollout plan, gates, canary, legacy-write retirement (Part N §52–§53, §85–§89)

Status vocabulary: **DONE** = actually performed with evidence in this package;
**NOT EXECUTED** = prepared only. Nothing beyond step 1 of the rollout sequence has been
performed.

## 1. Feature flag audit (§52)

All flags are read once at startup by `FeatureFlags::from_env()`
(`services/meeting-service/src/config/features.rs`). **Every flag change requires a
process restart / redeploy.** Accepted truthy values: `1`, `true`, `yes`, `on`
(case-insensitive); anything else, or unset, is the default. Every Phase 2 flag defaults
to **off** (unit test `config::features::tests::every_phase2_flag_defaults_off`). No
duplicate flags exist.

| Env name | Default | Scope | Enabling does | Depends on | Safe rollback effect (turn off) |
|---|---|---|---|---|---|
| `FEATURE_MEETING_V2_ENABLED` | off | process | Serves `/api/v2` meeting domain; starts the conference scheduler | — | Every `/api/v2` meeting route answers `FEATURE_DISABLED`; `/api/v1` and `/health` unaffected; v2 rows stay in PostgreSQL untouched |
| `MEETING_V2_WRITE_ALLOWLIST` | unset (= all authenticated users) | process | Restricts v2 **writes** to listed user UUIDs (comma separated; malformed entries ignored, all-malformed = no allowlist) | meeting v2 | Removing the list widens access; adding it narrows writes immediately after restart |
| `FEATURE_MEETING_V2_SHADOW_READ` | off | process | Legacy v1 meeting reads also compare legacy ↔ v2 state (logs + `cfmeet_ops_shadow_*`); no client effect | — | Comparison stops |
| `FEATURE_WAITING_ROOM_V2_ENABLED` | off | process | Allows non-`OFF` waiting-room policies on spaces and the waiting-room management API (list / admit / deny / return) | meeting v2 | **Fails closed**: existing waiting-room policies are still evaluated (joiners still wait), but admit / deny / list answer `FEATURE_DISABLED`; nobody is admitted by fallback (`rollout_flags::n_rollback_drill`) |
| `FEATURE_RTC_PROVIDER_CONTROL_V2` | off | process | v2 join issues provider (LiveKit) credentials; outbox relay executes provider commands; reconciler; provider webhooks applied | meeting v2 | v2 join returns `rtcStatus=PROVIDER_CONTROL_DISABLED` and **no credential** — never a legacy credential fallback |
| `FEATURE_CONFERENCE_REALTIME_ENABLED` | off | process | WebSocket stream / ticket / replay endpoints; relay publishes REALTIME events to Redis | meeting v2 | Realtime endpoints `FEATURE_DISABLED`; events keep accumulating durably in `outbox_events` for replay after re-enable |
| `FEATURE_COLLABORATION_V2_ENABLED` | off | process | Chat, reactions, hand raise, breakouts, polls, Q&A, media moderation, pre-join | meeting v2, rtc control (media), realtime (live delivery) | Collaboration routes `FEATURE_DISABLED`; stored messages / polls remain intact (drill re-enables with poll still OPEN) |
| `FEATURE_RECORDING_V2_ENABLED` | off | process | Recording start / stop (Egress), consent gates, artefact worker | rtc control, object storage config | Start / stop answer `RECORDING_DISABLED`; existing artefacts stay readable to authorised users; provider completion callbacks are still accepted, so in-flight recordings are **not orphaned** (drill: completion while disabled → READY after re-enable) |
| `FEATURE_TRANSCRIPTION_V2_ENABLED` | off | process | Transcription / captions provider path (external processing) | recording consent model | Transcription routes disabled |
| `FEATURE_INTELLIGENCE_V2_ENABLED` | off | process | AI processing of artefacts (summaries etc.) | transcription | Disabled; healthcare policy blocks external intelligence regardless |
| `FEATURE_WEBINAR_MODE_ENABLED` | off | process | `WEBINAR` / `TOWN_HALL` space types, registrations, backstage, stage capacity | meeting v2 | Creating webinar spaces / registrations / backstage answers `FEATURE_DISABLED` |
| `FEATURE_HEALTHCARE_PROFILE_ENABLED` | off | process | `HEALTHCARE` type / security profile / appointment access policy | meeting v2 (+ appointment adapter configured) | Creating healthcare spaces answers `FEATURE_DISABLED`; the adapter remains fail-closed |
| `FEATURE_QUALITY_TELEMETRY_ENABLED` | off | process | Client quality-sample ingestion | meeting v2 | Samples rejected; provider-side metrics continue |
| `FEATURE_LEGACY_MEETING_WRITE_DISABLED` | off | process | Every state-changing `/api/v1` meeting / participant / chat / third-party route answers `410 LEGACY_WRITE_DISABLED`; legacy reads and `/api/v2` unaffected | v2 consumers migrated | Legacy writes resume |

## 2. Flag combination testing (§53)

`tests/rollout_flags.rs::n_flag_combination_matrix` boots the service with each
combination over one database and probes create / join / legacy write. Observed (this run):

| Combination | v2 meeting | v2 join credential | Legacy v1 write |
|---|---|---|---|
| all new features off | `FEATURE_DISABLED` | — | 201 |
| core meeting v2 only | works | `PROVIDER_CONTROL_DISABLED` (no credential) | 201 |
| v2 + realtime | works | `PROVIDER_CONTROL_DISABLED` | 201 |
| v2 + waiting room | works | `PROVIDER_CONTROL_DISABLED` | 201 |
| v2 + RTC control | works | `ISSUED` | 201 |
| v2 + collaboration | works | `ISSUED` | 201 |
| v2 + recording | works | `ISSUED` | 201 |
| v2 + webinar | works | `PROVIDER_CONTROL_DISABLED` | 201 |
| v2 + healthcare | works | `PROVIDER_CONTROL_DISABLED` | 201 |
| all A–I enabled | works | `ISSUED` | 201 |
| all A–I + legacy writes disabled | works | `ISSUED` | **410** `LEGACY_WRITE_DISABLED` |
| v2 off + legacy writes enabled | `FEATURE_DISABLED` | — | 201 |

No combination produced an insecure fallback (no credential without provider control,
no admission without the waiting-room API, no legacy authority on v2 routes).

## 3. Staged rollout sequence (§85)

| # | Stage | Flags | Status |
|---|---|---|---|
| 1 | Local / CI certification | all A–I in tests | **DONE** (this package; local container — see 11-staging-certification.md) |
| 2 | Shared development environment | all A–I, allowlist = engineering | NOT EXECUTED |
| 3 | Staging with migrated production-like data + real LiveKit / Egress / S3 | all A–I; `SHADOW_READ` on; legacy writes enabled | NOT EXECUTED — **PENDING ENVIRONMENT** |
| 4 | Internal CosmicForge meetings | `MEETING_V2` + `RTC_PROVIDER_CONTROL_V2` + `REALTIME` + `WAITING_ROOM`; allowlist = internal staff | NOT EXECUTED |
| 5 | Small allowlisted production cohort (canary) | as 4 + collaboration; allowlist = cohort | NOT EXECUTED — **PRODUCTION CANARY — NOT EXECUTED** |
| 6 | Gradual tenant / percentage rollout | widen allowlist; enable recording / webinar / healthcare per tenant readiness | NOT EXECUTED |
| 7 | General availability after metrics remain healthy | remove allowlist | NOT EXECUTED |
| 8 | Legacy-write deprecation after documented observation period | `FEATURE_LEGACY_MEETING_WRITE_DISABLED` per §6 below | NOT EXECUTED |

Note: the allowlist gates v2 **writes** per user; percentage rollout across tenants is
achieved by widening the allowlist or by routing (the service has no percentage flag, and
none is added here).

## 4. Rollout gates (§86)

A stage advances only when every gate holds for the observation window. Values not
approved by product / operations are **APPROVAL REQUIRED**; certification-measured values
are shown for context only and are not SLAs.

| Gate | Threshold | Evidence source |
|---|---|---|
| Backend test suite | 0 failures (hard) | `cargo test --workspace --no-fail-fast` (04-test-report.md) |
| Security | 0 critical / 0 high unresolved (hard) | 09-security-review.md |
| Real provider certification | all `CERT … PASS` (hard) | 07, 08 |
| Rollback drill | passed in the target environment (hard) | 13-rollback-plan.md |
| Migration shadow mismatch rate | **APPROVAL REQUIRED** (certified synthetic: 0 / 200 sampled) | `cfmeet_ops_shadow_mismatches_total / shadow_reads_total` |
| Join success rate (denials excluded) | **APPROVAL REQUIRED** | `meeting_join_success_total / meeting_join_attempts_total` |
| Join p95 latency | **APPROVAL REQUIRED** (local single-conference burst: p95 ≈ 0.55 s at 25 simultaneous, ≈ 7 s at 500 simultaneous — see 06) | `meeting_join_latency_seconds` |
| Realtime delivery p95 | **APPROVAL REQUIRED** (local: ≈ 2.0–2.4 s, bounded by the 2 s relay poll) | load harness / client telemetry |
| Provider drift | **APPROVAL REQUIRED** (escalations: 0 expected) | `cfmeet_rtc_drift_*` |
| Recording failure rate | **APPROVAL REQUIRED** | `cfmeet_ops_recording_failed_total / recording_requested_total` |
| Data-loss incidents | 0 unexplained (hard) | incident log |

## 5. Production canary (§87) — PRODUCTION CANARY — NOT EXECUTED

Prepared only; no production deployment was made.

- **Configuration**: production deploy of this `staging` build with
  `FEATURE_MEETING_V2_ENABLED=true`, `FEATURE_RTC_PROVIDER_CONTROL_V2=true`,
  `FEATURE_CONFERENCE_REALTIME_ENABLED=true`, `FEATURE_WAITING_ROOM_V2_ENABLED=true`,
  `FEATURE_MEETING_V2_SHADOW_READ=true`, `MEETING_V2_WRITE_ALLOWLIST=<cohort user ids>`,
  legacy writes **enabled**; collaboration / recording / webinar / healthcare enabled only
  after the core canary holds.
- **Allowlist strategy**: start with internal staff accounts, then named pilot users;
  v2 reads stay open (v2 writes are what the allowlist controls).
- **Metrics watched**: join success / failure by outcome, join latency, provider errors,
  drift escalations, outbox backlog, WebSocket resync rate, shadow mismatches.
- **Rollback trigger**: any hard gate breached, or an APPROVAL REQUIRED threshold breached
  for its window, or an unexplained data-integrity incident → execute 13-rollback-plan.md.
- **Runbook**: 14-operational-runbook.md.

## 6. Legacy write retirement (§88)

`FEATURE_LEGACY_MEETING_WRITE_DISABLED` must **not** be enabled in general production: the
web frontend (`apps/web`) still uses only `/api/v1` meeting routes (verified in Part K).

Required sequence (none executed):
1. Migrate frontend / API consumers to `/api/v2` (a separate, not-started phase).
2. Observe shadow comparisons in production (`SHADOW_READ`) for the agreed period.
3. Run the production v2 cohort (stage 5–6).
4. Confirm no required legacy writer remains (`cfmeet_ops_legacy_writes_rejected_total`
   in a test environment with the flag on; v1 write traffic trending to zero in production).
5. Enable the flag for an allowlisted environment / cohort; observe.
6. Expand gradually.
7. Only later remove legacy write code. Legacy tables are **not** deleted in Phase 2.

## 7. Third-party / infrastructure position (§89)

| Dependency | Phase 2 position |
|---|---|
| LiveKit | Self-hosted baseline (`livekit-server`, certified with v1.8.4) |
| LiveKit Cloud | Not required |
| SIP / telecom carrier | Not required |
| SMS | Not required |
| Object storage (S3-compatible) | Required when recording is enabled (certified against MinIO) |
| LiveKit Egress | Required when recording is enabled (certified with v1.14.1) |
| Calendar providers | Optional |
| Transcription / AI commercial provider | Not mandatory (provider-neutral interfaces; disabled by default) |
| Redis | Required for fanout / rate limiting; not an authority |
| PostgreSQL | Required; the authority |
| Docker | Deployment / container packaging only; local development and tests use plain PostgreSQL / Redis (no Docker Compose) |
