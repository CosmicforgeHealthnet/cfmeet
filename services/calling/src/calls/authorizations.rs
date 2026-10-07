//! Call authorization decisions (Part C §16, §18).
//!
//! This is the persisted decision aggregate and its relationship to the state machine —
//! not the policy engine, which arrives with the next workstream. A decision records
//! APPROVED or DENIED with its policy version, reason code, bounded policy / limits
//! snapshots and safe evidence (actor service account, credential or internal service,
//! the Call's initiating principal, request and correlation ids).
//!
//! Recording a decision supersedes the previous one, so a Call has exactly one effective
//! decision. Rules:
//!
//! * REQUESTED → AUTHORIZED requires the Call's effective decision to be APPROVED;
//! * a DENIED decision never allows AUTHORIZED. There is no canonical DENIED call state:
//!   the decision stays its own evidence, and `DenyByPolicy` maps it to FAILED with the
//!   normalized failure category POLICY;
//! * a later DENIED decision (e.g. reason REVOKED) withdraws an earlier approval, which
//!   closes the paid public-network guard ([`PublicNetworkAuthorization`]).

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    attributes::SafeAttributes,
    ids::{CallAuthorizationId, CallId},
    kinds::{ActorKind, AuthorizationDecision, AuthorizationReason},
    routes::PolicyVersion,
    state::CallState,
};
use crate::{
    domain::{CredentialId, PrincipalId, ServiceAccountId},
    models::call_authorizations,
};

/// A decision to record. The decision itself follows from the reason code.
#[derive(Debug, Clone)]
pub struct NewAuthorization {
    pub reason: AuthorizationReason,
    pub policy_version: PolicyVersion,
    pub policy_snapshot: SafeAttributes,
    pub limits_snapshot: SafeAttributes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallAuthorization {
    pub id: CallAuthorizationId,
    pub call_id: CallId,
    pub decision: AuthorizationDecision,
    pub reason: AuthorizationReason,
    pub policy_version: String,
    pub actor_kind: ActorKind,
    pub evidence_service_account_id: ServiceAccountId,
    pub evidence_credential_id: Option<CredentialId>,
    pub evidence_service: Option<String>,
    pub evidence_principal_id: PrincipalId,
    pub request_id: String,
    pub correlation_id: String,
    pub decided_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub superseded_by: Option<CallAuthorizationId>,
}

impl CallAuthorization {
    pub fn is_effective(&self) -> bool {
        self.superseded_at.is_none()
    }
}

impl From<call_authorizations::Model> for CallAuthorization {
    fn from(m: call_authorizations::Model) -> Self {
        Self {
            id: CallAuthorizationId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            decision: m.decision,
            reason: m.reason_code,
            policy_version: m.policy_version,
            actor_kind: m.actor_kind,
            evidence_service_account_id: ServiceAccountId::from_uuid(m.evidence_service_account_id),
            evidence_credential_id: m.evidence_credential_id.map(CredentialId::from_uuid),
            evidence_service: m.evidence_service,
            evidence_principal_id: PrincipalId::from_uuid(m.evidence_principal_id),
            request_id: m.request_id,
            correlation_id: m.correlation_id,
            decided_at: m.decided_at,
            superseded_at: m.superseded_at,
            superseded_by: m
                .superseded_by_authorization_id
                .map(CallAuthorizationId::from_uuid),
        }
    }
}

/// States in which an authorized Call may perform paid public-network actions.
pub const PAID_ACTION_STATES: &[CallState] = &[
    CallState::Authorized,
    CallState::Resolving,
    CallState::Routing,
    CallState::Dialing,
    CallState::Ringing,
    CallState::Answered,
    CallState::Connected,
    CallState::Held,
    CallState::Transferring,
];

/// Proof, produced only by
/// [`CallService::require_public_network_authorization`](super::service::CallService::require_public_network_authorization),
/// that a Call may perform a paid public-network action now: the Call exists in the
/// caller's scope, is in an executable state, its effective decision is APPROVED and its
/// environment is allowed live telecom. Future execution commands take this value as an
/// argument, so they cannot be issued without the check. It cannot be constructed
/// elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicNetworkAuthorization {
    call_id: CallId,
    authorization_id: CallAuthorizationId,
    call_version: i64,
    checked_at: DateTime<Utc>,
}

impl PublicNetworkAuthorization {
    pub(crate) fn new(
        call_id: CallId,
        authorization_id: CallAuthorizationId,
        call_version: i64,
        checked_at: DateTime<Utc>,
    ) -> Self {
        Self {
            call_id,
            authorization_id,
            call_version,
            checked_at,
        }
    }

    pub fn call_id(&self) -> CallId {
        self.call_id
    }

    pub fn authorization_id(&self) -> CallAuthorizationId {
        self.authorization_id
    }

    pub fn call_version(&self) -> i64 {
        self.call_version
    }

    pub fn checked_at(&self) -> DateTime<Utc> {
        self.checked_at
    }
}
