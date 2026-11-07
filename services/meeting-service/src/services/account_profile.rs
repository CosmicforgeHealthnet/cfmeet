//! Personal account profile (Blueprint v1.0 Section 9: ACC-021, ACC-022, DATA-016).
//!
//! The Account (this service) is the single writer of the profile: username, display name,
//! avatar reference, time zone and locale live on `users`. The Communications Hub keeps a
//! projection (`communication_profiles.display_name` / `avatar_reference`) that the
//! account relay refreshes from this record on every account-authenticated Hub request, so
//! nothing writes the profile twice.
//!
//! Validation is one set of rules for every writer (v1 `PATCH /api/v1/users/me`, v2
//! `PATCH /api/v2/me/profile`, registration for the username):
//!
//! * username — trimmed, 3–50 characters of `[A-Za-z0-9_]` (the registration rule);
//! * display name — trimmed, 1–64 characters, no control or format characters;
//! * avatar URL — an `https://` reference of at most 512 characters without spaces or
//!   `@` (the Hub's avatar-reference rule; uploads are not part of this section);
//! * time zone — a known IANA zone name;
//! * locale — a BCP 47 language tag (`en`, `en-GB`, `pt-BR`, `zh-Hant-TW`).

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QuerySelect, Set, Statement, TransactionTrait,
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
};

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 50;
pub const DISPLAY_NAME_MAX: usize = 64;
pub const AVATAR_URL_MAX: usize = 512;

/// A rule violation; callers map it to their API's error shape (v1 or v2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

/// Trimmed username following the registration rule.
pub fn username(raw: &str) -> Result<String, Invalid> {
    let v = raw.trim();
    let n = v.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&n) {
        return Err(Invalid("username must be between 3 and 50 characters"));
    }
    if !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Invalid(
            "username can only contain letters, numbers, and underscores",
        ));
    }
    Ok(v.to_owned())
}

/// Unicode control (Cc) and the common invisible format characters (Cf) that let a
/// name impersonate another one or break rendering.
fn forbidden_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}')
}

pub fn display_name(raw: &str) -> Result<String, Invalid> {
    let v = raw.trim();
    let n = v.chars().count();
    if n == 0 || n > DISPLAY_NAME_MAX {
        return Err(Invalid("displayName must be between 1 and 64 characters"));
    }
    if v.chars().any(forbidden_char) {
        return Err(Invalid(
            "displayName must not contain control or invisible formatting characters",
        ));
    }
    Ok(v.to_owned())
}

pub fn avatar_url(raw: &str) -> Result<String, Invalid> {
    let v = raw.trim();
    let rest = v.strip_prefix("https://").unwrap_or("");
    if rest.is_empty()
        || v.len() > AVATAR_URL_MAX
        || v.chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '@')
    {
        return Err(Invalid(
            "avatarUrl must be an https:// URL of at most 512 characters without spaces or '@'",
        ));
    }
    Ok(v.to_owned())
}

pub fn timezone(raw: &str) -> Result<String, Invalid> {
    let v = raw.trim();
    if v.is_empty() || v.len() > 64 {
        return Err(Invalid("timezone must be an IANA time zone name"));
    }
    v.parse::<chrono_tz::Tz>()
        .map(|tz| tz.name().to_owned())
        .map_err(|_| Invalid("timezone must be an IANA time zone name"))
}

/// BCP 47: a 2–3 letter language, then up to three 2–8 character alphanumeric subtags.
/// Normalized to the conventional case (`en-GB`, `zh-Hant-TW`).
pub fn locale(raw: &str) -> Result<String, Invalid> {
    let v = raw.trim().replace('_', "-");
    let err = Invalid("locale must be a BCP 47 language tag such as en or en-GB");
    if v.is_empty() || v.len() > 35 {
        return Err(err);
    }
    let mut parts = v.split('-');
    let language = parts.next().unwrap_or_default();
    if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(err);
    }
    let mut out = vec![language.to_ascii_lowercase()];
    for part in parts {
        if out.len() > 3
            || !(2..=8).contains(&part.len())
            || !part.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(err);
        }
        out.push(match part.len() {
            2 if part.chars().all(|c| c.is_ascii_alphabetic()) => part.to_ascii_uppercase(),
            4 if part.chars().all(|c| c.is_ascii_alphabetic()) => {
                let mut s = part.to_ascii_lowercase();
                s[..1].make_ascii_uppercase();
                s
            }
            _ => part.to_ascii_lowercase(),
        });
    }
    Ok(out.join("-"))
}

/// Records that the address was proven (verification code or Google) once.
pub async fn mark_email_verified<C: ConnectionTrait>(conn: &C, user_id: Uuid) -> ApiResult<()> {
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET email_verified_at = now() WHERE id = $1 AND email_verified_at IS NULL",
        [user_id.into()],
    ))
    .await?;
    Ok(())
}

