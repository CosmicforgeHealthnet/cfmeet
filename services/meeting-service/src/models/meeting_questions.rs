//! MeetingQuestion — Q&A (Part F).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_questions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub participant_id: Uuid,
    pub body: String,
    pub is_anonymous: bool,
    pub status: String,
    pub answer_text: Option<String>,
    pub answered_by: Option<String>,
    pub upvote_count: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub pinned_at: Option<DateTimeUtc>,
    pub pinned_by: Option<String>,
    pub answered_at: Option<DateTimeUtc>,
    pub dismissed_at: Option<DateTimeUtc>,
    pub archived_at: Option<DateTimeUtc>,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
