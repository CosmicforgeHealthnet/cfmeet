# Part H — Security, privacy, fraud & regulatory controls: certification report (§§71–80)

Base: `staging` at `f2361b1e3187501d422abfa6841dc6e303b80c89` (Part G closed); first Part H
pass `81d05bb` (recorded PARTIAL); final closure on the tree of this commit.

Part H is a cross-cutting closure pass over the platform built in Parts B–G: no new service,
every control lives in its existing owner (Calling, Privacy Relay, Telephony, shared packages,
infrastructure). Each row says whether the control was **I**nherited (built in Parts B–G,
audited here, unchanged), **H**ardened (existing control strengthened) or **N**ew (absent
before Part H), with the code and the exact test evidence. Decision records: ADR 0029
(security closure) and ADR 0030 (Calling retention); existing records are linked, not
duplicated: ADR 0003 / 0004 (trust, service auth), 0011 (Contact Vault), 0024 (recording),
0028 (API, webhooks, limits).

## 0. Method

Stage 0 inventoried every §71–§80 bullet against the code (authorization chain, isolation,
Contact Vault, secrets, SIP edge, callbacks, fraud, regulatory boundary, recording,
retention) and classified it COMPLETE ALREADY / NEEDS HARDENING / MISSING; only the last two
classes produced code. The first pass (`81d05bb`) was recorded PARTIAL with every gap named;
the closure pass implemented each named gap, added a dedicated test for every new
security-sensitive path, ran mutations H1–H12, and re-ran every full suite on the final tree.

## 1. Final-tree gates

Every number below comes from one complete run on the final code tree after the last code
change (no targeted re-run substitutes for it).

| Gate | Result |
|---|---|
| Calling — `fmt --check`, `check`, `clippy -D warnings`, `build` (`--workspace --locked`) | clean |
| Calling — `cargo test --workspace --locked --no-fail-fast` | **632 passed, 0 failed, 0 ignored (77 test binaries; run with the real-binary e2e enabled: `privacy_relay_e2e` 3, `telephony_e2e` 2, `enterprise_sip_certification` with `ENTERPRISE_SIP_CERT_REQUIRED=1` against a local S3-compatible store: IVR, voicemail, recording and org-isolation sections certified). Only the RTC (LiveKit) certification section prints SKIPPED locally (no LiveKit server here) and is **not** counted; CI runs it (RTC certification workflow)** |
| Privacy Relay — fmt, check, clippy, full suite | **149 passed, 0 failed, 0 ignored (35 test binaries)** |
| Telephony — fmt, check, clippy, full suite with `TELEPHONY_SIP_CERT_REQUIRED=1` (real Asterisk + SIPp; carrier RFC 4733 DTMF exercised) | **130 passed, 0 failed, 0 ignored (24 test binaries); `sip_certification::real_sip_media_edge_certification` ran and passed** |
| `packages/service-auth` (core + client features) | 114 passed, 0 failed |
| `packages/webhooks`, `packages/object-storage` | webhooks 14 passed; object-storage 9 passed |
| Contracts — `make contracts-validate`; validator tests | OK — 40 contracts, 164 examples, 255 counter-examples; validator tests 17 passed |
| Boundaries — `make service-boundaries` (guard + mutation tests) | OK — calling 88 tables, privacy-relay 24, telephony 21; 55 guard tests passed |
| Secret scan — `make secret-scan` (repository + scanner mutation tests); built binaries scanned with `--tree` | clean; 9 scanner tests passed; the nine built service binaries (`--tree`) clean, no canary number or key block |
| Migrations — each service: fresh up / status / reset / re-up (CI smoke replayed locally) + migration suites | Calling 41, Privacy Relay 13, Telephony 14 migrations: the CI migration-smoke steps (fresh up / status / reset / re-up / table checks) replayed locally and green; each service's migration suite green in its full run |
| Cross-service e2e with real binaries — Calling ↔ Privacy Relay; Calling ↔ Telephony ↔ Privacy Relay | Calling ↔ Privacy Relay 3/3, Calling ↔ Telephony ↔ Privacy Relay 2/2 (inside the final Calling run) |
| Enterprise SIP certification (Calling + Telephony + Asterisk + SIPp) | All sections ran locally and passed (IVR four branches; voicemail capture → protected upload → READY → authorized retrieval → read → delete, with denied access audited; recording consent denial, notice, bridge capture, stop, audited retrieval; 36 cross-tenant attempts → 404); CI runs the same suite against MinIO |

