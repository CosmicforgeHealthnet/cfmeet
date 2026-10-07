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
    ValidationError,
    RateLimited,
    ServiceUnavailable,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::AuthenticationFailed => "AUTHENTICATION_FAILED",
            ErrorCode::InsufficientScope => "INSUFFICIENT_SCOPE",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            ErrorCode::InternalError => "INTERNAL_ERROR",
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
    /// Invalid request input. The message must be a fixed, safe string.
    #[error("validation failed")]
    Validation(&'static str),
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    #[error("service unavailable")]
    ServiceUnavailable,
    #[error("internal error")]
    Internal,
}

impl CallingError {
    pub fn code(&self) -> ErrorCode {
        match self {
            CallingError::AuthenticationFailed => ErrorCode::AuthenticationFailed,
            CallingError::InsufficientScope { .. } => ErrorCode::InsufficientScope,
            CallingError::NotFound => ErrorCode::NotFound,
            CallingError::Validation(_) => ErrorCode::ValidationError,
            CallingError::RateLimited { .. } => ErrorCode::RateLimited,
            CallingError::ServiceUnavailable => ErrorCode::ServiceUnavailable,
            CallingError::Internal => ErrorCode::InternalError,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CallingError::AuthenticationFailed => StatusCode::UNAUTHORIZED,
            CallingError::InsufficientScope { .. } => StatusCode::FORBIDDEN,
            CallingError::NotFound => StatusCode::NOT_FOUND,
            CallingError::Validation(_) => StatusCode::BAD_REQUEST,
            CallingError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            CallingError::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            CallingError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            CallingError::AuthenticationFailed => "Authentication failed.",
            CallingError::InsufficientScope { .. } => {
                "The credential is not authorized for this operation."
            }
            CallingError::NotFound => "The requested resource was not found.",
            CallingError::Validation(message) => message,
            CallingError::RateLimited { .. } => "Request rate limit exceeded; retry later.",
            CallingError::ServiceUnavailable => {
                "The service is temporarily unavailable; retry later."
            }
            CallingError::Internal => "An internal error occurred.",
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
