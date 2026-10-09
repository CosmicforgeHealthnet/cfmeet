# Part F — Enterprise Calling & Continuity: certification report (§§52–62)

Base: `staging` at `f28bdb92977747def4f2ac56b02bb59770359071` (Part E complete). Part E's
one external item — **real lawful telecom interconnect certification** — remains pending
(ADR 0016) and is not a Part F defect: every public-network result below is certified
against the simulated Part E adapter only and is **real-carrier certification pending**.

Decisions: ADR 0018 (architecture), 0019 (ring groups / queues), 0020 (IVR / SIP devices),
0021 (business hours / forwarding), 0022 (hold / resume / transfer), 0023 (object storage
/ voicemail), 0024 (recording policy), 0025 (history), 0026 (operations).

## 1. Certification evidence (§200–§213)

| § | Requirement | Evidence (test → assertion) | Media / stack |
|---|---|---|---|
| 200 | Extension A → B, no PSTN, real internal media, correct history | `rtc_certification::enterprise_extension_call` (CONNECTED on verified evidence, audio subscribed, no telephony command / public party / non-internal route, both histories ANSWERED with duration); `enterprise_extensions::extension_to_extension_is_one_internal_call`; real-stack scenario 6 (SIP desk → extension → app member, bridged mixed audio) | real LiveKit + WebRTC; real Asterisk + SIPp |
| 201 | Simultaneous group, ≥ 2 recipients, one winner, others answered-elsewhere | `rtc_certification::enterprise_ring_groups` (SIMULTANEOUS); `enterprise_ring_groups::simultaneous_group_rings_everyone_and_one_member_wins`, `concurrent_member_answers_have_exactly_one_winner`; `call_history::ring_group_members_see_answered_elsewhere_or_missed` | real LiveKit |
| 202 | Ordered group: first does not answer, second rings and answers, one Call | `rtc_certification::enterprise_ring_groups` (ORDERED); `enterprise_ring_groups::ordered_group_steps_member_by_member` | real LiveKit |
| 203 | Queue entry, position, offer, answer, timeout, overflow, abandonment | `rtc_certification::enterprise_queue`; `enterprise_queues::*` (FIFO positions, offers, timeouts / rotation, max wait overflow / timeout, capacity, abandonment, races); `enterprise_load::queue_burst_keeps_fifo_order` | real LiveKit |
| 204 | IVR against real Asterisk / SIPp: prompt, DTMF, valid / invalid branch, timeout, extension, queue, voicemail, hangup | `enterprise_sip_certification` scenarios 1–5 (RFC 4733 keys, built-in prompts, retry, timeout, extension, queue, IVR → mailbox, hangup) | real Asterisk + SIPp + real Telephony |
| 205 | Open, closed, exception date, DST boundary | `enterprise_hours_forwarding::schedules_route_open_and_closed_hours_on_one_call` (Lagos open / closed / exception, Europe/London spring-forward), `business_hours_administration_and_deterministic_evaluation`, `schedule_changes_during_route_selection_are_consistent` | — |
| 206 | Hold / resume of an actual enterprise call with state and media evidence | `rtc_certification::enterprise_extension_call` (HELD with publish permission revoked at the provider, CONNECTED with it restored); `enterprise_transfer::hold_resume_and_transfer_to_an_extension_keep_one_call` | real LiveKit |
| 207 | Transfer A/B → C on the same Call | `rtc_certification::enterprise_transfer` (B removed from the room, A hears C, same Call id); `enterprise_transfer::*` | real LiveKit |
| 208 | Failed transfer recovers, no orphan leg | `rtc_certification::enterprise_transfer` (declined transfer, CONNECTED, media intact); `enterprise_transfer::failed_transfers_recover_without_orphans`, `transfer_races_leave_one_owner` | real LiveKit |
| 209 | Unanswered → voicemail, real media captured, object finalized, history, authorized retrieval, read state, delete / retention | `enterprise_sip_certification` scenario 5 (edge capture, S3-compatible upload, READY, signed retrieval of the same bytes, READ, deletion, caller history); `enterprise_voicemail::*`; `call_history::voicemail_is_linked_for_the_mailbox_owner_only` | real Asterisk + SIPp + S3-compatible store |
| 210 | Call without recording; denied without evidence; allowed with evidence; protected artifact; access audited | `enterprise_sip_certification` scenario 6; `enterprise_recording::*` | real Asterisk + SIPp + S3-compatible store |
| 211 | Realtime / push disabled; missed and answered Calls; reconnect; canonical history | `call_history::a_missed_call_survives_push_and_realtime_outage`, `one_answered_device_answers_for_the_logical_participant` | — |
| 212 | Tenant A admin with B's ids cannot read, update, route through, listen to voicemail, download recording | `enterprise_certification::org_isolation_certification` | — |
| 213 | Public forwarding through the simulated Part E adapter | `enterprise_hours_forwarding::public_forwarding_passes_every_part_e_gate`, `public_forwarding_cannot_bypass_part_e` — **real-carrier certification pending (Part E)** | simulated adapter |

