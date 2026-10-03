//! Token Blacklist Service
//!
//! Handles JWT token blacklisting for proper logout functionality using Redis.
//! Tokens are stored with a TTL matching their expiration time.

use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::error::ApiResult;
use crate::services::RedisService;

/// Token Blacklist Service for managing revoked JWT tokens
#[derive(Clone)]
pub struct TokenBlacklistService {
    redis: Arc<RedisService>,
}

impl TokenBlacklistService {
    /// Create a new TokenBlacklistService
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }

    /// Generate Redis key for a blacklisted token
    /// Uses SHA-256 hash to avoid storing the actual token
    fn token_key(&self, token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let hash = hasher.finalize();
        format!("blacklist:{:x}", hash)
    }

    /// Add a token to the blacklist
    ///
    /// # Arguments
    /// * `token` - The JWT token to blacklist
    /// * `ttl_secs` - Time-to-live in seconds (should match token expiry)
    pub async fn blacklist_token(&self, token: &str, ttl_secs: u64) -> ApiResult<()> {
        let key = self.token_key(token);
        self.redis.set(&key, "1", Some(ttl_secs)).await?;
        tracing::debug!("Token blacklisted: TTL={}s", ttl_secs);
        Ok(())
    }

    /// Check if a token is blacklisted
    ///
    /// # Arguments
    /// * `token` - The JWT token to check
    ///
    /// # Returns
    /// * `true` if the token is blacklisted
    /// * `false` if the token is not blacklisted (or expired from blacklist)
    pub async fn is_blacklisted(&self, token: &str) -> ApiResult<bool> {
        let key = self.token_key(token);
        self.redis.exists(&key).await
    }

    /// Blacklist both access and refresh tokens (for logout)
    ///
    /// # Arguments
    /// * `access_token` - The access token to blacklist
    /// * `refresh_token` - The refresh token to blacklist (optional)
    /// * `access_ttl` - TTL for access token blacklist entry
    /// * `refresh_ttl` - TTL for refresh token blacklist entry
    pub async fn blacklist_tokens(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
        access_ttl: u64,
        refresh_ttl: u64,
    ) -> ApiResult<()> {
        self.blacklist_token(access_token, access_ttl).await?;

        if let Some(refresh) = refresh_token {
            self.blacklist_token(refresh, refresh_ttl).await?;
        }

        Ok(())
    }
}
