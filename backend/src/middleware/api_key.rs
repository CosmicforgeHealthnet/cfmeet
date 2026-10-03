//! API Key Authentication Middleware
//!
//! Middleware for authenticating requests using API keys.
//! Uses the `Api-Key` header for authentication.

use crate::{
    dto::ApiKeyClaims,
    error::{ApiError, ApiResult},
    models::api_keys::{self, ApiKeyStatus, Entity as ApiKeys},
    state::AppState,
    utils::now_utc,
};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

/// API key authentication middleware
///
/// Extracts API key from the `Api-Key` header and validates it.
/// On success, adds `ApiKeyClaims` to request extensions.
pub async fn api_key_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> ApiResult<Response> {
    let api_key_header = request
        .headers()
        .get("Api-Key")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing Api-Key header".to_string()))?;

    // Find API key in database
    let api_key = ApiKeys::find()
        .filter(api_keys::Column::ApiKey.eq(api_key_header))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Invalid API key".to_string()))?;

    // Check if API key is active
    if api_key.status != ApiKeyStatus::Active {
        return Err(ApiError::Unauthorized("API key has been revoked".to_string()));
    }

    // Check if API key has expired
    let now = now_utc();
    if api_key.expires_at < now {
        return Err(ApiError::Unauthorized("API key has expired".to_string()));
    }

    // Check usage quota
    if api_key.used_count >= api_key.usage_limit {
        return Err(ApiError::Forbidden(
            "API key usage limit exceeded".to_string(),
        ));
    }

    // Increment usage count
    let mut api_key_update: api_keys::ActiveModel = api_key.clone().into();
    api_key_update.used_count = Set(api_key.used_count + 1);
    api_key_update.updated_at = Set(now);
    api_key_update.update(&state.db).await?;

    // Add claims to request extensions
    let claims = ApiKeyClaims {
        user_id: api_key.user_id,
        api_key_id: api_key.id,
    };
    request.extensions_mut().insert(claims);

    Ok(next.run(request).await)
}

/// Combined authentication middleware
///
/// Tries JWT authentication first (Bearer token), then falls back to API key.
/// This allows both authentication methods on the same endpoint.
pub async fn combined_auth_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> ApiResult<Response> {
    // Check for Bearer token first
    if let Some(auth_header) = request.headers().get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                // Try JWT authentication
                if let Ok(claims) = state.jwt_service.verify_token(token) {
                    request.extensions_mut().insert(claims);
                    return Ok(next.run(request).await);
                }
            }
        }
    }

    // Fall back to API key authentication
    if let Some(api_key_header) = request.headers().get("Api-Key") {
        if let Ok(api_key_str) = api_key_header.to_str() {
            // Find API key in database
            if let Ok(Some(api_key)) = ApiKeys::find()
                .filter(api_keys::Column::ApiKey.eq(api_key_str))
                .one(&state.db)
                .await
            {
                // Check if API key is active
                if api_key.status != ApiKeyStatus::Active {
                    return Err(ApiError::Unauthorized("API key has been revoked".to_string()));
                }

                // Check if API key has expired
                let now = now_utc();
                if api_key.expires_at < now {
                    return Err(ApiError::Unauthorized("API key has expired".to_string()));
                }

                // Check usage quota
                if api_key.used_count >= api_key.usage_limit {
                    return Err(ApiError::Forbidden(
                        "API key usage limit exceeded".to_string(),
                    ));
                }

                // Increment usage count
                let mut api_key_update: api_keys::ActiveModel = api_key.clone().into();
                api_key_update.used_count = Set(api_key.used_count + 1);
                api_key_update.updated_at = Set(now);
                if let Err(e) = api_key_update.update(&state.db).await {
                    tracing::error!("Failed to update API key usage count: {}", e);
                }

                // Add claims to request extensions
                let claims = ApiKeyClaims {
                    user_id: api_key.user_id,
                    api_key_id: api_key.id,
                };
                request.extensions_mut().insert(claims);

                return Ok(next.run(request).await);
            }
        }
    }

    Err(ApiError::Unauthorized(
        "Missing or invalid authentication. Use Bearer token or Api-Key header.".to_string(),
    ))
}
