//! Conference lifecycle, scheduling and join/leave endpoints.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use super::{views, V2Json, V2OptionalJson, V2Path};
use crate::{
    domain::TerminationReason,
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, admission::RtcOutcome, conference::ConferenceOutcome},
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
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/conferences", tag = "Conferences (v2)", operation_id = "v2_create_conference",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = CreateConferenceRequest,
    responses((status = 201, description = "Created", body = ConferenceResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn create_conference(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<CreateConferenceRequest>,
) -> ApiResult<(StatusCode, Json<ConferenceResponse>)> {
    let c = state
        .meeting
        .conferences
        .create_occurrence(&actor, id, req)
        .await?;
    Ok((StatusCode::CREATED, Json(views::conference(&c, true))))
}

/// List occurrences of a meeting space (members only).
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
    let (c, _space, full) = state.meeting.conferences.get(&actor, id).await?;
    Ok(Json(views::conference(&c, full)))
}

/// CREATED → PREPARING → (provider room confirmed) → WAITING.
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
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/reschedule", tag = "Conferences (v2)", operation_id = "v2_reschedule_conference",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = RescheduleConferenceRequest,
    responses((status = 200, description = "Rescheduled", body = ConferenceActionResponse), (status = 409, description = "CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope)),
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

/// Join (or reconnect to) a conference. Identity, role and admission are resolved
/// server-side. A WAITING participant never receives an RTC credential.
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
    }))
}

/// Leave: close the caller's own session(s). Admission is unchanged.
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
