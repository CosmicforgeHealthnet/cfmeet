# Part H — Security, privacy, fraud & regulatory controls: certification report (§§71–80)

Base: `staging` at `f2361b1e3187501d422abfa6841dc6e303b80c89` (Part G closed).

Part H is a cross-cutting closure pass over the platform built in Parts B–G. Every row
below says whether the control was **inherited** (built earlier, audited here),
**hardened** (existing control strengthened) or **new** (absent before Part H), and cites
the code and test evidence. Decision record: ADR 0029 (security closure, incl. retention ownership). Existing records are linked, not duplicated: ADR 0003 / 0004
(trust model, service auth), 0011 (Contact Vault), 0024 (recording), 0028 (API, webhooks,
limits).

**Verdict: PART H — PARTIAL.** Several §§71–80 software capabilities are implemented and
certified below; every remaining gap is marked **MISSING**, **REVERTED** or "no dedicated test" in its row. Nothing here claims
real-carrier certification.

## 0. Stage 0 — control matrix and method

Before any code change, a security inventory of the existing platform was taken
(authorization chain, isolation, Contact Vault, secrets, SIP edge, callbacks, fraud
controls, regulatory boundary, recording, retention). The inventory classified every
§71–§80 bullet as COMPLETE ALREADY / NEEDS HARDENING / MISSING; only the last two
classes produced code. The rows below are that matrix with its outcome.

Legend: **I** inherited (Parts B–G, audited, unchanged) · **H** hardened · **N** new.

## 1. Gates on the final tree

| Gate | Result |
|---|---|
| Calling fmt / clippy `-D warnings` | clean |
| Calling full suite (`cargo test --workspace --locked`) | 552 tests: first run 547 passed / 5 failed (expectations made stale by the new tables, routes and safety-only authority); those 5 fixed and re-run green (`admin_cli`, `architecture_boundaries`, `http_contract`, `principals`) |
| Privacy Relay fmt / clippy / full suite | 143 tests: 142 passed / 1 failed on first run (TRUNCATE assertion did not accept the FK refusal); fixed, `audit` re-run green |
| Telephony fmt / clippy / full suite incl. real Asterisk + SIPp (`TELEPHONY_SIP_CERT_REQUIRED=1`) | 107 tests: 105 passed / 2 failed on first run. (a) The architecture allow-list lacked `src/destinations.rs`: fixed. (b) **SIP certification regression**: the new unknown-channel attribution in `edge_events.rs` quarantined legitimate carrier DTMF. It was reverted to the baseline behaviour, and `sip_certification`, `provider_events` and `architecture` were re-run green |
| Contracts (`make contracts-validate`) | 40 contracts, 164 examples, 255 counter-examples — OK |
| Service boundaries (`make service-boundaries`) | OK — calling 85 tables, privacy-relay 24, telephony 21 |
| Secret scan (`make secret-scan`, new) | clean; 9 rule / allow-list mutation tests pass |

Baseline before Part H: Calling 546, Privacy Relay 126, Telephony 87, all green.

## 2. §71 Authorization model — HARDENED

| Requirement | Status | Evidence |
|---|---|---|
| Credential active / environment / scope / revocation immediate | I | `platform/authentication.rs::evaluate`; `tests/authentication.rs`, `tests/rotation.rs`, `tests/scopes.rs` |
| Suspended tenant / application / environment: no new protected action | H | Suspended scopes authenticate into `AuthorityMode::SafetyOnly`; `middleware/platform_auth.rs::is_safety_route` admits only end call, stop recording, disable forwarding, delete recording / voicemail, webhook disable; `tests/authentication.rs` (`assert_safety_only` for environment / application / tenant), unit `safety_allow_list_is_method_and_template_exact`, `tests/admin_cli.rs` |
| Inbound PSTN / enterprise SIP offers honour scope status | N | `calls/app/public/inbound.rs`, `calls/app/enterprise/sip.rs` (`SCOPE_SUSPENDED`), dispatch re-check `public/dispatch.rs` — **no dedicated test** |
| Principal category allow-list | N | `domain/kinds.rs` capability methods, `calls/app/create.rs`, `PRINCIPAL_NOT_ELIGIBLE` — covered by the existing suites staying green, **no dedicated test** |
| Privacy Relay decision held by Calling; Telephony never authorizes | I | ADR 0010 / 0014; `call_authorization.rs`, `telephony_core.rs` |
| Route / country / spend / velocity before paid action | I | `public_app_to_phone::every_gate_fails_closed_before_any_paid_action`, `public_spend.rs` |
| Feature (recording) and enterprise admin scopes independent | I | `enterprise_recording::permission_to_call_is_not_permission_to_record`, `enterprise_security::calling_authority_never_implies_administration_or_artifact_access` |
| Customer credential never internal auth; callbacks are evidence | I | `internal_auth.rs`, `provider_events.rs` |
| One composite layered-authorization suite | **MISSING** | the per-layer evidence above exists; a single matrix suite was not written |

