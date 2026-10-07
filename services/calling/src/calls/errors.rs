//! Call-domain errors.
//!
//! Every variant has a stable, client-safe code ([`CallError::code`]). There are no public
//! call mutation routes yet, so the codes that only mutations can produce
//! (`CALL_STATE_CONFLICT`, `STALE_CALL_VERSION`, …) are not part of the published HTTP
//! contract; [`CallError::http_status`] fixes how they will map when those routes exist.
//! Messages never contain database errors, provider detail or foreign-scope existence:
//! anything outside the caller's tenant / application / environment is `NotFound`.

use axum::http::StatusCode;

use super::{attributes::AttributeError, kinds::FailureCategory, state::CallState};
use crate::{error::CallingError, platform::error::summarize_db_error};

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// The Call (or the referenced child) does not exist in the caller's scope —
    /// indistinguishable from a resource of another tenant / application / environment.
    #[error("not found")]
    NotFound,
    /// Optimistic concurrency: the command was built against another version.
    #[error("stale call version (expected {expected}, current {current})")]
    StaleVersion { expected: i64, current: i64 },
    /// The Call is terminal; nothing can move it again.
    #[error("call is terminal ({state})")]
    Terminal { state: CallState },
    /// The action is not valid in the Call's current state.
    #[error("action {action} is not allowed in state {state}")]
    InvalidTransition {
        state: CallState,
        action: &'static str,
    },
    /// Required evidence is missing or does not hold.
    #[error("insufficient evidence: {0}")]
    InsufficientEvidence(&'static str),
    /// AUTHORIZED (or a paid public-network action) requires an effective APPROVED decision.
    #[error("an effective approved authorization is required")]
    AuthorizationRequired,
    /// The effective authorization decision is DENIED.
    #[error("the call is not authorized")]
    AuthorizationDenied,
    /// The environment may not produce real public-network telecom effects.
    #[error("live telecom is not allowed in this environment")]
    LiveTelecomNotAllowed,
    /// The effective concurrent-call limit is reached.
    #[error("concurrent call limit reached ({level})")]
    ConcurrentCallLimitExceeded { level: &'static str, limit: i64 },
    /// A child record is in a state that does not allow the operation.
    #[error("{0}")]
    InvalidState(&'static str),
    /// A uniqueness / idempotency conflict.
    #[error("{0}")]
    Conflict(&'static str),
    /// Malformed input. The message is a fixed, safe string.
    #[error("{0}")]
    Validation(&'static str),
    /// A failure category the action may not use.
    #[error("failure category {0} is not allowed for this action")]
    FailureCategoryNotAllowed(FailureCategory),
    /// Unexpected database failure (summary: SQLSTATE + constraint only).
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

pub type CallResult<T> = Result<T, CallError>;

impl CallError {
    /// Stable, client-safe code.
    pub fn code(&self) -> &'static str {
        match self {
            CallError::NotFound => "NOT_FOUND",
            CallError::StaleVersion { .. } => "STALE_CALL_VERSION",
            CallError::Terminal { .. } | CallError::InvalidTransition { .. } => {
                "CALL_STATE_CONFLICT"
            }
            CallError::InsufficientEvidence(_) => "CALL_EVIDENCE_REQUIRED",
            CallError::AuthorizationRequired | CallError::AuthorizationDenied => {
                "CALL_NOT_AUTHORIZED"
            }
            CallError::LiveTelecomNotAllowed => "LIVE_TELECOM_NOT_ALLOWED",
            CallError::ConcurrentCallLimitExceeded { .. } => "CONCURRENT_CALL_LIMIT_EXCEEDED",
            CallError::InvalidState(_) => "CALL_RESOURCE_STATE_CONFLICT",
            CallError::Conflict(_) => "CONFLICT",
            CallError::Validation(_) | CallError::FailureCategoryNotAllowed(_) => {
                "VALIDATION_ERROR"
            }
            CallError::Database { connectivity, .. } => {
                if *connectivity {
                    "SERVICE_UNAVAILABLE"
                } else {
                    "INTERNAL_ERROR"
                }
            }
        }
    }

    /// HTTP status the code maps to once call mutations are exposed.
    pub fn http_status(&self) -> StatusCode {
        match self {
            CallError::NotFound => StatusCode::NOT_FOUND,
            CallError::StaleVersion { .. }
            | CallError::Terminal { .. }
            | CallError::InvalidTransition { .. }
            | CallError::InvalidState(_)
            | CallError::Conflict(_) => StatusCode::CONFLICT,
            CallError::InsufficientEvidence(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CallError::AuthorizationRequired
            | CallError::AuthorizationDenied
            | CallError::LiveTelecomNotAllowed => StatusCode::FORBIDDEN,
            CallError::ConcurrentCallLimitExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            CallError::Validation(_) | CallError::FailureCategoryNotAllowed(_) => {
                StatusCode::BAD_REQUEST
            }
            CallError::Database { connectivity, .. } => {
                if *connectivity {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
        }
    }
}

impl From<sea_orm::DbErr> for CallError {
    fn from(err: sea_orm::DbErr) -> Self {
        CallError::Database {
            summary: summarize_db_error(&err),
            connectivity: crate::platform::error::is_connectivity_error(&err),
        }
    }
}

impl From<AttributeError> for CallError {
    fn from(_: AttributeError) -> Self {
        CallError::Validation("attributes must be bounded, flat and free of protected data")
    }
}

/// Errors reaching the (read-only) HTTP layer. Detail is logged, never returned.
impl From<CallError> for CallingError {
    fn from(err: CallError) -> Self {
        match err {
            CallError::NotFound => CallingError::NotFound,
            CallError::Validation(message) => CallingError::Validation(message),
            CallError::Database {
                summary,
                connectivity,
            } => {
                tracing::error!(%summary, "call database error");
                if connectivity {
                    CallingError::ServiceUnavailable
                } else {
                    CallingError::Internal
                }
            }
            other => {
                tracing::warn!(code = other.code(), error = %other, "call operation refused");
                CallingError::Internal
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_detail_is_never_part_of_the_client_error() {
        let err: CallingError = CallError::Database {
            summary: "sqlstate 23514, constraint ck_calls_state".into(),
            connectivity: false,
        }
        .into();
        assert!(matches!(err, CallingError::Internal));
    }

    #[test]
    fn conflicts_have_stable_codes() {
        assert_eq!(
            CallError::StaleVersion {
                expected: 5,
                current: 6
            }
            .code(),
            "STALE_CALL_VERSION"
        );
        assert_eq!(
            CallError::Terminal {
                state: CallState::Ended
            }
            .code(),
            "CALL_STATE_CONFLICT"
        );
        assert_eq!(
            CallError::InvalidTransition {
                state: CallState::Requested,
                action: "CONFIRM_CONNECTED"
            }
            .http_status(),
            StatusCode::CONFLICT
        );
        assert_eq!(CallError::NotFound.http_status(), StatusCode::NOT_FOUND);
    }
}
