//! Meeting Handlers
//!
//! HTTP handlers for meeting lifecycle management with LiveKit integration.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::{
        ApiKeyClaims, ApiResponse, CreateMeetingRequest,
        EndMeetingResponse, JoinMeetingRequest, JoinMeetingResponse, ListMeetingsQuery,
        MeetingResponse, PaginatedResponse, PaginationMeta, UpdateMeetingRequest,
    },
    error::{ApiError, ApiResult},
    models::{
        chat_messages::{self, Entity as ChatMessages},
        meetings::{self, Entity as Meetings, MeetingStatus},
        participants::{self, Entity as Participants, ParticipantRole, ParticipantStatus},
        session_logs::{self, EventType},
    },
    services::auth::Claims,
    state::AppState,
    utils::{format_duration, format_role, format_status, format_utc, format_utc_opt, generate_meeting_identifier, local_to_utc, now_utc},
};

/// Create an instant public meeting
///
/// Creates a meeting that starts immediately and lasts 1 hour.
/// The authenticated user becomes the host and is automatically joined.
/// Returns join token for immediate connection.
#[utoipa::path(
    post,
    path = "/api/v1/meetings/instant",
    tag = "Meetings",
    request_body = InstantMeetingRequest,
    responses(
        (status = 201, description = "Instant meeting created and joined", body = InstantMeetingApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_instant_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<crate::dto::InstantMeetingRequest>,
) -> ApiResult<(StatusCode, Json<crate::dto::InstantMeetingApiResponse>)> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let now = now_utc();
    let end_time = now + chrono::Duration::hours(1);

    // Generate meeting identifier
    let meeting_identifier = generate_meeting_identifier();
    let meeting_id = Uuid::new_v4();
    let title = payload.title.unwrap_or_else(|| "Instant Meeting".to_string());

    // Create meeting with status = ongoing (starts immediately)
    let meeting = meetings::ActiveModel {
        id: Set(meeting_id),
        meeting_identifier: Set(meeting_identifier.clone()),
        user_id: Set(Some(user_id)),
        host_id: Set(user_id),
        title: Set(title.clone()),
        metadata: Set(None),
        is_private: Set(false), // Always public
        start_time: Set(now),
        end_time: Set(Some(end_time)),
        status: Set(MeetingStatus::Ongoing), // Start immediately
        created_at: Set(now),
        updated_at: Set(now),
    };

    let meeting = meeting.insert(&state.db).await?;

    // Get user's display name
    use crate::models::users::Entity as Users;
    let user = Users::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;

    // Create host participant record
    let participant_id = Uuid::new_v4();
    let participant = participants::ActiveModel {
        id: Set(participant_id),
        meeting_id: Set(meeting_id),
        user_id: Set(Some(user_id)),
        role: Set(ParticipantRole::Host),
        join_time: Set(now),
        leave_time: Set(None),
        status: Set(ParticipantStatus::Joined),
        is_muted: Set(false),
        is_video_on: Set(true),
        is_screen_sharing: Set(false),
        display_name: Set(user.username.clone()),
    };

    participant.insert(&state.db).await?;

    // Log the meeting start event
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::MeetingStart),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "started_by": user_id.to_string(),
            "display_name": user.username,
            "instant_meeting": true
        }))),
    };
    session_log.insert(&state.db).await?;

    // Log the host join event
    let join_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::ParticipantJoin),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "display_name": user.username,
            "role": "host",
            "is_guest": false
        }))),
    };
    join_log.insert(&state.db).await?;

    // Generate LiveKit join token
    let join_token = state.livekit_service.generate_join_token(
        &meeting_identifier,
        &participant_id.to_string(),
        &user.username,
        true, // is_host
    )?;

    let response = crate::dto::InstantMeetingResponse {
        id: meeting.id,
        meeting_identifier: meeting.meeting_identifier.clone(),
        host_id: meeting.host_id,
        title: meeting.title,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc(end_time),
        status: "ongoing".to_string(),
        join_url: state.join_url(&meeting.meeting_identifier),
        created_at: format_utc(meeting.created_at),
        participant_id,
        join_token,
        livekit_url: state.livekit_service.get_url().to_string(),
        room_name: meeting.meeting_identifier,
    };

    Ok((
        StatusCode::CREATED,
        Json(crate::dto::InstantMeetingApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// List meetings for the authenticated user
#[utoipa::path(
    get,
    path = "/api/v1/meetings",
    tag = "Meetings",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("limit" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)"),
        ("status" = Option<String>, Query, description = "Filter by status: scheduled, live, ended, cancelled")
    ),
    responses(
        (status = 200, description = "List of meetings", body = PaginatedMeetingResponse),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_meetings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListMeetingsQuery>,
) -> ApiResult<Json<PaginatedResponse<MeetingResponse>>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).min(100);

    let mut select = Meetings::find().filter(meetings::Column::HostId.eq(user_id));

    // Filter by status if provided
    if let Some(status) = &query.status {
        let meeting_status = match status.as_str() {
            "scheduled" => MeetingStatus::Scheduled,
            "live" | "ongoing" => MeetingStatus::Ongoing,
            "ended" => MeetingStatus::Ended,
            "cancelled" => MeetingStatus::Cancelled,
            _ => {
                return Err(ApiError::ValidationError(
                    "Invalid status. Use: scheduled, live, ended, cancelled".to_string(),
                ))
            }
        };
        select = select.filter(meetings::Column::Status.eq(meeting_status));
    }

    // Get total count
    let total = select.clone().count(&state.db).await?;

    // Get paginated results
    let meetings_list = select
        .order_by_desc(meetings::Column::CreatedAt)
        .paginate(&state.db, limit)
        .fetch_page(page - 1)
        .await?;

    let data: Vec<MeetingResponse> = meetings_list
        .into_iter()
        .map(|m| {
            let join_url = state.join_url(&m.meeting_identifier);
            MeetingResponse {
                id: m.id,
                meeting_identifier: m.meeting_identifier,
                host_id: m.host_id,
                title: m.title,
                metadata: m.metadata,
                is_private: m.is_private,
                start_time: format_utc(m.start_time),
                end_time: format_utc_opt(m.end_time),
                status: format_status(&m.status),
                join_url,
                created_at: format_utc(m.created_at),
                updated_at: format_utc(m.updated_at),
            }
        })
        .collect();

    let total_pages = (total as f64 / limit as f64).ceil() as u64;

    Ok(Json(PaginatedResponse {
        success: true,
        data,
        meta: PaginationMeta {
            page,
            limit,
            total,
            total_pages,
        },
    }))
}

