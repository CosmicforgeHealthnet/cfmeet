# ADR 0007: Calling media provider boundary (LiveKit as an adapter)

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part C §22–§24, §36–§54, §80–§85, §89–§92; implemented in
  `services/calling/src/media/` and `services/calling/src/calls/app/`

## Context

Application-to-application voice, video and group calls need a media server. CF Meet's
Meeting Service already uses LiveKit for meetings, but Calling must stay an independent
service (ADR 0005): no Meeting Service import, table, room namespace or permission type.
Media providers change, are configured per deployment and can fail. Clients are
untrusted: they must never choose a room, an identity or a permission, and a removed
device must not keep media access through a credential it still holds.

## Decision

### 1. Calling owns a provider-neutral media boundary

`src/media/provider.rs` defines `CallMediaProvider` (ensure / terminate a session,
issue join credentials, remove a participant, update permissions, list participants,
reconcile, verify callbacks) with neutral types (`MediaSessionRef`,
`MediaParticipantRef`, `ProviderParticipant`, `MediaPermissions`, `MediaProviderError`
{Timeout, Unavailable, Rejected, NotFound, Unsupported}) and explicit
`MediaProviderCapabilities`. Application calls require session control, credential
issuance, participant removal, server-side permission updates and participant listing;
otherwise creation answers `CAPABILITY_UNAVAILABLE`. Hold / resume / transfer are
offered only when the provider can enforce them.

LiveKit is **an adapter**, not the architecture: `src/media/livekit.rs` (Twirp
RoomService over HTTPS, HS256 tokens, webhook verification) and
`src/media/livekit_gateway.rs` (its connection gateway relay, §5) are the only files that
name the provider (architecture guard with mutation tests). No LiveKit SDK crate is
used; no LiveKit type crosses into `src/calls/`. Configuration is Calling's own:
`CALLING_MEDIA_PROVIDER` (`none` default | `livekit`), `CALLING_LIVEKIT_URL` (LiveKit
signaling, gateway upstream only), `CALLING_LIVEKIT_API_URL`, `CALLING_LIVEKIT_API_KEY`,
`CALLING_LIVEKIT_API_SECRET` (≥ 32 characters), `CALLING_MEDIA_GATEWAY_URL` (the public
gateway URL handed to clients), `CALLING_MEDIA_EDGE_SIGNING_KEY` (≥ 32 characters,
distinct from the LiveKit secret), `CALLING_LIVEKIT_API_TIMEOUT_MS`. TLS is required except on loopback;
invalid or partial configuration stops the service at startup (no fallback secret). The
secret lives in a redacting wrapper; config types have no derived `Debug`.

### 2. One deterministic media resource per Call, private binding

The resource name is `cfc-` + the Call id without hyphens — deterministic (retries and
other instances ensure the same room), never the Call id itself, never client-chosen,
disjoint from Meeting rooms. The binding (`call_media_sessions`: adapter, opaque
provider resource reference, status ENSURING / ACTIVE / TERMINATING / TERMINATED,
failure count, next reconcile time) is Calling-private: no API, event or signal exposes
the room name, provider ids or keys. LiveKit runs with `auto_create: false`, so only
Calling creates rooms and a join can never resurrect a terminated one.

### 3. Credentials are derived from server state, join-only, and sealed by Calling

