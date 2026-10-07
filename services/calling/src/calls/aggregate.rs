//! The Call aggregate root as the domain sees it (never the database row itself).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    command::CallScope,
    ids::CallId,
    kinds::{CallKind, FailureCategory, TerminalReason},
    state::CallState,
};
use crate::{
    domain::{ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId},
    models::calls,
};

/// First-entry timestamps of the lifecycle states. Each is written once, when the Call
/// first enters the corresponding state, and never rewritten (database-enforced).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallTimestamps {
    pub requested_at: DateTime<Utc>,
    pub authorized_at: Option<DateTime<Utc>>,
    pub resolving_at: Option<DateTime<Utc>>,
    pub routing_at: Option<DateTime<Utc>>,
    pub dialing_at: Option<DateTime<Utc>>,
    pub ringing_at: Option<DateTime<Utc>>,
    pub answered_at: Option<DateTime<Utc>>,
    pub connected_at: Option<DateTime<Utc>>,
    pub ending_at: Option<DateTime<Utc>>,
    /// When the Call reached a terminal state (any of the six).
    pub ended_at: Option<DateTime<Utc>>,
}

/// The canonical, provider-independent logical Call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub id: CallId,
    pub scope: CallScope,
    pub created_by_service_account_id: ServiceAccountId,
    pub created_by_credential_id: CredentialId,
    pub initiator_principal_id: PrincipalId,
    pub kind: CallKind,
    pub state: CallState,
    /// Optimistic-concurrency version: 1 at creation, +1 per state transition.
    pub version: i64,
    /// Opaque reference to a communication context owned by another service.
    pub communication_context_ref: Option<Uuid>,
    /// Opaque reference to a relay record owned by another service.
    pub privacy_relay_ref: Option<Uuid>,
    pub terminal_reason: Option<TerminalReason>,
    pub failure_category: Option<FailureCategory>,
    pub timestamps: CallTimestamps,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Call {
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

impl From<calls::Model> for Call {
    fn from(m: calls::Model) -> Self {
        Self {
            id: CallId::from_uuid(m.id),
            scope: CallScope::from_parts(
                TenantId::from_uuid(m.tenant_id),
                ApplicationId::from_uuid(m.application_id),
                EnvironmentId::from_uuid(m.environment_id),
            ),
            created_by_service_account_id: ServiceAccountId::from_uuid(
                m.created_by_service_account_id,
            ),
            created_by_credential_id: CredentialId::from_uuid(m.created_by_credential_id),
            initiator_principal_id: PrincipalId::from_uuid(m.initiator_principal_id),
            kind: m.call_kind,
            state: m.current_state,
            version: m.version,
            communication_context_ref: m.communication_context_ref,
            privacy_relay_ref: m.privacy_relay_ref,
            terminal_reason: m.terminal_reason,
            failure_category: m.failure_category,
            timestamps: CallTimestamps {
                requested_at: m.requested_at,
                authorized_at: m.authorized_at,
                resolving_at: m.resolving_at,
                routing_at: m.routing_at,
                dialing_at: m.dialing_at,
                ringing_at: m.ringing_at,
                answered_at: m.answered_at,
                connected_at: m.connected_at,
                ending_at: m.ending_at,
                ended_at: m.ended_at,
            },
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}
