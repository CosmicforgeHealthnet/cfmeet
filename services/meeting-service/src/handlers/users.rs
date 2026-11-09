use axum::{extract::State, Extension, Json};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::{
    dto::{DeactivateAccountRequest, MessageResponse, UpdateMeRequest, UserMeResponse},
    error::{ApiError, ApiResult, ErrorCode},
    middleware::AuthContext,
    models::users::{self, Entity as Users},
    services::{
        account_profile::{self as profile, ApplyError, ProfileChange},
        account_security::{self as security, Outcome, SecurityEvent},
        auth::{verify_password, Claims},
    },
    state::AppState,
};

/// v1 representation (ACC-022: one status rendering for every v1 sign-in response).
fn me_response(state: &AppState, user: users::Model) -> UserMeResponse {
    UserMeResponse {
        id: user.id,
        status: state
            .auth_policy
            .status_format
            .render(&user.status)
            .to_owned(),
        role: format!("{:?}", user.role).to_lowercase(),
        created_at: user.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        email: user.email,
        username: user.username,
    }
}

/// Get current authenticated user's information
///
/// Returns the full user profile for the authenticated user.
/// Requires authentication via JWT token. `status` is `pending_verification`, `active`,
/// `inactive` or `suspended`; until `ACCOUNT_STATUS_FORMAT=canonical` is set the unverified
/// state is spelled `pendingverification` (historical v1 value). The full profile (display
/// name, avatar, time zone, locale) is `GET /api/v2/me/profile`.
#[utoipa::path(
    get,
    path = "/api/v1/users/me",
    tag = "Users",
    responses(
        (status = 200, description = "Current user information", body = UserMeResponse),
        (status = 401, description = "Unauthorized - missing or invalid token"),
        (status = 403, description = "Forbidden - account deactivated"),
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_current_user(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
) -> ApiResult<Json<UserMeResponse>> {
    let user = Users::find_by_id(auth_context.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;
    Ok(Json(me_response(&state, user)))
}

/// Update current authenticated user's profile
///
/// Allows users to update their username (3-50 letters, digits or underscores after
/// surrounding spaces are removed: the registration rule). Email, role, status, and password
/// cannot be updated through this endpoint. Display name, avatar, time zone and locale are
/// changed with `PATCH /api/v2/me/profile`.
#[utoipa::path(
    patch,
    path = "/api/v1/users/me",
    tag = "Users",
    request_body = UpdateMeRequest,
    responses(
        (status = 200, description = "Profile updated successfully", body = UserMeResponse),
        (status = 400, description = "Validation error or no fields to update"),
        (status = 401, description = "Unauthorized - missing or invalid token"),
        (status = 403, description = "Forbidden - account deactivated"),
        (status = 409, description = "Conflict - username already taken"),
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn update_current_user(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Json(payload): Json<UpdateMeRequest>,
) -> ApiResult<Json<UserMeResponse>> {
    let Some(username) = payload.username else {
        return Err(ApiError::BadRequest(
            "No fields to update. Please provide at least one field.".to_string(),
        ));
    };
    // ACC-021: the registration rule, applied to the trimmed value.
    let change = ProfileChange {
        username: Some(username),
        ..Default::default()
    }
    .validated()
    .map_err(|e| ApiError::ValidationError(e.0.to_string()))?;
    let updated = match profile::apply(&state.db, auth_context.user_id, &change).await {
        Ok(u) => u,
        Err(ApplyError::NotFound) => {
            return Err(ApiError::Unauthorized("User not found".to_string()))
        }
        Err(ApplyError::UsernameTaken) => {
            return Err(ApiError::Conflict("Username is already taken".to_string()))
        }
        Err(ApplyError::Db(e)) => return Err(e),
    };
    Ok(Json(me_response(&state, updated)))
}

/// Deactivate current user's account (soft delete)
///
/// Sets the user's status to Inactive. This immediately invalidates all existing
/// tokens as the auth middleware checks user status on every request.
/// This operation is idempotent - calling it multiple times has the same effect.
///
/// Re-authentication (ACC-026): send `current_password`, or call this within 10 minutes of
/// signing in (a fresh sign-in session). Otherwise the answer is `403
/// REAUTHENTICATION_REQUIRED` and nothing changes. Accounts without a password (Google sign-in)
/// sign in again first. Both outcomes are recorded in the account security audit. An
/// administrator reverses a deactivation with `PATCH /api/v1/admin/users/{id}` (`status:
/// active`); the user then signs in again.
#[utoipa::path(
    post,
    path = "/api/v1/users/me/deactivate",
    tag = "Users",
    request_body(content = Option<DeactivateAccountRequest>, description = "Optional: the current password (required unless the caller signed in recently)"),
    responses(
        (status = 200, description = "Account deactivated successfully", body = MessageResponse),
        (status = 401, description = "Unauthorized - missing or invalid token"),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - wrong or missing current password and no recent sign-in"),
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_account(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
    Extension(claims): Extension<Claims>,
    body: Option<Json<DeactivateAccountRequest>>,
) -> ApiResult<Json<MessageResponse>> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    // Query current user from database
    let user = Users::find_by_id(auth_context.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;

    // Check if already inactive (idempotent behavior)
    if user.status == users::UserStatus::Inactive {
        return Ok(Json(MessageResponse {
            message: "Account is already deactivated".to_string(),
        }));
    }

    // ACC-026: a stolen or stale token alone cannot deactivate the account.
    let method = reauthentication(&state, &claims, &user, body.current_password.as_deref())?;
    let Some(method) = method else {
        security::record_event_best_effort(
            &state.db,
            SecurityEvent::new("ACCOUNT_DEACTIVATED", Outcome::Denied)
                .by_user(user.id)
                .detail(serde_json::json!({ "reason": "REAUTHENTICATION_REQUIRED" })),
        )
        .await;
        return Err(ApiError::domain(
            ErrorCode::ReauthenticationRequired,
            "Confirm your current password, or sign in again, to deactivate the account",
        ));
    };

    // Update user status to Inactive
    let user_id = user.id;
    let mut user_active: users::ActiveModel = user.into();
    user_active.status = Set(users::UserStatus::Inactive);
    user_active.updated_at = Set(chrono::Utc::now().naive_utc());

    // Save changes (the Section 2 trigger closes every session and advances the epoch).
    user_active.update(&state.db).await?;
    security::record_event_best_effort(
        &state.db,
        SecurityEvent::new("ACCOUNT_DEACTIVATED", Outcome::Success)
            .by_user(user_id)
            .detail(serde_json::json!({ "reauthentication": method })),
    )
    .await;
    // Section 14 (ACC-010): Messaging, Calling, the Hub and Platform Access follow.
    crate::services::communications::credentials_ended(
        &state,
        auth_context.user_id,
        "ACCOUNT_DISABLED",
    )
    .await;

    Ok(Json(MessageResponse {
        message: "Account deactivated successfully".to_string(),
    }))
}

/// ACC-026: how the caller proved it is the account holder (`None`: it did not).
/// A wrong password never falls back to the recent-sign-in rule.
fn reauthentication(
    state: &AppState,
    claims: &Claims,
    user: &users::Model,
    current_password: Option<&str>,
) -> ApiResult<Option<&'static str>> {
    let policy = &state.auth_policy;
    if !policy.deactivation_requires_reauth {
        return Ok(Some("NOT_REQUIRED"));
    }
    if let Some(password) = current_password {
        let ok = match user.password_hash.as_deref() {
            Some(hash) => verify_password(password, hash)?,
            None => false,
        };
        return Ok(ok.then_some("PASSWORD"));
    }
    let now = chrono::Utc::now().timestamp();
    Ok(policy
        .recently_authenticated(claims, now)
        .then_some("RECENT_SIGN_IN"))
}