`POST /v1/calls/{id}/sessions/{sid}/media-credentials` (API credential) and
`POST /v1/calling/sessions/{sid}/media-credentials` (endpoint's signaling credential)
issue a credential only for a session whose `media_access` is AUTHORIZED (the
initiator's session, or the single winning device of a participant) while the Call is in
a media state. The body must be empty. The response carries the gateway URL, the token,
its expiry and the capabilities the connection will receive — nothing else. TTL:
`CALLING_MEDIA_CREDENTIAL_TTL_SECONDS` (default 300, 30–900).

The token grants **room join only** (`canPublish`, `canSubscribe`, `canPublishData`
false, no publish sources, no room admin / create / list / record), binds the session's
server-generated media identity (`cfmid_…`) and the session's `media_generation`
(attribute `cfm.gen`), and is **sealed with Calling's media-edge key**
(`CALLING_MEDIA_EDGE_SIGNING_KEY`, issuer `cfmeet-media-edge`) — a key LiveKit does not
know. The client never receives LiveKit's signaling URL or any token LiveKit accepts.

### 4. Join-then-admit, enforced by the provider

Media rights are applied **server-side after the join is verified** (gateway
notification of the join, signed `participant_joined` callback, or reconciliation): the
admission decision (`media/events.rs`) gives a connection permissions only if its
identity is a known, AUTHORIZED, non-ended, non-removed session of this Call whose
participant is still in the Call, the generation matches, and the Call is in a media
state; permissions are `MediaPermissions::effective(authorized, kind, state)` (VOICE:
audio + subscribe; VIDEO: + camera; held: no publishing, subscription kept; never data).
Anything else is stripped and removed (evicted). Unknown identities are quarantined and
evicted, never attached to anyone.

### 5. Revocation and stale re-entry: the media connection gateway

**Why LiveKit's own mechanisms are not enough.** Self-hosted LiveKit cannot revoke an
access token. Removing a participant or updating its permissions does not invalidate
anything the client holds; worse, after `UpdateParticipant` LiveKit sends the client a
*refreshed* token reflecting the new grants (certified on a real server: `canPublish` /
`canSubscribe` true, signed with the API key, valid 600 s, `cfm.gen` preserved). A client
that saved it could rejoin after removal and publish immediately; webhook eviction,
15-second reconciliation and short TTLs only shorten that window. LiveKit Cloud's token
revocation is not available to a self-hosted deployment and is not relied on.

**Decision.** Every media signaling connection — fresh or reconnecting — goes through
Calling's **media connection gateway** (`GET /v1/calling/media/rtc[/v1]`, provider edge:
`media/gateway.rs` neutral, `media/livekit_gateway.rs` LiveKit relay):

1. the presented credential (Bearer header or `access_token`) must be edge-sealed,
   unexpired and name one room, one identity and a generation (a LiveKit-signed token
   is refused);
2. Calling's authority (`ConnectionAdmission`, implemented against the database, no
   cache) requires: the session exists, belongs to that Call (and scope),
   `media_access == AUTHORIZED`, not ended, not removed, its participant still in the
   Call, the Call in a media state with an active media binding, and the credential's
   generation equal to the session's **current** `media_generation`. Otherwise the
   upgrade is refused with 401 **before LiveKit is contacted** — no participant, no
   ICE, no media;
3. **fail closed:** a database error, unreadable state, missing or malformed generation,
   unknown identity or malformed token is a refusal (503 / 401), never an admission;
4. an admitted connection is relayed with a fresh, single-use, 30 s **join-only** token
   minted for exactly that room / identity / generation; join-then-admit (§4) then
   applies the media rights;
5. on the way back, every `refresh_token` LiveKit sends is re-checked against the
   database and **re-sealed with the edge key** (or the connection is closed);
   `room_moved` (which carries a token) and non-protobuf frames close the connection;
   client simulation requests are dropped; only the SDK's documented query parameters
   are forwarded. The client therefore never holds a token LiveKit itself accepts — not
   the original credential, not a refreshed one.

**Generation semantics.** `media_generation` is the session's credential epoch: each
issuance increments it, and only the current generation is admitted (generation N is
refused once N+1 exists). Revocation (`media_access` REVOKED — removal, session end,
participant retirement, terminal Call) is terminal in the database, so every generation
of a revoked session is permanently unusable; both conditions are checked at the
gateway and again at every join callback and reconciliation pass.

**Legitimate reconnect.** An AUTHORIZED session presenting its current generation —
original credential or the server-refreshed, edge-sealed one the SDK holds — is admitted
again; the gateway notifies Calling when LiveKit accepts the (re)join, so permissions are
re-applied immediately (also after a resume).

