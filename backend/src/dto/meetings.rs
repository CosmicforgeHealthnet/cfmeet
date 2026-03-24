//! Meeting DTOs
//!
//! Data Transfer Objects for meeting-related API endpoints.
//!
//! All stored times are in UTC. Frontend sends the user's IANA timezone
//! (e.g., "Africa/Lagos") and the backend converts to UTC before storage.
//! All response times are formatted as UTC with Z suffix: "2026-01-25T14:00:00Z"

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Request to create an instant public meeting
/// Meeting starts immediately and lasts 1 hour
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct InstantMeetingRequest {
    /// Optional title (defaults to "Instant Meeting")
    #[validate(length(
        max = 200,
        message = "Title must not exceed 200 characters"
    ))]
    pub title: Option<String>,
}

/// Response for instant meeting creation (includes join info)
#[derive(Debug, Serialize, ToSchema)]
pub struct InstantMeetingResponse {
    pub id: Uuid,
    pub meeting_identifier: String,
    pub host_id: Uuid,
    pub title: String,
    pub is_private: bool,
    /// Start time in UTC
    pub start_time: String,
    /// End time in UTC (1 hour from start)
    pub end_time: String,
    pub status: String,
    pub join_url: String,
    pub created_at: String,
    /// Participant ID for the host
    pub participant_id: Uuid,
    /// LiveKit join token for immediate connection
    pub join_token: String,
    /// LiveKit server URL
    pub livekit_url: String,
    /// Room name for LiveKit
    pub room_name: String,
}

/// API response wrapper for instant meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct InstantMeetingApiResponse {
    pub success: bool,
    pub data: InstantMeetingResponse,
}

/// Request to create a new meeting
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct CreateMeetingRequest {
    #[validate(length(
        min = 1,
        max = 200,
        message = "Title must be between 1 and 200 characters"
    ))]
    pub title: String,

    /// Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T14:00:00")]
    pub start_time: NaiveDateTime,

    /// End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T15:00:00")]
    pub end_time: Option<NaiveDateTime>,

    /// IANA timezone of the user (e.g., "Africa/Lagos", "America/New_York")
    #[schema(example = "Africa/Lagos")]
    pub timezone: String,

    #[serde(default)]
    pub is_private: Option<bool>,

    pub metadata: Option<JsonValue>,
}

/// Request to update an existing meeting
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdateMeetingRequest {
    #[validate(length(
        min = 1,
        max = 200,
        message = "Title must be between 1 and 200 characters"
    ))]
    pub title: Option<String>,

    /// Start time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T14:00:00")]
    pub start_time: Option<NaiveDateTime>,

    /// End time in the user's local timezone. Format: "YYYY-MM-DDTHH:MM:SS"
    #[schema(example = "2026-01-25T15:00:00")]
    pub end_time: Option<NaiveDateTime>,

    /// IANA timezone of the user (e.g., "Africa/Lagos"). Required when updating times.
    #[schema(example = "Africa/Lagos")]
    pub timezone: Option<String>,

    pub is_private: Option<bool>,

    pub metadata: Option<JsonValue>,
}



/// Request to join a meeting
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct JoinMeetingRequest {
    /// Optional user ID for authenticated users (null for guests)
    pub user_id: Option<Uuid>,

    #[validate(length(
        min = 1,
        max = 100,
        message = "Display name must be between 1 and 100 characters"
    ))]
    pub display_name: String,
}

/// Meeting response structure
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingResponse {
    pub id: Uuid,
    pub meeting_identifier: String,
    pub host_id: Uuid,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    pub is_private: bool,
    /// Start time in UTC. Format: "2026-01-25T14:00:00Z"
    pub start_time: String,
    /// End time in UTC. Format: "2026-01-25T15:00:00Z"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    pub status: String,
    /// Join URL for the hosted UI (e.g., https://meet.cosmicforge.com/join/ABCD1234)
    pub join_url: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Participant response structure
#[derive(Debug, Serialize, ToSchema)]
pub struct ParticipantResponse {
    pub participant_id: Uuid,
    pub meeting_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    pub role: String,
    pub display_name: String,
    pub status: String,
    pub join_time: String,
}

/// Response for joining a meeting (includes join token)
#[derive(Debug, Serialize, ToSchema)]
pub struct JoinMeetingResponse {
    pub meeting_id: Uuid,
    pub participant_id: Uuid,
    pub role: String,
    /// Short-lived token for WebSocket/RTC session
    pub join_token: String,
    /// LiveKit server URL
    pub livekit_url: String,
    /// Room name for LiveKit
    pub room_name: String,
    /// JWT Access Token (issued for guests)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// JWT Refresh Token (issued for guests)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

/// Response for ending a meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct EndMeetingResponse {
    pub status: String,
}

/// Query parameters for listing meetings
#[derive(Debug, Deserialize)]
pub struct ListMeetingsQuery {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    pub status: Option<String>,
}

// Swagger response wrappers

/// API response wrapper for meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingApiResponse {
    pub success: bool,
    pub data: MeetingResponse,
}

