//! Part F transport: breakout rooms (owned by the parent conference).

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Breakout rooms of a conference (members listed for breakout managers only).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/breakouts", tag = "Breakouts (v2)", operation_id = "v2_list_breakouts",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Breakouts", body = BreakoutListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<BreakoutListResponse>> {
    let (breakouts, me) = state.meeting.breakouts.list(&actor, id).await?;
    Ok(Json(BreakoutListResponse {
        breakouts,
        my_context: me.as_ref().map(Into::into),
    }))
}

/// Create named or numbered breakout rooms.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts", tag = "Breakouts (v2)", operation_id = "v2_create_breakouts",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = CreateBreakoutsRequest,
    responses(
        (status = 201, description = "Created", body = BreakoutListCreatedResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn create(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<CreateBreakoutsRequest>,
) -> ApiResult<(StatusCode, Json<BreakoutListCreatedResponse>)> {
    let breakouts = state.meeting.breakouts.create(&actor, id, req).await?;
    Ok((
        StatusCode::CREATED,
        Json(BreakoutListCreatedResponse { breakouts }),
    ))
}

/// Assign participants (MANUAL, RANDOM or BALANCED).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/assign", tag = "Breakouts (v2)", operation_id = "v2_assign_breakouts",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = AssignBreakoutsRequest,
    responses(
        (status = 200, description = "Assignments", body = AssignmentResult),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "BREAKOUT_NOT_FOUND / PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "BREAKOUT_CLOSED / BREAKOUT_ASSIGNMENT_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn assign(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<AssignBreakoutsRequest>,
) -> ApiResult<Json<AssignmentResult>> {
    Ok(Json(state.meeting.breakouts.assign(&actor, id, req).await?))
}

/// Move a participant: main → breakout, breakout → breakout, breakout → main.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/move", tag = "Breakouts (v2)", operation_id = "v2_move_participant",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = MoveParticipantRequest,
    responses(
        (status = 200, description = "Moved (idempotent)", body = MediaContextResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "BREAKOUT_NOT_FOUND / PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "BREAKOUT_CLOSED / BREAKOUT_ASSIGNMENT_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn move_participant(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<MoveParticipantRequest>,
) -> ApiResult<Json<MediaContextResponse>> {
    let pid = req.participant_id;
    let (ctx, changed, enforcement) = state
        .meeting
        .breakouts
        .move_participant(&actor, id, req)
        .await?;
    Ok(Json(MediaContextResponse {
        participant_id: Some(pid),
        media_context: (&ctx).into(),
        changed,
        provider_enforcement: enforcement.into(),
    }))
}

/// Add / remove participants of one breakout room.
#[utoipa::path(
    patch, path = "/api/v2/conferences/{id}/breakouts/{breakoutId}/assignments", tag = "Breakouts (v2)", operation_id = "v2_patch_breakout_assignments",
    params(("id" = Uuid, Path, description = "Conference id"), ("breakoutId" = Uuid, Path, description = "Breakout id")),
    request_body = RoomAssignmentsPatch,
    responses((status = 200, description = "Room", body = BreakoutView), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "BREAKOUT_NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "BREAKOUT_CLOSED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn patch_assignments(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, bid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<RoomAssignmentsPatch>,
) -> ApiResult<Json<BreakoutView>> {
    Ok(Json(
        state
            .meeting
            .breakouts
            .patch_room(&actor, id, bid, req)
            .await?,
    ))
}

/// Open a breakout room (provider room created, assigned participants rerouted).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/{breakoutId}/open", tag = "Breakouts (v2)", operation_id = "v2_open_breakout",
    params(("id" = Uuid, Path, description = "Conference id"), ("breakoutId" = Uuid, Path, description = "Breakout id")),
    responses((status = 200, description = "Opened (idempotent)", body = BreakoutResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "BREAKOUT_NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "BREAKOUT_CLOSED / CONFERENCE_NOT_LIVE", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn open(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, bid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<BreakoutResponse>> {
    let (breakout, changed, enforcement) = state.meeting.breakouts.open(&actor, id, bid).await?;
    Ok(Json(BreakoutResponse {
        breakout,
        changed,
        provider_enforcement: enforcement.into(),
    }))
}

/// Host / co-host enters a breakout room (privileged visit, not an assignment).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/{breakoutId}/enter", tag = "Breakouts (v2)", operation_id = "v2_enter_breakout",
    params(("id" = Uuid, Path, description = "Conference id"), ("breakoutId" = Uuid, Path, description = "Breakout id")),
    responses((status = 200, description = "Visiting", body = MediaContextResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "BREAKOUT_NOT_FOUND", body = ErrorEnvelope), (status = 409, description = "BREAKOUT_CLOSED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn enter(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, bid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<MediaContextResponse>> {
    let (ctx, enforcement) = state.meeting.breakouts.visit(&actor, id, Some(bid)).await?;
    Ok(Json(MediaContextResponse {
        participant_id: None,
        media_context: (&ctx).into(),
        changed: true,
        provider_enforcement: enforcement.into(),
    }))
}

/// Leave a breakout visit.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/leave", tag = "Breakouts (v2)", operation_id = "v2_leave_breakout_visit",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Back in the assigned room or main", body = MediaContextResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn leave(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<MediaContextResponse>> {
    let (ctx, enforcement) = state.meeting.breakouts.visit(&actor, id, None).await?;
    Ok(Json(MediaContextResponse {
        participant_id: None,
        media_context: (&ctx).into(),
        changed: true,
        provider_enforcement: enforcement.into(),
    }))
}

/// Announcement to every open breakout room.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/announcement", tag = "Breakouts (v2)", operation_id = "v2_breakout_announcement_all",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = BreakoutAnnouncementRequest,
    responses((status = 200, description = "Delivered", body = AnnouncementResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 409, description = "BREAKOUT_CLOSED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn announce_all(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<BreakoutAnnouncementRequest>,
) -> ApiResult<Json<AnnouncementResponse>> {
    Ok(Json(AnnouncementResponse {
        rooms: state
            .meeting
            .breakouts
            .announce(&actor, id, None, req)
            .await?,
    }))
}

/// Announcement to one open breakout room.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/{breakoutId}/announcement", tag = "Breakouts (v2)", operation_id = "v2_breakout_announcement",
    params(("id" = Uuid, Path, description = "Conference id"), ("breakoutId" = Uuid, Path, description = "Breakout id")),
    request_body = BreakoutAnnouncementRequest,
    responses((status = 200, description = "Delivered", body = AnnouncementResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "BREAKOUT_NOT_FOUND", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn announce(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, bid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<BreakoutAnnouncementRequest>,
) -> ApiResult<Json<AnnouncementResponse>> {
    Ok(Json(AnnouncementResponse {
        rooms: state
            .meeting
            .breakouts
            .announce(&actor, id, Some(bid), req)
            .await?,
    }))
}

/// Close one breakout (countdown persisted; 0 = immediate).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/{breakoutId}/close", tag = "Breakouts (v2)", operation_id = "v2_close_breakout",
    params(("id" = Uuid, Path, description = "Conference id"), ("breakoutId" = Uuid, Path, description = "Breakout id")),
    request_body = CloseBreakoutRequest,
    responses((status = 200, description = "Closing / closed", body = CloseBreakoutsResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "BREAKOUT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn close(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, bid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<CloseBreakoutRequest>,
) -> ApiResult<Json<CloseBreakoutsResponse>> {
    let (breakouts, enforcement) = state
        .meeting
        .breakouts
        .close(&actor, id, Some(bid), req)
        .await?;
    Ok(Json(CloseBreakoutsResponse {
        breakouts,
        provider_enforcement: enforcement.into(),
    }))
}

/// Close every breakout of the conference.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/breakouts/close-all", tag = "Breakouts (v2)", operation_id = "v2_close_all_breakouts",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = CloseBreakoutRequest,
    responses((status = 200, description = "Closing / closed", body = CloseBreakoutsResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn close_all(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<CloseBreakoutRequest>,
) -> ApiResult<Json<CloseBreakoutsResponse>> {
    let (breakouts, enforcement) = state.meeting.breakouts.close(&actor, id, None, req).await?;
    Ok(Json(CloseBreakoutsResponse {
        breakouts,
        provider_enforcement: enforcement.into(),
    }))
}
