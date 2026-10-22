# CF Meet Part V — backend-only closure

Specification: *CF Meet Part V — Secure Unified Messaging & End-to-End Encryption*, v2.0
(Parts A–L, §§1–166, Appendices G–N). Scope of this record: the BACKEND only (services,
crypto core, contracts, migrations, operations tooling). Frontend, mobile, hosted
environments, providers and external assessments are carried forward (section 9) and are
never counted as passes.

## 1. Baseline, branch and commits

| Item | Value |
|---|---|
| Baseline | `origin/main` = `71013c7` (re-fetched and verified; `staging` was behind and not used) |
| Branch | `part-v-backend-closure` (from `71013c7`) |
| Head at the time of writing | see the pull request; every commit is listed below |
| Merge / deploy | none — the branch is published for review only |

Commits (oldest first): `19dcb28` edit revisions, policy-governed threads / reactions / pins /
SMS eligibility · `b1caa39` missing domain events · `5323d63` Calling projection race, outage
mapping, harness port adoption · `b918ea9` DELIVERED receipts, own-device read sync,
removed-member delivery race · `1c54b05` harness listener ownership · `4e6bb97` secure backups,
independent kill switches · `d925c28` client tokens on member REST routes · `fefe75d` MLS
forward secrecy / PCS / large-group tests · `b511f3d` envelope encryption of MANAGED content ·
`1db049b` member pagination, window and race tests · `4ef5e7e` clippy · `4e22b08` abuse
signals, event scope ids, E2EE envelope fields, plain-text rule · `d54466f` conversation
retention, expiry grace, pin expiry, thread summaries · `9beeb6b` usage metering, durable jobs,
histograms, incidents · `879ade4` safety code / QR, parser property tests · `6a41e06` signed-read
expiry on real S3 · `11f6064` restore of objects / SMS / E2EE · `f1fbdb5` successor
conversations · `44e34d1` roles and invitations · `e0e73a6` privacy controls, relay-revocation
subscriptions, operations documents · `6877ad6` generic table name · plus the load-suite and
this record (see the pull request).

## 2. Requirement matrix

The full matrix — every row of §§1–166, workstreams A–Z and Appendices G–N, each with
implementation paths, tests and, where it was not complete at the baseline, the closing
commit and proof — is `evidence/part-v-backend-requirement-matrix.md`.

| Status at closure | Rows |
|---|---|
| IT — implemented and tested | 196 |
| IT / NB — backend implemented and tested; external part carried forward | 11 |
| NB — non-backend / external, carried forward | 3 |
| NR — not a requirement (informational / process) | 6 |
| PARTIAL / ABSENT / IMPLEMENTED-UNTESTED | **0** |

At the baseline the same audit had 47 PARTIAL, 7 PARTIAL / NB, 5 IT / PARTIAL, 2
IMPLEMENTED-UNTESTED and 1 ABSENT row; they decomposed into 34 backend gaps (G-01…G-34). A 35th
(G-35) was found during the Privacy Relay outage work. All 35 are closed:

| Gap | § | Closed by | Proof |
|---|---|---|---|
| G-01 roles MODERATOR / READ_ONLY / BOT_SERVICE, INVITED + invitations | §30 §35 §36 §113 | `44e34d1` | `engagement::moderator_read_only_and_bot_service_roles_are_enforced`, `::invitations_are_accepted_declined_revoked_or_expire` |
| G-02 large-group membership pagination | §30 | `1db049b` | `engagement::large_group_membership_is_paginated_and_the_view_is_bounded` |
| G-03 policy snapshot governs threads / reactions / pins / SMS eligibility | §28 §47 §106 | `19dcb28` | `engagement::the_policy_snapshot_governs_threads_reactions_and_pins`; `sms_bridge::sms_bridge_eligibility_follows_the_context_policy` |
| G-04 expiry grace period | §38 | `d54466f` | `conversations::an_expiry_grace_period_keeps_the_conversation_readable_then_expires_it` |
| G-05 text format rule | §41 | `4e22b08` | `messaging::managed_text_is_plain_text_never_interpreted_markup` |
| G-06 MANAGED edit revisions, hold-aware, version-checked | §45 §122 §146 | `19dcb28` | `compliance::managed_edits_keep_sealed_revisions_that_holds_preserve_and_purge_removes` |
| G-07 thread summary / unread replies | §47 | `d54466f` | `engagement::thread_summaries_count_replies_and_unread_replies` |
| G-08 pin expiry | §49 | `d54466f` | `engagement::pins_can_expire_and_be_renewed` |
| G-09 edit / delete windows | §50 §146 | `1db049b` | `messaging::edit_and_delete_windows_close_per_policy` |
| G-10 DELIVERED receipts | §52 §118 | `b918ea9` | `realtime::delivery_receipts_and_own_device_read_sync_respect_the_privacy_policy` |
| G-11 read-receipt privacy, own-device sync | §53 §55 §148 | `b918ea9`, `e0e73a6` | same + `engagement::read_receipts_are_configurable_per_user_and_conversation` |
| G-12 derivative events | §80 | `b1caa39` | event conformance (`conforming_event_types`) in the attachment suites |
| G-13 complaint / rejected-send abuse signals | §88 §133 | `4e22b08` | `compliance::rejected_sends_and_complaints_feed_abuse_scoring_without_report_bombing` |
| G-14 per-conversation retention / disappearing messages | §90 App. K | `d54466f` | `engagement::a_conversation_retention_only_tightens_and_expires_its_own_messages` |
| G-15 client tokens on member REST routes | §105 | `d925c28` | `client_tokens_rest::end_user_devices_use_member_operations_directly_with_their_client_token`, `::the_contract_documents_exactly_the_client_token_routes` |
| G-16 scope ids in durable events | §117 | `4e22b08` | event / realtime conformance |
| G-17 domain event catalogue | §118 §119 App. D | `b1caa39` | event conformance across suites |
| G-18 usage: connections, egress, search | §129 | `9beeb6b` | `realtime::realtime_connection_time_is_metered_when_the_connection_closes`; usage assertions in engagement / attachments |
| G-19 independent kill switches | §134 | `4e6bb97` | `operations::every_capability_has_an_independent_kill_switch` |
| G-20 durable jobs: dead-letter replay, KeyPackage expiry / low stock | §136 G.4 | `9beeb6b` | `operations::dead_letters_are_replayed_by_a_named_operator_and_delivered_once`, `::expired_key_packages_are_closed_and_low_stock_alerts_once` |
| G-21 MANAGED envelope encryption (wrapped per-environment keys) | App. J | `b511f3d` | `managed_keys::managed_content_is_sealed_under_wrapped_per_environment_keys` |
| G-22 restore of objects, SMS, unrecoverable E2EE | §137 App. E | `11f6064` | `disaster_recovery::a_restore_keeps_attachments_never_resubmits_sms_and_cannot_recover_e2ee_without_device_state` |
| G-23 runbook, integration guide, stale security doc | §138 §165 | `e0e73a6` | `docs/architecture/messaging-operations-runbook.md`, `messaging-integration-guide.md`, refreshed `messaging-security-architecture.md` |
| G-24 histograms, lag gauges, incident categories | §139 §141 | `9beeb6b` | `operations::backlogs_and_abuse_waves_raise_incidents_and_latency_is_a_histogram`; metrics unit tests |
| G-25 property / fuzz tests | §144 §158 | `879ade4`, `9beeb6b` | `messaging-crypto` `parser_properties` (4); `realtime::malformed_and_mutated_frames_never_break_a_connection` |
| G-26 signed-read expiry on real storage | §150 | `6a41e06` | `object-storage` `s3_live::protected_objects_on_a_real_s3_compatible_server` |
| G-27 member-removal vs send race | §155 | `1db049b` | `messaging::member_removal_racing_sends_never_commits_a_post_removal_message` |
| G-28 secure backup and recovery | App. I.3 H.4 | `4e6bb97` | `backups::*` (3); `messaging-crypto` `backup_vectors` |
| G-29 successor conversations with disclosure | App. G | `f1fbdb5` | `conversations::a_profile_change_is_a_disclosed_successor_conversation_with_provenance` |
| G-30 E2EE envelope fields | H.1 I.1 | `4e22b08` | E2EE message view conformance |
| G-31 receipts per user / conversation, message requests, block-aware keys | App. K | `e0e73a6` | `engagement::read_receipts_are_configurable_per_user_and_conversation`, `::direct_message_requests_and_block_aware_key_lookup` |
| G-32 safety code / QR | G.5 K L | `879ade4` | `messaging-crypto` `safety_code_vectors`, `verification` unit tests |
| G-33 forward secrecy, PCS, large group | App. N | `fefe75d` | `messaging-crypto` `mls_security_properties` |
| G-34 no WebSocket compression | H.3 | `4e22b08` | `realtime::websocket_compression_is_never_negotiated` |
| G-35 relay revocation revokes realtime subscriptions | §37 | `e0e73a6` | `cross_service_e2e::scenario_4b_privacy_relay_outage_and_restart_neither_grant_nor_lose_authority` |