Baseline before Part H: Calling 546, Privacy Relay 126, Telephony 87.

## 2. §71 Authorization model — PASS

| Requirement | | Evidence |
|---|---|---|
| Credential active / environment / scope / revocation immediate | I | `platform/authentication.rs::evaluate`; `tests/authentication.rs`, `tests/rotation.rs`, `tests/scopes.rs` |
| Suspended tenant / application / environment: no new protected action, safety operations still possible | H | `AuthorityMode::SafetyOnly` + `middleware/platform_auth.rs::is_safety_route`; `authorization_layers::{a_a_suspended_tenant_creates_nothing_but_may_end_a_live_call, b_…application…, c_…environment…}`; unit `safety_allow_list_is_method_and_template_exact` |
| Principal resolved in scope, active, never by UUID alone | I + H | `principals.rs::resolve_in`; `authorization_layers::d_a_principal_of_another_environment_or_inactive_is_refused`; `tenant_isolation::principal_resolution_never_crosses_tenant_application_or_environment` |
| Principal category allow-list | N | `domain/kinds.rs`, `calls/app/create.rs`, `public/outbound.rs`; `authorization_layers::{n_a_service_account_principal_never_initiates_or_receives_an_app_call, o_a_telephone_endpoint_principal_never_initiates_an_app_or_public_call}` |
| Privacy Relay decision held by Calling; context / direction | I | `authorization_layers::{e_a_principal_outside_the_privacy_relay_context_is_refused, f_a_direction_the_context_forbids_is_refused}`; `call_authorization.rs` |
| Destination block before relay authorization and spend | N | `authorization_layers::g_a_blocked_destination_is_refused_before_privacy_relay_and_spend` |
| Spend before any Telephony command | I | `authorization_layers::h_exhausted_spend_is_refused_before_any_telephony_command`; `public_spend.rs` |
| Right to call ≠ right to record | I | `authorization_layers::i_permission_to_call_is_not_permission_to_record`; `enterprise_recording::permission_to_call_is_not_permission_to_record` |
| Enterprise admin scopes explicit | I | `authorization_layers::j_calling_authority_never_configures_the_enterprise`; `enterprise_security::calling_authority_never_implies_administration_or_artifact_access` |
| Provider callback is evidence, never authority | I + test | `authorization_layers::k_a_provider_or_telephony_callback_never_creates_or_mutates_an_unauthorized_call` |
| Inbound PSTN / enterprise SIP into a suspended scope | N | `authorization_layers::{l_an_inbound_public_offer_into_a_suspended_scope_creates_no_call, m_an_enterprise_sip_offer_into_a_suspended_scope_creates_no_call}` |
| Composite layered suite (one test per layer, each asserting no protected side effect) | N | `tests/authorization_layers.rs` (15 tests A–O) |

## 3. §72 Tenant and context isolation — PASS

