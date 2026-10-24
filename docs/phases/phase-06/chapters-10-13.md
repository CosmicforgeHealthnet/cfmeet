# Part VI Chapters 10–13 (Sections 54–57) — requirement matrix and evidence

Staging only: nothing here was merged to `main`, deployed, or run against production data
or a live provider. Base: `staging` at `b1318eced4d4f65f237f214e75bdf7e230968f06`.

**DONE** = implemented and proven by the automated tests named in the evidence column.
**LIMIT** = a recorded limitation (not hidden, not claimed).

Test locations used below:

* PA = `services/platform-access/tests/credential_delegation.rs`
* DP = `services/developer-platform/tests/chapters_10_13.rs`
* GW = `services/api-gateway/src/quota.rs` unit tests
* UF = `packages/usage-facts` unit tests
* CERT = `certification/tests/chapters_10_13.rs` (real binaries, real gateway, real
  Platform Access, one database per service)

## Section 54 — service accounts, credentials, scopes, quotas

| Requirement | Status | Evidence |
|---|---|---|
| Credential self-service delegated to Platform Access (sole authority; DP stores no secret / verifier) | DONE | PA `/internal/v1/delegation/environments/{id}/…credentials…`; DP `src/control/credentials.rs`; PA `delegated_credentials_are_issued_once_suspended_resumed_and_revoked`; DP `credentials_are_self_service_with_the_secret_exactly_once`; CERT `external_onboarding_to_production_journey` (secret absent from metadata, list and activity) |
| Secret shown once; idempotent replay never reveals it again (`secretRedacted`), `Cache-Control: no-store` | DONE | DP `credentials_are_self_service_with_the_secret_exactly_once`; CERT `credential_lifecycle_and_caller_quota_journey` |
| List / read metadata, rotate with bounded overlap (org policy caps lifetime and overlap), revoke, expire | DONE | DP same test (policy 30 d / 60 min, `POLICY_VIOLATION` beyond); CERT lifecycle journey (predecessor works during the overlap, revocation → 401) |
| Credential suspension / resumption with immediate enforcement (`credential_suspended`, effective state `SUSPENDED`) | DONE | PA migration `m20261120_000004`, `authentication.rs`; PA test above; CERT lifecycle journey (401 on the very next request, 200 after resume) |
| SANDBOX / TEST / PRODUCTION isolation; PRODUCTION credentials only after activation and only for approved scopes | DONE | PA `scope_grants_and_production_activation_are_delegated_under_trust_rules`; DP `production_access_is_requested_reviewed_approved_activated_and_suspended`; CERT onboarding journey (unapproved `meetings.read` refused in PRODUCTION) |
| Human role authorization (OWNER / ADMIN / DEVELOPER manage; revoke / suspend allowed while RESTRICTED); machines `credentials.manage`, never more than they hold | DONE | DP `src/domain/rbac.rs`; DP credential test; CERT lifecycle journey (machine issues within its scopes, escalation refused, missing scope 403, other tenant 403/404) |
| Scope registry (§54.2) with trust levels; self-service grants STANDARD only; FIRST_PARTY_ONLY only by an explicit operator grant; PRIVILEGED_OPERATOR never | DONE | `GET /v1/scopes`; DP `the_scope_registry_and_grants_follow_trust_levels`; PA trust test; CERT `first_party_fixtures_journey` |
| Quota precedence Organization → Project → Environment → Service account → Credential, most restrictive wins; explicit audited operator override | DONE | DP `quotas_follow_organization_project_environment_service_account_credential_precedence`; DP `src/control/entitlements.rs` |
| Gateway enforcement of caller limits on the existing RPM / daily counters (separate caller keys; levels `SERVICE_ACCOUNT` / `CREDENTIAL`) | DONE | GW `caller_restrictions_have_their_own_counters_inside_the_environment_budget`; CERT lifecycle journey (8 concurrent requests, limit 3 → only the allowed number pass, 429 `x-cf-quota-level: CREDENTIAL`; sibling credential unaffected; service-account limit) |
| Entitlement snapshots, effective-limit reads, domain consumer integration | DONE | `GET /v1/environments/{id}/limits`, `/internal/v1/entitlements/environments/{id}?serviceAccountId&credentialId`; DP quota test; CERT journeys (`limitingLevel`) |

