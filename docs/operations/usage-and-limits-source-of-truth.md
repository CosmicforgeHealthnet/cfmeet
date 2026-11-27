# Usage and limits: the single source of truth (P6 / P14)

## Decision

**The Developer Platform's unified usage ledger is the single source of truth** for platform
usage and platform API limits:

| Figure | Source of truth | Fed by |
|---|---|---|
| API requests per day / per operation | `devplat_api_usage_daily`, API_OPERATION ledger facts | API Gateway (forwarded requests) **and** Calling (its direct routes, P6) through `POST /internal/v1/usage/api-requests` (`developer-usage.record`) |
| API request quota (per minute / per day) | the effective entitlement (`/internal/v1/entitlements/environments/{id}`) | plans, overrides, service-account / credential restrictions |
| Call minutes, recording minutes, storage | ledger facts (`CALL_MINUTE_APP`, `CALL_MINUTE_PUBLIC_NETWORK`, `RECORDING_MINUTE`, `STORAGE_BYTE_TIME`) | Calling's exporter (`CFMEET_USAGE_EXPORT_ENABLED`) over its immutable `call_usage_records` |

The developer console reads these (`/v1/environments/{id}/usage`, `/limits`). After the gateway
switch (`API_GATEWAY_CENTRAL_USAGE_ENABLED=true`) the machine paths `GET /v1/usage` and
`GET /v1/limits` are served by the Developer Platform from the same data in Calling's published
contract shape (`ledger/views.rs` `compat_usage` / `compat_limits`).

Calling remains the **authority of its own records and domain limits**: `call_usage_records`
(the evidence the ledger is built from), concurrent calls, telecom spend and the principal axis.
Those stay at `GET /v1/calling/usage` and `GET /v1/calling/limits`, which no switch re-points.

## Why the figures disagreed, and what changed

1. **Direct Calling traffic was not counted (P6, fixed).** Only gateway-forwarded requests
   reached the API request accounting, so the console's request totals missed every request a
   customer sent straight to Calling. Calling now counts its direct-route requests into the same
   endpoint and format (`services/calling/src/usage/direct.rs`): the direct authentication path
   only, so a gateway-forwarded request is never counted twice. On by default when the
   Developer Platform is a configured target and `CALLING_DIRECT_USAGE_ACCOUNTING=on` (off by default; set with the `developer-usage.record` grant by `activate-usage-export`).
   The Developer Platform grants Calling `developer-usage.record` in the same release step that
   gives Calling its Developer Platform target (`activate-developer-webhooks`).
2. **`/v1/usage` and `/v1/limits` are still Calling's legacy views until the switch.** Before
   `API_GATEWAY_CENTRAL_USAGE_ENABLED` they answer from Calling's own tables:
   * `/v1/usage` lists every Calling usage type, including participant / session connected time
     and artifacts the ledger does not meter; for the metered types the totals equal the
     ledger's (certified by `certification/tests/usage_migration.rs`, shadow comparison);
   * `/v1/limits` `REQUEST_RATE` reports Calling's own per-credential request-rate limit
     (enforced on Calling's direct routes), not the plan entitlement the gateway enforces.
   These are compatibility views, not a second source of truth.

## Open (owner decisions)

* **Flip the switch.** Setting `API_GATEWAY_CENTRAL_USAGE_ENABLED=true` (Part VI rollout step 10)
  makes `/v1/usage` / `/v1/limits` answer from the source of truth everywhere; then
  `CALLING_LEGACY_USAGE_RETIRED=true` retires Calling's legacy views (503, `CF-Usage-Migration:
  moved`). Not changed here: it is a production rollout control.
* **Quota enforcement on direct routes.** Direct Calling requests are now counted but the
  platform quota is enforced only at the gateway; direct routes keep Calling's own request-rate
  limit. Enforcing the plan quota there too (or retiring direct customer access in favour of the
  gateway) is a product decision.
* **Organization-forwarded traffic** (Meeting Service, first-party organizations) is not
  platform API usage and is counted by neither path.
