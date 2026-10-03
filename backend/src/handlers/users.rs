use axum::{extract::State, Extension, Json};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use validator::Validate;

use crate::{
    dto::{MessageResponse, UpdateMeRequest, UserMeResponse},
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    models::users::{self, Entity as Users},
    state::AppState,
};

/// Get current authenticated user's information
///
/// Returns the full user profile for the authenticated user.
/// Requires authentication via JWT token.
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
    // Query user from database
    let user = Users::find_by_id(auth_context.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;

    // Map to response DTO
    let response = UserMeResponse {
        id: user.id,
        email: user.email,
        username: user.username,
        role: format!("{:?}", user.role).to_lowercase(),
        status: format!("{:?}", user.status)
            .to_lowercase()
            .replace('_', "_"),
        created_at: user.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    };

    Ok(Json(response))
}

/// Update current authenticated user's profile
///
/// Allows users to update their username. Email, role, status, and password
/// cannot be updated through this endpoint.
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
    // Validate request
    payload
        .validate()
        .map_err(|e| ApiError::ValidationError(e.to_string()))?;

    // Check if at least one field is provided
    if payload.username.is_none() {
        return Err(ApiError::BadRequest(
            "No fields to update. Please provide at least one field.".to_string(),
        ));
    }

    // Query current user from database
    let user = Users::find_by_id(auth_context.user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;

    // Check if username is being changed and if it's already taken
    if let Some(ref new_username) = payload.username {
        let trimmed_username = new_username.trim();
        
        // Only check if username is actually different
        if trimmed_username != user.username {
            let existing_user = Users::find()
                .filter(users::Column::Username.eq(trimmed_username))
                .one(&state.db)
                .await?;

            if existing_user.is_some() {
                return Err(ApiError::Conflict(
                    "Username is already taken".to_string(),
                ));
            }
        }
    }

    // Update user using ActiveModel
    let mut user_active: users::ActiveModel = user.into();

    // Update fields if provided
    if let Some(username) = payload.username {
        user_active.username = Set(username.trim().to_string());
    }

    // Always update the updated_at timestamp
    user_active.updated_at = Set(chrono::Utc::now().naive_utc());

    // Save changes
    let updated_user = user_active.update(&state.db).await?;

    // Return updated user profile
    let response = UserMeResponse {
        id: updated_user.id,
        email: updated_user.email,
        username: updated_user.username,
        role: format!("{:?}", updated_user.role).to_lowercase(),
        status: format!("{:?}", updated_user.status)
            .to_lowercase()
            .replace('_', "_"),
        created_at: updated_user.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    };

    Ok(Json(response))
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

    Ok(Json(MessageResponse {
        message: "Account deactivated successfully".to_string(),
    }))
}
