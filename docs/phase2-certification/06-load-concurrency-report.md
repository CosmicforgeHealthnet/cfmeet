# 06 — Load, concurrency and performance report (Part M §36–§45, Part N §79, §95)

## Environment and method

| Item | Value |
|---|---|
| Machine | single cloud container: 4 vCPU (Intel Xeon @ 2.80 GHz), 15 GiB RAM, Linux 6.18 |
| PostgreSQL / Redis | 16.14 / 7.0.15 (same host) |
| Service | production router, in-process, served over loopback TCP (`tests/load_certification.rs`); **production DB pool: 10 connections, 30 s acquire timeout** (`LOAD_DB_POOL`, mirrors `src/main.rs`); production outbox relay (Redis publisher + RTC control, 2 s poll) |
| RTC provider (control-plane load) | mock LiveKit RoomService (media load is separate, below) |
| Clients | same process (reqwest / tokio-tungstenite); process CPU therefore includes client work |
| Harness | `scripts/certification/run-load.sh`, all sizes environment-configurable; supports an external deployment (`LOAD_BASE_URL`) |
| Raw results | `load-*.json`, `media-load-*.json`, `query-plans.json` (CI artifact) |

Classification per §95: **PASS**, **PASS WITH OBSERVED LIMIT**, **FAIL**, **NOT EXECUTED**.
The figures below describe this machine only. They are **not** product capacity limits,
SLAs or guarantees; no level beyond those listed was run.

## §38 Burst join (simultaneous join authorization + credential issuance into ONE conference)

30 % registered members / 70 % guests, all released together by a barrier.

| Concurrent | p50 ms | p95 ms | p99 ms | max ms | joins/s | errors | pool peak | PG active peak | lock waits peak | deadlocks | RSS MB start→peak | Class |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 25 | 537 | 552 | 554 | 554 | 45.0 | 0 | 10/10 | 9 | 7 | 0 | 57→70 | PASS |
| 50 | 692 | 733 | 735 | 735 | 67.4 | 0 | 10/10 | 9 | 8 | 0 | 71→80 | PASS |
| 100 | 1 227 | 1 288 | 1 291 | 1 291 | 76.8 | 0 | 10/10 | 9 | 7 | 0 | 81→100 | PASS |
| 250 | 3 147 | 3 293 | 3 300 | 3 302 | 74.8 | 0 | 10/10 | 8 | 7 | 0 | 100→155 | PASS WITH OBSERVED LIMIT |
| 500 | 6 687 | 6 980 | 6 990 | 7 014 | 70.5 | 0 | 10/10 | 7 | 5 | 0 | 155→251 | PASS WITH OBSERVED LIMIT |

Invariants at every level: participants = sessions = successes; 0 duplicate users; 0
duplicate media identities; 0 wrong roles; every credential distinct and bound to its own
session identity (`sub = ps_<session>`); 0 deadlocks. Process CPU 123–170 % of 4 vCPUs.

**Observed limit / bottleneck**: joins into the same conference serialize (row locks held
for capacity / admission correctness inside the 10-connection pool), so throughput into
one conference plateaus at ~70–77 joins/s on this host and burst latency grows linearly
(~14 ms per queued join). No request failed. Raising the pool size or shortening the
locked section are hardening options (01-summary.md backlog); multi-conference
throughput was not measured.

## §39 WebSocket fanout (one conference, all clients subscribed)

Per round the host / a rotating guest generate: conference event (lock / unlock), chat,
hand raise, participant update (audio lock), role change, poll (create + open) — 10 rounds,
60 actions, 20 broadcast events. 5 % of clients are slow (read nothing until the end).
Latency = API response (commit) → frame received.

