//! ConferenceSession — one occurrence/live call of a MeetingSpace.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{ConferenceKind, ConferenceStatus, RecordOrigin, TerminationReason};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "conference_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub occurrence_key: String,
    pub kind: ConferenceKind,
    pub origin: RecordOrigin,
    pub legacy_meeting_id: Option<Uuid>,
    pub recurrence_rule_id: Option<Uuid>,
    pub is_exception: bool,
    pub original_start_at: Option<DateTimeUtc>,
    pub status: ConferenceStatus,
    pub scheduled_start_at: Option<DateTimeUtc>,
    pub scheduled_end_at: Option<DateTimeUtc>,
    pub started_at: Option<DateTimeUtc>,
    pub started_by: Option<String>,
    pub ending_requested_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub ended_by: Option<String>,
    pub cancelled_at: Option<DateTimeUtc>,
    pub cancelled_by: Option<String>,
    pub termination_reason: Option<TerminationReason>,
    pub failure_reason: Option<String>,
    pub is_locked: bool,
    pub locked_at: Option<DateTimeUtc>,
    pub locked_by: Option<String>,
    pub rtc_provider: String,
    /// Opaque provider mapping — never a public identifier.
    pub rtc_room_name: Option<String>,
    pub rtc_room_id: Option<String>,
    pub region: Option<String>,
    pub recording_state: String,
    pub transcription_state: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::meeting_spaces::Entity",
        from = "Column::MeetingSpaceId",
        to = "super::meeting_spaces::Column::Id"
    )]
    MeetingSpace,
    #[sea_orm(has_many = "super::conference_participants::Entity")]
    Participants,
}

impl Related<super::meeting_spaces::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MeetingSpace.def()
    }
}

impl Related<super::conference_participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participants.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
