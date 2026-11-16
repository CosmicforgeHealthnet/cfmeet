//! Error model (contract `cfmeet.notifications.common` `Error`):
//! `{"schemaVersion":1,"error":{"code","message","requestId"}}` with
//! `Cache-Control: no-store`. `schemaVersion` is what the Meeting relay checks on every
//! relayed answer (the same envelope as the Communications Hub). Codes are the contract's
//! `ErrorCode` enum; messages never contain an address, a code, a token or database text.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

tokio::task_local! {
    /// The request id of the request being served (signed hop id, or a generated one).
    pub static REQUEST_ID: String;
}

/// The request id of the current request, if any.
pub fn current_request_id() -> String {
    REQUEST_ID
        .try_with(|r| r.clone())
        .unwrap_or_else(|_| format!("req_{}", uuid::Uuid::new_v4().simple()))
}

macro_rules! error_codes {
    ($($variant:ident = $s:literal => $status:ident, $message:literal;)+) => {
        /// Stable error codes (contract `ErrorCode`).
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
    AuthenticationRequired = "AUTHENTICATION_REQUIRED" => UNAUTHORIZED, "Authentication is required.";
    InternalAuthenticationFailed = "INTERNAL_AUTHENTICATION_FAILED" => UNAUTHORIZED, "Internal service authentication failed.";
    Forbidden = "FORBIDDEN" => FORBIDDEN, "The caller is not permitted to perform this operation.";
    NotFound = "NOT_FOUND" => NOT_FOUND, "The resource was not found.";
    VersionConflict = "VERSION_CONFLICT" => CONFLICT, "The resource was changed concurrently; reload and retry.";
    IdempotencyConflict = "IDEMPOTENCY_CONFLICT" => CONFLICT, "The Idempotency-Key was already used with a different request.";
    RateLimited = "RATE_LIMITED" => TOO_MANY_REQUESTS, "Too many requests; retry later.";
    Unavailable = "UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    ProtocolUnsupported = "PROTOCOL_UNSUPPORTED" => SERVICE_UNAVAILABLE, "The routing protocol version is not supported by this replica.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
}

/// An error of a Notification Service route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    /// A safe, value-free message replacing the default one.
    pub message: Option<String>,
    /// `Retry-After` seconds (RATE_LIMITED / UNAVAILABLE).
    pub retry_after: Option<u32>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: None,
            retry_after: None,
        }
    }

    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: Some(message.into()),
            retry_after: None,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::with_message(ErrorCode::ValidationError, message)
    }

    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound)
    }

    pub fn unavailable(retry_after: u32) -> Self {
        Self {
            code: ErrorCode::Unavailable,
            message: None,
            retry_after: Some(retry_after),
        }
    }

    pub fn rate_limited(retry_after: u32) -> Self {
        Self {
            code: ErrorCode::RateLimited,
            message: None,
            retry_after: Some(retry_after),
        }
    }
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError::new(code)
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        tracing::error!(summary = %crate::db::summarize_db_error(&e), "notifications database failure");
        ApiError::unavailable(5)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.code.as_str(),
            self.message
                .as_deref()
                .unwrap_or(self.code.default_message())
        )
    }
}

impl std::error::Error for ApiError {}

/// The error body (also used inside I2 relayed answers).
pub fn error_body(code: ErrorCode, message: Option<&str>) -> serde_json::Value {
    json!({
        "schemaVersion": 1,
        "error": {
            "code": code.as_str(),
            "message": message.unwrap_or(code.default_message()),
            "requestId": current_request_id(),
        }
    })
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.code.status(),
            Json(error_body(self.code, self.message.as_deref())),
        )
            .into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(seconds) = self.retry_after {
            if let Ok(v) = HeaderValue::from_str(&seconds.to_string()) {
                headers.insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_contract_enum() {
        let contract = [
            "VALIDATION_ERROR",
            "AUTHENTICATION_REQUIRED",
            "INTERNAL_AUTHENTICATION_FAILED",
            "FORBIDDEN",
            "NOT_FOUND",
            "VERSION_CONFLICT",
            "IDEMPOTENCY_CONFLICT",
            "RATE_LIMITED",
            "UNAVAILABLE",
            "PROTOCOL_UNSUPPORTED",
            "PAYLOAD_TOO_LARGE",
        ];
        let ours: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(ours, contract);
        assert_eq!(ErrorCode::RateLimited.status().as_u16(), 429);
        assert_eq!(ErrorCode::ProtocolUnsupported.status().as_u16(), 503);
    }
}
