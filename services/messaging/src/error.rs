//! The Messaging error model: stable codes (contract `cfmeet.messaging.common` `Error`),
//! an optional machine reason and a safe message. Messages never contain key material,
//! ciphertext, content, a credential or database text.

use axum::http::StatusCode;

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
    ClientTokensNotEnabled = "CLIENT_TOKENS_NOT_ENABLED" => UNAUTHORIZED, "Client tokens are not enabled for Messaging yet; use a server API credential.";
    AuthoritySuspended = "AUTHORITY_SUSPENDED" => FORBIDDEN, "The tenant, application or environment is suspended.";
    InsufficientScope = "INSUFFICIENT_SCOPE" => FORBIDDEN, "The credential lacks the required scope.";
    ActingPrincipalRequired = "ACTING_PRINCIPAL_REQUIRED" => BAD_REQUEST, "The CF-Acting-Principal header is required.";
    PrincipalNotAuthorized = "PRINCIPAL_NOT_AUTHORIZED" => FORBIDDEN, "The credential may not act for this principal.";
    NotFound = "NOT_FOUND" => NOT_FOUND, "The resource was not found.";
    MethodNotAllowed = "METHOD_NOT_ALLOWED" => METHOD_NOT_ALLOWED, "The method is not allowed.";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE" => PAYLOAD_TOO_LARGE, "The request body is too large.";
    RateLimited = "RATE_LIMITED" => TOO_MANY_REQUESTS, "Too many requests.";
    InternalAuthenticationFailed = "INTERNAL_AUTHENTICATION_FAILED" => UNAUTHORIZED, "Internal service authentication failed.";
    InternalPermissionDenied = "INTERNAL_PERMISSION_DENIED" => FORBIDDEN, "The caller is not permitted to perform this operation.";
    FeatureDisabled = "FEATURE_DISABLED" => SERVICE_UNAVAILABLE, "This capability is not enabled in this deployment.";
    ChallengeInvalid = "CHALLENGE_INVALID" => CONFLICT, "The registration challenge is unknown, expired, consumed or bound to another request.";
    KeyPackageRejected = "KEY_PACKAGE_REJECTED" => UNPROCESSABLE_ENTITY, "The KeyPackage was rejected.";
    DeviceRevoked = "DEVICE_REVOKED" => CONFLICT, "The device is revoked.";
    DeviceLimitReached = "DEVICE_LIMIT_REACHED" => CONFLICT, "The principal has the maximum number of active devices.";
    KeyPackageLimitReached = "KEY_PACKAGE_LIMIT_REACHED" => CONFLICT, "The device has the maximum number of available KeyPackages.";
    IdempotencyKeyRequired = "IDEMPOTENCY_KEY_REQUIRED" => BAD_REQUEST, "An Idempotency-Key header is required.";
    IdempotencyKeyReused = "IDEMPOTENCY_KEY_REUSED" => UNPROCESSABLE_ENTITY, "The idempotency key was used for a different request.";
    ContextNotAllowed = "CONTEXT_NOT_ALLOWED" => UNPROCESSABLE_ENTITY, "The context class is not enabled by the messaging policy.";
    SecurityProfileNotAllowed = "SECURITY_PROFILE_NOT_ALLOWED" => UNPROCESSABLE_ENTITY, "The security profile is not allowed for this context by the messaging policy.";
    SecurityProfileImmutable = "SECURITY_PROFILE_IMMUTABLE" => CONFLICT, "A conversation's security profile can never change; create a successor conversation.";
    E2eeNotAvailable = "E2EE_NOT_AVAILABLE" => CONFLICT, "End-to-end encryption cannot be established for this conversation; nothing was created.";
    VersionConflict = "VERSION_CONFLICT" => CONFLICT, "The resource changed; reload it and retry with its current version.";
    InvalidTransition = "INVALID_TRANSITION" => CONFLICT, "The lifecycle transition is not allowed.";
    ConversationNotActive = "CONVERSATION_NOT_ACTIVE" => CONFLICT, "The conversation does not accept this operation in its lifecycle state.";
    PolicyVersionConflict = "POLICY_VERSION_CONFLICT" => CONFLICT, "The messaging policy changed; reload it and retry with its current version.";
    Conflict = "CONFLICT" => CONFLICT, "The operation conflicts with existing state.";
    ServiceUnavailable = "SERVICE_UNAVAILABLE" => SERVICE_UNAVAILABLE, "The service is temporarily unavailable.";
    InternalError = "INTERNAL_ERROR" => INTERNAL_SERVER_ERROR, "An internal error occurred.";
}

/// An error of a Messaging operation.
#[derive(Debug)]
pub enum Error {
    /// A stable code with an optional machine reason and a safe message.
    Code {
        code: ErrorCode,
        reason: Option<&'static str>,
        message: Option<String>,
    },
    RateLimited {
        retry_after_seconds: u64,
    },
    /// A database failure (summary is value-free).
    Database {
        summary: String,
        connectivity: bool,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Renders the code and reason only (never a message built from input).
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Code { code, reason, .. } => match reason {
                Some(r) => write!(f, "{} ({r})", code.as_str()),
                None => f.write_str(code.as_str()),
            },
            Error::RateLimited { .. } => f.write_str("RATE_LIMITED"),
            Error::Database { summary, .. } => write!(f, "database: {summary}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn code(code: ErrorCode) -> Self {
        Error::Code {
            code,
            reason: None,
            message: None,
        }
    }

    pub fn reason(code: ErrorCode, reason: &'static str) -> Self {
        Error::Code {
            code,
            reason: Some(reason),
            message: None,
        }
    }

    /// A validation error with a safe, fixed message (never echoing input).
    pub fn validation(message: impl Into<String>) -> Self {
        Error::Code {
            code: ErrorCode::ValidationError,
            reason: None,
            message: Some(message.into()),
        }
    }

    pub fn error_code(&self) -> ErrorCode {
        match self {
            Error::Code { code, .. } => *code,
            Error::RateLimited { .. } => ErrorCode::RateLimited,
            Error::Database { .. } => ErrorCode::ServiceUnavailable,
        }
    }
}

impl From<ErrorCode> for Error {
    fn from(code: ErrorCode) -> Self {
        Error::code(code)
    }
}

impl From<sea_orm::DbErr> for Error {
    fn from(e: sea_orm::DbErr) -> Self {
        Error::Database {
            summary: crate::db::summarize_db_error(&e),
            connectivity: crate::db::is_connectivity_error(&e),
        }
    }
}

impl From<sea_orm::TransactionError<Error>> for Error {
    fn from(e: sea_orm::TransactionError<Error>) -> Self {
        match e {
            sea_orm::TransactionError::Connection(db) => db.into(),
            sea_orm::TransactionError::Transaction(inner) => inner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_upper_snake_case_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL {
            assert!(code
                .as_str()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'));
            assert!(code.as_str().as_bytes()[0].is_ascii_uppercase());
            assert!(seen.insert(code.as_str()));
        }
    }
}
