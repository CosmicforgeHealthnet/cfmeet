# Phase 2 Meeting Domain (`/api/v2`) — Parts A–I

Spec: *CF Meet Phase 2 Backend Implementation Specification v1.1*. Current→target map:
[`docs/phases/phase-02/parts-a-d-implementation-map.md`](../../../docs/phases/phase-02/parts-a-d-implementation-map.md).

## Domain

```
MeetingSpace (meeting_spaces)            persistent definition, owner, policies, meeting_code
 ├─ MeetingRecurrenceRule                RFC 5545 RRULE in the space timezone
 ├─ MeetingMember                        persistent membership (not participation)
 ├─ MeetingInvitation                    hash-only bearer invitation
 └─ ConferenceSession                    one occurrence / live call
     └─ ConferenceParticipant            ONE logical person per conference
         └─ ParticipantSession           ONE device / network / media session
ConferenceBan · ConferenceAuditEvent (append-only) · OutboxEvent (transactional)
Foundations (schema only): meeting_consents, meeting_artifacts, breakout_rooms/assignments,
meeting_polls/responses, meeting_questions/votes
```

## Authority

| Concern | Authority |
|---|---|
| Identity, ownership, membership, invitations, policy, roles, admission, lifecycle, lock, bans, recurrence | PostgreSQL |
| Rate-limit counters, token revocation cache | Redis (degrades safely; never identity authority) |
| Media enforcement | `RtcProvider` (LiveKit) — enforces, never decides |
| Delivery | `outbox_events` (written in the business transaction) |

`ActorContext` (`services/actor.rs`) is resolved server-side from a verified bearer token
(registered user) or a server-issued `X-Participant-Token` (guest / invited external). Request
bodies use `deny_unknown_fields`: `userId`, `role`, `ownerUserId`, `organizationId`, `admissionState`
… cannot be submitted.

## Modules

| Module | Responsibility |
|---|---|
| `services/meeting_space.rs` | create / read / update (versioned) / archive / duplicate / owner transfer / instant meeting |
| `services/conference.rs` | occurrences, explicit state machine, start/lock/unlock/end/cancel/fail/reschedule |
| `services/recurrence.rs` | RRULE validation, timezone-aware bounded materialization, exceptions |
| `services/meeting_access.rs` | access policies, passcode (bcrypt + rate limit + audit), adapters |
| `services/membership.rs`, `services/invitations.rs` | membership CRUD, invitations (hash-only, atomic use) |
| `services/admission.rs` | join pipeline, waiting room, admit/deny (bulk, idempotent), return-to-waiting, role change, remove, ban, attendance |
| `services/meeting_permissions.rs` | roles, Appendix A matrix, POLICY overrides, grant authority, media projection |
| `services/meeting_events.rs` | audit + outbox writers |
| `services/rtc_provider.rs` | `RtcProvider` contract + `LiveKitProvider` (RoomService admin API, webhooks) |
| `services/rtc_control.rs` | Part E control plane: provider-confirmed lifecycle, command execution, permission sync, mute |
| `services/rtc_webhooks.rs` | verified, deduplicated provider webhooks → ParticipantSession state |
| `services/outbox.rs` | crash-safe outbox claiming (leases, SKIP LOCKED), retry / dead-letter |
| `services/realtime.rs` | realtime envelope, Redis publisher, subscription authorization, tickets, replay |
| `services/rtc_metrics.rs` | control-plane counters |
| `services/compat.rs` | identifier resolver, lazy legacy mapping, shadow comparison, backfill |
| `workers/conference_scheduler.rs` | materialization + scheduled end (idempotent, multi-instance safe) |
| `workers/outbox_relay.rs` | outbox delivery: realtime → Redis, provider commands → RtcControl |
| `workers/conference_reconciler.rs` | PostgreSQL ↔ provider drift detection and repair |
| `handlers/internal.rs` | internal LiveKit webhook + metrics routes |
| `handlers/v2/*`, `routes/v2.rs` | thin transport |

## Conference lifecycle

```
CREATED ─prepare→ PREPARING ─(provider room confirmed)→ WAITING
CREATED|PREPARING|WAITING ─start→ LIVE   (row lock + unique "one LIVE per space"; provider room required)
LIVE ─end→ ENDING ─(room terminated + absence confirmed, sessions finalized)→ ENDED
CREATED|PREPARING|WAITING ─cancel→ CANCELLED
PREPARING|LIVE|ENDING ─fail→ FAILED
```

ENDING immediately stops admission and new usable join credentials. ENDED is only reached
through the Part E control plane once the provider confirmed termination (see below); a
credential-only provider never advances ENDING.

## Join pipeline

1. resolve conference + space → 2. ActorContext → 3. lifecycle (LIVE only) → 4. space ACTIVE →
5–6. access policy (owner / membership / invitation / passcode / org / domain / adapter) →
7. lock → 8. ban → 9. logical participant (one per user; guests get opaque `gst_…` identity and a
`pt_…` credential stored as SHA-256) → 10. role from authoritative sources → 11. waiting-room
policy → 12. COMMIT → 13. ParticipantSession (ADMITTED only, server media identity) →
14. RTC credential via `RtcProvider` after commit (re-checks LIVE; fails closed) → 15. response.

A WAITING participant never receives a credential (`rtcStatus = NOT_ISSUED_WAITING`). With
`FEATURE_RTC_PROVIDER_CONTROL_V2=false` business admission still commits and the response says
`PROVIDER_CONTROL_DISABLED` — no credential is fabricated. With provider control on, step 14 first
confirms the room with the provider (re-creating a room lost at the provider); an unreachable
provider yields `503 RTC_PROVIDER_UNAVAILABLE` and the session is closed — never a fallback token.