/// Create a new meeting
///
/// The authenticated user becomes the host. Meeting starts as "scheduled".
/// No RTC resources are allocated at this point.
#[utoipa::path(
    post,
    path = "/api/v1/meetings",
    tag = "Meetings",
    request_body = CreateMeetingRequest,
    responses(
        (status = 201, description = "Meeting created successfully", body = MeetingApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<CreateMeetingRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<MeetingResponse>>)> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Convert local times to UTC using the provided timezone
    let start_time_utc = local_to_utc(payload.start_time, &payload.timezone)
        .map_err(|e| ApiError::ValidationError(e))?;

    let end_time_utc = payload
        .end_time
        .map(|et| local_to_utc(et, &payload.timezone))
        .transpose()
        .map_err(|e| ApiError::ValidationError(e))?;

    let now = now_utc();

    // Validate start time is in the future (at least 1 minute from now)
    if start_time_utc <= now + chrono::Duration::minutes(1) {
        return Err(ApiError::ValidationError(
            "Start time must be at least 1 minute in the future".to_string(),
        ));
    }

    // Validate time range
    if let Some(end_time) = end_time_utc {
        if end_time <= start_time_utc {
            return Err(ApiError::ValidationError(
                "End time must be after start time".to_string(),
            ));
        }

        // Validate meeting duration is reasonable (max 24 hours)
        let duration = end_time - start_time_utc;
        if duration > chrono::Duration::hours(24) {
            return Err(ApiError::ValidationError(
                "Meeting duration cannot exceed 24 hours".to_string(),
            ));
        }
    }

    // Generate meeting identifier (human-friendly code)
    let meeting_identifier = generate_meeting_identifier();
    let meeting_id = Uuid::new_v4();

    // Create meeting with status = scheduled
    // Times are converted to UTC for storage
    let meeting = meetings::ActiveModel {
        id: Set(meeting_id),
        meeting_identifier: Set(meeting_identifier),
        user_id: Set(Some(user_id)),
        host_id: Set(user_id),
        title: Set(payload.title),
        metadata: Set(payload.metadata),
        is_private: Set(payload.is_private.unwrap_or(false)),
        start_time: Set(start_time_utc),
        end_time: Set(end_time_utc),
        status: Set(MeetingStatus::Scheduled),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let meeting = meeting.insert(&state.db).await?;

    let response = MeetingResponse {
        id: meeting.id,
        meeting_identifier: meeting.meeting_identifier.clone(),
        host_id: meeting.host_id,
        title: meeting.title,
        metadata: meeting.metadata,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc_opt(meeting.end_time),
        status: format_status(&meeting.status),
        join_url: state.join_url(&meeting.meeting_identifier),
        created_at: format_utc(meeting.created_at),
        updated_at: format_utc(meeting.updated_at),
    };

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// Get a meeting by ID
#[utoipa::path(
    get,
    path = "/api/v1/meetings/{id}",
    tag = "Meetings",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Meeting details", body = MeetingApiResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Meeting not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<MeetingResponse>>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let meeting = Meetings::find_by_id(id)
        .filter(meetings::Column::HostId.eq(user_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    let response = MeetingResponse {
        id: meeting.id,
        meeting_identifier: meeting.meeting_identifier.clone(),
        host_id: meeting.host_id,
        title: meeting.title,
        metadata: meeting.metadata,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc_opt(meeting.end_time),
        status: format_status(&meeting.status),
        join_url: state.join_url(&meeting.meeting_identifier),
        created_at: format_utc(meeting.created_at),
        updated_at: format_utc(meeting.updated_at),
    };

    Ok(Json(ApiResponse {
        success: true,
        data: response,
    }))
}

/// Update a meeting (host only)
#[utoipa::path(
    put,
    path = "/api/v1/meetings/{id}",
    tag = "Meetings",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    request_body = UpdateMeetingRequest,
    responses(
        (status = 200, description = "Meeting updated successfully", body = MeetingApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can update"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Cannot update ongoing or ended meetings")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateMeetingRequest>,
) -> ApiResult<Json<ApiResponse<MeetingResponse>>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check if user is host
    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can update the meeting".to_string(),
        ));
    }

    // Cannot update ended meetings
    if meeting.status == MeetingStatus::Ended {
        return Err(ApiError::Conflict(
            "Cannot update an ended meeting".to_string(),
        ));
    }

    // Cannot update ongoing meetings (meeting has started)
    if meeting.status == MeetingStatus::Ongoing {
        return Err(ApiError::Conflict(
            "Cannot update a meeting that has already started. Please end the meeting first if you need to make changes.".to_string(),
        ));
    }

    let now = now_utc();

    // Convert local times to UTC if timezone is provided
    let start_time_utc = match (payload.start_time, &payload.timezone) {
        (Some(st), Some(tz)) => Some(local_to_utc(st, tz).map_err(|e| ApiError::ValidationError(e))?),
        (Some(_), None) => {
            return Err(ApiError::ValidationError(
                "timezone is required when updating start_time".to_string(),
            ));
        }
        _ => None,
    };

    let end_time_utc = match (payload.end_time, &payload.timezone) {
        (Some(et), Some(tz)) => Some(local_to_utc(et, tz).map_err(|e| ApiError::ValidationError(e))?),
        (Some(_), None) => {
            return Err(ApiError::ValidationError(
                "timezone is required when updating end_time".to_string(),
            ));
        }
        _ => None,
    };

    // Determine final start_time and end_time after update (all in UTC)
    let final_start_time = start_time_utc.unwrap_or(meeting.start_time);
    let final_end_time = end_time_utc.or(meeting.end_time);

    // Validate start time is in the future (allow updates up to current time for ongoing meetings)
    if meeting.status == MeetingStatus::Scheduled && final_start_time <= now {
        return Err(ApiError::ValidationError(
            "Start time must be in the future for scheduled meetings".to_string(),
        ));
    }

    // Validate time range
    if let Some(end_time) = final_end_time {
        if end_time <= final_start_time {
            return Err(ApiError::ValidationError(
                "End time must be after start time".to_string(),
            ));
        }

        // Validate meeting duration is reasonable (max 24 hours)
        let duration = end_time - final_start_time;
        if duration > chrono::Duration::hours(24) {
            return Err(ApiError::ValidationError(
                "Meeting duration cannot exceed 24 hours".to_string(),
            ));
        }
    }

    // Update meeting (times converted to UTC)
    let mut meeting: meetings::ActiveModel = meeting.into();

    if let Some(title) = payload.title {
        meeting.title = Set(title);
    }
    if let Some(is_private) = payload.is_private {
        meeting.is_private = Set(is_private);
    }
    if let Some(start_time) = start_time_utc {
        meeting.start_time = Set(start_time);
    }
    if let Some(end_time) = end_time_utc {
        meeting.end_time = Set(Some(end_time));
    }
    if let Some(metadata) = payload.metadata {
        meeting.metadata = Set(Some(metadata));
    }
    meeting.updated_at = Set(now);

    let meeting = meeting.update(&state.db).await?;

    let response = MeetingResponse {
        id: meeting.id,
        meeting_identifier: meeting.meeting_identifier.clone(),
        host_id: meeting.host_id,
        title: meeting.title,
        metadata: meeting.metadata,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc_opt(meeting.end_time),
        status: format_status(&meeting.status),
        join_url: state.join_url(&meeting.meeting_identifier),
        created_at: format_utc(meeting.created_at),
        updated_at: format_utc(meeting.updated_at),
    };

    Ok(Json(ApiResponse {
        success: true,
        data: response,
    }))
}

