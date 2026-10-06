# 02 — Requirements matrix: coverage audit, wave closure, Definition of Done (Part M §4–§10, Part N §51, §57–§74, §90–§92)

Status vocabulary: **COVERED** / **PARTIALLY COVERED** / **NOT COVERED** (coverage audit);
**PASS** / **PARTIAL** / **FAIL** (waves, DoD). "Real" = against real `livekit-server` /
Egress / S3-compatible storage (07, 08); otherwise PostgreSQL + Redis are real and the RTC
provider is the mock RoomService. Test paths are under `services/meeting-service/`.

## 1. Coverage audit (§4) — requirement → test → file → status

### Unit layer (§5) — `cargo test --lib` (127 tests)

| Requirement | Tests | File | Status |
|---|---|---|---|
| Policy / access policies | `policy_step` table: public, authenticated, private / invite, organization, domain, passcode, healthcare / custom adapters (6) | `src/services/meeting_access.rs` | COVERED |
| Permissions | `appendix_a_fixed_cells`, `part_f_*`, `parts_g_to_i_*`, `viewer_never_receives_publish_grants`, `grant_authority_rules`, `role_change_rules`, `moderation_targets_must_be_less_privileged`, … (9) | `src/services/meeting_permissions.rs` | COVERED |
| Profiles | 6 (webinar / town hall defaults, healthcare restrictive, org ceilings, clamping, restrictive fallback, external roles never OWNER) | `src/services/profiles.rs` | COVERED |
| Recurrence | unsafe rules rejected, DST local time, bounded window, deterministic keys | `src/services/recurrence.rs` | COVERED |
| Conference state machine | `state_machine_transitions`, `illegal_transitions_are_rejected`, `start_window_policy`, `closing_states_block_join` | `src/services/conference.rs`, `src/domain/mod.rs` | COVERED |
| Recording / artefact lifecycle | `artifact_lifecycle_transition_table` (new: `ArtifactStatus::can_transition_to`, terminal states; enforced by `debug_assert!` in transitions) | `src/services/artifacts/mod.rs` | COVERED |
| Poll / Q&A state machines | `lifecycle_is_strict`, `definition_validation`, `response_validation_per_type`, `quiz_scoring_is_server_side`, `anonymous_projection_has_no_identity`, `results_visibility_modes`; Q&A `transitions`, anonymity | `src/services/collaboration/polls.rs`, `questions.rs` | COVERED |
| Breakout algorithms | balanced / random assignment invariants, strict transitions, distinct room prefix | `src/services/collaboration/breakouts.rs` | COVERED |
| Consent | `mode_for`, `withdrawal_mode_for` (new) | `src/services/artifacts/consent.rs` | COVERED |
| Quality classification | session thresholds, conference classification (SLI + incidents) | `src/services/quality/health.rs` | COVERED |
| DTO validation | 17 request DTOs reject injected `userId`, `ownerUserId`, `participantId`, `admissionState`, `rtcRoomName`, `storageKey`, `providerRef`, unknown fields; invalid enums; `JoinRealtimeInfo` (new) | `src/dto/v2.rs` | COVERED |
| Error serialization | envelope shape, `Retry-After` ≥ 1, internal / DB detail hidden, pool timeout → 503, code strings (new) | `src/error.rs` | COVERED |
| Provider capability projection | `grants_follow_media_permissions`, signed short-lived credentials, forged webhooks rejected, admin URL mapping | `src/services/rtc_provider.rs`, `services/livekit.rs` | COVERED |
| Flags | strict env parsing, allowlist parsing, every Phase 2 flag defaults off (new) | `src/config/features.rs` | COVERED |
| Session liveness | `is_live_at` (new; SEC-M-01) | `src/models/participant_sessions.rs` | COVERED |

### Database integration layer (§6)

| Requirement | Tests | Status |
|---|---|---|
| Constraints (identity, conference, membership / invitation, participant / session scoping, append-only audit, bans) | `tests/db_invariants.rs` i01–i06 | COVERED |
| Migrations idempotent / reversible | `db_invariants::i07`, `migration_backfill::m02`, CI up / reset / up | COVERED |
| Transactional outbox, gap-free sequences | `rtc_outbox_realtime::o1`, `o2` | COVERED |
| Concurrency at the database | `concurrency_v2`, `rtc_concurrency`, `platform_concurrency`, `invariants_certification::inv_admitted_and_denied_are_exclusive` | COVERED |
| Room ↔ conference uniqueness, READY artefact validity | `invariants_certification::inv_rtc_room_maps_to_exactly_one_conference`, `inv_ready_recording_references_conference_and_storage` | COVERED |
| Query plans at volume | `tests/query_plans.rs` (ignored; run by certification) | COVERED |