Bounded choices recorded with their tests (not open gaps): retention per context follows the
environment's CURRENT policy while a conversation retention can only tighten it; DIRECT
message requests are opt-in (`requestsRequireAcceptance`); PRIVATE_E2EE requests carry no
messages before acceptance; the MANAGED search index is maintained in the same transaction as
every write, so no separate reindex job exists; MANAGED key rotation is supported, a bulk
re-seal after a key compromise is a documented runbook procedure, not an automated job.

## 3. Tests and results

| Suite (local, this branch) | Result |
|---|---|
| Messaging, full (`cargo test --no-fail-fast`, 35 binaries, real PostgreSQL, Redis, MinIO; `CROSS_SERVICE_E2E_REQUIRED=1`) | 241 passed, 0 failed, 9 ignored (opt-in load / 100-device tests) — before the final load-suite and rename commits; the affected suites (realtime, migrations, architecture, disaster recovery) were re-run green after them |
| Authorization / IDOR matrix | every customer operation (incl. the 3 new ones) × 15 cases: passed |
| Real-process journeys (section `evidence/part-v-closure-real-process-2026-10-22.txt`) | cross-service 6/6 (incl. new 4b), platform-access e2e 3/3, Meeting unified messaging 2/2 + 1 web journey SKIPPED (frontend) |
| Crypto core (`--features client`) | parser properties 4/4, safety-code vectors 1/1, verification unit 3/3; clippy `--all-features` clean |
| Object storage against MinIO | `s3_live` 2/2 |
| Calling (Part B stability) | 691 passed, 0 failed, 91 binaries |
| Contracts validator / bundles | OK (66 contracts, 238 examples, 319 counter-examples) / current |
| `cargo fmt`, `cargo clippy --all-targets -D warnings` | clean for every changed crate |

The GitHub Actions run on the pull request is the final CI matrix (see the pull request).

## 4. Defects found and fixed

Twelve defects (beyond the 35 gaps), each with a regression test or guard:

| # | Defect | Origin | Fix | Regression proof |
|---|---|---|---|---|
| D-1 | Calling: concurrent first use of a public party / credential raced the projection upsert | main | `5323d63` | `concurrent_first_use_of_a_public_party_resolves_one_principal`, `concurrent_first_authentications_of_a_new_credential_all_succeed` |
| D-2 | Calling: a Platform Access outage was mapped to the wrong error | main | `5323d63` | Calling suite |
| D-3 | Real-process harnesses (Calling, Messaging, Meeting) could adopt a port served by another process | main | `5323d63`, `1c54b05` | `a_port_already_served_by_another_platform_access_is_never_adopted` |
| D-4 | Realtime: a removed member could still receive a message racing the removal (revalidation order) | main | `b918ea9` | realtime suite 30/30 repeated runs (main: 18 pass / 11 fail / 1 hang of 30) |
| D-5 | Test helper `Ws::recv` restarted its timeout on every server ping | main | `b918ea9` | realtime suite |
| D-6 | Contract: `Conversation.members` schema did not match the served view | main | `1db049b` | conformance in conversations / engagement |
| D-7 | Contract: `smsBridgeEligible` pinned to `false` | main | `19dcb28` | policy conformance |
| D-8 | Bundled Messaging manifest version out of step with its source | main | `b918ea9` | `bundle_openapi.py --check` |
| D-9 | Contract example lacked `memberCount` / `membersTruncated` | this branch (`1db049b`) | `d54466f` | contracts validator |
| D-10 | `messaging_presence_settings.visibility` VARCHAR(16) too short for its own allowed value `CONVERSATION_MEMBERS` (503) | main | `e0e73a6` (migration 000028) | `engagement::read_receipts_are_configurable_per_user_and_conversation` |
| D-11 | New table name matched the service-boundary product-word guard | this branch (`b918ea9`) | `6877ad6` | `check_service_boundaries.py` OK |
| D-12 | First MANAGED sends of an environment resolved the data key on a second pooled connection while holding a transaction; a burst larger than the pool starved it (503) — found by the load suite | this branch (`b511f3d`) | `ManagedKeyring::prepare` before every sealing transaction (see the pull request) | `managed_keys::a_burst_of_first_sends_never_starves_the_connection_pool` (fails without the fix: 503 after the 30 s pool timeout) |