## Section 55 — sandbox, production access, unified usage

| Requirement | Status | Evidence |
|---|---|---|
| Sandbox safety: no real PSTN / SMS / numbers in SANDBOX / TEST | DONE | deterministic SMS provider, synthetic numbering inventory, no Telephony; existing certification `journeys.rs` (synthetic inventory), live telecom only for activated PRODUCTION (PA `/delegation/environments/{id}/live-telecom`) |
| Workflow SANDBOX_READY → PRODUCTION_REQUESTED → UNDER_REVIEW → APPROVED / REJECTED → LIVE / SUSPENDED; customer request / read / cancel; operator review / approve / reject / suspend / reinstate | DONE | DP `src/control/production.rs`; DP `production_access_is_requested_reviewed_approved_activated_and_suspended`, `approvals_expire_rejections_carry_reasons_and_software_only_may_be_automatic`; CERT onboarding journey (full history) |
| Capability-specific approvals (subset approval); software-only may auto-approve (flag); regulated telecom / SMS / numbering need explicit review | DONE | same DP tests; `DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL` (default off) |
| Safe reason codes, audits, no self-approval (`SELF_APPROVAL_FORBIDDEN`, `OPERATOR_REQUIRED`), approval expiry (`APPROVAL_EXPIRED`, worker sweep) | DONE | same DP tests; CERT onboarding journey |
| Delegated Platform Access activation (append-only, idempotent on the approval reference), immediate suspension | DONE | PA `operator_activate_production_environment`; PA test; CERT onboarding journey (suspended PRODUCTION credential refused, sandbox unaffected, reinstated works) |
| Unified ledger, 8 types (API_OPERATION, MEETING_PARTICIPANT_MINUTE, CALL_MINUTE_APP, CALL_MINUTE_PUBLIC_NETWORK, NUMBER_ACTIVE, SMS_SEGMENT, RECORDING_MINUTE, STORAGE_BYTE_TIME) | DONE | `packages/usage-facts` (UF `the_catalogue_has_the_eight_part_vi_types`); DP `src/ledger/` |
| Producers in Meeting, Calling, Numbering, SMS, recording / storage owners and the gateway | DONE | `services/*/…/usage_export.rs`; CERT `usage_ledger_producers_journey` (each type from its owning producer, through the real exporters) |
| Idempotency on (producer, source_fact_id); conflict rejection; durable ingestion and backlog recovery (watermarks held by DP) | DONE | DP `usage_facts_are_ingested_once_rolled_up_queried_exported_and_reconciled`; UF `export_resumes_from_the_acknowledged_watermark_and_skips_unsettled_facts`; CERT usage journey (every stream reset and replayed → totals unchanged; changed payload → `CONFLICTING_REPLAY`, original kept) |
| Rollups and rebuild; backfill; shadow reconciliation | DONE | DP usage test, `gateway_batches_become_api_operation_facts_and_history_is_backfilled`; CERT usage journey (rebuild no-op, reconciliations all `MATCH`) |
| Queries / filters, CSV (formula-safe) and structured export, limits / remaining, warnings (80 % / 100 %) | DONE | DP views unit tests; DP usage test; CERT usage + onboarding journeys (no phone number / object key in customer usage) |
| `/v1/usage` and `/v1/limits` ownership migration with compatibility and rollback | DONE | gateway `API_GATEWAY_CENTRAL_USAGE_ENABLED` re-points Calling's paths to DP (unset = rollback; Calling's routes untouched); CERT onboarding + usage journeys |

## Section 56 — first-party integration

| Requirement | Status | Evidence |
|---|---|---|
| First-party tenants / projects as ordinary tenants with audited classification | DONE | operator `register_first_party`; CERT `first_party_fixtures_journey` (classification `FIRST_PARTY`, same public self-service path) |
| FIRST_PARTY_ONLY grants only by explicit operator grant; refused for external tenants | DONE | DP scope test; CERT first-party journey (self-service 422, operator grant 200, external tenant refused) |
| Cost-centre attribution | DONE | `GET /internal/v1/operator/cost-centers/{cc}/usage`; DP `first_party_usage_carries_cost_centres_and_support_profiles_route_incidents`; CERT first-party journey |
| ≥ 2 isolated first-party fixtures; no product-specific APIs; no cross-service database access | DONE | CERT first-party journey (two products, mutual 403/404); boundary guards `scripts/service-boundaries` |

