# CF Meet SMS Gateway

The authoritative SMS transport service of CF Meet (Part IV Workstreams M–O,
[ADR 0042](../../docs/decisions/0042-sms-gateway.md)). It owns, in its own PostgreSQL
database (`sms_*` tables only):

* **message transports** — one row per inbound or outbound message with its state
  machine (REQUESTED, AUTHORIZED, SUBMITTING, SUBMITTED, DELIVERED, RECEIVED, FAILED,
  UNKNOWN, EXPIRED), enforced in Rust and by a database trigger (transitions, immutable
  facts, content never rewritten, no deletes, optimistic version);
* **submission attempts** under a lease — at most ONE attempt may be live or possibly
  sent (unique index), so after an UNKNOWN outcome nothing can resubmit; only provider
  evidence (receipt or status query) resolves it;
* **provider message references** (internal only), **provider callback records**
  (de-duplicated by provider event id, with their disposition) and **quarantined
  inbound** messages that could not be attributed to a current owner;
* **transport threads** (one Number Resource × one remote party; a transport index, not
  the Part V conversation model), **usage records** (estimated / confirmed cost),
  **opt-out blocks**, **rate windows** and the **outbox** (leased publication, bounded
  retry, dead-letter).

Content, remote numbers and released senders are stored ONLY sealed (AEAD, bound to the
record) — the schema refuses plaintext; remote numbers are additionally kept as keyed
fingerprints and shown only masked (`+***1234`). Logs carry ids and codes only.

It does NOT own Number Resources, assignments, KYC or the provider-number lifecycle
(Numbering), identities (Platform Access) or customer webhooks (Calling), and never
reads another service's database.

## Authorities it calls

| Service | Permission | Use |
|---|---|---|
| Platform Access | `platform-credential.authenticate`, `platform-principal.resolve` | every customer credential (fail closed: 503); the principal a message is sent for |
| Numbering | `number-sms-sender.authorize`, `number-grant.verify` | single-use SMS_SENDER grant per outbound message, consumed before AUTHORIZED |
| Numbering | `number-sms-route.resolve` | the destination of a received message (`sms-destination-resolutions`, SMS only) |
| Calling | `platform-event.publish` | message events → `sms.*` customer webhooks |

The gateway serves no internal operation; `/internal/v1/**` is authenticated anyway
(unknown paths included) and its trust bundle trusts no issuer.

## HTTP surface

Served at `GET /openapi.json` — exactly the checked-in contract
`cfmeet.sms-gateway.http-api` (a test proves every documented route is served).

| Route | Authentication | Purpose |
|---|---|---|
| `GET /health`, `GET /ready` | none | liveness; readiness (database, migrations, provider, outbox / dead-letter / UNKNOWN counts) |
| `GET /openapi.json` | none | the served contract |
| `POST /provider-callbacks/v1/sms/{providerId}/inbound` | provider signature (raw body, 300 s window) | an inbound message: authenticated BEFORE parsing, de-duplicated by the provider event id, resolved by Numbering (`sms-destination-resolutions`) and its grant re-verified against current state right before storage; ROUTED → stored for the environment Numbering named (`sms.received`), a routing change in between → quarantined `ROUTING_CHANGED`; otherwise QUARANTINED; 503 while Numbering is unreachable (nothing stored, the provider retries) (N) |
| `GET /v1/sms/messages[/{messageId}]` | customer, `sms.read` | the environment's messages (filters `numberResourceId`, `direction`; cursor pages) — content to its owner only, remote party masked (N) |
| `GET /v1/sms/threads`, `GET /v1/sms/threads/{threadId}/messages` | customer, `sms.read` | transport threads and their messages (N) |
| `POST /v1/sms/messages` | customer, `sms.send`, `Idempotency-Key` | send ONE message from a Number Resource of the environment (`to` or a `threadId` reply); 202 AUTHORIZED, the same key replays (200) (O) |
| `POST /v1/sms/opt-outs` | customer, `sms.send` | block a recipient for one number or the environment (O) |
| `POST /provider-callbacks/v1/sms/{providerId}/delivery-receipts` | provider signature | delivery receipts: de-duplicated, order-safe, cost confirmed once (O) |

