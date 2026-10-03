//! Participant Management Handlers
//!
//! Handles participant-related operations including:
//! - Listing participants
//! - Kicking participants
//! - Waiting room management (admit/deny)
//! - Media control (audio/video/screen sharing)

use axum::{
    extract::{Path, State},
    Extension, Json,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::{
    dto::meetings::*,
    error::{ApiError, ApiResult},
    models::{
        meetings::Entity as Meetings,
        participants::{self, Entity as Participants, ParticipantRole, ParticipantStatus},
        session_logs::{self, EventType},
    },
    services::auth::Claims,
    state::AppState,
    utils::{format_participant_status, format_role, format_utc, format_utc_opt, now_utc},
};

// ============================================================================
// PARTICIPANT MANAGEMENT
// ============================================================================

/// List all participants in a meeting
#[utoipa::path(
    get,
    path = "/api/v1/meetings/{meeting_id}/participants",
    tag = "Participants",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Successfully retrieved participants", body = ParticipantsListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Meeting not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_participants(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
) -> ApiResult<Json<ParticipantsListResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists and user has access (host or participant)
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check if user is host or a participant in the meeting
    let is_host = meeting.host_id == user_id;
    let is_participant = if !is_host {
        Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting_id))
            .filter(participants::Column::UserId.eq(user_id))
            .one(&state.db)
            .await?
            .is_some()
    } else {
        true
    };

    if !is_host && !is_participant {
        return Err(ApiError::Forbidden(
            "You must be the host or a participant to view participants".to_string(),
        ));
    }

    // Get all active participants (not waiting, not kicked)
    let participants = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::Status.ne(ParticipantStatus::Waiting))
        .filter(participants::Column::Status.ne(ParticipantStatus::Kicked))
        .all(&state.db)
        .await?;

    let participant_responses: Vec<DetailedParticipantResponse> = participants
        .into_iter()
        .map(|p| DetailedParticipantResponse {
            participant_id: p.id,
            meeting_id: p.meeting_id,
            user_id: p.user_id,
            role: format_role(&p.role),
            display_name: p.display_name,
            status: format_participant_status(&p.status),
            join_time: format_utc(p.join_time),
            leave_time: format_utc_opt(p.leave_time),
            is_muted: p.is_muted,
            is_video_on: p.is_video_on,
            is_screen_sharing: p.is_screen_sharing,
        })
        .collect();

    Ok(Json(ParticipantsListResponse {
        success: true,
        data: participant_responses,
    }))
}

/// Kick a participant from the meeting (host only)
#[utoipa::path(
    post,
    path = "/api/v1/participants/{participant_id}/kick",
    tag = "Participants",
    params(
        ("participant_id" = Uuid, Path, description = "Participant UUID")
    ),
    responses(
        (status = 200, description = "Participant kicked successfully", body = ParticipantActionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can kick participants"),
        (status = 404, description = "Participant not found"),
        (status = 409, description = "Cannot kick the host")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn kick_participant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(participant_id): Path<Uuid>,
) -> ApiResult<Json<ParticipantActionResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find participant
    let participant = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found".to_string()))?;

    // Get meeting to verify host
    let meeting = Meetings::find_by_id(participant.meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Only host can kick
    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can kick participants".to_string(),
        ));
    }

    // Cannot kick the host
    if participant.role == ParticipantRole::Host {
        return Err(ApiError::Conflict("Cannot kick the host".to_string()));
    }

    // Update participant status to kicked
    let now = now_utc();
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.status = Set(ParticipantStatus::Kicked);
    participant_update.leave_time = Set(Some(now));
    participant_update.update(&state.db).await?;

    // Log the kick event
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(participant.meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::ParticipantLeave),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "kicked": true,
            "kicked_by": user_id.to_string(),
            "display_name": participant.display_name
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ParticipantActionResponse {
        success: true,
        message: format!("Participant {} has been kicked", participant.display_name),
    }))
}

