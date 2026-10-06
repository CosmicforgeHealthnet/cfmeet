//! BreakoutAssignment (Part F).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "breakout_assignments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub breakout_room_id: Uuid,
    pub conference_id: Uuid,
    pub participant_id: Uuid,
    pub assigned_by: Option<String>,
    pub assigned_at: DateTimeUtc,
    pub unassigned_at: Option<DateTimeUtc>,
    pub mode: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
