//! Part F transport: breakout rooms (owned by the parent conference).

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Breakout rooms of a conference (members listed for breakout managers only).
///
/// Returns the conference's breakout rooms in display order with status and participant count.
/// Per-room participant ids are included only for callers who may assign participants or enter any
/// room. `myContext` describes the caller's current media context (main room or a breakout) when
/// the caller is an admitted participant.
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
///
/// Creates rooms from either a list of unique `names` or a `count` of automatically numbered rooms
/// ("Room N"), optionally with a per-room `capacity` (1–1000) and recording policy. Rooms start in
/// CREATED state; open them with the `open` endpoint. Requires the breakout-create permission
/// (owner, host or co-host by default) and a conference that has not finished; the meeting policy
/// caps the number of active rooms.
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
///
/// MANUAL (the default when `assignments` is sent) applies an explicit participant-to-room list;
/// RANDOM and BALANCED (the default otherwise) distribute admitted participants over the target
/// rooms, by default skipping hosts and anyone already assigned, and return those who could not be
/// placed. Requires the breakout-assign permission. Closing or closed rooms cannot receive
/// assignments.
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
///
/// Moves one admitted participant into a breakout room, between rooms, or back to the main room
/// (`toBreakoutId` null). Requires the breakout-move permission and a live conference; the target
/// room must not be closing, closed or full. Moving a participant to where they already are is a
/// no-op (`changed` is false).
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
///
/// Assigns the participants in `add` to this room and returns those in `remove` (if assigned here)
/// to the main room. Only admitted participants can be added, and adding requires the room to be
/// neither closing nor closed and to have capacity. Requires the breakout-assign permission.
/// Returns the updated room.
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
///
/// Opens a CREATED room so its assigned participants are moved into it. Requires the
/// breakout-create permission and a live conference. Opening a room that is already open returns it
/// with `changed` false; a closed room cannot be reopened.
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
///
/// Moves the caller, who must be an admitted participant with the enter-any-room permission, into
/// an open breakout room as a visitor. A visit changes no assignments and does not count toward
/// capacity or balancing. Requires a live conference; end the visit with the `leave` endpoint.
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
///
/// Ends the caller's visit to a breakout room and returns them to their own open breakout
/// assignment, or to the main room if they have none. Requires the enter-any-room permission and a
/// live conference. Calling it when not visiting returns the current media context unchanged.
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
///
/// Sends a host announcement (1–1000 characters) to participants of every open or closing breakout
/// room and returns how many rooms received it. Requires the breakout-announce permission and a
/// live conference, and is rate limited per sender. Fails with `BREAKOUT_CLOSED` when no room is
/// open.
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
///
/// Sends a host announcement (1–1000 characters) to the participants of this breakout room, which
/// must be open or closing. Requires the breakout-announce permission and a live conference, and is
/// rate limited per sender.
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
///
/// An open room enters CLOSING for `countdownSeconds` (default 60, max 600), after which its
/// participants return to the main room; 0 closes it immediately, and a room that was never opened
/// closes at once. Requires the breakout-close permission. Repeating the call does not restart a
/// running countdown.
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
///
/// Closes every breakout room that is not already closed, with the same countdown rules as closing
/// a single room (`countdownSeconds`, default 60, max 600; 0 = immediate). Participants return to
/// the main room as each room closes. Requires the breakout-close permission.
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