## Database invariants

Unique meeting code / legacy id / occurrence key / active membership / registered participant per
conference / guest identity per conference / media identity per conference; one LIVE v2
conference per space; composite FK `participant_sessions(participant, conference)`; triggers:
active sessions require an ADMITTED participant, leaving ADMITTED terminates sessions, audit rows
are append-only; invitation `usage_count <= use_limit`.

## Feature flags

`FEATURE_MEETING_V2_ENABLED`, `FEATURE_WAITING_ROOM_V2_ENABLED`, `MEETING_V2_WRITE_ALLOWLIST`,
`FEATURE_MEETING_V2_SHADOW_READ`, Part E (`FEATURE_RTC_PROVIDER_CONTROL_V2`,
`FEATURE_CONFERENCE_REALTIME_ENABLED`), Part F (`FEATURE_COLLABORATION_V2_ENABLED`: media controls and
collaboration routes, breakout closer worker, webinar attendee defaults), and placeholders
(`FEATURE_RECORDING_V2_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_HEALTHCARE_PROFILE_ENABLED`).
`FEATURE_LEGACY_MEETING_WRITE_DISABLED` is not honoured during A–D.

## Legacy compatibility

Legacy rows are backfilled idempotently (`migration/src/phase2_backfill.rs`). Legacy-origin
conferences remain legacy-managed (read-only in v2, refreshed from the legacy row) until
certification. v2 codes (`xxx-xxxx-xxx`) and legacy codes (8 uppercase) never collide; the
resolver accepts v2 id, v2 code, legacy id and legacy code.

## A–D closure additions

**Status:** Part A PASS · Part B PASS · Part C PASS · Part D PASS · Overall A–D PASS.
HEALTHCARE_APPOINTMENT is integration-ready; the production HealthNet connection is a deferred
external integration (see below). Next: Part E — RTC control plane & realtime state.

### Legacy v1 join identity
`POST /api/v1/meetings/{id}/join` and `/api/v1/meetings/join/{code}` resolve identity through
`ActorContext`. The body `user_id` is a compatibility claim only: 401 without a bearer token,
403 if it differs from the authenticated user. Guests resume only with their server-signed guest
token (`Authorization: Bearer <guest access_token>`); display names are never matched. The
API-key route joins as the key owner.

**Web client compatibility (cross-spec patch, final A–D closure):**
`apps/web/src/services/meeting.service.ts#joinMeeting` now uses the authenticated `api` axios
client instead of `publicApi`, so every join and waiting-room poll carries
`Authorization: Bearer <cookie access_token>` — the signed-in user's token, or the guest token
the pre-join screen already persists from the first guest join. An expired token gets 401, the
client's existing refresh interceptor calls `POST /api/v1/auth/refresh` (guest refresh tokens
keep the guest role and participant subject) and retries. No other frontend code changed.
Regression coverage: `tests/legacy_v1_join.rs`, `tests/legacy_v1_web_compat.rs`.

### Organizations (ORGANIZATION_ONLY)
No organization source existed in the monorepo; the meeting service is the current CF account
authority, so `organizations` / `organization_memberships` live here behind
`OrganizationDirectory` (`DbOrganizationDirectory`). Only ACTIVE memberships of ACTIVE
organizations held by non-deactivated accounts count; membership is re-resolved on every request
(including reconnect). Management: `POST/PATCH /api/v2/admin/organizations` (platform admins),
`GET/PUT /api/v2/organizations/{id}/members[/{userId}]` (platform admins, org OWNER/ADMIN within
grant limits), `GET /api/v2/me/organizations`.

### DOMAIN_RESTRICTED
Domain = domain of the account email only when verified (Google accounts, or local accounts in
ACTIVE status when email verification is required). Request-body email/domain are rejected.

### HEALTHCARE_APPOINTMENT
`AppointmentAuthorizationAdapter::authorize(actor, space, conference, external_context)` →
Authorized | Denied. `HttpAppointmentAuthorizationAdapter` implements the HealthNet contract
(see `services/appointment_authorization.rs`) and fails closed on errors, timeouts, malformed or
inactive appointments; roles are capped below OWNER.

**HEALTHCARE_APPOINTMENT external integration: READY — integration contract implemented.**
Adapter abstraction, HTTP adapter, configurable URL (`HEALTHNET_APPOINTMENT_AUTHZ_URL`),
service-to-service credential (`HEALTHNET_APPOINTMENT_AUTHZ_TOKEN`), timeout
(`HEALTHNET_APPOINTMENT_AUTHZ_TIMEOUT_MS`), fail-closed behaviour, `appointmentRef` /
`externalContext` forwarding, server-resolved actor sent to the authority, validity window,
server-side role mapping with OWNER never grantable, and deny-all when unconfigured — all covered
by mocked-HTTP tests in `tests/appointment_revocation.rs` (allow, deny, 5xx, malformed, timeout,
expired / not-yet-valid, wrong service credential, unconfigured).

**PRODUCTION HEALTHNET CONNECTION: DEFERRED — will be wired when HealthNet integrates with CF
Meet.** This is an external integration/deployment task, not unfinished CF Meet A–D architecture.
CF Meet stores no HealthNet appointment data and embeds no clinical rules; until the URL is
configured, `UnconfiguredAppointmentAuthorization` denies all appointment-based joins.