// ============================================================================
// WAITING ROOM MANAGEMENT
// ============================================================================

/// List participants in the waiting room
#[utoipa::path(
    get,
    path = "/api/v1/meetings/{meeting_id}/waiting",
    tag = "Waiting Room",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Successfully retrieved waiting participants", body = WaitingListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can view waiting room"),
        (status = 404, description = "Meeting not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_waiting_participants(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
) -> ApiResult<Json<WaitingListResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists and user is host
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can view the waiting room".to_string(),
        ));
    }

    // Get waiting participants
    let waiting_participants = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::Status.eq(ParticipantStatus::Waiting))
        .all(&state.db)
        .await?;

    let waiting_responses: Vec<WaitingParticipantResponse> = waiting_participants
        .into_iter()
        .map(|p| WaitingParticipantResponse {
            participant_id: p.id,
            user_id: p.user_id,
            display_name: p.display_name,
            join_time: format_utc(p.join_time),
        })
        .collect();

    Ok(Json(WaitingListResponse {
        success: true,
        data: waiting_responses,
    }))
}

/// Admit a participant from the waiting room (host only)
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{meeting_id}/waiting/{participant_id}/admit",
    tag = "Waiting Room",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID"),
        ("participant_id" = Uuid, Path, description = "Participant UUID")
    ),
    responses(
        (status = 200, description = "Participant admitted successfully", body = ParticipantActionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can admit participants"),
        (status = 404, description = "Participant not found"),
        (status = 409, description = "Participant is not in waiting room")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn admit_participant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((meeting_id, participant_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ParticipantActionResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists and user is host
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can admit participants".to_string(),
        ));
    }

    // Find participant
    let participant = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found".to_string()))?;

    // Verify participant is in waiting room
    if participant.status != ParticipantStatus::Waiting {
        return Err(ApiError::Conflict(
            "Participant is not in the waiting room".to_string(),
        ));
    }

    // Admit participant (change status to joined)
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.status = Set(ParticipantStatus::Joined);
    participant_update.update(&state.db).await?;

    // Log the admit event
    let now = now_utc();
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::ParticipantJoin),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "admitted_from_waiting_room": true,
            "display_name": participant.display_name
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ParticipantActionResponse {
        success: true,
        message: format!("Participant {} has been admitted", participant.display_name),
    }))
}

/// Deny a participant from the waiting room (host only)
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{meeting_id}/waiting/{participant_id}/deny",
    tag = "Waiting Room",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID"),
        ("participant_id" = Uuid, Path, description = "Participant UUID")
    ),
    responses(
        (status = 200, description = "Participant denied successfully", body = ParticipantActionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Only host can deny participants"),
        (status = 404, description = "Participant not found"),
        (status = 409, description = "Participant is not in waiting room")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deny_participant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((meeting_id, participant_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ParticipantActionResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists and user is host
    let meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    if meeting.host_id != user_id {
        return Err(ApiError::Forbidden(
            "Only the host can deny participants".to_string(),
        ));
    }

    // Find participant
    let participant = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found".to_string()))?;

    // Verify participant is in waiting room
    if participant.status != ParticipantStatus::Waiting {
        return Err(ApiError::Conflict(
            "Participant is not in the waiting room".to_string(),
        ));
    }

    // Deny participant (change status to kicked and set leave time)
    let now = now_utc();
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.status = Set(ParticipantStatus::Kicked);
    participant_update.leave_time = Set(Some(now));
    participant_update.update(&state.db).await?;

    // Log the deny event
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::ParticipantLeave),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "denied_from_waiting_room": true,
            "display_name": participant.display_name
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ParticipantActionResponse {
        success: true,
        message: format!("Participant {} has been denied", participant.display_name),
    }))
}

// ============================================================================
// MEDIA CONTROL
// ============================================================================

