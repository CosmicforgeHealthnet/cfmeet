# ADR 0049 — Messaging realtime gateway: client tokens, tickets, durable delivery, ephemeral signals

Status: Accepted (Part V Part E, §58–§68). Builds on ADR 0045–0048. Production E2EE stays
disabled; AD-01…AD-10 remain pending.

## Decisions

1. **Device-direct authentication without application secrets on devices.** The
   application backend (Platform Access credential + acting principal) asks Messaging for
   a short-lived *Messaging Client Access Token* (compact JWS, EdDSA, `typ
   cfmeet-mcat+jwt`, bound to tenant / application / environment / principal / device /
   session, scopes ⊆ `messages.read` / `messages.send`). The device exchanges it for a
   single-use, seconds-long WebSocket ticket (SHA-256 stored, consumed atomically) sent as
   a `Sec-WebSocket-Protocol` entry — never in a URL. Client sessions are revocable
   (terminal `REVOKED`); device revocation revokes them in the same transaction.
2. **Connections are re-authorized.** At the handshake (TLS outside local / test, browser
   Origin allow-list, ticket, session, device, principal) and periodically; instance /
   tenant / principal / device connection limits; frame size and rate limits; heartbeats
   and idle timeout; draining on shutdown with jittered reconnect hints.
3. **Delivery only from committed state.** The transactional outbox (per-conversation
   event cursor) is the replay log. A leased publisher hands committed events to the
   broker as *pointers*; each connection fills gaps from PostgreSQL in cursor order,
   drops duplicates, delivers tombstones, honours history windows. WebSocket delivery is
   never proof of commit; the client's ack is bounded by what was delivered. A replay
   window beyond the server's bound answers `sync.required` (REST replay).
4. **Broker is not storage.** In-process or self-hosted Redis pub/sub carries pointers and
   ephemeral signals only; a broker outage is recovered by database catch-up. Redis is
   never the durable store of anything.
5. **Backpressure.** Bounded per-connection queues; a slow consumer is closed with 1013
   and its resume cursors (never silently dropped events).
6. **Ephemeral signals stay ephemeral.** Typing indicators are coalesced, throttled,
   TTL-bound and never persisted as messages. Presence is privacy-first: HIDDEN is
   reported OFFLINE everywhere; presence is visible only to co-members of a subscribed
   conversation (no lookup by principal id); last seen is opt-in and policy-gated.
7. **Push boundary.** Per-device registrations and idempotent, leased push jobs whose
   payload is metadata only (a database CHECK refuses content keys). No APNs / FCM
   adapter exists: jobs end `NOT_CONFIGURED`, never reported delivered.

## Consequences

Two gateway instances on one database share fan-out, presence and revocation through
Redis (tested with real Redis). Realtime is optional: without a token signing key the
gateway is disabled and REST works unchanged.