### Token revocation
Per-token revocation (logout) lives only in Redis (`TokenBlacklistService`); account deactivation
is authoritative in PostgreSQL and checked on every request. If Redis is unreachable a token is
honoured with `revocation_checked = false`; spaces with security profile HEALTHCARE or
HIGH_SECURITY, or `accessConfig.requireVerifiedRevocation = true`, then fail closed with
`503 REVOCATION_CHECK_UNAVAILABLE` (reads degrade to the SUMMARY view). Redis never holds meeting
state authority.

### Recurrence exceptions
`PUT /api/v2/meeting-spaces/{id}/recurrence/exceptions` cancels or reschedules any future
occurrence (materialized or not), keyed by the original wall-clock start in the series timezone
(`meeting_recurrence_exceptions`, unique per rule). Idempotent. The materializer applies the
exception when it generates the occurrence; already-materialized rows are updated immediately.
Conference-level cancel/reschedule of recurring occurrences record the same exception. Timezone
re-anchoring keeps exceptions aligned. Materialization stays bounded (30 days / 50 per run).

## Part E — RTC control plane & realtime state

PostgreSQL is the authority; LiveKit only enforces committed decisions; Redis only fans events
out. Everything lives inside meeting-service.

### RtcProvider

`RtcProvider` (provider-neutral; business code never calls LiveKit directly) — LiveKit status:

| Method | LiveKitProvider |
|---|---|
| `create_room(conference)` | RoomService `CreateRoom` (idempotent; canonical name `cf-<conference uuid>`) |
| `get_room(conference)` | `ListRooms` filtered by name |
| `terminate_room(conference, reason)` | `DeleteRoom` (absent room = success) |
| `issue_join_credentials(session, permissions, ttl)` | signed access token; identity = server media identity `ps_<session>`; grants from `MeetingPermissionService` only; never admin/record/create |
| `remove_participant(session, reason)` | `RemoveParticipant` (absent = success) |
| `update_permissions(session, permissions)` | `UpdateParticipant` (publish sources / subscribe / data) |
| `mute_track(session, track_kind)` | `GetParticipant` + `MutePublishedTrack` per matching track |
| `list_participants(conference)` | `ListParticipants` (identity, state, grants, tracks) |
| `list_rooms(prefix)` | `ListRooms` (reconciliation) |
| `start_recording` / `stop_recording` | interface kept; gated (`NotSupported`) until Part G |
| `reconcile(conference)` | room + participants snapshot |
| `parse_webhook(body, authorization)` | LiveKit webhook verification (JWT signed with the API secret, `sha256` = body hash) |

Every admin call is bounded (`LIVEKIT_API_TIMEOUT_MS`). Errors map to a stable
`RTC_PROVIDER_UNAVAILABLE`; provider details are logged, never returned. A provider declares
`capabilities()`; provider control is active only when `FEATURE_RTC_PROVIDER_CONTROL_V2` is on
AND the provider controls rooms. A credential-only provider keeps the Parts A–D semantics.

### Lifecycle

- PREPARE: CREATED → PREPARING commits with an `rtc.room_prepare_requested` command; after commit
  the command runs (`RtcControl::ensure_room`), and only the provider's confirmation moves
  PREPARING → WAITING (`conference.ready`, `rtc_room_ready_at`). Concurrent prepares serialize on
  the row lock; `CreateRoom` is idempotent; on failure the conference stays PREPARING and the
  command/reconciler retry. START from CREATED/PREPARING runs prepare first and refuses to go LIVE
  (`503`) until the room is confirmed; authorization and the start window are checked before any
  provider call.
- END: LIVE → ENDING commits with `rtc.room_terminate_requested` (joins and credentials refused from
  that commit). After commit: `DeleteRoom` → `ListRooms` confirms absence → active sessions are
  finalized (`TERMINATED`, `CONFERENCE_ENDED`) → ENDED (`conference.ended`, `rtc_room_terminated_at`).
  Any failure leaves ENDING, logs `alert=RTC_TERMINATION_FAILED`, increments
  `room_terminate_failures`, and is retried by the relay, a repeated END and the reconciler.
  A verified `room_finished` webhook is also accepted as confirmed absence.
- CANCEL / FAIL of a conference that had a room queue the same termination.

### Credentials, grants, moderation

- Credentials only after the admission/session commit, re-checked after minting (conference still
  LIVE, session still active); bounded TTL (`RTC_CREDENTIAL_TTL_SECS`, default 600 s, never longer
  than the session). VIEWER = subscribe-only (no publish sources).
- Role change commits, then `rtc.permissions_update_requested` pushes the grants the permission
  engine derives from the CURRENT role (payload grants are ignored) to every live session;
  `participant.media_permission_changed` is emitted when the projection changes. On provider
  connect the current grants are re-asserted (a credential minted before a demotion cannot keep
  publish rights).
- Remove / ban / return-to-waiting commit (DB trigger terminates sessions), then
  `rtc.participant_remove_requested` disconnects every provider session. The command resolves the
  identity from the session row of the same conference. Stale credentials: a provider join whose
  identity does not map to an ACTIVE session of an ADMITTED participant of that conference is
  evicted immediately (webhook) or by the reconciler. Reconnect always re-runs the join pipeline.
- Mute: `POST /api/v2/conferences/{id}/participants/{pid}/mute {trackKind}` (permission
  `participant.mute`, conference-scoped target, moderation rank) → `mute_track`. Cross-conference
  targets are `PARTICIPANT_NOT_FOUND` before any provider call.

