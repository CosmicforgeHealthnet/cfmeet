use sea_orm::entity::prelude::*;

use crate::endpoints::{EndpointKind, EndpointStatus, PresenceState};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_endpoints")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub endpoint_key: String,
    pub endpoint_kind: EndpointKind,
    pub supports_audio: bool,
    pub supports_video: bool,
    pub status: EndpointStatus,
    pub presence_state: PresenceState,
    pub last_seen_at: Option<DateTimeUtc>,
    pub push_target_ref: Option<Uuid>,
    pub registered_by_service_account_id: Uuid,
    pub registered_by_credential_id: Uuid,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub revoked_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