| Clients | Expected broadcast deliveries | Lost | Frames timed | p50 ms | p95 ms | p99 ms | slow-client p95 ms | disconnects | subscribe failures | RSS MB (before → subscribed → after → closed) | Redis MB | Class |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 25 | 500 | 0 | 1 800 | 1 095 | 1 975 | 2 057 | 9 035 | 0 | 0 | 71 → 73 → 75 → 75 | 1.2 | PASS WITH OBSERVED LIMIT |
| 50 | 1 000 | 0 | 3 600 | 1 364 | 2 021 | 2 110 | 8 430 | 0 | 0 | 84 → 86 → 87 → 87 | 1.2 | PASS WITH OBSERVED LIMIT |
| 100 | 2 000 | 0 | 7 125 | 1 561 | 2 207 | 2 320 | 8 348 | 0 | 0 | 88 → 91 → 93 → 93 | 1.2 | PASS WITH OBSERVED LIMIT |
| 250 | 5 000 | 0 | 17 850 | 1 500 | 2 361 | 2 595 | 8 549 | 0 | 0 | 93 → 98 → 103 → 103 | 1.2 | PASS WITH OBSERVED LIMIT |
| 500 | 10 000 | 0 | 35 625 | 1 250 | 2 163 | 2 411 | 17 255 | 0 | 0 | 103 → 110 → 119 → 118 | 1.2 | PASS WITH OBSERVED LIMIT |

**Observed limit**: delivery latency is dominated by the outbox relay poll interval (2 s in
`main.rs`), not by client count (p95 stays ≈ 2.0–2.4 s from 25 to 500 clients). Slow
clients receive everything (0 lost) when they read; their latency is their own read delay.
Memory grows ~16 KB per subscribed client and stays bounded; Redis memory flat. No action
errors, disconnects or resyncs.

## §40 Participant / waiting-room churn

20 workers × 60 s continuously: guest join → WAITING → host admit or deny → (admitted)
join + leave + rejoin + leave / (denied) re-request → WAITING → deny again; REST readers
poll the waiting room and participant list every 100 ms.

| Operation | n | p50 ms | p95 ms | p99 ms | max ms |
|---|---|---|---|---|---|
| join (→ WAITING) | 1 408 | 193 | 304 | 371 | 1 187 |
| admit | 702 | 93 | 157 | 202 | 1 088 |
| deny | 1 412 | 90 | 163 | 205 | 1 038 |
| admitted join (credential) | 702 | 335 | 454 | 535 | 1 321 |
| rejoin | 702 | 326 | 452 | 530 | 1 333 |
| leave | 1 404 | 51 | 106 | 134 | 303 |
| denied re-request | 706 | 215 | 318 | 381 | 1 169 |
| read waiting room | 176 | 91 | 149 | 194 | 964 |
| read participants | 176 | 136 | 202 | 223 | 274 |

1 408 cycles, 0 errors, 46 016 commits, 0 deadlocks, lock waits peak 8, pool 10/10.
Invariants: 0 stale WAITING entries, 0 duplicate guest identities, 0 participants both
admitted and denied, 0 denied participants with a live session, 0 live sessions after
leave, no denied participant ever received a credential. **PASS** (no lock starvation:
max latency 1.3 s under saturation of the 10-connection pool).

**Finding CAP-M-01 (fixed) — interactive capacity counted lifetime admissions.** The CI
certification run (GitHub runner, faster than this host: 2 201 cycles ≈ 1 100 distinct
admissions in 60 s) failed this scenario with `admit 409 MEETING_CAPACITY_REACHED`:
`ensure_capacity` counted every ADMITTED participant, and a participant stays ADMITTED
after leaving (that is the rejoin authorization), so once 1 000 distinct guests had ever
been admitted the room refused everyone while only a handful were present; the refused
guests stayed WAITING (stale-waiting invariant). Fixed semantics — **interactive capacity
= current logical interactive occupancy**: a participant occupies one seat while it is
ADMITTED and has ≥ 1 live ParticipantSession (PENDING / CONNECTED, not past
`expires_at`); multiple devices count once; the last live session ending (leave, expiry,
reconnect replacement, removal, ban, conference end) releases the seat; admission
authorization, admission / audit / attendance history and reconnect are unchanged. A
departed participant reconnecting takes a free seat again (refused only if the room is
genuinely full); every seat-taking path runs under the conference row lock. The dedicated
VIEWER pool (`viewerCapacity`) is unchanged. Regression suite `tests/capacity_occupancy.rs`
(A–K, incl. 1 000 distinct cycles through a 3-seat room and last-seat races).

