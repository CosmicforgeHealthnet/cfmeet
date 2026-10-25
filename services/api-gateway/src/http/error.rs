//! Public error model: `{ "error": { "code", "message", "requestId" } }` with
//! `Cache-Control: no-store` (contract `cfmeet.api-gateway.common` `Error`). Messages are
//! constant: they never echo input, a credential, a token, an upstream name or upstream
//! text.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::context::current_context;

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
    NotFound = "NOT_FOUND" => NOT_FOUND, "No such route.";
    MethodNotAllowed = "METHOD_NOT_ALLOWED" => METHOD_NOT_ALLOWED, "The method is not allowed.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
    ValidationError = "VALIDATION_ERROR" => BAD_REQUEST, "The request is invalid.";
    AuthenticationFailed = "AUTHENTICATION_FAILED" => UNAUTHORIZED, "Authentication failed.";
    InsufficientScope = "INSUFFICIENT_SCOPE" => FORBIDDEN, "The credential lacks the required scope.";
    RateLimited = "RATE_LIMITED" => TOO_MANY_REQUESTS, "Too many requests.";
    QuotaExceeded = "QUOTA_EXCEEDED" => TOO_MANY_REQUESTS, "The platform request quota of this environment is exhausted.";
    HeadersTooLarge = "HEADERS_TOO_LARGE" => REQUEST_HEADER_FIELDS_TOO_LARGE, "The request headers are too large.";
    UriTooLong = "URI_TOO_LONG" => URI_TOO_LONG, "The request URI is too long.";
    OriginNotAllowed = "ORIGIN_NOT_ALLOWED" => FORBIDDEN, "This origin is not allowed.";
    ProtocolNotEnabled = "PROTOCOL_NOT_ENABLED" => NOT_IMPLEMENTED, "This protocol is not enabled for the route.";
    UpstreamUnavailable = "UPSTREAM_UNAVAILABLE" => BAD_GATEWAY, "The service behind this route is unavailable.";
    UpstreamTimeout = "UPSTREAM_TIMEOUT" => GATEWAY_TIMEOUT, "The service behind this route did not answer in time.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    Code(ErrorCode),
    RateLimited { retry_after_seconds: u64 },
    QuotaExceeded { retry_after_seconds: u64 },
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError::Code(code)
    }
}

pub fn error_response(code: ErrorCode) -> Response {
    let body = json!({ "error": {
        "code": code.as_str(),
        "message": code.default_message(),
        "requestId": current_context().map(|c| c.request_id.as_str().to_string()),
    }});
    let mut response = (code.status(), Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if code == ErrorCode::AuthenticationFailed {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"cf-meet-api\""),
        );
    }
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Code(code) => error_response(code),
            ApiError::RateLimited {
                retry_after_seconds,
            }
            | ApiError::QuotaExceeded {
                retry_after_seconds,
            } => {
                let code = if matches!(self, ApiError::RateLimited { .. }) {
                    ErrorCode::RateLimited
                } else {
                    ErrorCode::QuotaExceeded
                };
                let mut r = error_response(code);
                if let Ok(v) = HeaderValue::from_str(&retry_after_seconds.max(1).to_string()) {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
                r
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
            assert!(code.status().is_client_error() || code.status().is_server_error());
        }
    }
}
