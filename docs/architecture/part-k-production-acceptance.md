# Part K — Testing, certification and production acceptance (§§99–110)

Status: **PART K SOFTWARE TEST PROGRAMME — COMPLETE**. **FULL PART III PRODUCTION
ACCEPTANCE — PENDING** (Telecom gate: no lawful carrier interconnect, ADR 0016).
Parts I and J: [`part-i-reliability-certification.md`](part-i-reliability-certification.md),
[`part-j-persistence-deployment-certification.md`](part-j-persistence-deployment-certification.md).
Security baseline: [`part-h-security-certification.md`](part-h-security-certification.md).

The evidence is kept in five separate classes, never merged into one green:

| Class | Status | Evidence |
|---|---|---|
| SOFTWARE (deterministic, PostgreSQL, fakes behind the real ports) | **PASS** | §1 full suites, 0 failures |
| REAL RTC (LiveKit) | **PASS** | Calling RTC certification workflow on the final code SHA (§1) |
| CF-OWNED SIP / MEDIA EDGE (Asterisk + SIPp) | **PASS** | `sip_certification` + enterprise SIP certification with `*_CERT_REQUIRED=1`, locally and in CI |
| REAL CARRIER (lawful PSTN interconnect) | **PENDING — EXTERNAL CREDENTIAL BLOCKER** | `real_interconnect_certification` prints NOT RUN; nothing about a real carrier is certified |
| LOAD / CAPACITY | **CONTROL PLANE MEASURED**; media and carrier scale not certified | §108 |

## 1. Final gates

Final code tree: see the closure report (git section). Every suite was run in full on the
stabilized tree (targeted runs during development are not counted here).

| Gate | Result |
|---|---|
| Calling — fmt, check, clippy `-D warnings`, `cargo test --workspace --no-fail-fast`, build | **682 passed, 0 failed** (87 binaries; `privacy_relay_e2e` and `telephony_e2e` against the real binaries; enterprise SIP certification required and run against real Asterisk + SIPp + an S3-compatible store; disaster recovery required and run). The local RTC section prints SKIPPED (no LiveKit here) and is not counted. |
| Privacy Relay — same | **168 passed, 0 failed** (41 binaries; disaster recovery required and run) |
| Telephony — same, `TELEPHONY_SIP_CERT_REQUIRED=1`, `DISASTER_RECOVERY_CERT_REQUIRED=1` | **148 passed, 0 failed** (29 binaries; SIP / media edge sections A–H incl. carrier RFC 4733 DTMF; SIP signalling load; real interconnect NOT RUN) |
| Shared packages | service-auth 56 (core) / 58 (client); webhooks 14; object-storage 9 (incl. a live S3-compatible server); contracts validator 17 + `contracts: OK — 40 contracts, 164 examples, 255 counter-examples, 461 JSON files` |
| Service boundaries guard + operations registry + mutation tests | guard OK (calling 89, meeting-service 40, privacy-relay 26, telephony 21 tables); operations registry OK (21 responsibilities); 66 guard + registry mutation tests OK |
| Secret scan | repository clean; 12 scanner mutation tests OK (incl. image trees: unmodified Debian package files are verified against dpkg MD5 manifests, anything added or altered is scanned) |
| Per-service database isolation (real migrations, least-privilege roles) | OK — 36 checks; the real Calling (42), Privacy Relay (14) and Telephony (15) migrations apply as their migrator roles and each runtime role reads every table |
| Migrations — fresh up / status / reset / re-up; upgrade from previous head with data | fresh up / status / reset / re-up OK for all three (applied 42 / 14 / 15); `migration_upgrade` (N-1 → head with data → down one → re-up) passes in each service |
| RustSec advisories (all Part III lockfiles) | clean (RUSTSEC-2023-0071 ignored with written justification: `rsa` unreachable, EdDSA / HS256 only); rustls bumped to 0.23.46 |
| GitHub Actions on the final code SHA | recorded in §14 |

## 2. §99 Testing philosophy

Deterministic fakes implement the same ports as production adapters and cover every
branch; real-system evidence is required and kept separate where the specification asks
for it (RTC, SIP edge, carrier). A SKIP is never counted: certification workflows set
`*_CERT_REQUIRED=1` and fail on a SKIPPED marker; the real-carrier test prints NOT RUN.

## 3. §100 Domain and state tests — PASS

