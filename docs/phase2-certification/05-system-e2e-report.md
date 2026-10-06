# 05 — Multi-client system E2E report (Part M §11–§23, Part N §78)

Harness: `services/meeting-service/tests/system_e2e.rs` — the production router served on a
real TCP port, real PostgreSQL 16.14 and Redis 7.0.15, multiple concurrent actors
(owner / host, co-host, members, guests, presenter, webinar attendee, clinician, patient)
over REST **and** live WebSocket subscriptions (`tests/common/ws.rs`), the production
`LiveKitProvider` talking to the in-process **mock LiveKit RoomService / SFU**
(`tests/common/livekit_mock.rs`, which verifies every credential's signature, room and grants
on connect), and the production workers (scheduler, reconciler, outbox relay, artefact
worker) driven explicitly.

**Provider evidence column**: "mock SFU" = state of the mock RoomService (participants,
grants, rooms, egress); "real SFU" = the corresponding real `livekit-server` 1.8.4 scenario in
`tests/livekit_certification.rs` (07-rtc-provider-certification.md). The system E2E suite
itself is **not** a real-provider certification.

Result: **17 / 17 PASS** (11 core + 6 recovery).

| ID | Setup | Actors | Actions | Expected | Actual | Result | Provider evidence |
|---|---|---|---|---|---|---|---|
| M-E2E-01 | Scheduled space, `GUESTS_ONLY` waiting room, invitation | owner, member, guest | Schedule; member joins directly; guest joins → WAITING; host sees `participant.requested_join` live and in REST; admit; guest receives `participant.admitted` on its SELF_ONLY stream; both obtain credentials; connect | Member admitted immediately; guest waits without credential; after admission both connect; canonical state = DB = provider | As expected; 3 ADMITTED; sessions CONNECTED | PASS | mock SFU: identities present with policy grants; real SFU: `real_livekit_m_waiting_subscription_ban` (wait → admit → real connect + subscription) |
| M-E2E-02 | LIVE conference, co-host member | owner, co-host, guest | Co-host moderates (mute / admit); owner disconnects | Conference keeps running; co-host keeps exactly its granted authority (cannot exceed CO_HOST) | As expected | PASS | mock SFU: moderation calls under co-host; owner identity gone |
| M-E2E-03 | LIVE conference | owner, participant, joiner | Lock; non-exempt join; unlock; join | Locked → join refused; existing participants unaffected; unlock restores | As expected | PASS | mock SFU: existing identity untouched |
| M-E2E-04 | LIVE conference | host, participant (modified client) | Audio lock; client attempts microphone publish; unlock | Provider refuses publish while locked; allowed after unlock | As expected | PASS | mock SFU publish refusal; real SFU: Part F audio lock / restore |
| M-E2E-05 | LIVE conference | host, participant | Remove; reconnect with old credential; new join | Disconnected at provider; old credential evicted; new join refused | As expected | PASS | mock SFU `RemoveParticipant`; real SFU: removal in `real_livekit_full_lifecycle` |
| M-E2E-06 | Recurring daily series across a DST change (time zone aware) | owner | Materialize; cancel one occurrence (exception); re-materialize | Correct local times across DST; cancelled occurrence absent; series intact; no duplicates | As expected | PASS | n/a |
| M-E2E-07 | LIVE conference, 2 breakouts | host, 4 guests | Assign; scoped media + chat; move A → B; countdown close | Media / chat scoped per room; after move no A context; close returns everyone to main | As expected | PASS | mock SFU: breakout rooms created, identity rerouted, rooms terminated |
| M-E2E-08 | Polls with each visibility, anonymous poll | host, guests | Vote; read results per role / visibility | Results visible only per policy; anonymous responses uncorrelatable | As expected | PASS | n/a |
| M-E2E-09 | Recording-capable space | host, participants | Consent (start refused with `CONSENT_REQUIRED` until accepted); start; `recording.started` on live streams; stop; processing; READY; authorized download | Capture only after consent; READY with verified object; download only for authorized | As expected | PASS | mock Egress + in-memory store; real: Part G Egress + MinIO |
| M-E2E-10 | WEBINAR space | host, attendee | Attendee joins as VIEWER; publish attempt; Q&A + poll + reaction; promote to PRESENTER; publish; demote | VIEWER cannot publish; engagement works; promotion grants publish; demotion removes it | As expected | PASS | mock SFU grants; real SFU: `real_livekit_part_h_stage_and_backstage` |
| M-E2E-11 | HEALTHCARE space with appointment adapter | clinician, patient, outsider | Uncontrolled join; join without appointment; with appointment → WAITING; clinician admits; recording policy check | Uncontrolled rejected; appointment required; waiting enforced; recording off by default; no clinical payload persisted | As expected | PASS | mock SFU |
| R-E2E (WebSocket) | LIVE conference | host, guest | Subscribe; disconnect; lock / unlock / lock; reconnect with cursor; reconnect with cursor too far | Replay of exactly the missed events in order; bad cursor → `resync_required` → canonical refetch; media continues | As expected | PASS | session stays CONNECTED |
| R-E2E (callback loss / room loss / orphans) | LIVE conference | guest | Connect without webhook; drop provider room; insert orphan identity; reconciler ticks | Late join repaired → CONNECTED; `LIVE_ROOM_MISSING` repaired (room re-created); orphan evicted; drift logged | As expected | PASS | mock SFU; real SFU: `real_livekit_reconciliation` |
| R-E2E (end while recording) | Recording active | host | End conference | Capture stopped in the same transaction; conference ENDED; artefact finalizes | As expected | PASS | mock Egress stop |
| R-E2E (multi-device / reconnect) | LIVE conference | member (2 devices), guest | Second device; reconnect; guest reconnect with its token | One logical participant, several sessions; guest reconnects only with its server-issued credential; attendance merged | As expected | PASS | two identities, one participant |
| R-E2E (Redis outage) | Redis through a toggling proxy | host, guests | Cut Redis; act; restore | Degraded frame; REST + authority unaffected; nothing granted by Redis; after recovery outbox converges and live delivery resumes | As expected | PASS | n/a |
| R-E2E (invitation / ban) | Invitations, ban | host, invitees | Revoke; expire; use; ban; rejoin | Revoked / expired invitations fail; banned cannot rejoin | As expected | PASS | n/a |

Additional recovery coverage outside this file: `failure_injection_m` (LiveKit down during
remove; slow consumer; cursor too old; scheduler starvation), `rtc_failure`,
`collab_failure`, `platform_failure`, `recording_g` G-F*, `operations_i` I-F*
(10-failure-recovery-report.md).
