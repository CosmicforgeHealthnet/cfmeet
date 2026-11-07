//! Personal account profile (Blueprint v1.0 Section 9: ACC-021, ACC-022, DATA-016).

use axum::{extract::State, Json};
use serde::{Deserialize, Deserializer};
use utoipa::ToSchema;

use super::V2Json;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        account_profile::{self as profile, AccountProfile, ApplyError, ProfileChange},
        account_security::{self as security, Outcome, SecurityEvent},
        actor::ActorContext,
    },
    state::AppState,
};

/// Present-but-null → `Some(None)` (clear); absent → `None` (unchanged).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Fields to change. Omitted fields stay as they are; `null` clears an optional field.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAccountProfileRequest {
    /// 3-50 letters, digits or underscores (surrounding spaces are removed). Unique.
    #[schema(example = "ada_lovelace")]
    pub username: Option<String>,
    /// 1-64 characters shown to other people instead of the username; `null` clears it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, nullable, example = "Ada Lovelace")]
    pub display_name: Option<Option<String>>,
    /// `https://` avatar image reference (at most 512 characters); `null` clears it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, nullable, example = "https://cdn.example.com/avatars/ada.png")]
    pub avatar_url: Option<Option<String>>,
    /// IANA time zone used to present times to you (for example in notification emails);
    /// `null` clears it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, nullable, example = "Europe/London")]
    pub timezone: Option<Option<String>>,
    /// BCP 47 language tag; `null` clears it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, nullable, example = "en-GB")]
    pub locale: Option<Option<String>>,
}

/// Get my profile
///
/// Returns the signed-in account's profile: username, display name, avatar reference, time
/// zone, locale, email verification and the canonical account status. The Account is the
/// authority for these fields; the Communications profile shows the same display name and
/// avatar. Phone verification is not available yet (`PROVIDER_BLOCKED`, never required).
#[utoipa::path(
    get, path = "/api/v2/me/profile", tag = "Account profile (v2)", operation_id = "v2_get_my_profile",
    responses(
        (status = 200, description = "Profile", body = AccountProfile),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_my_profile(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<AccountProfile>> {
    let user = actor.require_registered()?;
    let record = profile::load(&state.db, user.user_id)
        .await?
        .ok_or_else(signed_out)?;
    Ok(Json(AccountProfile::of(
        &record,
        state.require_email_verification,
    )))
}

/// Update my profile
///
/// Changes any of username, display name, avatar reference, time zone and locale (send only
/// the fields to change; `null` clears an optional field). The same rules apply as at
/// registration and on `PATCH /api/v1/users/me`. Returns the updated profile. A username that
/// belongs to another account answers `409 USERNAME_TAKEN`.
#[utoipa::path(
    patch, path = "/api/v2/me/profile", tag = "Account profile (v2)", operation_id = "v2_update_my_profile",
    request_body = UpdateAccountProfileRequest,
    responses(
        (status = 200, description = "Updated profile", body = AccountProfile),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 409, description = "USERNAME_TAKEN", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_my_profile(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<UpdateAccountProfileRequest>,
) -> ApiResult<Json<AccountProfile>> {
    let user = actor.require_registered()?;
    let change = ProfileChange {
        username: req.username,
        display_name: req.display_name,
        avatar_url: req.avatar_url,
        timezone: req.timezone,
        locale: req.locale,
    };
    if change.is_empty() {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "Provide at least one field to change",
        ));
    }
    let change = change.validated().map_err(profile::validation)?;
    let updated = match profile::apply(&state.db, user.user_id, &change).await {
        Ok(u) => u,
        Err(ApplyError::NotFound) => return Err(signed_out()),
        Err(ApplyError::UsernameTaken) => {
            return Err(ApiError::domain(
                ErrorCode::UsernameTaken,
                "This username is already taken",
            ))
        }
        Err(ApplyError::Db(e)) => return Err(e),
    };
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("PROFILE_UPDATED", Outcome::Success)
            .by_user(user.user_id)
            .detail(serde_json::json!({ "fields": change.fields() })),
    )
    .await;
    Ok(Json(AccountProfile::of(
        &updated,
        state.require_email_verification,
    )))
}

fn signed_out() -> ApiError {
    ApiError::domain(
        ErrorCode::AuthenticationRequired,
        "Sign in to perform this action",
    )
}
