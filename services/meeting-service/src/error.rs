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
    // Part F
    MediaPermissionDenied,
    MediaLocked,
    ScreenShareNotAllowed,
    ScreenShareNotActive,
    ChatDisabled,
    PrivateChatDisabled,
    ChatParticipantRestricted,
    ReactionsDisabled,
    HandRaiseDisabled,
    BreakoutNotFound,
    BreakoutClosed,
    BreakoutAssignmentConflict,
    PollNotFound,
    PollNotOpen,
    PollAlreadyResponded,
    PollResponseInvalid,
    PollResultsNotVisible,
    QuestionNotFound,
    QnaDisabled,
    PollsDisabled,
    MessageNotFound,
    ConsentRequired,
    // Parts G–I
    RecordingDisabled,
    RecordingAlreadyActive,
    RecordingNotActive,
    RecordingStateConflict,
    RecordingProviderUnavailable,
    RecordingNotFound,
    ConsentDeclined,
    ConsentPolicyConflict,
    ArtifactNotFound,
    ArtifactNotReady,
    ArtifactDeleted,
    ArtifactAccessDenied,
    ArtifactStorageUnavailable,
    ArtifactIntegrityFailed,
    TranscriptionDisabled,
    TranscriptionNotAllowed,
    TranscriptionProviderUnavailable,
    TranscriptionNotFound,
    IntelligenceDisabled,
    IntelligenceNotAllowed,
    IntelligenceProviderUnavailable,
    RegistrationDisabled,
    RegistrationClosed,
    RegistrationNotFound,
    AdmissionTokenInvalid,
    BackstageAccessDenied,
    ProfilePolicyDenied,
    StageCapacityReached,
    QualitySampleInvalid,
    QualityRateLimited,
    OperationsAccessDenied,
    /// Body exceeds the route's size limit (e.g. quality samples).
    RequestTooLarge,
    // Parts J–L
    /// No `/api/v2` operation exists at this path.
    EndpointNotFound,
    /// The path exists but does not accept this HTTP method.
    MethodNotAllowed,
    /// Legacy (v1) meeting writes are retired (`FEATURE_LEGACY_MEETING_WRITE_DISABLED`);
    /// use the `/api/v2` equivalent.
    LegacyWriteDisabled,
    /// A required dependency (PostgreSQL) is unreachable; retry later. No state changed.
    ServiceUnavailable,
    /// Unexpected database failure (details are logged, never returned).
    DatabaseError,
    /// Unexpected internal failure (details are logged, never returned).
    InternalError,
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
            ErrorCode::MediaPermissionDenied => "MEDIA_PERMISSION_DENIED",
            ErrorCode::MediaLocked => "MEDIA_LOCKED",
            ErrorCode::ScreenShareNotAllowed => "SCREEN_SHARE_NOT_ALLOWED",
            ErrorCode::ScreenShareNotActive => "SCREEN_SHARE_NOT_ACTIVE",
            ErrorCode::ChatDisabled => "CHAT_DISABLED",
            ErrorCode::PrivateChatDisabled => "PRIVATE_CHAT_DISABLED",
            ErrorCode::ChatParticipantRestricted => "CHAT_PARTICIPANT_RESTRICTED",
            ErrorCode::ReactionsDisabled => "REACTIONS_DISABLED",
            ErrorCode::HandRaiseDisabled => "HAND_RAISE_DISABLED",
            ErrorCode::BreakoutNotFound => "BREAKOUT_NOT_FOUND",
            ErrorCode::BreakoutClosed => "BREAKOUT_CLOSED",
            ErrorCode::BreakoutAssignmentConflict => "BREAKOUT_ASSIGNMENT_CONFLICT",
            ErrorCode::PollNotFound => "POLL_NOT_FOUND",
            ErrorCode::PollNotOpen => "POLL_NOT_OPEN",
            ErrorCode::PollAlreadyResponded => "POLL_ALREADY_RESPONDED",
            ErrorCode::PollResponseInvalid => "POLL_RESPONSE_INVALID",
            ErrorCode::PollResultsNotVisible => "POLL_RESULTS_NOT_VISIBLE",
            ErrorCode::QuestionNotFound => "QUESTION_NOT_FOUND",
            ErrorCode::QnaDisabled => "QNA_DISABLED",
            ErrorCode::PollsDisabled => "POLLS_DISABLED",
            ErrorCode::MessageNotFound => "MESSAGE_NOT_FOUND",
            ErrorCode::ConsentRequired => "CONSENT_REQUIRED",
            ErrorCode::RecordingDisabled => "RECORDING_DISABLED",
            ErrorCode::RecordingAlreadyActive => "RECORDING_ALREADY_ACTIVE",
            ErrorCode::RecordingNotActive => "RECORDING_NOT_ACTIVE",
            ErrorCode::RecordingStateConflict => "RECORDING_STATE_CONFLICT",
            ErrorCode::RecordingProviderUnavailable => "RECORDING_PROVIDER_UNAVAILABLE",
            ErrorCode::RecordingNotFound => "RECORDING_NOT_FOUND",
            ErrorCode::ConsentDeclined => "CONSENT_DECLINED",
            ErrorCode::ConsentPolicyConflict => "CONSENT_POLICY_CONFLICT",
            ErrorCode::ArtifactNotFound => "ARTIFACT_NOT_FOUND",
            ErrorCode::ArtifactNotReady => "ARTIFACT_NOT_READY",
            ErrorCode::ArtifactDeleted => "ARTIFACT_DELETED",
            ErrorCode::ArtifactAccessDenied => "ARTIFACT_ACCESS_DENIED",
            ErrorCode::ArtifactStorageUnavailable => "ARTIFACT_STORAGE_UNAVAILABLE",
            ErrorCode::ArtifactIntegrityFailed => "ARTIFACT_INTEGRITY_FAILED",
            ErrorCode::TranscriptionDisabled => "TRANSCRIPTION_DISABLED",
            ErrorCode::TranscriptionNotAllowed => "TRANSCRIPTION_NOT_ALLOWED",
            ErrorCode::TranscriptionProviderUnavailable => "TRANSCRIPTION_PROVIDER_UNAVAILABLE",
            ErrorCode::TranscriptionNotFound => "TRANSCRIPTION_NOT_FOUND",
            ErrorCode::IntelligenceDisabled => "INTELLIGENCE_DISABLED",
            ErrorCode::IntelligenceNotAllowed => "INTELLIGENCE_NOT_ALLOWED",
            ErrorCode::IntelligenceProviderUnavailable => "INTELLIGENCE_PROVIDER_UNAVAILABLE",
            ErrorCode::RegistrationDisabled => "REGISTRATION_DISABLED",
            ErrorCode::RegistrationClosed => "REGISTRATION_CLOSED",
            ErrorCode::RegistrationNotFound => "REGISTRATION_NOT_FOUND",
            ErrorCode::AdmissionTokenInvalid => "ADMISSION_TOKEN_INVALID",
            ErrorCode::BackstageAccessDenied => "BACKSTAGE_ACCESS_DENIED",
            ErrorCode::ProfilePolicyDenied => "PROFILE_POLICY_DENIED",
            ErrorCode::StageCapacityReached => "STAGE_CAPACITY_REACHED",
            ErrorCode::QualitySampleInvalid => "QUALITY_SAMPLE_INVALID",
            ErrorCode::QualityRateLimited => "QUALITY_RATE_LIMITED",
            ErrorCode::OperationsAccessDenied => "OPERATIONS_ACCESS_DENIED",
            ErrorCode::RequestTooLarge => "REQUEST_TOO_LARGE",
            ErrorCode::EndpointNotFound => "ENDPOINT_NOT_FOUND",
            ErrorCode::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            ErrorCode::LegacyWriteDisabled => "LEGACY_WRITE_DISABLED",
            ErrorCode::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            ErrorCode::DatabaseError => "DATABASE_ERROR",
            ErrorCode::InternalError => "INTERNAL_ERROR",
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
            ErrorCode::MediaPermissionDenied
            | ErrorCode::MediaLocked
            | ErrorCode::ScreenShareNotAllowed
            | ErrorCode::ChatDisabled
            | ErrorCode::PrivateChatDisabled
            | ErrorCode::ChatParticipantRestricted
            | ErrorCode::ReactionsDisabled
            | ErrorCode::HandRaiseDisabled
            | ErrorCode::PollResultsNotVisible
            | ErrorCode::QnaDisabled
            | ErrorCode::PollsDisabled
            | ErrorCode::ConsentRequired => StatusCode::FORBIDDEN,
            ErrorCode::ScreenShareNotActive
            | ErrorCode::BreakoutClosed
            | ErrorCode::BreakoutAssignmentConflict
            | ErrorCode::PollNotOpen
            | ErrorCode::PollAlreadyResponded => StatusCode::CONFLICT,
            ErrorCode::BreakoutNotFound
            | ErrorCode::PollNotFound
            | ErrorCode::QuestionNotFound
            | ErrorCode::MessageNotFound => StatusCode::NOT_FOUND,
            ErrorCode::PollResponseInvalid => StatusCode::BAD_REQUEST,
            ErrorCode::RecordingDisabled => StatusCode::NOT_FOUND,
            ErrorCode::RecordingAlreadyActive => StatusCode::CONFLICT,
            ErrorCode::RecordingNotActive => StatusCode::CONFLICT,
            ErrorCode::RecordingStateConflict => StatusCode::CONFLICT,
            ErrorCode::RecordingProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RecordingNotFound => StatusCode::NOT_FOUND,
            ErrorCode::ConsentDeclined => StatusCode::FORBIDDEN,
            ErrorCode::ConsentPolicyConflict => StatusCode::CONFLICT,
            ErrorCode::ArtifactNotFound => StatusCode::NOT_FOUND,
            ErrorCode::ArtifactNotReady => StatusCode::CONFLICT,
            ErrorCode::ArtifactDeleted => StatusCode::GONE,
            ErrorCode::ArtifactAccessDenied => StatusCode::FORBIDDEN,
            ErrorCode::ArtifactStorageUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ArtifactIntegrityFailed => StatusCode::CONFLICT,
            ErrorCode::TranscriptionDisabled => StatusCode::NOT_FOUND,
            ErrorCode::TranscriptionNotAllowed => StatusCode::FORBIDDEN,
            ErrorCode::TranscriptionProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::TranscriptionNotFound => StatusCode::NOT_FOUND,
            ErrorCode::IntelligenceDisabled => StatusCode::NOT_FOUND,
            ErrorCode::IntelligenceNotAllowed => StatusCode::FORBIDDEN,
            ErrorCode::IntelligenceProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RegistrationDisabled => StatusCode::NOT_FOUND,
            ErrorCode::RegistrationClosed => StatusCode::CONFLICT,
            ErrorCode::RegistrationNotFound => StatusCode::NOT_FOUND,
            ErrorCode::AdmissionTokenInvalid => StatusCode::GONE,
            ErrorCode::BackstageAccessDenied => StatusCode::FORBIDDEN,
            ErrorCode::ProfilePolicyDenied => StatusCode::FORBIDDEN,
            ErrorCode::StageCapacityReached => StatusCode::CONFLICT,
            ErrorCode::QualitySampleInvalid => StatusCode::BAD_REQUEST,
            ErrorCode::QualityRateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::OperationsAccessDenied => StatusCode::FORBIDDEN,
            ErrorCode::RequestTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::EndpointNotFound => StatusCode::NOT_FOUND,
            ErrorCode::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ErrorCode::LegacyWriteDisabled => StatusCode::GONE,
            ErrorCode::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::DatabaseError | ErrorCode::InternalError => {
                StatusCode::INTERNAL_SERVER_ERROR
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

    /// `RATE_LIMITED` with retry semantics (`Retry-After` header + body field).
    #[error("RATE_LIMITED: {message}")]
    RateLimited {
        message: String,
        retry_after_secs: u64,
    },
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
            ApiError::RateLimited { .. } => Some(ErrorCode::RateLimited),
            _ => None,
        }
    }

    pub fn rate_limited(message: impl Into<String>, retry_after_secs: u64) -> Self {
        ApiError::RateLimited {
            message: message.into(),
            retry_after_secs: retry_after_secs.max(1),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::RateLimited {
            message,
            retry_after_secs,
        } = self
        {
            let body = Json(json!({
                "success": false,
                "error": {
                    "code": ErrorCode::RateLimited.as_str(),
                    "message": message,
                    "retryAfterSeconds": retry_after_secs,
                    "requestId": current_request_id(),
                }
            }));
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(
                    axum::http::header::RETRY_AFTER,
                    retry_after_secs.to_string(),
                )],
                body,
            )
                .into_response();
        }
        let (status, error_code, message) = match self {
            ApiError::DatabaseError(e) => {
                // Never expose SQL text / SeaORM internals / DSNs to clients.
                tracing::error!("Database error: {:?}", e);
                if is_connectivity_error(&e) {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        ErrorCode::ServiceUnavailable.as_str(),
                        "The service is temporarily unavailable; retry later".to_string(),
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ErrorCode::DatabaseError.as_str(),
                        "A database error occurred".to_string(),
                    )
                }
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
                    ErrorCode::InternalError.as_str(),
                    "An internal error occurred".to_string(),
                )
            }
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            ApiError::TooManyRequests(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_REQUESTS", msg)
            }
            ApiError::Domain { code, message } => (code.status(), code.as_str(), message),
            ApiError::RateLimited { .. } => unreachable!("handled above"),
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

