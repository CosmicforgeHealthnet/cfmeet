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
    /// Blueprint Section 9 profile (nullable; `profile_notifications_migrations`). The
    /// Account is authoritative; the Communications Hub profile is a projection of it.
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// IANA time zone used to present times to this user (for example in notifications).
    pub timezone: Option<String>,
    /// BCP 47 language tag (for example `en` or `en-GB`).
    pub locale: Option<String>,
    /// When the address was proven (verification code or Google sign-in).
    pub email_verified_at: Option<DateTimeUtc>,
    pub profile_updated_at: Option<DateTimeUtc>,
}

impl Model {
    /// The name other people see: the chosen display name, otherwise the username.
    pub fn presentation_name(&self) -> String {
        self.display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.username.clone())
    }

    /// One identity rule for "this account's email address is verified" (meeting
    /// invitations, agenda, Hub delegation, realtime, session bridge): Google sign-in, a
    /// recorded verification, or an active account where verification is required to
    /// sign in at all.
    pub fn email_verified(&self, require_email_verification: bool) -> bool {
        self.auth_type == AuthType::Google
            || self.email_verified_at.is_some()
            || (self.status == UserStatus::Active && require_email_verification)
    }

    /// The canonical status string of the account contract (ACC-022).
    pub fn status_contract(&self) -> &'static str {
        self.status.as_contract_str()
    }
}

impl UserStatus {
    /// Canonical wire value (`pending_verification`, `active`, `inactive`, `suspended`),
    /// identical to the database enum value and the admin API.
    pub fn as_contract_str(&self) -> &'static str {
        match self {
            UserStatus::PendingVerification => "pending_verification",
            UserStatus::Active => "active",
            UserStatus::Inactive => "inactive",
            UserStatus::Suspended => "suspended",
        }
    }
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
    #[sea_orm(string_value = "suspended")]
    Suspended,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(auth_type: AuthType, status: UserStatus) -> Model {
        let now = chrono::Utc::now();
        Model {
            id: Uuid::new_v4(),
            username: "ada_l".into(),
            email: "ada@example.com".into(),
            password_hash: None,
            auth_type,
            role: UserRole::User,
            status,
            created_at: now.naive_utc(),
            updated_at: now.naive_utc(),
            last_login: None,
            display_name: None,
            avatar_url: None,
            timezone: None,
            locale: None,
            email_verified_at: None,
            profile_updated_at: None,
        }
    }

    #[test]
    fn one_email_verification_rule() {
        assert!(user(AuthType::Google, UserStatus::Active).email_verified(false));
        assert!(!user(AuthType::Local, UserStatus::Active).email_verified(false));
        assert!(user(AuthType::Local, UserStatus::Active).email_verified(true));
        assert!(!user(AuthType::Local, UserStatus::PendingVerification).email_verified(true));
        let mut proven = user(AuthType::Local, UserStatus::Active);
        proven.email_verified_at = Some(chrono::Utc::now());
        assert!(
            proven.email_verified(false),
            "a recorded verification counts"
        );
    }

    #[test]
    fn presentation_name_prefers_the_display_name() {
        let mut u = user(AuthType::Local, UserStatus::Active);
        assert_eq!(u.presentation_name(), "ada_l");
        u.display_name = Some("Ada Lovelace".into());
        assert_eq!(u.presentation_name(), "Ada Lovelace");
        u.display_name = Some("   ".into());
        assert_eq!(u.presentation_name(), "ada_l");
        assert_eq!(
            UserStatus::PendingVerification.as_contract_str(),
            "pending_verification"
        );
    }
}
