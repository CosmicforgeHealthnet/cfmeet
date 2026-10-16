//! The SMS transport domain.
//!
//! A **transport** is one message, inbound or outbound, with its state machine (enforced
//! by the database as well):
//!
//! ```text
//! OUTBOUND  REQUESTED ─▶ AUTHORIZED ─▶ SUBMITTING ─▶ SUBMITTED ─▶ DELIVERED
//!               │            │   ▲          │  │          │
//!               ▼            ▼   └─(NOT_SENT, retry)      ├─▶ FAILED
//!            FAILED       EXPIRED          │  └─▶ UNKNOWN ┘   (provider evidence only)
//!                                          ▼
//!                                        FAILED
//! INBOUND   RECEIVED
//! ```
//!
//! UNKNOWN means the provider MAY hold the message: nothing resubmits it; only provider
//! evidence (a delivery receipt or a status query) resolves it.

pub mod controls;
pub mod e164;
pub mod events;
pub mod protection;
pub mod segments;
pub mod store;
pub mod threads;
pub mod workers;

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use uuid::Uuid;

/// Transport status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Requested,
    Authorized,
    Submitting,
    Submitted,
    Delivered,
    Received,
    Failed,
    Unknown,
    Expired,
}

impl Status {
    pub const ALL: [Status; 9] = [
        Status::Requested,
        Status::Authorized,
        Status::Submitting,
        Status::Submitted,
        Status::Delivered,
        Status::Received,
        Status::Failed,
        Status::Unknown,
        Status::Expired,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Requested => "REQUESTED",
            Status::Authorized => "AUTHORIZED",
            Status::Submitting => "SUBMITTING",
            Status::Submitted => "SUBMITTED",
            Status::Delivered => "DELIVERED",
            Status::Received => "RECEIVED",
            Status::Failed => "FAILED",
            Status::Unknown => "UNKNOWN",
            Status::Expired => "EXPIRED",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Status::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Final: no further transition.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Status::Delivered | Status::Received | Status::Failed | Status::Expired
        )
    }

    /// The transitions the domain (and the database) allow.
    pub fn may_become(&self, next: Status) -> bool {
        use Status::*;
        matches!(
            (self, next),
            (Requested, Authorized | Failed | Expired)
                | (Authorized, Submitting | Failed | Expired)
                | (Submitting, Submitted | Failed | Unknown | Authorized)
                | (Submitted, Delivered | Failed | Unknown)
                | (Unknown, Submitted | Delivered | Failed)
        )
    }
}

/// Direction of a transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outbound,
    Inbound,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Outbound => "OUTBOUND",
            Direction::Inbound => "INBOUND",
        }
    }
}

/// One transport row (sealed values stay sealed here).
#[derive(Debug, Clone, FromQueryResult)]
pub struct TransportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub direction: String,
    pub status: String,
    pub number_resource_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub routing_revision: Option<i64>,
    pub principal_id: Option<Uuid>,
    pub target_kind: Option<String>,
    pub target_ref: Option<Uuid>,
    pub remote_sealed: String,
    pub remote_fingerprint: String,
    pub remote_masked: String,
    pub sender_sealed: Option<String>,
    pub content_sealed: Option<String>,
    pub content_purged_at: Option<DateTime<Utc>>,
    pub content_encoding: String,
    pub char_count: i32,
    pub segment_count: i16,
    pub thread_id: Uuid,
    pub service_account_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub request_digest: Option<String>,
    pub sender_grant_id: Option<Uuid>,
    pub provider_id: Option<String>,
    pub estimated_cost_minor: Option<i64>,
    pub confirmed_cost_minor: Option<i64>,
    pub cost_currency: Option<String>,
    pub failure_code: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt_count: i16,
    pub max_attempts: i16,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub received_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub correlation_id: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const TRANSPORT_COLUMNS: &str = "id, tenant_id, application_id, environment_id, direction, status, \
    number_resource_id, assignment_id, routing_revision, principal_id, target_kind, target_ref, remote_sealed, \
    remote_fingerprint, remote_masked, sender_sealed, content_sealed, content_purged_at, content_encoding, \
    char_count, segment_count, thread_id, service_account_id, idempotency_key, request_digest, sender_grant_id, \
    provider_id, estimated_cost_minor, confirmed_cost_minor, cost_currency, failure_code, blocked_reason, \
    attempt_count, max_attempts, next_attempt_at, expires_at, submitted_at, delivered_at, received_at, failed_at, \
    correlation_id, version, created_at, updated_at";

impl TransportRow {
    pub fn status(&self) -> Status {
        Status::parse(&self.status).unwrap_or(Status::Unknown)
    }

    pub fn scope(&self) -> events::EventScope {
        events::EventScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}

/// Domain errors (stable codes; never a number, content or provider text).
#[derive(Debug, thiserror::Error)]
pub enum SmsError {
    #[error("{0}")]
    Validation(&'static str),
    #[error("not found")]
    NotFound,
    /// A safety control refused the message (recorded as a FAILED transport).
    #[error("blocked ({reason})")]
    Blocked {
        reason: &'static str,
        message_id: Option<Uuid>,
    },
    /// A rate control refused the message (not recorded).
    #[error("rate limited ({reason})")]
    RateLimited {
        reason: &'static str,
        retry_after_seconds: u64,
    },
    #[error("the content needs too many segments")]
    ContentTooLong,
    #[error("the idempotency key was used for a different request")]
    IdempotencyKeyReused,
    #[error("numbering unavailable")]
    NumberingUnavailable,
    #[error("no SMS provider is available")]
    ProviderUnavailable,
    #[error("the identity authority is unavailable")]
    AuthorityUnavailable,
    #[error("conflict ({0})")]
    Conflict(&'static str),
    #[error("database error ({summary})")]
    Database { summary: String, connectivity: bool },
}

impl From<sea_orm::DbErr> for SmsError {
    fn from(e: sea_orm::DbErr) -> Self {
        SmsError::Database {
            summary: crate::db::summarize_db_error(&e),
            connectivity: crate::db::is_connectivity_error(&e),
        }
    }
}

pub type SmsResult<T> = Result<T, SmsError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_machine_never_leaves_a_final_state_or_resubmits_an_unknown() {
        for s in Status::ALL {
            assert_eq!(Status::parse(s.as_str()), Some(s));
            if s.is_final() {
                assert!(Status::ALL.iter().all(|n| !s.may_become(*n)), "{s:?}");
            }
        }
        assert!(
            !Status::Unknown.may_become(Status::Submitting),
            "never a blind resubmission"
        );
        assert!(!Status::Unknown.may_become(Status::Authorized));
        assert!(!Status::Submitted.may_become(Status::Submitting));
        assert!(
            Status::Submitting.may_become(Status::Authorized),
            "definitely unsent: retry"
        );
        assert!(
            Status::Unknown.may_become(Status::Delivered),
            "provider evidence"
        );
    }
}