## 2. Mutation tests (§188)

Each mutation was applied alone, its target test run, the source restored
(`scratchpad` runner, results below).

| # | Mutation | Target test | Result |
|---|---|---|---|
| A | extension lookup without its tenant / application / environment scope | `enterprise_extensions::extension_calls_are_scoped_and_policy_gated` | **killed** |
| B | ring-group claim conflict ignored (`claimed == 0` check removed) | `enterprise_ring_groups::concurrent_member_answers_have_exactly_one_winner` | survived — the Call row lock + offer state machine still admit one winner (defence in depth) |
| B2 | claim check AND the database `require_group_claim` guard removed | same | survived — same reason |
| B3 | ALL single-winner locking removed (claim check, database guard, Call / invitation row locks) | same | **killed** (two winners) |
| C | queue ownership locking removed (advisory lock + unique pending-offer indexes) | `enterprise_queues::concurrent_dispatchers_never_double_offer` | **killed** |
| D | IVR step budget removed | `enterprise_ivr::the_step_budget_bounds_a_menu_cycle` | **killed** |
| E | node-level visited / hop guard removed | `enterprise_hours_forwarding::forwarding_loops_are_impossible` | survived — the principal-level visited guard still blocks the loop (defence in depth) |
| E2 | BOTH visited-set loop guards removed | same | **killed** |
| F | raw `phoneNumber` field added to the forwarding-rule contract | `enterprise_security::public_enterprise_contracts_carry_no_numbers_provider_ids_or_storage_secrets` | **killed** |
| G | business hours evaluated in UTC instead of the rule's IANA zone | `enterprise_hours_forwarding::schedules_route_open_and_closed_hours_on_one_call` | **killed** |
| H | voicemail access without mailbox ownership | `enterprise_voicemail::retrieval_is_authorized_and_audited` | **killed** |
| I | recording allowed without the required consent | `enterprise_recording::consent_is_each_participants_own_and_bound_to_the_call` | **killed** |
| J | missed derived from push deliverability (endpoint push target) | `call_history::a_missed_call_survives_push_and_realtime_outage` | **killed** |

Every required mutation (A–J) is detected: where the first, narrow mutation survived (B,
E) the protected invariant has a second independent guard, and removing the whole
mechanism (B3, E2) makes the target test fail. Sources were restored after each run.

## 3. Races (§189), failure injection (§191–§196), load (§197–§199)

**Races (§189 / §190)** — each asserts one winner / owner / artifact and coherent
history: `enterprise_ring_groups::concurrent_member_answers_have_exactly_one_winner`,
`enterprise_queues::concurrent_dispatchers_never_double_offer` (two workers, same entries),
`enterprise_queues::abandon_and_answer_race_is_consistent`,
`enterprise_transfer::transfer_races_leave_one_owner` (transfer vs transfer, accept vs
end, transfer vs end), `enterprise_hours_forwarding::schedule_changes_during_route_selection_are_consistent`,
`enterprise_voicemail::concurrent_finalization_stores_once`,
`enterprise_voicemail::delete_racing_retention_deletes_once`,
`enterprise_recording::concurrent_starts_record_once`,
`enterprise_recording::stop_racing_the_call_end_stops_once`, `enterprise_load::ring_fanout_is_bounded`
(concurrent answers at maximum fan-out).

**Failure injection (§191–§196)** — `enterprise_operations::a_database_failure_mid_routing_leaves_no_effect`
(no Call / offer / step / event, retry with the same key succeeds),
`telephony_down_degrades_only_sip_paths` (application Calls answered; SIP bridge command
durable and retried; never CONNECTED without media evidence; end never blocked),
storage outage (`enterprise_voicemail::storage_failure_is_recoverable_and_captures_can_fail`,
`enterprise_recording::stops_revocation_deadlines_and_failures`), realtime / push down
(`call_history`, PostgreSQL LISTEN / NOTIFY only — Calling has no Redis), worker crash /
long lock (`a_stuck_queue_entry_is_an_incident_and_drains_later`).