| Area | Evidence |
|---|---|
| Every allowed / disallowed Call transition | `call_state_machine::{database_graph_equals_the_rust_graph_for_every_pair, every_allowed_transition_succeeds_once_with_one_event_and_one_outbox_row, disallowed_transitions_and_out_of_source_actions_are_refused_without_side_effects}` |
| Call-leg transitions (exhaustive, N) | Calling `the_leg_transition_table_is_exhaustive_in_rust_and_in_the_database`; Telephony `every_leg_state_pair_agrees_between_the_rust_graph_and_the_database` |
| Unknown telecom outcome | Telephony `unknown_outcome::*`; Calling `unknown_outcome_is_not_failure_and_events_are_deduplicated_ordered_and_matched` |
| Context versioning / expiry / revocation / direction / grace | Privacy Relay `contexts`, `lifecycle`, `expiry`, `authorization`, `relay_numbers` suites |
| Route policy, app-first preference | `routing/engine.rs` unit tests, `routing_engine::routing_policies_are_typed_versioned_and_fail_closed` |
| Spend, destination, concurrency, rate limits | `public_spend::*`, `destination_classes::*`, `call_concurrency::*`, `quota_levels::*`, `limits::*` |
| Extension, queue, business-hours routing | `enterprise_extensions::*`, `enterprise_queues::*`, `enterprise/hours.rs` (incl. DST) |
| Provider status normalization, late events | `sip/ari.rs::causes_and_states_map_to_the_neutral_vocabulary`, `provider_contract::*`, `late_events_never_change_a_terminal_call`, `out_of_order_events_never_regress_and_late_success_never_resurrects` |

## 4. §101 API and authorization — PASS

`api_authorization_matrix` (N) reads all 81 mutable operations from the Calling OpenAPI
document and, against the real router, proves for each: no / invalid credential → 401,
each missing scope → 403, internal / endpoint / provider-callback authorities refused,
malformed input refused, and **no table changed** (row counts and content
fingerprints). A new mutable operation that is not classified fails the test. Success and
409 (stale version, invalid state, idempotency conflict, terminal Call), scope × ownership
independence, and JSON-object bodies (a defect: array bodies were accepted positionally —
fixed by a body-shape middleware) are covered. IDOR: `tenant_isolation::*`,
`call_api::foreign_calls_are_indistinguishable_from_missing_ones`, Privacy Relay
`isolation::*`, `part_d_certification::tenant_b_cannot_touch_anything_of_tenant_a_and_learns_nothing`,
enterprise / usage cross-scope tests. Credentials: `authentication::*` (expired, revoked,
guessing throttle N), `rotation::*`. Authority spoofing: `environment_isolation::*`,
`scopes::client_supplied_scopes_do_nothing`, `limits::limits_cannot_be_supplied_by_the_client`.

## 5. §102 Real app-to-app RTC — PASS (CI)

`rtc_certification::real_livekit_certification` against a real LiveKit server: voice
(media flows, camera denied, hold / resume enforced at the provider, end → room
terminated, client disconnected, usage), video and removal, ring groups (one winner,
ANSWERED_ELSEWHERE), forged / duplicate callbacks, unauthorized / refreshed / superseded /
terminal-Call tokens refused, legitimate reconnect, reconciliation, unknown-participant
quarantine, enterprise extension / queue / transfer, and (N) the end-of-call quality
summary recorded from the real media evidence (2 admitted sessions, 2 connected
participants). Locally there is no LiveKit server: the local run prints SKIPPED and is not
counted; the workflow run on the final code SHA is the evidence (§1).

## 6. §103 Privacy Relay — PASS

Plaintext unavailable (`contact_vault::a_number_is_canonicalized_encrypted_and_only_ever_returned_masked`,
`architecture_boundaries::calling_never_requests_plaintext_contact_resolution`); expired
relay (`expiry::a_session_expires_on_its_own_window`, `relay_inbound::a_delayed_callback_never_reaches_the_next_session`);
revoked context (`privacy_enforcement::revocation_cancels_ringing_and_ends_connected_protected_calls_once`,
`cross_service_races::context_revoked_*` N); reassignment (`reassignment::*`,
`cross_service_races::reassignment_while_the_former_participant_rings_leaves_no_authority_behind` N);
cross-scope (`contact_crypto_scope::*`, `isolation::*`); logs / metrics / webhooks
(`contact_logging::*`, metric-safety suites, `webhooks::privacy_relay_events_are_normalized_before_delivery`);
number reuse (`relay_numbers::a_released_number_cools_down_before_anyone_else_gets_it`).

