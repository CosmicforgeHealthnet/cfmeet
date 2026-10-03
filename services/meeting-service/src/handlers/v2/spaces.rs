//! Meeting space, instant meeting, resolver and recurrence endpoints.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{views, V2Json, V2OptionalJson, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Create a meeting space. The owner is always the authenticated caller.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces", tag = "Meeting Spaces (v2)", operation_id = "v2_create_meeting_space",
    request_body = CreateMeetingSpaceRequest,
    responses(
        (status = 201, description = "Created", body = MeetingSpaceResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_meeting_space(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<CreateMeetingSpaceRequest>,
) -> ApiResult<(StatusCode, Json<MeetingSpaceResponse>)> {
    let space = state.meeting.spaces.create(&actor, req).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok((StatusCode::CREATED, Json(views::space(&view))))
}

/// List meeting spaces the caller owns or is a member of.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces", tag = "Meeting Spaces (v2)", operation_id = "v2_list_meeting_spaces",
    responses(
        (status = 200, description = "Spaces", body = MeetingSpaceListResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_meeting_spaces(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<MeetingSpaceListResponse>> {
    let items = state.meeting.spaces.list_for_actor(&actor).await?;
    Ok(Json(MeetingSpaceListResponse {
        items: items.iter().map(views::space).collect(),
    }))
}

/// Read a meeting space by v2 id, v2 meeting code, legacy meeting id or legacy code.
/// Callers without management rights receive the SUMMARY view; resolution never
/// implies authorization.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}", tag = "Meeting Spaces (v2)", operation_id = "v2_get_meeting_space",
    params(("id" = String, Path, description = "Space UUID, meeting code, legacy meeting UUID or legacy code")),
    responses(
        (status = 200, description = "Space", body = MeetingSpaceResponse),
        (status = 404, description = "MEETING_NOT_FOUND", body = ErrorEnvelope),
    ),
    security((), ("bearer_auth" = []))
)]
pub async fn get_meeting_space(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id_or_code): V2Path<String>,
) -> ApiResult<Json<MeetingSpaceResponse>> {
    let resolution = state.meeting.compat.resolve(&id_or_code).await?;
    let view = state
        .meeting
        .spaces
        .view_of(&actor, resolution.space)
        .await?;
    Ok(Json(views::space(&view)))
}

/// Resolve any supported meeting identifier to the canonical space and conference.
#[utoipa::path(
    get, path = "/api/v2/resolve/{identifier}", tag = "Meeting Spaces (v2)", operation_id = "v2_resolve_identifier",
    params(("identifier" = String, Path, description = "Space UUID, meeting code, legacy meeting UUID or legacy code")),
    responses(
        (status = 200, description = "Resolved", body = ResolveResponse),
        (status = 404, description = "MEETING_NOT_FOUND", body = ErrorEnvelope),
    ),
    security((), ("bearer_auth" = []))
)]
pub async fn resolve_identifier(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(identifier): V2Path<String>,
) -> ApiResult<Json<ResolveResponse>> {
    let resolution = state.meeting.compat.resolve(&identifier).await?;
    let conference = state
        .meeting
        .compat
        .canonical_conference(resolution.space.id)
        .await?;
    let view = state
        .meeting
        .spaces
        .view_of(&actor, resolution.space)
        .await?;
    let full = view.full;
    Ok(Json(ResolveResponse {
        meeting_space: views::space(&view),
        conference: conference.as_ref().map(|c| views::conference(c, full)),
        matched_by: resolution.matched_by.as_str().to_string(),
    }))
}

