use sea_orm::entity::prelude::*;

use crate::calls::kinds::DisconnectReason;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_session_intervals")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub interval_number: i32,
    pub connected_at: DateTimeUtc,
    pub disconnected_at: Option<DateTimeUtc>,
    pub disconnect_reason: Option<DisconnectReason>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