## 3. §72 Tenant and context isolation — PARTIAL

| Requirement | Status | Evidence |
|---|---|---|
| Stolen Call / participant / context / session / ContactReference / enterprise ids → 404 | I | `tenant_isolation.rs`, `call_api::foreign_calls_are_indistinguishable_from_missing_ones`, privacy-relay `isolation.rs`, `enterprise_foundation::cross_tenant_application_and_environment_ids_behave_like_missing_ones` |
| Relay assignment across application / environment | N (test) | privacy-relay `relay_numbers::reads_and_releases_from_another_application_or_environment_are_not_found` |
| Resolution grant for another tenant indistinguishable from unknown | H (test) | `part_d_certification.rs` (byte-identical response assertion) |
| Context isolation (participant, direction, channel, revoked, reassigned) | I | privacy-relay `authorization.rs`, `sessions.rs`, `lifecycle.rs`, `reassignment.rs` |
| Cross-application / cross-environment theft for ring groups, queues, IVR, voicemail, recordings, webhooks, usage | **MISSING** (tests) | lookups are scope-filtered (audited), but only cross-tenant tests exist |
| Metric labels bounded | I / partial | `&'static str` label types; `telephony paid_safety::metrics_are_bounded_and_never_carry_identifiers`, `enterprise_operations::metrics_are_bounded`; full-registry Calling guard **MISSING** |
| Log leak scans | I / partial | `secret_leakage.rs`, privacy-relay `contact_logging.rs`, Telephony callback log-capture test (`malformed_callbacks_aggregate_into_one_incident_without_leaking_into_logs`); webhook / LiveKit / signed-URL runtime scans **MISSING** |
| Operator / admin audit | H (Telephony) / partial | `telephony_operator_audit` + `--operator` in `telephony-admin`; Calling `platform_operator_audit` table created, CLI write path **MISSING** |

## 4. §73 Contact Vault cryptography — HARDENED

| Requirement | Status | Evidence |
|---|---|---|
| AES-256-GCM, fresh nonce, AAD scope / contact / version binding, tamper / wrong key / version / copy tests | I + H | `contact_vault/crypto.rs` tests (application-only / environment-only / version-label misbinding added), `the_aad_and_fingerprint_labels_are_pinned`; ADR 0011 label corrected to `cfmeet-contact-vault-v1` |
| Key material outside PostgreSQL | I | `m20261008_000006_contact_vault.rs`; `contact_safety.rs`, `contact_equality.rs` |
| Purpose separation, malformed / weak / missing-active refused | H | `keyring.rs` strict variant assertions, stronger weak-key rule |
| Keyed, scope-bound fingerprints | I | `fingerprint.rs`; `contact_equality::the_same_number_in_one_scope_is_one_contact_and_independent_across_scopes` |
| Rotation (add → activate → re-encrypt → backfill → retire) | H | `privacy-relay-admin` writes keyrings atomically with a `.bak` copy (no remove-before-write); referenced-version guard; `contact_vault.rs` rotation tests; `retention::rotation_skips_erased_rows_and_they_reference_no_key_version` |
| Narrow Telephony-only decryption via grant | I | `contact_resolution.rs` (4 tests), `service-auth policy::j_protected_contact_resolution_is_telephony_only` |
| Append-only audit incl. TRUNCATE | H | migration `m20261010_000013` BEFORE TRUNCATE guards; `audit::evidence_tables_cannot_be_truncated` |
| Backup sensitivity documented | H | `security-deployment-checklist.md` §3 (Privacy Relay, Calling, Telephony) |

## 5. §74 API / service secret protection — PARTIAL

| Requirement | Status | Evidence |
|---|---|---|
| No production fallback secret; startup refusal | I | CI startup-refusal jobs; config unit tests (Part B–G) |
| ARI password / provider secret / test-adapter refusal tests | N | `sip/ari.rs::startup_refuses_inline_missing_or_short_control_plane_passwords`, `providers/mod.rs` tests |
| Service isolation, LiveKit admin secret server-only | I | architecture tests, boundary guard, CI log grep |
| Provider callback secret overlap | N | `<credential_ref>.previous`; `provider_events::a_callback_signed_with_the_previous_secret_is_accepted_during_the_overlap` |
| Rotation runbooks (honest about restarts) | N | `docs/architecture/secret-rotation-runbook.md` |
| Repository secret scan | N | `scripts/secret-scan/` + CI job in `service-boundaries.yml` |
| `_FILE` loading for inline Calling secrets; webhook secret-box key weak-key check / previous-key overlap / reseal | **MISSING** | runbook §5 describes the intended procedure; not implemented |
| Built-image scans | I (CI) | per-service image scan steps; not re-run locally (no image changed by docs; services changed — run by CI) |