/// Delete a meeting (host only, only scheduled meetings)
#[utoipa::path(
    delete,
    path = "/api/v1/meetings/{id}",
    tag = "Meetings",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 204, description = "Meeting deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can delete"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Can only delete scheduled meetings")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn delete_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check if user is host
    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can delete the meeting".to_string(),
        ));
    }

    // Can only delete scheduled meetings (not live or ended)
    if meeting.status != MeetingStatus::Scheduled {
        return Err(ApiError::Conflict(
            "Can only delete scheduled meetings. Use cancel or end instead.".to_string(),
        ));
    }

    // Delete meeting
    Meetings::delete_by_id(id).exec(&state.db).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Join a meeting
///
/// This is where control-plane meets real-time:
/// 1. Validates meeting exists and is not ended
/// 2. Creates a Participant record
/// 3. Assigns role (host or participant)
/// 4. Generates a short-lived LiveKit join token
/// 5. If meeting is scheduled and start time reached, transitions it to live
/// 6. Returns join token to client
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{id}/join",
    tag = "Meetings",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    request_body = JoinMeetingRequest,
    responses(
        (status = 200, description = "Successfully joined meeting", body = JoinMeetingApiResponse),
        (status = 400, description = "Validation error - Invalid user_id or display_name"),
        (status = 403, description = "Private meeting - guests not allowed"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting has ended, cancelled, hasn't started yet, or past scheduled end time")
    )
)]
pub async fn join_meeting(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<JoinMeetingRequest>,
) -> ApiResult<Json<ApiResponse<JoinMeetingResponse>>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Use shared join logic
    join_meeting_internal(state, meeting, payload).await
}

