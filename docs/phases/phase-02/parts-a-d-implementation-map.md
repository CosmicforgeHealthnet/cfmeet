# Phase 2 — Parts A–D: Current → Target Implementation Map

Specification: *CF Meet Phase 2 Backend Implementation Specification v1.1 (03 Oct 2026)*.
Scope: `services/meeting-service/` only. No `apps/web` changes.

## 1. Discovery (baseline `9bcff01`)

| Concern | Current location | Finding |
|---|---|---|
| Workspace | `services/meeting-service/Cargo.toml` (`backend` + `migration`) | Axum 0.7, SeaORM 1.1, utoipa 4, chrono-tz, bcrypt, sha2, livekit-api 0.4, deadpool-redis |
| Migrations | `migration/src/m2026*.rs` | Postgres enum types, `timestamp` (naive UTC). Run with `cargo run -p migration -- up` |
| DB/state | `src/state.rs` `AppState` | Concrete `Arc` services, built in `main.rs` |
| Auth | `src/middleware/auth.rs` | JWT (`Claims{sub, role}`), Redis blacklist, `AuthContext`. Guest JWT uses `sub = legacy participant id` |
| Redis | `src/services/redis.rs`, `rate_limiter.rs`, `otp.rs`, `token_blacklist.rs` | OTP, rate limits, blacklist, email stream |
| Errors | `src/error.rs` `ApiError` | `{success:false,error:{code,message}}`; DB error text was leaked to clients |
| DTO | `src/dto/*` | snake_case, `ApiResponse<T>` envelope |
| OpenAPI | `src/swagger/mod.rs` | utoipa `ApiDoc`, served at `/api-docs/openapi.json`; no export |
| Workers | `src/workers/meeting_auto_{start,end}.rs` | Poll legacy `meetings`; not multi-instance safe |
| LiveKit | `src/services/livekit.rs` | Token generation called directly from handlers |
| Legacy meeting | `src/models/meetings.rs`, `src/handlers/meetings.rs` | One row = definition + live call. Join trusts body `user_id` |
| Legacy participant | `src/models/participants.rs`, `src/handlers/participants.rs` | One row per join; guests matched by display name; admit/deny not scoped to meeting |
| Chat | `src/handlers/chat.rs` | Legacy-meeting scoped; untouched in A–D |
| Feature flags | none | Added `src/config/features.rs` |
| Tests | 9 unit tests in `utils` | No DB/API test harness |

## 2. Current → Target

| Legacy | Target |
|---|---|
| `meetings` table/model | Retained. `meeting_spaces` is the v2 persistent aggregate (`legacy_meeting_id` mapping, code preserved) |
| `participants` table/model | Retained. `conference_participants` (logical person) + `participant_sessions` (device) |
| `handlers/meetings.rs` | Legacy transport kept. v2 thin handlers in `handlers/v2/*` → `MeetingSpaceService`, `ConferenceService` |
| `handlers/participants.rs` | Legacy kept (admit/deny now meeting-scoped). v2 → `AdmissionService` |
| Direct LiveKit usage | `RtcProvider` trait (`services/rtc_provider.rs`), `LiveKitProvider` wraps the existing token code |
| Auto start/end workers | Legacy workers retained for legacy rows. `workers/conference_scheduler.rs` (lease + idempotent) for v2 |
| Chat | Unchanged; future `MeetingMessagingService` |

## 3. Module plan (implemented)

```
src/domain/            enums + value types (roles, permissions, policies, states)
src/models/            meeting_spaces, conference_sessions, meeting_recurrence_rules, meeting_members,
                       meeting_invitations, conference_participants, participant_sessions, conference_bans,
                       meeting_consents, meeting_artifacts, breakout_rooms, breakout_assignments, meeting_polls,
                       meeting_poll_responses, meeting_questions, meeting_question_votes,
                       conference_audit_events, outbox_events
src/services/          actor (ActorContext), meeting_space, conference, recurrence, meeting_access,
                       meeting_permissions, admission, membership, invitations, meeting_events (audit+outbox),
                       rtc_provider, compat (resolver, shadow comparison, backfill), secrets
src/handlers/v2/       thin transport
src/routes/v2.rs       /api/v2
src/workers/conference_scheduler.rs
migration/src/m20261003_000001_phase2_meeting_domain.rs   additive schema
migration/src/m20261003_000002_phase2_legacy_backfill.rs  idempotent backfill
migration/src/phase2_backfill.rs                          shared backfill + evidence report
```

## 4. Legacy → v2 translation

| Legacy | v2 |
|---|---|
| `meetings.id` | `meeting_spaces.legacy_meeting_id` (unique) |
| `meetings.meeting_identifier` | `meeting_spaces.meeting_code` (legacy 8-char uppercase; v2 codes use `xxx-xxxx-xxx` lowercase, so formats never collide) |
| `host_id` | `owner_user_id` |
| `is_private=false` | `PUBLIC` + waiting room `OFF` |
| `is_private=true` | `AUTHENTICATED` + waiting room `EVERYONE` (legacy behaviour) |
| status scheduled/ongoing/ended/cancelled | conference `CREATED`/`LIVE`/`ENDED`/`CANCELLED` (`origin = LEGACY`, read-only in v2) |
| participant (registered user, n rows) | 1 `conference_participant` + n historical `participant_sessions` |
| participant (guest row) | 1 `GUEST` participant per row (display name is never identity) |
| participant status waiting/joined/kicked | `WAITING`/`ADMITTED`/`REMOVED` |
| participant `join_time`/`leave_time` | session `established_at`/`disconnected_at`; no session for never-admitted rows |
