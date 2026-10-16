# Part L — A–Z implementation programme and final closure register (§§111–140)

Status: **PART III SOFTWARE IMPLEMENTATION — COMPLETE; PART III SOFTWARE CERTIFICATION —
PASS.** **FULL PART III PRODUCTION ACCEPTANCE — PENDING** (real lawful carrier
interconnect, carrier scale, media-plane scale). This register is the authoritative Part
III closure record. It does not replace the Part F–K certification reports; it maps the
original A–Z workstreams onto them and onto the final tree.

## A. Scope and methodology

Part L is an audit, not a feature programme. For every §111–§140 requirement the final
repository was checked against code, migrations, tests, contracts, ADRs, CI runs,
certification reports and operations documentation, and classified:

| Status | Meaning |
|---|---|
| PASS — EXISTING | Implemented and certified before Part L; evidence re-verified on the final tree |
| PASS — HARDENED | A gap found by Part L and fixed in Part L (none in software; see §L for documentation) |
| PENDING — EXTERNAL | Software complete; the remaining evidence needs an external party (carrier, environment) |
| SUPERSEDED — PROCESS ONLY | A historical workflow step replaced by current repository governance; its intent is verified instead |
| FAIL — SOFTWARE GAP | Requirement absent or wrong in software (**none found**) |

Method:

1. Closure mapping (§C): implementation and test evidence for each workstream. Every
   cited test function was confirmed to exist in source; every `suite::test` reference in
   the Part F–K reports was checked mechanically against the code (all resolve; the only
   non-literal references are wildcards and prefixes that name existing tests).
2. Delta: **no software gap was found, so no production code, configuration, migration
   or contract changed in Part L.** Documentation that contradicted the final tree was
   corrected, and the missing provider-outage runbook was written (§L).
3. Certification: existing GitHub Actions evidence was re-verified run by run (workflow,
   run id, head SHA, branch, status, conclusion) and its relevance to the final code was
   proven by path-filter analysis (§I). Expensive certifications were not re-run because
   no code or configuration changed.
4. Local gates on the starting tree: `make service-boundaries` (66 mutation tests OK,
   guard OK, operations registry OK) and `make secret-scan` (12 mutation tests OK,
   repository clean).

Historical process steps (merge an older baseline into `main`, a Part III branch, a
phase-labelled documentation directory, merge at closure) are **SUPERSEDED — PROCESS
ONLY**. All Part III work lives on `staging`; nothing was merged into `main`.

## B. Commits

| | SHA |
|---|---|
| Starting `staging` | `13fddd054e8a40aa94fde8d5f4ce6b585f4dcebc` |
| Final code SHA (unchanged by Part L) | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| `main` observed at Part L start (not modified) | `68d93fa27d3a052e6f16b80b7d68b0e3e6e32ec9` |
| Final `staging` | the Part L documentation commit that adds this file |

`a019d21 → 8b830e3` changed only `scripts/secret-scan/*`, one Calling test file and the
Part K report; `8b830e3 → 13fddd0` changed only the Part K report.

## C. A–Z closure matrix

Test names are `suite::test` in the owning service's `tests/` unless noted.

