//! Meeting space, instant meeting, resolver and recurrence endpoints.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{views, V2Json, V2OptionalJson, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Create a meeting space. The owner is always the authenticated caller.
///
/// Creates an ACTIVE meeting space with a newly generated meeting code; any owner sent by the
/// client is ignored. Requires a signed-in user with v2 meetings enabled for the account
/// (FEATURE_DISABLED otherwise). An `organizationId` is accepted only if the caller is a verified
/// member of that organization, and an optional passcode is validated and stored only as a hash. No
/// conference is created; schedule one separately or use `POST /api/v2/instant-meetings`.
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
///
/// Returns every non-deleted meeting space the signed-in caller owns or is an active member of,
/// newest first; archived spaces are included. Each item carries the caller's role and its
/// canonical conference (the LIVE one, else the next upcoming one, else the latest). Owners, hosts
/// and co-hosts get the full view, other members the summary view. Not paginated.
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
///
/// Callers without management rights receive the SUMMARY view; resolution never implies
/// authorization.
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
///
/// Accepts a v2 space id, v2 meeting code, legacy meeting id or legacy meeting code and returns the
/// matching space, its canonical conference (the LIVE one, else the next upcoming one, else the
/// latest) and `matchedBy` (V2_ID, V2_CODE, LEGACY_ID or LEGACY_CODE). Authentication is optional.
/// Resolving does not grant access: callers who cannot manage the space get the summary view only.
/// Unknown identifiers return MEETING_NOT_FOUND.
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
///
/// Partially updates the space's settings and policies; omitted fields are left unchanged. Requires
/// the owner or a host, v2 meetings enabled for the account, and the space's current `version` (a
/// stale version returns MEETING_VERSION_CONFLICT). Archived spaces cannot be modified. A passcode
/// can be set or cleared with `clearPasscode`; changing the timezone re-anchors an active
/// recurrence to the new timezone at the same wall-clock time. Ownership cannot be changed here;
/// use the owner-transfer endpoint.
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
///
/// Archives the space: it can no longer be modified, its recurring series is ended and unstarted
/// future occurrences are cancelled, while past conferences, participants and audit history are
/// kept. Only the owner (or a platform administrator) may archive. The optional body `version` is
/// checked against the current version (MEETING_VERSION_CONFLICT on mismatch). Archiving an
/// already-archived space returns it unchanged.
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
///
/// Creates a new ACTIVE space with a new meeting code, owned by the caller, copying the source
/// space's settings and policies, including its passcode. Conferences, members, invitations,
/// history and recordings are not copied. Requires the owner or a host of the source space and v2
/// meetings enabled for the account. The title defaults to "Copy of " plus the source title; the
/// organization is dropped if the caller is not a verified member of it.
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
///
/// Makes another active registered (non-guest) user the owner of an active space. Only the current
/// owner (or a platform administrator) may transfer, and the request must carry the space's current
/// `version`. The new owner's existing membership is replaced by ownership. The previous owner
/// stays on as a HOST member unless `retainPreviousOwner` is false.
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
///
/// Creates a meeting space owned by the caller and a conference that is LIVE immediately, in one
/// step, and returns both. The body is optional: the title defaults to "Instant meeting" and the
/// access policy to PUBLIC; timezone, waiting-room policy and passcode may be set. Requires a
/// signed-in user with v2 meetings enabled for the account. The conference's media room is prepared
/// right away on a best-effort basis.
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
///
/// Sets the space's recurring schedule from an RFC 5545 RRULE, a first start time (wall-clock time
/// in the space's timezone) and a duration of 1–1440 minutes. An existing active rule is ended and
/// its unstarted future occurrences are removed or cancelled before the new rule takes effect;
/// occurrences within the scheduling horizon are created right away. Requires permission to
/// schedule conferences (owner and host; co-host by default) and v2 meetings enabled for the
/// account. Archived spaces cannot recur.
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
///
/// Returns the space's active recurrence rule (RRULE, timezone, start and duration), or 404 if the
/// space has no active recurrence. The caller must be a signed-in owner or member of the space.
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
///
/// Ends the space's active recurrence: future unstarted occurrences of the series are removed or
/// cancelled, while started or past occurrences and recorded exceptions are kept. Requires
/// permission to schedule conferences (owner and host; co-host by default). Succeeds with 204 even
/// when there is no active recurrence.
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

fn exception_view(
    e: &crate::models::meeting_recurrence_exceptions::Model,
    timezone: String,
    applied: Option<Uuid>,
) -> RecurrenceExceptionResponse {
    RecurrenceExceptionResponse {
        id: e.id,
        recurrence_rule_id: e.recurrence_rule_id,
        original_start_local: e.original_start_local,
        timezone,
        kind: e.kind,
        new_start_at: e.new_start_at,
        new_end_at: e.new_end_at,
        version: e.version,
        applied_to_conference_id: applied,
    }
}

/// Cancel or reschedule ONE future occurrence of the series.
///
/// Works whether or not the occurrence has been materialized yet. Idempotent; applied by the
/// scheduler when the occurrence is generated (or immediately if it already exists).
#[utoipa::path(
    put, path = "/api/v2/meeting-spaces/{id}/recurrence/exceptions", tag = "Scheduling (v2)", operation_id = "v2_set_recurrence_exception",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = RecurrenceExceptionRequest,
    responses(
        (status = 200, description = "Exception recorded", body = RecurrenceExceptionResponse),
        (status = 400, description = "VALIDATION_ERROR (not an occurrence, past, bad window)", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT (occurrence already started)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_recurrence_exception(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<RecurrenceExceptionRequest>,
) -> ApiResult<Json<RecurrenceExceptionResponse>> {
    let (e, applied) = state
        .meeting
        .recurrence
        .set_exception(&actor, id, req)
        .await?;
    let tz = state
        .meeting
        .recurrence
        .get(&actor, id)
        .await?
        .map(|r| r.timezone)
        .unwrap_or_default();
    Ok(Json(exception_view(&e, tz, applied)))
}

/// List exceptions of the active series.
///
/// Returns the cancelled or rescheduled occurrences recorded for the space's active recurrence,
/// ordered by original start time, each with the series timezone. Returns an empty list when there
/// is no active recurrence. The caller must be a signed-in owner or member of the space.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/recurrence/exceptions", tag = "Scheduling (v2)", operation_id = "v2_list_recurrence_exceptions",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses((status = 200, description = "Exceptions", body = [RecurrenceExceptionResponse]), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_recurrence_exceptions(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<Vec<RecurrenceExceptionResponse>>> {
    let list = state.meeting.recurrence.list_exceptions(&actor, id).await?;
    Ok(Json(
        list.into_iter()
            .map(|(e, tz)| exception_view(&e, tz, None))
            .collect(),
    ))
}
