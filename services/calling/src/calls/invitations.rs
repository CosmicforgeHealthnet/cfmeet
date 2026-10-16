//! Ring offers (Part C §20) and transfers (§25) as read by the public API.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::{
    ids::{CallId, CallInvitationId, CallParticipantId, CallTransferId},
    kinds::{
        FailureCategory, InvitationPurpose, InvitationStatus, InvitationTerminalReason,
        TransferFailureReason, TransferState,
    },
    state::CallState,
};
use crate::models::{call_invitations, call_transfers};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallInvitation {
    pub id: CallInvitationId,
    pub call_id: CallId,
    pub participant_id: CallParticipantId,
    pub endpoint_id: Uuid,
    pub purpose: InvitationPurpose,
    pub status: InvitationStatus,
    pub invitation_generation: i32,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub terminal_reason: Option<InvitationTerminalReason>,
    pub session_id: Option<Uuid>,
    pub transfer_id: Option<Uuid>,
}

impl From<call_invitations::Model> for CallInvitation {
    fn from(m: call_invitations::Model) -> Self {
        Self {
            id: CallInvitationId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            participant_id: CallParticipantId::from_uuid(m.participant_id),
            endpoint_id: m.endpoint_id,
            purpose: m.purpose,
            status: m.status,
            invitation_generation: m.invitation_generation,
            issued_at: m.issued_at,
            expires_at: m.expires_at,
            finished_at: m.finished_at,
            terminal_reason: m.terminal_reason,
            session_id: m.session_id,
            transfer_id: m.transfer_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallTransfer {
    pub id: CallTransferId,
    pub call_id: CallId,
    pub transferring_participant_id: CallParticipantId,
    pub target_principal_id: Uuid,
    pub target_participant_id: Option<CallParticipantId>,
    pub state: TransferState,
    pub pre_transfer_state: CallState,
    pub failure_reason: Option<TransferFailureReason>,
    pub failure_category: Option<FailureCategory>,
    pub initiated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

impl From<call_transfers::Model> for CallTransfer {
    fn from(m: call_transfers::Model) -> Self {
        Self {
            id: CallTransferId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            transferring_participant_id: CallParticipantId::from_uuid(
                m.transferring_participant_id,
            ),
            target_principal_id: m.requested_target_principal_id,
            target_participant_id: m.target_participant_id.map(CallParticipantId::from_uuid),
            state: m.state,
            pre_transfer_state: m.pre_transfer_state,
            failure_reason: m.failure_reason,
            failure_category: m.failure_category,
            initiated_at: m.initiated_at,
            completed_at: m.completed_at,
            failed_at: m.failed_at,
            cancelled_at: m.cancelled_at,
        }
    }
}