/// Internal helper function containing the core join logic
/// This is shared between join_meeting (by UUID) and join_meeting_by_identifier
async fn join_meeting_internal(
    state: AppState,
    meeting: meetings::Model,
    payload: JoinMeetingRequest,
) -> ApiResult<Json<ApiResponse<JoinMeetingResponse>>> {
    // Check if meeting is ended - no joins allowed after ended
    if meeting.status == MeetingStatus::Ended {
        return Err(ApiError::Conflict("Meeting has ended".to_string()));
    }

    if meeting.status == MeetingStatus::Cancelled {
        return Err(ApiError::Conflict("Meeting has been cancelled".to_string()));
    }

    let now = now_utc();
    let start_time = meeting.start_time;

    // Time-based validation for scheduled meetings
    // Prevent joining if meeting status is still "Scheduled" and current time is before start time
    if meeting.status == MeetingStatus::Scheduled && now < start_time {
        let minutes_until_start = (start_time - now).num_minutes();
        return Err(ApiError::Conflict(
            format!(
                "Meeting hasn't started yet. Please try again in {}.",
                format_duration(minutes_until_start)
            )
        ));
    }

    // Check if meeting has passed its scheduled end time
    if let Some(end_time) = meeting.end_time {
        if now > end_time {
            // Meeting should have ended - prevent new joins
            return Err(ApiError::Conflict(
                "This meeting has passed its scheduled end time and is no longer accepting participants.".to_string()
            ));
        }
    }

    // Check if meeting is private (would require invitation check in real impl)
    if meeting.is_private && payload.user_id.is_none() {
        return Err(ApiError::Forbidden(
            "This is a private meeting. Guests cannot join.".to_string(),
        ));
    }

    // Validate user exists if user_id is provided
    if let Some(user_id) = payload.user_id {
        use crate::models::users::Entity as Users;

        let user_exists = Users::find_by_id(user_id).one(&state.db).await?.is_some();

        if !user_exists {
            return Err(ApiError::ValidationError(
                "User not found. Please provide a valid user_id or join as a guest.".to_string(),
            ));
        }
    }

    // Determine participant type and role
    let is_host = payload
        .user_id
        .map(|uid| uid == meeting.host_id)
        .unwrap_or(false);
    let role = if is_host {
        ParticipantRole::Host
    } else {
        ParticipantRole::Participant
    };

    let now = now_utc();

    // Check if participant already exists for this meeting (by user_id or display_name for guests)
    let existing_participant = if let Some(user_id) = payload.user_id {
        // For authenticated users, check by user_id
        Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting.id))
            .filter(participants::Column::UserId.eq(user_id))
            .filter(participants::Column::LeaveTime.is_null())
            .one(&state.db)
            .await?
    } else {
        // For guests, check by display_name (not ideal but necessary)
        Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting.id))
            .filter(participants::Column::UserId.is_null())
            .filter(participants::Column::DisplayName.eq(&payload.display_name))
            .filter(participants::Column::LeaveTime.is_null())
            .one(&state.db)
            .await?
    };

    // If participant exists, handle based on their status
    if let Some(existing) = existing_participant {
        match existing.status {
            ParticipantStatus::Waiting => {
                // Still in waiting room - return without token
                return Ok(Json(ApiResponse {
                    success: true,
                    data: JoinMeetingResponse {
                        meeting_id: meeting.id,
                        participant_id: existing.id,
                        role: format_role(&existing.role),
                        join_token: String::new(),
                        livekit_url: String::new(),
                        room_name: meeting.meeting_identifier.clone(),
                        access_token: None,
                        refresh_token: None,
                    },
                }));
            }
            ParticipantStatus::Joined => {
                // Already admitted - generate token and return
                let room_name = &meeting.meeting_identifier;
                let join_token = state.livekit_service.generate_join_token(
                    room_name,
                    &existing.id.to_string(),
                    &existing.display_name,
                    existing.role == ParticipantRole::Host,
                )?;

                // Helper to generate guest tokens if needed
                let (access_token, refresh_token) = if existing.user_id.is_none() {
                    let acc = state.jwt_service.generate_access_token(existing.id, "guest")?;
                    let reference = state.jwt_service.generate_refresh_token(existing.id, "guest")?;
                    (Some(acc), Some(reference))
                } else {
                    (None, None)
                };

                return Ok(Json(ApiResponse {
                    success: true,
                    data: JoinMeetingResponse {
                        meeting_id: meeting.id,
                        participant_id: existing.id,
                        role: format_role(&existing.role),
                        join_token,
                        livekit_url: state.livekit_service.get_url().to_string(),
                        room_name: room_name.clone(),
                        access_token,
                        refresh_token,
                    },
                }));
            }
            ParticipantStatus::Kicked => {
                return Err(ApiError::Forbidden(
                    "You have been removed from this meeting".to_string(),
                ));
            }
        }
    }

    // Create new participant record
    let participant_id = Uuid::new_v4();

    // Determine initial status: waiting room for private meetings (except host)
    let initial_status = if meeting.is_private && !is_host {
        ParticipantStatus::Waiting
    } else {
        ParticipantStatus::Joined
    };

    let participant = participants::ActiveModel {
        id: Set(participant_id),
        meeting_id: Set(meeting.id),
        user_id: Set(payload.user_id), // NULL for guests
        role: Set(role.clone()),
        join_time: Set(now),
        leave_time: Set(None),
        status: Set(initial_status.clone()),
        is_muted: Set(false),
        is_video_on: Set(true),
        is_screen_sharing: Set(false),
        display_name: Set(payload.display_name.clone()),
    };

    let _participant = participant.insert(&state.db).await?;

    // If participant is in waiting room, return a different response
    if initial_status == ParticipantStatus::Waiting {
        // Generate guest tokens even for waiting room?
        // Yes, they might need to poll "waiting status" or "chat" if allowed?
        // Actually, waiting participants usually can't do much.
        // But if we want them to sustain a session, we should probably give them a token.
        // However, `auth_middleware` checks for "guest" role against `Participants`.
        // If they are in `Participants` table, they are valid.
        
        // Let's issue tokens for guests in waiting room too, so they can poll cleanly.
        let (access_token, refresh_token) = if payload.user_id.is_none() {
            let acc = state.jwt_service.generate_access_token(participant_id, "guest")?;
            let reference = state.jwt_service.generate_refresh_token(participant_id, "guest")?;
            (Some(acc), Some(reference))
        } else {
            (None, None)
        };

        return Ok(Json(ApiResponse {
            success: true,
            data: JoinMeetingResponse {
                meeting_id: meeting.id,
                participant_id,
                role: format_role(&role),
                join_token: String::new(), // No token for waiting room
                livekit_url: String::new(),
                room_name: meeting.meeting_identifier.clone(),
                access_token,
                refresh_token,
            },
        }));
    }

    // If meeting is scheduled, transition to live (ongoing)
    if meeting.status == MeetingStatus::Scheduled {
        let mut meeting_update: meetings::ActiveModel = meeting.clone().into();
        meeting_update.status = Set(MeetingStatus::Ongoing);
        meeting_update.updated_at = Set(now);
        meeting_update.update(&state.db).await?;

        // Log the meeting start event
        let session_log = session_logs::ActiveModel {
            id: Set(Uuid::new_v4()),
            meeting_id: Set(meeting.id),
            participant_id: Set(Some(participant_id)),
            event_type: Set(EventType::MeetingStart),
            event_time: Set(now),
            metadata: Set(Some(serde_json::json!({
                "started_by": participant_id.to_string(),
                "display_name": payload.display_name
            }))),
        };
        session_log.insert(&state.db).await?;
    }

    // Log the join event
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting.id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::ParticipantJoin),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "display_name": payload.display_name,
            "role": format_role(&role),
            "is_guest": payload.user_id.is_none()
        }))),
    };
    session_log.insert(&state.db).await?;

    // Generate LiveKit join token
    // Room name is the meeting identifier (human-friendly code)
    let room_name = &meeting.meeting_identifier;
    let join_token = state.livekit_service.generate_join_token(
        room_name,
        &participant_id.to_string(),
        &payload.display_name,
        is_host,
    )?;

    // Generate guest tokens if needed
    let (access_token, refresh_token) = if payload.user_id.is_none() {
        let acc = state.jwt_service.generate_access_token(participant_id, "guest")?;
        let reference = state.jwt_service.generate_refresh_token(participant_id, "guest")?;
        (Some(acc), Some(reference))
    } else {
        (None, None)
    };

    let response = JoinMeetingResponse {
        meeting_id: meeting.id,
        participant_id,
        role: format_role(&role),
        join_token,
        livekit_url: state.livekit_service.get_url().to_string(),
        room_name: room_name.clone(),
        access_token,
        refresh_token,
    };

    Ok(Json(ApiResponse {
        success: true,
        data: response,
    }))
}