## 7. §104 Real telephony

```
§104 SOFTWARE / HARNESS                 PASS  (provider contract, events, paid safety, reconciliation,
                                               failure injection; real_interconnect_certification harness ready)
§104 REAL CARRIER EXECUTION             PENDING — EXTERNAL CREDENTIAL BLOCKER
CF-OWNED SIP / MEDIA EDGE               PASS  (Asterisk + SIPp, carrier RFC 4733 DTMF, SIP signalling load)
TELECOM PRODUCTION GATE                 PENDING
```

No fake PSTN certification was run; no credentials were invented.

## 8. §105 Enterprise — PASS

Real Asterisk + SIPp + S3 (`enterprise_sip_certification`, `ENTERPRISE_SIP_CERT_REQUIRED=1`):
IVR DTMF routing (valid / invalid + retry / timeout / hang-up / abandon), voicemail
(edge capture → protected upload → READY → authorized retrieval → read → delete →
history; denied access audited), recording (consent denial, notice, bridge capture, stop,
audited retrieval), org isolation (36 cross-tenant attempts → 404). Real LiveKit:
extension-to-extension, ring-group single winner, queue answer, hold / resume, transfer,
failed-transfer recovery. Deterministic: business hours open / closed, queue timeout /
overflow, voicemail retention / deletion (`enterprise_*` suites).

## 9. §106 Concurrency and races — PASS

| Race | Evidence (rounds) |
|---|---|
| Two devices accept | `app_races::concurrent_accepts_have_exactly_one_winner` (20 × 8 devices), `enterprise_ring_groups::concurrent_member_answers_have_exactly_one_winner` |
| Context revoked while the PSTN leg originates | `cross_service_races::{context_revoked_while_the_originate_is_in_flight_ends_in_the_controlled_outcome, context_revoked_after_the_grant_was_issued_issues_no_paid_command}` (12 each) — the second exposed a real defect (a paid originate after revocation), now fixed |
| Participant reassigned while ringing | `cross_service_races::reassignment_while_the_former_participant_rings_leaves_no_authority_behind` (12, three orders) |
| Two workers, same provider event | `provider_events_are_deduplicated_by_provider_and_event_id`, `two_reconcilers_on_the_same_object_make_exactly_one_repair` |
| Two workers, same relay number | `relay_numbers::one_remaining_number_goes_to_exactly_one_session_and_replays_are_idempotent`, `relay_load` (64 contenders, 8 numbers → 8 assignments) |
| End vs transfer | `enterprise_transfer::transfer_races_leave_one_owner` |
| Failover vs late primary success | `failover::a_late_primary_success_suppresses_the_secondary` |
| Usage finalization concurrent / after crash | `cross_service_races::usage_finalization_after_a_crash_and_under_concurrency_records_one_fact` (12), `missing_usage_finalization_is_completed_exactly_once` |

Barriers and row locks, not sleeps, order the racing steps.

## 10. §107 Failure injection — PASS

PostgreSQL unavailable at decision points (`app_admission::admission_fails_closed_when_authoritative_state_is_unreadable`,
`a_database_failure_mid_routing_leaves_no_effect`, `the_database_failing_at_the_transmit_fence_means_nothing_is_sent`,
`degradation_matrix::unprovable_spend_authority_refuses_before_any_paid_command`, Privacy
Relay `failures_never_produce_an_authoritative_mutation_or_a_plaintext_fallback`); realtime
down; event transport down (`webhooks::transport_outage_keeps_the_outbox_authoritative`,
`enforcement::revocation_commits_even_when_calling_is_down_and_is_retried_until_applied`);
LiveKit down; SIP edge down (`sip_edge_outage::*` N, `telephony_down_degrades_only_sip_paths`);
carrier down (`failover::*`, `public_failover::*`); callback lost (`reconciliation::*`);
Contact Vault down (`degradation_matrix::contact_vault_unavailable_holds_the_originate_before_any_paid_command`,
`the_kill_switch_and_missing_configuration_fail_closed`); object storage down
(`storage_outage_keeps_the_artifact_recoverable`, `deletion_survives_a_storage_outage_with_one_incident`).

