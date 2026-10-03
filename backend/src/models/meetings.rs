use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meetings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_identifier: String,
    pub user_id: Option<Uuid>,
    pub host_id: Uuid,
    pub title: String,
    pub metadata: Option<JsonValue>,
    pub is_private: bool,
    pub start_time: DateTime,
    pub end_time: Option<DateTime>,
    pub status: MeetingStatus,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "meeting_status")]
pub enum MeetingStatus {
    #[sea_orm(string_value = "scheduled")]
    Scheduled,
    #[sea_orm(string_value = "ongoing")]
    Ongoing,
    #[sea_orm(string_value = "ended")]
    Ended,
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::HostId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Host,
    #[sea_orm(has_many = "super::participants::Entity")]
    Participants,
    #[sea_orm(has_many = "super::chat_messages::Entity")]
    ChatMessages,
    #[sea_orm(has_many = "super::session_logs::Entity")]
    SessionLogs,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Host.def()
    }
}

impl Related<super::participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participants.def()
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