### Webhooks

`POST /internal/rtc/livekit/webhook` (not the user webhook system; enabled only with provider
control). Verification: LiveKit JWT signed with the API secret, issuer = API key, `sha256` claim =
body hash, expiry enforced (60 s leeway) — otherwise `401`. Dedupe: `rtc_webhook_events(event_id)`
inserted in the same transaction as the state change (`DUPLICATE` on replay). Room → conference by
`rtc_room_name`; identity → session only within that conference. Handled: `participant_joined`
(PENDING → CONNECTED, `participant.joined`), `participant_left` / `participant_connection_aborted`
(→ DISCONNECTED, `participant.left`), `room_finished` (confirmed absence: ENDING → ENDED; a LIVE
room loss clears readiness so the next join/reconciler re-creates it), `room_started` (records the
room id). Callbacks never create participants or change role/admission/membership/access; unknown
rooms are recorded and ignored.

### Reconciler

`workers/conference_reconciler.rs` — see the module docs for the drift table (PREPARING not ready,
LIVE room missing, ENDING not finalized, orphan rooms, unmapped rooms (escalated, never deleted),
provider participants without session (evicted), late webhooks (session marked CONNECTED), sessions
absent at the provider beyond grace (DISCONNECTED), permission drift (re-pushed)). Singleton via
`worker_leases` (crash-safe expiry), bounded batches, findings in `rtc_drift_events`.

### Outbox, realtime, recovery

- `outbox_events.channel`: `REALTIME` (client events) vs `PROVIDER_COMMAND` (`rtc.*`, never
  delivered to clients). Client events of a conference carry `conference_seq`: allocated by
  incrementing `conference_sessions.event_seq` in the emitting transaction, so the sequence is
  gap-free and in commit order.
- Relay: leases via `available_at` + `locked_by` with `FOR UPDATE SKIP LOCKED` (multi-worker,
  crash-safe: an expired lease is re-claimed; a zombie cannot complete a row it lost). Transient
  failures (Redis/provider outage) retry with capped backoff and are never dropped; poison rows
  (unknown/malformed/out-of-scope) are dead-lettered `FAILED`.
- Redis: `PUBLISH cfmeet:rt:conference:{id}` with the envelope `{eventId, eventType, schemaVersion,
  conferenceId, meetingSpaceId, sequence, occurredAt, correlationId, payload}` (payload scrubbed of
  provider ids / secrets). Redis down → writes still commit, events stay PENDING, WebSockets
  degrade to tailing PostgreSQL (`degraded` frame), REST replay keeps working.
- WebSocket: `POST /api/v2/conferences/{id}/realtime/ticket` (authorized; 60 s one-shot ticket,
  domain-separated key) then `GET /api/v2/conferences/{id}/realtime?ticket=…&cursor=…` (header auth
  also accepted). Admitted participants / members / owners get `FULL`; waiting participants
  `SELF_ONLY` (lifecycle + their own events). Re-authorized on relevant events and every 15 s;
  removed/banned subscribers get `revoked` + close `4403`; conference end → `closed`.
- Recovery: reconnect with `cursor` → missed events replayed in order (≤ 500); otherwise
  `resync_required` with `canonical: GET /api/v2/conferences/{id}`. `GET /api/v2/conferences/{id}/events?after=`
  offers the same replay over REST. Consumers must tolerate duplicates across reconnects.

### Observability

`GET /internal/rtc/metrics` (`X-Internal-Token` = `INTERNAL_API_TOKEN`) → counters (provider
errors, room create/terminate failures, credentials, removals, permission updates, mutes, webhooks
received/rejected/duplicate/unknown, evictions, drift detected/repaired/escalated, outbox
published/retried/dead-lettered, realtime publish failures, WebSocket connections/rejections) and
outbox backlog per channel.

### Certification

- Mocked provider: `tests/rtc_*.rs` run the production `LiveKitProvider` over HTTP against an
  in-process mock RoomService that speaks the LiveKit Twirp/protobuf protocol, verifies admin
  grants, and simulates SFU clients that enforce credential grants.
- Real self-hosted LiveKit: `scripts/livekit-certification/run.sh` starts a local
  `livekit-server` (config in the same folder, webhooks to `127.0.0.1:18089`) and runs
  `tests/livekit_certification.rs` (`#[ignore]`d in the default suite) with the `rtcprobe` pion
  WebRTC client: room create/lookup, credential + real media join, verified webhook session
  sync, participant listing, mute, permission update, VIEWER publish refusal, removal,
  termination, reconciliation, and (Part F) host mute, audio/video lock and restore,
  screen-share policy enforcement, `track_published` screen-share evidence and host stop.

## Part F — Media controls & collaboration

Everything lives inside meeting-service (no new deployable service). PostgreSQL stays the
authority; LiveKit enforces the committed media grant; Redis only fans out. Routes are under
`/api/v2/conferences/{id}/…` and exist only with `FEATURE_COLLABORATION_V2_ENABLED`
(otherwise `404 FEATURE_DISABLED`). Every request runs the same loader: identity (anonymous /
legacy guest → `401`) → conference scope + state → admitted participant → permission
(`MeetingPermissionService`, baseline `v2`) → collaboration policy. Recording, transcription and
AI are Part G and are not implemented here (policy only stores consent / notice metadata).

### Collaboration policy