/// Update participant audio state (mute/unmute)
#[utoipa::path(
    patch,
    path = "/api/v1/participants/{participant_id}/audio",
    tag = "Media Control",
    params(
        ("participant_id" = Uuid, Path, description = "Participant UUID")
    ),
    request_body = UpdateAudioRequest,
    responses(
        (status = 200, description = "Audio state updated successfully", body = MediaStateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Can only control your own media or host can control any"),
        (status = 404, description = "Participant not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_audio(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(participant_id): Path<Uuid>,
    Json(payload): Json<UpdateAudioRequest>,
) -> ApiResult<Json<MediaStateResponse>> {
    let subject_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find participant
    let participant = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found".to_string()))?;

    // Get meeting to check if user is host
    let meeting = Meetings::find_by_id(participant.meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check permissions:
    // - For guests: claims.sub is participant_id, so check if it matches
    // - For users: claims.sub is user_id, check if they own this participant or are host
    let is_guest = claims.role == "guest";
    let is_own_participant = if is_guest {
        // For guests, the token subject IS the participant_id
        participant_id == subject_id
    } else {
        // For authenticated users, check user_id match
        participant.user_id == Some(subject_id)
    };
    let is_host = !is_guest && meeting.host_id == subject_id;

    // Can only control own media or host can control any
    if !is_host && !is_own_participant {
        return Err(ApiError::Forbidden(
            "You can only control your own media state".to_string(),
        ));
    }

    // Update audio state
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.is_muted = Set(payload.is_muted);
    let updated_participant = participant_update.update(&state.db).await?;

    // Log the media toggle event
    let now = now_utc();
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(participant.meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::MediaToggle),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "media_type": "audio",
            "action": if payload.is_muted { "muted" } else { "unmuted" },
            "controlled_by_host": is_host && !is_own_participant
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(MediaStateResponse {
        success: true,
        participant_id,
        is_muted: updated_participant.is_muted,
        is_video_on: updated_participant.is_video_on,
        is_screen_sharing: updated_participant.is_screen_sharing,
    }))
}

/// Update participant video state (enable/disable)
#[utoipa::path(
    patch,
    path = "/api/v1/participants/{participant_id}/video",
    tag = "Media Control",
    params(
        ("participant_id" = Uuid, Path, description = "Participant UUID")
    ),
    request_body = UpdateVideoRequest,
    responses(
        (status = 200, description = "Video state updated successfully", body = MediaStateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Can only control your own media or host can control any"),
        (status = 404, description = "Participant not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_video(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(participant_id): Path<Uuid>,
    Json(payload): Json<UpdateVideoRequest>,
) -> ApiResult<Json<MediaStateResponse>> {
    let subject_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Find participant
    let participant = Participants::find_by_id(participant_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Participant not found".to_string()))?;

    // Get meeting to check if user is host
    let meeting = Meetings::find_by_id(participant.meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Check permissions:
    // - For guests: claims.sub is participant_id, so check if it matches
    // - For users: claims.sub is user_id, check if they own this participant or are host
    let is_guest = claims.role == "guest";
    let is_own_participant = if is_guest {
        // For guests, the token subject IS the participant_id
        participant_id == subject_id
    } else {
        // For authenticated users, check user_id match
        participant.user_id == Some(subject_id)
    };
    let is_host = !is_guest && meeting.host_id == subject_id;

    // Can only control own media or host can control any
    if !is_host && !is_own_participant {
        return Err(ApiError::Forbidden(
            "You can only control your own media state".to_string(),
        ));
    }

    // Update video state
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.is_video_on = Set(payload.is_video_on);
    let updated_participant = participant_update.update(&state.db).await?;

    // Log the media toggle event
    let now = now_utc();
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(participant.meeting_id),
        participant_id: Set(Some(participant_id)),
        event_type: Set(EventType::MediaToggle),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "media_type": "video",
            "action": if payload.is_video_on { "enabled" } else { "disabled" },
            "controlled_by_host": is_host && !is_own_participant
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(MediaStateResponse {
        success: true,
        participant_id,
        is_muted: updated_participant.is_muted,
        is_video_on: updated_participant.is_video_on,
        is_screen_sharing: updated_participant.is_screen_sharing,
    }))
}

/// Start screen sharing
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{meeting_id}/screen-share/start",
    tag = "Media Control",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Screen sharing started successfully", body = ScreenShareResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Meeting or participant not found"),
        (status = 409, description = "Already screen sharing or another participant is sharing")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn start_screen_share(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
) -> ApiResult<Json<ScreenShareResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists
    let _meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Find user's participant record
    let participant = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::UserId.eq(user_id))
        .filter(participants::Column::Status.eq(ParticipantStatus::Joined))
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound("You are not a participant in this meeting".to_string())
        })?;

    // Check if already screen sharing
    if participant.is_screen_sharing {
        return Err(ApiError::Conflict(
            "You are already screen sharing".to_string(),
        ));
    }

    // Check if another participant is already screen sharing
    let other_sharing = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::IsScreenSharing.eq(true))
        .filter(participants::Column::Id.ne(participant.id))
        .one(&state.db)
        .await?;

    if other_sharing.is_some() {
        return Err(ApiError::Conflict(
            "Another participant is already screen sharing".to_string(),
        ));
    }

    // Start screen sharing
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.is_screen_sharing = Set(true);
    participant_update.update(&state.db).await?;

    // Log the screen share start event
    let now = now_utc();
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant.id)),
        event_type: Set(EventType::ScreenShareStart),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "display_name": participant.display_name
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ScreenShareResponse {
        success: true,
        participant_id: participant.id,
        is_screen_sharing: true,
        message: "Screen sharing started successfully".to_string(),
    }))
}

