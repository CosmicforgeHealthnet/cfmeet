//! HTTP rendering of [`crate::error::Error`]: `{ "error": { "code", "message",
//! "requestId", "reason"? } }` with `Cache-Control: no-store`.

use axum::{
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::domain::current_context;
pub use crate::error::{Error as ApiError, ErrorCode};

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
    if code == ErrorCode::AuthenticationFailed || code == ErrorCode::ClientTokensNotEnabled {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"cf-meet-messaging\""),
        );
    }
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Code {
                code,
                reason,
                message,
            } => error_response(
                code,
                message.as_deref().unwrap_or(code.default_message()),
                reason,
            ),
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
            ApiError::Database {
                summary,
                connectivity,
            } => {
                tracing::error!(%summary, connectivity, "messaging database failure");
                error_response(
                    ErrorCode::ServiceUnavailable,
                    ErrorCode::ServiceUnavailable.default_message(),
                    None,
                )
            }
        }
    }
}
