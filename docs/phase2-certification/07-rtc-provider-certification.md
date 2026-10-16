# 07 — Real RTC provider certification (Part M §7, §43–§44, Part N §80)

**Classification: LOCAL REAL-PROVIDER CERTIFICATION** — real self-hosted `livekit-server`
1.8.4 (single node, loopback, certification keys from
`scripts/livekit-certification/livekit.yaml`) and real WebRTC participants (`rtcprobe`, pion,
built from `scripts/livekit-certification/rtcprobe`) driving the production
`LiveKitProvider`. Not staging, not production. Mock-provider suites are not counted here.

Command: `scripts/livekit-certification/run.sh` (all 8 `#[ignore]` tests in
`tests/livekit_certification.rs`). Final M/N run (2026-10-05, after the last code change):
**7 / 8 tests PASS**; the eighth (Part G, real Egress capture) is reported in 08.

| Test | Result (final run) | Certified |
|---|---|---|
| `real_livekit_full_lifecycle` | PASS | `CERT room create+lookup` (RM_…), `credential + media join` (real published track TR_…), `webhook verification + session sync`, `participant listing`, `mute_track`, `update_permissions` (provider grants follow VIEWER), `VIEWER cannot publish`, `remove_participant` (real disconnect), `terminate_room + ENDED` (room absent); `room_finished` applied once |
| `real_livekit_reconciliation` | PASS | room terminated out of band → `LIVE_ROOM_MISSING` repaired (room re-created) |
| `real_livekit_part_f_media_controls` | PASS | host audio mute, audio / video permission reduction (live camera unpublished), permission restore, screen-share policy enforcement, real `track_published` screen-share event, host stop screen share |
| `real_livekit_part_h_stage_and_backstage` | PASS | VIEWER publish denial, promotion to PRESENTER grants publish, demotion removes publish, backstage in a separate provider room (isolation), demotion evicts from backstage |
| `real_livekit_part_i_presence_and_mapping` | PASS | provider presence + join latency (87 ms), node mapping (`ND_…`; region not reported by a single-node SFU), reconciliation without false drift, disconnect state |
| **`real_livekit_m_waiting_subscription_ban`** (new) | PASS | waiting → admit → credential; **real subscription** of the host's audio + video (322 RTP packets received, 0 lost, 2 tracks); ban → real disconnect; reconnect with the old credential → evicted; identity absent at the provider; no new credential; no active session |
| **`real_livekit_m_media_load_and_large_audience`** (new; default 4 × 16) | PASS — media CLEAN | 0 join failures, connect p50 40 ms / p95 52 ms, 0 disconnects, 152 subscriptions, every participant received every stage track, 242 611 RTP packets, 0 gaps; LiveKit CPU 36 % of a core, 132 MB RSS |
| `real_livekit_part_g_recording_and_object_storage` | see 08 | real S3 checks PASS; real Egress capture failed for an environmental reason in the final re-run |

## §7 checklist

| Requirement | Real evidence | Status |
|---|---|---|
| room create / lookup / terminate | full lifecycle | PASS |
| credential minting | full lifecycle (signed, short-lived) | PASS |
| actual WebRTC join | all tests (pion client) | PASS |
| audio publish | full lifecycle | PASS |
| video publish | Part F / H, media load | PASS |
| subscription | M waiting / subscription, media load | PASS |
| VIEWER publication denial | full lifecycle, Part H, media load | PASS |
| permission update | full lifecycle, Part F | PASS |
| host mute | full lifecycle, Part F | PASS |
| audio lock / video lock enforcement | Part F | PASS |
| screen-share policy / stop | Part F | PASS |
| participant removal | full lifecycle | PASS |
| ban / reconnect protection | M waiting / subscription / ban | PASS |
| provider callbacks / dedupe | full lifecycle (verified webhooks; `room_finished` once), Part G (4 unique egress callbacks → one READY, J–L run) | PASS |
| reconciliation | reconciliation, Part I | PASS |
| conference ending | full lifecycle | PASS |
| backstage isolation, presenter promotion / demotion | Part H | PASS |

## Media load and large audience (§43–§44)

Full table in 06-load-concurrency-report.md §43. Summary: highest **clean** envelope on this
single shared 4-vCPU host = **6 publishers × 50 subscribers**; 4 × 75 and above show RTP gaps
(5.6 %–84 %) caused by the SFU and up to 158 client processes sharing 4 vCPUs (observed
limit of the host, not a product capacity). Control plane (joins, stage cap, viewer pool,
audience publish denial, subscriptions) held at every size up to 8 × 150. No claim beyond
the sizes run is made.

## Reconciliation evidence (§80)

10-failure-recovery-report.md (real: lost room, orphan participant / banned identity,
stale connected session, permission drift; mock: orphan rooms, escalation, breakout and
recording drift).

## Reproduce

```sh
cd services/meeting-service
LIVEKIT_SERVER_BIN=/path/livekit-server RTCPROBE_BIN=/path/rtcprobe \
  scripts/livekit-certification/run.sh                     # E–I + M
CERT_FILTER=real_livekit_m_media_load MEDIA_LOAD_PUBLISHERS=6 MEDIA_LOAD_SUBSCRIBERS=50 \
  MEDIA_LOAD_SECS=45 LIVEKIT_SERVER_BIN=… RTCPROBE_BIN=… scripts/livekit-certification/run.sh
```
CI: job `real-provider` in `.github/workflows/phase2-backend-certification.yml` (manual).
