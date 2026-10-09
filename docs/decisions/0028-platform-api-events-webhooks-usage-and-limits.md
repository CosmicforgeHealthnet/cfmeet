# ADR 0028: Platform API authority, durable events, customer webhooks, usage and limits

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part III Part G §63–§70. Implemented in `packages/webhooks`
  (`cf-webhooks`: signing, endpoint / SSRF policy, retry classification, secret sealing),
  `services/calling/src/webhooks/` (outbox publisher, public event catalog, subscriptions,
  delivery worker, internal ingest, limit notices), `services/calling/src/usage/`
  (`GET /v1/usage`, `GET /v1/limits`), `services/calling/src/platform/limits.rs`,
  `services/calling/src/telephony/spend.rs`, `services/privacy-relay/src/events.rs`;
  migrations Calling 000031–000032 and Privacy Relay 000012; contracts
  `platform/webhooks/v1`, `platform/usage/v1`, `calling/http/v1.openapi.json`,
  `calling/realtime/v1`. Builds on ADRs 0003–0006, 0009, 0010, 0013–0017 and 0026.

## Context

Before Part G, every Calling and Privacy Relay mutation already wrote immutable domain
events plus a transactional outbox row in the same transaction, but **nothing published
them**. Customers had no webhooks and no usage or limit read model. Usage existed only
for application media, public-network legs had spend settlement but no usage fact, and
stored artifacts had neither. Limits were enforced per application and credential
(rate, concurrency, spend), with no tenant level and no customer-visible status. Part G
must close these gaps without building billing, a broker deployment or a developer
portal. It must also never leak provider or personal data.

## Decision

### §63–§64 API surface and authority

* Every new route is versioned (`/v1/webhooks…`, `/v1/webhook-deliveries…`, `/v1/usage`,
  `/v1/limits`). `packages/contracts` stays authoritative: OpenAPI operations
  (`x-cfmeet-required-scope`) plus JSON-schema bodies with examples and counter-examples.
  The route-to-contract test covers every implemented route.
* Authority comes only from the authenticated credential. Tenant, application and
  environment are never body or query fields (`deny_unknown_fields`, unknown query
  parameters are 400). Another scope's resource is 404, never 403. New scopes:
  `webhooks.read`, `webhooks.manage`, `usage.read`. Subscribing to an event type also
  needs that type's read scope (`calls.read` for `call.*`, `usage.read` for `limit.*`, …).
* Subscription mutations carry `expectedVersion`; every mutation (create, update,
  delete, rotate-secret, replay) is audited through the Part F audit log, with request
  and correlation ids.

### §65 Idempotency matrix (existing guarantees, verified in Part G)

| Operation | Key space | Replay | Different payload | Paid execution |
|---|---|---|---|---|
| Create Call (incl. callback) | (credential scope, `Idempotency-Key`) + payload fingerprint | original Call | 409 `IDEMPOTENCY_CONFLICT` | one reservation set, one originate |
| Transfer | (Call, `Idempotency-Key`) | original transfer | 409 | — |
| Privacy Relay session | (requester scope, key) | original session | 409 | — |
| Calling → Telephony originate | `originate:{leg}:{attempt}` (UUID-scoped, globally unique) | original command | Telephony 409 (requester, key, digest) | one provider call; an unknown outcome is reconciled, never retried blindly |
| Provider command | Telephony (requester, key) + digest | original | 409 | — |
| Usage fact | (Call, `leg-usage:{leg}` / `{artifact}-artifact:{id}`) | no-op | ignored (facts are immutable) | settlement once per leg |

### §66 Realtime

`call.route_changed` joins the realtime catalog: it is emitted to the Call's endpoints when
public-network failover moves the Call to another route (`data: {"failover": true}`, no
route or provider detail). Reconnect recovery stays as it is: invitation recovery API and
LISTEN/NOTIFY fan-out, with Calls and invitations as the source of truth.

### §67 Durable events

