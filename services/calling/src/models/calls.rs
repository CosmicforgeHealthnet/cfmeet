//! `calls` row (aggregate root). Mapped to [`crate::calls::aggregate::Call`] before it
//! leaves the call domain.

use sea_orm::entity::prelude::*;

use crate::calls::{
    kinds::{CallKind, FailureCategory, TerminalReason},
    state::CallState,
};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "calls")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub created_by_service_account_id: Uuid,
    pub created_by_credential_id: Uuid,
    pub initiator_principal_id: Uuid,
    pub call_kind: CallKind,
    pub current_state: CallState,
    pub version: i64,
    pub communication_context_ref: Option<Uuid>,
    pub privacy_relay_ref: Option<Uuid>,
    pub terminal_reason: Option<TerminalReason>,
    pub failure_category: Option<FailureCategory>,
    pub requested_at: DateTimeUtc,
    pub authorized_at: Option<DateTimeUtc>,
    pub resolving_at: Option<DateTimeUtc>,
    pub routing_at: Option<DateTimeUtc>,
    pub dialing_at: Option<DateTimeUtc>,
    pub ringing_at: Option<DateTimeUtc>,
    pub answered_at: Option<DateTimeUtc>,
    pub connected_at: Option<DateTimeUtc>,
    pub ending_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
