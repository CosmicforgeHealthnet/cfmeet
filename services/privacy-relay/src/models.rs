//! Persisted rows of the Privacy Relay tables (SeaORM `FromQueryResult`). Text columns
//! hold the canonical vocabulary (CHECK-constrained); typed accessors parse them and
//! fail closed on anything unexpected.

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    context::policy::ContextPolicy,
    error::{ErrorCode, RelayError, RelayResult},
    vocabulary::{
        CommunicationChannel, CommunicationContextStatus, DirectionPolicy, ParticipantSubjectKind,
        PrivacyRelaySessionStatus, RelayStrategy, SnapshotScope,
    },
};

fn corrupt(what: &'static str) -> RelayError {
    tracing::error!(what, "persisted privacy relay row has an unexpected value");
    RelayError::Refused(ErrorCode::InternalError)
}

pub const CONTEXT_COLUMNS: &str = "id, tenant_id, application_id, environment_id, environment_kind, \
    external_context_reference, version, status, direction_policy, allowed_relay_strategies, starts_at, \
    expires_at, grace_expires_at, final_expires_at, policy_version, policy_snapshot, \
    created_by_service_account_id, created_at, updated_at, suspended_at, completed_at, revoked_at, expired_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct ContextRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub external_context_reference: String,
    pub version: i64,
    pub status: String,
    pub direction_policy: String,
    pub allowed_relay_strategies: Vec<String>,
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub final_expires_at: DateTime<Utc>,
    pub policy_version: String,
    pub policy_snapshot: Value,
    pub created_by_service_account_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
}

impl ContextRow {
    pub fn status(&self) -> RelayResult<CommunicationContextStatus> {
        CommunicationContextStatus::parse(&self.status).ok_or_else(|| corrupt("context status"))
    }

    pub fn direction_policy(&self) -> RelayResult<DirectionPolicy> {
        DirectionPolicy::parse(&self.direction_policy).ok_or_else(|| corrupt("direction policy"))
    }

    pub fn policy(&self) -> RelayResult<ContextPolicy> {
        serde_json::from_value(self.policy_snapshot.clone()).map_err(|_| corrupt("context policy"))
    }