Another environment's message or thread is NOT_FOUND.

## Inbound messages (Workstream N)

* STOP / START keywords (the whole message) block / unblock the sender for that number.
* An unattributable message (unknown number, not ACTIVE, released, SMS not routed) is
  kept sealed in `sms_quarantined_inbound`, attributed to no customer, and purged after
  `SMS_GATEWAY_QUARANTINE_RETENTION_DAYS`; numbers or content that are not valid are
  recorded REFUSED.
* Content of final messages is purged after `SMS_GATEWAY_CONTENT_RETENTION_DAYS` (the
  message stays, with `contentPurged: true`).
* `sms-gateway.message.received` is published to Calling's platform event ingestion and
  delivered as the `sms.received` customer webhook (references, status and codes only).

## Outbound messages (Workstream O)

1. **Authorization** (request path): content classified GSM-7 / UCS-2 and segmented (≤ 10);
   recipient by `to` or the thread's party; principal ACTIVE (Platform Access); send-time
   controls in the gateway itself (premium ranges, allowed prefixes, opt-outs, provider
   anomaly and cost-spike circuits, environment / tenant daily spend — conservative:
   estimates count until confirmed — identical-content fan-out and repeats, rate windows
   per tenant / environment / number / principal / recipient). A control refusal is
   recorded FAILED with its reason (`sms.blocked`, 422 `SMS_BLOCKED`); a rate window is a
   429 and not recorded. Numbering then issues a single-use SMS_SENDER grant bound to
   `sms:<message id>`, consumed at once (ownership, ACTIVE assignment, SMS_OUTBOUND,
   current revision); the released sender is stored sealed. AUTHORIZED, `sms.requested`.
2. **Dispatch** (workers): an attempt is CLAIMED under a lease; Numbering re-confirms the
   sender is still the scope's ACTIVE assignment (else FAILED `SENDER_REVOKED`, nothing
   sent); the provider is called with the message's stable key under a bounded timeout.
   ACCEPTED → SUBMITTED; REJECTED → FAILED; NOT_SENT → retried with backoff; a timeout
   or an ambiguous answer → UNKNOWN (`sms.unknown`). An expired lease is UNKNOWN too.
3. **Unknown outcomes** are never resubmitted (one live attempt per message, enforced by
   the database): provider read-back resolves them (found → its state; confirmed never
   received after a grace → FAILED `NOT_TRANSMITTED`; no answer → asked again later).
4. **Receipts** apply by the state machine only (no regression: a receipt after a final
   state is STALE), confirm the provider cost once, and announce `sms.delivered` /
   `sms.failed`. An unknown reference is UNMATCHED evidence.

## Provider adapters

`src/providers/` holds the provider-neutral adapter trait (submit → accepted / rejected /
definitely not sent / unknown; status query; callback authentication BEFORE parsing;
parsing) and the reusable contract suite (`providers::contract::run`). The only adapter in
this build is `deterministic` (scripted fault modes: accept, reject, not sent, timeout
before / after creation, malformed, stalls) for local development and tests; it is
refused in staging and production. No live carrier or aggregator credential is needed.

## Running locally (no Docker)

```bash
cp services/sms-gateway/.env.example services/sms-gateway/.env   # fill in the key files
make sms-gateway-migrate
make sms-gateway
```

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make sms-gateway-test
```

Each test gets its own database cloned from a migrated template. Suites:
`foundation` (schema guards, protection, outbox, auth boundaries, served OpenAPI,
provider contract), `startup` (the real binary: fail-closed configuration, the
deterministic provider refused in protected runtimes, pending migrations),
`inbound` (callbacks, quarantine, ownership changes, keywords, reads, retention),
`outbound` (authorization, idempotency, unknown outcomes, receipts, segments, controls),
`architecture` (no other service's source / tables, no Numbering authority replicated,
no numbers or content in logs).
