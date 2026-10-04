//! OutboxEvent — transactional outbox; delivery only, never authority.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{OutboxChannel, OutboxStatus};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "outbox_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub event_type: String,
    pub payload: Json,
    pub meeting_space_id: Option<Uuid>,
    pub conference_id: Option<Uuid>,
    pub status: OutboxStatus,
    pub attempts: i32,
    pub available_at: DateTimeUtc,
    pub published_at: Option<DateTimeUtc>,
    pub last_error: Option<String>,
    pub request_id: Option<String>,
    pub created_at: DateTimeUtc,
    pub channel: OutboxChannel,
    /// Gap-free, commit-ordered per-conference sequence (realtime replay cursor).
    pub conference_seq: Option<i64>,
    /// Relay lease holder while a delivery attempt is in flight.
    pub locked_by: Option<String>,
    /// Part F: server-side realtime audience (never sent to clients).
    pub audience: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
