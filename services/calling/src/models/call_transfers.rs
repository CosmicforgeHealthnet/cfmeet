use sea_orm::entity::prelude::*;

use crate::calls::{
    kinds::{FailureCategory, TransferFailureReason, TransferState},
    state::CallState,
};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_transfers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub transferring_participant_id: Uuid,
    pub requested_target_principal_id: Uuid,
    pub target_participant_id: Option<Uuid>,
    pub new_route_id: Option<Uuid>,
    pub new_leg_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub command_id: Uuid,
    pub state: TransferState,
    pub pre_transfer_state: CallState,
    pub expected_call_version: i64,
    pub failure_category: Option<FailureCategory>,
    pub failure_reason: Option<TransferFailureReason>,
    pub initiated_at: DateTimeUtc,
    pub ringing_at: Option<DateTimeUtc>,
    pub accepted_at: Option<DateTimeUtc>,
    pub completed_at: Option<DateTimeUtc>,
    pub failed_at: Option<DateTimeUtc>,
    pub cancelled_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
