use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "participants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub user_id: Option<Uuid>,
    pub role: ParticipantRole,
    pub join_time: DateTime,
    pub leave_time: Option<DateTime>,
    pub status: ParticipantStatus,
    pub is_muted: bool,
    pub is_video_on: bool,
    pub is_screen_sharing: bool,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "participant_role")]
pub enum ParticipantRole {
    #[sea_orm(string_value = "host")]
    Host,
    #[sea_orm(string_value = "participant")]
    Participant,
    #[sea_orm(string_value = "viewer")]
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "participant_status")]
pub enum ParticipantStatus {
    #[sea_orm(string_value = "waiting")]
    Waiting,
    #[sea_orm(string_value = "joined")]
    Joined,
    #[sea_orm(string_value = "kicked")]
    Kicked,
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
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    User,
    #[sea_orm(has_many = "super::audio_video_devices::Entity")]
    AudioVideoDevices,
    #[sea_orm(has_many = "super::chat_messages::Entity")]
    ChatMessages,
    #[sea_orm(has_many = "super::session_logs::Entity")]
    SessionLogs,
}

impl Related<super::meetings::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Meeting.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::audio_video_devices::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AudioVideoDevices.def()
    }
}

impl Related<super::chat_messages::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ChatMessages.def()
    }
}

impl Related<super::session_logs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionLogs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