| WS | Responsibility | Implementation evidence | Certification evidence | Status | Remaining blocker |
|---|---|---|---|---|---|
| **§111** | Delivery principles: platform not consumer-specific; Privacy Relay not consumer infrastructure; telecom guarded by authorization, spend, fraud, destination, concurrency, provider-failure, unknown-outcome and reconciliation controls; flags; measurability; no consumer inside CF Meet authority | Generic tenant / application model (ADR 0003); consumer-neutral contexts (ADR 0010); layered authorization (ADR 0029); switches (§95, ADR 0031); bounded metrics | `architecture_boundaries::platform_core_has_no_consumer_specific_code`, Privacy Relay `architecture::privacy_relay_has_no_consumer_specific_model`, `authorization_layers::*`, `feature_rollback::*`, `metric_safety::*` | **PASS — EXISTING** | — |
| **A** | Baseline / repository | `services/{calling,privacy-relay,telephony}`, `packages/contracts`, `infra/telephony`; no Part III business code in `services/meeting-service`; `service-data-ownership.md` lists Calling, Privacy Relay, Telephony as Active | `architecture_boundaries::{calling_does_not_depend_on_meeting_service, calling_never_touches_meeting_service_tables, path_dependencies_are_own_crates_or_shared_packages_only}`; `make service-boundaries` | **PASS — EXISTING**; old branch / merge / path steps **SUPERSEDED — PROCESS ONLY** | — |
| **B** | Tenant / application / environment | Calling migration `m20261007_000001_platform_consumers` (one tenant model, FIRST_PARTY / EXTERNAL; SANDBOX / TEST / PRODUCTION; DB CHECK `kind='PRODUCTION' OR live_telecom_allowed=FALSE`; immutable ownership); scope columns + composite FKs on calls, events, outbox, usage, authorizations, audit, limits; paid paths re-read live-telecom eligibility (`calls/app/public/outbound.rs`) | `first_party_external::first_party_and_external_consumers_share_one_model_and_path`, `environment_isolation::{non_production_environments_can_never_allow_live_telecom, sandbox_credential_cannot_become_production_through_the_request}`, `call_authorization::sandbox_calls_never_pass_the_paid_public_network_guard`, `tenant_isolation::calls_are_confined_to_their_tenant_application_and_environment`, `limits::limits_cannot_be_supplied_by_the_client` | **PASS — EXISTING** | — |
| **C** | Credentials / service trust | `platform/{credentials,verifier,authentication,scopes}.rs` (HMAC-SHA-256 verifier under a versioned pepper, one-time secret, expiry, revoke, rotate with overlap); `packages/service-auth` (Ed25519 / EdDSA JWS, audience, runtime environment, ≤300 s, jti, request / correlation ids, receiver-side trust policy, delegated context) | `credential_storage::{raw_secret_is_never_persisted_only_a_keyed_verifier, creation_result_contains_the_secret_exactly_once}`, `authentication::{expired_and_not_yet_valid_credentials_fail, revoked_credential_fails_immediately_and_revocation_is_idempotent}`, `rotation::old_and_new_are_valid_during_overlap_then_only_new`, service-auth `authentication::{c_tokens_work_only_for_their_exact_audience, d_tokens_never_cross_runtime_environments, h_customer_credentials_are_never_service_credentials}`, Privacy Relay `internal_auth::scope_comes_only_from_the_signed_delegated_context` | **PASS — EXISTING** | — |
| **D** | Calling domain / persistence | `src/calls/*` (Call, Participant, Session, Leg, Route, Attempt, Authorization, Event, Quality, Usage); `transitions.rs::plan_transition`; migrations 000005–000009 (transition + terminal-guard triggers, append-only `call_events`, no-delete triggers); event + outbox row in one transaction; Telephony / Privacy Relay hold no Call state | `call_state_machine::{database_graph_equals_the_rust_graph_for_every_pair, terminal_calls_can_never_be_resurrected_in_rust_or_by_direct_sql}`, `call_evidence::{state_changes_commit_with_their_event_and_outbox_row_or_not_at_all, call_events_are_append_only_gap_free_and_faithful}`, `call_api::{foreign_calls_are_indistinguishable_from_missing_ones, keyset_pagination_is_stable_without_duplicates_or_gaps}`, `trace_correlation::*` | **PASS — EXISTING** | — |
| **E** | Authorization / policy | `call_authorizations` (decision, reason, policy version + bounded snapshots, one effective decision per Call); `calls/app/blocks.rs`; `routing/compliance.rs`; spend / velocity (`telephony/spend.rs`, `call_spend_reservations`); `platform/limits.rs`; Privacy Relay `relay_authorization_decisions` | `authorization_layers::{f_a_direction_the_context_forbids_is_refused, g_a_blocked_destination_is_refused_before_privacy_relay_and_spend, h_exhausted_spend_is_refused_before_any_telephony_command}`, `compliance::a_required_country_without_a_record_is_denied_before_any_paid_action`, `public_spend::velocity_per_principal_service_account_and_destination`, `call_authorization::decisions_persist_safe_evidence_and_exactly_one_is_effective` | **PASS — EXISTING** | — |
| **F** | Routing engine | `routing/engine.rs` (pure `select` over provider-independent inputs; classes APP_RTC, APP_TO_PSTN, PSTN_TO_APP, PSTN_RELAY, CALLBACK_RELAY, ENTERPRISE_INTERNAL; app-first; health / capacity / cost; unknown evidence never invented); circuits in Telephony `routing.rs`; ADR 0013 / 0017 | `routing::engine` unit tests `{app_rtc_wins_when_every_target_is_reachable, unknown_origination_blocks_failover_until_reconciled, plans_never_contain_numbers_or_provider_names}`, `public_failover::{a_definite_provider_failure_fails_over_to_a_new_attempt, unknown_never_fails_over_but_proven_absence_does}`, `routing_engine::identical_route_selection_never_creates_a_second_active_route` | **PASS — EXISTING** | — |
| **G** | App-to-app RTC | `src/media/provider.rs` (provider-neutral port), `media/livekit.rs` (self-hosted LiveKit adapter), `media/credentials.rs` (TTL 30–900 s), `media/permissions.rs`, `calls/app/media.rs` (DB-read admission, stale generation / terminal Call refused), `media/reconciliation.rs`; no Meeting Service dependency | `rtc_certification::real_livekit_certification` (real LiveKit in CI, `CALLING_RTC_CERT_REQUIRED=1`), `app_media::*`, `app_calls::video_calls_grant_camera_and_removed_sessions_lose_access` | **PASS — EXISTING** (REAL RTC PASS, run 38767666636) | — |
| **H** | Realtime / multi-device ringing | `src/signaling/*` (PostgreSQL authority, signals after commit, recovery through `GET /v1/calling/invitations`), `signaling/push.rs` + `calling/push/invitation/v1` contract; single-winner unique constraints (migration 000011) | `app_races::{concurrent_accepts_have_exactly_one_winner, the_single_winner_constraint_is_the_database_guarantee}`, `app_ringing::{one_device_decline_declines_only_that_offer, group_calls_have_one_winning_device_per_participant}`, `signaling::a_disconnected_endpoint_recovers_from_the_api`, `app_calls::cancel_during_ringing_stops_everything_and_late_accept_fails` | **PASS — EXISTING** | — |
| **I** | Communication Context | Privacy Relay migrations 000001–000002 (opaque `external_context_reference` unique per scope, version moves by exactly one), `context/service.rs` (create / update with `ContextVersionConflict` / complete / revoke / expire) | `contexts::{valid_contexts_are_created_at_version_one, one_context_per_external_reference}`, `lifecycle::revoke_is_terminal_and_cascades`, `expiry::concurrent_workers_expire_each_context_and_session_exactly_once`, `reassignment::stale_updates_conflict_and_never_restore_removed_authority` | **PASS — EXISTING** | — |
| **J** | Privacy Relay | migration 000003 (policy snapshot, grace, session bounded by its context), `PRIVACY_RELAY_ENABLED` kill switch (removing authority never gated), enforcement jobs (000005) + Calling `privacy_relay/enforcement.rs`; protected calls opt-in only | `lifecycle::kill_switch_denies_new_authority_but_never_blocks_removing_it`, `reassignment::reassignment_keeps_the_context_and_revokes_the_removed_participant_immediately`, Calling `privacy_enforcement::revocation_cancels_ringing_and_ends_connected_protected_calls_once`, `enforcement::revocation_commits_even_when_calling_is_down_and_is_retried_until_applied`, `cross_service_races::reassignment_while_the_former_participant_rings_leaves_no_authority_behind` | **PASS — EXISTING** | — |
| **K** | Contact Vault | migrations 000006–000008 (ciphertext + nonce, masked display, country, capability, key version; keyed fingerprints; verification, resolution grants, access audit); `contact_vault/crypto.rs` (AES-256-GCM, scope-bound AAD); keyrings outside the database | `contact_vault::{a_number_is_canonicalized_encrypted_and_only_ever_returned_masked, encryption_key_rotation_reencrypts_in_batches_without_changing_any_contact}`, `contact_crypto_scope::a_ciphertext_copied_across_tenant_application_environment_or_contact_never_opens`, `contact_resolution::resolution_permission_is_receiver_authoritative_and_telephony_only`, `contact_logging::no_number_token_or_key_reaches_the_logs`, `disaster_recovery::a_restored_privacy_relay_database_resumes_safely_and_fails_closed_without_keys` | **PASS — EXISTING** | — |
| **L** | Relay-number capability | migrations 000009–000011 (opaque `provider_binding_ref`, partial unique holder index over ACTIVE / COOLDOWN, one assignment per session, inbound resolutions); `relay_numbers/*`; no Part IV Digital Communications Number lifecycle (guard `NUMBERING_IN_PRIVACY_RELAY`, `FUTURE_SERVICE_IMPLEMENTED`) | `relay_numbers::{one_remaining_number_goes_to_exactly_one_session_and_replays_are_idempotent, a_released_number_cools_down_before_anyone_else_gets_it, neither_the_number_nor_its_provider_binding_leaves_the_inventory}`, `relay_inbound::{every_inactive_mapping_is_the_same_generic_refusal, a_delayed_callback_never_reaches_the_next_session}` | **PASS — EXISTING** | Part IV numbering out of scope |
| **M** | Telephony core | `services/telephony/src/{legs,commands,bridges,usage}`; migrations 000002–000004; Part I fix: deferred constraint trigger `trg_calls_release_public_legs` (Calling migration 000043) writes TERMINATE for every live paid leg in the transaction of the terminal transition; revocation race fix: the attempt is recorded only for a still-active Call under a held claim (`calls/app/public/dispatch.rs`) | `durable_workers::the_terminate_intent_commits_with_the_terminal_transition_and_survives_a_crash`, `cross_service_races::context_revoked_after_the_grant_was_issued_issues_no_paid_command`, `telephony_core::{duplicate_commands_have_one_provider_effect, a_rolled_back_transaction_leaves_no_leg_command_or_outbox_intent, every_leg_state_pair_agrees_between_the_rust_graph_and_the_database}` | **PASS — EXISTING** | — |
| **N** | SIP / telecom edge | ADR 0015 Asterisk + ARI; `infra/telephony/asterisk/pjsip.conf` (`endpoint_identifier_order = ip`, TLS 1.2 transport, no anonymous endpoint, RFC 4733), `extensions.conf` (Stasis only; dialplan never dials); `sip/{ari,peer_config}.rs`; no Asterisk / ARI / PJSIP term in public contracts | `sip_certification::real_sip_media_edge_certification` (A–H incl. DTMF, hold, bridge, transfer, edge-down; Asterisk + SIPp, `TELEPHONY_SIP_CERT_REQUIRED=1`), `edge_config::the_production_edge_identifies_endpoints_by_ip_only`, `sip_ingress::a_source_outside_the_peers_networks_is_rejected_with_an_incident`, `destination_classes::*` | **PASS — EXISTING** (CF-owned edge; **not** carrier certification) | — |
| **O** | Provider abstraction / first adapter | `providers/mod.rs` (`TelecomInterconnectProvider`, registry, file-based secrets with `.previous` rotation, test adapter refused in staging / production), `providers/sip_trunk.rs`, `providers/deterministic.rs`; vendor tokens banned outside `src/providers/`. No Africa's Talking code (ADR 0016 names no carrier) | `provider_contract::{adapters_advertise_capabilities_and_health_in_the_neutral_vocabulary, a_lost_response_is_unknown_never_failed_and_read_back_finds_the_truth, callbacks_are_authenticated_before_parsing_and_parsed_strictly}`, `architecture::provider_and_sip_stack_specifics_stay_in_their_directories` | **SOFTWARE PROVIDER ADAPTER CONTRACT — PASS — EXISTING**; **FIRST REAL NIGERIAN INTERCONNECT — PENDING — EXTERNAL ONBOARDING** | Carrier onboarding (see §H) |
| **P** | Inbound public calling | Telephony `inbound.rs` (peer validation, `SIP_PEER_REJECTED`, `resolve_inbound`), Calling `calls/app/public/inbound.rs` (one PUBLIC_INBOUND Call ringing endpoints through the ordinary path), Privacy Relay `relay_numbers/inbound.rs`; contract `telephony/inbound/v1` | `public_phone_to_app::{a_phone_calls_a_relay_number_and_an_app_user_answers, inbound_calls_are_refused_without_an_owner_authority_or_from_another_service}`, `sip_ingress::unbound_numbers_and_unknown_peers_are_rejected_as_sip_peer_rejected`, Privacy Relay `incidents::an_ambiguous_relay_mapping_is_refused_and_raises_one_deduplicated_incident` | **SOFTWARE PASS — EXISTING**; real carrier **PENDING — EXTERNAL** | No lawful inbound number |
| **Q** | Callback relay | `calls/app/public/{callback,events,bridge}.rs` (leg A, answer triggers leg B, bridge, hang-up propagation, busy / no-answer / decline mapping), `telephony/spend.rs` | `public_callback_relay::{the_requester_is_called_first_then_the_callee_then_both_are_bridged, a_failed_second_leg_releases_the_first, the_requester_hanging_up_first_cancels_without_dialing_the_callee, concurrent_identical_callback_requests_create_one_call_and_one_paid_leg}` | **CALLBACK SOFTWARE / SIMULATION / SIP EDGE — PASS — EXISTING**; **REAL LOW-CONNECTIVITY CARRIER CALLBACK — PENDING — EXTERNAL** | No lawful carrier |
| **R** | Provider events / unknown outcomes / reconciliation | Telephony `events/ingest.rs`, `reconciliation/mod.rs` (leases, version-checked repair, escalation), `workers/mod.rs` (periodic sweep, health refresh), `incidents/mod.rs`; DB trigger `trg_telephony_commands_no_blind_retry` | `provider_events::{a_callback_signed_with_the_previous_secret_is_accepted_during_the_overlap, provider_events_are_deduplicated_by_provider_and_event_id, out_of_order_events_never_regress_and_late_success_never_resurrects}`, `unknown_outcome::timeout_after_send_is_unknown_and_never_blindly_retried`, `reconciliation::{connected_but_absent_at_the_provider_is_not_guessed, two_reconcilers_on_the_same_object_make_exactly_one_repair}`, `real_interconnect_certification` prints NOT RUN | **PASS — EXISTING** | Real-carrier event behaviour pending with the carrier |
| **S** | Call records / evidence | Calling migrations 000005 / 000007 / 000008 (ordered timestamps, normalized `failure_category` / `terminal_reason`, opaque context ref, append-only events, bounded quality records); `privacy_relay/evidence.rs` (Part D §36 support / dispute evidence: timing, attempts, route class, normalized outcomes, participants, decisions); `GET /v1/calls/{id}`, `GET /v1/calling/history`, `GET /v1/usage`; retention / minimization (ADR 0030) keeps evidence | `call_records::{attempts_are_numbered_linked_and_unknown_is_not_failure, routes_are_provider_independent_decisions_with_bounded_snapshots, quality_records_are_bounded_numeric_and_append_only}`, `call_evidence::events_and_outbox_never_carry_secrets_contacts_or_media_identities`, `privacy_evidence::evidence_is_composed_from_calling_records_with_authoritative_timing`, `retention::terminal_calls_are_minimized_after_the_bound_and_their_evidence_is_kept` | **PASS — EXISTING** (observation O-1) | — |
| **T** | Usage / cost / limits | Calling migration 000032 `ck_call_usage_records_cost` (UNKNOWN ⇒ NULL amount and currency; KNOWN / ESTIMATED ⇒ amount ≥ 0 + ISO currency), `uq_call_usage_records_idempotency`, `uq_call_usage_records_one_final_fact` (000043); `LegCost {Known, Estimated, Unknown}`; Telephony per-leg usage unique; spend reservations carry principal / service account / credential / context (internal allocation); contract `platform/usage/v1` with counter-example `usage-unknown-cost-with-zero` | `call_records::a_different_idempotency_key_can_never_bill_the_same_subject_twice`, `public_spend::{an_unknown_cost_never_opens_an_unreserved_or_unbounded_paid_leg, estimated_cost_is_reserved_and_actual_cost_settled_without_pretending}`, `cross_service_races::usage_finalization_after_a_crash_and_under_concurrency_records_one_fact`, Telephony `paid_safety::the_call_duration_limit_is_enforced_durably` | **PASS — EXISTING** | — |
| **U** | Fraud / abuse / spend | `telephony/spend.rs` (per party / principal / service account / context, daily / monthly), `routing/compliance.rs`, `calls/app/{blocks,abuse_reports}.rs`, Telephony `destinations.rs` / `abuse.rs`, circuits; Calling migrations 000034–000037 | `public_spend::velocity_per_principal_service_account_and_destination`, `destination_classes::standard_proceeds_and_premium_and_high_risk_are_refused_before_any_provider_action`, `authorization_layers::a_suspended_tenant_creates_nothing_but_may_end_a_live_call`, `blocks::repeated_blocked_attempts_raise_exactly_one_incident_per_window`, `degradation_matrix::unprovable_spend_authority_refuses_before_any_paid_command` | **PASS — EXISTING** | — |
| **V** | Voicemail / continuity | Calling migrations 000026 / 000027 / 000041; `calls/app/enterprise/{voicemail,orphans}.rs`, `enterprise/{voicemail,hours,forwarding}.rs`; `packages/object-storage`; app-first fallback policy (`routing/policy.rs`) | `enterprise_voicemail::{retrieval_is_authorized_and_audited, voicemail_access_decisions_are_audited_without_revealing_existence, deletion_and_retention, voicemail_capture_objects_are_never_left_behind}`, `call_history::a_missed_call_survives_push_and_realtime_outage`, `enterprise_hours_forwarding::{schedules_route_open_and_closed_hours_on_one_call, forwarding_loops_are_impossible}` | **PASS — EXISTING** | — |
| **W** | Enterprise calling | Calling migrations 000020–000026, 000030; `src/enterprise/*`, `calls/app/transfer.rs`; ADRs 0018–0022, 0026 | `enterprise_sip_certification` (real Asterisk + SIPp + S3, `ENTERPRISE_SIP_CERT_REQUIRED=1`), real-LiveKit enterprise sections, `enterprise_queues::concurrent_dispatchers_never_double_offer`, `enterprise_ivr::menu_routes_a_valid_key_after_an_invalid_one`, `enterprise_transfer::{hold_resume_and_transfer_to_an_extension_keep_one_call, failed_transfers_recover_without_orphans}`, `enterprise_certification::org_isolation_certification` | **PASS — EXISTING** | — |
| **X** | Observability / operations | Bounded per-service metrics; correlation id on the Call and every command (000043); incidents deduplicated while OPEN with audited resolve (all three admin CLIs); route health / answer ratio; backlog gauges; backup / restore (`infra/deployment/backup/`); runbooks; switches (§95) | `metric_safety::{every_metric_call_uses_allowed_keys_and_bounded_values, the_rendered_registry_never_carries_identifiers}`, Telephony `paid_safety::metrics_are_bounded_and_never_carry_identifiers`, `trace_correlation::one_public_call_is_linked_end_to_end_by_call_id_and_request_correlation`, `durable_workers::the_worker_backlog_is_sampled_and_an_old_backlog_raises_one_platform_incident`, `disaster_recovery::*` (each service) | **PASS — HARDENED (documentation)**: the carrier-outage runbook was referenced but missing; written as [`provider-outage-runbook.md`](provider-outage-runbook.md) | Observation O-2 |
| **Y** | Certification | Part F–K reports; CI workflows (§I) | Unit / integration / authorization (`api_authorization_matrix`, 81 mutable operations), real RTC, SIP edge, Privacy Relay, Contact Vault, enterprise, races, failure injection, control-plane load, DR, boundaries, secret scan, RustSec + image scan — all green | **PASS — EXISTING**; **REAL LAWFUL PUBLIC-NETWORK CERTIFICATION — PENDING — EXTERNAL CARRIER CREDENTIAL / INTERCONNECT BLOCKER** | Carrier |
| **Z** | Closure | No open Severity-1 / critical software defect (Part K §13: all found defects fixed with regression tests); CI green on the final code SHA; API / event contracts validated; ADRs 0001–0031 indexed; rollback = switches + image rollback (`infra/deployment/README.md`, `feature_rollback::*`) | §I | **Z-SOFTWARE-CLOSURE — PASS**; **Z-REAL-CARRIER-CLOSURE — PENDING**; **Z-MERGE — DEFERRED — USER AUTHORIZATION REQUIRED**; **Z-PRODUCTION-DEPLOYMENT — NOT PART OF THIS TASK** | Carrier; user authorization |

