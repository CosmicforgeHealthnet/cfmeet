//! API Key Authentication Middleware
//!
//! Authenticates third-party requests with the legacy `Api-Key` header. Keys are verified
//! against a stored one-way verifier in constant time and every use is consumed with one
//! atomic conditional update (PLT-002 / SEC-006; `services::legacy_credentials`).

use crate::{
    dto::ApiKeyClaims,
    error::{ApiError, ApiResult},
    services::legacy_credentials,
    state::AppState,
    utils::now_utc,
};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

/// API key authentication middleware
///
/// Extracts the API key from the `Api-Key` header and validates it.
/// On success, adds `ApiKeyClaims` to request extensions.
pub async fn api_key_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> ApiResult<Response> {
    let presented = request
        .headers()
        .get("Api-Key")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing Api-Key header".to_string()))?;

    let api_key = legacy_credentials::find_api_key(&state.db, presented)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Invalid API key".to_string()))?;

    legacy_credentials::consume_use(&state.db, &api_key, now_utc())
        .await?
        .map_err(|refusal| refusal.into_error())?;

    request.extensions_mut().insert(ApiKeyClaims {
        user_id: api_key.user_id,
        api_key_id: api_key.id,
    });

    Ok(next.run(request).await)
}