* **Outbox publisher.** Calling and Privacy Relay outboxes gain `lease_until` and
  `last_error_code`, and their status adds `DEAD_LETTER`. Workers claim with
  `FOR UPDATE SKIP LOCKED` plus a lease, and a crashed worker's lease expires and the row
  is reclaimed. Completion is guarded by status and attempt count. Retries are bounded
  (10) with backoff; then the row is DEAD_LETTER, visible, and replayable by an operator
  (`DEAD_LETTER → PENDING`).
* **Broker-neutral transport.** `EventTransport` is the seam. The implementation shipped
  today, `LocalWebhookTransport`, projects envelopes into the public catalog inside
  Calling. A broker (Kafka / RabbitMQ) is **not** built. It would be another
  `EventTransport`.
* **Privacy Relay → Calling.** Privacy Relay's publisher pushes envelopes to
  `POST /internal/v1/platform/events`, signed with internal service auth. The permission
  `platform-event.publish` is receivable by Calling and grantable only to Privacy Relay,
  and the producer must equal the authenticated service. Payloads are bounded at 16 KiB.
* **Telephony's outbox is intentionally unchanged.** It already claims with a lease and
  retries **without a bound**. It is the ordered internal channel of per-leg execution
  facts to Calling, which deduplicates and orders them per leg. Dead-lettering a leg
  event would leave a Call's state wrong, so a stuck event stays visible as backlog
  (`telephony_outbox_backlog`) and is an operational incident, not a silent drop.

### §68 Customer webhooks

* **Catalog.** There are 34 public event types (`call.*`, `voicemail.*`, `recording.*`,
  `relay.*`, `usage.recorded`, `limit.warning`, `limit.exceeded`), each requiring a
  scope. Events are **projected** from committed domain events, never forwarded:
  `data` carries only allow-listed keys whose values are UUIDs, bounded upper-case codes
  or integers. Provider callbacks, Telephony payloads and Privacy Relay enforcement
  events are never forwarded.
* **Storage.** `webhook_events` is unique per (producer, source event) and is written
  only when an ACTIVE subscription wants the type. It fans out to `webhook_deliveries`,
  one per (subscription, event); a replay is a new delivery linked by `replay_of`.
  `webhook_delivery_attempts` is immutable history.
* **Signature.** The headers are:
  ```text
  CF-Webhook-Id:        <delivery id>
  CF-Webhook-Event-Id:  <event id>          (stable across retries and replays; dedupe key)
  CF-Webhook-Timestamp: <unix seconds>
  CF-Webhook-Signature: v1=hex(HMAC-SHA256(secret, "v1:" + timestamp + ":" + delivery id + ":" + raw body))
  ```
  Receivers compare in constant time and refuse timestamps outside ±300 s. The reference
  verifier is `cf_webhooks::signing::verify`.
* **Secrets.** Secrets are generated server-side (`whsec_` + 256 bits) and returned
  **only** on create and rotate. They are sealed at rest with AES-256-GCM under
  `CALLING_WEBHOOK_SECRET_KEY`, with the subscription id as associated data. They never
  appear in views, events, errors, logs or audit records. Rotation takes effect
  immediately.
* **Retries.** 2xx is delivered. Timeouts, connection errors, 408, 429 and 5xx retry with
  jittered exponential backoff: 30 s × 2^(n−2), capped at 6 h, at most 8 attempts.
  3xx, other 4xx and a blocked endpoint are permanent failures. FAILED is the dead
  letter: visible in the delivery history and replayable.
* **SSRF.** Only https URLs are accepted, with no credentials or fragment, on port 443
  or ≥ 1024. Loopback, private, link-local, CGNAT, benchmark, reserved, ULA, IPv4-mapped
  and NAT64 addresses are refused, as are `localhost`, `.local`, `.internal` and
  metadata names. They are refused at registration **and at connection time**: a
  resolver filter defeats DNS rebinding, and there are no redirects and no proxy.
  `CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` is honoured only in the `local` runtime.

### §69 Usage facts (not billing)

