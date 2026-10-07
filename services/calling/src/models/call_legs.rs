//! Canonical Calling-side leg. Provider / carrier identifiers and payloads are not
//! columns here: execution detail belongs to the Telephony boundary, which Calling only
//! references through the opaque `execution_binding_ref`.

use sea_orm::entity::prelude::*;

use crate::calls::kinds::{FailureCategory, LegDirection, LegKind, LegState};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_legs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub route_id: Option<Uuid>,
    pub participant_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub direction: LegDirection,
    pub leg_kind: LegKind,
    pub state: LegState,
    pub failure_category: Option<FailureCategory>,
    pub execution_binding_ref: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub dialing_at: Option<DateTimeUtc>,
    pub ringing_at: Option<DateTimeUtc>,
    pub answered_at: Option<DateTimeUtc>,
    pub connected_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
