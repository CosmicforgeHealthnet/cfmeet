//! Error model: `{ "error": { "code", "message", "requestId" } }` with
//! `Cache-Control: no-store` (contract `cfmeet.developer-platform.common` `Error`). Codes
//! are stable; messages never contain an identifier, a credential, a token or database
//! text.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::domain::current_context;

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
    DelegatedContextInvalid = "DELEGATED_CONTEXT_INVALID" => FORBIDDEN, "The identity authority did not confirm the delegated platform context.";
    AuthenticationRequired = "AUTHENTICATION_REQUIRED" => UNAUTHORIZED, "A verified CF Meet session or an API credential is required.";
    PermissionDenied = "PERMISSION_DENIED" => FORBIDDEN, "Your workspace role does not permit this operation.";
    InsufficientScope = "INSUFFICIENT_SCOPE" => FORBIDDEN, "The credential lacks the required scope.";
    OrganizationRestricted = "ORGANIZATION_RESTRICTED" => FORBIDDEN, "The organization is restricted; this operation is disabled.";
    OrganizationSuspended = "ORGANIZATION_SUSPENDED" => FORBIDDEN, "The organization is suspended.";
    OrganizationClosed = "ORGANIZATION_CLOSED" => FORBIDDEN, "The organization is closed.";
    OrganizationNotActive = "ORGANIZATION_NOT_ACTIVE" => FORBIDDEN, "The organization has not been activated yet.";
    VersionConflict = "VERSION_CONFLICT" => CONFLICT, "The resource was changed concurrently; reload and retry.";
    InvalidState = "INVALID_STATE" => CONFLICT, "The resource is not in a state that allows this operation.";
    LastOwner = "LAST_OWNER" => CONFLICT, "An organization must keep at least one owner.";
    AlreadyMember = "ALREADY_MEMBER" => CONFLICT, "The user is already a member of this organization.";
    InvitationPending = "INVITATION_PENDING" => CONFLICT, "A pending invitation already exists for this address.";
    InvitationInvalid = "INVITATION_INVALID" => NOT_FOUND, "The invitation is invalid, expired or already used.";
    InvitationEmailMismatch = "INVITATION_EMAIL_MISMATCH" => FORBIDDEN, "The invitation was issued to another verified email address.";
    IdempotencyKeyReused = "IDEMPOTENCY_KEY_REUSED" => UNPROCESSABLE_ENTITY, "The Idempotency-Key was already used with a different request.";
    PolicyViolation = "POLICY_VIOLATION" => UNPROCESSABLE_ENTITY, "The operation is not permitted by platform policy.";
    OperatorRequired = "OPERATOR_REQUIRED" => BAD_REQUEST, "Operator administration requires a named operator.";
    ApprovalExpired = "APPROVAL_EXPIRED" => CONFLICT, "The production-access approval expired before activation; submit a new request.";
    SelfApprovalForbidden = "SELF_APPROVAL_FORBIDDEN" => FORBIDDEN, "A reviewer may not decide a request of an organization they belong to.";
    ProductionAccessRequired = "PRODUCTION_ACCESS_REQUIRED" => FORBIDDEN, "The PRODUCTION environment of this project is not live.";
    CapabilityUnavailable = "CAPABILITY_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The capability is not available in this deployment.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

/// An error of a Developer Platform route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiError(pub ErrorCode);

impl From<ErrorCode> for ApiError {
    fn from(code: ErrorCode) -> Self {
        ApiError(code)
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        tracing::error!(summary = %crate::db::summarize_db_error(&e), "developer platform database failure");
        ApiError(ErrorCode::ServiceUnavailable)
    }
}

/// An error response with the envelope.
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
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.0)
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