The scenario now also reports occupancy, admissions, capacity refusals and deadlocks, and
fails on any capacity refusal while measured occupancy is below capacity, or any deadlock.

| Run (this host, 20 workers) | Cycles | Admissions | Max simultaneous occupancy | Capacity refusals | Stale waiting | Deadlocks | Class |
|---|---|---|---|---|---|---|---|
| 60 s, fixed code | 1 416 | 705 | 17 | 0 | 0 | 0 | PASS |
| 150 s, fixed code | 3 435 | 1 715 | 17 | 0 | 0 | 0 | PASS |
| 150 s, **previous code** (control) | 3 666 | 1 000 | 17 | **1 653** | **1 653** | 0 | **FAIL** |
| GitHub CI run 37416638738 (`3be8f81`, 2-vCPU runner, 60 s) | 1 102 | 546 | 19 | 0 | 0 | 0 | PASS |

**Final CI load certification (run 37416638738, commit `3be8f81`) — all PASS in one run:**
burst join 25–500 (0 errors; p99 9.0 s at 500 on the 2-vCPU runner), WebSocket fanout
25–500 (0 of 10 000 deliveries lost at 500; p95 3.0 s), waiting-room churn (above), scheduler,
600 s soak (717 cycles; RSS 60 → 69 MB, 0 active sessions / stuck recordings at the end),
migration certification (3 000 meetings), query plans 22 / 22 INDEX. Not capacity claims.

## §41 Scheduler load

300 recurring series (3 000 occurrences) with materialization bookkeeping wiped (all due
at once), 200 LIVE v2 conferences past their scheduled end, 200 legacy meetings due for
auto-start / auto-end; **4 scheduler instances** race, each draining until idle.

| Result | Value |
|---|---|
| Occurrences after race | 3 000 (0 duplicates) |
| v2 auto-ended | 200 / 200, exactly one terminate command each |
| Legacy auto-start / auto-end | 100 / 100 and 100 / 100; 0 duplicate start / end logs |
| Tick latency | p50 1 516 ms, p95 2 037 ms, p99 2 072 ms (24 ticks) |
| Deadlocks | 0 (120 rollbacks = lost conditional-update races, expected) |
| Class | **PASS** (after fix REL-M-01) |

The first run of this scenario surfaced REL-M-01 (scheduler auto-end starvation — fixed,
regression test added; 10-failure-recovery-report.md). **Observed limit**: instances work
the same bounded batch (no partitioning), so adding scheduler instances adds safety, not
throughput.

## §42 Database query plans (realistic volume)

`tests/query_plans.rs` at 20 001 spaces, 45 001 conferences, 402 000 participants,
401 900 sessions, 1 200 000 outbox events, 300 000 messages (100 000 in one hot
conference with 2 000 participants), 60 000 memberships, 40 000 invitations, 10 119 polls,
13 417 artefacts, 4 000 recurrence rules. `EXPLAIN (ANALYZE, BUFFERS)`:

| Query (shape as issued by the code) | Index | Exec ms |
|---|---|---|
| participant lookup (registered) | `uq_participant_registered_user` | 0.04 |
| participant lookup (scoped id) | `uq_participant_scope` | 0.03 |
| participant list (admitted) | `idx_participants_conference_state` | 0.99 |
| session lookup (active by participant) | `idx_sessions_participant_state` | 0.05 |
| session lookup (provider identity) | `uq_session_media_identity` | 0.04 |
| active sessions of a conference | `idx_sessions_conference_active` | 0.91 |
| membership lookup | `uq_meeting_member_active` | 0.10 |
| invitation lookup | `meeting_invitations_token_hash_key` | 0.04 |
| waiting room | `idx_participants_waiting` | 0.14 |
| event replay | `idx_outbox_conference_replay` | 0.47 |
| current sequence | `idx_outbox_conference_replay` (index-only) | 0.04 |
| main-room message pagination (newest / deep cursor) | **`idx_messages_main_cursor` (new)** | 0.05 / 0.13 |
| polls / Q&A / artefacts | `idx_polls_conference` / `idx_questions_conference` / `idx_artifact_conference_type` | 0.04 / 0.14 / 0.03 |
| scheduler due auto-end | `uq_conference_one_live_per_space` | 0.03 |
| scheduler due auto-start | `idx_conference_scheduler_start` | 0.59 |
| recurrence due materialization | `idx_recurrence_materialization` | 0.61 |
| outbox relay claim (`FOR UPDATE SKIP LOCKED`) | `idx_outbox_pending` | 0.07 |
| resolve by meeting code | `meeting_spaces_meeting_code_key` | 0.05 |
| occurrences by schedule | `idx_conference_space_start` | 0.11 |

