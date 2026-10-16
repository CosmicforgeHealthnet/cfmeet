use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_application_limits")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub application_id: Uuid,
    pub tenant_id: Uuid,
    pub requests_per_minute: Option<i32>,
    pub max_concurrent_communications: Option<i32>,
    pub telecom_spend_daily_limit_minor: Option<i64>,
    pub telecom_spend_monthly_limit_minor: Option<i64>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