| Requirement | | Evidence |
|---|---|---|
| Stolen Call / participant / context / session / ContactReference → 404 | I | `tenant_isolation.rs`, `call_api::foreign_calls_are_indistinguishable_from_missing_ones`, privacy-relay `isolation.rs` |
| Cross-application and cross-environment id theft: extension, department, ring group, queue, IVR, mailbox, message, recording, webhook, Call / history, usage, block rule, abuse report — same status and body as an unknown id, no audit, no URL, no mutation | N | `tenant_isolation::cross_scope::ids_from_another_application_or_environment_behave_like_unknown_ones` (no leaking lookup found) |
| Relay assignment across application / environment; grant existence oracle | N | privacy-relay `relay_numbers::reads_and_releases_from_another_application_or_environment_are_not_found`; `part_d_certification.rs` byte-identical response |
| Context isolation (participant, direction, channel, revoked, reassigned) | I | privacy-relay `authorization.rs`, `sessions.rs`, `lifecycle.rs`, `reassignment.rs` |
| Metric labels bounded (Calling-wide guard: source + rendered registry) | N | `metric_safety::{every_metric_call_uses_allowed_keys_and_bounded_values, the_source_guard_rejects_identifier_labels, the_runtime_guard_rejects_identifier_values, the_rendered_registry_never_carries_identifiers}`; Telephony `paid_safety::metrics_are_bounded_and_never_carry_identifiers` |
| TRACE log-leak scans: webhook signing secret, secret-box key, LiveKit secret, media-edge / signaling keys, signed object URLs / tokens, Contact Resolution Grant secret, protected numbers, pepper, object keys | H | `secret_leakage::runtime_secrets_never_reach_logs_or_any_other_response`; privacy-relay `contact_logging.rs` (fingerprints, keys, numbers); Telephony `provider_events::token_route_callbacks_never_leak_the_token_signature_secret_or_body_into_logs`, `malformed_callbacks_aggregate_into_one_incident_without_leaking_into_logs` |
| Operator audit (explicit operator identity, correlation id, append-only, same transaction) | N | `src/platform/operator.rs`; `admin_cli::{every_mutation_needs_an_operator_and_writes_one_append_only_audit_row, cli_manages_the_full_credential_lifecycle, retention_holds_tombstones_and_reseals_are_operator_audited}`; Telephony `telephony_operator_audit` + `--operator` |

## 4. §73 Contact Vault cryptography — PASS

| Requirement | | Evidence |
|---|---|---|
| AES-256-GCM, fresh nonce, AAD = scope + ContactReference + key version | I + H | `contact_vault/crypto.rs` unit tests (tamper, wrong nonce / AAD / key / version, truncation, per-part misbinding, pinned labels); ADR 0011 label corrected |
| Ciphertext from tenant / application / environment A never opens as B; copied to another ContactReference fails | N | `contact_crypto_scope::{a_ciphertext_copied_across_tenant_application_environment_or_contact_never_opens, a_row_rehomed_to_another_scope_with_its_contact_reference_intact_never_opens}` |
| Missing referenced key version → start refused and runtime fail-closed | N | `contact_crypto_scope::a_key_version_missing_from_the_keyring_refuses_start_and_fails_closed_at_runtime` |
| Key material outside PostgreSQL; purpose separation; weak / malformed refused | I + H | `contact_safety.rs`, `contact_equality.rs`; `keyring.rs` variant-exact tests |
| Keyed, scope-bound fingerprints | I | `contact_equality::the_same_number_in_one_scope_is_one_contact_and_independent_across_scopes` |
| Rotation (CLI end to end, atomic keyring replacement, guarded retire) | H + N | `admin_cli::{the_admin_cli_rotates_the_encryption_key_add_activate_reencrypt_retire, the_admin_cli_rotates_the_fingerprint_key_add_backfill_activate_prune_retire}`; `secret_file` unit tests; `contact_vault::encryption_key_rotation_reencrypts_in_batches_without_changing_any_contact` |
| Telephony-only resolution via grant | I | `contact_resolution.rs`; `service-auth policy::j_protected_contact_resolution_is_telephony_only` |
| Append-only audit incl. TRUNCATE; FAILED outcomes | H | `audit::evidence_tables_cannot_be_truncated`; FAILED rows in `contact_crypto_scope` tests |
| Backup / keyring separation documented | N | `docs/architecture/security-deployment-checklist.md` §3 |

## 5. §74 Secret protection — PASS

