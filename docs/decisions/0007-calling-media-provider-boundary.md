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
RoomService over HTTPS, HS256 tokens, webhook verification). It is the only file that
names the provider (architecture guard with mutation tests). No LiveKit SDK crate is
used; no LiveKit type crosses into `src/calls/`. Configuration is Calling's own:
`CALLING_MEDIA_PROVIDER` (`none` default | `livekit`), `CALLING_LIVEKIT_URL`,
`CALLING_LIVEKIT_API_URL`, `CALLING_LIVEKIT_API_KEY`, `CALLING_LIVEKIT_API_SECRET`
(≥ 32 characters), `CALLING_LIVEKIT_API_TIMEOUT_MS`. TLS is required except on loopback;
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

### 3. Credentials are derived from server state and are join-only

`POST /v1/calls/{id}/sessions/{sid}/media-credentials` (API credential) and
`POST /v1/calling/sessions/{sid}/media-credentials` (endpoint's signaling credential)
issue a credential only for a session whose `media_access` is AUTHORIZED (the
initiator's session, or the single winning device of a participant) while the Call is in
a media state. The body must be empty. The response carries URL, token, expiry and the
capabilities the connection will receive — nothing else. TTL:
`CALLING_MEDIA_CREDENTIAL_TTL_SECONDS` (default 300, 30–900).

The LiveKit token grants **room join only**: `canPublish`, `canSubscribe`,
`canPublishData` false, no publish sources, no room admin / create / list / record. It
binds the session's server-generated media identity (`cfmid_…`) and the session's
`media_generation` (attribute `cfm.gen`, incremented on every issuance).

### 4. Join-then-admit, enforced by the provider

Media rights are applied **server-side after the join is verified** (signed
`participant_joined` callback, or reconciliation): the admission decision
(`media/events.rs`) gives a connection permissions only if its identity is a known,
AUTHORIZED, non-ended session of this Call, the generation matches, and the Call is in a
media state; permissions are `MediaPermissions::effective(authorized, kind, state)`
(VOICE: audio + subscribe; VIDEO: + camera; held: no publishing, subscription kept;
never data). Anything else is stripped and removed (evicted). Unknown identities are
quarantined and evicted, never attached to anyone.

### 5. Stale re-entry prevention

Removing a session (or ending a Call) revokes `media_access` for ever
(NONE → AUTHORIZED → REVOKED, database trigger), stops further credential issuance,
strips the connection's permissions, removes it at the provider and marks enforcement
pending until the provider confirms. LiveKit does allow a removed identity to reconnect
with a still-valid token; certified against a real server, such a connection holds no
publish / subscribe right (join-only token), receives no media, and is evicted
(`PARTICIPANT_REMOVED`) as soon as the join is reported, with reconciliation as the
backstop.

**Residual risk (documented):** while a connection is admitted, LiveKit may hand the
client refreshed tokens reflecting its *current* permissions. A client that saved such a
token before being removed could rejoin with those grants until admission evicts it — a
window bounded by webhook delivery (sub-second in certification) and, at worst, by the
reconciliation interval (`CALLING_MEDIA_RECONCILE_INTERVAL_SECONDS`, default 15) and the
token's expiry. Short credential TTLs and revocation-on-removal keep it bounded.

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
video / group calls, hold / resume at the provider, removal and stale re-entry, room
termination without resurrection, forged webhook rejection, reconciliation and
unknown-participant quarantine. The same provider contract runs against the test
provider used by the PostgreSQL suites.

## Consequences

* Another media provider is a new adapter implementing the trait and passing the same
  contract and certification; no change to the Call domain.
* The Meeting Service and Calling can use different LiveKit deployments, keys and
  versions independently.
* Admission adds one provider round trip after each join; publishing starts only after
  it.