/// End a meeting (host only)
///
/// Terminates meeting for all participants and triggers session logs.
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{id}/end",
    tag = "Meetings",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Meeting ended successfully", body = EndMeetingApiResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can end meeting"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting already ended")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn end_meeting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ApiResponse<EndMeetingResponse>>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check if user is host
    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can end the meeting".to_string(),
        ));
    }

    // Check if meeting can be ended
    if meeting.status == MeetingStatus::Ended {
        return Err(ApiError::Conflict("Meeting has already ended".to_string()));
    }

    let now = now_utc();

    // Update meeting status to ended
    let mut meeting_update: meetings::ActiveModel = meeting.into();
    meeting_update.status = Set(MeetingStatus::Ended);
    meeting_update.end_time = Set(Some(now));
    meeting_update.updated_at = Set(now);
    meeting_update.update(&state.db).await?;

    // Update all active participants to mark leave time
    let active_participants = Participants::find()
        .filter(participants::Column::MeetingId.eq(id))
        .filter(participants::Column::LeaveTime.is_null())
        .all(&state.db)
        .await?;

    for p in active_participants {
        let mut participant: participants::ActiveModel = p.into();
        participant.leave_time = Set(Some(now));
        participant.update(&state.db).await?;
    }

    // Clear chat messages (volatile per session)
    ChatMessages::delete_many()
        .filter(chat_messages::Column::MeetingId.eq(id))
        .exec(&state.db)
        .await?;

    // Log the meeting end event
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(id),
        participant_id: Set(None),
        event_type: Set(EventType::MeetingEnd),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "ended_by_host": user_id.to_string(),
            "ended_at": now.to_string()
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ApiResponse {
        success: true,
        data: EndMeetingResponse {
            status: "ended".to_string(),
        },
    }))
}