## 11. §108 Load and capacity

Environment for every figure: developer container, 4 vCPU, PostgreSQL 16.15 on the same
host, **debug** builds, harness pool of 20 connections, measured inside the final full
test runs (other tests of the same binary run in parallel). These are control-plane correctness-under-load measurements, **not**
production capacity. Error rate was 0 in every scenario. `lock_waits` = sum of waiting-lock
observations over 5–10 ms samples (not distinct waits).

| Scenario (service) | Concurrency | Ops | Throughput /s | p50 / p95 / p99 ms | Max DB conns | Worker lag ms |
|---|---|---|---|---|---|---|
| `call_create_authorize_sequential` (Calling) | 1 | 40 | 1.5 | 678 / 890 / 1070 | 4 | — |
| `call_create_authorize_concurrent` (Calling) | 16 | 80 | 3.4 | 3395 / 8630 / 10390 | 20 | — |
| `ringing_fanout_concurrent` (Calling) | 8 | 30 | 1.0 | 7734 / 11407 / 12372 | 12 | — |
| `callback_relay_concurrent` (Calling) | 8 | 40 | 1.4 | 5348 / 6847 / 8459 | 12 | — |
| `outbox_backlog_build` (Calling) | 8 | 60 | 1.4 | 5700 / 9175 / 13323 | 11 | — |
| `outbox_backlog_drain` (Calling) | 1 | 1860 | 202.0 | 4 / 7 / 7 | 11 | 9206.3 |
| `privacy_relay_session_reads` (Calling) | 16 | 200 | 19.0 | 473 / 2947 / 3542 | 19 | — |
| `extension_call_sequential` (Calling) | 1 | 20 | 1.6 | 330 / 572 / 576 | 6 | — |
| `extension_call_concurrent` (Calling) | 20 | 20 | 3.5 | 3880 / 5313 / 5586 | 20 | — |
| `history_page_25` (Calling) | 1 | 20 | 23.2 | 40 / 53 / 54 | 20 | — |
| `ivr_call_and_transition` (Calling) | 1 | 20 | 1.8 | 479 / 607 / 755 | 9 | — |
| `business_hours_evaluation` (Calling) | 1 | 100 | 35.6 | 26 / 47 / 51 | 9 | — |
| `queue_insert_burst` (Calling) | 60 | 60 | 3.5 | 15879 / 16337 / 16823 | 20 | — |
| `queue_assignment` (Calling) | 1 | 12 | 2.6 | 118 / 157 / 157 | 20 | — |
| `ring_group_fanout_call` (Calling) | 1 | 10 | 1.4 | 614 / 1127 / 1127 | 8 | — |
| `context_session_create` (Privacy Relay) | 16 | 400 | 39.6 | 317 / 944 / 1474 | 20 | — |
| `protected_authorization` (Privacy Relay) | 32 | 1000 | 133.7 | 200 / 485 / 630 | 20 | — |
| `relay_number_allocation_contention` (Privacy Relay) | 64 | 64 | 127.5 | 252 / 455 / 494 | 20 | — |
| `outbox_drain` (Privacy Relay) | 4 | 938 | 743.3 | 479 / 522 / 522 | 20 | — |
| `leg_setup_intake` (Telephony) | 50 | 200 | 38.6 | 626 / 1942 / 2013 | 20 | 0 |
| `command_execution` (Telephony) | 2 | 200 | 46.6 | 2307 / 4291 / 4291 | 20 | 4291 |
| `provider_event_ingestion` (Telephony) | 60 | 600 | 316.8 | 106 / 181 / 196 | 20 | 2550 |
| `provider_event_duplicate_dedupe` (Telephony) | 60 | 600 | 362.2 | 139 / 170 / 180 | 20 | 0 |
| `reconciliation` (Telephony) | 2 | 200 | 141.1 | 747 / 1399 / 1399 | 20 | 1399 |
| `sip_signalling_setup` (Telephony) | 20 | 20 | 7.2 | 2159 / 2716 / 2716 | 20 | 2159 |
| `sip_signalling_teardown` (Telephony) | 20 | 20 | 17.5 | 1091 / 1091 / 1091 | 20 | 1091 |

* **CONTROL-PLANE SCALE — MEASURED** (above; targets for a release build on a dedicated
  host are documented in each load test header and not asserted).
