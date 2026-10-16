//! Error model of the Platform Access API.
//!
//! Every error has the platform envelope
//! `{ "error": { "code": "…", "message": "…", "requestId": "req_…" } }` with
//! `Cache-Control: no-store`. Codes are stable (contract
//! `cfmeet.platform-access.internal-api`). Service-facing messages are fixed strings;
//! operator administration errors carry the domain's operator message, which never
//! contains a secret, verifier or raw database text (`PlatformError` guarantees that).

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use super::middleware::current_request_id;
use crate::identity::PlatformError;

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
    NotFound = "NOT_FOUND" => NOT_FOUND, "The resource was not found.";
    MethodNotAllowed = "METHOD_NOT_ALLOWED" => METHOD_NOT_ALLOWED, "The method is not allowed.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
    InternalAuthenticationFailed = "INTERNAL_AUTHENTICATION_FAILED" => UNAUTHORIZED, "Internal service authentication failed.";
    InternalPermissionDenied = "INTERNAL_PERMISSION_DENIED" => FORBIDDEN, "The caller is not permitted to perform this operation.";
    DelegatedContextRequired = "DELEGATED_CONTEXT_REQUIRED" => FORBIDDEN, "The operation requires a signed delegated platform context.";
    DelegatedContextInvalid = "DELEGATED_CONTEXT_INVALID" => FORBIDDEN, "The delegated platform context does not describe a valid authority chain.";
    OperatorRequired = "OPERATOR_REQUIRED" => BAD_REQUEST, "The administration request must name its operator (x-cf-operator).";
    Conflict = "CONFLICT" => CONFLICT, "The resource already exists.";
    PolicyViolation = "POLICY_VIOLATION" => UNPROCESSABLE_ENTITY, "The operation is not allowed by platform policy.";
    InvalidState = "INVALID_STATE" => CONFLICT, "The resource is not in a state that allows the operation.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    /// A stable refusal with its default message.
    #[error("{0:?}")]
    Refused(ErrorCode),
    /// A stable code with an operator-safe message (administration API only).
    #[error("{1}")]
    Operator(ErrorCode, String),
    /// Database / unexpected failure (summary is secret-free; never returned).
    #[error("internal failure ({summary})")]
    Internal { summary: String, connectivity: bool },
}

pub type AccessResult<T> = Result<T, AccessError>;

impl AccessError {
    pub fn code(&self) -> ErrorCode {
        match self {
            AccessError::Refused(code) | AccessError::Operator(code, _) => *code,
            AccessError::Internal { connectivity, .. } => {
                if *connectivity {
                    ErrorCode::ServiceUnavailable
                } else {
                    ErrorCode::InternalError
                }
            }
        }
    }

    /// The domain error as a SERVICE-facing refusal: fixed messages only.
    pub fn service(err: PlatformError) -> Self {
        match AccessError::operator(err) {
            AccessError::Operator(code, _) => AccessError::Refused(code),
            other => other,
        }
    }

    /// The domain error for the operator administration API (operator-safe message).
    pub fn operator(err: PlatformError) -> Self {
        match err {
            PlatformError::Validation(m) => AccessError::Operator(ErrorCode::ValidationError, m),
            PlatformError::NotFound(what) => {
                AccessError::Operator(ErrorCode::NotFound, format!("{what} not found"))
            }
            PlatformError::Conflict(m) => AccessError::Operator(ErrorCode::Conflict, m),
            PlatformError::PolicyViolation(m) => {
                AccessError::Operator(ErrorCode::PolicyViolation, m)
            }
            PlatformError::InvalidState(m) => AccessError::Operator(ErrorCode::InvalidState, m),
            PlatformError::Database {
                summary,
                connectivity,
            } => AccessError::Internal {
                summary,
                connectivity,
            },
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

impl IntoResponse for AccessError {
    fn into_response(self) -> Response {
        match &self {
            AccessError::Internal { summary, .. } => {
                tracing::error!(%summary, "platform access internal failure");
                error_response(self.code(), self.code().default_message())
            }
            AccessError::Operator(code, message) => error_response(*code, message),
            AccessError::Refused(code) => error_response(*code, code.default_message()),
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

    #[test]
    fn database_errors_are_never_echoed() {
        let err = AccessError::operator(PlatformError::Database {
            summary: "sqlstate 23514, constraint ck_x".into(),
            connectivity: false,
        });
        assert_eq!(err.code(), ErrorCode::InternalError);
        let service = AccessError::service(PlatformError::Validation("detail".into()));
        assert!(matches!(service, AccessError::Refused(ErrorCode::ValidationError)));
    }
}