/// A profile change. `None` leaves a field unchanged; `Some(None)` clears it (username
/// cannot be cleared).
#[derive(Debug, Default, Clone)]
pub struct ProfileChange {
    pub username: Option<String>,
    pub display_name: Option<Option<String>>,
    pub avatar_url: Option<Option<String>>,
    pub timezone: Option<Option<String>>,
    pub locale: Option<Option<String>>,
}

impl ProfileChange {
    pub fn is_empty(&self) -> bool {
        self.username.is_none()
            && self.display_name.is_none()
            && self.avatar_url.is_none()
            && self.timezone.is_none()
            && self.locale.is_none()
    }

    /// Validate and normalize every provided field (first violation wins).
    pub fn validated(self) -> Result<Self, Invalid> {
        fn opt(
            v: Option<Option<String>>,
            f: fn(&str) -> Result<String, Invalid>,
        ) -> Result<Option<Option<String>>, Invalid> {
            v.map(|inner| inner.map(|s| f(&s)).transpose()).transpose()
        }
        Ok(Self {
            username: self.username.map(|u| username(&u)).transpose()?,
            display_name: opt(self.display_name, display_name)?,
            avatar_url: opt(self.avatar_url, avatar_url)?,
            timezone: opt(self.timezone, timezone)?,
            locale: opt(self.locale, locale)?,
        })
    }

    /// Names of the provided fields (audit detail; never values).
    pub fn fields(&self) -> Vec<&'static str> {
        let mut out = vec![];
        if self.username.is_some() {
            out.push("username");
        }
        if self.display_name.is_some() {
            out.push("displayName");
        }
        if self.avatar_url.is_some() {
            out.push("avatarUrl");
        }
        if self.timezone.is_some() {
            out.push("timezone");
        }
        if self.locale.is_some() {
            out.push("locale");
        }
        out
    }
}

/// Outcome of [`apply`].
#[derive(Debug)]
pub enum ApplyError {
    NotFound,
    UsernameTaken,
    Db(ApiError),
}

impl From<sea_orm::DbErr> for ApplyError {
    fn from(e: sea_orm::DbErr) -> Self {
        ApplyError::Db(e.into())
    }
}

/// Applies an already validated change under the account row lock. The username stays
/// unique: a concurrent claim of the same name is caught by the unique index.
pub async fn apply<C: TransactionTrait>(
    db: &C,
    user_id: Uuid,
    change: &ProfileChange,
) -> Result<users::Model, ApplyError> {
    let txn = db.begin().await?;
    let user = users::Entity::find_by_id(user_id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or(ApplyError::NotFound)?;
    if let Some(name) = &change.username {
        if name != &user.username {
            let taken = users::Entity::find()
                .filter(users::Column::Username.eq(name.as_str()))
                .filter(users::Column::Id.ne(user_id))
                .one(&txn)
                .await?
                .is_some();
            if taken {
                return Err(ApplyError::UsernameTaken);
            }
        }
    }
    let mut am: users::ActiveModel = user.clone().into();
    if let Some(name) = &change.username {
        if name != &user.username {
            am.username = Set(name.clone());
        }
    }
    if let Some(v) = &change.display_name {
        am.display_name = Set(v.clone());
    }
    if let Some(v) = &change.avatar_url {
        am.avatar_url = Set(v.clone());
    }
    if let Some(v) = &change.timezone {
        am.timezone = Set(v.clone());
    }
    if let Some(v) = &change.locale {
        am.locale = Set(v.clone());
    }
    let now = Utc::now();
    am.profile_updated_at = Set(Some(now));
    am.updated_at = Set(now.naive_utc());
    let updated = match am.update(&txn).await {
        Ok(u) => u,
        Err(e) if is_unique_violation(&e) => return Err(ApplyError::UsernameTaken),
        Err(e) => return Err(e.into()),
    };
    txn.commit().await?;
    Ok(updated)
}

fn is_unique_violation(e: &sea_orm::DbErr) -> bool {
    let text = e.to_string();
    text.contains("23505") || text.contains("duplicate key value")
}

/// `GET/PATCH /api/v2/me/profile` representation.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfile {
    pub user_id: Uuid,
    pub username: String,
    /// Chosen display name (`null` when not set).
    pub display_name: Option<String>,
    /// What other people see: `displayName`, otherwise `username`.
    pub effective_display_name: String,
    /// `https://` avatar reference (`null` when not set).
    pub avatar_url: Option<String>,
    /// IANA time zone (`null`: clients use the device zone).
    pub timezone: Option<String>,
    /// BCP 47 language tag (`null`: clients use the device language).
    pub locale: Option<String>,
    pub email: String,
    pub email_verified: bool,
    pub email_verified_at: Option<DateTime<Utc>>,
    /// Canonical account status: `pending_verification`, `active`, `inactive` or `suspended`.
    #[schema(example = "active")]
    pub status: String,
    /// `local` or `google`.
    pub auth_type: String,
    pub created_at: DateTime<Utc>,
    pub profile_updated_at: Option<DateTime<Utc>>,
    pub phone_verification: PhoneVerificationStatus,
}