| Requirement | | Evidence |
|---|---|---|
| No production fallback; startup refusal | I + H | CI startup-refusal steps (replayed locally for all three services); `config::tests::production_without_a_pepper_refuses_to_start`; Telephony ARI / provider-secret refusal unit tests |
| `*_FILE` secret delivery (pepper, previous pepper, signaling, media edge, LiveKit, webhook keys, S3 secret); ambiguous / missing / world-readable refused | N | `config::secret::tests::*`, `config::tests::{the_pepper_can_be_a_secret_file_and_is_never_ambiguous, a_world_readable_pepper_file_is_refused_in_production}`, `signaling::credentials::tests::the_key_may_be_a_secret_file_never_both_never_a_fallback`, `media::livekit::tests::provider_secrets_accept_files_and_refuse_placeholders`, `cf_object_storage tests::the_secret_access_key_may_be_a_secret_file` |
| Webhook master-key rotation: versioned seals, current + previous overlap, bounded audited reseal, unknown kid fails closed, startup refuses unreadable kids | N | `cf_webhooks secret_box::tests::*`; `webhooks::{deployment_key_rotation_keeps_deliveries_signed_and_reseals, operator_reseal_writes_one_audit_row_with_counts_only}` |
| Executable rotation tests | I + N | service-auth `policy::g_key_rotation_overlap_without_downtime`; API credentials `tests/rotation.rs`; Contact Vault / fingerprint CLI tests above; webhook key above; provider callback secret `provider_events::a_callback_signed_with_the_previous_secret_is_accepted_during_the_overlap` |
| Service isolation, LiveKit admin secret server-only | I | architecture tests, boundary guard |
| Runbooks honest about restarts | N | `docs/architecture/secret-rotation-runbook.md` |
| Repository / image secret scan | N | `scripts/secret-scan/` + CI job; per-service CI image scans |

## 6. §75 SIP / telecom security — PASS

| Requirement | | Evidence |
|---|---|---|
| Carrier identification by network address only (`endpoint_identifier_order = ip`, user identifier module not loaded, no anonymous endpoint) | N | `infra/telephony/asterisk/*`; `edge_config::{the_production_edge_identifies_endpoints_by_ip_only, the_production_edge_never_loads_the_user_or_anonymous_identifier, the_certification_edge_mirrors_the_ip_only_rule, the_guard_rejects_username_identification_unidentified_and_anonymous_endpoints}`; SIP certification section E |
| ARI private / encrypted; inline / short password refused | H | `AriSettings::validate_url`; `ari::tests` |
| Transmit-time re-check of route, provider, circuits, destination class | N | `executor.rs::transmit_gate`; `paid_safety::a_change_between_route_choice_and_the_transmit_fence_stops_the_request` |
| Destination classes (premium / high-risk / satellite / shared-cost, unclassified policy, longest prefix, country separation) | N | `destination_classes::*` (6 tests) |
| SIP ingress: source CIDR, dialog cap, disabled peer, rate-limit incident, unknown peer, bound / unbound number | N | `sip_ingress::*` (6 tests) |
| Pre-toll order; operator route / provider disable; app-to-app unaffected | I | `failover.rs`, `paid_safety.rs`, `public_failover::public_route_trouble_never_disables_app_calls` |
| Segmentation and credential rotation documented | N | `security-deployment-checklist.md`, `secret-rotation-runbook.md` §7 |
| SIP certification with carrier DTMF | I | `sip_certification::real_sip_media_edge_certification` (final run, REQUIRED=1) |

## 7. §76 Provider callback security — PASS