Effective policy = meeting-type defaults → `meeting_spaces.collaboration_policy` →
`conference_sessions.collaboration_policy` (sparse patches, unknown keys rejected). Updated with
`PATCH …/collaboration-policy {collaborationVersion, changes}` (optimistic: stale version →
`409`) or `PATCH …/chat-policy`. Fields: `screenSharePolicy` (`HOST_ONLY` | `PRESENTERS_ONLY` |
`ALL_INTERACTIVE`), conference-wide `audioLocked` / `videoLocked`, `maxSpotlights`, chat
(`chatEnabled`, `chatSendMode` `EVERYONE`|`HOSTS_ONLY`, `privateChatMode`
`EVERYONE`|`HOSTS_ONLY`|`DISABLED`, `breakoutChatEnabled`, `mainChatVisibleInBreakouts`),
`reactionsEnabled` / `allowedReactions`, `handRaiseEnabled`, `qnaEnabled` /
`qnaAnonymousAllowed`, `pollsEnabled`, `maxBreakouts`, `breakoutRecordingPolicy` (metadata only),
`requiredConsents`, `consentVersion`, `aiNotice`. Policy changes re-push grants to every live
session and re-scope every realtime subscriber.

### Media controls

`effective_media(role, policy, locks)` = role grant ∧ screen-share tier ∧ ¬conference-wide lock
(roles holding `participant.mute` are exempt) ∧ ¬participant lock. It is the single projection
used by join credentials, `POST …/media-credential` (refresh), permission sync after any change,
and the reconciler — there is no second permission matrix.

- MUTE NOW: `POST …/participants/{pid}/mute` (Part E) mutes the live track; the participant may
  unmute.
- LOCK: `POST|DELETE …/participants/{pid}/audio-lock | video-lock | screen-share-lock` persists
  `*_locked_at/by`, commits, then removes the publish source at the provider
  (`providerEnforcement: ENFORCED | PENDING_RETRY`). Real LiveKit unpublishes a live track whose
  source is withdrawn and refuses re-publication until unlocked.
- Screen share: evidence comes only from verified `track_published` / `track_unpublished`
  webhooks for `SCREEN_SHARE` video (screen-share audio is ignored). `screenshare.started` /
  `screenshare.ended` are emitted once per share (late/duplicate evidence is deduplicated).
  `POST …/screen-share/{pid}/stop` withdraws the grant, mutes, restores the grant, and emits
  `screenshare.ended` only after provider confirmation. Withdrawing the grant (lock, policy,
  demotion) ends a recorded share with reason `PERMISSION_REVOKED`.
- Spotlight: `POST …/spotlights`, `DELETE …/spotlights/{pid}`; bounded by `maxSpotlights`.

### Pre-join

`GET …/pre-join` returns policy-derived capabilities (media defaults, chat/Q&A/poll availability,
display-name editability, required consents + version, AI notice). Required consents are enforced
at join (`CONSENT_REQUIRED`) and stored in `meeting_consents`; in HEALTHCARE / HIGH_SECURITY
profiles registered users join with their profile name. Webinar meetings default new attendees to
`VIEWER` (subscribe-only) while keeping policy-controlled Q&A, reactions, polls and hand raise.

### Chat (Phase 5 compatible)

`conference_messages` (scope `CONFERENCE` | `BREAKOUT` | `PRIVATE`, `client_message_id`
idempotency, soft delete by moderators, cursor pagination `GET …/messages?before=&limit=`). Access
is behind the `MessageStore` trait (`PgMessageStore` today), so Phase 5 Unified Messaging can
replace the store without changing the HTTP or realtime contract. Policy gates: chat disabled,
announcements-only, private chat mode, breakout chat, per-participant restriction
(`POST|DELETE …/participants/{pid}/chat-restriction`).

### Reactions & hand raise

Reactions are transient: validated against `allowedReactions`, rate-limited, published straight
to Redis as `{transient: true, audience, event}` (no outbox row, no replay) and delivered only to
subscribers that pass the audience check. Hand raise is durable (`hand_raised_at` /
`hand_lowered_at`), idempotent, ordered by raise time (`GET …/hand-raise`); self-lower is always
allowed, lowering others requires `hand.lower_other`.

### Breakouts

- One provider room per breakout: `cfbr-<breakout uuid>` (distinct from the main `cf-` prefix).
  `breakout_rooms.status` `CREATED → OPEN → CLOSING → CLOSED`, `capacity`, `closing_at`,
  `version`; active names are unique per conference.
- Assignment modes `MANUAL` / `RANDOM` / `BALANCED` / `MOVE`; one active assignment per
  participant. A participant has exactly one media context: host visit room → active assignment
  in an OPEN/CLOSING room → MAIN. Credentials, the realtime audience and the reconciler all use
  that context; `rtc.participant_reroute_requested` removes provider sessions from the previous
  room, and a provider join in the wrong room is evicted (`STALE_MEDIA_CONTEXT`).
- Close: `POST …/breakouts/{bid}/close {countdownSeconds}` persists `closing_at`; the
  `BreakoutCloser` worker (restart-safe) completes due closes, returns participants to MAIN and
  terminates `cfbr-` rooms. Ending the conference closes every breakout. The reconciler repairs
  missing / orphan / unmapped `cfbr-` rooms and context drift.

### Polls & quizzes

Typed questions (`SINGLE_CHOICE`, `MULTIPLE_CHOICE`, `SHORT_TEXT`, `LONG_TEXT`, `RATING`,
`RANKING`); `DRAFT → OPEN → CLOSED → ARCHIVED` (no reopening). Quiz answers are hidden until
results are visible and scores are computed server-side (client scores are ignored). Results
visibility `HOST_ONLY` | `AFTER_VOTE` | `AFTER_CLOSE`. Idempotent responses (client
`idempotencyKey` digest).