**Performance.** Admission is per connection (a few indexed reads), never per packet;
there is no cache and therefore no stale-cache window.

**Deployment.** Clients are given only the gateway URL (`CALLING_MEDIA_GATEWAY_URL`).
LiveKit's signaling port should not be exposed to clients either, but security does not
depend on network location: a client has no token LiveKit accepts.

**Proof.** The real-LiveKit certification (`rtc_certification.rs`, CI
`calling-rtc-certification.yml`, three rounds per run) captures the server-refreshed
privileged token from the real SDK, verifies it is the admitted session's (`canPublish` /
`canSubscribe` true, current generation), removes the session, confirms REVOKED and the
provider disconnect, then presents the saved token from a new client: refused by the
gateway (401) and refused by LiveKit when sent around the gateway ("invalid API key"),
with no join callback, no provider participant and no media delivered; the original
join-only credential, a superseded generation and a terminal Call's credential are
refused; a legitimate current-generation reconnect regains media. Mutation runs
(generation comparison removed; revocation check removed; gateway admission bypassed;
refreshed tokens passed through unsealed — the previous behaviour) each make the
certification fail. The residual-risk statement of the first version of this ADR is
withdrawn on that evidence.

### 6. Callbacks are evidence only

`POST /internal/provider/media/events` accepts only provider-signed callbacks (LiveKit:
HS256 JWT issued by the API key, carrying the SHA-256 of the exact body; 30 s leeway);
unsigned or forged callbacks are 401 before parsing. Each event is stored once
(`call_media_provider_events`, UNIQUE (adapter, provider event id)); duplicates are
no-ops. Callbacks can confirm or deny media facts of existing sessions; they never
create participants, authorize Calls, widen permissions or resolve principals, and a
late callback never resurrects a terminal Call (a lingering room is torn down instead).
Only normalized, bounded evidence is stored — no raw payload, token or secret.

### 7. CONNECTED and ENDED need real evidence

A token being issued never makes a Call CONNECTED: CONNECTED requires admitted,
authorized, connected sessions of at least two different participants. END goes through
ENDING; ENDED requires cleanup evidence (connections removed, room terminated, or
confirmed absent). Usage (`CALL_CONNECTED_TIME`, `PARTICIPANT_CONNECTED_TIME`,
`SESSION_CONNECTED_TIME`) comes from Calling's own interval evidence; quality is recorded
only where the provider offers metrics (LiveKit adapter: none yet — nothing is invented).

### 8. Reconciliation

A multi-instance-safe worker (`reconcile_due`: `FOR UPDATE SKIP LOCKED` leases on
`call_media_sessions.next_reconcile_at`) compares Calling's sessions with the provider's
participants and repairs: evict unknown / revoked / stale connections, re-apply
permissions, record joins / absences, re-ensure a missing room for a live Call, finish
pending removals, and terminate rooms of terminal Calls. Repairs never create business
authority. Creation interrupted between transactions is resumed from the binding.

### 9. Certification

`tests/rtc_certification.rs` runs against a real LiveKit with real WebRTC clients and
real webhooks (CI: `calling-rtc-certification.yml`): the shared provider contract,
deterministic idempotent rooms, join-only grants, server permission changes, voice /
video / group calls, hold / resume at the provider, removal and refreshed-token re-entry
(§5), superseded generations, terminal Calls, legitimate reconnect, room termination
without resurrection, forged webhook rejection, reconciliation and unknown-participant
quarantine. The same provider contract runs against the test
provider used by the PostgreSQL suites.

## Consequences

* Another media provider is a new adapter implementing the trait and passing the same
  contract and certification; no change to the Call domain.
* The Meeting Service and Calling can use different LiveKit deployments, keys and
  versions independently.
* Admission adds a few indexed database reads before each media connection and one
  provider round trip after each join; publishing starts only after it.
* Media signaling is relayed by Calling's gateway (provider edge); media itself
  (WebRTC / UDP) flows directly between clients and LiveKit.
