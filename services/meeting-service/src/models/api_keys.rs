use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "api_keys")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub user_id: Uuid,
    pub api_key: String,
    pub usage_limit: i32,
    pub used_count: i32,
    pub expires_at: DateTime,
    pub status: ApiKeyStatus,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    /// PLT-002: SHA-256 verifier (`account_hardening_migrations`); `None` only for rows an
    /// older image wrote after the migration (healed on first use).
    pub key_hash: Option<String>,
    /// Non-secret lookup / display prefix (first 8 characters of the key).
    pub key_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "api_key_status")]
pub enum ApiKeyStatus {
    #[sea_orm(string_value = "active")]
    Active,
    #[sea_orm(string_value = "revoked")]
    Revoked,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    User,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