/// Update a meeting space (optimistic concurrency via `version`).
#[utoipa::path(
    patch, path = "/api/v2/meeting-spaces/{id}", tag = "Meeting Spaces (v2)", operation_id = "v2_update_meeting_space",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = UpdateMeetingSpaceRequest,
    responses(
        (status = 200, description = "Updated", body = MeetingSpaceResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "MEETING_VERSION_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_meeting_space(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<UpdateMeetingSpaceRequest>,
) -> ApiResult<Json<MeetingSpaceResponse>> {
    let space = state.meeting.spaces.update(&actor, id, req).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok(Json(views::space(&view)))
}

/// Soft-archive a meeting space (owner only). History is preserved.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/archive", tag = "Meeting Spaces (v2)", operation_id = "v2_archive_meeting_space",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body(content = VersionedRequest, description = "Optional expected version"),
    responses(
        (status = 200, description = "Archived", body = MeetingSpaceResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn archive_meeting_space(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<VersionedRequest>,
) -> ApiResult<Json<MeetingSpaceResponse>> {
    let space = state
        .meeting
        .spaces
        .archive(&actor, id, req.version)
        .await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok(Json(views::space(&view)))
}

/// Duplicate reusable configuration into a new space owned by the caller.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/duplicate", tag = "Meeting Spaces (v2)", operation_id = "v2_duplicate_meeting_space",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = DuplicateMeetingSpaceRequest,
    responses(
        (status = 201, description = "Duplicated", body = MeetingSpaceResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn duplicate_meeting_space(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<DuplicateMeetingSpaceRequest>,
) -> ApiResult<(StatusCode, Json<MeetingSpaceResponse>)> {
    let space = state.meeting.spaces.duplicate(&actor, id, req).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok((StatusCode::CREATED, Json(views::space(&view))))
}

/// Transfer ownership (dedicated, protected operation).
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/owner-transfer", tag = "Meeting Spaces (v2)", operation_id = "v2_transfer_owner",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = OwnerTransferRequest,
    responses(
        (status = 200, description = "Transferred", body = MeetingSpaceResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "MEETING_VERSION_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn transfer_owner(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<OwnerTransferRequest>,
) -> ApiResult<Json<MeetingSpaceResponse>> {
    let space = state.meeting.spaces.transfer_owner(&actor, id, req).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok(Json(views::space(&view)))
}

/// Instant meeting: creates a meeting space and a LIVE conference atomically.
#[utoipa::path(
    post, path = "/api/v2/instant-meetings", tag = "Meeting Spaces (v2)", operation_id = "v2_create_instant_meeting",
    request_body = InstantMeetingRequest,
    responses(
        (status = 201, description = "Created", body = InstantMeetingResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_instant_meeting(
    State(state): State<AppState>,
    actor: ActorContext,
    V2OptionalJson(req): V2OptionalJson<InstantMeetingRequest>,
) -> ApiResult<(StatusCode, Json<InstantMeetingResponse>)> {
    let (space, conference) = state.meeting.spaces.create_instant(&actor, req).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok((
        StatusCode::CREATED,
        Json(InstantMeetingResponse {
            meeting_space: views::space(&view),
            conference: views::conference(&conference, true),
        }),
    ))
}

/// Create or replace the space's recurrence (RFC 5545 RRULE in the space timezone).
#[utoipa::path(
    put, path = "/api/v2/meeting-spaces/{id}/recurrence", tag = "Scheduling (v2)", operation_id = "v2_set_recurrence",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = SetRecurrenceRequest,
    responses(
        (status = 200, description = "Recurrence set; near-future occurrences materialized", body = RecurrenceResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_recurrence(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<SetRecurrenceRequest>,
) -> ApiResult<Json<RecurrenceResponse>> {
    let rule = state.meeting.recurrence.set(&actor, id, req).await?;
    Ok(Json(views::recurrence(&rule)))
}

/// Read the active recurrence rule.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/recurrence", tag = "Scheduling (v2)", operation_id = "v2_get_recurrence",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses(
        (status = 200, description = "Active rule", body = RecurrenceResponse),
        (status = 404, description = "No active recurrence", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_recurrence(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<RecurrenceResponse>> {
    let rule = state
        .meeting
        .recurrence
        .get(&actor, id)
        .await?
        .ok_or_else(|| {
            crate::error::ApiError::domain(
                crate::error::ErrorCode::MeetingNotFound,
                "No active recurrence",
            )
        })?;
    Ok(Json(views::recurrence(&rule)))
}

/// End the recurring series (past and exception occurrences are preserved).
#[utoipa::path(
    delete, path = "/api/v2/meeting-spaces/{id}/recurrence", tag = "Scheduling (v2)", operation_id = "v2_end_recurrence",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses((status = 204, description = "Series ended"), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn end_recurrence(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<StatusCode> {
    state.meeting.recurrence.end(&actor, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
