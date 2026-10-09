# Part G — API, Realtime, Events & Usage: certification report (§§63–70)

Base: `staging` at `2062dab66ed4cb4048c260da7776f36e839d7f12` (Part F closed).

| Commit | Content |
|---|---|
| `514f931` | Wave 1 — §§63–68: durable events, customer webhooks, `call.route_changed` |
| `0e87c66` | Wave 2 — §§69–70: usage facts, usage / limits APIs, tenant level, limit notices |
| `73c3820` | First certification record (superseded by this report) |
| `6a3e945` | **§70 closure** — SERVICE_ACCOUNT and PRINCIPAL quota levels (migration 000033) |
| `741e53a` | Service-account race test asserts the aggregate before the reported level |
| `103c5f3` | ADR 0028 §70 amendment (final limit model), README limits sections |
| `a5fd96e` | Fix: no second pool connection inside a Call-creating transaction (found by the final full suite) |

**Every gate in §4 ran on `a5fd96e`, the final code tree.** The commit carrying this
report changes documentation only. Decision record: ADR 0028.

## 0. Correction of the first report (`73c3820`)

The first report described §70 as **tenant → application → credential (→ principal)** and
counted the existing per-principal / per-service-account **velocity** guards of the spend
policy as those levels. That was incomplete:

* §70 requires *configurable quota levels* for request rate, concurrent calls and telecom
  spend by service account and by principal.
* Velocity guards are risk controls, not quotas.

The closure commit `6a3e945` added both levels across all three limits. The earlier claim
"PART G — CLOSED" from `73c3820` is withdrawn and replaced by this report.

Part E's external item — **real lawful telecom interconnect certification** — remains
pending (ADR 0016). Every public-network figure below (leg usage, cost evidence, spend
limits) is exercised against the simulated Part E adapter and the real-Asterisk / SIPp
carrier simulator only. **No real-carrier result is claimed.**

## 1. Final §70 limit model

* **API-client chain** TENANT → APPLICATION → SERVICE_ACCOUNT → CREDENTIAL.
  * The effective per-credential value is the most restrictive configured value (ties
    name the more specific level), else the platform default.
  * Every configured parent is also an aggregate ceiling across all of its children.
* **PRINCIPAL** — an independent communication-subject axis.
  * It is evaluated only for a principal the server resolved in the caller's scope.
  * It is shared by every credential, service account, device and creation path acting
    for that principal.
* An operation must satisfy every applicable ceiling (plus the environment spend
  policy). The broadest exhausted level is reported.
* **Lock order:**
  * Concurrency takes advisory locks TENANT → APPLICATION → SERVICE_ACCOUNT → PRINCIPAL.
    The application lock also serializes the application's credentials.
  * Spend row-locks the caps tenant → application → service account → credential →
    principal → environment policy.
* **Request rate:** one atomic upsert counts the tenant, application, service account
  and credential windows of every request. The principal window is counted per Call
  created for that initiator, inside the creating transaction.

## 2. Evidence per section