| Requirement | | Evidence (`services/telephony/tests/provider_events.rs` unless noted) |
|---|---|---|
| Valid / invalid / missing signature; auth before parse | I | `callbacks_are_authenticated_size_limited_and_strictly_parsed_before_any_effect`; `provider_contract::callbacks_are_authenticated_before_parsing_and_parsed_strictly` |
| Previous-secret overlap | N | `a_callback_signed_with_the_previous_secret_is_accepted_during_the_overlap` |
| Unknown provider / malformed id never becomes a subject | N | `unknown_provider_callbacks_aggregate_under_the_unknown_subject`, `malformed_provider_ids_are_refused_and_never_become_a_subject` |
| Disabled provider: callbacks still apply to in-flight legs (documented) | N | `a_disabled_providers_callbacks_still_apply_to_its_in_flight_legs` |
| Oversized body counted | N | `oversized_callbacks_are_counted_even_when_the_listener_refuses_them` |
| Strict parsing; over-long ids → 400 before any transaction | N | `over_long_identifiers_are_refused_with_400_before_any_transaction`, `authenticated_but_malformed_payloads_are_refused_before_any_effect` |
| Dedupe, replay burst, out-of-order, late success after terminal | I + N | `provider_events_are_deduplicated_by_provider_and_event_id`, `replay_bursts_become_one_incident_while_a_single_duplicate_is_normal`, `out_of_order_events_never_regress_and_late_success_never_resurrects`, `a_late_success_on_a_terminal_leg_with_a_live_provider_call_is_cleaned_up` |
| Incident threshold (below none, at one, past one with occurrences) | N | `the_auth_failure_incident_starts_exactly_at_the_threshold`, `malformed_callbacks_aggregate_into_one_incident_without_leaking_into_logs` |
| No secret / body / path token in logs | N | `token_route_callbacks_never_leak_the_token_signature_secret_or_body_into_logs` |

## 8. §77 Fraud / spam / abuse — PASS

| Requirement | | Evidence |
|---|---|---|
| Rate / velocity / concurrency / spend | I | Part G `quota_levels.rs`, `public_spend.rs`, `call_concurrency.rs` |
| Unknown cost: no unreserved or unbounded paid leg (positive capped reservation before ORIGINATE; settles at the reservation, `cost_known = false`, usage fact never rewritten) — no extra policy needed | evaluated | `public_spend::an_unknown_cost_never_opens_an_unreserved_or_unbounded_paid_leg` |
| Blocks: principal ↔ principal, principal / application → ContactReference; before Privacy Relay, spend and Telephony; ring group, queue, forwarding, callback, inbound; isolation; existing Calls untouched | N | `blocks::*` (10 tests); fix found: forwarding ignored the caller's own block |
| Repeated blocked attempts / route denials → one incident per window, unlabelled metrics | N | `blocks::{repeated_blocked_attempts_raise_exactly_one_incident_per_window, repeated_route_denials_raise_one_incident_and_stay_unlabelled}` |
| Abuse reports: participant-only, foreign scope 404, bounded vocabulary, idempotent, burst incident, no number / payload | N | `abuse_reports::*` (4 tests) |
| Nuisance signals as incidents | I | Telephony `abuse.rs`; `paid_safety::abuse_signals_are_raised_without_numbers` |
| Suspension semantics; carrier circuit breaker | H / I | §2; `paid_safety::an_operator_disables_one_route_or_interconnect_without_disabling_telephony` |

## 9. §78 Telecom regulatory boundary — PASS

| Requirement | | Evidence |
|---|---|---|
| No caller-id input | I | `architecture_boundaries::request_bodies_carry_cf_meet_concepts_only`; Telephony parser test |
| Other tenant's / other session's / expired / released / disabled relay identity refused | N | privacy-relay `telephony_execution::{the_caller_identity_ignores_other_sessions_tenants_and_ended_or_disabled_numbers, the_caller_identity_is_withheld_at_read_time_for_expired_or_unavailable_numbers_and_foreign_bindings}`; Telephony `telephony_core::the_caller_identity_must_be_the_relay_identity_bound_to_the_chosen_interconnect` |
| Relay assignment reference per leg (evidence) | N | Telephony migration 000013; same test |
| Compliance eligibility hook + operator admin path (set / revoke / show, audited, opaque evidence) | N | `routing/compliance.rs`, `platform_admin compliance`; `compliance::*` (6 tests), `admin_cli::compliance_commands_validate_bounded_opaque_input_and_show_a_safe_view`; fix found: the platform overlay was never wired in |
| CF Meet is not a carrier | I | ADR 0016, ADR 0029 §9 |

## 10. §79 Recording / consent / privacy — PASS

