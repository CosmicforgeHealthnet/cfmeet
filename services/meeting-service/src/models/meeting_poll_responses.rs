//! MeetingPollResponse (Part F).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_poll_responses")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub poll_id: Uuid,
    pub conference_id: Uuid,
    /// NULL for anonymous polls: answers never carry identity.
    pub participant_id: Option<Uuid>,
    pub selected_options: Json,
    pub created_at: DateTimeUtc,
    pub answers: Json,
    pub score: Option<f64>,
    pub max_score: Option<f64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