## 6. §75 SIP / telecom security — PARTIAL

| Requirement | Status | Evidence |
|---|---|---|
| Dialplan never dials; unknown source refused | I | `sip_certification` section E |
| ARI private; public ARI URL refused; TLS / private-address rule | H | `AriSettings::validate_url` (address, not host-name suffix), unit test |
| `allowed_source_cidrs` / `max_concurrent_dialogs` enforced in Telephony | N | `inbound.rs` (`SOURCE_NOT_ALLOWED`, `PEER_CAPACITY`, bounded metric) |
| Rate-limited inbound → incident | N | `SIP_INGRESS_RATE_LIMITED` (migration 000010) |
| Pre-toll order; route / provider disable | I | `executor.rs::execute_originate`; `failover.rs`, `paid_safety.rs` |
| Destination classification hook (premium / high-risk) | N | `destinations.rs`, `telecom_destination_classes`, `TELEPHONY_BLOCKED_DESTINATION_CLASSES`, `TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY` — **no dedicated test** |
| Segmentation / credential rotation docs | N | `security-deployment-checklist.md`, runbook §7 |
| PJSIP `endpoint_identifier_order = ip` (username identification removed) | **MISSING** | edge config not changed |
| Fence-time re-check of route / provider status | **MISSING** | |
| Unambiguous attribution of unknown-channel edge events | **REVERTED** | broke carrier DTMF in SIP certification; baseline behaviour kept |

SIP certification on the final tree: **PASS** (CF-owned edge only; not carrier certification).

## 7. §76 Provider callback security — HARDENED

| Requirement | Status | Evidence |
|---|---|---|
| Authentication before mutation | I | `ingest.rs::ingest_callback`; `callbacks_are_authenticated_size_limited_and_strictly_parsed_before_any_effect` |
| Identifiers validated before any transaction (400, never 500) | N | `valid_event_ids`; `over_long_identifiers_are_refused_with_400_before_any_transaction` |
| Body limit 16 KiB, oversize counted | I + N | `oversized_callbacks_are_counted_even_when_the_listener_refuses_them` |
| Dedupe / out-of-order | I | `provider_events_are_deduplicated_by_provider_and_event_id`, `out_of_order_events_never_regress_and_late_success_never_resurrects` |
| Invalid-callback incident aggregation with threshold | N | `CALLBACK_MALFORMED`, `CALLBACK_UNKNOWN_PROVIDER`, `CALLBACK_REPLAY_ABUSE`; `malformed_callbacks_aggregate_into_one_incident_without_leaking_into_logs`, `unknown_provider_callbacks_aggregate_under_the_unknown_subject`, `replay_bursts_become_one_incident_while_a_single_duplicate_is_normal` |
| Log safety (no token / body / signature) | H | `middleware.rs` logs the route template; log-capture assertion in the malformed test |

## 8. §77 Fraud / spam / abuse — PARTIAL

| Requirement | Status | Evidence |
|---|---|---|
| Rate / velocity / concurrency / spend | I | Part G `quota_levels.rs`, `public_spend.rs`, `call_concurrency.rs` |
| Nuisance signals persisted as incidents | I | Telephony `abuse.rs` (`ABUSE_SIGNAL`); `paid_safety::abuse_signals_are_raised_without_numbers` |
| Block / opt-out rules enforced before ringing, relay authorization, route | N | `calling_block_rules`, `calls/app/blocks.rs`, enforcement in `create.rs`, `public/outbound.rs`, `public/inbound.rs`, `callback.rs`, ring groups / queues / transfer / forward; `/v1/blocks` — **no dedicated test** |
| Repeated blocked attempts / route denials → incident | N | `calling_abuse_windows` + `calls/app/incidents.rs` — **no dedicated test** |
| User abuse report foundation | N | `calling_abuse_reports`, `calls/app/abuse_reports.rs`, `/v1/calls/{id}/abuse-reports` — **no dedicated test** |
| Suspension semantics | H | §2 above; principal suspension leaves connected Calls (ADR 0029 §2) |
| Carrier circuit breaker; app calls unaffected | I | `public_failover::public_route_trouble_never_disables_app_calls`, `paid_safety::an_operator_disables_one_route_or_interconnect_without_disabling_telephony` |
| Unknown-cost fail-closed policy | **MISSING** | |