| Requirement | | Evidence |
|---|---|---|
| Recording authority independent; notice; per-participant consent; withdrawal | I | ADR 0024; `enterprise_recording.rs` |
| Orphan objects never left behind (integrity mismatch, failed finalization, no longer awaiting) | N | `enterprise_recording::orphaned_capture_objects_are_never_left_behind`, `enterprise_voicemail::voicemail_capture_objects_are_never_left_behind` |
| FAILED recordings retention-deleted | N | `enterprise_recording::failed_recordings_are_retention_deleted` |
| Storage delete failure: stays DELETING, retried, one incident, eventual DELETED | N | `enterprise_recording::{deletion_survives_a_storage_outage_with_one_incident, retention_honours_holds_and_survives_a_storage_outage}` |
| Voicemail allowed and denied access audited; cross-tenant leaves no audit and reveals nothing | N | `enterprise_voicemail::voicemail_access_decisions_are_audited_without_revealing_existence` |
| Cross-tenant artifact → 404, no grant | I | `enterprise_recording`, `enterprise_voicemail`, cross-scope test §3 |

## 11. §80 Data retention / deletion — PASS

| Service / data class | Action | | Evidence |
|---|---|---|---|
| Privacy Relay: contact value (erase after disable), verification events, grants, access audit, relay evidence, outbox | Minimize / Delete | N | privacy-relay `retention.rs` (7 tests) |
| Telephony: provider events, resolved incidents; holds; retention audit | Delete | N | Telephony retention module; `provider_events.rs` purge tests |
| Calling CALL_RECORD (display minimization; Call, events, usage, spend kept) | Minimize | N | `retention::terminal_calls_are_minimized_after_the_bound_and_their_evidence_is_kept`, `enterprise_entry_labels_are_minimized_to_the_extension_code` |
| Calling CALL_EVENT, CALL_USAGE, SPEND_EVIDENCE | Retain (evidence) | N (policy) | same test (row counts unchanged; DELETE refused even under the worker flag) |
| Calling webhook event / delivery history (pending / retrying never) | Delete | N | `retention::delivered_webhook_history_is_purged_and_pending_work_is_never_touched` |
| Calling incidents (OPEN never), media / telephony provider evidence, operator audit (own bound) | Delete | N | `retention::incidents_provider_events_and_operator_audit_follow_their_own_bounds` |
| Holds (scope-exact; cross-tenant never protects; audited) | — | N | `retention::holds_protect_their_own_scope_only_and_are_operator_audited` |
| Worker: bounded, SKIP LOCKED, idempotent, retried, audited | — | N | `retention::{two_concurrent_workers_act_on_each_row_exactly_once, a_failed_batch_leaves_nothing_partial_and_is_retried}` |
| Principal tombstone (FKs intact, history coherent, never resolves again) | Minimize | N | `retention::principal_tombstone_keeps_history_and_foreign_keys_and_never_resolves_again` |
| Recording / voicemail artifacts | Part F workers + holds | I + H | `enterprise_voicemail::deletion_and_retention`, `enterprise_recording::retention_honours_holds_and_survives_a_storage_outage` |

## 12. Mutations H1–H12 (each applied alone, target test run, source restored)