/// PostgreSQL unreachable (pool exhausted / connection refused / connection lost):
/// a retryable outage, reported as `503 SERVICE_UNAVAILABLE`. The authority is
/// unavailable, so nothing was decided — callers retry; nothing falls back to Redis.
fn is_connectivity_error(e: &sea_orm::DbErr) -> bool {
    use sea_orm::{sqlx::Error as SqlxError, DbErr, RuntimeErr};
    match e {
        DbErr::ConnectionAcquire(_) | DbErr::Conn(_) => true,
        DbErr::Query(RuntimeErr::SqlxError(x)) | DbErr::Exec(RuntimeErr::SqlxError(x)) => matches!(
            x,
            SqlxError::Io(_) | SqlxError::PoolTimedOut | SqlxError::PoolClosed
        ),
        _ => false,
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
    /// Stable semantic code, e.g. `MEETING_ACCESS_DENIED`. Every `/api/v2` error uses a
    /// value of [`ErrorCode`]; legacy `/api/v1` endpoints may additionally return their
    /// historical generic codes (`NOT_FOUND`, `UNAUTHORIZED`, …).
    pub code: ErrorCode,
    /// Safe, human-readable message (never SQL, provider bodies, secrets or paths).
    pub message: String,
    /// Server-generated correlation ID (also in the `x-request-id` response header).
    pub request_id: Option<String>,
    /// Present on `RATE_LIMITED` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    async fn render(e: ApiError) -> (StatusCode, axum::http::HeaderMap, Value) {
        let resp = e.into_response();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn domain_errors_serialize_to_the_standard_envelope() {
        for code in [
            ErrorCode::MeetingAccessDenied,
            ErrorCode::ConferenceNotLive,
            ErrorCode::RtcProviderUnavailable,
            ErrorCode::EndpointNotFound,
            ErrorCode::LegacyWriteDisabled,
        ] {
            let (status, _, v) = render(ApiError::domain(code, "safe message")).await;
            assert_eq!(status, code.status());
            assert_eq!(v["success"], false);
            assert_eq!(v["error"]["code"], code.as_str());
            assert_eq!(v["error"]["message"], "safe message");
            assert!(v["error"].as_object().unwrap().contains_key("requestId"));
            assert_eq!(v["error"].as_object().unwrap().len(), 3, "{v}");
        }
    }

    #[tokio::test]
    async fn rate_limit_carries_retry_semantics() {
        let (status, headers, v) = render(ApiError::rate_limited("slow down", 0)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            headers[axum::http::header::RETRY_AFTER],
            "1",
            "retry-after floored at 1s"
        );
        assert_eq!(v["error"]["code"], "RATE_LIMITED");
        assert_eq!(v["error"]["retryAfterSeconds"], 1);
    }

    #[tokio::test]
    async fn internal_and_database_details_never_reach_the_client() {
        let (status, _, v) = render(ApiError::InternalError(
            "panicked at /home/build/src/secret.rs: postgres://user:pw@db".into(),
        ))
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], "INTERNAL_ERROR");
        assert!(!v.to_string().contains("postgres://") && !v.to_string().contains("/home/"));

        let (status, _, v) = render(ApiError::DatabaseError(sea_orm::DbErr::Custom(
            "duplicate key value violates unique constraint \"uq_secret\"".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], "DATABASE_ERROR");
        assert!(!v.to_string().contains("uq_secret"));

        let (status, _, v) = render(ApiError::DatabaseError(sea_orm::DbErr::ConnectionAcquire(
            sea_orm::ConnAcquireErr::Timeout,
        )))
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["error"]["code"], "SERVICE_UNAVAILABLE");
    }

    #[test]
    fn every_error_code_has_a_stable_string_and_client_or_server_status() {
        let all = [
            ErrorCode::MeetingNotFound,
            ErrorCode::PermissionDenied,
            ErrorCode::ValidationError,
            ErrorCode::RateLimited,
            ErrorCode::FeatureDisabled,
            ErrorCode::ServiceUnavailable,
            ErrorCode::DatabaseError,
            ErrorCode::InternalError,
            ErrorCode::MethodNotAllowed,
        ];
        for c in all {
            let s = c.as_str();
            assert!(
                s.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'),
                "{s}"
            );
            assert_eq!(serde_json::to_value(c).unwrap(), serde_json::json!(s));
            assert!(c.status().is_client_error() || c.status().is_server_error());
        }
    }
}
