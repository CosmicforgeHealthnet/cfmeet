use crate::{
    error::{ApiError, ApiResult},
    models::users::{self, UserRole, UserStatus},
    state::AppState,
};
use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use sea_orm::EntityTrait;
use uuid::Uuid;

/// Authentication context extracted from JWT and validated against database
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub role: UserRole,
    pub status: UserStatus,
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> ApiResult<Response> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing authorization header".to_string()))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::Unauthorized("Invalid authorization format".to_string()))?;

    // Check if token is blacklisted (logout)
    if state.token_blacklist.is_blacklisted(token).await? {
        return Err(ApiError::Unauthorized("Token has been revoked".to_string()));
    }

    // Verify token
    let claims = state
        .jwt_service
        .verify_token(token)
        .map_err(|_| ApiError::Unauthorized("Invalid or expired token".to_string()))?;

    // Check for Guest Role
    if claims.role == "guest" {
        use crate::models::participants::Entity as Participants;
        
        let participant_id = Uuid::parse_str(&claims.sub).map_err(|_| {
            ApiError::Unauthorized("Invalid participant identifier in token".to_string())
        })?;

        let participant = Participants::find_by_id(participant_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| ApiError::Unauthorized("Guest participant not found".to_string()))?;

        // Create auth context for guest
        // We set status to Active as guests don't have account status
        // We set user_id to participant_id for guests in the context, but this field is typed as Uuid so it fits.
        // NOTE: Handlers must know that if role is Guest, user_id is actually a participant_id (or we rely on meeting ownership checks which won't match anyway).
        let auth_context = AuthContext {
            user_id: participant.id, // Using participant_id as the "user_id" in context
            role: UserRole::Guest,
            status: UserStatus::Active,
        };

        request.extensions_mut().insert(claims);
        request.extensions_mut().insert(auth_context);

        return Ok(next.run(request).await);
    }

    // Parse user ID from claims
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
        ApiError::Unauthorized("Invalid user identifier in token".to_string())
    })?;

    // Query user from database
    let user = users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;

    // Check if user account is active
    if user.status == UserStatus::Inactive {
        return Err(ApiError::Forbidden(
            "Account has been deactivated".to_string(),
        ));
    }

    // Create auth context
    let auth_context = AuthContext {
        user_id: user.id,
        role: user.role.clone(),
        status: user.status.clone(),
    };

    // Add both claims (for backward compatibility) and auth context to request extensions
    request.extensions_mut().insert(claims);
    request.extensions_mut().insert(auth_context);

    Ok(next.run(request).await)
}
