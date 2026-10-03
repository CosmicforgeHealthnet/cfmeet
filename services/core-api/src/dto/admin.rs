use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

// ============================================================================
// ADMIN USER DTOs
// ============================================================================

/// Admin view of a user
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserResponse {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub role: String,
    pub status: String,
    pub auth_type: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login: Option<String>,
}

/// Request to update a user (admin only)
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct AdminUpdateUserRequest {
    /// Role: user, admin, guest
    pub role: Option<String>,
    /// Status: pending_verification, active, inactive
    pub status: Option<String>,
}

/// Pagination metadata for admin endpoints
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminPaginationMeta {
    pub page: u64,
    pub limit: u64,
    pub total: u64,
    pub total_pages: u64,
}

/// Paginated response for admin user list
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserListResponse {
    pub success: bool,
    pub data: Vec<AdminUserResponse>,
    pub meta: AdminPaginationMeta,
}

/// API response wrapper for single admin user
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserApiResponse {
    pub success: bool,
    pub data: AdminUserResponse,
}

/// Query parameters for listing users
#[derive(Debug, Deserialize)]
pub struct AdminListUsersQuery {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    /// Filter by status: pending_verification, active, inactive
    pub status: Option<String>,
    /// Filter by role: user, admin, guest
    pub role: Option<String>,
}

// ============================================================================
// ADMIN MEETING DTOs
// ============================================================================

/// Admin view of a meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminMeetingResponse {
    pub id: Uuid,
    pub meeting_identifier: String,
    pub title: String,
    pub host_id: Uuid,
    pub host_username: String,
    pub participant_count: i64,
    pub status: String,
    pub is_private: bool,
    pub start_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    pub created_at: String,
}

/// Paginated response for admin meeting list
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminMeetingListResponse {
    pub success: bool,
    pub data: Vec<AdminMeetingResponse>,
    pub meta: AdminPaginationMeta,
}

/// Query parameters for listing meetings (admin)
#[derive(Debug, Deserialize)]
pub struct AdminListMeetingsQuery {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    /// Filter by status: scheduled, ongoing, ended, cancelled
    pub status: Option<String>,
    /// Filter by host user ID
    pub host_id: Option<Uuid>,
}

// ============================================================================
// ADMIN LOGS DTOs
// ============================================================================

/// Admin view of a session log entry
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminLogResponse {
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub meeting_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_name: Option<String>,
    pub event_type: String,
    pub event_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

/// Paginated response for admin logs
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminLogListResponse {
    pub success: bool,
    pub data: Vec<AdminLogResponse>,
    pub meta: AdminPaginationMeta,
}

/// Query parameters for listing logs
#[derive(Debug, Deserialize)]
pub struct AdminListLogsQuery {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    /// Filter by meeting ID
    pub meeting_id: Option<Uuid>,
    /// Filter by event type
    pub event_type: Option<String>,
}