**Load (§197–§199)** — `enterprise_load` (debug build, in-process HTTP stack, local
PostgreSQL 16, scale 1; printed `LOAD` lines):

| Path | Measured |
|---|---|
| extension lookup + Call, sequential | 20 in 5.1 s (3.9 Calls/s) |
| extension lookup + Call, 20 concurrent | 20 in 3.2 s (6.3 Calls/s) |
| history page (25 entries) | 20 in 0.51 s (39 pages/s); plan = `idx_call_participants_history` range |
| queue burst insertion (60 concurrent Calls, one queue) | 60 in 7.0 s (8.5/s); positions 1..60 unique, FIFO, plan = `idx_enterprise_queue_entries_fifo` |
| queue assignment (4 agents, 3 rounds) | 12 in 3.1 s; the 12 oldest entries, in order |
| ring-group fan-out at the policy maximum (10 of 15 members) | 10 Calls in 3.2 s; exactly 10 offers each; one winner of 10 concurrent answers |
| IVR Call + key transition | 20 in 3.8 s (5.2/s) |
| business-hours evaluation | 100 in 1.0 s (96/s) |

Throughput is bounded here by the debug build and the single local database; the
invariants (FIFO, unique positions, bounded fan-out, single winner, index-only plans) are
what CI enforces (`enterprise-races` job).

## 4. Gates (§214–§224)

Local run on the final tree (PostgreSQL 16, Asterisk 20.6, SIPp, LiveKit 1.13.9 with real
WebRTC clients, an S3-compatible store), before the push:

| Gate | Result |
|---|---|
| §214 repository search: no phase path, no Meeting Service import, no PBX / voicemail / IVR service, no duplicate Call table, no raw number fields, no Asterisk concept in the Calling domain, no public object URL, no logged key, no cross-service database URL | clean (only doc comments / deny-list tests match) |
| §215 contracts: `make contracts-validate`; validator tests | OK — 38 contracts, 150 examples, 218 counter-examples; 17 / 17 |
| §216 boundaries: `check_service_boundaries.py` + its tests; `architecture_boundaries` | OK (calling 73 tables, telephony 17); 55 / 55; 18 / 18 |
| §217 service-auth (default and `client` features) | 114 / 114 |
| §218 Calling: fmt, `check --workspace --locked`, `clippy --workspace --all-targets --locked -D warnings`, `test --workspace --no-fail-fast`, `build --workspace --locked` | clean; **505 passed, 0 failed** |
| §219 Telephony: fmt, clippy, full suite | clean; 87 / 87 |
| object storage (`packages/object-storage`) | clean; 6 / 6 |
| §220 Calling migrations up / status / reset / up (30 migrations) + the CI structural checks | OK |
| startup refusals and health (CI step, run locally) | OK |
| §221 Calling RTC certification (real LiveKit + WebRTC): Part C regression, refreshed-token re-entry (3 rounds), enterprise extension (+ history), SIMULTANEOUS / ORDERED groups, queue, transfer, failed transfer | **PASS** |
| §222 Telephony SIP certification (real Asterisk + SIPp) | **PASS** |
| §223 enterprise certification (real Telephony + Asterisk + SIPp + S3-compatible store): IVR × 4, voicemail (+ history), recording | **PASS** |
| Calling ↔ Privacy Relay e2e (real binary); Calling ↔ Telephony ↔ Privacy Relay e2e (real binaries) | 3 / 3; 2 / 2 |
| §224 high-risk repetitions: 11 tests × 5 (ring-group dual answer, queue dual dispatch, abandon race, transfer / end races, voicemail finalization and delete-vs-retention, recording start / stop races, schedule change during routing, history reconnect × 2) | **55 / 55**, no flakes |
| mutation tests A–J | all detected (section 2) |
| Calling container build context (`Dockerfile` + `.dockerignore` now include `packages/object-storage`) | the filtered context resolves `--locked`; the previous context did not |

## 5. Pending (recorded, not faked)

* **Part E external blocker:** real lawful telecom interconnect certification
  (ADR 0016). Part F's public forwarding is certified against the simulated adapter only.
* Not offered by this deployment, refused safely rather than faked: transfer of an
  established Call to a public contact (422); voicemail / recording capture of
  application-only (RTC) Calls — needs a LiveKit Egress deployment as the RTC capture
  adapter (SIP edge capture is certified); keypad consent from SIP desk phones (policies
  that require it deny recording); recordings longer than 24 minutes (single bounded
  delivery).
