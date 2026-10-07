//! Calling-private media binding. `provider_adapter` and `provider_resource_ref` are
//! internal operational data: never serialized into the public API or customer events.

use sea_orm::entity::prelude::*;

use crate::calls::kinds::{MediaFailure, MediaSessionStatus};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_media_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub route_id: Uuid,
    pub leg_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub provider_adapter: String,
    pub provider_resource_ref: Option<String>,
    pub media_resource_name: String,
    pub status: MediaSessionStatus,
    pub max_participants: i32,
    pub enforcement_pending: bool,
    pub failure_count: i32,
    pub last_failure: Option<MediaFailure>,
    pub created_at: DateTimeUtc,
    pub ensured_at: Option<DateTimeUtc>,
    pub terminated_at: Option<DateTimeUtc>,
    pub last_reconciled_at: Option<DateTimeUtc>,
    pub next_reconcile_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
