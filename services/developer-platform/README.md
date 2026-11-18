# services/developer-platform — CF Meet Platform Developer Console

The **Developer Console** of the CF Meet Platform: the control plane where a partner manages its
**workspaces** (called *organizations* in paths and fields: `/v1/organizations`,
`organizationId`), projects, environments, service accounts, API keys, scopes, webhooks, usage,
limits, plans and production access
([ADR 0055](../../docs/decisions/0055-developer-platform-and-api-gateway-foundation.md),
[ADR 0061](../../docs/decisions/0061-cf-meet-platform-partner-embedding.md)). It is served through
the API Gateway and is not on the data path of meetings, messaging or calling. The service,
crate and database keep the name `developer-platform`.

* **Authority:** Platform Access is the only authority for tenants, applications,
  environments, service accounts, API credential verifiers, scopes and principals. The
  Developer Platform stores their canonical ids only (`devplat_environment_refs`) and asks
  Platform Access (`platform-identity.read`) to re-validate every forwarded context. It
  never verifies a credential and never issues one.
* **Database:** its own PostgreSQL database, `devplat_` tables, history in
  `developer_platform_seaql_migrations`; additive changes since P9 in a separate gap-closure
  history (`developer_platform_gap_closure_seaql_migrations`, `migration/src/gap/`) that
  `developer-platform-migration up` applies after the main one (`reset` rolls it back first);
  the server refuses to start with pending migrations in either.
* **Retention (P9):** the worker purges webhook delivery / event history, the API usage
  batch ledger and counters, ingestion rejections and the activity feed past their bounds
  (`src/retention.rs`; `DEVELOPER_PLATFORM_RETENTION_*_DAYS`, `0` keeps a class,
  `DEVELOPER_PLATFORM_RETENTION_ENABLED=false` turns it off), at most hourly, in bounded,
  audited batches (`devplat_retention_audit`). Only the worker can delete: the tables' guards
  still refuse every other delete. Unified-ledger facts / rollups, reconciliations and the
  operator audit are not purged (billing evidence: owner decision).
* **Binaries:** `developer-platform` (API), `developer-platform-worker` (own loop:
  heartbeats; Section 50+ jobs plug in), `developer-platform-migration`.
* **HTTP:** `GET /health`, `GET /ready`, `GET /openapi.json` (contract
  `cfmeet.developer-platform.internal-api`) and `/internal/v1/**` (service tokens only):
  `GET /internal/v1/gateway/context` — `developer-request.forward` (API Gateway only),
  signed delegated context required, behind `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`
  (default off).
* **Central webhooks (Section 53):** `POST /internal/v1/platform/events`
  (`developer-event.publish`; the envelope's producer is the authenticated service) ingests
  committed producer events, projects them into the public catalog (allow-listed safe keys
  only), dedupes on (producer, source event id) and fans out to the matching ACTIVE
  subscriptions of the same environment; the worker delivers them signed
  (`cf_webhooks::signing`), SSRF-safe, with bounded retries, dead letters, replay, endpoint
  health and automatic disablement. The customer API is Calling's (`/v1/webhooks*`,
  `/v1/webhook-deliveries/{id}/replay`; served once the API Gateway re-points it with
  `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`); the console manages subscriptions under
  `/v1/environments/{environmentId}/webhooks*`. Behind `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED`
  (default off) and `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` (default `shadow`: nothing is
  sent). Signing secrets are stored sealed only (`DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY*`).
* **Credential lifecycle webhooks (Blueprint Section 15, PLT-008):** the worker sweeps the
  environments subscribed to `credential.expiring_soon` / `credential.expired` (at most once
  per `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_INTERVAL_SECS`, default 3600, across
  instances), reads their credentials from Platform Access (read-only delegation) and emits
  one notice per crossed lead time (`DEVELOPER_PLATFORM_CREDENTIAL_EXPIRY_WARNING_DAYS`,
  default `14,7,1`; only the smallest crossed one) and one `credential.expired` (within
  `DEVELOPER_PLATFORM_CREDENTIAL_EXPIRED_LOOKBACK_DAYS`, default 7) through the central
  webhook outbox (signed, retried, dead-lettered). Deduplication is persisted in
  `devplat_credential_lifecycle_notices` (credential, event type, threshold, expiry) and by
  a deterministic source event id; each notice writes one `SERVICE` activity event
  (`system:credential-lifecycle`). Revoked / superseded / rotated credentials get none.
  `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED=false` turns it off.
* **Notification Service publications (roadmap N7 / N11):** only with
  `CF_NOTIFICATIONS_BASE_URL` (API and worker; unset: nothing is enqueued or sent). The
  transactional outbox `devplat_notification_publications` holds ids-only
  `cfmeet.notifications.event` envelopes built with `packages/notification-client`:
  `developer-platform.credential.expiring-soon` / `.expired` (worker sweep over every
  environment of the platform's projects, same lead times as the webhooks; recipients: the
  organization's ACTIVE owners and admins, `cfmeet-user:<id>`) and
  `developer-platform.production-access.decided` (operator approve / reject transaction;
  recipient: the applicant, else the owners). Deterministic event ids and idempotency keys
  (`devplat-access:<request>:<decision>`, today's Meeting ledger format), batches of ≤ 100
  to `POST /internal/v1/notification-events` signed for `cfmeet.notifications`
  (`notifications-event.publish`), retries with backoff on 5xx / 401 / 403 / 408 / 429,
  dead letter on contract 4xx / `REJECTED`, expiry after 3 days, purge after 30. The
  production-access decision already has a user email (operator console → Meeting Service
  `developer-access-decision`), so it is published only with
  `DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE=notification-service` (default
  `legacy`), and that email must be switched off at the same time.
  `DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE=off` stops the credential notices.
  Customer webhooks are unaffected.
* **Reference reconciliation (DATA-017, report only):**
  `developer-platform-reconcile-references` (also in the image; `cargo run --bin …`) prints
  a JSON report of every foreign reference (Platform Access tenants / applications /
  environments / credentials, Meeting Service accounts) with local invariant breaks and,
  with service authentication configured, organizations / projects / environments
  `MISSING` or `MISMATCHED` at Platform Access; exit 1 when anything needs attention. The
  worker logs the same report every `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS`
  (default 86400, 0 = off). Nothing is repaired or deleted.
* **Service accounts (Section 50):** `/v1/environments/{environmentId}/service-accounts*`
  (list / create / get / suspend / resume) are delegated to Platform Access
  (`/internal/v1/delegation/environments/{id}/service-accounts*`, the only store): OWNER /
  ADMIN manage, DEVELOPER reads, machine credentials need `service_accounts.manage`;
  SANDBOX / TEST only (PRODUCTION: `POLICY_VIOLATION`), never deleted or revoked here, and
  no credential is issued (Section 54).
* **Console UI (frontend-owned):** `apps/web/src/app/dashboard/developers/` is the console
  UI; it will call the published gateway API with the same contracts as every customer
  (see `docs/phases/phase-06/developer-console-boundary.md`).

```bash
make developer-platform-migrate   # services/developer-platform/.env
make developer-platform           # API
make developer-platform-worker    # worker
make developer-platform-test      # TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres
make developer-platform-lint
```
