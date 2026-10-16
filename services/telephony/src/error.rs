//! Error model of the Telephony service.
//!
//! Every error has the platform envelope
//! `{ "error": { "code": "AUTHORIZATION_PROOF_INVALID", "message": "…", "requestId": "req_…" } }`.
//! Codes are stable (contract `cfmeet.telephony.internal-api`); messages are fixed, safe
//! strings. Responses never contain database errors, provider payloads, provider ids,
//! SIP data, telephone numbers, tokens or secrets.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::DbErr;
use serde_json::json;

use crate::middleware::current_request_id;

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

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some(ErrorCode::$variant),)+ _ => None }
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
    IdempotencyConflict = "IDEMPOTENCY_CONFLICT" => CONFLICT, "The idempotency key was used for a different request.";
    AuthorizationProofInvalid = "AUTHORIZATION_PROOF_INVALID" => FORBIDDEN, "The paid action is not backed by valid authorization evidence.";
    AuthorizationProofExpired = "AUTHORIZATION_PROOF_EXPIRED" => FORBIDDEN, "The authorization evidence of the paid action has expired.";
    SpendReservationRequired = "SPEND_RESERVATION_REQUIRED" => FORBIDDEN, "The paid action requires a sufficient spend reservation.";
    StaleGeneration = "STALE_GENERATION" => CONFLICT, "The command targets an outdated execution generation.";
    ExecutionInProgress = "EXECUTION_IN_PROGRESS" => CONFLICT, "Another execution of this call leg is still in progress or unresolved.";
    TelephonyDisabled = "TELEPHONY_DISABLED" => SERVICE_UNAVAILABLE, "Telephony is disabled.";
    PublicOutboundDisabled = "PUBLIC_OUTBOUND_DISABLED" => SERVICE_UNAVAILABLE, "Public outbound calling is disabled.";
    PublicInboundDisabled = "PUBLIC_INBOUND_DISABLED" => SERVICE_UNAVAILABLE, "Public inbound calling is disabled.";
    CallbackRelayDisabled = "CALLBACK_RELAY_DISABLED" => SERVICE_UNAVAILABLE, "Callback relay is disabled.";
    RouteUnavailable = "ROUTE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "No eligible public route is available.";
    CapacityExhausted = "CAPACITY_EXHAUSTED" => SERVICE_UNAVAILABLE, "Public route capacity is exhausted.";
    LegNotFound = "LEG_NOT_FOUND" => NOT_FOUND, "The telephony leg was not found.";
    LegNotActive = "LEG_NOT_ACTIVE" => CONFLICT, "The telephony leg is not in a state that allows the operation.";
    BridgeNotFound = "BRIDGE_NOT_FOUND" => NOT_FOUND, "The bridge was not found.";
    BridgeConflict = "BRIDGE_CONFLICT" => CONFLICT, "The bridge request conflicts with the call's current bridge.";
    ProviderEventRejected = "PROVIDER_EVENT_REJECTED" => UNAUTHORIZED, "The provider event was rejected.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

#[derive(Debug, thiserror::Error)]
pub enum TelephonyError {
    /// A stable refusal with its default message.
    #[error("{0:?}")]
    Refused(ErrorCode),
    /// Malformed input; the message is a fixed, safe string.
    #[error("{0}")]
    Validation(&'static str),
    /// Database failure (summary is secret-free; never returned to the caller).
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

pub type TelephonyResult<T> = Result<T, TelephonyError>;

impl TelephonyError {
    pub fn code(&self) -> ErrorCode {
        match self {
            TelephonyError::Refused(code) => *code,
            TelephonyError::Validation(_) => ErrorCode::ValidationError,
            TelephonyError::Database { connectivity, .. } => {
                if *connectivity {
                    ErrorCode::ServiceUnavailable
                } else {
                    ErrorCode::InternalError
                }
            }
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            TelephonyError::Validation(message) => message,
            other => other.code().default_message(),
        }
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

/// Whether `err` is a unique violation of `constraint`.
pub fn is_unique_violation(err: &TelephonyError, constraint: &str) -> bool {
    matches!(err, TelephonyError::Database { summary, .. } if summary == &format!("unique violation: {constraint}"))
}

impl From<DbErr> for TelephonyError {
    fn from(err: DbErr) -> Self {
        let (summary, connectivity) = summarize_db_error(&err);
        TelephonyError::Database {
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

impl IntoResponse for TelephonyError {
    fn into_response(self) -> Response {
        if let TelephonyError::Database { summary, .. } = &self {
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
            let m = code.default_message().to_lowercase();
            for word in ["sip", "provider id", "carrier", "+234"] {
                assert!(!m.contains(word), "{code:?}");
            }
        }
    }

    #[test]
    fn database_errors_never_leak() {
        let err = TelephonyError::Database {
            summary: "exec error, sqlstate 23514, constraint ck_x".into(),
            connectivity: false,
        };
        assert_eq!(err.code(), ErrorCode::InternalError);
        assert_eq!(err.message(), "An internal error occurred.");
    }
}
