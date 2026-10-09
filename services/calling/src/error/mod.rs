//! Client-facing error model of the Calling service.
//!
//! Every error response has the same envelope:
//!
//! ```json
//! { "error": { "code": "AUTHENTICATION_FAILED", "message": "…", "requestId": "req_…" } }
//! ```
//!
//! Messages are fixed, safe strings. Responses never contain database errors, credential
//! secrets or verifiers, internal tenant details or whether a foreign resource exists.
//! Authentication failures are deliberately uniform: the client cannot tell an unknown
//! credential from a revoked one or from a suspended tenant.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{middleware::request_id::current_request_id, platform::PlatformError};

/// Stable public error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    AuthenticationFailed,
    InsufficientScope,
    NotFound,
    MethodNotAllowed,
    ValidationError,
    RateLimited,
    ServiceUnavailable,
    InternalError,
    /// Internal (service-to-service) routes only; contract `cfmeet.platform.internal-http`.
    InternalAuthenticationFailed,
    /// Internal routes only: the caller is not granted the required permission.
    InternalPermissionDenied,
    StaleCallVersion,
    CallStateConflict,
    CallEvidenceRequired,
    CallNotAuthorized,
    ConcurrentCallLimitExceeded,
    CallResourceStateConflict,
    Conflict,
    IdempotencyConflict,
    InvitationExpired,
    InvitationNotActionable,
    AnsweredElsewhere,
    CapabilityUnavailable,
    MediaUnavailable,
    ParticipantLimitExceeded,
    TargetUnavailable,
    PrivacyAuthorizationUnavailable,
    PublicRouteDenied,
    PublicRouteUnavailable,
    TelecomSpendLimitExceeded,
    VersionConflict,
    EnterprisePolicyDenied,
}