### RTC integration layer (§7) — real `livekit-server` 1.8.4 (07-rtc-provider-certification.md)

| Requirement | Real test | Status |
|---|---|---|
| room create / lookup / terminate | `real_livekit_full_lifecycle` | COVERED |
| credential minting, actual WebRTC join | same | COVERED |
| audio publish / video publish | `real_livekit_full_lifecycle` (mic), Part F / H / media load (camera) | COVERED |
| subscription | `real_livekit_m_waiting_subscription_ban` (audio + video RTP received), media load | COVERED |
| VIEWER publication denial | `real_livekit_full_lifecycle`, Part H, media load | COVERED |
| permission update, host mute | `real_livekit_full_lifecycle`, Part F | COVERED |
| audio / video lock, screen-share policy / stop | `real_livekit_part_f_media_controls` | COVERED |
| participant removal | `real_livekit_full_lifecycle` | COVERED |
| ban / reconnect protection | `real_livekit_m_waiting_subscription_ban` | COVERED |
| provider callbacks, dedupe | `real_livekit_full_lifecycle` (verified webhooks; `room_finished` applied once) | COVERED |
| reconciliation | `real_livekit_reconciliation`, Part I | COVERED |
| conference ending | `real_livekit_full_lifecycle` | COVERED |
| backstage isolation, presenter promotion / demotion | `real_livekit_part_h_stage_and_backstage` | COVERED |

### Recording / storage (§8) — real Egress + S3 (08)

`real_livekit_part_g_recording_and_object_storage`: COVERED by the J–L real run; the M / N re-run certified real S3 again but the real Egress capture failed for an environmental reason (08) — PARTIALLY COVERED in M / N.

### API integration layer (§9) and OpenAPI parity (§10)

Every v2 operation has at least one success-path test and one authorization / error-path
test, measured at runtime (`tests/common/route_coverage.rs` records method, matched route
and status for every request issued by any test; `scripts/certification/route_coverage.py
--strict` joins with the contract). The Part M audit found 16 operations without a
success-path test; `tests/api_operation_coverage.rs` closed them. Current report:
[`route-coverage.md`](route-coverage.md) (generated from the full regression run).
OpenAPI parity: 15-openapi-realtime-compatibility.md. Status: COVERED.

### System E2E (§11–§23), security (§24–§26), migration (§27–§28), failure (§29–§35), load (§36–§45)

05, 09, 03, 10, 06 respectively. Status: COVERED (with staging / production items NOT
EXECUTED as stated in 11).

## 2. Wave closure audit (§51)

| Wave | Scope | Evidence | Status |
|---|---|---|---|
| Schema & abstractions | additive schema, `RtcProvider`, actor / access foundation, compatibility mapping | `db_invariants`, `architecture_boundaries` (LiveKit SDK confined to provider adapters; provider commands only via outbox), `migration_backfill` | PASS |
| Meeting core | MeetingSpace, ConferenceSession, scheduling, occurrences | `api_v2_e2e`, `recurrence_exceptions`, unit conference / recurrence, `m_e2e_06` | PASS |
| Access & participants | members, invitations, waiting room, roles, ParticipantSession | `api_v2_roles`, `security_v2`, `organization_domain_access`, `m_e2e_01..03` | PASS |
| RTC enforcement & realtime | provider control, remove / end, permissions, event stream | `rtc_*` suites, real LiveKit, `rtc_outbox_realtime` | PASS |
| Collaboration | chat, reactions, hand raise, breakouts, polls, Q&A | `collab_*` suites, real Part F, `m_e2e_07/08` | PASS |
| Recording / artefacts | recording, consent, artefact lifecycle, attendance | `recording_g`, real Part G, `m_e2e_09` | PASS |
| Special modes | webinar, town hall, healthcare | `profiles_h`, real Part H, `m_e2e_10/11` | PASS |
| Quality / operations | telemetry, reconciliation, operations visibility | `operations_i`, real Part I | PASS |
| Migration / certification | backfill, compatibility, load, security, failure, rollout controls | 03, 06, 09, 10, `rollout_flags` | PASS (local); staging NOT EXECUTED |

## 3. Definition of Done (§57–§74)