    pub fn strategies(&self) -> RelayResult<Vec<RelayStrategy>> {
        self.allowed_relay_strategies
            .iter()
            .map(|s| RelayStrategy::parse(s).ok_or_else(|| corrupt("relay strategy")))
            .collect()
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ParticipantRow {
    pub id: Uuid,
    pub context_id: Uuid,
    pub subject_kind: String,
    pub principal_ref: Option<Uuid>,
    pub contact_ref: Option<Uuid>,
    pub added_in_version: i64,
    pub removed_in_version: Option<i64>,
    pub added_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

pub const PARTICIPANT_COLUMNS: &str = "id, context_id, subject_kind, principal_ref, contact_ref, \
    added_in_version, removed_in_version, added_at, removed_at";

impl ParticipantRow {
    pub fn subject_kind(&self) -> RelayResult<ParticipantSubjectKind> {
        ParticipantSubjectKind::parse(&self.subject_kind).ok_or_else(|| corrupt("subject kind"))
    }

    pub fn is_active(&self) -> bool {
        self.removed_at.is_none()
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ChannelRow {
    pub channel: String,
    pub enabled: bool,
    pub changed_in_version: i64,
}

impl ChannelRow {
    pub fn channel(&self) -> RelayResult<CommunicationChannel> {
        CommunicationChannel::parse(&self.channel).ok_or_else(|| corrupt("channel"))
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct DirectionRow {
    pub id: Uuid,
    pub from_participant_id: Uuid,
    pub to_participant_id: Uuid,
    pub channel: Option<String>,
    pub added_in_version: i64,
    pub removed_in_version: Option<i64>,
}

pub const DIRECTION_COLUMNS: &str =
    "id, from_participant_id, to_participant_id, channel, added_in_version, removed_in_version";

impl DirectionRow {
    pub fn channel(&self) -> RelayResult<Option<CommunicationChannel>> {
        self.channel
            .as_deref()
            .map(|c| CommunicationChannel::parse(c).ok_or_else(|| corrupt("direction channel")))
            .transpose()
    }
}

pub const SESSION_COLUMNS: &str = "id, context_id, tenant_id, application_id, environment_id, \
    source_context_version, version, status, strategy, direction_policy, participant_scope, \
    channel_scope, direction_scope, starts_at, expires_at, grace_expires_at, final_expires_at, \
    policy_version, policy_snapshot, relay_identity_binding_ref, created_by_service_account_id, \
    created_at, updated_at, suspended_at, revoked_at, completed_at, expired_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct SessionRow {
    pub id: Uuid,
    pub context_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub source_context_version: i64,
    pub version: i64,
    pub status: String,
    pub strategy: String,
    pub direction_policy: String,
    pub participant_scope: String,
    pub channel_scope: String,
    pub direction_scope: String,
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub final_expires_at: DateTime<Utc>,
    pub policy_version: String,
    pub policy_snapshot: Value,
    pub relay_identity_binding_ref: Option<Uuid>,
    pub created_by_service_account_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
}

impl SessionRow {
    pub fn status(&self) -> RelayResult<PrivacyRelaySessionStatus> {
        PrivacyRelaySessionStatus::parse(&self.status).ok_or_else(|| corrupt("session status"))
    }

    pub fn strategy(&self) -> RelayResult<RelayStrategy> {
        RelayStrategy::parse(&self.strategy).ok_or_else(|| corrupt("session strategy"))
    }

    pub fn direction_policy(&self) -> RelayResult<DirectionPolicy> {
        DirectionPolicy::parse(&self.direction_policy).ok_or_else(|| corrupt("direction policy"))
    }

    pub fn participant_scope(&self) -> RelayResult<SnapshotScope> {
        SnapshotScope::parse(&self.participant_scope).ok_or_else(|| corrupt("participant scope"))
    }

    pub fn channel_scope(&self) -> RelayResult<SnapshotScope> {
        SnapshotScope::parse(&self.channel_scope).ok_or_else(|| corrupt("channel scope"))
    }

    pub fn direction_scope(&self) -> RelayResult<SnapshotScope> {
        SnapshotScope::parse(&self.direction_scope).ok_or_else(|| corrupt("direction scope"))
    }

    pub fn policy(&self) -> RelayResult<ContextPolicy> {
        serde_json::from_value(self.policy_snapshot.clone()).map_err(|_| corrupt("session policy"))
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct SessionParticipantRow {
    pub id: Uuid,
    pub context_participant_id: Uuid,
    pub subject_kind: String,
    pub principal_ref: Option<Uuid>,
    pub contact_ref: Option<Uuid>,
    pub status: String,
    pub source_context_version: i64,
    pub added_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub const SESSION_PARTICIPANT_COLUMNS: &str = "id, context_participant_id, subject_kind, \
    principal_ref, contact_ref, status, source_context_version, added_at, revoked_at";

#[derive(Debug, Clone, FromQueryResult)]
pub struct SessionChannelRow {
    pub channel: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct SessionDirectionRow {
    pub id: Uuid,
    pub from_participant_id: Uuid,
    pub to_participant_id: Uuid,
    pub channel: Option<String>,
    pub status: String,
}

pub const SESSION_DIRECTION_COLUMNS: &str =
    "id, from_participant_id, to_participant_id, channel, status";

impl SessionDirectionRow {
    pub fn channel(&self) -> RelayResult<Option<CommunicationChannel>> {
        self.channel
            .as_deref()
            .map(|c| CommunicationChannel::parse(c).ok_or_else(|| corrupt("direction channel")))
            .transpose()
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct CommandRow {
    pub id: Uuid,
    pub request_fingerprint: String,
    pub state: String,
    pub response_status: Option<i16>,
    pub response_snapshot: Option<Value>,
    pub result_resource_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct EnforcementJobRow {
    pub id: Uuid,
    pub enforcement_event_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub on_behalf_of_service_account_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    pub relay_session_id: Option<Uuid>,
    pub session_version: Option<i64>,
    pub reason: String,
    pub affected_principal_refs: Vec<Uuid>,
    pub terminate_active_calls: bool,
    pub payload: Value,
    pub status: String,
    pub attempt_count: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub last_error_category: Option<String>,
    pub result_snapshot: Option<Value>,
    pub request_id: String,
    pub correlation_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub const ENFORCEMENT_JOB_COLUMNS: &str = "id, enforcement_event_id, tenant_id, application_id, \
    environment_id, environment_kind, on_behalf_of_service_account_id, context_id, context_version, \
    relay_session_id, session_version, reason, affected_principal_refs, terminate_active_calls, \
    payload, status, attempt_count, next_attempt_at, lease_owner, lease_expires_at, \
    last_error_category, result_snapshot, request_id, correlation_id, created_at, updated_at, completed_at";