impl ErrorCode {
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::AuthenticationFailed,
        ErrorCode::InsufficientScope,
        ErrorCode::NotFound,
        ErrorCode::MethodNotAllowed,
        ErrorCode::ValidationError,
        ErrorCode::RateLimited,
        ErrorCode::ServiceUnavailable,
        ErrorCode::InternalError,
        ErrorCode::InternalAuthenticationFailed,
        ErrorCode::InternalPermissionDenied,
        ErrorCode::StaleCallVersion,
        ErrorCode::CallStateConflict,
        ErrorCode::CallEvidenceRequired,
        ErrorCode::CallNotAuthorized,
        ErrorCode::ConcurrentCallLimitExceeded,
        ErrorCode::CallResourceStateConflict,
        ErrorCode::Conflict,
        ErrorCode::IdempotencyConflict,
        ErrorCode::InvitationExpired,
        ErrorCode::InvitationNotActionable,
        ErrorCode::AnsweredElsewhere,
        ErrorCode::CapabilityUnavailable,
        ErrorCode::MediaUnavailable,
        ErrorCode::ParticipantLimitExceeded,
        ErrorCode::TargetUnavailable,
        ErrorCode::PrivacyAuthorizationUnavailable,
        ErrorCode::PublicRouteDenied,
        ErrorCode::PublicRouteUnavailable,
        ErrorCode::TelecomSpendLimitExceeded,
        ErrorCode::VersionConflict,
        ErrorCode::EnterprisePolicyDenied,
    ];

    /// Codes that only internal routes return.
    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            ErrorCode::InternalAuthenticationFailed | ErrorCode::InternalPermissionDenied
        )
    }

    /// Fixed, safe message of a call-domain code.
    fn message(&self) -> &'static str {
        match self {
            ErrorCode::StaleCallVersion => {
                "The call has changed since expectedVersion; fetch it and retry."
            }
            ErrorCode::CallStateConflict => {
                "The action is not allowed in the call's current state."
            }
            ErrorCode::CallEvidenceRequired => {
                "The required evidence for this transition is not present."
            }
            ErrorCode::CallNotAuthorized => "The call is not authorized for this action.",
            ErrorCode::ConcurrentCallLimitExceeded => "The concurrent call limit is reached.",
            ErrorCode::CallResourceStateConflict => {
                "A call resource is not in a state that allows this action."
            }
            ErrorCode::Conflict => "The request conflicts with an existing resource.",
            ErrorCode::IdempotencyConflict => {
                "The Idempotency-Key was already used for a different request."
            }
            ErrorCode::InvitationExpired => "The invitation has expired.",
            ErrorCode::InvitationNotActionable => "The invitation is no longer actionable.",
            ErrorCode::AnsweredElsewhere => "The call was answered on another device.",
            ErrorCode::CapabilityUnavailable => {
                "The action is not available for this call's media capabilities."
            }
            ErrorCode::MediaUnavailable => {
                "Media enforcement could not be confirmed; retry the request."
            }
            ErrorCode::ParticipantLimitExceeded => {
                "The call's participant limit would be exceeded."
            }
            ErrorCode::TargetUnavailable => "The target has no eligible application endpoint.",
            ErrorCode::PrivacyAuthorizationUnavailable => {
                "Privacy authorization is unavailable; the protected call was not placed. Retry later."
            }
            ErrorCode::PublicRouteDenied => {
                "Routing policy does not permit a public-network route for this call."
            }
            ErrorCode::PublicRouteUnavailable => {
                "No public-network route is currently available; the call was not placed."
            }
            ErrorCode::TelecomSpendLimitExceeded => {
                "A telecom spend or velocity limit is reached; the call was not placed."
            }
            ErrorCode::VersionConflict => {
                "The resource has changed since expectedVersion; fetch it and retry."
            }
            ErrorCode::EnterprisePolicyDenied => {
                "The enterprise policy does not permit this operation."
            }
            _ => "The request could not be completed.",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::AuthenticationFailed => "AUTHENTICATION_FAILED",
            ErrorCode::InsufficientScope => "INSUFFICIENT_SCOPE",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::InternalAuthenticationFailed => "INTERNAL_AUTHENTICATION_FAILED",
            ErrorCode::InternalPermissionDenied => "INTERNAL_PERMISSION_DENIED",
            ErrorCode::StaleCallVersion => "STALE_CALL_VERSION",
            ErrorCode::CallStateConflict => "CALL_STATE_CONFLICT",
            ErrorCode::CallEvidenceRequired => "CALL_EVIDENCE_REQUIRED",
            ErrorCode::CallNotAuthorized => "CALL_NOT_AUTHORIZED",
            ErrorCode::ConcurrentCallLimitExceeded => "CONCURRENT_CALL_LIMIT_EXCEEDED",
            ErrorCode::CallResourceStateConflict => "CALL_RESOURCE_STATE_CONFLICT",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            ErrorCode::InvitationExpired => "INVITATION_EXPIRED",
            ErrorCode::InvitationNotActionable => "INVITATION_NOT_ACTIONABLE",
            ErrorCode::AnsweredElsewhere => "ANSWERED_ELSEWHERE",
            ErrorCode::CapabilityUnavailable => "CAPABILITY_UNAVAILABLE",
            ErrorCode::MediaUnavailable => "MEDIA_UNAVAILABLE",
            ErrorCode::ParticipantLimitExceeded => "PARTICIPANT_LIMIT_EXCEEDED",
            ErrorCode::TargetUnavailable => "TARGET_UNAVAILABLE",
            ErrorCode::PrivacyAuthorizationUnavailable => "PRIVACY_AUTHORIZATION_UNAVAILABLE",
            ErrorCode::PublicRouteDenied => "PUBLIC_ROUTE_DENIED",
            ErrorCode::PublicRouteUnavailable => "PUBLIC_ROUTE_UNAVAILABLE",
            ErrorCode::TelecomSpendLimitExceeded => "TELECOM_SPEND_LIMIT_EXCEEDED",
            ErrorCode::VersionConflict => "VERSION_CONFLICT",
            ErrorCode::EnterprisePolicyDenied => "ENTERPRISE_POLICY_DENIED",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CallingError {
    /// Normalized authentication failure for every rejection reason.
    #[error("authentication failed")]
    AuthenticationFailed,
    /// The authenticated credential lacks a required scope.
    #[error("insufficient scope")]
    InsufficientScope { required: String },
    /// Resource absent or outside the caller's scope (indistinguishable by design).
    #[error("not found")]
    NotFound,
    #[error("method not allowed")]
    MethodNotAllowed,
    /// Invalid request input. The message must be a fixed, safe string.
    #[error("validation failed")]
    Validation(&'static str),
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    #[error("service unavailable")]
    ServiceUnavailable,
    #[error("internal error")]
    Internal,
    /// Normalized internal service authentication failure (every reason looks the same).
    #[error("internal authentication failed")]
    InternalAuthenticationFailed,
    /// The authenticated service lacks the internal permission the route requires.
    #[error("internal permission denied")]
    InternalPermissionDenied,
    /// A call-domain refusal with its stable code and status.
    #[error("call operation refused")]
    Call { code: ErrorCode, status: StatusCode },
}

impl CallingError {
    pub fn code(&self) -> ErrorCode {
        match self {
            CallingError::AuthenticationFailed => ErrorCode::AuthenticationFailed,
            CallingError::InsufficientScope { .. } => ErrorCode::InsufficientScope,
            CallingError::NotFound => ErrorCode::NotFound,
            CallingError::MethodNotAllowed => ErrorCode::MethodNotAllowed,
            CallingError::Validation(_) => ErrorCode::ValidationError,
            CallingError::RateLimited { .. } => ErrorCode::RateLimited,
            CallingError::ServiceUnavailable => ErrorCode::ServiceUnavailable,
            CallingError::Internal => ErrorCode::InternalError,
            CallingError::InternalAuthenticationFailed => ErrorCode::InternalAuthenticationFailed,
            CallingError::InternalPermissionDenied => ErrorCode::InternalPermissionDenied,
            CallingError::Call { code, .. } => *code,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CallingError::AuthenticationFailed => StatusCode::UNAUTHORIZED,
            CallingError::InsufficientScope { .. } => StatusCode::FORBIDDEN,
            CallingError::NotFound => StatusCode::NOT_FOUND,
            CallingError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            CallingError::Validation(_) => StatusCode::BAD_REQUEST,
            CallingError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            CallingError::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            CallingError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            CallingError::InternalAuthenticationFailed => StatusCode::UNAUTHORIZED,
            CallingError::InternalPermissionDenied => StatusCode::FORBIDDEN,
            CallingError::Call { status, .. } => *status,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            CallingError::AuthenticationFailed => "Authentication failed.",
            CallingError::InsufficientScope { .. } => {
                "The credential is not authorized for this operation."
            }
            CallingError::NotFound => "The requested resource was not found.",
            CallingError::MethodNotAllowed => "The method is not allowed for this resource.",
            CallingError::Validation(message) => message,
            CallingError::RateLimited { .. } => "Request rate limit exceeded; retry later.",
            CallingError::ServiceUnavailable => {
                "The service is temporarily unavailable; retry later."
            }
            CallingError::Internal => "An internal error occurred.",
            CallingError::InternalAuthenticationFailed => "Internal service authentication failed.",
            CallingError::InternalPermissionDenied => {
                "The calling service is not permitted to perform this operation."
            }
            CallingError::Call { code, .. } => code.message(),
        }
    }
}

/// Domain errors reaching the HTTP layer. Operator-facing detail is logged, not returned.
impl From<PlatformError> for CallingError {
    fn from(err: PlatformError) -> Self {
        match err {
            PlatformError::NotFound(_) => CallingError::NotFound,
            PlatformError::Validation(_) => CallingError::Validation("The request is invalid."),
            PlatformError::Database {
                summary,
                connectivity,
            } => {
                tracing::error!(%summary, "database error");
                if connectivity {
                    CallingError::ServiceUnavailable
                } else {
                    CallingError::Internal
                }
            }
            other => {
                tracing::warn!(error = %other, "platform operation refused");
                CallingError::Internal
            }
        }
    }
}

impl IntoResponse for CallingError {
    fn into_response(self) -> Response {
        let code = self.code();
        let body = Json(json!({
            "error": {
                "code": code.as_str(),
                "message": self.message(),
                "requestId": current_request_id().map(|id| id.as_str().to_string()),
            }
        }));
        let mut response = (self.status(), body).into_response();
        let headers = response.headers_mut();
        match &self {
            CallingError::AuthenticationFailed => {
                headers.insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"cf-meet-calling\""),
                );
            }
            CallingError::InsufficientScope { required } => {
                if let Ok(value) = HeaderValue::from_str(&format!(
                    "Bearer realm=\"cf-meet-calling\", error=\"insufficient_scope\", scope=\"{required}\""
                )) {
                    headers.insert(header::WWW_AUTHENTICATE, value);
                }
            }
            CallingError::RateLimited { retry_after_secs } => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from(*retry_after_secs));
            }
            _ => {}
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_stable() {
        assert_eq!(
            CallingError::AuthenticationFailed.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            CallingError::InsufficientScope {
                required: "x.y".into()
            }
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            CallingError::RateLimited {
                retry_after_secs: 3
            }
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(CallingError::NotFound.code().as_str(), "NOT_FOUND");
    }

    #[test]
    fn database_errors_are_not_exposed() {
        let err: CallingError = PlatformError::Database {
            summary: "sqlstate 23514, constraint ck_x".into(),
            connectivity: false,
        }
        .into();
        assert!(matches!(err, CallingError::Internal));
        assert_eq!(err.message(), "An internal error occurred.");
    }
}
