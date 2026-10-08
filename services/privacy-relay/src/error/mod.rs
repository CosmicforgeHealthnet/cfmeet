//! Error model of the Privacy Relay service.
//!
//! Every error has the platform envelope
//! `{ "error": { "code": "CONTEXT_VERSION_CONFLICT", "message": "…", "requestId": "req_…" } }`.
//! Codes are stable (contract `cfmeet.privacy-relay.http-api`); messages are fixed, safe
//! strings. Responses never contain database errors, service-token contents, external
//! context references or whether a resource of another tenant / application /
//! environment exists (a foreign id is exactly NOT_FOUND).

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::DbErr;
use serde_json::json;

use crate::middleware::current_request_id;

/// Stable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ValidationError,
    NotFound,
    MethodNotAllowed,
    ContextNotFound,
    SessionNotFound,
    ContextVersionConflict,
    SessionVersionConflict,
    ContextReferenceConflict,
    ContextNotActive,
    ContextExpired,
    ContextRevoked,
    ContextSuspended,
    SessionNotActive,
    ParticipantNotAuthorized,
    DirectionNotAllowed,
    ChannelNotAllowed,
    StrategyNotAllowed,
    ValidityNotContained,
    IdempotencyConflict,
    PrivacyRelayDisabled,
    ServiceUnavailable,
    InternalError,
    InternalAuthenticationFailed,
    InternalPermissionDenied,
    ContactNotFound,
    ContactVersionConflict,
    ContactInvalid,
    ContactAlreadyExists,
    ContactDisabled,
    ContactBlocked,
    ContactNotVerified,
    ContactVerificationExpired,
    ContactDelegationInvalid,
    ContactScopeDenied,
    ContactResolutionDenied,
    ContactResolutionGrantExpired,
    ContactVaultDisabled,
    RelayNumberUnavailable,
    RelayAssignmentNotFound,
    RelayMappingNotActive,
    RelayMappingAmbiguous,
    RelayNumberPoolDisabled,
}

