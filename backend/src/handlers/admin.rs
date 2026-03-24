//! Admin Handlers
//!
//! HTTP handlers for admin-only operations.

use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;
use validator::Validate;

use crate::{
    dto::{
        AdminListLogsQuery, AdminListMeetingsQuery, AdminListUsersQuery,
        AdminLogListResponse, AdminLogResponse, AdminMeetingListResponse, AdminMeetingResponse,
        AdminPaginationMeta, AdminUpdateUserRequest, AdminUserApiResponse, AdminUserListResponse,
        AdminUserResponse,
    },
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    models::{
        meetings::{self, Entity as Meetings},
        participants::{self, Entity as Participants},
        session_logs::{self, Entity as SessionLogs, EventType},
        users::{self, AuthType, Entity as Users, UserRole, UserStatus},
    },
    state::AppState,
    utils::now_utc,
};

/// Check if the user has admin role
fn require_admin(auth_context: &AuthContext) -> ApiResult<()> {
    if auth_context.role != UserRole::Admin {
        return Err(ApiError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}

/// Format datetime to ISO 8601 UTC string
fn format_datetime(dt: chrono::NaiveDateTime) -> String {
    format!("{}Z", dt.format("%Y-%m-%dT%H:%M:%S"))
}

/// Format user role enum to string
fn format_role(role: &UserRole) -> String {
    match role {
        UserRole::User => "user".to_string(),
        UserRole::Admin => "admin".to_string(),
        UserRole::Guest => "guest".to_string(),
    }
}

/// Format user status enum to string
fn format_status(status: &UserStatus) -> String {
    match status {
        UserStatus::PendingVerification => "pending_verification".to_string(),
        UserStatus::Active => "active".to_string(),
        UserStatus::Inactive => "inactive".to_string(),
    }
}

/// Format auth type enum to string
fn format_auth_type(auth_type: &AuthType) -> String {
    match auth_type {
        AuthType::Local => "local".to_string(),
        AuthType::Google => "google".to_string(),
    }
}

/// Format meeting status enum to string
fn format_meeting_status(status: &meetings::MeetingStatus) -> String {
    match status {
        meetings::MeetingStatus::Scheduled => "scheduled".to_string(),
        meetings::MeetingStatus::Ongoing => "ongoing".to_string(),
        meetings::MeetingStatus::Ended => "ended".to_string(),
        meetings::MeetingStatus::Cancelled => "cancelled".to_string(),
    }
}

/// Format event type enum to string
fn format_event_type(event_type: &EventType) -> String {
    match event_type {
        EventType::MeetingStart => "meeting_start".to_string(),
        EventType::MeetingEnd => "meeting_end".to_string(),
        EventType::ParticipantJoin => "participant_join".to_string(),
        EventType::ParticipantLeave => "participant_leave".to_string(),
        EventType::RoleChange => "role_change".to_string(),
        EventType::ScreenShareStart => "screen_share_start".to_string(),
        EventType::ScreenShareEnd => "screen_share_end".to_string(),
        EventType::MediaToggle => "media_toggle".to_string(),
    }
}

/// Parse user role string to enum
fn parse_role(role: &str) -> Result<UserRole, ApiError> {
    match role {
        "user" => Ok(UserRole::User),
        "admin" => Ok(UserRole::Admin),
        "guest" => Ok(UserRole::Guest),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid role: {}. Valid roles: user, admin, guest",
            role
        ))),
    }
}

/// Parse user status string to enum
fn parse_status(status: &str) -> Result<UserStatus, ApiError> {
    match status {
        "pending_verification" => Ok(UserStatus::PendingVerification),
        "active" => Ok(UserStatus::Active),
        "inactive" => Ok(UserStatus::Inactive),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid status: {}. Valid statuses: pending_verification, active, inactive",
            status
        ))),
    }
}

// ============================================================================
// USER MANAGEMENT
// ============================================================================