Anonymous privacy model: the "has voted" ballot (`meeting_poll_ballots(poll_id, participant_id,
request_digest)`) is stored separately from the answers (`meeting_poll_responses.participant_id`
NULL, `created_at` = poll opened time so ordering cannot re-identify voters). Anonymous
aggregates — for hosts too — are withheld until the poll is CLOSED; no API, event, audit row or
export links an anonymous answer to a participant.

### Q&A

`OPEN → ANSWERED | DISMISSED | ARCHIVED` plus pin / unpin. Votes are recomputed under a row lock
(no self-vote, one vote per participant); the first answer wins. Anonymous questions never expose
the author in any projection or event.

### Rate limits

Per participant (Redis `INCR`+TTL, in-process fallback): chat 20/10 s, reactions 10/10 s, hand
toggle 10/60 s, poll responses 20/60 s, Q&A submit 5/60 s, Q&A vote 30/60 s, announcements
5/60 s. Exceeded → `429 RATE_LIMITED`, `Retry-After` header and `retryAfterSeconds`.

### Realtime event contract

Audience is evaluated server-side per subscriber (`outbox_events.audience`; NULL = the Part E
rules). Every durable event is recoverable through cursor replay and the REST resource listed.

| Event | Audience | Payload (summary) | Persistence | Privacy | REST recovery |
|---|---|---|---|---|---|
| `participant.audio_locked/unlocked`, `video_*`, `screen_share_*` | conference | participantId, by, providerEnforcement | outbox | — | `GET …/collaboration` |
| `participant.muted` | conference | participantId, trackKind, mutedTracks | outbox | — | `GET …/participants` |
| `screenshare.started` / `screenshare.ended` | conference | participantId, reason | outbox | no provider track ids | `GET …/collaboration` |
| `participant.spotlighted` / `spotlight_removed` | conference | participantId | outbox | — | `GET …/collaboration` |
| `collaboration.policy_changed`, `chat.policy_changed` | conference | collaborationVersion, policy | outbox | — | `GET …/collaboration` |
| `chat.message_created` / `message_deleted` | main room, breakout, or sender+recipient (private) | message projection | outbox + `conference_messages` | private: participants only (moderators never implicitly) | `GET …/messages` |
| `chat.participant_restricted/unrestricted` | target + `chat.moderate` | participantId | outbox | — | `GET …/collaboration` |
| `reaction.sent` | current media context (main / breakout) | participantId, reaction | Redis only (transient) | — | none (ephemeral) |
| `hand.raised` / `hand.lowered` | conference | participantId, raisedAt | outbox | — | `GET …/hand-raise` |
| `breakout.created/opened/closing/closed` | conference | breakout projection, closingAt | outbox | — | `GET …/breakouts` |
| `breakout.assignment_changed` | `breakout.assign` holders | assignments | outbox | — | `GET …/breakouts` |
| `breakout.participant_moved` | moved participant + `breakout.assign` | participantId, from, to | outbox | — | `GET …/breakouts` |
| `breakout.announcement` | one breakout or all breakouts | message | outbox | — | — |
| `poll.created` | `poll.create` holders (drafts) | poll | outbox | quiz answers stripped | `GET …/polls` |
| `poll.started/closed/archived` | conference | poll (answers revealed only in results state) | outbox | — | `GET …/polls/{pid}` |
| `poll.response_recorded` | responder | pollId | outbox | anonymous: no identity | `GET …/polls/{pid}` |
| `poll.results_updated` | `poll.view_results` holders / per visibility | aggregates | outbox | anonymous aggregates only after CLOSED | `GET …/polls/{pid}/results` |
| `qna.question_*` | conference | question projection | outbox | anonymous: no author | `GET …/questions` |

Subscribers are re-authorized (and re-scoped) on `breakout.opened/closed`, policy changes and
their own `breakout.participant_moved`. Provider commands (`rtc.screenshare_stop_requested`,
`rtc.participant_reroute_requested`, `rtc.breakout_prepare_requested`,
`rtc.breakout_terminate_requested`) use the `PROVIDER_COMMAND` channel and never reach clients.

### Audit

Every host action is audited (`MEDIA_*_LOCK/UNLOCK`, `SCREEN_SHARE_STOP`, `SPOTLIGHT_*`,
`COLLABORATION_POLICY_UPDATE`, `CHAT_MESSAGE_DELETE`, `CHAT_PARTICIPANT_(UN)RESTRICT`,
`CHAT_ANNOUNCEMENT`, `HAND_LOWER_OTHER`, `BREAKOUT_*`, `POLL_*`, `QNA_*`, `CONSENT_RECORDED`) in
the same transaction as the state change and its outbox row. Audit metadata never carries message
bodies, poll answers or anonymous identities. Counters: `GET /internal/rtc/metrics` →
`collaboration`.

## Parts G–I — Artefacts, meeting profiles, operations

All three parts are internal modules of meeting-service (`services/artifacts/*`,
`services/profiles.rs`, `services/specialized.rs`, `services/quality/*`); no new deployable
service exists. Authorities are unchanged: PostgreSQL owns business state, LiveKit (RoomService
+ Egress) only executes media work, object storage only holds bytes, Redis / WebSocket only
deliver, the outbox carries durable events and provider commands. One additive migration
(`m20261005_000005_artifacts_profiles_operations`, verified up → down → up).

