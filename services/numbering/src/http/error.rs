//! Error model of the Numbering API: `{ "error": { "code", "message", "requestId",
//! "reason"? } }` with `Cache-Control: no-store`. Codes are stable (contract
//! `cfmeet.numbering.common` `Error`); `reason` carries a domain rule code (e.g.
//! `ELIGIBILITY_REQUIRED`) for PRECONDITION_FAILED / CONFLICT. Messages never contain a
//! number, provider reference, credential or database text.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{domain::current_context, numbers::DomainError};

macro_rules! error_codes {
    ($($variant:ident = $s:literal => $status:ident, $message:literal;)+) => {
        /// Stable error codes.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode { $($variant),+ }

        impl ErrorCode {
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $(ErrorCode::$variant => $s),+ }
            }

            pub fn status(&self) -> StatusCode {
                match self { $(ErrorCode::$variant => StatusCode::$status),+ }
            }

            pub fn default_message(&self) -> &'static str {
                match self { $(ErrorCode::$variant => $message),+ }
            }
        }
    };
}

error_codes! {
    ValidationError = "VALIDATION_ERROR" => BAD_REQUEST, "The request is invalid.";
    AuthenticationFailed = "AUTHENTICATION_FAILED" => UNAUTHORIZED, "Authentication failed.";
    InsufficientScope = "INSUFFICIENT_SCOPE" => FORBIDDEN, "The credential lacks the required scope.";
    NotFound = "NOT_FOUND" => NOT_FOUND, "The resource was not found.";
    MethodNotAllowed = "METHOD_NOT_ALLOWED" => METHOD_NOT_ALLOWED, "The method is not allowed.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
    RateLimited = "RATE_LIMITED" => TOO_MANY_REQUESTS, "Too many requests.";
    InternalAuthenticationFailed = "INTERNAL_AUTHENTICATION_FAILED" => UNAUTHORIZED, "Internal service authentication failed.";
    InternalPermissionDenied = "INTERNAL_PERMISSION_DENIED" => FORBIDDEN, "The caller is not permitted to perform this operation.";
    VersionConflict = "VERSION_CONFLICT" => CONFLICT, "The resource was changed concurrently.";
    InvalidTransition = "INVALID_TRANSITION" => CONFLICT, "The transition is not allowed.";
    Conflict = "CONFLICT" => CONFLICT, "The operation conflicts with existing state.";
    PreconditionFailed = "PRECONDITION_FAILED" => UNPROCESSABLE_ENTITY, "A prerequisite of the operation is not met.";
    CapabilityNotSupported = "CAPABILITY_NOT_SUPPORTED" => UNPROCESSABLE_ENTITY, "The number does not support the requested capability.";
    OperatorRequired = "OPERATOR_REQUIRED" => BAD_REQUEST, "Administration names the human operator (X-CF-Operator).";
    IdempotencyKeyReused = "IDEMPOTENCY_KEY_REUSED" => UNPROCESSABLE_ENTITY, "The idempotency key was used for a different request.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

/// An error of a Numbering route.
#[derive(Debug)]
pub enum ApiError {
    Code(ErrorCode),
    Domain(DomainError),
    RateLimited { retry_after_seconds: u64 },
}

impl From<DomainError> for ApiError {
    fn from(e: DomainError) -> Self {
        ApiError::Domain(e)
    }
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError::Code(code)
    }
}

/// An error response with the envelope.
pub fn error_response(code: ErrorCode, message: &str, reason: Option<&str>) -> Response {
    let mut error = json!({
        "code": code.as_str(),
        "message": message,
        "requestId": current_context().map(|c| c.request_id.as_str().to_string()),
    });
    if let Some(reason) = reason {
        error["reason"] = json!(reason);
    }
    let mut response = (code.status(), Json(json!({ "error": error }))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Code(code) => {
                let mut response = error_response(code, code.default_message(), None);
                if code == ErrorCode::AuthenticationFailed {
                    response.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Bearer realm=\"cf-meet-numbering\""),
                    );
                }
                response
            }
            ApiError::RateLimited {
                retry_after_seconds,
            } => {
                let mut response = error_response(
                    ErrorCode::RateLimited,
                    ErrorCode::RateLimited.default_message(),
                    None,
                );
                if let Ok(v) = HeaderValue::from_str(&retry_after_seconds.max(1).to_string()) {
                    response.headers_mut().insert(header::RETRY_AFTER, v);
                }
                response
            }
            ApiError::Domain(e) => {
                let (code, reason) = match &e {
                    DomainError::Validation(_) => (ErrorCode::ValidationError, None),
                    DomainError::NotFound(_) => (ErrorCode::NotFound, None),
                    DomainError::VersionConflict(_) => (ErrorCode::VersionConflict, None),
                    DomainError::InvalidTransition { .. } => (ErrorCode::InvalidTransition, None),
                    DomainError::Precondition {
                        code: "CAPABILITY_NOT_SUPPORTED",
                        ..
                    } => (
                        ErrorCode::CapabilityNotSupported,
                        Some("CAPABILITY_NOT_SUPPORTED"),
                    ),
                    DomainError::Precondition { code, .. } => {
                        (ErrorCode::PreconditionFailed, Some(*code))
                    }
                    DomainError::Conflict { code, .. } => (ErrorCode::Conflict, Some(*code)),
                    DomainError::Database { summary, .. } => {
                        tracing::error!(%summary, "numbering database failure");
                        (ErrorCode::ServiceUnavailable, None)
                    }
                };
                let message = match &e {
                    DomainError::Database { .. } | DomainError::NotFound(_) => {
                        code.default_message().to_string()
                    }
                    other => other.to_string(),
                };
                error_response(code, &message, reason)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_upper_snake_case() {
        for code in ErrorCode::ALL {
            assert!(code
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'_'));
        }
    }
}