**Finding (fixed)**: main-room chat pagination sequentially scanned `conference_messages`
(37.9 ms / 26.1 ms at this volume, growing linearly) because PostgreSQL cannot use
`idx_messages_room_cursor` for `breakout_room_id IS NULL … ORDER BY created_at DESC`. One
justified additive partial index (`m20261005_000006_message_main_cursor_index`) fixes it
(0.05 ms). No other index was added. Class: **PASS** (all 22 critical queries indexed;
the test fails on any sequential scan of a large table).

**Finding QP-M-02 (test defect, fixed) — unrepresentative Q&A seed.** The CI run failed
on `Q&A lookup` (`SELECT * FROM meeting_questions WHERE conference_id = $hot AND status =
'OPEN' ORDER BY created_at`). With the new per-query diagnostics the plan was `Seq Scan on
meeting_questions` returning 471 of 471 rows (0 removed by filter): the seed put every
question into the hot conference, so the predicate selected 100 % of a few-page table and
a sequential scan was PostgreSQL's correct choice (index vs. heap cost near-tied — which is
why it passed here earlier and failed on CI). The intended index
`idx_questions_conference (conference_id, status, created_at)` already existed — **not an
index defect; no index added**. The seed now carries Q&A history like polls / chat /
artefacts (≈ 60 000 questions across ≈ 10 000 historical conferences); the plan becomes
`Bitmap Index Scan using idx_questions_conference` (465 rows, 0.31 ms) and the
no-sequential-scan gate is kept unchanged (22 / 22 INDEX).

Diagnostics: every query now prints a `QUERY_PLAN` line (expected index, indexes used,
seq-scanned tables with their row counts, rows returned, execution time); a failing query
prints its SQL and the full rendered plan (`QUERY_PLAN_FAIL …`: node, relation / index,
estimated vs. actual rows, filter / index condition); the console filter in
`run-load.sh` no longer truncates these or the panic message.

## §43 Self-hosted LiveKit media load (real SFU) — separate from control-plane load

`livekit_certification::real_livekit_m_media_load_and_large_audience`: real
`livekit-server` 1.8.4 on the same 4-vCPU host; real WebRTC clients (`rtcprobe`, pion)
**also on the same host** (one process each). TOWN_HALL conference: P presenters on a
capped stage publish Opus audio + VP8 video; S VIEWER attendees subscribe only. Hold 45 s
(30 s for 4×16).

