# Part VI Chapter 19 — migration and compatibility (backend)

Staging only. Starting SHA `4d92f24820ab3f29fa41b6658ffb3bad9e83650d`. Nothing was merged
to `main`, deployed or cut over: every production step below is
**AWAITING_AUTHORIZED_CUTOVER**.

Test locations: CERT = `certification/tests/` (real processes), DP =
`services/developer-platform/tests/`, CALL = `services/calling/tests/`.

## 19.1 Baseline and migration readiness

| Requirement | Status | Evidence |
|---|---|---|
| Deployed schema assumption: the newest pre-Part-VI migration of each service is the deployed one; Part VI migrations are exactly the newest ones, in order | COMPLETE (**NEW**) | CERT `schema_upgrade.rs` asserts the pending list per service (fails if a later migration is added without extending it) |
| Upgrade from a representative pre-Part-VI schema with data preserved | COMPLETE (**NEW**) | CERT `schema_upgrade.rs`: Calling, Meeting Service, Messaging, Platform Access, Privacy Relay, SMS Gateway — seeded rows, digest of every pre-Part-VI column unchanged after `up` |
| No historical re-delivery after the outbox outcome columns appear | COMPLETE (**NEW**) | same test (published / relayed rows stay published); publishers only claim `PENDING` rows (`calling/src/webhooks/worker.rs`, `sms-gateway/src/infra/outbox.rs`, `privacy-relay/src/events.rs`, `messaging/src/webhook_relay.rs`) |
| Re-runnable / guarded | COMPLETE (**NEW**) | same test: second `up` is a no-op; `down` of the Part VI migrations succeeds with no Part VI data and refuses (data intact) once Part VI data exists (Meeting environment-owned meeting, Platform Access suspension audit) |
| Developer Platform migrations (new service) up / reset / up, down refuses to drop history | COMPLETE | DP `foundation.rs::migrations_apply_roll_back_and_reapply…` |
| No cross-service database coupling in any migration path | COMPLETE | webhooks move over Calling's internal export API, usage over the producers' exporters; boundary guard `make service-boundaries` |
| Dependency on current `main` | RECORDED | [`baseline-and-branch-reconciliation.md`](baseline-and-branch-reconciliation.md) |

## 19.2 Credential and identity continuity

| Requirement | Status | Evidence |
|---|---|---|
| Platform Access is the only machine-identity authority; the Developer Platform stores references only | COMPLETE | DP `foundation.rs` schema test; boundary guard `IDENTITY_HIERARCHY_DUPLICATED` |
| Rotation, suspension, revocation and expiry behave identically before, during and after a migration switch and its rollback; no credential is invalidated by the switch | COMPLETE (**NEW**) | CERT `usage_migration.rs::usage_migration_rehearsal_journey` (rotated predecessor refused, successor works, suspended / revoked refused, expiring credential works until expiry, untouched credential works — on the moved route and on a Calling route, through both gateway restarts) |
| Upgrade never suspends, activates or alters a credential | COMPLETE (**NEW**) | CERT `schema_upgrade.rs` (Platform Access case: verifier bytes, status and validity unchanged; no suspension / activation rows) |
| Secrets never logged or exported | COMPLETE | webhook migration tests below; secret scan |

## 19.3 Central webhook migration (Calling → Developer Platform)

Audited against all twelve required mechanisms; the implementation and runbook
([`webhook-migration-runbook.md`](webhook-migration-runbook.md)) were complete. Re-run on the
final integrated SHA (see [`chapter-20-certification.md`](chapter-20-certification.md)).

| # | Mechanism | Evidence |
|---|---|---|
| 1 | Bounded mutation freeze on both sides (`CALLING_WEBHOOK_MUTATIONS_FROZEN`, `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN`; 503 + `CF-Webhook-Migration: frozen`, reads continue) | CALL / DP `webhook_migration.rs`; CERT `webhook_migration.rs` |
| 2 | Ids, endpoints, filters, status and delivery history preserved (same ids / versions, `origin = MIGRATED_FROM_CALLING`) | DP `webhook_migration.rs` (import twice = no duplicates; digest reconcile) |
| 3–4 | Secrets re-sealed under a transfer key bound to subscription and direction; plaintext only in memory; no plaintext logs, rows or files | CALL / DP tests (no plaintext in responses / rows / logs) |
| 5 | Durable checkpoints, crash-resume to an identical result | DP `webhook_migration.rs` (crash after N pages) |
| 6 | Shadow publication and parity report | CERT `webhooks.rs::shadow_mode_no_double_delivery_journey`; DP shadow report |
| 7, 12 | No duplicate customer delivery: DP never sends a migrated subscription's delivery before `takeover`; takeover requires Calling delivery off and de-duplicates each logical delivery to one sender | DP takeover de-duplication test; CERT `webhook_migration.rs` |
| 8 | Existing `/v1/webhooks` clients unchanged (same ids / versions through the gateway) | CERT `webhooks.rs::gateway_webhook_switch_journey` |
| 9 | Feature-flagged ownership switch (`API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`) | same |
| 10 | Independent rollback to Calling (`rollback-export`, `rollback`) | DP rollback round trip; CERT `webhook_migration.rs` (Calling delivers again with the secret rotated at the DP) |
| 11 | Failed secret transfer → `ROTATION_REQUIRED`, imported disabled with no substitute secret, explicit operator acknowledgement | DP `webhook_migration.rs` (rotation-required, transfer-key mismatch) |

