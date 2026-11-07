use axum::{extract::State, Extension, Json};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::{
    dto::{MessageResponse, UpdateMeRequest, UserMeResponse},
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    models::users::{self, Entity as Users},
    services::account_profile::{self as profile, ApplyError, ProfileChange},
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
#[utoipa::path(
    post,
    path = "/api/v1/users/me/deactivate",
    tag = "Users",
    responses(
        (status = 200, description = "Account deactivated successfully", body = MessageResponse),
        (status = 401, description = "Unauthorized - missing or invalid token"),
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn deactivate_account(
    State(state): State<AppState>,
    Extension(auth_context): Extension<AuthContext>,
) -> ApiResult<Json<MessageResponse>> {
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

    // Update user status to Inactive
    let mut user_active: users::ActiveModel = user.into();
    user_active.status = Set(users::UserStatus::Inactive);
    user_active.updated_at = Set(chrono::Utc::now().naive_utc());

    // Save changes
    user_active.update(&state.db).await?;
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
