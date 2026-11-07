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

## Amendment A1 — MANAGED realtime through a session device (Blueprint v1.0 Section 13, decision D-5)

Status: Accepted for `staging` (Blueprint v1.0 Section 13; owner decision D-5 in
`docs/implementation/cfmeet-master-execution-register.md`). Engineering direction only;
production enablement is an operator configuration decision. PRIVATE_E2EE stays blocked
(independent review #13): nothing here enables, weakens or bypasses the production refusal
of `MESSAGING_E2EE_DEVICES_ENABLED=true`.

**Context.** Decision 1 binds every client token to a registered MLS device. Production
refuses device registration (PRIVATE_E2EE is not certified), so CF Meet's own users, whose
production conversations are all MANAGED, could never obtain a client token and fell back
to 15-second polling (gap register MSG-004).

**Decision.**

1. *A separate identity, not a weaker device.* A **session device**
   (`messaging_session_devices`) is a distinct, bounded identity. It is not a
   `messaging_devices` row, so by construction it owns no KeyPackage, Welcome, push
   registration, history transfer or MLS leaf, is never returned by the device directory,
   never counts toward "has an E2EE device" (PRIVATE_E2EE creation still answers
   `E2EE_NOT_AVAILABLE` for a principal holding only session devices) and can never be
   promoted into one. Client sessions record `device_kind` (`E2EE_DEVICE` |
   `SESSION_DEVICE`); the original foreign key keeps protecting E2EE-device sessions.
2. *Realtime only, MANAGED only.* A session-device client token is accepted by
   `POST /v1/realtime/tickets` and the WebSocket and by nothing else: every device-direct
   REST operation refuses it (`CLIENT_TOKENS_NOT_ENABLED` / `SESSION_DEVICE_REALTIME_ONLY`).
   The gateway refuses a PRIVATE_E2EE subscription (`E2EE_DEVICE_REQUIRED`, connection kept)
   and drops device events for it, so it never receives ciphertext, Welcomes, typing or
   presence of an encrypted conversation. MANAGED never masquerades as E2EE: the session
   device's only capability is `REALTIME_MANAGED`.
3. *Bound to the account session.* Only a FIRST_PARTY delegated context may create one
   (`POST /v1/messaging/session-devices`; customers keep registering MLS devices). The
   relay (Meeting, `POST /api/v2/messaging/session-tokens`) verifies the CF Meet login with
   its account-security authority (`account_security::authorize_user_token`: token type,
   account epoch, open account session) and requires a session-bound token. It sends an
   opaque, domain-separated SHA-256 reference of the account session
   (`cfmeet-account-session:v1|environment|session`) and `notAfter` = the presented access
   token's own expiry; the client can name only the session device to renew and a shorter
   lifetime. Tokens live at most `MESSAGING_SESSION_DEVICE_TOKEN_TTL_SECONDS` (default 300,
   max 900) and never past `notAfter`; a session device lives at most 24 hours. Every
   renewal returns through the relay, which re-verifies the account session and epoch.
   Ending one account session revokes its session devices at once
   (`POST /v1/messaging/session-devices/revoke` by the same reference); "log out
   everywhere" (`client-sessions/revoke-all`, already queued durably by Meeting on logout)
   revokes all of them; either closes open connections through the outbox (4401). An
   epoch advance that is not pushed (password change, account disablement) is bounded by
   the token lifetime: the next renewal is refused.
4. *Separately switched.* `MESSAGING_SESSION_DEVICES_ENABLED` (default on only in local /
   test) is permitted in production and is independent of
   `MESSAGING_E2EE_DEVICES_ENABLED`, which production still refuses (config test
   `production_enables_session_devices_while_e2ee_devices_stay_refused`). Switching session
   devices off ends existing session-device connections at their next re-validation;
   revocation stays available while off.

**Consequences.** Production MANAGED chat can have live delivery, typing, presence and
receipts without any E2EE enrollment. The trust placed in the session device equals the
trust already placed in the account session for MANAGED (server-readable) content; no E2EE
property is claimed. Evidence: `services/messaging/tests/realtime.rs`
(`session_devices_*`), `services/meeting-service/tests/unified_messaging_api.rs`
(`session_tokens_are_bound_to_the_verified_account_session`), migration
`m20261202_000030_session_devices_inbox`.