| § | Requirement | Evidence (test → assertion) |
|---|---|---|
| 63 | Versioned `/v1` APIs, contracts authoritative, OpenAPI covers implemented routes, no provider leak | `http_contract::*` (route ↔ OpenAPI scan includes `/v1/webhooks*`, `/v1/webhook-deliveries*`, `/v1/usage`, `/v1/limits` incl. `principalId`, `/internal/v1/platform/events`; responses validated against `platform/webhooks`, `platform/usage`); contract validator: 40 contracts, 164 examples, 255 counter-examples |
| 64 | Authority from authentication only; scoped principals; versioned mutations; audit | `webhooks::subscriptions_are_scoped_versioned_and_never_return_the_secret`; `usage_limits::usage_api_lists_final_facts_of_the_caller_scope_only`; `limits::limits_cannot_be_supplied_by_the_client`; `quota_levels::limits_api_reports_service_account_and_principal_levels_safely` (`serviceAccountId` / `tenantId` / `applicationId` / `credentialId` → 400; another environment's or an unknown principal → 404; no identifier in the view; `principals.read` required) |
| 65 | Idempotency matrix; replay = prior result; different payload = conflict; no duplicate paid execution | create call: `http_contract::call_api_responses_validate_against_the_calling_contract`; callback: `public_callback_relay::a_reused_key_with_a_different_callback_is_a_conflict_and_never_dials`, `concurrent_identical_callback_requests_create_one_call_and_one_paid_leg`; transfer: `app_control`; relay session: privacy-relay `idempotency`; origination / provider command: telephony `telephony_core`, `public_app_to_phone::a_lost_response_is_retried_with_the_same_command_and_never_duplicates`; usage: late usage event adds / changes nothing |
| 66 | Realtime semantics incl. `call.route_changed`; reconnect recovery | `public_failover::*`; `call_history::a_missed_call_survives_push_and_realtime_outage`; `app_ringing` recovery API |
| 67 | Transactional outbox, claim / lease, bounded retry, backoff, dedupe, dead letter, manual replay, broker-neutral | `webhooks::outbox_events_become_signed_public_deliveries`, `transport_outage_keeps_the_outbox_authoritative`; privacy-relay `outbox_publication::committed_events_are_published_once_retried_and_dead_lettered` |
| 68 | Webhooks: scoped subscriptions, HMAC-SHA256 over timestamp + delivery id + body, retries, dead letter, replay, SSRF / rebinding / redirects, secret protection, normalization | `webhooks::*` (8); `cf-webhooks` unit tests (10) |
| 69 | Usage facts (not billing), leg facts with KNOWN / ESTIMATED / UNKNOWN, callback two legs, voicemail / recording artifacts, idempotent finalization, `GET /v1/usage` | `public_callback_relay::the_requester_is_called_first_…`; `public_phone_to_phone::a_protected_caller_reaches_a_protected_callee_through_one_call`; `records::usage_fact_tests::cost_evidence_is_never_invented`; `enterprise_certification::org_isolation_certification`; `usage_limits::usage_api_lists_final_facts_of_the_caller_scope_only` |
| 70 tenant / application / credential | Each level enforced, race-safe | `usage_limits::tenant_concurrency_ceiling_holds_across_applications` (15 rounds × 6, exactly 2); `call_concurrency::*`; `public_spend::part_b_application_and_credential_caps_are_enforced`, `concurrent_reservations_never_exceed_the_limit`; `limits::*` |
| 70 service account — rate | Combined traffic of two credentials of one service account stops at N; another service account unaffected | `quota_levels::service_account_request_rate_is_shared_by_its_credentials` (N = 6 across two credentials: exactly 6 × 200, then 429; a second service account 3 × 200) |
| 70 service account — concurrency | Two credentials, 6 simultaneous creations, ceiling 2, ≥ 15 rounds | `quota_levels::service_account_concurrency_ceiling_holds_across_its_credentials` (15 rounds: exactly 2 every round, refusals at SERVICE_ACCOUNT) |
| 70 service account — spend | Two credentials reserve concurrently; never above the shared cap | `public_spend::service_account_spend_ceiling_is_shared_by_its_credentials` (8 concurrent reservations, cap 4 000 with 1 000 already held: exactly 3 granted, sum = 4 000; another service account unaffected) |
| 70 principal — rate | Principal ceiling across multiple credentials / service accounts | `quota_levels::principal_rate_is_shared_across_credentials_and_service_accounts` (ceiling 3 across two service accounts: ok, ok, ok, RATE_LIMITED, RATE_LIMITED; another principal unaffected). Principal rate is defined over principal-scoped operations (Call creation for that initiator); requests without a communication principal have no principal rate. |
| 70 principal — concurrency | Same principal, multiple credentials, ceiling 1, ≥ 15 rounds | `quota_levels::principal_concurrency_ceiling_holds_across_credentials` (15 rounds × 6 across two service accounts: exactly 1 every round; another principal still creates) |
| 70 principal — spend | Multiple call paths for one principal reserve concurrently | `public_spend::principal_spend_ceiling_holds_across_call_paths` (two service accounts, cap 3 000 with 1 000 held: exactly 2 granted; another principal unaffected; velocity guard unchanged) |
| 70 complete hierarchy | All five configured with different ceilings; most restrictive blocks first; parents not bypassable through child identities | `quota_levels::every_level_is_enforced_and_parents_cannot_be_bypassed` (principal 1 → credential 3 → new credential of the same service account blocked at 4 → new service account blocked by the application at 5 → new application blocked by the tenant at 6); `platform::limits::tests::service_account_level_is_part_of_the_chain` |
| 70 isolation | Sibling service accounts / principals keep their allowances | rate, concurrency and spend tests above (second service account, second principal) |
| 70 limits API | SERVICE_ACCOUNT and PRINCIPAL levels reported; no client authority; unrelated principals concealed | `quota_levels::limits_api_reports_service_account_and_principal_levels_safely`; `usage_limits::limits_api_reports_every_level_and_the_binding_one` |
| 70 events | SA warning once, SA exceeded once per window, principal warning once, principal exceeded once; no identifiers in payloads or metric labels | `quota_levels::service_account_request_rate_is_shared_by_its_credentials` (warning 5/6 once, exceeded once, a new window → a second); `quota_levels::principal_rate_is_shared_across_credentials_and_service_accounts` (warning once, exceeded once at 4); both assert data keys ⊆ {limit, scope, window, threshold, current, ceiling}, no UUID values, no identifier in `/metrics`; `usage_limits::limit_notices_are_emitted_once_per_window` |

## 3. Mutations (one run each, source restored from git)

§70 closure, run on the final code (`a5fd96e`):

| # | Mutation | Target test | Result |
|---|---|---|---|
| M1 | service-account request-rate window no longer compared | `quota_levels::service_account_request_rate_is_shared_by_its_credentials` | **killed** |
| M2 | service-account Call concurrency check removed | `quota_levels::service_account_concurrency_ceiling_holds_across_its_credentials` | **killed** (aggregate assertion: more than 2 created) |
| M3 | principal Call concurrency check removed | `quota_levels::principal_concurrency_ceiling_holds_across_credentials` | **killed** |
| M4 | service-account spend cap lock / check removed | `public_spend::service_account_spend_ceiling_is_shared_by_its_credentials` | **killed** |
| M5 | principal spend cap removed | `public_spend::principal_spend_ceiling_holds_across_call_paths` | **killed** |
| M6 | limits API accepts client-supplied `serviceAccountId` (and tenant / application / credential) | `quota_levels::limits_api_reports_service_account_and_principal_levels_safely` | **killed** (400 assertion) |
| M7 | client-supplied principal trusted without scoped resolution | same | **killed** (404 assertion) |

Earlier Part G mutations (on `73c3820`; the guarded code paths are unchanged): A tenant
filter, B delivery lease, C signature timestamp, D SSRF, E usage uniqueness, F tenant
lock — all **killed**.

## 4. Gates — direct, clean environment, final tree `a5fd96e`

| Gate | Check | Result |
|---|---|---|
| Calling | `cargo fmt --all -- --check` / `check --workspace --locked` / `clippy --workspace --all-targets --locked -D warnings` / `build --workspace --locked` | all exit 0 |
| Calling | `cargo test --workspace --locked --no-fail-fast` (full suite, once, final tree) | **546 passed, 0 failed** (71 test binaries) |
| Calling | migrations 000001–000033 on a fresh database (`calling-migration up` + `status`) and the migration suite (up / down / re-up) | applied; 7 / 7 |
| Privacy Relay | fmt, clippy `-D warnings`, full suite | **126 passed, 0 failed** |
| Telephony | fmt, clippy `-D warnings`, full suite with `TELEPHONY_SIP_CERT_REQUIRED=1` (real Asterisk + SIPp carrier simulator) | **87 passed, 0 failed** (incl. `sip_certification` 25.5 s); `real_interconnect_certification` reports NOT RUN (no carrier configuration) |
| Paid action | reservation / UNKNOWN-outcome / no-duplicate-origination tests | `public_spend` 7 / 7, `public_app_to_phone` (lost response, UNKNOWN), `public_failover`, telephony `telephony_core` — all in the suites above |
| `cf-webhooks` | fmt, clippy `-D warnings`, tests | 10 passed |
| `service-auth` | full suite (all features), fmt, clippy | 58 passed; clean |
| Contracts | `contracts-validate`; validator tests | 40 contracts, 164 examples valid, 255 counter-examples rejected; 17 passed |
| Boundaries | guard + 55 unit / mutation tests | OK — calling 80 tables, meeting-service 40, privacy-relay 22, telephony 17 |
| Not re-run | real-stack RTC (LiveKit) and enterprise SIP certification | no media path changed; `enterprise_sip_certification` skips without its stack configuration and is **not** counted as evidence |

**Defect found and fixed by the final full suite.** The first full run of the closure
tree (`103c5f3`) failed two load tests (`app_races::load_many_calls…`,
`enterprise_load::queue_burst_keeps_fifo_order`) with 503:

* The new principal-rate check read its configuration on a *second* pool connection
  while the Call-creating transaction held one.
* Under a burst, every pooled connection waited in a transaction on the application
  advisory lock, while the lock holder waited for a free connection. The result was pool
  starvation and acquire timeouts.

`a5fd96e` fixed it:

* The principal-rate check now runs on the creating transaction.
* Refusal notices are emitted by a detached task after the refused transaction releases
  its connection.

The complete suite was then re-run on `a5fd96e` with 0 failures. A targeted re-run is not
claimed as the final result.

**GitHub Actions:** NOT RUN. Every workflow run on `staging` (e.g. run `37957497492`,
Calling service, head `73c3820`) ended within about 2 s with no steps, no output and no
downloadable log (HTTP 404): no runner was allocated. This is the external
billing / Actions-availability blocker already recorded for Part F, not a code failure.
The direct certification above is the evidence.

## 5. Known limitations (recorded, not faked)

* A provider cost that arrives after a leg's usage was finalized is not applied (fact and
  settlement are written once); correction belongs to reconciliation / billing.
* Webhook events are stored only when a matching ACTIVE subscription exists at
  publication time.
* Limit notices are best-effort; refusal notices are asynchronous (detached).
  Enforcement never depends on them.
* A principal's refused or failed Call creation does not consume its rate window. Only
  committed creations count.
* Telephony's internal outbox keeps unbounded retries by design (ADR 0028 §67).
* No message broker, billing, invoicing, pricing or developer portal (out of scope).
* **Real lawful telecom interconnect certification: PENDING (Part E, ADR 0016)** — no
  carrier credentials exist in this environment: the `TELEPHONY_REAL_CERT_TRUNK`,
  `TELEPHONY_REAL_CERT_DESTINATION` and `TELEPHONY_REAL_CERT_CALLER_ID` inputs and the
  trunk secret mount are all absent. Public-network flags remain off by default.