/// Stop screen sharing
#[utoipa::path(
    post,
    path = "/api/v1/meetings/{meeting_id}/screen-share/stop",
    tag = "Media Control",
    params(
        ("meeting_id" = Uuid, Path, description = "Meeting UUID")
    ),
    responses(
        (status = 200, description = "Screen sharing stopped successfully", body = ScreenShareResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Meeting or participant not found"),
        (status = 409, description = "Not currently screen sharing")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn stop_screen_share(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(meeting_id): Path<Uuid>,
) -> ApiResult<Json<ScreenShareResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    // Verify meeting exists
    let _meeting = Meetings::find_by_id(meeting_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Meeting not found".to_string()))?;

    // Find user's participant record
    let participant = Participants::find()
        .filter(participants::Column::MeetingId.eq(meeting_id))
        .filter(participants::Column::UserId.eq(user_id))
        .filter(participants::Column::Status.eq(ParticipantStatus::Joined))
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::NotFound("You are not a participant in this meeting".to_string())
        })?;

    // Check if currently screen sharing
    if !participant.is_screen_sharing {
        return Err(ApiError::Conflict(
            "You are not currently screen sharing".to_string(),
        ));
    }

    // Stop screen sharing
    let mut participant_update: participants::ActiveModel = participant.clone().into();
    participant_update.is_screen_sharing = Set(false);
    participant_update.update(&state.db).await?;

    // Log the screen share stop event
    let now = now_utc();
    let session_log = session_logs::ActiveModel {
        id: Set(Uuid::new_v4()),
        meeting_id: Set(meeting_id),
        participant_id: Set(Some(participant.id)),
        event_type: Set(EventType::ScreenShareEnd),
        event_time: Set(now),
        metadata: Set(Some(serde_json::json!({
            "display_name": participant.display_name
        }))),
    };
    session_log.insert(&state.db).await?;

    Ok(Json(ScreenShareResponse {
        success: true,
        participant_id: participant.id,
        is_screen_sharing: false,
        message: "Screen sharing stopped successfully".to_string(),
    }))
}