| Publishers × subscribers | Join failures | Connect p50 / p95 ms | Disconnects | Subscriptions | Everyone got every stage track | RTP packets | RTP gaps | LiveKit CPU avg | LiveKit peak RSS | Media delivery | Class |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 4 × 16 | 0 | 37 / 80 | 0 | 152 | 20 / 20 | 243 321 | 0 % | 36 % | 137 MB | CLEAN | PASS |
| 4 × 50 | 0 | 44 / 59 | 0 | 424 | 54 / 54 | 961 181 | 0 % | 83 % | 249 MB | CLEAN | PASS |
| 6 × 50 | 0 | 39 / 60 | 0 | 660 | 56 / 56 | 1 487 504 | 0 % | 78 % | 303 MB | CLEAN | PASS |
| 4 × 75 | 0 | 45 / 74 | 0 | 624 | 79 / 79 | 1 281 030 | 5.6 % | 74 % | 552 MB | OBSERVED LIMIT | PASS WITH OBSERVED LIMIT |
| 6 × 75 | 0 | 40 / 52 | 0 | 960 | 81 / 81 | 1 718 490 | 18.4 % | 82 % | 404 MB | OBSERVED LIMIT | PASS WITH OBSERVED LIMIT |
| 8 × 100 | 0 | 38 / 58 | 0 | 1 712 | 108 / 108 | 1 022 238 | 70.5 % | 88 % | 882 MB | OBSERVED LIMIT | PASS WITH OBSERVED LIMIT |
| 8 × 150 | 0 | 44 / 61 | 0 | 2 512 | 158 / 158 | 777 152 | 84.2 % | 88 % | 1 021 MB | OBSERVED LIMIT | PASS WITH OBSERVED LIMIT |

(CPU % = share of one core.) Control plane held at every size: 0 join failures, 0
disconnects, every subscriber received every stage track, stage capacity enforced
(`STAGE_CAPACITY_REACHED` for one extra promotion), audience publish denied
(`publish_denied` on a modified attendee client), viewer pool enforced
(`MEETING_CAPACITY_REACHED` beyond `viewerCapacity`).

**Highest clean envelope on this host: 6 publishers × 50 subscribers (56 participants).**
From ~79 participants upward RTP gaps appear because the SFU and up to 158 client
processes share 4 vCPUs; client-side starvation and SFU saturation cannot be separated on
this host. An earlier 8 × 100 attempt with the default space capacity hit
`MEETING_CAPACITY_REACHED` for 8 joins (correct enforcement of the 100-participant default)
and was re-run using the viewer pool. This does **not** certify any production media
capacity; that requires a dedicated SFU host and separate client machines (staging).

## §44 Large-audience certification

The architecture principle "many viewers ≠ many interactive publishers" is certified by
the same runs: small controlled stage (`maxStageParticipants` / `maxPresenters` = P),
subscribe-only VIEWER audience in a separate viewer pool (`viewerCapacity`), promotion
under load succeeds up to the stage cap and is refused beyond it, demotion removes publish
(Part H real-SFU test). Largest audience exercised: 150 subscribers (real SFU) and 500
realtime subscribers (control plane). **PASS** within those sizes.

## §45 Resource-leak soak

600 s of continuous full meeting lifecycles (create + start, 8 guest joins, 4 WebSocket
subscribers, 8 chat messages, 2 breakouts opened and closed, recording start / stop,
leaves, end) with the production relay running: **543 cycles**, 0 errors.

| Indicator | First sample (t = 6 s) | Last sample (t = 597 s) | Trend |
|---|---|---|---|
| Process RSS | 67 MB | 74 MB | plateau 71–74 MB after the first minute |
| Orphan ParticipantSessions (active in non-LIVE conferences) | 0 | 0 | none |
| LIVE / ENDING conferences left behind | 0 | 0 | none |
| Expired unclaimed worker leases | 0 | 0 | none |
| Outbox PENDING | 48 | 12 | oscillates 0–78 with the 2 s relay poll; drained to 0 repeatedly — no growth |
| Open breakouts in ended conferences | 0 | 0 | none |
| Mock provider rooms | 2 | 0 | rooms terminated as conferences end |
| Recordings stuck (REQUESTED / RECORDING / STOPPING > 10 min) | 0 | 0 | none |
| PostgreSQL connections | 10 | 10 | bounded by the pool |

Class: **PASS** for a 10-minute soak on this host. A multi-hour soak was not run
(**NOT EXECUTED**; staging item).

## Concurrency suites (permanent)

`concurrency_v2` (C01–C09), `rtc_concurrency` (C1–C8), `collab_concurrency`,
`platform_concurrency`, `profiles_h` H-C01..C08, `operations_i` I-C01..C06,
`invariants_certification::inv_admitted_and_denied_are_exclusive` (5 rounds × 10 racing
admit / deny) — all PASS (04-test-report.md).
