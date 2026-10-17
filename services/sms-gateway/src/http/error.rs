//! Error model of the SMS Gateway API: `{ "error": { "code", "message", "requestId",
//! "reason"?, "messageId"? } }` with `Cache-Control: no-store`. Codes are stable
//! (contract `cfmeet.sms-gateway.common` `Error`); `reason` carries a control reason code
//! (e.g. `RECIPIENT_OPTED_OUT`). Messages never contain a number, content, provider
//! reference, credential or database text.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::{domain::current_context, sms::SmsError};

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
    CallbackRejected = "CALLBACK_REJECTED" => UNAUTHORIZED, "The provider callback was not authenticated.";
    IdempotencyKeyRequired = "IDEMPOTENCY_KEY_REQUIRED" => BAD_REQUEST, "An Idempotency-Key header is required.";
    IdempotencyKeyReused = "IDEMPOTENCY_KEY_REUSED" => UNPROCESSABLE_ENTITY, "The idempotency key was used for a different request.";
    ContentTooLong = "CONTENT_TOO_LONG" => UNPROCESSABLE_ENTITY, "The message needs more than 10 segments.";
    SmsBlocked = "SMS_BLOCKED" => UNPROCESSABLE_ENTITY, "A safety control refused the message.";
    Conflict = "CONFLICT" => CONFLICT, "The operation conflicts with existing state.";
    NumberingUnavailable = "NUMBERING_UNAVAILABLE" => SERVICE_UNAVAILABLE, "Number authority is temporarily unavailable; nothing was sent.";
    ProviderUnavailable = "PROVIDER_UNAVAILABLE" => SERVICE_UNAVAILABLE, "No SMS provider is available; nothing was sent.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

/// An error of an SMS Gateway route.
#[derive(Debug)]
pub enum ApiError {
    Code(ErrorCode),
    Sms(SmsError),
    RateLimited {
        retry_after_seconds: u64,
        reason: Option<&'static str>,
    },
}

impl From<SmsError> for ApiError {
    fn from(e: SmsError) -> Self {
        ApiError::Sms(e)
    }
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError::Code(code)
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        ApiError::Sms(e.into())
    }
}

/// An error response with the envelope.
pub fn error_response(
    code: ErrorCode,
    message: &str,
    reason: Option<&str>,
    message_id: Option<Uuid>,
) -> Response {
    let mut error = json!({
        "code": code.as_str(),
        "message": message,
        "requestId": current_context().map(|c| c.request_id.as_str().to_string()),
    });
    if let Some(reason) = reason {
        error["reason"] = json!(reason);
    }
    if let Some(id) = message_id {
        error["messageId"] = json!(id);
    }
    let mut response = (code.status(), Json(json!({ "error": error }))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn retry_after(mut response: Response, seconds: u64) -> Response {
    if let Ok(v) = HeaderValue::from_str(&seconds.max(1).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, v);
    }
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Code(code) => {
                let mut response = error_response(code, code.default_message(), None, None);
                if code == ErrorCode::AuthenticationFailed {
                    response.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Bearer realm=\"cf-meet-sms\""),
                    );
                }
                response
            }
            ApiError::RateLimited {
                retry_after_seconds,
                reason,
            } => retry_after(
                error_response(
                    ErrorCode::RateLimited,
                    ErrorCode::RateLimited.default_message(),
                    reason,
                    None,
                ),
                retry_after_seconds,
            ),
            ApiError::Sms(e) => {
                let simple =
                    |code: ErrorCode| error_response(code, code.default_message(), None, None);
                match e {
                    SmsError::Validation(message) => {
                        error_response(ErrorCode::ValidationError, message, None, None)
                    }
                    SmsError::NotFound => simple(ErrorCode::NotFound),
                    SmsError::Blocked { reason, message_id } => error_response(
                        ErrorCode::SmsBlocked,
                        ErrorCode::SmsBlocked.default_message(),
                        Some(reason),
                        message_id,
                    ),
                    SmsError::RateLimited {
                        reason,
                        retry_after_seconds,
                    } => retry_after(
                        error_response(
                            ErrorCode::RateLimited,
                            ErrorCode::RateLimited.default_message(),
                            Some(reason),
                            None,
                        ),
                        retry_after_seconds,
                    ),
                    SmsError::ContentTooLong => simple(ErrorCode::ContentTooLong),
                    SmsError::IdempotencyKeyReused => simple(ErrorCode::IdempotencyKeyReused),
                    SmsError::NumberingUnavailable => simple(ErrorCode::NumberingUnavailable),
                    SmsError::ProviderUnavailable => simple(ErrorCode::ProviderUnavailable),
                    SmsError::AuthorityUnavailable => simple(ErrorCode::ServiceUnavailable),
                    SmsError::Conflict(reason) => error_response(
                        ErrorCode::Conflict,
                        ErrorCode::Conflict.default_message(),
                        Some(reason),
                        None,
                    ),
                    SmsError::Database { summary, .. } => {
                        tracing::error!(%summary, "sms gateway database failure");
                        simple(ErrorCode::ServiceUnavailable)
                    }
                }
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