/// List all users (admin only)
#[utoipa::path(
    get,
    path = "/api/v1/admin/users",
    tag = "Admin",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("limit" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)"),
        ("status" = Option<String>, Query, description = "Filter by status: pending_verification, active, inactive"),
        ("role" = Option<String>, Query, description = "Filter by role: user, admin, guest")
    ),
    responses(
        (status = 200, description = "List of users", body = AdminUserListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Admin access required")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Query(query): Query<AdminListUsersQuery>,
) -> ApiResult<Json<AdminUserListResponse>> {
    require_admin(&auth_context)?;

    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).min(100).max(1);

    let mut select = Users::find().order_by_desc(users::Column::CreatedAt);

    if let Some(status) = &query.status {
        let status_enum = parse_status(status)?;
        select = select.filter(users::Column::Status.eq(status_enum));
    }

    if let Some(role) = &query.role {
        let role_enum = parse_role(role)?;
        select = select.filter(users::Column::Role.eq(role_enum));
    }

    let paginator = select.paginate(&state.db, limit);
    let total = paginator.num_items().await?;
    let total_pages = paginator.num_pages().await?;
    let users_list = paginator.fetch_page(page - 1).await?;

    let data: Vec<AdminUserResponse> = users_list
        .into_iter()
        .map(|u| AdminUserResponse {
            id: u.id,
            username: u.username,
            email: u.email,
            role: format_role(&u.role),
            status: format_status(&u.status),
            auth_type: format_auth_type(&u.auth_type),
            created_at: format_datetime(u.created_at),
            last_login: u.last_login.map(format_datetime),
        })
        .collect();

    Ok(Json(AdminUserListResponse {
        success: true,
        data,
        meta: AdminPaginationMeta {
            page,
            limit,
            total,
            total_pages,
        },
    }))
}

/// Update a user (admin only)
#[utoipa::path(
    patch,
    path = "/api/v1/admin/users/{id}",
    tag = "Admin",
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    request_body = AdminUpdateUserRequest,
    responses(
        (status = 200, description = "User updated successfully", body = AdminUserApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Admin access required or cannot modify own role"),
        (status = 404, description = "User not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Json(payload): Json<AdminUpdateUserRequest>,
) -> ApiResult<Json<AdminUserApiResponse>> {
    require_admin(&auth_context)?;

    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    let user = Users::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;

    // Prevent admin from changing their own role
    if let Some(new_role) = &payload.role {
        if id == auth_context.user_id {
            return Err(ApiError::Forbidden(
                "Cannot change your own role".to_string(),
            ));
        }

        // Parse to validate
        let _ = parse_role(new_role)?;
    }

    let mut user: users::ActiveModel = user.into();

    if let Some(role) = payload.role {
        user.role = Set(parse_role(&role)?);
    }

    if let Some(status) = payload.status {
        user.status = Set(parse_status(&status)?);
    }

    user.updated_at = Set(now_utc());
    let user = user.update(&state.db).await?;

    let response = AdminUserResponse {
        id: user.id,
        username: user.username,
        email: user.email,
        role: format_role(&user.role),
        status: format_status(&user.status),
        auth_type: format_auth_type(&user.auth_type),
        created_at: format_datetime(user.created_at),
        last_login: user.last_login.map(format_datetime),
    };

    Ok(Json(AdminUserApiResponse {
        success: true,
        data: response,
    }))
}

// ============================================================================
// MEETING MANAGEMENT
// ============================================================================

/// List all meetings (admin only)
#[utoipa::path(
    get,
    path = "/api/v1/admin/meetings",
    tag = "Admin",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("limit" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)"),
        ("status" = Option<String>, Query, description = "Filter by status: scheduled, ongoing, ended, cancelled"),
        ("host_id" = Option<Uuid>, Query, description = "Filter by host user ID")
    ),
    responses(
        (status = 200, description = "List of meetings", body = AdminMeetingListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Admin access required")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_meetings(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Query(query): Query<AdminListMeetingsQuery>,
) -> ApiResult<Json<AdminMeetingListResponse>> {
    require_admin(&auth_context)?;

    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).min(100).max(1);

    let mut select = Meetings::find().order_by_desc(meetings::Column::CreatedAt);

    if let Some(status) = &query.status {
        let status_enum = match status.as_str() {
            "scheduled" => meetings::MeetingStatus::Scheduled,
            "ongoing" => meetings::MeetingStatus::Ongoing,
            "ended" => meetings::MeetingStatus::Ended,
            "cancelled" => meetings::MeetingStatus::Cancelled,
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "Invalid status: {}. Valid statuses: scheduled, ongoing, ended, cancelled",
                    status
                )))
            }
        };
        select = select.filter(meetings::Column::Status.eq(status_enum));
    }

    if let Some(host_id) = query.host_id {
        select = select.filter(meetings::Column::HostId.eq(host_id));
    }

    let paginator = select.paginate(&state.db, limit);
    let total = paginator.num_items().await?;
    let total_pages = paginator.num_pages().await?;
    let meetings_list = paginator.fetch_page(page - 1).await?;

    // Get participant counts and host usernames
    let mut data: Vec<AdminMeetingResponse> = Vec::new();

    for meeting in meetings_list {
        // Get participant count
        let participant_count = Participants::find()
            .filter(participants::Column::MeetingId.eq(meeting.id))
            .filter(participants::Column::Status.eq(participants::ParticipantStatus::Joined))
            .count(&state.db)
            .await? as i64;

        // Get host username
        let host_username = Users::find_by_id(meeting.host_id)
            .one(&state.db)
            .await?
            .map(|u| u.username)
            .unwrap_or_else(|| "Unknown".to_string());

        data.push(AdminMeetingResponse {
            id: meeting.id,
            meeting_identifier: meeting.meeting_identifier,
            title: meeting.title,
            host_id: meeting.host_id,
            host_username,
            participant_count,
            status: format_meeting_status(&meeting.status),
            is_private: meeting.is_private,
            start_time: format_datetime(meeting.start_time),
            end_time: meeting.end_time.map(format_datetime),
            created_at: format_datetime(meeting.created_at),
        });
    }

    Ok(Json(AdminMeetingListResponse {
        success: true,
        data,
        meta: AdminPaginationMeta {
            page,
            limit,
            total,
            total_pages,
        },
    }))
}