* **MEDIA SCALE — NOT CERTIFIED** (no multi-node LiveKit environment; functional real-media
  evidence only, §5).
* **CARRIER SCALE — PENDING EXTERNAL ENVIRONMENT** (no carrier; external carrier capacity
  not certified, no contractual figures assumed).

## 12. §109 Security — PASS

Part H controls unchanged and green (credential revocation, IDOR, Contact Vault crypto,
log / secret leakage, webhook signature / replay / SSRF, provider callback auth / replay,
SIP exposure / toll fraud). New: credential-guessing throttle
(`authentication::repeated_failed_authentications_are_throttled_without_locking_out_the_owner`),
secret blast-radius guard, least-privilege database roles proven against the real
migrations (Calling cannot open Privacy Relay's or Telephony's database, and so on),
RustSec gate per lockfile, image root-filesystem secret scan and Debian security-update
check. Unresolved upstream finding, documented: RUSTSEC-2023-0071 (`rsa`, no fix) is
unreachable. The Meeting Service lockfile has its own advisories (owned by that service's
workflow, outside Part III).

## 13. Defects found by this programme (all fixed, with regression tests)

* **Paid originate after revocation** (Calling): a context revoked after the Contact
  Resolution Grant was issued, but before the originate was sent, still produced a paid
  leg that no TERMINATE would end. Fixed: the attempt is recorded only for a still-active
  Call under a held claim, and the TERMINATE path never declares a leased originate dead
  (`cross_service_races::context_revoked_after_the_grant_was_issued_issues_no_paid_command`).
* **Commit-then-enqueue of TERMINATE** (Calling): fixed by the deferred trigger (Part I §81).
* **Array JSON bodies accepted positionally** (Calling): refused by a body-shape
  middleware (`api_authorization_matrix::request_bodies_must_be_json_objects`).
* **Body validated before scope** on two routes (Calling): scope checked first.
* **Lock-order deadlock** between Telephony capacity reservation and event ingestion,
  found under SIP load: `FOR NO KEY UPDATE`
  (`failover::capacity_reservation_is_not_blocked_by_a_legs_foreign_key_locks`).
* **Unbounded retries**: Telephony reschedule paths, the Telephony outbox and Privacy
  Relay enforcement now have caps and dead letters (Part I §81).
* CI: a planner assertion on a 40-row table chose a sort with fresh statistics; the test
  now asserts the property the history index exists for (an ordered page with no sort);
  the new image secret scan flagged GnuTLS's public FIPS self-test keys; the scanner now
  skips only package files verified unmodified against the dpkg manifests.

## 14. GitHub Actions

Recorded in a documentation-only follow-up commit once the runs of the final code commit are terminal.

## 15. §110 Production acceptance gates

```
ARCHITECTURE GATE                  ✅
DEVELOPER / CONSUMER GATE          ✅
CALLING / RTC GATE                 ✅
PRIVACY GATE                       ✅
TELECOM GATE                       🟡  PENDING — no lawful carrier interconnect (software + CF-owned edge PASS)
PROVIDER INDEPENDENCE GATE         ✅
ENTERPRISE GATE                    ✅
FRAUD / SPEND GATE                 ✅
RELIABILITY GATE                   ✅
SCALE GATE                         🟡  control plane measured; media scale not certified; carrier scale pending
SECURITY GATE                      ✅
OPERATIONS GATE                    ✅
ROLLBACK GATE                      ✅
DOCUMENTATION GATE                 ✅
```

* Architecture: boundaries enforced by the guard (no cross-service source / database /
  secret coupling); consumer APIs provider-independent (contracts).
* Developer / consumer: generic tenant / application credentials, scopes and versioned
  contracts serve first-party and external-style consumers (`first_party_external::*`,
  contract suites).
* Provider independence: the deterministic and SIP-trunk adapters pass the same provider
  contract suite; disabling a provider is an audited operator action that changes no
  public contract.
* Operations: metrics, incidents with audited resolve, runbooks, backup / restore
  exercises, provider-outage procedure (Part I §86–§88).
* Rollback: every capability switch disables new work only (`feature_rollback::*`).
* Documentation: OpenAPI / event contracts validated, ADR 0031, Part I / J / K reports.

**FULL PART III PRODUCTION ACCEPTANCE — PENDING** until §104 passes on a real lawful
interconnect and the Telecom and Scale gates turn green.