## Section 57 — external integration journey and support path

| Requirement | Status | Evidence |
|---|---|---|
| Org → project → SANDBOX → service account + scoped credential → public APIs through the gateway, without database edits or engineer-issued credentials | DONE | `certification/src/provision.rs::self_service_credential` (now used by every sandbox journey); CERT onboarding journey |
| Signed webhook delivery and delivery history | DONE | certification `webhooks.rs` (runs on the self-service sandbox credential) |
| Sandbox usage / limits → production request → approval → activation → production credentials and usage | DONE | CERT onboarding journey |
| Support profile: tier / reference, technical contact, incident contact (member references only), production status, last incident reference; read / update APIs; no ticketing | DONE | `src/control/support.rs`; DP first-party test; CERT onboarding journey |

## Operations added (contracts, route manifest, SDKs)

| Surface | Before → after |
|---|---|
| Developer Platform public (`platform-developer/http`, PUBLIC_CUSTOMER) | 46 → 73 (+27) |
| Developer Platform internal / operator (`platform-developer/internal`) | 33 → 52 (+19) |
| Platform Access internal (delegation + admin suspension, `platform-access/internal`) | 53 → 66 (+13) |
| Gateway route manifest PUBLIC_CUSTOMER | 470 → 497 |
| SDK operations (TypeScript / Dart, generated) | 470 → 497 |

Count correction: `chapters-6-9.md` recorded 465 PUBLIC_CUSTOMER / SDK operations; the
generated manifest at the end of Chapters 6–9 held **470** (corrected there).

## Defect found by the certification and fixed

| Defect | Fix |
|---|---|
| Meeting Service usage export enabled without a public-events destination had no Developer Platform target (every pass deferred) | `PlatformConfig` now requires and registers `CF_DEVELOPER_PLATFORM_BASE_URL` when `CFMEET_USAGE_EXPORT_ENABLED` is on |

## Limitations (recorded)

* No deterministic media or carrier path exists in a certification stack: the usage
  journey commits the domain evidence rows into each owning service's database (as that
  service's transaction would) and certifies the unmodified exporters from there.
* Production activation, deployment and live provider traffic were not performed; the
  software behaviour is certified on staging / synthetic fixtures only.
* Producer exporters are off by default (`CFMEET_USAGE_EXPORT_ENABLED`); the gateway
  usage re-pointing is off by default (`API_GATEWAY_CENTRAL_USAGE_ENABLED`).
* Dependency security, RUSTSEC-2026-0387 (`crypto-bigint 0.5.5`, published 2026-10-23):
  * **fixed** for the `jsonwebtoken` path: `packages/service-auth/src/jwt.rs` installs an
    EdDSA + HMAC-only provider (no bundled `rust_crypto` backend, so no RSA / P-256 /
    P-384); tokens are byte-identical (`tests/jws_provider.rs` golden vectors). `crypto-bigint`
    is gone from 10 lockfiles (service-auth, usage-facts, platform-access,
    developer-platform, api-gateway, calling, numbering, sms-gateway, privacy-relay,
    telephony); messaging, meeting-service and certification lost the `jsonwebtoken`
    path but keep the MLS one below.
  * **open (upstream)** for the MLS path in `packages/messaging-crypto` (also locked by
    services/messaging, services/meeting-service, certification and the wasm /
    web-harness builds): `openmls_rust_crypto` 0.6.0 and `openmls_basic_credential` 0.6.0
    (latest releases) require `p256` / `p384` 0.13 → `crypto-bigint` 0.5. The profile uses
    only `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. Any dependency change there also
    changes the committed browser artifact (`apps/web/public/messaging-crypto`, checked
    byte-for-byte by web-crypto-artifact.yml), which is outside this backend scope. No
    ignore entry was added.
* Developer Console UI (§18) unchanged; `apps/web`, `frontend.yml` and Vercel untouched.