// ============================================================================
// LOGS
// ============================================================================

/// List session logs (admin only)
#[utoipa::path(
    get,
    path = "/api/v1/admin/logs",
    tag = "Admin",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("limit" = Option<u64>, Query, description = "Items per page (default: 20, max: 100)"),
        ("meeting_id" = Option<Uuid>, Query, description = "Filter by meeting ID"),
        ("event_type" = Option<String>, Query, description = "Filter by event type")
    ),
    responses(
        (status = 200, description = "List of session logs", body = AdminLogListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Admin access required")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_logs(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Query(query): Query<AdminListLogsQuery>,
) -> ApiResult<Json<AdminLogListResponse>> {
    require_admin(&auth_context)?;

    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).min(100).max(1);

    let mut select = SessionLogs::find().order_by_desc(session_logs::Column::EventTime);

    if let Some(meeting_id) = query.meeting_id {
        select = select.filter(session_logs::Column::MeetingId.eq(meeting_id));
    }

    if let Some(event_type) = &query.event_type {
        let event_type_enum = match event_type.as_str() {
            "meeting_start" => EventType::MeetingStart,
            "meeting_end" => EventType::MeetingEnd,
            "participant_join" => EventType::ParticipantJoin,
            "participant_leave" => EventType::ParticipantLeave,
            "role_change" => EventType::RoleChange,
            "screen_share_start" => EventType::ScreenShareStart,
            "screen_share_end" => EventType::ScreenShareEnd,
            "media_toggle" => EventType::MediaToggle,
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "Invalid event type: {}",
                    event_type
                )))
            }
        };
        select = select.filter(session_logs::Column::EventType.eq(event_type_enum));
    }

    let paginator = select.paginate(&state.db, limit);
    let total = paginator.num_items().await?;
    let total_pages = paginator.num_pages().await?;
    let logs_list = paginator.fetch_page(page - 1).await?;

    // Enrich logs with meeting titles and participant names
    let mut data: Vec<AdminLogResponse> = Vec::new();

    for log in logs_list {
        // Get meeting title
        let meeting_title = Meetings::find_by_id(log.meeting_id)
            .one(&state.db)
            .await?
            .map(|m| m.title)
            .unwrap_or_else(|| "Unknown".to_string());

        // Get participant name if available
        let participant_name = if let Some(participant_id) = log.participant_id {
            Participants::find_by_id(participant_id)
                .one(&state.db)
                .await?
                .map(|p| p.display_name)
        } else {
            None
        };

        data.push(AdminLogResponse {
            id: log.id,
            meeting_id: log.meeting_id,
            meeting_title,
            participant_id: log.participant_id,
            participant_name,
            event_type: format_event_type(&log.event_type),
            event_time: format_datetime(log.event_time),
            metadata: log.metadata,
        });
    }

    Ok(Json(AdminLogListResponse {
        success: true,
        data,
        meta: AdminPaginationMeta {
            page,
            limit,
            total,
            total_pages,
        },
    }))
}