/// Join a meeting by meeting identifier (8-character code)
///
/// This endpoint allows joining using the human-friendly meeting code
/// instead of the UUID. More convenient for users.
#[utoipa::path(
    post,
    path = "/api/v1/meetings/join/{meeting_identifier}",
    tag = "Meetings",
    params(
        ("meeting_identifier" = String, Path, description = "Meeting identifier (8-character code, e.g., ABCD1234)")
    ),
    request_body = JoinMeetingRequest,
    responses(
        (status = 200, description = "Successfully joined meeting", body = JoinMeetingApiResponse),
        (status = 400, description = "Validation error - Invalid user_id or display_name"),
        (status = 403, description = "Private meeting - guests not allowed"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting has ended, cancelled, hasn't started yet, or past scheduled end time")
    )
)]
pub async fn join_meeting_by_identifier(
    State(state): State<AppState>,
    Path(meeting_identifier): Path<String>,
    Json(payload): Json<JoinMeetingRequest>,
) -> ApiResult<Json<ApiResponse<JoinMeetingResponse>>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find meeting by identifier
    let meeting = Meetings::find()
        .filter(meetings::Column::MeetingIdentifier.eq(&meeting_identifier))
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Meeting with code '{}' not found",
                meeting_identifier
            ))
        })?;

    // Use shared join logic
    join_meeting_internal(state, meeting, payload).await
}

