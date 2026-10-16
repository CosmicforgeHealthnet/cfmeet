//! Call-domain errors.
//!
//! Every variant has a stable, client-safe code ([`CallError::code`]) and HTTP status
//! ([`CallError::http_status`]); the public codes are part of the Calling HTTP contract.
//! Messages never contain database errors, provider detail or foreign-scope existence:
//! anything outside the caller's tenant / application / environment is `NotFound`.

use axum::http::StatusCode;

use super::{attributes::AttributeError, kinds::FailureCategory, state::CallState};
use crate::{
    error::{CallingError, ErrorCode},
    platform::error::summarize_db_error,
};

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
    /// The initiator's principal category may not perform this operation (Part H §10).
    #[error("the principal category is not eligible")]
    PrincipalCategoryNotAllowed,
    /// The environment may not produce real public-network telecom effects.
    #[error("live telecom is not allowed in this environment")]
    LiveTelecomNotAllowed,
    /// A request-rate ceiling of the operation's principal is reached (Part G §70).
    #[error("request rate limit reached")]
    RateLimited { retry_after_secs: u64 },
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
    /// The same Idempotency-Key was used for a different request.
    #[error("idempotency key reused for a different request")]
    IdempotencyConflict,
    /// The invitation's server-time ring window has passed; it can never be revived.
    #[error("invitation expired")]
    InvitationExpired,
    /// The invitation was declined, cancelled or is otherwise no longer actionable.
    #[error("invitation is no longer actionable")]
    InvitationNotActionable,
    /// Another device of the same participant won the answer.
    #[error("answered elsewhere")]
    AnsweredElsewhere,
    /// The configured media provider (or policy) cannot execute this action.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(&'static str),
    /// Provider enforcement of a command could not be confirmed; nothing was recorded as
    /// done and the command may be retried.
    #[error("media provider enforcement unavailable")]
    MediaUnavailable,
    /// The server-side participant limit of a Call would be exceeded.
    #[error("participant limit exceeded")]
    ParticipantLimitExceeded { limit: i64 },
    /// A target principal has no eligible application endpoint.
    #[error("target unavailable")]
    TargetUnavailable,
    /// A protected call needs a current Privacy Relay decision and none could be
    /// obtained (timeout, outage, authentication, malformed answer): fail closed.
    #[error("privacy authorization unavailable")]
    PrivacyAuthorizationUnavailable,
    /// The Routing Engine denied a public-network route (stable denial code, safe).
    #[error("public route denied ({0})")]
    PublicRouteDenied(&'static str),
    /// No public-network route is usable now (unavailable, full or unknown health).
    #[error("public route unavailable ({0})")]
    PublicRouteUnavailable(&'static str),
    /// A durable spend reservation was refused (stable reason code).
    #[error("telecom spend limit ({0})")]
    TelecomSpendLimitExceeded(&'static str),
    /// Protected artifact storage could not store / serve an artifact now (retry later).
    #[error("artifact storage unavailable")]
    ArtifactStorageUnavailable,
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
            CallError::PrincipalCategoryNotAllowed => "PRINCIPAL_NOT_ELIGIBLE",
            CallError::ConcurrentCallLimitExceeded { .. } => "CONCURRENT_CALL_LIMIT_EXCEEDED",
            CallError::RateLimited { .. } => "RATE_LIMITED",
            CallError::InvalidState(_) => "CALL_RESOURCE_STATE_CONFLICT",
            CallError::Conflict(_) => "CONFLICT",
            CallError::Validation(_) | CallError::FailureCategoryNotAllowed(_) => {
                "VALIDATION_ERROR"
            }
            CallError::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            CallError::InvitationExpired => "INVITATION_EXPIRED",
            CallError::InvitationNotActionable => "INVITATION_NOT_ACTIONABLE",
            CallError::AnsweredElsewhere => "ANSWERED_ELSEWHERE",
            CallError::CapabilityUnavailable(_) => "CAPABILITY_UNAVAILABLE",
            CallError::MediaUnavailable => "MEDIA_UNAVAILABLE",
            CallError::ParticipantLimitExceeded { .. } => "PARTICIPANT_LIMIT_EXCEEDED",
            CallError::TargetUnavailable => "TARGET_UNAVAILABLE",
            CallError::PrivacyAuthorizationUnavailable => "PRIVACY_AUTHORIZATION_UNAVAILABLE",
            CallError::PublicRouteDenied(_) => "PUBLIC_ROUTE_DENIED",
            CallError::PublicRouteUnavailable(_) => "PUBLIC_ROUTE_UNAVAILABLE",
            CallError::TelecomSpendLimitExceeded(_) => "TELECOM_SPEND_LIMIT_EXCEEDED",
            CallError::ArtifactStorageUnavailable => "ARTIFACT_STORAGE_UNAVAILABLE",
            CallError::Database { connectivity, .. } => {
                if *connectivity {
                    "SERVICE_UNAVAILABLE"
                } else {
                    "INTERNAL_ERROR"
                }
            }
        }
    }

    /// HTTP status of the code.
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
            | CallError::LiveTelecomNotAllowed
            | CallError::PrincipalCategoryNotAllowed => StatusCode::FORBIDDEN,
            CallError::ConcurrentCallLimitExceeded { .. } | CallError::RateLimited { .. } => {
                StatusCode::TOO_MANY_REQUESTS
            }
            CallError::Validation(_) | CallError::FailureCategoryNotAllowed(_) => {
                StatusCode::BAD_REQUEST
            }
            CallError::IdempotencyConflict
            | CallError::InvitationNotActionable
            | CallError::AnsweredElsewhere
            | CallError::TargetUnavailable => StatusCode::CONFLICT,
            CallError::InvitationExpired => StatusCode::GONE,
            CallError::CapabilityUnavailable(_) | CallError::ParticipantLimitExceeded { .. } => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            CallError::MediaUnavailable
            | CallError::PrivacyAuthorizationUnavailable
            | CallError::ArtifactStorageUnavailable
            | CallError::PublicRouteUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            CallError::PublicRouteDenied(_) => StatusCode::FORBIDDEN,
            CallError::TelecomSpendLimitExceeded(_) => StatusCode::TOO_MANY_REQUESTS,
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

/// Errors reaching the HTTP layer: a stable code, its status and a fixed safe message.
/// Database detail is logged, never returned; foreign-scope resources are NOT_FOUND.
impl From<CallError> for CallingError {
    fn from(err: CallError) -> Self {
        let code = match &err {
            CallError::NotFound => return CallingError::NotFound,
            CallError::RateLimited { retry_after_secs } => {
                return CallingError::RateLimited {
                    retry_after_secs: *retry_after_secs,
                }
            }
            CallError::Validation(message) => return CallingError::Validation(message),
            CallError::FailureCategoryNotAllowed(_) => {
                return CallingError::Validation("The failure category is not allowed.")
            }
            CallError::Database {
                summary,
                connectivity,
            } => {
                tracing::error!(%summary, "call database error");
                return if *connectivity {
                    CallingError::ServiceUnavailable
                } else {
                    CallingError::Internal
                };
            }
            CallError::StaleVersion { .. } => ErrorCode::StaleCallVersion,
            CallError::Terminal { .. } | CallError::InvalidTransition { .. } => {
                ErrorCode::CallStateConflict
            }
            CallError::InsufficientEvidence(_) => ErrorCode::CallEvidenceRequired,
            CallError::AuthorizationRequired
            | CallError::AuthorizationDenied
            | CallError::LiveTelecomNotAllowed => ErrorCode::CallNotAuthorized,
            CallError::PrincipalCategoryNotAllowed => ErrorCode::PrincipalNotEligible,
            CallError::ConcurrentCallLimitExceeded { .. } => ErrorCode::ConcurrentCallLimitExceeded,
            CallError::InvalidState(_) => ErrorCode::CallResourceStateConflict,
            CallError::Conflict(_) => ErrorCode::Conflict,
            CallError::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            CallError::InvitationExpired => ErrorCode::InvitationExpired,
            CallError::InvitationNotActionable => ErrorCode::InvitationNotActionable,
            CallError::AnsweredElsewhere => ErrorCode::AnsweredElsewhere,
            CallError::CapabilityUnavailable(_) => ErrorCode::CapabilityUnavailable,
            CallError::MediaUnavailable => ErrorCode::MediaUnavailable,
            CallError::ParticipantLimitExceeded { .. } => ErrorCode::ParticipantLimitExceeded,
            CallError::TargetUnavailable => ErrorCode::TargetUnavailable,
            CallError::ArtifactStorageUnavailable => ErrorCode::ArtifactStorageUnavailable,
            CallError::PrivacyAuthorizationUnavailable => {
                ErrorCode::PrivacyAuthorizationUnavailable
            }
            CallError::PublicRouteDenied(_) => ErrorCode::PublicRouteDenied,
            CallError::PublicRouteUnavailable(_) => ErrorCode::PublicRouteUnavailable,
            CallError::TelecomSpendLimitExceeded(_) => ErrorCode::TelecomSpendLimitExceeded,
        };
        tracing::info!(code = code.as_str(), error = %err, "call operation refused");
        CallingError::Call {
            code,
            status: err.http_status(),
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
