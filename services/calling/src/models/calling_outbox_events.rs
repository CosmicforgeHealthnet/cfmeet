use sea_orm::entity::prelude::*;

use crate::calls::kinds::OutboxStatus;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "calling_outbox_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub event_id: String,
    pub source_event_id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub event_type: String,
    pub schema_version: i32,
    #[sea_orm(column_type = "JsonBinary")]
    pub envelope: Json,
    pub status: OutboxStatus,
    pub attempt_count: i32,
    pub next_attempt_at: DateTimeUtc,
    pub published_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
