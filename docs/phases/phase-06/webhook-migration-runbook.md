# Webhook ownership migration — Calling → Developer Platform (Section 53 §9.1, §19.3)

> **Status: tooling only.** This runbook describes the controlled migration of customer
> webhooks from Calling to the Developer Platform and the tooling that implements it.
> **No production migration, cutover, rollback or retirement has been performed.** Every
> step below is an operator decision taken in a change window.

## 1. What moves, and the guarantees

| Calling (today) | Developer Platform (after) |
|---|---|
| `webhook_subscriptions` (secret sealed under `CALLING_WEBHOOK_SECRET_KEY*`) | `devplat_webhook_subscriptions` (same id, same `version`, `origin = MIGRATED_FROM_CALLING`, secret sealed under `DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY*`) |
| `webhook_events` (`whe_…`) | `devplat_webhook_events` (same `whe_` id, `origin = MIGRATED_FROM_CALLING`) |
| `webhook_deliveries` (incl. replays, dead letters) | `devplat_webhook_deliveries` (same id, `replay_of`, status; `origin = MIGRATED_FROM_CALLING`; open ones held) |
| `webhook_delivery_attempts` | `devplat_webhook_attempts` (append-only history) |

* **Safe / idempotent / resumable.** Every write is an upsert keyed by Calling's ids; each
  import page is committed together with the run checkpoint
  (`devplat_webhook_migration_runs`), so a crash loses at most the page in flight and the
  next `import` resumes the open run. A completed run imported again changes nothing.
* **Secret custody.** Secrets leave Calling only re-sealed under a dedicated *migration
  transfer key* (both services hold it under their own variable names). The associated
  data binds each transfer seal to the subscription id **and** the direction
  (`webhook-migration|calling-to-developer-platform|<id>` /
  `webhook-migration|developer-platform-to-calling|<id>`). Plaintext exists only in memory
  (zeroized on drop) — never in logs, rows, dumps, files or HTTP bodies. Crypto is only
  `cf_webhooks::secret_box` (AES-256-GCM); continuity is proven with a keyed fingerprint
  (the `CF-Webhook-Signature` of a fixed probe).
* **Never a substitute secret.** A secret Calling cannot open (key id not configured, seal
  does not open), or a transfer seal the Developer Platform cannot open (wrong transfer
  key, tampering, fingerprint mismatch), makes the subscription `ROTATION_REQUIRED`: it is
  imported `DISABLED` (`disabled_reason = SECRET_ROTATION_REQUIRED`) **without any secret**;
  it can be enabled only after the customer rotates its secret. The operator must
  acknowledge every such subscription before the cutover.
* **No duplicate deliveries, no lost delivery.** Both services keep recording throughout.
  The Developer Platform never sends a delivery of a migrated subscription before the
  *takeover* (cutover gate) and never sends a held mirror of a Calling delivery; the
  takeover verifies that Calling stopped sending and de-duplicates every logical delivery
  `(subscription, producer, source event id)` to exactly one sender (Calling's open
  delivery keeps its delivery id and event id — customers see continuity).

## 2. Configuration

### Calling (`services/calling`)

| Variable | Default | Meaning |
|---|---|---|
| `CALLING_WEBHOOK_MUTATIONS_FROZEN` | `false` | `true`: customer create / update / delete / rotate-secret / replay answer **503 `SERVICE_UNAVAILABLE`** with `CF-Webhook-Migration: frozen` and `Retry-After: 300`; reads and deliveries continue |
| `CALLING_WEBHOOK_DELIVERY_ENABLED` | `true` | `false`: the delivery worker claims and sends nothing; deliveries stay recorded (a rollback resumes them) |
| `CALLING_WEBHOOK_AUTHORITY` | `active` | `retired`: every customer webhook route answers **503 `SERVICE_UNAVAILABLE`** with `CF-Webhook-Migration: moved`; delivery off (`CALLING_WEBHOOK_DELIVERY_ENABLED=true` is then refused); records retained, export still served |
| `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY` / `_FILE` | unset | base64 32-byte transfer key (not weak); unset = subscription export refused (503 `CAPABILITY_UNAVAILABLE`) |
| `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID` | required with the key | its key id (`[A-Za-z0-9_-]`, 1–32) |

Internal routes (contract `cfmeet.calling.http-api` 1.12.0):

* `webhook-migration.export` (grantable to `cfmeet.developer-platform` only):
  `GET /internal/v1/webhook-migration/{inventory,digest,subscriptions,events,deliveries,attempts}`
  (keyset pages, `limit` ≤ 500 and ≤ 768 KiB, opaque `cursor`; deliveries accept
  `status=open`).
* `webhook-migration.import` (grantable to `cfmeet.developer-platform` only):
  `POST /internal/v1/webhook-migration/rollback/subscriptions`,
  `POST /internal/v1/webhook-migration/rollback/deliveries`.