### Part G — Recording, artefacts, consent, transcription, intelligence

**Recording state machine** (`meeting_artifacts`, type `RECORDING`):
`REQUESTED → RECORDING → STOPPING → PROCESSING → READY → DELETED`, plus `FAILED`.

- `POST /conferences/{id}/recordings` checks permission (`recording.start`), the meeting profile
  (`recording.allowed`), configured provider + storage, and explicit consent; it commits
  `REQUESTED` with a server-chosen object key and the outbox command
  `rtc.recording_start_requested`, then dispatches. A partial unique index allows one active
  composite recording per conference. Consent and the profile are re-checked under the space
  (shared) and conference locks, so a concurrent withdrawal or profile restriction can never be
  bypassed.
- The start command (`RecordingService::execute_command`) first adopts an active, unowned egress
  of the room (provider-side idempotency), otherwise calls `StartRoomCompositeEgress`
  (H.264 + Opus MP4 with `RECORDING_ENCODING=h264-opus`, provider default otherwise).
- `RECORDING` only on provider evidence (`egress_started` / active egress); `STOPPING → PROCESSING`
  only on provider completion with an output file; `READY` only after storage verification
  (HEAD, provider-reported size, streamed SHA-256, MD5 vs ETag where applicable) — retried by the
  artefact worker with backoff, `FAILED` (`ARTIFACT_MISSING` / integrity) after
  `ARTIFACT_FINALIZE_MAX_ATTEMPTS`. Provider callbacks arrive through the verified LiveKit webhook,
  are applied idempotently and deduplicated by event id; evidence whose room does not match the
  recording's conference is rejected without mutation.
- Ending a conference stops every active capture in the same transaction (never waits).
- Reconciliation: stale `REQUESTED` (start timeout), lost completion callbacks (provider query),
  vanished provider jobs and unowned provider captures (escalated, never stopped by guess).

**Artefacts**: `GET /conferences/{id}/artifacts[?artifactType=]`, `GET …/artifacts/{artifactId}`,
`POST …/artifacts/{artifactId}/access` (authorize → short-lived signed URL, never stored, audited,
`ARTIFACT_SIGNED_URL_TTL_SECS` default 300), `DELETE …/artifacts/{artifactId}` (request →
storage delete → confirmed absence → `DELETED`; never `DELETED` before absence is confirmed),
`POST …/artifacts/reports` (`ATTENDANCE_REPORT`, `POLL_REPORT` — anonymous polls stay aggregate,
`CHAT_EXPORT` — main chat only, never private messages). Lookups are always scoped by conference
id; storage keys, buckets and provider ids are never returned. Retention: `retention_at` from the
profile (`recording.retentionDays`, `transcription.retentionDays`, …); the `ArtifactRetentionWorker`
(lease per row, restart-safe) deletes due artefacts.

**Storage**: `ArtifactStorage` trait; `S3ArtifactStorage` (SigV4, S3 / MinIO / R2,
`ARTIFACT_STORAGE_*`), `MemoryArtifactStorage` for tests. The Egress service writes into the same
bucket using its own storage configuration.

