use sea_orm::entity::prelude::*;

use crate::calls::kinds::{UsageType, UsageUnit};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_usage_records")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub participant_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub leg_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub usage_type: UsageType,
    pub quantity: i64,
    pub unit: UsageUnit,
    pub usage_started_at: Option<DateTimeUtc>,
    pub usage_ended_at: Option<DateTimeUtc>,
    pub idempotency_key: String,
    pub finalized_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