// ============================================================================
// PUBLIC MEETING INFO ENDPOINT
// ============================================================================

use crate::dto::{PublicMeetingInfoApiResponse, PublicMeetingInfoResponse};

/// Get public meeting info by meeting identifier
///
/// This endpoint allows the hosted UI to fetch meeting details before joining.
/// No authentication required. Only returns public information.
#[utoipa::path(
    get,
    path = "/api/v1/meetings/public/{meeting_identifier}",
    tag = "Meetings",
    params(
        ("meeting_identifier" = String, Path, description = "Meeting identifier (8-character code, e.g., ABCD1234)")
    ),
    responses(
        (status = 200, description = "Public meeting info", body = PublicMeetingInfoApiResponse),
        (status = 404, description = "Meeting not found")
    )
)]
pub async fn get_public_meeting_info(
    State(state): State<AppState>,
    Path(meeting_identifier): Path<String>,
) -> ApiResult<Json<PublicMeetingInfoApiResponse>> {
    // Find meeting by identifier
    let meeting = Meetings::find()
        .filter(meetings::Column::MeetingIdentifier.eq(&meeting_identifier))
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Meeting with code '{}' not found",
                meeting_identifier
            ))
        })?;

    let response = PublicMeetingInfoResponse {
        meeting_identifier: meeting.meeting_identifier.clone(),
        title: meeting.title,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc_opt(meeting.end_time),
        status: format_status(&meeting.status),
        join_url: state.join_url(&meeting.meeting_identifier),
    };

    Ok(Json(PublicMeetingInfoApiResponse {
        success: true,
        data: response,
    }))
}

// ============================================================================
// API KEY AUTHENTICATED ENDPOINTS
// ============================================================================

/// Request to create a meeting via API key
///
/// **Note**: `start_time` and `end_time` are in the user's local timezone.
/// The `timezone` field must be a valid IANA timezone string.
#[derive(Debug, serde::Deserialize, Validate, utoipa::ToSchema)]
pub struct ApiCreateMeetingRequest {
    #[validate(length(
        min = 1,
        max = 200,
        message = "Title must be between 1 and 200 characters"
    ))]
    pub title: String,

    /// Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T14:00:00")]
    pub start_time: chrono::NaiveDateTime,

    /// End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T15:00:00")]
    pub end_time: Option<chrono::NaiveDateTime>,

    /// IANA timezone of the user (e.g., "Africa/Lagos", "America/New_York")
    #[schema(example = "Africa/Lagos")]
    pub timezone: String,

    #[serde(default)]
    pub is_private: Option<bool>,

    pub metadata: Option<serde_json::Value>,
}

/// Request to join a meeting via API key
#[derive(Debug, serde::Deserialize, Validate, utoipa::ToSchema)]
pub struct ApiJoinMeetingRequest {
    #[validate(length(
        min = 1,
        max = 100,
        message = "Display name must be between 1 and 100 characters"
    ))]
    pub display_name: String,
}