**Consent** (`meeting_consents`): independent types `RECORDING`, `TRANSCRIPTION`, `AI_PROCESSING`,
`DATA_SHARING`. `GET …/consents/requirements`, `POST …/consents {consentVersion, decisions}` (the
caller's own decisions only), `POST …/consents/{consentType}/withdraw`. `NOTICE_ONLY` never blocks;
`EXPLICIT_CONSENT_REQUIRED` blocks the provider start until every active participant consented
(targeted `consent.required` to the missing participants) and requires the active capture's
consent at join. Withdrawal modes: `STOP_FUTURE_PROCESSING` (stops an explicit-consent capture),
`REQUIRE_PARTICIPANT_EXIT` (participant returned to the waiting room), `STOP_PROCESSING_MODE`,
`NO_RETROACTIVE_DELETION`. A withdrawal never deletes retained artefacts.

**Transcription — classification: NOT REQUIRED for this release** (README "Future |
Intelligence: transcription, translation…", platform overview "Communications intelligence |
future"). The provider-neutral path is implemented and disabled by default
(`FEATURE_TRANSCRIPTION_V2_ENABLED=false`, no provider configured): `POST …/transcription`
(`LIVE` / `POST_MEETING` from a READY recording), `POST …/transcription/{jobId}/stop`,
`GET …/transcripts`, `GET …/transcripts/{artifactId}` (paged segments), translation
(`POST …/transcripts/{artifactId}/translations`, never overwrites the original), WebVTT caption
track. Provider callbacks: `POST /internal/transcription/events`, HMAC
(`X-CF-Timestamp`, `X-CF-Signature: sha256=…`, `TRANSCRIPTION_WEBHOOK_SECRET`), segments
idempotent per `(job, provider segment id)`, speaker identity resolved only to a participant of
the same conference.

**Intelligence**: `POST …/intelligence/requests {sourceArtifactId, kind}` over a READY transcript,
gated by `FEATURE_INTELLIGENCE_V2_ENABLED` (off), the profile processing location
(`DISABLED` / `SELF_HOSTED_ONLY` / `EXTERNAL_ALLOWED` checked against the provider's declared
location before any call) and AI consent. Speakers reach the provider as opaque labels.
Production AI summarization is not part of this release (no provider is configured).

### Part H — Meeting profiles, webinar / town hall, healthcare

**Profile resolver** (`MeetingProfileResolver`): base → meeting type → security profile (hard
ceilings) → organization (explicit values become ceilings) → space
(`meeting_spaces.profile_policy`) → conference → clamp. A stored layer that fails validation
switches to the restrictive fallback (`misconfigured: true`). `GET|PATCH
/meeting-spaces/{id}/profile-policy` (optimistic `profilePolicyVersion`; clamped fields are
reported). A tightened policy stops processing it no longer permits.

**Webinar / town hall**: attendees default to `VIEWER` (subscribe-only grant; Q&A, polls,
reactions and hand raise per policy). Promotion / demotion re-push provider grants
(`participant.promoted` / `participant.demoted`); stage limits (`stage.maxStageParticipants`,
`stage.maxPresenters`) return `STAGE_CAPACITY_REACHED`. Registration:
`POST /meeting-spaces/{id}/registrations` (public when enabled; only policy-approved fields;
idempotency key) returns a one-time `reg_…` admission token = a hash-only, VIEWER-role,
use-limited invitation consumed by the normal join pipeline (never an RTC grant).
`GET|DELETE …/registrations/{registrationId}` for organizers (cancel revokes the token).
Backstage: `POST|DELETE /conferences/{id}/backstage` (hosts and presenters only) routes the
participant into its own provider room `cfbs-<uuid>` (created before routing; a provider failure
leaves the context unchanged); demotion / removal evicts from it (re-checked under lock), the
webhook evicts stale backstage credentials, the reconciler repairs drift.

**Healthcare**: public / passcode entry is rejected (`PROFILE_POLICY_DENIED`); the waiting room is
forced; recording / transcription off with explicit consent, intelligence `DISABLED`, chat export
off, enhanced semantic audit. The appointment authority returns only references: an external role
token (mapped by policy — `DOCTOR→HOST`, `PATIENT→PARTICIPANT`, `NURSE→PRESENTER`,
`SPECIALIST→CO_HOST`, … — never OWNER), an opaque authorization reference and a validity window
that caps the session. Clinical payloads are dropped at parse time and never persisted; CF Meet
keeps the final admission decision; adapter errors and timeouts fail closed.

### Part I — Quality telemetry, health, incidents, operations

- **Join SLI**: every join records an outcome (success / waiting / denied / provider failure /
  timeout / client failure) on `conference_sessions` counters and the sink; denials are excluded
  from the failure rate. Join latency ends at the verified provider media connection
  (PENDING → CONNECTED, measured at callback receipt because provider timestamps have whole-second
  precision); reconnects count once per provider-confirmed reconnect.
- **Quality samples**: `POST /conferences/{id}/quality/samples` — technical metrics only (RTT,
  jitter, loss, bitrate, resolution, fps, publish / subscribe health, failure kind), bounded per
  sample, per batch (`QUALITY_MAX_SAMPLES_PER_BATCH`=20), per body (`QUALITY_MAX_BODY_BYTES`=16 KiB,
  `413 REQUEST_TOO_LARGE`) and per session rate (12/min, `QUALITY_RATE_LIMITED`); session resolved
  server-side; `batchId` makes retries idempotent; one row-locked aggregate update per batch on
  `participant_sessions`. `GET /conferences/{id}/quality` for holders of `quality.view`
  (no infrastructure identifiers).
- **Health**: `OperationsService` classifies conferences HEALTHY / DEGRADED / UNHEALTHY / UNKNOWN
  from PostgreSQL aggregates and open incidents using `QUALITY_*_DEGRADED|UNHEALTHY` thresholds.
- **Incidents**: `rtc_drift_events` generalised (status OPEN / RESOLVED, category, severity,
  `dedupe_key`, occurrences); one OPEN incident per dedupe key.
- **Telemetry sink**: `QualityTelemetrySink` trait; `PrometheusSink` with bounded labels only (no
  ids, names, emails). Sink failures never affect meetings or relational aggregates.
- **Internal operations routes** (require `X-Internal-Token` = `INTERNAL_API_TOKEN`, otherwise
  404): `GET /internal/metrics` (Prometheus text), `GET /internal/operations/conferences/health
  [?state=]`, `GET /internal/operations/conferences/{id}/health`, `GET
  /internal/operations/incidents[?status=]`.

### Parts G–I feature flags

| Flag | Default | Effect |
| --- | --- | --- |
| `FEATURE_RECORDING_V2_ENABLED` | false | Recording API (`RECORDING_DISABLED` otherwise) |
| `FEATURE_TRANSCRIPTION_V2_ENABLED` | false | Transcription / captions / translation |
| `FEATURE_INTELLIGENCE_V2_ENABLED` | false | Intelligence requests |
| `FEATURE_WEBINAR_MODE_ENABLED` | false | Registration, backstage |
| `FEATURE_HEALTHCARE_PROFILE_ENABLED` | false | HEALTHCARE meeting type / profile |
| `FEATURE_QUALITY_TELEMETRY_ENABLED` | false | Client quality-sample ingestion |

### Real certification

`scripts/livekit-certification/run.sh` with `EGRESS_BIN` and `MINIO_BIN` additionally starts
MinIO and LiveKit Egress and runs `real_livekit_part_g_recording_and_object_storage` (real
composite capture, start/stop, real callbacks, real MP4 in object storage, signed access,
unauthorized rejection, callback dedupe, delete + confirmed absence, plus direct S3 object /
metadata / checksum / signed read / delete / absence checks), `real_livekit_part_h_stage_and_backstage`
and `real_livekit_part_i_presence_and_mapping`.
