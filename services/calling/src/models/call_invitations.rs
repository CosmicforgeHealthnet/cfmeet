use sea_orm::entity::prelude::*;

use crate::calls::kinds::{InvitationPurpose, InvitationStatus, InvitationTerminalReason};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_invitations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub participant_id: Uuid,
    pub principal_id: Uuid,
    pub endpoint_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub session_id: Option<Uuid>,
    pub transfer_id: Option<Uuid>,
    pub purpose: InvitationPurpose,
    pub status: InvitationStatus,
    pub invitation_generation: i32,
    pub issued_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub ringing_at: Option<DateTimeUtc>,
    pub accepted_at: Option<DateTimeUtc>,
    pub declined_at: Option<DateTimeUtc>,
    pub cancelled_at: Option<DateTimeUtc>,
    pub finished_at: Option<DateTimeUtc>,
    pub terminal_reason: Option<InvitationTerminalReason>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
