//! Conference lifecycle, scheduling and join/leave endpoints.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use super::{
    idempotency_key, views, V2Json, V2JsonDigest, V2OptionalJson, V2Path, IDEMPOTENT_REPLAYED,
};
use crate::{
    domain::TerminationReason,
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext, admission::RtcOutcome, conference::ConferenceOutcome,
        idempotency::IdempotencyRequest,
    },
    state::AppState,
};

fn action_response(o: ConferenceOutcome) -> Json<ConferenceActionResponse> {
    Json(ConferenceActionResponse {
        conference: views::conference(&o.conference, true),
        changed: o.changed,
        provider_enforcement: o.enforcement.into(),
    })
}

/// Create a one-time scheduled conference occurrence.
///
/// Adds a single (non-recurring) conference to an active meeting space in state CREATED, with
/// optional `scheduledStartAt` / `scheduledEndAt` (the end requires a start, must be after it and
/// at most 24 hours later; a start more than 5 minutes in the past is rejected). Requires a bearer
/// user token whose space role may schedule conferences. Send an `Idempotency-Key` header (1-128
/// visible ASCII characters, kept 24 hours per caller and space) to make retries safe: the same
/// key and body return the same conference (header `Idempotent-Replayed: true`); the same key
/// with a different body is IDEMPOTENCY_CONFLICT.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/conferences", tag = "Conferences (v2)", operation_id = "v2_create_conference",
    params(
        ("id" = Uuid, Path, description = "Meeting space id"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional retry key (1-128 visible ASCII characters)"),
    ),
    request_body = CreateConferenceRequest,
    responses(
        (status = 201, description = "Created (or replayed for the same Idempotency-Key)", body = ConferenceResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "IDEMPOTENCY_CONFLICT / CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    headers: HeaderMap,
    V2JsonDigest(req, digest): V2JsonDigest<CreateConferenceRequest, true>,
) -> ApiResult<Response> {
    let idempotency = match idempotency_key(&headers)? {
        Some(key) => {
            IdempotencyRequest::new(&actor, format!("v2_create_conference:{id}"), &key, digest)?
        }
        None => None,
    };
    let (c, replayed) = state
        .meeting
        .conferences
        .create_occurrence_idempotent(&actor, id, req, idempotency)
        .await?;
    let mut response = (StatusCode::CREATED, Json(views::conference(&c, true))).into_response();
    if replayed {
        response
            .headers_mut()
            .insert(IDEMPOTENT_REPLAYED, HeaderValue::from_static("true"));
    }
    Ok(response)
}

/// List occurrences of a meeting space (members only).
///
/// Returns the conferences of the meeting space (full view), ordered by scheduled start. Optional
/// `from` (inclusive) and `to` (exclusive) RFC 3339 timestamps filter on the scheduled start time.
/// Requires a bearer user token of the space owner or an active member.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/conferences", tag = "Conferences (v2)", operation_id = "v2_list_conferences",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("from" = Option<String>, Query, description = "RFC 3339"), ("to" = Option<String>, Query, description = "RFC 3339")),
    responses((status = 200, description = "Occurrences", body = ConferenceListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_conferences(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    query: Result<Query<ListConferencesQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<ConferenceListResponse>> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let list = state
        .meeting
        .conferences
        .list_for_space(&actor, id, q.from, q.to)
        .await?;
    Ok(Json(ConferenceListResponse {
        items: list.iter().map(|c| views::conference(c, true)).collect(),
    }))
}

/// Read a conference. Callers without authority get the SUMMARY view.
///
/// Works with or without authentication. The space owner, members and admitted participants get the
/// FULL view (schedule, lock, recording/transcription state, version); anyone else gets the SUMMARY
/// view with the id, meeting space id, status and — except for HEALTHCARE / HIGH_SECURITY /
/// healthcare-appointment meetings — the scheduled start and end (pre-join countdown).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}", tag = "Conferences (v2)", operation_id = "v2_get_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Conference", body = ConferenceResponse), (status = 404, description = "CONFERENCE_NOT_FOUND", body = ErrorEnvelope)),
    security((), ("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceResponse>> {
    let (c, space, full) = state.meeting.conferences.get(&actor, id).await?;
    Ok(Json(views::conference_in(&c, &space, full)))
}

/// CREATED → PREPARING → (provider room confirmed) → WAITING.
///
/// Prepares a CREATED conference: it moves to PREPARING, the media room is requested from the RTC
/// provider, and it becomes WAITING once the provider confirms the room. Requires
/// `conference.start`. Calling it on a PREPARING or WAITING conference is a no-op that may retry
/// provider confirmation; `providerEnforcement` is PENDING_PROVIDER_CONTROL while the room is not
/// yet confirmed.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/prepare", tag = "Conferences (v2)", operation_id = "v2_prepare_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn prepare_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state.meeting.conferences.prepare(&actor, id).await?;
    Ok(action_response(outcome))
}

/// Start a conference. Concurrent starts yield exactly one LIVE authority.
///
/// Moves a CREATED, PREPARING or WAITING conference to LIVE (a CREATED one is prepared first).
/// Requires `conference.start`, an active meeting space, and the current time within the start
/// window (from the space's early-start allowance before the scheduled start until its grace period
/// after the scheduled end). Starting a LIVE conference is a no-op (`changed: false`), and only one
/// conference per meeting space can be LIVE at a time.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/start", tag = "Conferences (v2)", operation_id = "v2_start_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn start_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state.meeting.conferences.start(&actor, id).await?;
    Ok(action_response(outcome))
}

/// Lock a live conference: no new non-exempt entrants.
///
/// While locked, new entrants whose role lacks `participant.admit` are refused with
/// CONFERENCE_LOCKED; admitted participants can still reconnect and those already waiting keep
/// waiting. Requires `conference.lock` and a LIVE conference (CONFERENCE_NOT_LIVE otherwise).
/// Locking an already locked conference is a no-op (`changed: false`).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/lock", tag = "Conferences (v2)", operation_id = "v2_lock_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn lock_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state.meeting.conferences.set_lock(&actor, id, true).await?;
    Ok(action_response(outcome))
}

/// Unlock a live conference.
///
/// Lifts the lock so new entrants can join again under the normal access and waiting-room rules.
/// Requires `conference.lock` and a LIVE conference (CONFERENCE_NOT_LIVE otherwise). Unlocking a
/// conference that is not locked is a no-op (`changed: false`).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/unlock", tag = "Conferences (v2)", operation_id = "v2_unlock_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn unlock_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state
        .meeting
        .conferences
        .set_lock(&actor, id, false)
        .await?;
    Ok(action_response(outcome))
}

/// LIVE → ENDING. Admission and new join credentials stop immediately; ENDED follows provider termination (Part E).
///
/// Ends a LIVE conference for everyone: active recordings are stopped and the media room is
/// terminated with the RTC provider. Requires `conference.end`; a conference that never went live
/// returns CONFERENCE_NOT_LIVE (cancel it instead). Repeating the call while ENDING or ENDED is a
/// no-op (`changed: false`); while ENDING it retries provider termination. `providerEnforcement`
/// is ENFORCED once the conference is ENDED.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/end", tag = "Conferences (v2)", operation_id = "v2_end_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn end_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state
        .meeting
        .conferences
        .end(&actor, id, TerminationReason::HostEnded)
        .await?;
    Ok(action_response(outcome))
}

