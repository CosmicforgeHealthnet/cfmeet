# Part G — API, Realtime, Events & Usage: certification report (§§63–70)

Base: `staging` at `2062dab66ed4cb4048c260da7776f36e839d7f12` (Part F closed). Commits:
`514f931` (Wave 1, §§63–68), `0e87c66` (Wave 2, §§69–70), and the certification commit
that carries this report. Decision record: ADR 0028.

Part E's external item — **real lawful telecom interconnect certification** — remains
pending (ADR 0016). Every public-network figure below (leg usage, cost evidence, spend
limits) is exercised against the simulated Part E adapter only. **No real-carrier result
is claimed.**

## 1. Evidence per section

| § | Requirement | Evidence (test → assertion) |
|---|---|---|
| 63 | Versioned `/v1` APIs, contracts authoritative, OpenAPI covers implemented routes, no provider leak | `http_contract::*` (route ↔ OpenAPI scan includes `/v1/webhooks*`, `/v1/webhook-deliveries*`, `/v1/usage`, `/v1/limits`, `/internal/v1/platform/events`; responses validated against `platform/webhooks`, `platform/usage`); contract validator: 40 contracts, 163 examples, 248 counter-examples (phone numbers, provider ids, SIP URIs, prices, object keys, UNKNOWN-cost-as-zero, tenant fields refused) |
| 64 | Authority from authentication only; scoped principals; versioned mutations; audit | `webhooks::subscriptions_are_scoped_versioned_and_never_return_the_secret` (another application's subscription / delivery: 404 on every verb; stale `expectedVersion` 409; audit `CREATE, UPDATE, ROTATE_SECRET, DELETE`); `usage_limits::usage_api_lists_final_facts_of_the_caller_scope_only` (403 without `usage.read`, unknown / scope query parameters 400, another environment sees nothing); `limits::limits_cannot_be_supplied_by_the_client` |
| 65 | Idempotency matrix; replay = prior result; different payload = conflict; no duplicate paid execution | create call: `http_contract::call_api_responses_validate_against_the_calling_contract` (replay, `IDEMPOTENCY_CONFLICT`); callback: `public_callback_relay::a_reused_key_with_a_different_callback_is_a_conflict_and_never_dials` (new), `concurrent_identical_callback_requests_create_one_call_and_one_paid_leg`; transfer: `app_control` (transfer conflict); relay session: privacy-relay `idempotency`; origination / provider command: telephony `telephony_core` (requester + key + digest), `public_app_to_phone::a_lost_response_is_retried_with_the_same_command_and_never_duplicates`; usage: `public_callback_relay::the_requester_is_called_first_then_the_callee_then_both_are_bridged` (late usage event adds / changes nothing) |
| 66 | Realtime semantics incl. `call.route_changed`; reconnect recovery | `public_failover::*` (`call.route_changed` with `{"failover": true}` on the bus after a successful failover, no route / provider detail); `call_history::a_missed_call_survives_push_and_realtime_outage`; `app_ringing` recovery API |
| 67 | Transactional outbox, claim / lease, bounded retry, backoff, ACK, dedupe, dead letter, manual replay, broker-neutral | `webhooks::outbox_events_become_signed_public_deliveries`, `transport_outage_keeps_the_outbox_authoritative` (transport down → PENDING with `last_error_code`, then DEAD_LETTER after the bound, replay → PUBLISHED once); privacy-relay `outbox_publication::committed_events_are_published_once_retried_and_dead_lettered` (racing workers publish each row once, no publisher → nothing lost) |
| 68 | Webhooks: scoped subscriptions, allowed types, HMAC-SHA256 over timestamp + delivery id + body, retries, dead letter, replay, SSRF / rebinding / redirects, secret protection, no verbatim provider payload | `webhooks::*` (8 tests: signature verified by the reference verifier, stale timestamp refused, unique delivery ids, stable event id; 5xx / timeout retries → FAILED → replay as a new delivery; concurrent workers + crash recovery deliver once; SSRF matrix; DNS rebinding to 127.0.0.1 refused at connection time; Privacy Relay events normalized, enforcement events dropped); `cf-webhooks` unit tests (10: fixture signature, verify binding, secret box AAD, redirect refusal, resolver filter) |
| 69 | Usage facts (not billing), PSTN leg facts with KNOWN / ESTIMATED / UNKNOWN cost, callback both legs, artifacts, idempotent finalization, `GET /v1/usage` filters + pagination | `public_callback_relay::the_requester_is_called_first_…` (two leg facts of one Call: routing class, country, billable seconds, KNOWN cost; late event no-op); `public_phone_to_phone::a_protected_caller_reaches_a_protected_callee_through_one_call` (KNOWN and UNKNOWN — no amount — per leg); `records::usage_fact_tests::cost_evidence_is_never_invented` (ESTIMATED, malformed → UNKNOWN); `enterprise_certification::org_isolation_certification` (one VOICEMAIL / RECORDING artifact fact each, repeated delivery adds nothing); `usage_limits::usage_api_lists_final_facts_of_the_caller_scope_only` (non-final hidden, `from` / `to` / `usageType` / `resourceClass`, keyset cursor, DB CHECKs refuse UNKNOWN-with-amount and size-less artifacts, facts immutable, duplicate key refused) |
| 70 | Rate / concurrency / spend limits, precedence tenant → application → credential (→ principal), normalized errors, deduped `limit.warning` / `limit.exceeded`, `GET /v1/limits`, race safety at the last slot / headroom | `platform::limits::tests::tenant_level_is_part_of_the_precedence_chain`; `usage_limits::limits_api_reports_every_level_and_the_binding_one`; `usage_limits::tenant_concurrency_ceiling_holds_across_applications` (15 rounds × 6 concurrent creations across two applications: exactly 2); `call_concurrency::*` (credential / application last slot); `public_spend::concurrent_reservations_never_exceed_the_limit`, `part_b_application_and_credential_caps_are_enforced`, `velocity_per_principal_service_account_and_destination`; `usage_limits::limit_notices_are_emitted_once_per_window` (warning at 4/5, one exceeded per burst, a new window → a new notice, concurrency warning / exceeded, one delivery per notice, only allow-listed data keys); `limits::*` (fail-closed limiter, per-subject windows pruned) |

## 2. Mutations (one run each, source restored from git)

| # | Mutation | Target test | Result |
|---|---|---|---|
| A | subscription lookup without its tenant / application / environment filter | `webhooks::subscriptions_are_scoped_versioned_and_never_return_the_secret` | **killed** (IDOR 404 assertion) |
| B | delivery claim without the lease guard and `FOR UPDATE SKIP LOCKED` | `webhooks::concurrent_workers_and_crash_recovery_deliver_once` | **killed** ("disjoint claims") |
| C | timestamp removed from the signing input | `cf-webhooks` `signing::tests` | **killed** (fixture input and timestamp-binding asserts) |
| D | `is_prohibited` always false (localhost / private endpoints allowed) | `webhooks::ssrf_endpoints_are_refused` | **killed** |
| E | leg-usage idempotency key made unique per write (uniqueness guard defeated) | `public_callback_relay::the_requester_is_called_first_then_the_callee_then_both_are_bridged` | **killed** (duplicate leg fact) |
| F | tenant concurrency lock skipped | `usage_limits::tenant_concurrency_ceiling_holds_across_applications` | **killed** (more than 2 Calls created) |

## 3. Gates (direct, clean environment; GitHub Actions unavailable — billing)

| Gate | Check | Result |
|---|---|---|
| A | `contracts-validate` | OK — 40 contracts, 163 examples, 248 counter-examples, 453 JSON files |
| A | `contracts-test` (validator) | 17 passed |
| A | `service-auth` tests (all features) + fmt + clippy | 58 passed; clean |
| A | `service-boundaries` (guard + 55 unit / mutation tests) | OK — calling 78 tables, meeting-service 40, privacy-relay 22, telephony 17 |
| A | fmt + clippy `-D warnings`: Calling (+ migration), Privacy Relay, `cf-webhooks` | clean |
| A | scans of the Part G production diff: phone numbers, SIP URIs, secrets, provider names, debug output | only test fixtures (the documented HMAC fixture secret; catalog tests proving phone / SIP / provider values are dropped) |
| B | Calling full suite (once) | 536 passed, 1 failed → fixed (see below), target re-run green |
| B | Privacy Relay full suite | 126 passed, 0 failed |
| B | Telephony full suite (shares the changed `service-auth` registry) | 87 passed, 0 failed |
| C | Part G certification suite (§1 tests) | green |

The one full-suite failure was `principals::telephone_endpoints_store_only_opaque_references`.
Its schema-wide scan for phone-shaped column names found
`webhook_delivery_attempts.attempt_number`, an INTEGER ordinal. That column joined the
documented ordinal allowlist; the scan is unchanged for every other column. Gate A also
caught the boundary guard's table-family inventory missing `webhook_*`, which was added
with an explicit assertion of the four webhook tables.

## 4. Known limitations (recorded, not faked)

* A provider cost that arrives after a leg's usage was finalized is not applied (fact and
  settlement are written once); correction belongs to reconciliation / billing.
* Webhook events are stored only when a matching ACTIVE subscription exists at
  publication time; a later subscription does not receive earlier events.
* Limit notices are best-effort signals; enforcement never depends on them.
* Telephony's internal outbox keeps unbounded retries by design (ADR 0028 §67).
* No message broker, billing, invoicing, pricing or developer portal (out of scope).
* **Real lawful telecom interconnect certification: PENDING (Part E, ADR 0016).**
| B | RTC / SIP real-stack smoke | not re-run: no media path changed in Part G |