### Developer Platform (`services/developer-platform`)

| Variable | Default | Meaning |
|---|---|---|
| `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` | `false` | central webhooks |
| `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | `shadow` | `shadow`: record SHADOW, send nothing; `active`: send |
| `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN` | `false` | `true`: customer and console webhook mutations answer 503 `SERVICE_UNAVAILABLE` (`CF-Webhook-Migration: frozen`) |
| `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY` / `_FILE` / `_ID` | unset | the same transfer key and id as Calling's; unset = import / rollback export refused |
| `CF_CALLING_BASE_URL` | — | Calling's internal base URL (the export) |

Trust bundles: Calling's bundle grants `cfmeet.developer-platform` both
`webhook-migration.export` and `webhook-migration.import`; the Developer Platform's bundle
grants the operator console `developer-platform.administer`.

Operator interface (`developer-platform.administer` + `X-CF-Operator`; contract
`cfmeet.developer-platform.internal-api` 1.5.0), prefix
`/internal/v1/operator/webhook-migration/`:

| Route | Writes | Purpose |
|---|---|---|
| `GET status` | no | phase (`null` / `PREPARING` / `CUT_OVER` / `ROLLED_BACK`), latest run, counts |
| `GET inventory[?pageSize=]` | **no** (one transaction, rolled back) | dry run: would insert / update / unchanged, conflicts, rotation-required |
| `POST import {"pageSize":1-500}` | yes, audited | import or resume |
| `GET reconcile` | no | digests + record-level drift |
| `GET shadow-report[?since=RFC3339]` | no | dual-ingestion parity |
| `GET cutover-check` | no | preconditions (`ready`) |
| `POST rotation-acknowledgements {"webhookIds":[…]}` | yes, audited | acknowledge required rotations |
| `POST takeover` | yes, audited | the delivery switch |
| `POST rollback-export` | yes, audited | push DP subscription changes back to Calling |
| `POST rollback {"acceptOrphans":false}` | yes, audited | reverse the delivery switch |

Audit rows: `devplat_webhook_migration_audit` (append-only; codes and counts only).

### Digest algorithm `sha256-canonical-lines-v1`

One canonical line per row — a compact JSON array — in key order, each followed by `\n`,
hashed with SHA-256; timestamps RFC 3339 UTC with microseconds; UUIDs lower-case;
subscription event types sorted; event bodies as `sha256(body::text)` of the stored JSONB:

* subscription: `[id, tenantId, applicationId, environmentId, url, description, eventTypes, status, secretVersion, version, createdAt, updatedAt, deletedAt]` (key: id). A Developer Platform row awaiting rotation is compared with Calling's status.
* event: `[eventId, producer, sourceEventId, eventType, tenantId, applicationId, environmentId, occurredAt, createdAt, bodySha256]` (key: eventId, byte order)
* delivery: `[id, webhookId, eventId, replayOf, status, attemptCount, nextAttemptAt, lastHttpStatus, failureCategory, createdAt, completedAt]` (key: id)
* attempt: `[deliveryId, attemptNumber, outcome, httpStatus, failureCategory, startedAt, finishedAt]` (key: deliveryId, attemptNumber)

## 3. Preconditions

1. Developer Platform migration `m20261105_000005_webhook_migration` applied.
2. A transfer key generated in the secret manager (32 random bytes, base64) and mounted
   into Calling **and** the Developer Platform (API and worker) as
   `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_FILE` / `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY_FILE`
   with the same `…_ID`.
3. Trust bundles updated (see §2); `CF_CALLING_BASE_URL` set on the Developer Platform.
4. Developer Platform: `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=true`,
   `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE=shadow`, key ring configured, and
   `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN=true` for the whole migration window
   (console changes to migrated subscriptions would diverge from Calling).
5. Calling key ring opens every stored secret (`platform_admin webhooks key-status`); the
   inventory's `subscriptions.unopenableSecrets` should be 0 — otherwise those customers
   will have to rotate.

## 4. Procedure (each step: change, then verify)

| # | Change | Verify |
|---|---|---|
| 1 | — | `GET inventory` — expected counts, `conflicts: []`, review `rotationRequired` |
| 2 | — | `POST import` → `state: COMPLETED`; `GET reconcile` → `clean: true` |
| 3 | Producers dual-publish: `CALLING_PUBLIC_EVENTS_DESTINATION=both`, `MESSAGING_/PRIVACY_RELAY_/SMS_GATEWAY_/NUMBERING_PUBLIC_EVENTS_DESTINATION=both` (where the producer offers it) (shadow window starts; note the time) | `GET shadow-report?since=<window start>` → `parity: true`, `developerPlatformDeliveriesSent: 0` |
| 4 | **Freeze** Calling: `CALLING_WEBHOOK_MUTATIONS_FROZEN=true` (DP already frozen) | customer `POST /v1/webhooks` → 503 `CF-Webhook-Migration: frozen`; reads 200 |
| 5 | — | `POST import` (incremental), `GET reconcile` clean, acknowledge rotations, `GET cutover-check` → `ready: true` |
| 6 | **Delivery switch (one change window):** Calling `CALLING_WEBHOOK_DELIVERY_ENABLED=false` and DP `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE=active` | Calling inventory `flags.deliveryEnabled=false`, `deliveries.inFlight=0` |
| 7 | — | `POST import` (final; its `callingAtStart.deliveryEnabled` must be `false`) then `POST takeover` → `phase: CUT_OVER` |
| 8 | Gateway: `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED=true` | `GET /v1/webhooks/{id}` through the gateway: same id / version, served by the DP |
| 9 | Producers: `…_PUBLIC_EVENTS_DESTINATION=developer-platform` | DP `status`; Calling receives no new events |
| 10 | **Unfreeze** DP: `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN=false` | a customer update succeeds |

**Why step 6 has no window where both send or neither records.** Recording never stops:
both services ingest every event while producers dual-publish. Sending is gated: the
Developer Platform never sends a delivery of a migrated subscription before the takeover,
whatever its delivery mode, so the order of the two restarts in step 6 does not matter
(Calling stops → its unsent deliveries stay recorded; DP active → its deliveries stay
recorded and gated). The takeover (step 7) is refused unless Calling reports delivery
disabled with no live claim **and** the last import started after that; it then releases
Calling's open deliveries (held mirrors, same ids), cancels native duplicates of them
(`DUPLICATE_OF_MIGRATED`), cancels native deliveries Calling already settled
(`DELIVERED_BY_CALLING`), and opens the gate. Each logical delivery therefore has exactly
one sender.

## 5. Rollback (after step 7)

| # | Change | Verify |
|---|---|---|
| R1 | Freeze DP: `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN=true` | |
| R2 | — | `POST rollback-export` → `conflicts: []` (subscriptions changed / created at the DP are applied at Calling; rotated secrets travel transfer-sealed for the rollback direction; event types Calling does not serve are reported `EVENT_TYPE_UNSUPPORTED`) |
| R3 | Gateway `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED=false`; producers back to `both` | customer reads served by Calling |
| R4 | — | `POST rollback` → `phase: ROLLED_BACK`, `completed: true` (the DP stops sending; open Calling deliveries already delivered by the DP are settled at Calling `CANCELLED / DELIVERED_BY_DEVELOPER_PLATFORM`; the DP's open deliveries Calling also has are handed back). Deliveries of events only the DP received are reported as orphans and stop the rollback unless `acceptOrphans` |
| R5 | Calling `CALLING_WEBHOOK_DELIVERY_ENABLED=true`, `CALLING_WEBHOOK_MUTATIONS_FROZEN=false`; DP `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE=shadow` | Calling delivers; DP records SHADOW |

A later re-cutover repeats §4 from step 4 (the import treats subscriptions pushed back as
known; `takeover` moves `ROLLED_BACK → CUT_OVER`).

## 6. Retire (after an agreed parity period)

1. `CALLING_WEBHOOK_AUTHORITY=retired` (implies delivery off): Calling's customer webhook
   routes answer 503 `CF-Webhook-Migration: moved`; its webhook records are retained for
   their retention period and the export keeps working for audits.
2. Remove `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY*` and
   `DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY*` from both deployments and destroy
   the transfer key in the secret manager; remove the migration permissions from the trust
   bundles.

## 7. Evidence (automated)

* `services/calling/tests/webhook_migration.rs` — empty export, deterministic paged
  export with every state, transfer-sealed secrets opening only for their subscription and
  direction, unopenable secrets exported without material, freeze, delivery disabled,
  retired authority, rollback import and settlement, no plaintext in responses / rows /
  logs, Retry-After.
* `services/developer-platform/tests/webhook_migration.rs` — dry run writes nothing,
  import twice = no duplicates, crash after N pages resumes to an identical result, secret
  continuity (the original secret verifies DP deliveries), rotation-required without
  substitute, transfer-key mismatch, no plaintext, reconcile drift, shadow parity, takeover
  de-duplication, rollback round trip, freeze, Retry-After.
* `certification/tests/webhook_migration.rs` — the whole procedure (seed at Calling,
  inventory, import, shadow window, freeze, cutover check, switch, takeover, gateway,
  producers, one DP delivery verified with the original secret, rollback, Calling delivers
  with the secret rotated at the DP) on the real processes.

Run: `cd services/calling && cargo test --test webhook_migration`,
`cd services/developer-platform && cargo test --test webhook_migration`,
`make certification-bins && cd certification && CFMEET_BIN_DIR=$CARGO_TARGET_DIR/debug cargo test --test webhook_migration -- --test-threads=1`.
