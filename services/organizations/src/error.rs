//! Error model (contract `docs/api/organizations-internal-v1.md` §2):
//! `{"schemaVersion":1,"error":{"code","message","requestId"}}` with
//! `Cache-Control: no-store`. Codes are stable; messages never contain a token, an email
//! address or database text.

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
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(|r| r.clone()).ok()
}

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
    OrganizationNotFound = "ORGANIZATION_NOT_FOUND" => NOT_FOUND, "The organization was not found.";
    MembershipRequired = "MEMBERSHIP_REQUIRED" => FORBIDDEN, "An active membership of the organization is required.";
    PermissionDenied = "PERMISSION_DENIED" => FORBIDDEN, "Your organization role does not permit this operation.";
    OrganizationSuspended = "ORGANIZATION_SUSPENDED" => CONFLICT, "The organization is suspended.";
    OrganizationArchived = "ORGANIZATION_ARCHIVED" => CONFLICT, "The organization is archived.";
    LastOwner = "LAST_OWNER" => CONFLICT, "An organization must keep at least one active owner.";
    VersionConflict = "VERSION_CONFLICT" => CONFLICT, "The resource was changed concurrently; reload and retry.";
    SlugTaken = "SLUG_TAKEN" => CONFLICT, "The slug is already used by another organization.";
    InviteExpired = "INVITE_EXPIRED" => GONE, "The invitation has expired.";
    InvitationInvalid = "INVITATION_INVALID" => NOT_FOUND, "The invitation is invalid or was already used.";
    IdempotencyConflict = "IDEMPOTENCY_CONFLICT" => CONFLICT, "The Idempotency-Key was already used with a different request.";
    ProvisioningPending = "PROVISIONING_PENDING" => CONFLICT, "The organization's technical provisioning is not ready.";
    RateLimited = "RATE_LIMITED" => TOO_MANY_REQUESTS, "Too many requests; retry later.";
    ActorNotAllowed = "ACTOR_NOT_ALLOWED" => FORBIDDEN, "The caller may not assert an acting user.";
    InternalAuthenticationFailed = "INTERNAL_AUTHENTICATION_FAILED" => UNAUTHORIZED, "Internal service authentication failed.";
    InternalPermissionDenied = "INTERNAL_PERMISSION_DENIED" => FORBIDDEN, "The caller is not permitted to perform this operation.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    // Section 6.
    PolicyRestricted = "POLICY_RESTRICTED" => FORBIDDEN, "The organization policy does not permit this operation.";
    TeamNotFound = "TEAM_NOT_FOUND" => NOT_FOUND, "The team was not found.";
    TeamArchived = "TEAM_ARCHIVED" => CONFLICT, "The team is archived.";
    TeamNameTaken = "TEAM_NAME_TAKEN" => CONFLICT, "An active team of the organization already has this name.";
    // Infrastructure codes (unknown route / method, oversized body, unexpected failure).
    NotFound = "NOT_FOUND" => NOT_FOUND, "The resource was not found.";
    MethodNotAllowed = "METHOD_NOT_ALLOWED" => METHOD_NOT_ALLOWED, "The method is not allowed.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

/// An error of an Organizations route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    /// A safe, value-free message replacing the default one.
    pub message: Option<String>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: None,
        }
    }

    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: Some(message.into()),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::with_message(ErrorCode::ValidationError, message)
    }
}

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError::new(code)
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        tracing::error!(summary = %crate::db::summarize_db_error(&e), "organizations database failure");
        ApiError::new(ErrorCode::ServiceUnavailable)
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

/// An error response with the envelope.
pub fn error_response(code: ErrorCode, message: Option<&str>) -> Response {
    let body = json!({
        "schemaVersion": 1,
        "error": {
            "code": code.as_str(),
            "message": message.unwrap_or(code.default_message()),
            "requestId": current_request_id(),
        }
    });
    let mut response = (code.status(), Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.code, self.message.as_deref())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_upper_snake_case_and_match_the_contract_statuses() {
        for code in ErrorCode::ALL {
            assert!(code
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b == b'_'));
            assert!(code.status().is_client_error() || code.status().is_server_error());
        }
        assert_eq!(ErrorCode::InviteExpired.status().as_u16(), 410);
        assert_eq!(ErrorCode::OrganizationSuspended.status().as_u16(), 409);
        assert_eq!(ErrorCode::InvitationInvalid.status().as_u16(), 404);
        assert_eq!(ErrorCode::RateLimited.status().as_u16(), 429);
        assert_eq!(ErrorCode::ActorNotAllowed.status().as_u16(), 403);
    }
}
