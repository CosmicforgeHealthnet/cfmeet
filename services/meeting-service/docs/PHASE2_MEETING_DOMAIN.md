# Phase 2 Meeting Domain (`/api/v2`) — Parts A–D

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
| `services/rtc_provider.rs` | `RtcProvider` trait + `LiveKitProvider` |
| `services/compat.rs` | identifier resolver, lazy legacy mapping, shadow comparison, backfill |
| `workers/conference_scheduler.rs` | materialization + scheduled end (idempotent, multi-instance safe) |
| `workers/outbox_relay.rs` | outbox delivery (started with Part E) |
| `handlers/v2/*`, `routes/v2.rs` | thin transport |

## Conference lifecycle

```
CREATED ─prepare→ PREPARING ─(provider ready, Part E)→ WAITING
CREATED|PREPARING|WAITING ─start→ LIVE   (row lock + unique "one LIVE per space")
LIVE ─end→ ENDING ─(provider terminated, Part E)→ ENDED
CREATED|PREPARING|WAITING ─cancel→ CANCELLED
PREPARING|LIVE|ENDING ─fail→ FAILED
```

ENDING immediately stops admission and new usable join credentials. ENDED is never set by
Parts A–D code: the `rtc.room_terminate_requested` outbox intent is consumed by Part E.

## Join pipeline

1. resolve conference + space → 2. ActorContext → 3. lifecycle (LIVE only) → 4. space ACTIVE →
5–6. access policy (owner / membership / invitation / passcode / org / domain / adapter) →
7. lock → 8. ban → 9. logical participant (one per user; guests get opaque `gst_…` identity and a
`pt_…` credential stored as SHA-256) → 10. role from authoritative sources → 11. waiting-room
policy → 12. COMMIT → 13. ParticipantSession (ADMITTED only, server media identity) →
14. RTC credential via `RtcProvider` after commit (re-checks LIVE; fails closed) → 15. response.

A WAITING participant never receives a credential (`rtcStatus = NOT_ISSUED_WAITING`). With
`FEATURE_RTC_PROVIDER_CONTROL_V2=false` business admission still commits and the response says
`PROVIDER_CONTROL_DISABLED` — no credential is fabricated.

## Database invariants

Unique meeting code / legacy id / occurrence key / active membership / registered participant per
conference / guest identity per conference / media identity per conference; one LIVE v2
conference per space; composite FK `participant_sessions(participant, conference)`; triggers:
active sessions require an ADMITTED participant, leaving ADMITTED terminates sessions, audit rows
are append-only; invitation `usage_count <= use_limit`.

## Feature flags

`FEATURE_MEETING_V2_ENABLED`, `FEATURE_WAITING_ROOM_V2_ENABLED`, `MEETING_V2_WRITE_ALLOWLIST`,
`FEATURE_MEETING_V2_SHADOW_READ`, and Part E+ placeholders (`FEATURE_RTC_PROVIDER_CONTROL_V2`,
`FEATURE_CONFERENCE_REALTIME_ENABLED`, `FEATURE_COLLABORATION_V2_ENABLED`,
`FEATURE_RECORDING_V2_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_HEALTHCARE_PROFILE_ENABLED`).
`FEATURE_LEGACY_MEETING_WRITE_DISABLED` is not honoured during A–D.

## Legacy compatibility

Legacy rows are backfilled idempotently (`migration/src/phase2_backfill.rs`). Legacy-origin
conferences remain legacy-managed (read-only in v2, refreshed from the legacy row) until
certification. v2 codes (`xxx-xxxx-xxx`) and legacy codes (8 uppercase) never collide; the
resolver accepts v2 id, v2 code, legacy id and legacy code.

## A–D closure additions

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
inactive appointments; roles are capped below OWNER. **Blocked dependency:** the authoritative
HealthNet appointment service is not in this repository; until `HEALTHNET_APPOINTMENT_AUTHZ_URL`
points at it, `UnconfiguredAppointmentAuthorization` denies all appointment joins.

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
