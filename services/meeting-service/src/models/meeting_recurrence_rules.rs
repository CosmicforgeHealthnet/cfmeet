//! MeetingRecurrenceRule — RFC 5545 RRULE interpreted in the space timezone.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::RecurrenceStatus;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_recurrence_rules")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub rrule: String,
    pub timezone: String,
    /// Wall-clock start in `timezone`.
    pub dtstart_local: DateTime,
    pub duration_minutes: i32,
    pub status: RecurrenceStatus,
    pub materialized_until: Option<DateTimeUtc>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub ended_at: Option<DateTimeUtc>,
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
}

impl Related<super::meeting_spaces::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MeetingSpace.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
