//! MeetingPoll (Part F).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_polls")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub created_by_participant_id: Option<Uuid>,
    pub question: String,
    pub options: Json,
    pub poll_type: String,
    pub is_anonymous: bool,
    pub status: String,
    pub created_at: DateTimeUtc,
    pub opened_at: Option<DateTimeUtc>,
    pub closed_at: Option<DateTimeUtc>,
    pub version: i32,
    pub definition: Json,
    pub quiz_mode: bool,
    pub results_visibility: String,
    pub created_by: Option<String>,
    pub archived_at: Option<DateTimeUtc>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