/// Create a new meeting via API key
///
/// Third-party integrations can use this endpoint with Api-Key authentication
/// to create meetings on behalf of the key owner.
#[utoipa::path(
    post,
    path = "/api/v1/api/meetings",
    tag = "API Keys",
    request_body = ApiCreateMeetingRequest,
    responses(
        (status = 201, description = "Meeting created successfully", body = MeetingApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized - Invalid or missing Api-Key"),
        (status = 403, description = "Forbidden - API key usage limit exceeded")
    ),
    security(
        ("api_key_auth" = [])
    )
)]
pub async fn api_create_meeting(
    State(state): State<AppState>,
    Extension(api_claims): Extension<ApiKeyClaims>,
    Json(payload): Json<ApiCreateMeetingRequest>,
) -> ApiResult<(StatusCode, Json<ApiResponse<MeetingResponse>>)> {
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user_id = api_claims.user_id;

    // Convert local times to UTC using the provided timezone
    let start_time_utc = local_to_utc(payload.start_time, &payload.timezone)
        .map_err(|e| ApiError::ValidationError(e))?;

    let end_time_utc = payload
        .end_time
        .map(|et| local_to_utc(et, &payload.timezone))
        .transpose()
        .map_err(|e| ApiError::ValidationError(e))?;

    let now = now_utc();

    // Validate start time is in the future (at least 1 minute from now)
    if start_time_utc <= now + chrono::Duration::minutes(1) {
        return Err(ApiError::ValidationError(
            "Start time must be at least 1 minute in the future".to_string(),
        ));
    }

    // Validate time range
    if let Some(end_time) = end_time_utc {
        if end_time <= start_time_utc {
            return Err(ApiError::ValidationError(
                "End time must be after start time".to_string(),
            ));
        }

        // Validate meeting duration is reasonable (max 24 hours)
        let duration = end_time - start_time_utc;
        if duration > chrono::Duration::hours(24) {
            return Err(ApiError::ValidationError(
                "Meeting duration cannot exceed 24 hours".to_string(),
            ));
        }
    }

    // Generate meeting identifier (human-friendly code)
    let meeting_identifier = generate_meeting_identifier();
    let meeting_id = Uuid::new_v4();

    // Create meeting with status = scheduled
    // Times are converted to UTC for storage
    let meeting = meetings::ActiveModel {
        id: Set(meeting_id),
        meeting_identifier: Set(meeting_identifier),
        user_id: Set(Some(user_id)),
        host_id: Set(user_id),
        title: Set(payload.title),
        metadata: Set(payload.metadata),
        is_private: Set(payload.is_private.unwrap_or(false)),
        start_time: Set(start_time_utc),
        end_time: Set(end_time_utc),
        status: Set(MeetingStatus::Scheduled),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let meeting = meeting.insert(&state.db).await?;

    let response = MeetingResponse {
        id: meeting.id,
        meeting_identifier: meeting.meeting_identifier.clone(),
        host_id: meeting.host_id,
        title: meeting.title,
        metadata: meeting.metadata,
        is_private: meeting.is_private,
        start_time: format_utc(meeting.start_time),
        end_time: format_utc_opt(meeting.end_time),
        status: format_status(&meeting.status),
        join_url: state.join_url(&meeting.meeting_identifier),
        created_at: format_utc(meeting.created_at),
        updated_at: format_utc(meeting.updated_at),
    };

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// Join a meeting via API key
///
/// Third-party integrations can use this endpoint with Api-Key authentication
/// to join meetings. The user associated with the API key is used for authentication.
#[utoipa::path(
    post,
    path = "/api/v1/api/meetings/{id}/join",
    tag = "API Keys",
    params(
        ("id" = Uuid, Path, description = "Meeting UUID")
    ),
    request_body = ApiJoinMeetingRequest,
    responses(
        (status = 200, description = "Successfully joined meeting", body = JoinMeetingApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized - Invalid or missing Api-Key"),
        (status = 403, description = "Forbidden - API key usage limit exceeded or private meeting"),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting has ended, cancelled, too early to join, or past scheduled end time")
    ),
    security(
        ("api_key_auth" = [])
    )
)]
pub async fn api_join_meeting(
    State(state): State<AppState>,
    Extension(api_claims): Extension<ApiKeyClaims>,
    Path(id): Path<Uuid>,
    Json(payload): Json<ApiJoinMeetingRequest>,
) -> ApiResult<Json<ApiResponse<JoinMeetingResponse>>> {
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Find meeting
    let meeting = Meetings::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Use shared join logic with API key user
    let join_request = JoinMeetingRequest {
        user_id: Some(api_claims.user_id),
        display_name: payload.display_name,
    };

    join_meeting_internal(state, meeting, join_request).await
}
