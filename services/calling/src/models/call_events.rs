//! Immutable call event row (append-only; the database refuses UPDATE / DELETE).

use sea_orm::entity::prelude::*;

use crate::calls::{events::CallEventType, kinds::ActorKind, state::CallState};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    pub call_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub sequence: i64,
    pub event_type: CallEventType,
    pub schema_version: i32,
    pub from_state: Option<CallState>,
    pub to_state: Option<CallState>,
    pub call_version: i64,
    pub actor_kind: ActorKind,
    pub actor_service_account_id: Uuid,
    pub actor_credential_id: Option<Uuid>,
    pub actor_service: Option<String>,
    pub actor_endpoint_id: Option<Uuid>,
    pub request_id: String,
    pub correlation_id: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub payload: Json,
    pub occurred_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