/// Cancel an unstarted conference (single-occurrence exception for recurring series).
///
/// Moves a CREATED, PREPARING or WAITING conference to CANCELLED and releases any media room
/// already prepared. Requires `conference.start`. For an occurrence of a recurring series only that
/// occurrence is cancelled. Cancelling an already cancelled conference is a no-op
/// (`changed: false`); live or finished conferences return CONFERENCE_STATE_CONFLICT.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/cancel", tag = "Conferences (v2)", operation_id = "v2_cancel_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Applied (or idempotent no-op)", body = ConferenceActionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn cancel_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let outcome = state.meeting.conferences.cancel(&actor, id).await?;
    Ok(action_response(outcome))
}

/// Reschedule one unstarted occurrence (series untouched).
///
/// Sets a new `scheduledStartAt` (required; more than 5 minutes in the past is rejected) for a
/// conference still in state CREATED; any other state returns CONFERENCE_STATE_CONFLICT. An omitted
/// `scheduledEndAt` keeps the occurrence's duration (the end is never cleared); an explicit one must
/// be after the start and at most 24 hours later. An optional `version` guards against concurrent
/// changes (MEETING_VERSION_CONFLICT). Requires a bearer user token whose role holds
/// `conference.start`. For an occurrence of a recurring series only that occurrence moves.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/reschedule", tag = "Conferences (v2)", operation_id = "v2_reschedule_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = RescheduleConferenceRequest,
    responses(
        (status = 200, description = "Rescheduled", body = ConferenceActionResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT / MEETING_VERSION_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn reschedule_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<RescheduleConferenceRequest>,
) -> ApiResult<Json<ConferenceActionResponse>> {
    let o = state
        .meeting
        .conferences
        .reschedule(&actor, id, req)
        .await?;
    Ok(action_response(o))
}

/// Join (or reconnect to) a conference.
///
/// Identity, role and admission are resolved server-side. A WAITING participant never receives
/// an RTC credential.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/join", tag = "Conferences (v2)", operation_id = "v2_join_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = JoinConferenceRequest,
    responses(
        (status = 200, description = "Joined or waiting", body = JoinConferenceResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED / MEETING_PASSCODE_REQUIRED / MEETING_PASSCODE_INVALID", body = ErrorEnvelope),
        (status = 403, description = "MEETING_ACCESS_DENIED / CONFERENCE_LOCKED / PARTICIPANT_BANNED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE / MEETING_CAPACITY_REACHED", body = ErrorEnvelope),
        (status = 410, description = "INVITATION_INVALID / INVITATION_EXPIRED", body = ErrorEnvelope),
        (status = 503, description = "RTC_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security((), ("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn join_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<JoinConferenceRequest>,
) -> ApiResult<Json<JoinConferenceResponse>> {
    let o = state.meeting.admission.join(&actor, id, req).await?;
    let (rtc, rtc_status) = match o.rtc {
        RtcOutcome::Issued(c) => (
            Some(RtcCredentialResponse {
                provider: c.provider.to_string(),
                url: c.url,
                credential: c.credential,
                expires_at: c.expires_at,
            }),
            RtcStatus::Issued,
        ),
        RtcOutcome::NotIssuedWaiting => (None, RtcStatus::NotIssuedWaiting),
        RtcOutcome::ProviderControlDisabled => (None, RtcStatus::ProviderControlDisabled),
    };
    Ok(Json(JoinConferenceResponse {
        conference: JoinConferenceSummary {
            id: o.conference.id,
            status: o.conference.status,
            meeting_space_id: o.conference.meeting_space_id,
            is_locked: o.conference.is_locked,
        },
        participant: views::participant(&o.participant),
        participant_session: o.session.as_ref().map(|s| ParticipantSessionResponse {
            id: s.id,
            expires_at: s.expires_at,
        }),
        participant_token: o.participant_token,
        rtc,
        rtc_status,
        permissions: o.permissions.into(),
        policy: JoinPolicy {
            recording_active: o.recording_active,
            transcription_active: o.transcription_active,
        },
        media_context: (&o.media_context).into(),
        realtime: JoinRealtimeInfo::for_conference(
            o.conference.id,
            state.meeting.core.flags.conference_realtime_enabled,
        ),
    }))
}

/// Leave: close the caller's own session(s). Admission is unchanged.
///
/// Disconnects all of the caller's active sessions in the conference, or only the one given in
/// `participantSessionId`, and returns how many were closed. The caller keeps their admission state
/// and can join again. Returns PARTICIPANT_NOT_FOUND if the caller has not joined this conference.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/leave", tag = "Conferences (v2)", operation_id = "v2_leave_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = LeaveConferenceRequest,
    responses((status = 200, description = "Left", body = LeaveConferenceResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn leave_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<LeaveConferenceRequest>,
) -> ApiResult<Json<LeaveConferenceResponse>> {
    let closed = state
        .meeting
        .admission
        .leave(&actor, id, req.participant_session_id)
        .await?;
    Ok(Json(LeaveConferenceResponse {
        closed_sessions: closed,
    }))
}