## 5. API inventory and Swagger

Details: `evidence/part-v-closure-api-inventory-2026-10-22.txt`.

* All 12 source OpenAPI documents: **661 operations, 544 unique paths**; in the unified Swagger
  selector **650 operations / 533 paths** (11 not in the selector); by audience: public
  infrastructure 16, customer 530, internal 109, provider callback 6.
* Messaging: public document **99 operations / 80 paths** (3 infrastructure, 94 customer, of
  which 58 client-token capable, plus 2 device-direct realtime); internal document **7**.
  Route parity: **106 / 106** documented operations matched to registered routes, none
  documented without a route; the 94-route delegated mirror equals the customer surface;
  `OPTIONS /v1/realtime/tickets` is a CORS preflight.
* Bundled (Swagger-facing) documents equal their sources. Documented operations are contract
  entries, not a statement about deployed endpoints.

## 6. Dependency, security, database and reliability findings

Details: `evidence/part-v-closure-security-checks-2026-10-22.txt`.

* `cargo audit` on all 15 lockfiles: 0 vulnerabilities beyond the four CI-documented and
  CI-proven ignores; informational unmaintained / yanked warnings only.
* Licences allowed (156 / 157 / 406 packages); CycloneDX SBOM of 884 components; secret scan
  clean; service boundaries OK (after D-11); operations registry OK; database isolation OK;
  migrations 28 up → 8 down → up on an empty database.
* Data contracts: ciphertext-only E2EE paths, no plaintext / keys in persistence, logs, metrics
  labels or events (architecture tests, outbox payload CHECK, histogram labels are route
  templates without ids); tenant isolation (authorization matrix); immutable profiles with
  successors only; no silent E2EE → MANAGED / SMS downgrade.
* Reliability: PostgreSQL loss (nothing acknowledged lost, no duplicates), SMS Gateway death
  (one provider submission each), Privacy Relay outage / restart (nothing granted, nothing
  lost, idempotent re-delivery), restore of database + objects (objects verified, no SMS
  re-submission, E2EE unrecoverable without device state).

## 7. Performance (development host, no thresholds)

PERFORMANCE_SECTION

## 8. Status

**BACKEND SOFTWARE COMPLETE: YES** — every mandatory backend requirement of §§1–166 and
Appendices G–N is implemented and tested (0 PARTIAL / ABSENT / UNTESTED rows); 35 gaps and 12
defects found, 47 resolved, 0 open.

**BACKEND INTEGRATION READY: YES** — the backend integrates end to end across real service
processes (Platform Access, Messaging, Privacy Relay, SMS Gateway with a synthetic carrier,
Calling webhooks, Meeting Service) and real PostgreSQL / Redis / S3 / clamd, with failure and
restart journeys green, published contracts in parity with the routes, and operator runbooks.

Both are conditional on the GitHub Actions run of the pull request being green; a red required
check reverts the status to NO until fixed. Neither status is a production, hosted-staging or
E2EE-production claim; release holds are unchanged.

## 9. Carry-forward register (not backend; never counted as PASS)

| Area | Item |
|---|---|
| Frontend | Web messaging client, the web journey of `unified_messaging_e2e` (SKIPPED), Swagger UI rendering in a browser, SRI / bundling |
| Mobile | iOS / Android clients, device runtimes of the crypto core, physical-device testing, safety-code / QR presentation |
| Staging | Hosted staging deployment and capacity certification on staging hardware |
| Production | Rollout, production E2EE enablement (`MESSAGING_E2EE_DEVICES_ENABLED` stays off outside local / test), measured RTO / RPO, managed-key custody (KMS / HSM) |
| External cryptographic review | AD-01…AD-10 owner decisions; independent assessment (AD-10); MPL-2.0 `hpke-rs` legal confirmation |
| SMS carrier | Real carrier onboarding (synthetic carrier only) |
| Key-transparency witnesses | Third-party witness onboarding (reference witnesses only) |
| Push providers | APNs / FCM / Web Push provider onboarding and live delivery |
| Antivirus signatures | Production ClamAV signature databases (tests use the real engine with the EICAR signature) |
