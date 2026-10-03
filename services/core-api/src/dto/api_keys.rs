//! API Key DTOs
//!
//! Data Transfer Objects for API key management endpoints.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Request to create a new API key (no user-configurable fields - system sets defaults)
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct CreateApiKeyRequest {}

/// Response when creating an API key (includes the full key - only shown once)
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub id: Uuid,
    /// The full API key - only returned on creation, never retrievable again
    pub api_key: String,
    pub usage_limit: i32,
    pub used_count: i32,
    pub expires_at: NaiveDateTime,
    pub status: String,
    pub created_at: NaiveDateTime,
}

/// Response for API key info (masked key)
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyResponse {
    pub id: Uuid,
    /// Masked API key (shows last 4 characters only)
    pub api_key_masked: String,
    pub usage_limit: i32,
    pub used_count: i32,
    pub expires_at: NaiveDateTime,
    pub status: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Response for listing API keys
#[derive(Debug, Serialize, ToSchema)]
pub struct ListApiKeysResponse {
    pub success: bool,
    pub data: Vec<ApiKeyResponse>,
}

/// API response wrapper for create API key
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateApiKeyApiResponse {
    pub success: bool,
    pub data: CreateApiKeyResponse,
}

/// API response wrapper for single API key
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyApiResponse {
    pub success: bool,
    pub data: ApiKeyResponse,
}

/// Response for API key revocation
#[derive(Debug, Serialize, ToSchema)]
pub struct RevokeApiKeyResponse {
    pub success: bool,
    pub message: String,
}

/// Claims extracted from API key authentication
#[derive(Debug, Clone)]
pub struct ApiKeyClaims {
    pub user_id: Uuid,
    pub api_key_id: Uuid,
}
