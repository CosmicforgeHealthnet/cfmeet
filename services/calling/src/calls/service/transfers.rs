//! Application-to-application transfer records (Part C §25) — transaction-scoped
//! primitives. The transfer's sub-states live in `call_transfers`, never in the Call
//! state: the Call is TRANSFERRING while a transfer is in flight and returns to
//! CONNECTED (completed or recovered) or HELD (recovered from a held Call).

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
};
use uuid::Uuid;

use super::{data, text, CallService, EventDraft};
use crate::{
    calls::{
        command::CallCommandContext,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::CallTransferId,
        kinds::{FailureCategory, TransferFailureReason, TransferState},
        state::CallState,
    },
    models::{call_transfers, calls},
};

/// Input of [`CallService::record_transfer_in`].
pub(crate) struct NewTransfer {
    pub command_id: Uuid,
    pub transferring_participant_id: Uuid,
    pub target_principal_id: Uuid,
    pub pre_transfer_state: CallState,
    pub expected_call_version: i64,
}

impl CallService {
    /// Records a transfer REQUESTED with CALL_TRANSFER_REQUESTED.
    pub(crate) async fn record_transfer_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewTransfer,
    ) -> CallResult<call_transfers::Model> {
        if !matches!(
            new.pre_transfer_state,
            CallState::Connected | CallState::Held
        ) {
            return Err(CallError::InvalidTransition {
                state: new.pre_transfer_state,
                action: "TRANSFER",
            });
        }
        let at = self.now().max(call.updated_at);
        let transfer = call_transfers::ActiveModel {
            id: Set(CallTransferId::generate().as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            transferring_participant_id: Set(new.transferring_participant_id),
            requested_target_principal_id: Set(new.target_principal_id),
            target_participant_id: Set(None),
            new_route_id: Set(None),
            new_leg_id: Set(None),
            attempt_id: Set(None),
            command_id: Set(new.command_id),
            state: Set(TransferState::Requested),
            pre_transfer_state: Set(new.pre_transfer_state),
            expected_call_version: Set(new.expected_call_version),
            failure_category: Set(None),
            failure_reason: Set(None),
            initiated_at: Set(at),
            ringing_at: Set(None),
            accepted_at: Set(None),
            completed_at: Set(None),
            failed_at: Set(None),
            cancelled_at: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallTransferRequested,
                data(vec![
                    ("transfer_id", text(transfer.id)),
                    (
                        "transferring_participant_id",
                        text(transfer.transferring_participant_id),
                    ),
                    ("pre_transfer_state", text(transfer.pre_transfer_state)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(transfer)
    }

    /// REQUESTED → RINGING_TARGET with the target participant and the transfer route /
    /// leg / attempt evidence.
    pub(crate) async fn transfer_ringing_in(
        &self,
        txn: &DatabaseTransaction,
        transfer: call_transfers::Model,
        target_participant_id: Uuid,
        route_id: Uuid,
        leg_id: Uuid,
        attempt_id: Uuid,
    ) -> CallResult<call_transfers::Model> {
        let at = self.now().max(transfer.updated_at);
        let mut active: call_transfers::ActiveModel = transfer.into();
        active.state = Set(TransferState::RingingTarget);
        active.target_participant_id = Set(Some(target_participant_id));
        active.new_route_id = Set(Some(route_id));
        active.new_leg_id = Set(Some(leg_id));
        active.attempt_id = Set(Some(attempt_id));
        active.ringing_at = Set(Some(at));
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// RINGING_TARGET → ACCEPTED (the target's winning device answered).
    pub(crate) async fn transfer_accepted_in(
        &self,
        txn: &DatabaseTransaction,
        transfer: call_transfers::Model,
    ) -> CallResult<call_transfers::Model> {
        if transfer.state != TransferState::RingingTarget {
            return Err(CallError::InvalidState("the transfer is not ringing"));
        }
        let at = self.now().max(transfer.updated_at);
        let mut active: call_transfers::ActiveModel = transfer.into();
        active.state = Set(TransferState::Accepted);
        active.accepted_at = Set(Some(at));
        active.updated_at = Set(at);
        Ok(active.update(txn).await?)
    }

    /// ACCEPTED → COMPLETED with CALL_TRANSFER_COMPLETED.
    pub(crate) async fn transfer_completed_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        transfer: call_transfers::Model,
    ) -> CallResult<call_transfers::Model> {
        if transfer.state != TransferState::Accepted {
            return Err(CallError::InvalidState("the transfer was not accepted"));
        }
        let at = self.now().max(transfer.updated_at);
        let mut active: call_transfers::ActiveModel = transfer.into();
        active.state = Set(TransferState::Completed);
        active.completed_at = Set(Some(at));
        active.updated_at = Set(at);
        let transfer = active.update(txn).await?;
        let mut details = vec![("transfer_id", text(transfer.id))];
        if let Some(target) = transfer.target_participant_id {
            details.push(("target_participant_id", text(target)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallTransferCompleted, data(details)?),
            at,
        )
        .await?;
        Ok(transfer)
    }

    /// In-flight → FAILED with CALL_TRANSFER_FAILED.
    pub(crate) async fn fail_transfer_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        transfer: call_transfers::Model,
        reason: TransferFailureReason,
        category: FailureCategory,
    ) -> CallResult<call_transfers::Model> {
        if !transfer.state.is_active() {
            return Err(CallError::InvalidState("the transfer is not in flight"));
        }
        let at = self.now().max(transfer.updated_at);
        let mut active: call_transfers::ActiveModel = transfer.into();
        active.state = Set(TransferState::Failed);
        active.failed_at = Set(Some(at));
        active.failure_reason = Set(Some(reason));
        active.failure_category = Set(Some(category));
        active.updated_at = Set(at);
        let transfer = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallTransferFailed,
                data(vec![
                    ("transfer_id", text(transfer.id)),
                    ("failure_reason", text(reason)),
                    ("failure_category", text(category)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(transfer)
    }

    /// In-flight → CANCELLED with CALL_TRANSFER_CANCELLED (the Call ended or the
    /// transfer was withdrawn).
    pub(crate) async fn cancel_transfer_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        transfer: call_transfers::Model,
        reason: TransferFailureReason,
    ) -> CallResult<call_transfers::Model> {
        if !transfer.state.is_active() {
            return Ok(transfer);
        }
        let at = self.now().max(transfer.updated_at);
        let mut active: call_transfers::ActiveModel = transfer.into();
        active.state = Set(TransferState::Cancelled);
        active.cancelled_at = Set(Some(at));
        active.failure_reason = Set(Some(reason));
        active.updated_at = Set(at);
        let transfer = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallTransferCancelled,
                data(vec![
                    ("transfer_id", text(transfer.id)),
                    ("reason", text(reason)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(transfer)
    }

    /// The Call's in-flight transfer, if any.
    pub(crate) async fn active_transfer(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<Option<call_transfers::Model>> {
        Ok(call_transfers::Entity::find()
            .filter(call_transfers::Column::CallId.eq(call.id))
            .filter(call_transfers::Column::State.is_in([
                TransferState::Requested,
                TransferState::RingingTarget,
                TransferState::Accepted,
            ]))
            .one(txn)
            .await?)
    }
}
