use sea_orm::entity::prelude::*;

use crate::domain::{PrincipalKind, PrincipalStatus};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_principals")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub linked_service_account_id: Option<Uuid>,
    pub status: PrincipalStatus,
    /// Platform Access revision last applied to this projected row (0 = Part III row).
    pub pa_revision: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
