use sea_orm::entity::prelude::*;

use crate::calls::kinds::CallKind;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_participant_media_permissions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub participant_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub call_kind: CallKind,
    pub revision: i32,
    pub can_publish_audio: bool,
    pub can_publish_video: bool,
    pub can_publish_data: bool,
    pub can_subscribe: bool,
    pub policy_version: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