/// Phone binding is provider-gated (ADR 0056 §8): `PROVIDER_BLOCKED` until an SMS or
/// verification provider is approved, and never required.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneVerificationStatus {
    #[schema(example = "PROVIDER_BLOCKED")]
    pub status: String,
    pub required: bool,
}

impl AccountProfile {
    pub fn of(user: &users::Model, require_email_verification: bool) -> Self {
        Self {
            user_id: user.id,
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            effective_display_name: user.presentation_name(),
            avatar_url: user.avatar_url.clone(),
            timezone: user.timezone.clone(),
            locale: user.locale.clone(),
            email: user.email.clone(),
            email_verified: user.email_verified(require_email_verification),
            email_verified_at: user.email_verified_at,
            status: user.status_contract().to_owned(),
            auth_type: match user.auth_type {
                users::AuthType::Local => "local",
                users::AuthType::Google => "google",
            }
            .to_owned(),
            created_at: user.created_at.and_utc(),
            profile_updated_at: user.profile_updated_at,
            phone_verification: PhoneVerificationStatus {
                status: "PROVIDER_BLOCKED".into(),
                required: false,
            },
        }
    }
}

/// v2 error for a rule violation.
pub fn validation(e: Invalid) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, e.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rule_matches_registration_after_trimming() {
        assert_eq!(username("  alice_1 ").unwrap(), "alice_1");
        assert!(
            username("  ab ").is_err(),
            "length is checked after trimming"
        );
        assert!(username("a b c").is_err());
        assert!(username("bob\u{0007}x").is_err());
        assert!(username(&"x".repeat(51)).is_err());
        assert!(username("élodie").is_err());
    }

    #[test]
    fn display_name_rejects_control_and_invisible_characters() {
        assert_eq!(display_name("  Ada Lovelace ").unwrap(), "Ada Lovelace");
        assert_eq!(display_name("Zoë 李").unwrap(), "Zoë 李");
        assert!(display_name("   ").is_err());
        assert!(display_name("a\nb").is_err());
        assert!(display_name("admin\u{202E}").is_err());
        assert!(display_name("x\u{200B}y").is_err());
        assert!(display_name(&"é".repeat(65)).is_err());
        assert!(display_name(&"é".repeat(64)).is_ok());
    }

    #[test]
    fn avatar_url_is_an_https_reference() {
        assert!(avatar_url("https://cdn.example.com/a.png").is_ok());
        assert!(avatar_url("http://cdn.example.com/a.png").is_err());
        assert!(avatar_url("https://").is_err());
        assert!(avatar_url("https://user@evil.example/a.png").is_err());
        assert!(avatar_url("https://a.example/a b.png").is_err());
        assert!(avatar_url(&format!("https://a.example/{}", "x".repeat(600))).is_err());
        assert!(avatar_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn timezone_and_locale_are_normalized() {
        assert_eq!(timezone("Africa/Lagos").unwrap(), "Africa/Lagos");
        assert_eq!(timezone("UTC").unwrap(), "UTC");
        assert!(timezone("Mars/Olympus").is_err());
        assert!(timezone("").is_err());
        assert_eq!(locale("en").unwrap(), "en");
        assert_eq!(locale("en_gb").unwrap(), "en-GB");
        assert_eq!(locale("zh-hant-tw").unwrap(), "zh-Hant-TW");
        assert_eq!(locale("es-419").unwrap(), "es-419");
        assert!(locale("e").is_err());
        assert!(locale("english").is_err());
        assert!(locale("en-").is_err());
        assert!(locale("en-GB-a-b-c").is_err());
    }

    #[test]
    fn change_validation_normalizes_and_lists_fields() {
        let c = ProfileChange {
            display_name: Some(Some(" Ada ".into())),
            timezone: Some(None),
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert_eq!(c.display_name, Some(Some("Ada".into())));
        assert_eq!(c.timezone, Some(None));
        assert_eq!(c.fields(), vec!["displayName", "timezone"]);
        assert!(ProfileChange::default().is_empty());
        assert!(ProfileChange {
            locale: Some(Some("??".into())),
            ..Default::default()
        }
        .validated()
        .is_err());
    }
}