| § | Area | Condition | Evidence | Status |
|---|---|---|---|---|
| 57 | Meeting space | persistent config, ownership, security, recurrence, stable links, legacy links resolve | `api_v2_e2e::e2e01`, `e2e02`, `e2e04_05`, `security_v2`, migration report (20 000 links) | PASS |
| 58 | Conference | start / end / cancel / failure transactional + idempotent; real media terminates on end | unit state machine, `concurrency_v2::c01/c04`, `rtc_concurrency::c3`, real `terminate_room + ENDED` | PASS |
| 59 | Scheduling | instant, scheduled, recurring, timezone-aware, exceptions | `api_v2_e2e::e2e03/e2e04_05`, `recurrence_exceptions::rx01..03` (DST), `m_e2e_06` | PASS |
| 60 | Access | public / authenticated / private / organization / invite / passcode distinct, server-side | unit `meeting_access`, `security_v2::s04/s06_07_08/s20`, `organization_domain_access` | PASS |
| 61 | Waiting room | policy modes, queue, bulk admit, deny, return-to-waiting, realtime | `api_v2_e2e::e2e07_08/e2e09`, `api_contract_matrix::j_api_05`, `m_e2e_01`, churn load | PASS |
| 62 | Roles | roles ↔ backend permissions; RTC grants align; modified client cannot bypass | unit `meeting_permissions`, `rtc_security::s05_s06/s07`, real VIEWER denial | PASS |
| 63 | Participants | logical vs device separation, multi-device, reconnect, attendance | `api_v2_e2e::e2e14`, `r_e2e_reconnect_across_sessions_and_guest_reconnect`, `j_leave_closes_session_and_keeps_logical_participant` | PASS |
| 64 | Media | mute, audio lock, video lock, screen share, VIEWER — provider-enforced | real Part F + VIEWER denial; `collab_e2e::f01..f06` | PASS |
| 65 | Collaboration | chat, reactions, hand raise, breakouts, polls, Q&A scoped | `collab_e2e::f08..f22`, `collab_security::*` | PASS |
| 66 | Recording | consent / policy gates, real provider recording, storage, READY, authorized access, retention / deletion | real Part G (J–L run: full PASS; GitHub CI run 37384968304: 8 / 8 real-provider tests, real Egress → READY, real S3 — 08), `recording_g` | **PASS** (re-certified in CI) |
| 67 | Transcription readiness | provider-neutral interfaces, consent / policy gates, segment model, caption readiness | `recording_g` transcription tests (`g_f12`, `g_c11`), `artifacts::transcription` unit | PASS (production vendor not required by approved scope) |
| 68 | Webinar | viewer attendee, Q&A / collaboration, promotion, RTC permission update | `profiles_h::h_e2e_01..06`, real Part H, `m_e2e_10` | PASS |
| 69 | Healthcare | appointment authorization, waiting-room / recording defaults, transcription / AI restrictions, audit, fail-closed adapter | `profiles_h::h_e2e_14..18`, `h_s*`, `h_f05/h_f06`, `appointment_revocation`, `m_e2e_11` | PASS (production HealthNet connection intentionally deferred) |
| 70 | Quality / ops | join quality, media quality, drift, recording / transcription failures, reconciliation, ops visibility | `operations_i::i_e2e_01..15`, real Part I, runbook | PASS |
| 71 | Migration | mapping, links, compatibility; no required consumer depends on legacy writes **before** retirement | 03 | **Backend migration capability: PASS. Production legacy-write retirement: NOT STARTED** (frontend still uses v1 writes) |
| 72 | Security | all mandatory security tests pass; no critical / high | 09 (SEC-M-01 fixed) | PASS |
| 73 | Reliability | concurrency, failure, recovery, worker / event durability, idempotency | 10 (REL-M-01 fixed), concurrency suites | PASS |
| 74 | CI | automatic tests, build, lint, OpenAPI, migration, security; fmt pass or documented baseline with 0 new drift; no regression behind `continue-on-error` | `.github/workflows/backend.yml` (fast gate on `main` + `staging`; fmt baseline check now **blocking**; `continue-on-error` removed), `phase2-backend-certification.yml` | PASS (workflows not yet executed on GitHub — they run on the next push) |

## 4. Invariant re-certification (§91)