impl ErrorCode {
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::ValidationError,
        ErrorCode::NotFound,
        ErrorCode::MethodNotAllowed,
        ErrorCode::ContextNotFound,
        ErrorCode::SessionNotFound,
        ErrorCode::ContextVersionConflict,
        ErrorCode::SessionVersionConflict,
        ErrorCode::ContextReferenceConflict,
        ErrorCode::ContextNotActive,
        ErrorCode::ContextExpired,
        ErrorCode::ContextRevoked,
        ErrorCode::ContextSuspended,
        ErrorCode::SessionNotActive,
        ErrorCode::ParticipantNotAuthorized,
        ErrorCode::DirectionNotAllowed,
        ErrorCode::ChannelNotAllowed,
        ErrorCode::StrategyNotAllowed,
        ErrorCode::ValidityNotContained,
        ErrorCode::IdempotencyConflict,
        ErrorCode::PrivacyRelayDisabled,
        ErrorCode::ServiceUnavailable,
        ErrorCode::InternalError,
        ErrorCode::InternalAuthenticationFailed,
        ErrorCode::InternalPermissionDenied,
        ErrorCode::ContactNotFound,
        ErrorCode::ContactVersionConflict,
        ErrorCode::ContactInvalid,
        ErrorCode::ContactAlreadyExists,
        ErrorCode::ContactDisabled,
        ErrorCode::ContactBlocked,
        ErrorCode::ContactNotVerified,
        ErrorCode::ContactVerificationExpired,
        ErrorCode::ContactDelegationInvalid,
        ErrorCode::ContactScopeDenied,
        ErrorCode::ContactResolutionDenied,
        ErrorCode::ContactResolutionGrantExpired,
        ErrorCode::ContactVaultDisabled,
        ErrorCode::RelayNumberUnavailable,
        ErrorCode::RelayAssignmentNotFound,
        ErrorCode::RelayMappingNotActive,
        ErrorCode::RelayMappingAmbiguous,
        ErrorCode::RelayNumberPoolDisabled,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            ErrorCode::ContextNotFound => "CONTEXT_NOT_FOUND",
            ErrorCode::SessionNotFound => "SESSION_NOT_FOUND",
            ErrorCode::ContextVersionConflict => "CONTEXT_VERSION_CONFLICT",
            ErrorCode::SessionVersionConflict => "SESSION_VERSION_CONFLICT",
            ErrorCode::ContextReferenceConflict => "CONTEXT_REFERENCE_CONFLICT",
            ErrorCode::ContextNotActive => "CONTEXT_NOT_ACTIVE",
            ErrorCode::ContextExpired => "CONTEXT_EXPIRED",
            ErrorCode::ContextRevoked => "CONTEXT_REVOKED",
            ErrorCode::ContextSuspended => "CONTEXT_SUSPENDED",
            ErrorCode::SessionNotActive => "SESSION_NOT_ACTIVE",
            ErrorCode::ParticipantNotAuthorized => "PARTICIPANT_NOT_AUTHORIZED",
            ErrorCode::DirectionNotAllowed => "DIRECTION_NOT_ALLOWED",
            ErrorCode::ChannelNotAllowed => "CHANNEL_NOT_ALLOWED",
            ErrorCode::StrategyNotAllowed => "STRATEGY_NOT_ALLOWED",
            ErrorCode::ValidityNotContained => "VALIDITY_NOT_CONTAINED",
            ErrorCode::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            ErrorCode::PrivacyRelayDisabled => "PRIVACY_RELAY_DISABLED",
            ErrorCode::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::InternalAuthenticationFailed => "INTERNAL_AUTHENTICATION_FAILED",
            ErrorCode::InternalPermissionDenied => "INTERNAL_PERMISSION_DENIED",
            ErrorCode::ContactNotFound => "CONTACT_NOT_FOUND",
            ErrorCode::ContactVersionConflict => "CONTACT_VERSION_CONFLICT",
            ErrorCode::ContactInvalid => "CONTACT_INVALID",
            ErrorCode::ContactAlreadyExists => "CONTACT_ALREADY_EXISTS",
            ErrorCode::ContactDisabled => "CONTACT_DISABLED",
            ErrorCode::ContactBlocked => "CONTACT_BLOCKED",
            ErrorCode::ContactNotVerified => "CONTACT_NOT_VERIFIED",
            ErrorCode::ContactVerificationExpired => "CONTACT_VERIFICATION_EXPIRED",
            ErrorCode::ContactDelegationInvalid => "CONTACT_DELEGATION_INVALID",
            ErrorCode::ContactScopeDenied => "CONTACT_SCOPE_DENIED",
            ErrorCode::ContactResolutionDenied => "CONTACT_RESOLUTION_DENIED",
            ErrorCode::ContactResolutionGrantExpired => "CONTACT_RESOLUTION_GRANT_EXPIRED",
            ErrorCode::ContactVaultDisabled => "CONTACT_VAULT_DISABLED",
            ErrorCode::RelayNumberUnavailable => "RELAY_NUMBER_UNAVAILABLE",
            ErrorCode::RelayAssignmentNotFound => "RELAY_ASSIGNMENT_NOT_FOUND",
            ErrorCode::RelayMappingNotActive => "RELAY_MAPPING_NOT_ACTIVE",
            ErrorCode::RelayMappingAmbiguous => "RELAY_MAPPING_AMBIGUOUS",
            ErrorCode::RelayNumberPoolDisabled => "RELAY_NUMBER_POOL_DISABLED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == value)
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ErrorCode::ValidationError => StatusCode::BAD_REQUEST,
            ErrorCode::NotFound | ErrorCode::ContextNotFound | ErrorCode::SessionNotFound => {
                StatusCode::NOT_FOUND
            }
            ErrorCode::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ErrorCode::ContextVersionConflict
            | ErrorCode::SessionVersionConflict
            | ErrorCode::ContextReferenceConflict
            | ErrorCode::ContextNotActive
            | ErrorCode::ContextExpired
            | ErrorCode::ContextRevoked
            | ErrorCode::ContextSuspended
            | ErrorCode::SessionNotActive
            | ErrorCode::IdempotencyConflict => StatusCode::CONFLICT,
            ErrorCode::ParticipantNotAuthorized
            | ErrorCode::DirectionNotAllowed
            | ErrorCode::ChannelNotAllowed
            | ErrorCode::StrategyNotAllowed
            | ErrorCode::ValidityNotContained => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::PrivacyRelayDisabled | ErrorCode::ServiceUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::InternalAuthenticationFailed => StatusCode::UNAUTHORIZED,
            ErrorCode::InternalPermissionDenied => StatusCode::FORBIDDEN,
            ErrorCode::ContactNotFound => StatusCode::NOT_FOUND,
            ErrorCode::ContactVersionConflict => StatusCode::CONFLICT,
            ErrorCode::ContactInvalid => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ContactAlreadyExists => StatusCode::CONFLICT,
            ErrorCode::ContactDisabled => StatusCode::CONFLICT,
            ErrorCode::ContactBlocked => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ContactNotVerified => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ContactVerificationExpired => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::ContactDelegationInvalid => StatusCode::UNAUTHORIZED,
            ErrorCode::ContactScopeDenied => StatusCode::FORBIDDEN,
            ErrorCode::ContactResolutionDenied => StatusCode::FORBIDDEN,
            ErrorCode::ContactResolutionGrantExpired => StatusCode::FORBIDDEN,
            ErrorCode::ContactVaultDisabled => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RelayNumberUnavailable => StatusCode::CONFLICT,
            ErrorCode::RelayAssignmentNotFound => StatusCode::NOT_FOUND,
            ErrorCode::RelayMappingNotActive => StatusCode::CONFLICT,
            ErrorCode::RelayMappingAmbiguous => StatusCode::CONFLICT,
            ErrorCode::RelayNumberPoolDisabled => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Fixed, safe default message.
    pub fn message(&self) -> &'static str {
        match self {
            ErrorCode::ValidationError => "The request is invalid.",
            ErrorCode::NotFound => "The requested resource was not found.",
            ErrorCode::MethodNotAllowed => "The method is not allowed for this resource.",
            ErrorCode::ContextNotFound => "The communication context was not found.",
            ErrorCode::SessionNotFound => "The privacy relay session was not found.",
            ErrorCode::ContextVersionConflict => {
                "The communication context has changed since expectedVersion; fetch it and retry."
            }
            ErrorCode::SessionVersionConflict => {
                "The privacy relay session has changed since expectedVersion; fetch it and retry."
            }
            ErrorCode::ContextReferenceConflict => {
                "A communication context already exists for this external context reference."
            }
            ErrorCode::ContextNotActive => {
                "The communication context does not allow this action in its current state."
            }
            ErrorCode::ContextExpired => "The communication context has expired.",
            ErrorCode::ContextRevoked => "The communication context has been revoked.",
            ErrorCode::ContextSuspended => "The communication context is suspended.",
            ErrorCode::SessionNotActive => {
                "The privacy relay session does not allow this action in its current state."
            }
            ErrorCode::ParticipantNotAuthorized => {
                "A participant is not an active participant of the communication context."
            }
            ErrorCode::DirectionNotAllowed => {
                "A direction is not allowed by the communication context."
            }
            ErrorCode::ChannelNotAllowed => {
                "A channel is not allowed by the communication context."
            }
            ErrorCode::StrategyNotAllowed => {
                "The relay strategy is not allowed by the communication context."
            }
            ErrorCode::ValidityNotContained => {
                "The validity window must be contained by the communication context's validity."
            }
            ErrorCode::IdempotencyConflict => {
                "The Idempotency-Key was already used for a different request."
            }
            ErrorCode::PrivacyRelayDisabled => "Privacy Relay is currently disabled.",
            ErrorCode::ServiceUnavailable => "The service is temporarily unavailable; retry later.",
            ErrorCode::InternalError => "An internal error occurred.",
            ErrorCode::InternalAuthenticationFailed => "Internal service authentication failed.",
            ErrorCode::InternalPermissionDenied => {
                "The calling service is not permitted to perform this operation."
            }
            ErrorCode::ContactNotFound => "The contact was not found.",
            ErrorCode::ContactVersionConflict => "The contact has changed since expectedVersion; fetch it and retry.",
            ErrorCode::ContactInvalid => "The telephone number is not a valid, protectable telephone number.",
            ErrorCode::ContactAlreadyExists => "Another contact of this application environment already protects this telephone number.",
            ErrorCode::ContactDisabled => "The contact is disabled.",
            ErrorCode::ContactBlocked => "The contact is blocked or invalid and cannot be used.",
            ErrorCode::ContactNotVerified => "The contact is not verified.",
            ErrorCode::ContactVerificationExpired => "The contact's verification has expired.",
            ErrorCode::ContactDelegationInvalid => "A valid Contact Vault access token is required.",
            ErrorCode::ContactScopeDenied => "The Contact Vault access token does not grant this operation.",
            ErrorCode::ContactResolutionDenied => "Contact resolution is not authorized for this execution.",
            ErrorCode::ContactResolutionGrantExpired => "The contact resolution grant has expired.",
            ErrorCode::ContactVaultDisabled => "The Contact Vault is currently disabled.",
            ErrorCode::RelayNumberUnavailable => "No eligible relay number is available; retry later.",
            ErrorCode::RelayAssignmentNotFound => "The relay number assignment was not found.",
            ErrorCode::RelayMappingNotActive => "The relay mapping is not active.",
            ErrorCode::RelayMappingAmbiguous => "The relay mapping is ambiguous and was refused.",
            ErrorCode::RelayNumberPoolDisabled => "The relay number pool is currently disabled.",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Invalid input; the message is a fixed, safe string.
    #[error("validation failed: {0}")]
    Validation(&'static str),
    /// A domain refusal with its stable code (default message).
    #[error("refused: {}", .0.as_str())]
    Refused(ErrorCode),
    /// A refusal with a fixed, safe message more specific than the code's default.
    #[error("refused: {}", .0.as_str())]
    Coded(ErrorCode, &'static str),
    /// Database failure (summary is operator-facing, never returned).
    #[error("database error: {summary}")]
    Database { summary: String, connectivity: bool },
}

pub type RelayResult<T> = Result<T, RelayError>;

impl RelayError {
    pub fn code(&self) -> ErrorCode {
        match self {
            RelayError::Validation(_) => ErrorCode::ValidationError,
            RelayError::Refused(code) | RelayError::Coded(code, _) => *code,
            RelayError::Database { connectivity, .. } => {
                if *connectivity {
                    ErrorCode::ServiceUnavailable
                } else {
                    ErrorCode::InternalError
                }
            }
        }
    }

    fn message(&self) -> &'static str {
        match self {
            RelayError::Validation(message) | RelayError::Coded(_, message) => message,
            other => other.code().message(),
        }
    }

    /// The unique constraint a database error violated, if any.
    pub fn violated_unique(&self) -> Option<&str> {
        match self {
            RelayError::Database { summary, .. } => summary
                .strip_prefix("unique violation: ")
                .map(|s| s.split_whitespace().next().unwrap_or_default()),
            _ => None,
        }
    }
}

impl From<ErrorCode> for RelayError {
    fn from(code: ErrorCode) -> Self {
        RelayError::Refused(code)
    }
}

/// Bounded, secret-free summary of a database error (SQLSTATE class and constraint only).
pub fn summarize_db_error(err: &DbErr) -> (String, bool) {
    let lowered = err.to_string().to_lowercase();
    let connectivity = matches!(err, DbErr::Conn(_) | DbErr::ConnectionAcquire(_))
        || lowered.contains("pool timed out")
        || lowered.contains("connection refused")
        || lowered.contains("connection reset");
    if let Some(sea_orm::SqlErr::UniqueConstraintViolation(message)) = err.sql_err() {
        let constraint = message
            .split('"')
            .nth(1)
            .unwrap_or("unknown")
            .chars()
            .take(80)
            .collect::<String>();
        return (format!("unique violation: {constraint}"), false);
    }
    let text = err.to_string();
    let sqlstate = text
        .split("code: ")
        .nth(1)
        .map(|s| s.chars().take(5).collect::<String>())
        .unwrap_or_default();
    let constraint = text
        .split("constraint \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .map(|s| s.chars().take(80).collect::<String>());
    let kind = match err {
        DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => "connection",
        DbErr::Exec(_) => "exec",
        DbErr::Query(_) => "query",
        DbErr::RecordNotFound(_) => "not found",
        _ => "database",
    };
    let mut summary = format!("{kind} error");
    if !sqlstate.is_empty() {
        summary.push_str(&format!(", sqlstate {sqlstate}"));
    }
    if let Some(c) = constraint {
        summary.push_str(&format!(", constraint {c}"));
    }
    (summary, connectivity)
}

impl From<DbErr> for RelayError {
    fn from(err: DbErr) -> Self {
        let (summary, connectivity) = summarize_db_error(&err);
        RelayError::Database {
            summary,
            connectivity,
        }
    }
}

/// An HTTP error response with the platform envelope.
pub fn error_response(code: ErrorCode, message: &str) -> Response {
    let body = Json(json!({
        "error": {
            "code": code.as_str(),
            "message": message,
            "requestId": current_request_id().map(|id| id.as_str().to_string()),
        }
    }));
    let mut response = (code.status(), body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        if let RelayError::Database { summary, .. } = &self {
            tracing::error!(%summary, "database error");
        }
        error_response(self.code(), self.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_parse() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(*code));
            assert!(code
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'_'));
        }
    }

    #[test]
    fn database_errors_never_leak() {
        let err = RelayError::Database {
            summary: "exec error, sqlstate 23514, constraint ck_x".into(),
            connectivity: false,
        };
        assert_eq!(err.code(), ErrorCode::InternalError);
        assert_eq!(err.message(), "An internal error occurred.");
    }
}