| # | Mutation | Killing test | Result |
|---|---|---|---|
| H1 | principal resolution without the tenant / application / environment predicate | `tenant_isolation::principal_resolution_never_crosses_tenant_application_or_environment`; `authorization_layers::d_a_principal_of_another_environment_or_inactive_is_refused` | **KILLED** |
| H2 | resolve-for-execution without a valid grant secret / binding | privacy-relay `contact_resolution::grants_are_bound_to_contact_leg_scope_time_and_current_authority` | **KILLED** |
| H3 | Contact Vault AAD without scope (and each scope part alone) | `crypto::tests::{fixed_vector_is_stable_and_bound_to_its_aad, tampering_and_misbinding_fail_authentication}`; `contact_crypto_scope::a_row_rehomed_to_another_scope_with_its_contact_reference_intact_never_opens` | **KILLED** |
| H4 | production fallback pepper when unset | `config::tests::production_without_a_pepper_refuses_to_start` | **KILLED** |
| H5 | callback signature check accepts anything | `provider_events::callbacks_are_authenticated_size_limited_and_strictly_parsed_before_any_effect` (+4) | **KILLED** |
| H6 | provider-event dedupe branch disabled | `provider_events::provider_events_are_deduplicated_by_provider_and_event_id`, `replay_bursts_become_one_incident_while_a_single_duplicate_is_normal` | **KILLED** |
| H7 | skip block / compliance (Calling) and destination class (Telephony) before originate | `blocks::a_blocked_contact_is_refused_before_privacy_relay_spend_and_telephony`, `authorization_layers::g_…`, `compliance::*`; `destination_classes::*`, `paid_safety::a_change_between_route_choice_and_the_transmit_fence_stops_the_request` | **KILLED** |
| H8 | executor accepts a caller identity not bound to the interconnect | `telephony_core::the_caller_identity_must_be_the_relay_identity_bound_to_the_chosen_interconnect` | **KILLED** |
| H9 | required recording consent ignored | `enterprise_recording::consent_is_each_participants_own_and_bound_to_the_call` | **KILLED** |
| H10 | retention deletes immutable usage evidence of a minimized Call | `retention::terminal_calls_are_minimized_after_the_bound_and_their_evidence_is_kept` | **KILLED** |
| H11 | tenant UUID as a metric label | `metric_safety::every_metric_call_uses_allowed_keys_and_bounded_values`, `the_rendered_registry_never_carries_identifiers` | **KILLED** |
| H12 | webhook signing secret logged | `secret_leakage::runtime_secrets_never_reach_logs_or_any_other_response` | **KILLED** |

## 13. Defects found and fixed during closure

* Compliance overlay and abuse thresholds were parsed but never wired into Calling's state.
* Forwarding to a public contact checked application blocks only, not the caller's own.
* `privacy-relay-admin add-key / activate-key` deleted the live keyring before writing the
  new one (first pass).
* SIP: username-based endpoint identification enabled in the production edge config.
* A first-pass stricter edge-event attribution quarantined legitimate carrier DTMF; reverted
  (Part E behaviour kept, SIP certification guards it).
* CI: hard-coded table / migration counts went stale (Part G and Part H) — the migration
  counts are now derived from each migrator; the `telephony-admin` operator requirement
  broke the e2e harnesses (fixed); the Calling ↔ Privacy Relay e2e "Calling down" step left
  pooled keep-alive connections served (graceful shutdown now); MinIO moved to the pinned
  `pgsty/minio` digest (Docker Hub no longer serves `minio/minio`); two secret-scan false
  positives in test / source text.
* Tests: the enterprise history-plan assertion depended on autovacuum timing (ANALYZE
  before EXPLAIN now; assertion unchanged); a capture-egress test tag could produce an
  invalid name (digit-safe now); the enterprise certification voicemail audit expectation
  now includes the audited denied access and metadata read (§79).
* Tests: the "no telephone number in Calling" heuristic scanned bare digit runs, so a
  digit run inside a random hex hash / UUID group could match (seen once in CI on
  `c15f48f`); it now scans standalone all-digit tokens, with a self-test proving a stored
  number is still found and random identifiers are not.

## 14. External items (kept separate from implementation correctness)

* **GITHUB ACTIONS** — Actions now execute (the earlier runner / billing blocker is gone). The `81d05bb` runs failed on real CI defects that this closure fixes: stale table / migration counts, the e2e harnesses calling `telephony-admin` without an operator, the Calling-down e2e race, the `minio/minio` image no longer pullable, and two secret-scan false positives. Results for the closure commit are recorded in the final report.
* **PART E REAL LAWFUL TELECOM INTERCONNECT CERTIFICATION — PENDING — EXTERNAL CARRIER
  CREDENTIAL BLOCKER** (ADR 0016). Asterisk + SIPp certify the CF-owned SIP / media edge
  only; no carrier credentials or interconnect exist in this environment.