### Observations (non-blocking, recorded for review)

* **O-1 (S, quality and attempt read surface).** `call_quality_records` are durable,
  bounded and append-only, and signalled by the `CALL_QUALITY_RECORDED` event (record id
  and metric count), but no HTTP route, webhook payload or admin command returns their
  values. Attempt and route-class evidence is retrievable through the support / dispute
  evidence of Privacy-Relay-protected Calls only; for ordinary Calls the public read is
  `GET /v1/calls/{id}` (lifecycle, timing, normalized failure, participants, sessions),
  history and usage. ADR 0006 §2 scopes quality records as persistence primitives ("no …
  analytics"), so this is not a Part III requirement gap. A customer / operator read of
  quality and attempts for every Call is a product follow-up candidate.
* **O-2 (X, public-network quality).** As decided in ADR 0031 §5, public-network quality
  is route-level (answer ratio, response classes); no per-leg RTCP jitter / loss record is
  produced and none is invented. RTC quality is the end-of-call summary from admitted
  sessions, reconnects and connected participants.
* **O-3 (T).** The database admits a `LEG_CONNECTED_TIME` usage row with `cost_status`
  NULL; the only writer always sets it, and NULL is never zero (no zero-cost fact
  can arise). A NOT NULL constraint would be optional hardening.
* **O-4 (docs in code).** The `telephony-admin` module comment writes `circuit --state
  OPEN`; the CLI takes the state positionally (`provider circuit <id> OPEN`). The runbook
  uses the correct form; the comment was left unchanged to keep Part L free of code
  changes.

## D. §138 stop-condition matrix

| # | Stop condition | Result | Evidence |
|---|---|---|---|
| 1 | Calling depends on Meeting Service tables | **NOT PRESENT** | Guard `CROSS_SERVICE_TABLE_ACCESS` (ownership derived from migrations; meeting-service 40 tables); `architecture_boundaries::calling_never_touches_meeting_service_tables`; no meeting table name in Part III SQL |
| 2 | Calling imports Meeting Service source | **NOT PRESENT** | Calling path dependencies: own `migration` + `packages/{service-auth,object-storage,webhooks}`; guard `CARGO_CROSS_SERVICE_DEPENDENCY`, `CROSS_SERVICE_SOURCE_INCLUDE`; `architecture_boundaries::calling_does_not_depend_on_meeting_service` |
| 3 | Privacy Relay depends on consumer business tables | **NOT PRESENT** | Contexts use an opaque external reference; Karryam / HealthNet appear only in deny-list tests (`vocabulary.rs` test block, `tests/architecture.rs`); `architecture::privacy_relay_has_no_consumer_specific_model` |
| 4 | A consumer integrates directly with a telecom provider | **NOT PRESENT** | `apps/web` and the Meeting Service reference no telecom provider; provider code exists only in `services/telephony/src/providers/` (`architecture::provider_and_sip_stack_specifics_stay_in_their_directories`) |
| 5 | Raw private numbers in ordinary Calling / public contracts | **NOT PRESENT** | Calling / platform schemas and OpenAPI have no phone / E.164 field; envelope `propertyNames` refuse `phone|msisdn|e164`; counter-examples (`phone-field-in-create`, `transfer-to-phone-number`); guard `PLAINTEXT_CONTACT_FIELD`; E.164 appears only in Contact Vault input, Telephony-only resolution and the platform-owned relay inventory |
| 6 | Provider-specific concepts in public API / event contracts | **NOT PRESENT** | No LiveKit / Asterisk / ARI / PJSIP / Twilio / Africa's Talking term in Calling or platform contracts (only an IVR dialplan counter-example); "SIP" appears only as the protocol of enterprise SIP devices; `routing::engine::plans_never_contain_numbers_or_provider_names` |
| 7 | Paid actions without idempotency | **NOT PRESENT** | `telephony_commands.idempotency_key NOT NULL`, `uq_telephony_commands_idempotency UNIQUE (requester_service, idempotency_key)`; `telephony_core::duplicate_commands_have_one_provider_effect`; `uq_telephony_legs_one_live_per_party` |
| 8 | Blind retry after unknown outcome | **NOT PRESENT** | Trigger `trg_telephony_commands_no_blind_retry` (transmitted command never back to PENDING); executor requeues only `WHERE transmitted_at IS NULL`; `unknown_outcome::timeout_after_send_is_unknown_and_never_blindly_retried`, `failover::unknown_blocks_any_second_attempt_until_reconciled` |
| 9 | Contact Vault plaintext outside narrow execution | **NOT PRESENT** | `expose_e164` is `pub(crate)` and zeroized, used only for crypto, fingerprint and Telephony-only resolution; guard `CONTACT_VAULT_OUTSIDE_OWNER`; `contact_resolution::resolution_permission_is_receiver_authoritative_and_telephony_only`, `architecture_boundaries::calling_never_handles_raw_contact_values` |
| 10 | Cross-service database access | **NOT PRESENT** | Guard `CROSS_SERVICE_DATABASE_URL`, `CROSS_SERVICE_DB_HELPER`, `FOREIGN_SECRET_REFERENCE`; per-service roles, CONNECT revoked from PUBLIC (`verify-service-isolation.sh`, 36 checks with real migrations) |
| 11 | Another service can become Call authority | **NOT PRESENT** | Telephony and Privacy Relay have no Call-state table (Telephony refers to Calls by opaque `execution_binding_ref`); `tests/migrations.rs` and `architecture_boundaries` refuse `enterprise_calls`, `pbx_calls` and similar |
| 12 | A provider required by the public core data model | **NOT PRESENT** | `calls`, legs and routes (000005–000007) have no provider column; `provider_adapter` exists only in internal media binding / evidence tables and is never public |

Run on the starting tree: `make service-boundaries` → `service boundaries: OK — calling: 89
tables, meeting-service: 40, privacy-relay: 26, telephony: 21`; `operations registry: OK —
21 responsibilities`; 66 mutation tests OK. `make secret-scan` → `secret scan: clean`; 12
mutation tests OK.

**No stop condition is present.**

## E. §139 deliverable register

| | Deliverable | Location | Status |
|---|---|---|---|
| A | Calling source / migrations / OpenAPI / events | `services/calling/src`; `services/calling/migration` (42 registered); `packages/contracts/calling/http/v1.openapi.json`; `packages/contracts/calling/{events,realtime,push,control,enterprise,common}`, `platform/{events,webhooks,usage}` | ✅ |
| B | Privacy Relay source / Contact Vault / migrations / contracts / security docs | `services/privacy-relay/src` (`contact_vault/`); migrations (14); `packages/contracts/privacy-relay/`; ADR 0011 / 0012, Part H §73, `secret-rotation-runbook.md` §4 | ✅ |
| C | Telephony source / SIP architecture / provider abstraction / migrations / internal contracts | `services/telephony/src` (`sip/`, `providers/`, `ports.rs`); ADR 0015–0017, `infra/telephony/README.md`; migrations (15); `packages/contracts/telephony/`, `telephony-contract-inventory.md` | ✅ |
| D | Shared | `packages/contracts` (40 contracts + validator), `packages/service-auth`, `packages/webhooks`, `packages/object-storage` | ✅ |
| E | Infrastructure | `infra/livekit/`, `infra/telephony/{asterisk,certification}`, `infra/deployment/backup/{backup,restore}.sh`, `infra/deployment/postgres/{provision-service-database,verify-service-isolation}.sh` | ✅ |
| F | Controls: flags, kill switches, rollback | `services/calling/README.md` §95, Telephony / Privacy Relay READMEs and `.env.example`, Part J §95, ADR 0031, `infra/deployment/README.md` (release order, rollback); `feature_rollback::*` | ✅ |
| G | CI | `calling.yml`, `privacy-relay.yml`, `telephony.yml`, `calling-rtc-certification.yml`, `telephony-sip-certification.yml`, `enterprise-calling-certification.yml`, `contracts.yml`, `webhooks.yml`, `object-storage.yml`, `dependency-security.yml`, `service-boundaries.yml` | ✅ (§I) |
| H | Certification | Real RTC: Part K §5; SIP edge: Part K §7, Part H §75; Privacy / Contact Vault: Part K §6, Part H §73; enterprise: Part F, Part K §8; race / failure: Part K §9–§10; load: Part K §11; DR: Part I §88 + `tests/disaster_recovery.rs` | ✅ |
| I | Operations | Metrics: Part I §84; incidents: Part I §86; runbooks: [`provider-outage-runbook.md`](provider-outage-runbook.md) (Part L), `secret-rotation-runbook.md`, `security-deployment-checklist.md`; backup / recovery: `infra/deployment/README.md`, Part I §88 | ✅ (provider-outage procedure added in Part L) |
| J | Closure | This register; Part K §13–§15 | ✅ |
| — | Real-provider certification evidence | `real_interconnect_certification` prints NOT RUN | 🟡 **PENDING — EXTERNAL** (not a software defect) |

## F. §140 final Definition of Done register

| # | Capability | Result |
|---|---|---|
| 1 | App-to-app voice (real media, ringing, accept / decline, end, recovery) | ✅ **PASS** |
| 2 | App-to-app video (real media, policy-controlled video) | ✅ **PASS** |
| 3 | App-to-phone | ✅ **SOFTWARE PASS** · 🟡 **REAL CARRIER EXECUTION PENDING** |
| 4 | Phone-to-app | ✅ **SOFTWARE PASS** · 🟡 **REAL CARRIER EXECUTION PENDING** |
| 5 | Phone-to-phone relay | ✅ **SOFTWARE PASS** · 🟡 **REAL CARRIER EXECUTION PENDING** |
| 6 | Callback relay | ✅ **SOFTWARE PASS** · 🟡 **REAL CARRIER EXECUTION PENDING** |
| 7 | Generic platform integration (no Karryam / HealthNet dependency) | ✅ **PASS** |
| 8 | Contact Vault | ✅ **PASS** |
| 9 | Expiry / revocation / reassignment | ✅ **PASS** |
| 10 | Provider independence | ✅ **PASS** |
| 11 | Enterprise calling (extensions, ring groups, queues, IVR, hold / transfer, voicemail) | ✅ **PASS** |
| 12 | Fraud / spend | ✅ **PASS** |
| 13 | Reliability | ✅ **PASS** |
| 14 | Security / privacy | ✅ **PASS** |
| 15 | Operations | ✅ **PASS** (provider-outage runbook completed in Part L) |
| 16 | Scale | ✅ **CONTROL-PLANE SCALE — MEASURED** (Part K §11) · 🟡 **REAL MEDIA-PLANE SCALE — NOT FULLY CERTIFIED** (no multi-node LiveKit environment) · 🟡 **REAL CARRIER SCALE — PENDING** |
| 17 | Documentation | ✅ **PASS** (closure docs match the final tree after the Part L corrections, §L) |

## G. Acceptance-gate summary

```
PART L A–Z SOFTWARE WORKSTREAMS                 ✅ CLOSED — PASS
PART L STOP-CONDITION AUDIT                      ✅ PASS
PART L REQUIRED SOFTWARE DELIVERABLES            ✅ COMPLETE
PART L CLOSURE REGISTER                          ✅ COMPLETE

PART III SOFTWARE IMPLEMENTATION                 ✅ COMPLETE
PART III SOFTWARE CERTIFICATION                  ✅ PASS

REAL APP RTC                                     ✅ PASS
PRIVACY RELAY                                    ✅ PASS
CONTACT VAULT                                    ✅ PASS
CF-OWNED SIP EDGE                                ✅ PASS
ENTERPRISE CALLING                               ✅ PASS
FRAUD / SPEND                                    ✅ PASS
RELIABILITY                                      ✅ PASS
SECURITY                                         ✅ PASS
OPERATIONS / RECOVERY                            ✅ PASS
CONTROL-PLANE SCALE                              ✅ MEASURED

REAL LAWFUL CARRIER CERTIFICATION                🟡 PENDING
REAL PUBLIC CALL PATH CERTIFICATION              🟡 PENDING
MEDIA-PLANE SCALE                                🟡 NOT FULLY CERTIFIED
CARRIER SCALE                                    🟡 PENDING
TELECOM PRODUCTION GATE                          🟡 PENDING
FULL PART III PRODUCTION ACCEPTANCE              🟡 PENDING
```

## H. External blockers

1. **Lawful carrier interconnect.** No carrier has yet provided a lawful account /
   contract, allocated number, SIP / API connectivity, credentials, caller-ID authority,
   inbound routing and a real public network. Until then the real-carrier test reports
   NOT RUN, and app-to-phone, phone-to-app, phone-to-phone and callback relay have no
   real-carrier execution. Africa's Talking onboarding is being pursued separately; no
   carrier-specific code was added. A carrier offering SIP trunking with fixed signalling
   IP ranges is provisioned as configuration (a `SIP_TRUNK` provider, SIP peer, route and
   PJSIP trunk file, following `infra/telephony/asterisk/trunk.example.conf`). A carrier
   that requires outbound REGISTER needs an edge template (configuration); a carrier
   reachable only through a proprietary HTTP voice API would need a new adapter behind
   the existing provider contract (by design, no public contract change).
2. **Media-plane scale.** No multi-node LiveKit environment exists; only functional
   real-media evidence is certified.
3. **Carrier scale.** Depends on blocker 1 and a carrier environment; no capacity figures
   are assumed.

## I. CI evidence (verified in GitHub Actions)

Every run below: `event = push`, `head_branch = staging`, `status = completed`,
`conclusion = success`.

| Workflow | Run | Head SHA |
|---|---|---|
| Calling service | 38767666451 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Calling RTC certification (real LiveKit) | 38767666636 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Enterprise calling certification | 38767666624 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Privacy Relay service | 38767666611 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Telephony service | 38767666557 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Dependency and image security | 38767666500 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Service boundaries | 38767666288 | `8b830e34d73eaee3f2b3a0f41439337335ca3eb9` |
| Telephony SIP certification (Asterisk + SIPp) | 38764752641 | `a019d21210ecfef60f8584bc1c84549e2b1f53fc` |
| Contracts and service auth | 38764752903 | `a019d21210ecfef60f8584bc1c84549e2b1f53fc` |
| Webhooks package | 38764752642 | `a019d21210ecfef60f8584bc1c84549e2b1f53fc` |
| Object storage | 38764752773 | `a019d21210ecfef60f8584bc1c84549e2b1f53fc` |
| Service boundaries | 38768808461 | `13fddd054e8a40aa94fde8d5f4ce6b585f4dcebc` |

Relevance: the four `a019d21` runs remain valid for the final code because their path
filters (`services/telephony/**`, `infra/telephony/**`, `packages/service-auth/**`,
`packages/contracts/**`, `packages/webhooks/**`, `packages/object-storage/**` and their own
workflow files) do not match any file changed in `a019d21 → 8b830e3` (secret scanner, one
Calling test, Part K report). Part L changes only `docs/**` and `README.md`, which no code
workflow's path filter matches; only the unfiltered Service boundaries workflow runs on
the Part L commit.

Final local counts on the final code tree (Part K §1, unchanged): Calling 682 passed / 0
failed (87 binaries); Privacy Relay 168 / 0; Telephony 148 / 0; service-auth 56 (core) /
58 (client); webhooks 14; object-storage 9; contracts 17 tests, 40 contracts validated;
database isolation 36 checks; migrations Calling 42, Privacy Relay 14, Telephony 15.

## J. Final conclusion

Every A–Z software workstream is implemented and certified, no §138 stop condition is
present, and every §139 software and documentation deliverable exists. Part L found no
software gap and changed no production code, configuration, migration or contract. It
corrected documentation that contradicted the final tree and completed the
provider-outage runbook (§L). The remaining items are external: a lawful carrier
interconnect, then carrier and media-plane scale.

**PART L — SOFTWARE-COMPLETE.** **PART III SOFTWARE IMPLEMENTATION — COMPLETE; SOFTWARE
CERTIFICATION — PASS.** **FULL PART III PRODUCTION ACCEPTANCE — PENDING** until the real
lawful carrier certification (§104) passes and the Telecom and Scale gates are green.
Merge to `main` and production deployment are not part of Part L and require separate
authorization.

## K. Architectural lock

Karryam, HealthNet and future CosmicForge products are API consumers of CF Meet through
the generic tenant / application / environment model; none is part of CF Meet's internal
architecture. CF Meet public contracts depend on no named media or telecom provider
(Africa's Talking, Twilio, LiveKit, Asterisk or another). Media and telecom providers are
replaceable infrastructure edges behind Calling's media port and Telephony's provider
contract.

## L. Part L documentation changes

| File | Change |
|---|---|
| `docs/architecture/part-l-final-closure-register.md` | New: this register |
| `docs/architecture/provider-outage-runbook.md` | New: the provider / carrier outage procedure that Part I §88 referenced but that existed only as a one-line mention (detect, contain, unknown-leg handling, recover, record) |
| `docs/architecture/part-i-reliability-certification.md` | §88 playbook reference points to the runbook |
| `docs/architecture/part-k-production-acceptance.md` | §1 names the final code SHA instead of a non-existent "closure report" |
| `docs/architecture/platform-overview.md` | Privacy Relay / Telephony / packages / infrastructure status matches the tree (was pre-Part E) |
| `docs/architecture/platform-boundaries.md` | Same; adds the Privacy Relay / Telephony section, `packages/{webhooks,object-storage}`, `infra/telephony` |
| `docs/architecture/service-data-ownership.md` | Complete Privacy Relay table list; Calling's webhook / operational table families; the six guard rules missing from the rules table; stale "planned Telephony" sentence |
| `README.md` | Deployable-services table, service-evolution tree, `.env` steps and documentation links include Privacy Relay and Telephony |