## 9. §78 Telecom regulatory boundary — HARDENED

| Requirement | Status | Evidence |
|---|---|---|
| No caller-id input; relay identity only | I | `architecture_boundaries::request_bodies_carry_cf_meet_concepts_only`, Telephony parser test |
| Foreign / other-session / expired / released / disabled relay identity refused | N (test) | privacy-relay `telephony_execution::the_caller_identity_ignores_other_sessions_tenants_and_ended_or_disabled_numbers` |
| Relay assignment reference persisted per leg | N | Telephony migration 000013, `ports.rs` |
| Country policy typed; compliance eligibility hook | N | `platform_compliance_eligibility`, `routing/compliance.rs`, `compliance_required_countries` + `CALLING_COMPLIANCE_REQUIRED_COUNTRIES` overlay — **no dedicated test**; CLI to set it **MISSING** |
| CF Meet not a carrier | I | ADR 0016, ADR 0029 §9 |

## 10. §79 Recording / consent / privacy — INHERITED (no Part H change)

Independent recording authority, server-issued notice, per-participant consent,
withdrawal, protected storage, metadata minimization, access audit, cross-tenant 404:
ADR 0024 and `enterprise_recording.rs`, `enterprise_voicemail.rs`,
`enterprise_security.rs`, `rtc_certification.rs`. The hardening found by the audit
(orphan object on integrity mismatch, FAILED recordings never retention-deleted,
unaudited voicemail denials, no recording delete-outage test) was **not** implemented.

## 11. §80 Retention / deletion — PARTIAL

| Data class | Owner | Status | Evidence |
|---|---|---|---|
| Contact Vault protected value | Privacy Relay | N — erased after `CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS` once disabled; opaque reference kept | `retention::a_disabled_contact_is_erased_after_the_delay_and_stays_an_opaque_reference`, `an_erased_contact_never_resolves_even_with_a_live_grant_and_is_audited` |
| Verification events, grants, access audit, relay evidence, outbox | Privacy Relay | N — configurable bounds, holds, audit, worker flag | `evidence_purges_after_its_bound_and_not_before_and_only_the_worker_may_delete`, `a_live_context_and_an_unfinished_one_are_never_purged`, `held_contacts_are_not_erased_and_a_cross_scope_hold_does_not_protect`, `concurrent_workers_erase_each_contact_exactly_once` |
| Provider raw evidence, resolved incidents | Telephony | N — `TELEPHONY_PROVIDER_EVENT_RETENTION_DAYS`, `TELEPHONY_INCIDENT_RETENTION_DAYS`, holds, retention audit (migration 000014) — **no dedicated test** beyond the existing purge test |
| Voicemail / recording artifacts | Calling | I (Part F workers) | `enterprise_voicemail::deletion_and_retention`, `enterprise_recording::call_end_stops_and_artifacts_are_protected_and_audited` |
| Calls / events / usage / spend / webhook history / Calling incidents / media provider events / admin audit | Calling | **MISSING** — retained indefinitely; no Calling retention module, holds or deletion audit |
| Principal / account deletion | Calling | I — no hard delete exists (status only; evidence FKs RESTRICT); pseudonymisation **MISSING** |

## 12. Mutations (Stage 11)

**NOT RUN.** The H1–H12 mutation set was not executed in this pass.

## Known limitations (recorded, not faked)

* Every row marked **MISSING**, **REVERTED** or "no dedicated test" above is open work, not
  certified behaviour. New code without a dedicated test is exercised only to the
  extent the existing suites (all green) reach it.
* Default `TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY=ALLOW`: no destination-range data
  ships, so classification refuses only ranges an operator loads.
* Principal suspension does not end connected Calls (deliberate, ADR 0029 §2).

## External blockers (kept separate from implementation correctness)

* **GITHUB ACTIONS** — NOT RUN — EXTERNAL BLOCKER — GITHUB BILLING / ACTIONS AVAILABILITY. Every staging run up to `f2361b1` (e.g. run `37997209607`) completed in ~4 s with no steps executed; this is runner allocation, not a code-test result.
* **REAL LAWFUL CARRIER CERTIFICATION — PENDING — EXTERNAL CARRIER CREDENTIAL BLOCKER**
  (Part E, ADR 0016). No carrier credentials or interconnect exist in this environment;
  every telecom figure in this report is certified against the simulated interconnect
  adapter and the real-Asterisk / SIPp carrier simulator. Platform-side SIP / media edge
  behaviour is certified; carrier-specific production assertions are not claimed.
