use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use utoipa::ToSchema;

use crate::middleware::request_id::current_request_id;

/// Stable, semantic public error codes for the Phase 2 meeting domain (spec v1.1 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    MeetingNotFound,
    MeetingAccessDenied,
    MeetingPasscodeRequired,
    MeetingPasscodeInvalid,
    MeetingVersionConflict,
    MeetingCapacityReached,
    InvitationInvalid,
    InvitationExpired,
    ConferenceNotFound,
    ConferenceNotLive,
    ConferenceLocked,
    ConferenceStateConflict,
    WaitingRoomRequired,
    ParticipantNotFound,
    ParticipantBanned,
    ParticipantStateConflict,
    PermissionDenied,
    AuthenticationRequired,
    ValidationError,
    RateLimited,
    FeatureDisabled,
    RtcProviderUnavailable,
    RecordingNotAllowed,
    RevocationCheckUnavailable,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::MeetingNotFound => "MEETING_NOT_FOUND",
            ErrorCode::MeetingAccessDenied => "MEETING_ACCESS_DENIED",
            ErrorCode::MeetingPasscodeRequired => "MEETING_PASSCODE_REQUIRED",
            ErrorCode::MeetingPasscodeInvalid => "MEETING_PASSCODE_INVALID",
            ErrorCode::MeetingVersionConflict => "MEETING_VERSION_CONFLICT",
            ErrorCode::MeetingCapacityReached => "MEETING_CAPACITY_REACHED",
            ErrorCode::InvitationInvalid => "INVITATION_INVALID",
            ErrorCode::InvitationExpired => "INVITATION_EXPIRED",
            ErrorCode::ConferenceNotFound => "CONFERENCE_NOT_FOUND",
            ErrorCode::ConferenceNotLive => "CONFERENCE_NOT_LIVE",
            ErrorCode::ConferenceLocked => "CONFERENCE_LOCKED",
            ErrorCode::ConferenceStateConflict => "CONFERENCE_STATE_CONFLICT",
            ErrorCode::WaitingRoomRequired => "WAITING_ROOM_REQUIRED",
            ErrorCode::ParticipantNotFound => "PARTICIPANT_NOT_FOUND",
            ErrorCode::ParticipantBanned => "PARTICIPANT_BANNED",
            ErrorCode::ParticipantStateConflict => "PARTICIPANT_STATE_CONFLICT",
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
            ErrorCode::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::FeatureDisabled => "FEATURE_DISABLED",
            ErrorCode::RtcProviderUnavailable => "RTC_PROVIDER_UNAVAILABLE",
            ErrorCode::RecordingNotAllowed => "RECORDING_NOT_ALLOWED",
            ErrorCode::RevocationCheckUnavailable => "REVOCATION_CHECK_UNAVAILABLE",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ErrorCode::MeetingNotFound
            | ErrorCode::ConferenceNotFound
            | ErrorCode::ParticipantNotFound
            | ErrorCode::FeatureDisabled => StatusCode::NOT_FOUND,
            ErrorCode::MeetingAccessDenied
            | ErrorCode::PermissionDenied
            | ErrorCode::ParticipantBanned
            | ErrorCode::ConferenceLocked
            | ErrorCode::RecordingNotAllowed => StatusCode::FORBIDDEN,
            ErrorCode::MeetingPasscodeRequired
            | ErrorCode::MeetingPasscodeInvalid
            | ErrorCode::AuthenticationRequired => StatusCode::UNAUTHORIZED,
            ErrorCode::InvitationInvalid | ErrorCode::InvitationExpired => StatusCode::GONE,
            ErrorCode::MeetingVersionConflict
            | ErrorCode::ConferenceNotLive
            | ErrorCode::ConferenceStateConflict
            | ErrorCode::ParticipantStateConflict
            | ErrorCode::WaitingRoomRequired
            | ErrorCode::MeetingCapacityReached => StatusCode::CONFLICT,
            ErrorCode::ValidationError => StatusCode::BAD_REQUEST,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RtcProviderUnavailable | ErrorCode::RevocationCheckUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] sea_orm::DbErr),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal server error: {0}")]
    InternalError(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Too many requests: {0}")]
    TooManyRequests(String),

    /// Phase 2 semantic domain error with a stable public code.
    #[error("{code:?}: {message}")]
    Domain { code: ErrorCode, message: String },
}

impl ApiError {
    pub fn domain(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError::Domain {
            code,
            message: message.into(),
        }
    }

    /// The semantic code, when this is a domain error.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            ApiError::Domain { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match self {
            ApiError::DatabaseError(e) => {
                // Never expose SQL text / SeaORM internals to clients.
                tracing::error!("Database error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DATABASE_ERROR",
                    "A database error occurred".to_string(),
                )
            }
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg),
            ApiError::ValidationError(msg) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg),
            ApiError::InternalError(msg) => {
                tracing::error!("Internal error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "An internal error occurred".to_string(),
                )
            }
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            ApiError::TooManyRequests(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_REQUESTS", msg)
            }
            ApiError::Domain { code, message } => (code.status(), code.as_str(), message),
        };

        let body = Json(json!({
            "success": false,
            "error": {
                "code": error_code,
                "message": message,
                "requestId": current_request_id(),
            }
        }));

        (status, body).into_response()
    }
}

/// OpenAPI description of the public error envelope.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    pub success: bool,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    /// Stable semantic code, e.g. `MEETING_ACCESS_DENIED`.
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

pub type ApiResult<T> = Result<T, ApiError>;
