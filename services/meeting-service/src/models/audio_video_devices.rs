use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "audio_video_devices")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub participant_id: Uuid,
    pub device_type: DeviceType,
    pub device_name: String,
    pub is_active: bool,
    pub created_at: DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "device_type")]
pub enum DeviceType {
    #[sea_orm(string_value = "audio_input")]
    AudioInput,
    #[sea_orm(string_value = "audio_output")]
    AudioOutput,
    #[sea_orm(string_value = "video_input")]
    VideoInput,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::participants::Entity",
        from = "Column::ParticipantId",
        to = "super::participants::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Participant,
}

impl Related<super::participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participant.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
