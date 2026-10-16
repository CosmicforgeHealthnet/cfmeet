use sea_orm::entity::prelude::*;

use crate::calls::kinds::{ActorKind, CommandOperation, CommandStatus};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_commands")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub call_id: Option<Uuid>,
    pub operation: CommandOperation,
    pub idempotency_key: String,
    pub request_fingerprint: String,
    pub status: CommandStatus,
    pub result_call_version: Option<i64>,
    pub result_resource_id: Option<Uuid>,
    pub response_status: Option<i16>,
    pub response_snapshot: Option<Json>,
    pub actor_kind: ActorKind,
    pub actor_service_account_id: Uuid,
    pub actor_credential_id: Option<Uuid>,
    pub actor_service: Option<String>,
    pub actor_endpoint_id: Option<Uuid>,
    pub request_id: String,
    pub correlation_id: String,
    pub created_at: DateTimeUtc,
    pub completed_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
