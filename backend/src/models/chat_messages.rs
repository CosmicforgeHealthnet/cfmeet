use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "chat_messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_id: Uuid,
    pub participant_id: Uuid,
    pub message: String,
    pub created_at: DateTime,
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
        on_delete = "Cascade"
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
