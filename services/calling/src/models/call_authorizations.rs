use sea_orm::entity::prelude::*;

use crate::calls::kinds::{ActorKind, AuthorizationDecision, AuthorizationReason};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_authorizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub decision: AuthorizationDecision,
    pub reason_code: AuthorizationReason,
    pub policy_version: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub policy_snapshot: Json,
    #[sea_orm(column_type = "JsonBinary")]
    pub limits_snapshot: Json,
    pub actor_kind: ActorKind,
    pub evidence_service_account_id: Uuid,
    pub evidence_credential_id: Option<Uuid>,
    pub evidence_service: Option<String>,
    pub evidence_principal_id: Uuid,
    pub request_id: String,
    pub correlation_id: String,
    pub decided_at: DateTimeUtc,
    pub superseded_at: Option<DateTimeUtc>,
    pub superseded_by_authorization_id: Option<Uuid>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
