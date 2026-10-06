//! BreakoutRoom (Part F).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "breakout_rooms")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub name: String,
    pub status: String,
    pub position: i32,
    pub rtc_room_name: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub opened_at: Option<DateTimeUtc>,
    pub closed_at: Option<DateTimeUtc>,
    pub capacity: Option<i32>,
    pub created_by: Option<String>,
    /// Persisted close deadline (CLOSING → CLOSED); survives restarts.
    pub closing_at: Option<DateTimeUtc>,
    pub close_requested_at: Option<DateTimeUtc>,
    pub recording_policy: String,
    pub config: Json,
    pub rtc_room_ready_at: Option<DateTimeUtc>,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
