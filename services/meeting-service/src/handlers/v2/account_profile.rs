//! Personal account profile (Blueprint v1.0 Section 9: ACC-021, ACC-022, DATA-016).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Deserializer};
use utoipa::ToSchema;
use uuid::Uuid;

use super::V2Json;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        account_avatar as avatar,
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
    // Gap audit A22: pointing the avatar elsewhere (or clearing it) drops the upload.
    if let Some(url) = &change.avatar_url {
        if !url.as_deref().is_some_and(avatar::is_uploaded) {
            avatar::remove(&state.db, user.user_id).await?;
        }
    }
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

/// Set the avatar address through the profile update, so the Hub projection follows.
async fn set_avatar(
    state: &AppState,
    user_id: Uuid,
    url: Option<String>,
    event: &'static str,
) -> ApiResult<AccountProfile> {
    let change = ProfileChange {
        username: None,
        display_name: None,
        avatar_url: Some(url),
        timezone: None,
        locale: None,
    };
    let updated = match profile::apply(&state.db, user_id, &change).await {
        Ok(u) => u,
        Err(ApplyError::NotFound) => return Err(signed_out()),
        Err(ApplyError::UsernameTaken) => {
            return Err(ApiError::InternalError("unexpected username change".into()))
        }
        Err(ApplyError::Db(e)) => return Err(e),
    };
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new(event, Outcome::Success)
            .by_user(user_id)
            .detail(serde_json::json!({ "fields": ["avatarUrl"] })),
    )
    .await;
    Ok(AccountProfile::of(
        &updated,
        state.require_email_verification,
    ))
}

/// Upload my avatar
///
/// Sends the image itself as the request body (`Content-Type: image/png`, `image/jpeg` or
/// `image/webp`; at most 512 KiB). Uploads need the service's https public address
/// (`AVATAR_PUBLIC_BASE_URL`); without one they answer `404 FEATURE_DISABLED`. The image
/// is recognised by its content; a body that is
/// not one of these images, or whose declared type does not match, answers `400
/// VALIDATION_ERROR`. The profile's `avatarUrl` becomes the image's public, immutable
/// address (`/api/v2/avatars/{avatarId}`), replacing any earlier upload. Returns the
/// updated profile.
#[utoipa::path(
    put, path = "/api/v2/me/avatar", tag = "Account profile (v2)", operation_id = "v2_upload_my_avatar",
    request_body(content = Vec<u8>, description = "PNG, JPEG or WebP image, at most 512 KiB", content_type = "image/png"),
    responses(
        (status = 200, description = "Updated profile", body = AccountProfile),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 413, description = "REQUEST_TOO_LARGE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn upload_my_avatar(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<AccountProfile>> {
    let user = actor.require_registered()?;
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let kind = avatar::validate(&body, declared)?;
    let id = Uuid::new_v4();
    let url = avatar::address(id)?;
    avatar::store(&state.db, user.user_id, id, kind, &body).await?;
    let profile = set_avatar(&state, user.user_id, Some(url), "AVATAR_UPLOADED").await?;
    Ok(Json(profile))
}

/// Remove my avatar
///
/// Removes the uploaded avatar image (if any) and clears the profile's `avatarUrl`.
/// Returns the updated profile. Removing when there is no avatar is not an error.
#[utoipa::path(
    delete, path = "/api/v2/me/avatar", tag = "Account profile (v2)", operation_id = "v2_remove_my_avatar",
    responses(
        (status = 200, description = "Updated profile", body = AccountProfile),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_my_avatar(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<AccountProfile>> {
    let user = actor.require_registered()?;
    avatar::remove(&state.db, user.user_id).await?;
    Ok(Json(
        set_avatar(&state, user.user_id, None, "AVATAR_REMOVED").await?,
    ))
}

/// Get an avatar image
///
/// Public and read-only: the image bytes of an uploaded avatar, with its content type. An
/// address never changes content (a new upload gets a new address), so it may be cached
/// for a long time. A replaced or removed image, or one of a deactivated or suspended
/// account, answers `404 AVATAR_NOT_FOUND`.
#[utoipa::path(
    get, path = "/api/v2/avatars/{avatarId}", tag = "Account profile (v2)", operation_id = "v2_get_avatar",
    params(("avatarId" = Uuid, Path, description = "Avatar id from the profile's avatarUrl")),
    responses(
        (status = 200, description = "The image (image/png, image/jpeg or image/webp)", content_type = "image/png", body = Vec<u8>),
        (status = 404, description = "AVATAR_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(())
)]
pub async fn get_avatar(
    State(state): State<AppState>,
    Path(avatar_id): Path<Uuid>,
) -> ApiResult<Response> {
    let (kind, bytes, digest) = avatar::fetch(&state.db, avatar_id)
        .await?
        .ok_or_else(|| ApiError::domain(ErrorCode::AvatarNotFound, "No such avatar"))?;
    let mut response = (StatusCode::OK, bytes).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&kind).map_err(|_| ApiError::InternalError("bad type".into()))?,
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    if let Ok(tag) = HeaderValue::from_str(&format!("\"{digest}\"")) {
        h.insert(header::ETAG, tag);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    Ok(response)
}

fn signed_out() -> ApiError {
    ApiError::domain(
        ErrorCode::AuthenticationRequired,
        "Sign in to perform this action",
    )
}