* `call_usage_records` stays the single consumption ledger, with a new fact for each
  public-network leg and each stored artifact:
  * **Leg facts:** connected time, routing class, destination **country** (never the
    number), billable seconds and **cost evidence**. KNOWN is provider-reported;
    ESTIMATED is an amount the provider did not confirm; UNKNOWN means no amount, never
    zero. Database CHECKs enforce these shapes.
  * **Artifact facts:** `VOICEMAIL_ARTIFACT` and `RECORDING_ARTIFACT`, with duration and
    stored size.
* Facts are written in the transaction that settles the leg or makes the artifact READY.
  The key is unique per Call (`leg-usage:{leg}`, `…-artifact:{id}`) and the fact columns
  are immutable. A callback's two legs are two facts of one Call. Each fact emits
  `CALL_USAGE_FINALIZED` → `usage.recorded`.
* `GET /v1/usage` (`usage.read`) lists final facts of the caller's application
  environment. It filters by `from` / `to` (recording time), `usageType` and
  `resourceClass` (`APP_MEDIA` / `PUBLIC_NETWORK` / `STORAGE`) and pages by keyset. There
  is no price, invoice, tax or balance: billing is out of scope.
* **Known limitation:** a provider cost that arrives **after** the leg's usage was
  finalized is not applied. Both the fact and the spend settlement are written once. A
  later correction belongs to reconciliation / billing, outside Part G. It is never
  double-counted.

### §70 Limits

* **Precedence:** tenant → application → credential (`platform_tenant_limits` is new;
  NULL means not configured). The most restrictive configured value wins; on a tie, the
  more specific level is named. With no configured level, the platform default applies.
  Each configured tenant or application level is **also** an aggregate ceiling across
  everything beneath it. Telecom spend additionally obeys the environment's spend
  policy, and is zero where live telecom is not allowed. Per-principal, service-account,
  context and destination velocity remain spend-policy guards.
* **Race safety:**
  * **Request rate:** atomic upserts of shared one-minute counters, per credential,
    application and tenant. The limiter fails closed (503).
  * **Concurrent Calls:** creation takes the tenant's advisory lock (only when a tenant
    ceiling is configured), then the application's, always in that order. The last slot
    goes to exactly one creator.
  * **Spend:** reservation row-locks the tenant cap, then the application cap, then the
    credential cap, then the environment policy. Headroom is never shared by two
    reservations.
* **Errors** stay normalized: `RATE_LIMITED`, `CONCURRENT_CALL_LIMIT_EXCEEDED`,
  `TELECOM_SPEND_LIMIT_EXCEEDED` (429), with bounded reason codes. Spend has new
  `TENANT_DAILY_SPEND_LIMIT` and `TENANT_MONTHLY_SPEND_LIMIT` codes.
* **Notices:** `limit.warning` fires when a change crosses 80 % of a ceiling;
  `limit.exceeded` fires on refusal. The data is `limit`, `scope` (level), `window`,
  `current`, `ceiling` and `threshold`. They are deduplicated per kind, level, subject,
  receiving environment and window (minute / UTC day / UTC month), so a refused burst is
  one notice. Warnings are emitted in the transaction that crossed the threshold.
  Refusal notices are emitted on their own connection, because the refused transaction
  rolls back. Notices are best-effort signals; the enforcement decision never depends on
  them.
* `GET /v1/limits` (`usage.read`) returns each limit with every configured level
  (ceiling / current / remaining), the binding level (least remaining) and the reset
  time.

## Consequences

* Customers get signed, retried, replayable webhooks and a usage and limits read model,
  and no provider identifier, telephone number, SIP URI, credential or secret leaves the
  platform through them.
* There is still exactly one outbox per service and one usage ledger. No new service was
  created for webhooks (Calling hosts the platform ingress).
* A configured tenant concurrency ceiling serializes Call creation per tenant (a cost
  accepted only when the operator configures one). The tenant request counter is one row
  per tenant per minute.
* Billing, invoices, pricing, a broker deployment and a developer portal remain out of
  scope. **Real lawful telecom interconnect certification remains pending (ADR 0016).**
