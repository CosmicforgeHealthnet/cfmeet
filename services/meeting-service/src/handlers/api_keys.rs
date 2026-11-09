//! API Key Handlers
//!
//! HTTP handlers for API key management.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use rand::Rng;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use uuid::Uuid;

use crate::{
    dto::{
        ApiKeyApiResponse, ApiKeyResponse, CreateApiKeyApiResponse, CreateApiKeyRequest,
        CreateApiKeyResponse, ListApiKeysResponse, RevokeApiKeyResponse,
    },
    error::{ApiError, ApiResult},
    models::api_keys::{self, ApiKeyStatus, Entity as ApiKeys},
    services::{auth::Claims, legacy_credentials},
    state::AppState,
    utils::now_utc,
};

/// Generate a secure random API key
fn generate_api_key() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..64)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Format API key status to string
fn format_status(status: &ApiKeyStatus) -> String {
    match status {
        ApiKeyStatus::Active => "active".to_string(),
        ApiKeyStatus::Revoked => "revoked".to_string(),
    }
}

/// Create a new API key
///
/// Generates a new API key owned by the authenticated user. The request body currently takes no
/// fields: every key gets a usage limit of 100 requests and expires 20 days after creation. The
/// full key value is returned only in this response and is never stored by the service (only a
/// one-way verifier and its first eight characters are kept), so store it now; it cannot be shown
/// again. Send it in the `Api-Key` header on endpoints that accept API-key authentication.
#[utoipa::path(
    post,
    path = "/api/v1/api-keys",
    tag = "API Keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 201, description = "API key created successfully", body = CreateApiKeyApiResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> ApiResult<(StatusCode, Json<CreateApiKeyApiResponse>)> {
    // Validation not needed - no user-configurable fields
    let _ = payload;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let now = now_utc();

    // System-controlled defaults
    const DEFAULT_USAGE_LIMIT: i32 = 100;
    let expires_at = now + chrono::Duration::days(20);

    // Generate API key
    let raw_api_key = generate_api_key();
    let api_key_id = Uuid::new_v4();

    // PLT-002: only a verifier and the lookup prefix are stored, never the key.
    let api_key = api_keys::ActiveModel {
        id: Set(api_key_id),
        user_id: Set(user_id),
        api_key: Set(legacy_credentials::api_key_placeholder(api_key_id)),
        key_hash: Set(Some(legacy_credentials::api_key_hash(&raw_api_key))),
        key_prefix: Set(legacy_credentials::api_key_prefix(&raw_api_key)),
        usage_limit: Set(DEFAULT_USAGE_LIMIT),
        used_count: Set(0),
        expires_at: Set(expires_at),
        status: Set(ApiKeyStatus::Active),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let api_key = api_key.insert(&state.db).await?;

    let response = CreateApiKeyResponse {
        id: api_key.id,
        api_key: raw_api_key,
        usage_limit: api_key.usage_limit,
        used_count: api_key.used_count,
        expires_at: api_key.expires_at.and_utc(),
        status: format_status(&api_key.status),
        created_at: api_key.created_at.and_utc(),
    };

    Ok((
        StatusCode::CREATED,
        Json(CreateApiKeyApiResponse {
            success: true,
            data: response,
        }),
    ))
}

/// List all API keys for the authenticated user
///
/// Returns every API key owned by the authenticated user, both active and revoked, newest first.
/// Key values are masked: only the first eight characters (the non-secret lookup prefix) are shown,
/// followed by asterisks; usage count, usage limit, expiry and status are included. Not paginated.
#[utoipa::path(
    get,
    path = "/api/v1/api-keys",
    tag = "API Keys",
    responses(
        (status = 200, description = "List of API keys", body = ListApiKeysResponse),
        (status = 401, description = "Unauthorized")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<ListApiKeysResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let api_keys_list = ApiKeys::find()
        .filter(api_keys::Column::UserId.eq(user_id))
        .order_by_desc(api_keys::Column::CreatedAt)
        .all(&state.db)
        .await?;

    let data: Vec<ApiKeyResponse> = api_keys_list
        .into_iter()
        .map(|key| ApiKeyResponse {
            id: key.id,
            api_key_masked: legacy_credentials::masked(&key),
            usage_limit: key.usage_limit,
            used_count: key.used_count,
            expires_at: key.expires_at.and_utc(),
            status: format_status(&key.status),
            created_at: key.created_at.and_utc(),
            updated_at: key.updated_at.and_utc(),
        })
        .collect();

    Ok(Json(ListApiKeysResponse {
        success: true,
        data,
    }))
}

/// Get a specific API key by ID
///
/// Returns one API key owned by the authenticated user, with the key value masked (only its first
/// eight characters are shown), plus its usage count, usage limit, expiry and status. Returns 404 if the key does
/// not exist or belongs to another user.
#[utoipa::path(
    get,
    path = "/api/v1/api-keys/{id}",
    tag = "API Keys",
    params(
        ("id" = Uuid, Path, description = "API Key UUID")
    ),
    responses(
        (status = 200, description = "API key details", body = ApiKeyApiResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "API key not found")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn get_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ApiKeyApiResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let api_key = ApiKeys::find_by_id(id)
        .filter(api_keys::Column::UserId.eq(user_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("API key not found".to_string()))?;

    let response = ApiKeyResponse {
        id: api_key.id,
        api_key_masked: legacy_credentials::masked(&api_key),
        usage_limit: api_key.usage_limit,
        used_count: api_key.used_count,
        expires_at: api_key.expires_at.and_utc(),
        status: format_status(&api_key.status),
        created_at: api_key.created_at.and_utc(),
        updated_at: api_key.updated_at.and_utc(),
    };

    Ok(Json(ApiKeyApiResponse {
        success: true,
        data: response,
    }))
}

/// Revoke an API key
///
/// Marks an API key owned by the authenticated user as revoked so it can no longer authenticate
/// requests. The key is not deleted: it stays in the list with status `revoked`. Revoking a key
/// that is already revoked returns 409.
#[utoipa::path(
    delete,
    path = "/api/v1/api-keys/{id}",
    tag = "API Keys",
    params(
        ("id" = Uuid, Path, description = "API Key UUID")
    ),
    responses(
        (status = 200, description = "API key revoked successfully", body = RevokeApiKeyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "API key not found"),
        (status = 409, description = "API key already revoked")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
pub async fn revoke_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RevokeApiKeyResponse>> {
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| ApiError::Unauthorized("Invalid token".to_string()))?;

    let api_key = ApiKeys::find_by_id(id)
        .filter(api_keys::Column::UserId.eq(user_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("API key not found".to_string()))?;

    if api_key.status == ApiKeyStatus::Revoked {
        return Err(ApiError::Conflict("API key already revoked".to_string()));
    }

    let mut api_key: api_keys::ActiveModel = api_key.into();
    api_key.status = Set(ApiKeyStatus::Revoked);
    api_key.updated_at = Set(now_utc());
    api_key.update(&state.db).await?;

    Ok(Json(RevokeApiKeyResponse {
        success: true,
        message: "API key revoked successfully".to_string(),
    }))
}
