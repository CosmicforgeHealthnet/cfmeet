use sea_orm::entity::prelude::*;

use crate::domain::ServiceAccountStatus;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_service_accounts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: ServiceAccountStatus,
    pub revoked_at: Option<DateTimeUtc>,
    /// Platform Access revision last applied to this projected row (0 = Part III row).
    pub pa_revision: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
