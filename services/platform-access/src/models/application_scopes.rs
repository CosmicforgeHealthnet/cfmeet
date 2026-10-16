use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_application_scopes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub application_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope: String,
    pub tenant_id: Uuid,
    /// NULL = every environment kind.
    pub allowed_environment_kinds: Option<Vec<String>>,
    /// `STANDARD` or `PRIVILEGED_OPERATOR`.
    pub grant_basis: String,
    pub granted_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
