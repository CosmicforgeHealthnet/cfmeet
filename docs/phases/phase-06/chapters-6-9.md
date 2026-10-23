# Part VI Chapters 6–9 (Sections 50–53) — requirement matrix and evidence

Staging only. Nothing here was merged to `main`, deployed, or executed against production
data. Foundation (Chapters 1–5) baseline: `42f45764a5ae493060470c92ea6de7d91d524b48`.

Status legend: **DONE** = implemented and proven by automated tests (the evidence column
names them); **DEPENDENT** = the requirement belongs to, or needs, a later Part VI section
(54–57, 18) and is recorded, not claimed; **OPEN** = required here and not yet complete.

## Section 50 — Multi-tenant Developer Platform (`services/developer-platform`)

| Requirement | Status | Evidence |
|---|---|---|
| Organization = exactly one Platform Access tenant; DP stores membership / commercial metadata keyed to `tenant_id`; no competing identity | DONE | `m20261102_000002_control_plane.rs`; `tests/control_plane.rs`; `tests/foundation.rs::the_schema_owns_only_devplat_tables_and_no_identity_hierarchy`; boundary guard `IDENTITY_HIERARCHY_DUPLICATED` |
| Project = one Platform Access application (created by delegated, audited Platform Access call); environments SANDBOX / TEST / PRODUCTION preserved (PRODUCTION never self-created) | DONE | `services/platform-access/src/http/delegation.rs`, `tests/delegation.rs`; DP `src/control/projects.rs`; certification `production_approval_journey` |
| Service accounts: DP delegates create / list / suspend / resume to Platform Access (no delete; PRODUCTION refused; OWNER / ADMIN manage, DEVELOPER reads; machine scope `service_accounts.manage`) | DONE | Platform Access `/delegation/environments/{id}/service-accounts*` (`tests/delegation.rs`); DP `src/control/service_accounts.rs`, `tests/control_plane.rs`; certification `service_accounts.rs::service_account_delegation_journey` (suspended service account's credential refused at the gateway, works again after resume) |
| API credentials: secret creation / rotation / revocation stay inside Platform Access | DONE (boundary) / DEPENDENT (self-service UI/API is §54) | DP stores no credential, verifier or secret (`tests/foundation.rs`, architecture test); journeys issue credentials through the Platform Access operator API |
| Roles OWNER / ADMIN / DEVELOPER / ANALYST / BILLING with the §50 matrix | DONE | `src/domain/rbac.rs` (unit tests), `tests/control_plane.rs`, console webhook RBAC in `tests/webhooks.rs` |
| Invitations (token digest only), acceptance, expiry, revocation, role change, removal, ownership transfer, last-owner protection | DONE | `src/control/members.rs`; `tests/control_plane.rs` (incl. concurrent demotion) |
| Organization states INVITED / ACTIVE / RESTRICTED / SUSPENDED / CLOSED, enforced in the control plane and at machine authentication (suspension withdraws Platform Access authority first) | DONE | `OrganizationState` in `src/domain/rbac.rs`; `tests/control_plane.rs`; certification `first_party_product_journey` |
| Session-to-org bridge on the existing CF Meet session; no machine secret in the browser | DONE | Meeting Service `POST /internal/v1/user-sessions/verify` (`tests/session_bridge.rs`); DP `src/session.rs`; certification sandbox journey (register → session → organization) |
| Machine callers through the gateway; credential re-verified live on every request | DONE | `src/http/public.rs` (`live_credential`); `tests/control_plane.rs` (revoked credential refused on every route); `tests/foundation.rs`; certification `credential_revocation_journey` |
| Project metadata, stricter project entitlements; archive / suspend never deletes domain data | DONE | `tests/control_plane.rs`; entitlements `src/control/entitlements.rs` |
| Explicit, audited first-party classification; FIRST_PARTY_ONLY scopes; externals can never self-grant privileged scopes | DONE | operator `register_first_party`; certification `first_party_product_journey` |
| Commercial plans (versioned), assignments with immutable history, billing-account references, internal cost centers; no payments / invoices / tax / balances | DONE | `src/control/commercial.rs`; `tests/control_plane.rs`; architecture test (forbidden payment vocabulary) |
| Immutable activity history (tenant, actor, action, target, request / correlation id, outcome, safe summary; secret-looking keys refused) | DONE | `devplat_activity_events` guards; `tests/control_plane.rs` |
| Contracts with pagination, idempotency (`Idempotent-Replayed` on replay), safe errors, tenant isolation (non-members get NOT_FOUND) | DONE | `packages/contracts/platform-developer/{http,control,common,internal}`; `tests/control_plane.rs`; certification `tenant_isolation_journey`; SDK parity `isolation.otherTenant`, `developerPlatform.session.idempotentCreate` |
| Production-access request / approval workflow | DEPENDENT (§55) | PRODUCTION creation is refused for customers (422 `POLICY_VIOLATION`); operator-created PRODUCTION works (`production_approval_journey`) |

## Section 51 — API Gateway and service interfaces (`services/api-gateway`)

| Requirement | Status | Evidence |
|---|---|---|
| Public routing for Meetings (incl. `/api/v2`), Calling, Privacy Relay (no Contact Vault), Messaging, Numbering (customer `/v1/numbers` only), SMS, Developer Platform | DONE | `infra/api-gateway/route-manifest.json` (465 PUBLIC_CUSTOMER operations); `tests/routing.rs::the_generated_catalogue_is_in_parity_with_the_contracts`; certification `external_developer_sandbox_journey` |
| Route manifest generated from contracts + explicit allow-list, 5 classes, default deny; CI fails on drift or internal / admin / callback leakage | DONE | `scripts/contracts/public_api.py --check` (contracts.yml, api-gateway.yml); `infra/api-gateway/public-routes.json`; `tests/routing.rs`; `tests/ingress.rs` |
| Platform Access credential authentication; 60-second signed delegated context with the credential id; receivers re-verify live and authorize their own routes | DONE | `src/identity.rs`; Platform Access `/credentials/delegated-authority`; receiver tests: Calling `tests/gateway_forwarding.rs`, Messaging `tests/gateway_forwarding.rs`, Numbering `tests/service.rs`, SMS `tests/gateway.rs`; certification revocation journey |
| Client authority-header stripping; spoofing, cross-environment and bypass refusals | DONE | `src/headers.rs`; `tests/ingress.rs`; `tests/platform_access_e2e.rs` (forged headers / tokens) |
| Forwarding (streaming), body / header / URI limits, CORS (exact origins), per-route timeouts, request / correlation ids, health / readiness / discovery, family kill switches, rollback | DONE | `src/http/{mod,ingress}.rs`; `tests/ingress.rs` |
| RPM / daily quotas on shared atomic Redis counters (aggregate across services), per-instance fallback, durable daily accounting exactly once | DONE | `src/quota.rs` (Redis test with two instances), `tests/ingress.rs`, DP `tests/control_plane.rs::api_request_usage_batches_are_counted_exactly_once`, certification `quota_enforcement_journey` |
| Normalized API_OPERATION usage after final response classification | DONE | gateway `record_operation`; DP `m20261110_000010_api_operation_usage`; `tests/ingress.rs`, `tests/platform_access_e2e.rs` (real processes) |
| Meetings through the gateway with platform credentials (21 v2 operations; `meetings.read` / `meetings.write`; meetings owned by an application environment; `meeting-request.forward`, live re-verification) | DONE | Meeting Service `src/middleware/platform_auth.rs`, `src/services/platform/operations.rs`, `tests/platform_access.rs`; migration `m20261101_000001_platform_environment_meetings`; certification `meetings_platform.rs` |
| WebSocket forwarding: Meeting realtime, Calling signaling, Messaging realtime; tickets transported, never translated; clean close handshake | DONE | `src/ws.rs`; `tests/websocket.rs`; certification `websocket_gateway_journey`; SDK parity realtime scenarios |
| Public per-domain and aggregate OpenAPI; public Swagger page; engineering Swagger selector preserved | DONE | `packages/contracts/bundled/public/**`; `src/docs.rs` (parity test); Swagger render test (all bundles, Chromium) |
| Central-webhook switch (`/v1/webhooks` → Developer Platform) | DONE | `tests/central_webhooks.rs`; certification `gateway_webhook_switch_journey` |

## Section 52 — SDKs (`sdks/`)

| Requirement | Status | Evidence |
|---|---|---|
| Generated only from public contracts; generated vs curated separation; CI drift detection; compatibility manifest (pinned contract versions) | DONE | `sdks/generate.py --check` (465 operations, 12 pinned contracts); `sdk-typescript.yml`, `sdk-dart.yml` |
| Auth helpers; browser-safe (platform credentials refused in browser runtimes by default) | DONE | TS `test/client.test.ts`; Dart `test/policy_test.dart`, `tool/web_probe.dart` |
| Typed errors with request ids; timeouts / cancellation | DONE | both client test suites |
| Retries with Retry-After; idempotency keys reused on retry; paid telecom mutations NEVER retried automatically (`UnknownOutcomeError`) | DONE | `sdks/generation.json` `paidTelecomOperations`; client tests; SDK parity `retry.neverPaidTelecom` (exactly one request through the real gateway) |
| Cursor pagination | DONE | client tests; SDK parity `pagination.cursor` |
| Realtime tickets + reconnecting WebSockets | DONE | realtime tests; SDK parity realtime scenarios (incl. meetings resume) |
| Webhook signature verification over vectors generated by the reference signer | DONE | `packages/webhooks/tests/sdk_vectors.rs` → `sdks/testdata/webhook-signature-vectors.json`; both SDK webhook tests |
| TypeScript integrates the existing Messaging crypto WASM | DONE | `src/messaging-crypto.ts`; tests against the shipped, SRI-verified artifact; SDK parity `messaging.e2ee` |
| Dart native bridge to the Rust crypto core (no MLS in Dart; private keys stay native) | DONE | `packages/messaging-crypto/ffi` (C ABI, 15 tests); Dart `test/messaging_crypto_test.dart`; cross-SDK MLS interop `messaging.e2ee.crossSdk` |
| Deterministic test fakes | DONE | `package:cfmeet_platform_sdk/testing.dart`, `@cfmeet/platform-sdk/testing`; `testing_fakes_test.dart`, `testing.test.ts` |
| Both SDKs pass the same published scenarios against the real gateway and services | DONE | `sdks/testdata/parity-scenarios.json` (17); `certification.yml` `sdk-parity` job |
| Typed Usage client | DEPENDENT (§55 public usage API) | — |

## Section 53 — Webhook platform (`services/developer-platform/src/webhooks`)

| Requirement | Status | Evidence |
|---|---|---|
| Subscriptions / events / deliveries / attempts, endpoint health, durable worker, dead letter, replay, immutable history; reuse `packages/webhooks` (no second scheme) | DONE | `m20261104_000004_webhooks.rs`; `tests/webhooks.rs` (13); architecture test (only `cf_webhooks` crypto) |
| Lifecycle API (create / list / get / update / delete / rotate / deliveries / replay); secrets server-generated, sealed, revealed only on create / rotate; `webhooks.read` / `webhooks.manage` + domain read scopes | DONE | `src/webhooks/http.rs`; `tests/webhooks.rs`; certification `external_developer_signed_webhook_journey` |
| Sandbox / production isolation | DONE | `tests/webhooks.rs::environments_and_tenants_are_isolated`; certification signed-webhook journey |
| Ingestion from domains via outboxes with the service-auth envelope; dedupe on (producer, source event id); producer must be the caller | DONE | ingest + producers (`22091fa`): SMS, Privacy Relay, Messaging, Calling, Numbering tests; certification journeys (spoofing, duplicates) |
| Safe projections (no provider ids, numbers, content, secrets) | DONE | `src/webhooks/catalog.rs`; `tests/webhooks.rs`; certification payload assertions |
| Signature format preserved; retries (incl. Retry-After), SSRF and DNS-rebinding protection | DONE | `cf_webhooks::retry::next_delay`; `tests/webhooks.rs`; certification `webhook_failure_and_replay_journey`, `webhook_retry_after_is_honoured`, `webhook_ssrf_policy_journey` |
| Meetings event family (`meeting.*`, `participant.*`, `recording.*` / `artifact.*`) for environment-owned meetings, read with `meetings.read` | DONE | Meeting Service `src/services/platform/events.rs`, `tests/platform_events.rs`; DP catalog (`meeting.created`, `conferenceId`); certification `meetings_platform.rs` (signed `meeting.started` delivered, other tenant never receives it) |
| Calling → Developer Platform migration: inventory / dry-run, freeze, reconcile, shadow, no duplicates, flagged `/v1/webhooks` switch, rollback, retire; secret re-encryption under controlled custody (rotation required when continuity cannot be proven) | DONE (tooling; no production run) | Calling `src/webhooks/ownership_migration.rs`, `tests/webhook_migration.rs`; DP `src/webhooks/migration/`, `tests/webhook_migration.rs`; certification `webhook_migration_journey`, `shadow_mode_no_double_delivery_journey`; runbook [`webhook-migration-runbook.md`](webhook-migration-runbook.md) |

## Cross-chapter certification (`certification/`, real processes)

Platform Access, Developer Platform (+ worker), API Gateway, Meeting, Calling, Messaging,
Numbering, SMS Gateway and Privacy Relay run as real binaries with their own databases and
migrations, Redis, deterministic provider adapters and registry-derived trust bundles.

| Journey | Evidence |
|---|---|
| External developer — sandbox | `journeys.rs::external_developer_sandbox_journey` |
| Production approval boundary | `journeys.rs::production_approval_journey` |
| Internal first-party product | `journeys.rs::first_party_product_journey` |
| Credential revocation | `journeys.rs::credential_revocation_journey` |
| Quota enforcement (aggregate) | `journeys.rs::quota_enforcement_journey` |
| Tenant isolation | `journeys.rs::tenant_isolation_journey` |
| WebSocket gateway | `journeys.rs::websocket_gateway_journey` |
| Signed webhooks across families | `webhooks.rs::external_developer_signed_webhook_journey` |
| Webhook failure / replay / Retry-After | `webhooks.rs::webhook_failure_and_replay_journey`, `webhook_retry_after_is_honoured` |
| Shadow mode, no double delivery | `webhooks.rs::shadow_mode_no_double_delivery_journey` |
| Gateway webhook switch | `webhooks.rs::gateway_webhook_switch_journey` |
| SSRF on the real process | `webhooks.rs::webhook_ssrf_policy_journey` |
| Webhook migration | `webhook_migration.rs::webhook_migration_journey` |
| Service-account delegation | `service_accounts.rs::service_account_delegation_journey` |
| Meetings with platform credentials + meeting webhooks | `meetings_platform.rs` (2 journeys) |
| SDK parity (TypeScript and Dart) | `certification.yml` `sdk-parity` job over `sdks/testdata/parity-scenarios.json` |

## Defects found by the certification and fixed

| Defect | Fix |
|---|---|
| DP never registered the Meeting Service peer: every console session refused | `2b21246` |
| Gateway WebSocket relay reset connections instead of completing the close handshake; Calling signaling dropped sockets (1006) | `091978a` |
| Meeting realtime tickets documented one-shot but reusable | `69ba7ea` |
| DP accepted a replayed gateway hop of a revoked credential for up to 60 s | `a52ef49` |
| DP idempotent replays lacked `Idempotent-Replayed` | `ecb87f6` |
| Dart SDK lost meeting events across a reconnect | `a7915ff` |
| Both webhook delivery workers ignored `Retry-After` | `13e6d1d` + worker wiring |
| Unknown environment kind received production limits | `cb027a3` |

## DEPENDENT items (later sections, not claimed here)

* §54: credential self-service (issue / rotate / revoke through the console API).
* §55: production-access request / approval workflow, public usage / limits API and the
  usage ledger beyond API_OPERATION counts, typed Usage SDK client.
* §18: Developer Console UI (`apps/web` unchanged; see
  [`developer-console-boundary.md`](developer-console-boundary.md)).
* Deterministic Calling media adapter and an SMS sandbox test number (journeys exercise the
  refusals instead; both domains' own suites cover the event flows).

## Open items

None within Sections 50–53. The DEPENDENT items above belong to later sections.

## Known limitations (recorded, not hidden)

* Meeting chat (Unified Messaging) is unavailable for machine-owned meetings (no owner
  principal): it fails closed with `CHAT_UNAVAILABLE`.
* Without provider room control the deterministic stack cannot end a conference, so the
  certification journey observes `meeting.started`; `meeting.ended` and participant events
  are covered by the Meeting Service suites.
* Meeting recordings / artefacts are not readable by machine callers; their readiness
  events carry ids only.
* Production webhook migration and cutover were NOT executed (no deployment
  authorization); the runbook documents the procedure and rollback.