| Invariant | Permanent tests | Status |
|---|---|---|
| ENDED / CANCELLED conference cannot issue join credentials | `security_v2::s19_ended_cannot_join`, `rtc_security::s10_s11_*`, `credential_replay` (pre-end), unit `closing_states_block_join` | PASS |
| ENDING cannot admit a new participant | `security_v2::s18_ending_cannot_admit`, `platform_security::l_s08_to_s11_*` | PASS |
| Participant cannot be ADMITTED and DENIED simultaneously | `invariants_certification::inv_admitted_and_denied_are_exclusive` (new: real admit / deny race), schema single-valued `admission_state` + `conference_participants_check2` | PASS |
| BANNED participant cannot hold an active ParticipantSession | `security_v2::s16_banned_cannot_hold_session`, `db_invariants::i06_*`, real ban test | PASS |
| VIEWER receives no publication grants before role / policy change | unit `viewer_never_receives_publish_grants`, `rtc_security::s05_s06`, real VIEWER denial | PASS |
| RTC room maps to exactly one live ConferenceSession | `invariants_certification::inv_rtc_room_maps_to_exactly_one_conference` (new), `uq_conference_one_live_per_space`, unique `rtc_room_name` | PASS |
| READY recording references valid conference + authorized storage object | `invariants_certification::inv_ready_recording_references_conference_and_storage` (new), `ck_artifact_ready_validated`, real Part G | PASS |
| Revoked / expired / exhausted invitation unusable | `security_v2::s06_07_08`, `credential_replay`, `r_e2e_invitation_revoke_expiry_and_ban_rejoin` | PASS |
| ParticipantSession belongs to exactly one ConferenceParticipant and ConferenceSession | `db_invariants::i04_participant_and_session_scoping`, FK `fk_session_participant_scope` | PASS |
| Cross-conference moderation fails before provider call | `invariants_certification::m_s04_*` (new: zero provider calls), `rtc_security::s12`, `idor_matrix` | PASS |

## 5. Architecture review questions (§92)

| Question | Answer | Evidence |
|---|---|---|
| Can a MeetingSpace exist without an active conference? | **YES** | `api_v2_e2e::e2e01` (space created and configured with no conference) |
| Can one recurring MeetingSpace produce many independent ConferenceSessions? | **YES** | `e2e04_05`, `recurrence_exceptions`, scheduler load (300 series → 3 000 occurrences, 0 duplicates) |
| Can one logical participant join from multiple devices? | **YES** | `e2e14_multi_device`, `r_e2e_reconnect_across_sessions_and_guest_reconnect` |
| Can the backend prove guest identity without relying on display name? | **YES** | server-issued hashed participant token; M-S2 tests |
| Can the host control admission / media without trusting client UI? | **YES** | permission engine + provider grants; M-S6, real Part F / H |
| Does end / remove actually enforce against the RTC provider? | **YES** | real `remove_participant` / `terminate_room` (disconnected, room absent); ENDED only after confirmed absence |
| Can critical state survive retries / duplicates / multiple workers? | **YES** | concurrency suites, idempotency keys, webhook dedupe, scheduler / outbox / reconciler races, REL-M-01 fix |
| Can clients recover canonical state after WebSocket loss? | **YES** | cursor replay, `resync_required` + canonical refetch (§35 tests) |
| Can recording / transcription only activate under policy / consent? | **YES** | consent gates (`CONSENT_REQUIRED`), healthcare defaults off, `recording_g` |
| Can webinar / viewer and healthcare run on the same generic core? | **YES** | profiles over one domain model (`profiles_h`, M-E2E-10 / 11) |
| Can LiveKit be replaced without changing the public CF Meet API? | **YES** | `RtcProvider` trait; SDK confined to adapters (`architecture_boundaries::k_e2e_09`); public contract has no provider ids (`contract_never_describes_secrets_or_provider_ids`) |
| Can existing meeting links / records survive migration? | **YES** | 20 000 / 20 000 links preserved; resolver + legacy reads (03) |

No question is answered NO.

## 6. Final engineering deliverable checklist (§90)

| Item | Status |
|---|---|
| additive migrations | ✔ (Part M adds one additive index) |
| SeaORM models / relations | ✔ |
| MeetingSpace / Conference services | ✔ |
| access / admission / permissions | ✔ |
| RtcProvider + LiveKitProvider | ✔ (real certification) |
| realtime / outbox | ✔ |
| scheduler | ✔ (REL-M-01 fixed) |
| reconciler | ✔ (+ session expiry sweep) |
| artefact workers | ✔ |
| v2 routes, handlers, DTOs | ✔ |
| OpenAPI | ✔ (187 / 136, parity enforced) |
| healthcare profile | ✔ |
| quality / metrics | ✔ |
| migration compatibility | ✔ |
| automated test matrices | ✔ |
| system E2E | ✔ |
| load tests | ✔ (harness + local results) |
| failure tests | ✔ |
| runbooks | ✔ (14) |
| alerts / dashboard specification | ✔ (14) |
| rollback plan | ✔ (13) |
| staging certification readiness | ✔ prepared — execution PENDING ENVIRONMENT (11) |
