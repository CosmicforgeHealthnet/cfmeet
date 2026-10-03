use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "session_logs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub participant_id: Option<Uuid>,
    pub event_type: EventType,
    pub event_time: DateTime,
    pub metadata: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "event_type")]
pub enum EventType {
    #[sea_orm(string_value = "meeting_start")]
    MeetingStart,
    #[sea_orm(string_value = "meeting_end")]
    MeetingEnd,
    #[sea_orm(string_value = "participant_join")]
    ParticipantJoin,
    #[sea_orm(string_value = "participant_leave")]
    ParticipantLeave,
    #[sea_orm(string_value = "role_change")]
    RoleChange,
    #[sea_orm(string_value = "screen_share_start")]
    ScreenShareStart,
    #[sea_orm(string_value = "screen_share_end")]
    ScreenShareEnd,
    #[sea_orm(string_value = "media_toggle")]
    MediaToggle,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::meetings::Entity",
        from = "Column::MeetingId",
        to = "super::meetings::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Meeting,
    #[sea_orm(
        belongs_to = "super::participants::Entity",
        from = "Column::ParticipantId",
        to = "super::participants::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    Participant,
}

impl Related<super::meetings::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Meeting.def()
    }
}

impl Related<super::participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participant.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