All rehearsals use synthetic subscriptions, isolated databases and test keys.

## 19.4 Unified usage / limits migration

**Defect found and fixed.** With `API_GATEWAY_CENTRAL_USAGE_ENABLED=true` the gateway
re-pointed `GET /v1/usage` and `GET /v1/limits` to the Developer Platform, which answered a
different shape than the published contract (`cfmeet.calling.http-api` `listUsage` /
`getLimits`, `platform/usage` `UsageList` / `Limits`): normalized usage types, no
`callId` / `resourceClass`, limit names outside the enum, and Calling's `resourceClass` /
`principalId` parameters refused. Generated SDK decoders and existing clients would have
broken at cutover. The earlier "DONE" in [`chapters-10-13.md`](chapters-10-13.md) is
superseded by this section.

Fix (contract-preserving):

* The Developer Platform answers both paths in the published shape
  (`services/developer-platform/src/ledger/views.rs` `compat_usage` / `compat_limits`):
  Calling's metered call facts projected from the ledger (`CALL_MINUTE_APP` →
  `CALL_CONNECTED_TIME`, `CALL_MINUTE_PUBLIC_NETWORK` → `LEG_CONNECTED_TIME`; same record
  id, call / leg id, quantity, route class, destination country, billable seconds, cost
  evidence, recording time and order), Calling's filters, paging and `VALIDATION_ERROR`
  rules; `/v1/limits` reports `REQUEST_RATE` from the effective plan entitlement.
* Calling's full detail (participant / session / voicemail / recording facts, concurrent
  calls, telecom spend, the PRINCIPAL axis) is preserved for existing clients at the new
  service-specific paths `GET /v1/calling/usage` and `GET /v1/calling/limits`
  (`cfmeet.calling.http-api` 1.13.0), never re-pointed. Public operations 497 → 499;
  manifest, public OpenAPI, bundles and both SDKs regenerated.
* `cfmeet.developer-platform.internal-api` 1.8.0 declares the real response schemas of
  the forwarded paths (it said `{"type": "object"}`).
* Deprecating `/v1/usage` and `/v1/limits` waits for telemetry (spec §19.4 item 21): not done.

| Requirement | Status | Evidence |
|---|---|---|
| Domain usage records stay authoritative | COMPLETE | CERT `usage_migration.rs` (Calling's records unchanged by metering, backfills and switches) |
| Backfill from immutable domain facts with producer / source ids | COMPLETE | same (exporter backfill window, source id = Calling record id); CERT `chapters_10_13.rs::usage_ledger_producers_journey` (all producers) |
| No double counting on repeated backfill | COMPLETE | CERT `usage_migration.rs` (every Calling stream reset twice: totals unchanged, 3 facts for 3 metered records); producers journey (all streams) |
| Reconciliation by tenant / environment / type / period | COMPLETE | CERT `usage_migration.rs` (legacy vs ledger totals per type, plus the historical day window); producer reconciliations (`MATCH`) |
| Duplicate, delayed, out-of-order, conflicting facts | COMPLETE | DP `chapters_10_13.rs`, `webhooks.rs`; CERT producers journey (`CONFLICTING_REPLAY` recorded, original kept) |
| Daily rollups rebuildable | COMPLETE | CERT producers journey; load profile `rollup-rebuild-under-load` (chapter 20) |
| Shadow comparison of old vs central responses | COMPLETE (**NEW**) | CERT `usage_migration.rs`: central `/v1/usage` records field-for-field equal to Calling's for every metered type |
| `/v1/usage` and `/v1/limits` compatibility | COMPLETE (**NEW**) | DP `chapters_10_13.rs` (schema-validated `UsageList` / `Limits`, filters, paging, validation); CALL `usage_limits.rs` (legacy paths identical) |
| Legacy routes preserved | COMPLETE (**NEW**) | `/v1/calling/usage`, `/v1/calling/limits` |
| Independent enable / disable, rollback without losing facts | COMPLETE (**NEW**) | CERT `usage_migration.rs` (gateway restarted ON then OFF: legacy view restored exactly, ledger fact count unchanged) |
| Metering triggers no customer mutation | COMPLETE (**NEW**) | same (no webhook delivery, no domain record change) |
| All eight usage types | COMPLETE | CERT `chapters_10_13.rs::usage_ledger_producers_journey` |

## 19.5 Frontend boundary

No frontend change. [`developer-console-api-handoff.md`](developer-console-api-handoff.md)
updated for the changed contract (the console should read the unified views; the
compatibility and legacy paths are machine APIs).

## Production-only actions (AWAITING_AUTHORIZED_CUTOVER)

Transfer-key provisioning, freeze windows, import / takeover / gateway switches for
webhooks; enabling exporters and `API_GATEWAY_CENTRAL_USAGE_ENABLED` for usage; retiring
Calling's webhook authority; deprecating legacy usage paths after telemetry.
