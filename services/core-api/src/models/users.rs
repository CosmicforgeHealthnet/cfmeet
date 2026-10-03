use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub password_hash: Option<String>,
    pub auth_type: AuthType,
    pub role: UserRole,
    pub status: UserStatus,
    // Note: OTP tokens (verification_token, reset_token) are now stored in Redis
    // with automatic expiration. See services/otp.rs for details.

    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub last_login: Option<DateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "auth_type")]
pub enum AuthType {
    #[sea_orm(string_value = "local")]
    Local,
    #[sea_orm(string_value = "google")]
    Google,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "user_role")]
pub enum UserRole {
    #[sea_orm(string_value = "user")]
    User,
    #[sea_orm(string_value = "admin")]
    Admin,
    #[sea_orm(string_value = "guest")]
    Guest,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "user_status")]
pub enum UserStatus {
    #[sea_orm(string_value = "pending_verification")]
    PendingVerification,
    #[sea_orm(string_value = "active")]
    Active,
    #[sea_orm(string_value = "inactive")]
    Inactive,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::meetings::Entity")]
    Meetings,
    #[sea_orm(has_many = "super::participants::Entity")]
    Participants,
    #[sea_orm(has_many = "super::api_keys::Entity")]
    ApiKeys,
    #[sea_orm(has_many = "super::webhooks::Entity")]
    Webhooks,
}

impl Related<super::meetings::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Meetings.def()
    }
}

impl Related<super::participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participants.def()
    }
}

impl Related<super::api_keys::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ApiKeys.def()
    }
}

impl Related<super::webhooks::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Webhooks.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
