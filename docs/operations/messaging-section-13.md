# Messaging — Blueprint v1.0 Section 13 (relay parity, MANAGED realtime, inbox)

Engineering record for staging. Not a production release authorization; nothing here
changes production until an operator deploys and configures it. PRIVATE_E2EE stays blocked
(independent review #13): `MESSAGING_E2EE_DEVICES_ENABLED=true` is still refused in
production.

## What changed

| Finding | Change | Where |
|---|---|---|
| MSG-001 | The relay forwards Messaging's canonical replay cursor `after`; `afterEventSequence` is accepted as a compatibility alias and rewritten (both at once: 400) | `services/meeting-service/src/services/unified_messaging/relay.rs` |
| MSG-002 | `PATCH /api/v2/messaging/relay/{path}` mounted for every allowlisted PATCH (MANAGED edit, member role, conversation update / retention / title) | `routes/v2.rs`, `handlers/v2/unified_messaging.rs` |
| MSG-003 | Per-operation query allowlists equal the Messaging contract (`scope`, `reason`, `threadRootId`, `lifecycleState`, `contextClass`, `orderBy`); a unit test walks the bundled contract, an integration test walks it through the router | `relay.rs` tests, `tests/unified_messaging_api.rs` |
| MSG-004 | Session devices (ADR 0049 amendment A1): `POST /api/v2/messaging/session-tokens` → Messaging `POST /v1/messaging/session-devices`; realtime-only, MANAGED-only, bound to the account session | `services/messaging/src/realtime/session_devices.rs` |
| MSG-007 | `GET /v1/conversations` carries `title` / `avatarRef` (MANAGED) and, for an acting member, `inbox` (last activity, last visible message preview metadata, unread and mention counts); `orderBy=lastActivity` with its own keyset cursor; MANAGED `mentionedPrincipalIds` on send | `conversations/service.rs`, `messages/mod.rs` |
| MSG-018 | Contract documents that omitting `afterEventSequence` on subscribe resumes from the stored per-device ack cursor (server behaviour unchanged; the web client change is frontend work) | realtime contract |
| MSG-019 | MANAGED retries are answered before rate limits are charged; a retry with different text or attachment set is `IDEMPOTENCY_KEY_REUSED` | `messages/mod.rs` (`send_replay`) |
| MSG-015 | ADR 0053 superseded-scope note, native API map regenerated, realtime ingress documented in the handoff | `docs/decisions`, `docs/operations/messaging-native-api-map.md` |

## Configuration (Messaging)

| Variable | Default | Notes |
|---|---|---|
| `MESSAGING_SESSION_DEVICES_ENABLED` | `true` in local / test, else `false` | Permitted in production. Never enables E2EE devices. Switching it off ends session-device connections at the next re-validation; revocation stays available. |
| `MESSAGING_SESSION_DEVICE_TOKEN_TTL_SECONDS` | 300 (60–900) | Each renewal returns through Meeting, which re-verifies the account session. |

Session devices also need the existing client-token signing key
(`MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE`); without it no token of any kind is issued.

## Database

Messaging migration `m20261202_000030_session_devices_inbox` (main history; run
`messaging-migration up` before starting the new image, as for every Messaging
migration): `messaging_session_devices`, `messaging_client_sessions.device_kind` (the
E2EE-device foreign key is kept through a generated column), `messaging_conversations.title`
/ `avatar_ref` (CHECK: MANAGED only), `messaging_message_mentions`. Rolling the image back
requires `CF_MIGRATION_ALLOW_DESTRUCTIVE=down messaging-migration down -n 1` first (it deletes
session-device sessions only; the override is required by the SEC-015 guard).
Meeting has no migration in this section.

## Client flow (MANAGED realtime without an E2EE device)

1. `POST /api/v2/messaging/session-tokens` (Account bearer of a session-bound sign-in; body
   `{}` or `{ "sessionDeviceId", "ttlSeconds" }`) → `SessionDeviceToken`.
2. `POST /v1/realtime/tickets` with `Authorization: CF-Client-Token <token>`, then the
   WebSocket; subscribe to MANAGED conversations (PRIVATE_E2EE answers
   `E2EE_DEVICE_REQUIRED`).
3. Before expiry: step 1 with `sessionDeviceId`, a new ticket, and `auth.refresh` on the open
   connection.
4. Signing out of the session (or everywhere) revokes the session devices and closes the
   connection (4401).

## Not in this section

* Organization conversations: integration point documented in ADR 0053 (needs the
  Section 7 organization scopes).
* Hub `capabilities.available` still reflects E2EE device readiness (Section 14 Hub work);
  MANAGED realtime readiness is the session-token endpoint answering 201.
* Hub inbox membership filtering and `block-events` (MSG-016) are Section 14.
* Production enablement: the production release scripts
  (`infra/calling/production/messaging_runtime.py`) do not set
  `MESSAGING_SESSION_DEVICES_ENABLED`; adding it is a release-time operator change.
* The PRIVATE_E2EE retry path's rate limit ordering (E2EE is blocked in production) is
  left as it was. Receipt-event coalescing (MSG-018 server half) was closed later: see
  "Audit closure MSG-018 / MSG-009" below.

## Audit closure MSG-018 / MSG-009 (after Section 13)

| Finding | Change | Where |
|---|---|---|
| MSG-018 (server half) | Read / delivery receipts are coalesced per conversation, member and kind (`messaging_receipt_emissions`): the first advance of a window is published at once, later advances inside the window only raise a durable pending cursor, and one `read.cursor.updated` / `delivery.cursor.updated` with the HIGHEST cursor is published when the window ends (`MESSAGING_RECEIPT_COALESCE_WINDOW_MS`, default 1500, 0–10000; 0 = every advance). Receipt traffic no longer takes the conversation's event-sequence lock per read / ack. Published cursors only grow (database trigger); repeated cursors publish nothing; privacy is re-checked when a deferred receipt is published. Reconnect: realtime client frame `receipts.query` → `receipts.snapshot` (current cursors, same privacy filter); the REST replay keeps the last published receipt of every window. | Messaging `messages/receipts.rs`, `messages/mod.rs` (`set_read_cursor`, `advance_delivery`), `realtime/connection.rs`, migration `m20261203_000031_receipt_coalescing_timeline`; test `tests/realtime_receipts_timeline.rs` |
| MSG-009 (Messaging half) | `POST /internal/v1/communications/events` accepts `communications.timeline.changed {conversationId, timelineVersion, reason}` from the Hub and records the durable conversation event `messaging.timeline.changed` (client type `timeline.changed`, data `timelineVersion`, `reason`) once per higher timeline version (`messaging_conversation_timeline_versions`): members get it live on their subscriptions and from the replay (late members inside their history window); a redelivered, repeated or older nudge publishes nothing; another scope's conversation is acknowledged without effect. | Messaging `communications.rs` (`timeline_changed`); Hub half in [communications-hub-section-14.md](communications-hub-section-14.md) |

Contracts: `cfmeet.messaging.realtime-protocol` 1.3.0, `cfmeet.messaging.event` 1.12.0,
`cfmeet.messaging.communications-internal` 1.3.0 (+bundled). Migration: additive (two
tables); its down drops only them. Rollback: the previous image ignores both tables;
deferred receipts not yet published when rolling back stay unpublished (the canonical
cursors are unaffected and `receipts.query` / REST reads still return them).
