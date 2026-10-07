use sea_orm::entity::prelude::*;

use crate::calls::kinds::{EndpointCategory, MediaAccess, SessionState};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub participant_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub media_identity: String,
    pub endpoint_category: EndpointCategory,
    pub state: SessionState,
    pub generation: i32,
    pub replaces_session_id: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub first_connected_at: Option<DateTimeUtc>,
    pub last_disconnected_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub updated_at: DateTimeUtc,
    pub endpoint_id: Option<Uuid>,
    pub media_access: MediaAccess,
    pub media_generation: i32,
    pub media_admitted_at: Option<DateTimeUtc>,
    pub media_enforcement_pending: bool,
    pub removed_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