/// API response wrapper for join meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct JoinMeetingApiResponse {
    pub success: bool,
    pub data: JoinMeetingResponse,
}

/// API response wrapper for end meeting
#[derive(Debug, Serialize, ToSchema)]
pub struct EndMeetingApiResponse {
    pub success: bool,
    pub data: EndMeetingResponse,
}

// ============================================================================
// PUBLIC MEETING INFO DTOs
// ============================================================================

/// Public meeting info response (no authentication required)
/// Used by the hosted UI to display meeting details before joining
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicMeetingInfoResponse {
    pub meeting_identifier: String,
    pub title: String,
    pub is_private: bool,
    /// Start time in UTC. Format: "2026-01-25T14:00:00Z"
    pub start_time: String,
    /// End time in UTC. Format: "2026-01-25T15:00:00Z"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    pub status: String,
    /// Join URL for the hosted UI
    pub join_url: String,
}

/// API response wrapper for public meeting info
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicMeetingInfoApiResponse {
    pub success: bool,
    pub data: PublicMeetingInfoResponse,
}

/// Pagination metadata
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingPaginationMeta {
    pub page: u64,
    pub limit: u64,
    pub total: u64,
    pub total_pages: u64,
}

/// Paginated response for meetings list
#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedMeetingResponse {
    pub success: bool,
    pub data: Vec<MeetingResponse>,
    pub meta: MeetingPaginationMeta,
}

// ============================================================================
// PARTICIPANT MANAGEMENT DTOs
// ============================================================================

/// Detailed participant response with media states
#[derive(Debug, Serialize, ToSchema)]
pub struct DetailedParticipantResponse {
    pub participant_id: Uuid,
    pub meeting_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    pub role: String,
    pub display_name: String,
    pub status: String,
    pub join_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leave_time: Option<String>,
    pub is_muted: bool,
    pub is_video_on: bool,
    pub is_screen_sharing: bool,
}

/// Response for listing participants
#[derive(Debug, Serialize, ToSchema)]
pub struct ParticipantsListResponse {
    pub success: bool,
    pub data: Vec<DetailedParticipantResponse>,
}

/// Response for participant action (kick, admit, deny)
#[derive(Debug, Serialize, ToSchema)]
pub struct ParticipantActionResponse {
    pub success: bool,
    pub message: String,
}

// ============================================================================
// WAITING ROOM DTOs
// ============================================================================

/// Waiting participant response
#[derive(Debug, Serialize, ToSchema)]
pub struct WaitingParticipantResponse {
    pub participant_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    pub display_name: String,
    pub join_time: String,
}

/// Response for listing waiting participants
#[derive(Debug, Serialize, ToSchema)]
pub struct WaitingListResponse {
    pub success: bool,
    pub data: Vec<WaitingParticipantResponse>,
}

// ============================================================================
// MEDIA CONTROL DTOs
// ============================================================================

/// Request to update audio state
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAudioRequest {
    pub is_muted: bool,
}

/// Request to update video state
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateVideoRequest {
    pub is_video_on: bool,
}

/// Response for media state update
#[derive(Debug, Serialize, ToSchema)]
pub struct MediaStateResponse {
    pub success: bool,
    pub participant_id: Uuid,
    pub is_muted: bool,
    pub is_video_on: bool,
    pub is_screen_sharing: bool,
}

/// Response for screen share action
#[derive(Debug, Serialize, ToSchema)]
pub struct ScreenShareResponse {
    pub success: bool,
    pub participant_id: Uuid,
    pub is_screen_sharing: bool,
    pub message: String,
}

// ============================================================================
// CHAT DTOs
// ============================================================================

/// Request to send a chat message
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct SendChatMessageRequest {
    /// The participant sending the message
    pub participant_id: Uuid,

    #[validate(length(
        min = 1,
        max = 2000,
        message = "Message must be between 1 and 2000 characters"
    ))]
    pub message: String,
}

/// Single chat message response
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatMessageResponse {
    pub id: Uuid,
    pub participant_id: Uuid,
    pub display_name: String,
    pub message: String,
    pub created_at: String,
}

/// Response for sending a chat message
#[derive(Debug, Serialize, ToSchema)]
pub struct SendChatMessageApiResponse {
    pub success: bool,
    pub data: ChatMessageResponse,
}

/// Response for listing chat messages
#[derive(Debug, Serialize, ToSchema)]
pub struct ChatMessagesListResponse {
    pub success: bool,
    pub data: Vec<ChatMessageResponse>,
}

/// Query parameters for chat messages
#[derive(Debug, Deserialize)]
pub struct ChatMessagesQuery {
    /// Filter messages after this timestamp (UTC). Format: "YYYY-MM-DDTHH:MM:SS"
    pub after: Option<NaiveDateTime>,
    /// Limit number of messages (default: 100, max: 500)
    pub limit: Option<u64>,
}
