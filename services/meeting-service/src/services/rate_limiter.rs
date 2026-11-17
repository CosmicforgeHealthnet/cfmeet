//! Rate Limiter Service
//!
//! Simple rate limiting using Redis counters.

use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::services::RedisService;

/// TTL for login rate limit window (15 minutes)
const LOGIN_RATE_LIMIT_TTL_SECS: u64 = 15 * 60;

/// Maximum login attempts per window
const MAX_LOGIN_ATTEMPTS: i64 = 5;

/// Gap audit A9: failed sign-ins per account and window, across every client address.
pub const MAX_LOGIN_ATTEMPTS_ACCOUNT_WIDE: i64 = 50;

/// Rate Limiter Service
#[derive(Clone)]
pub struct RateLimiterService {
    redis: Arc<RedisService>,
}

impl RateLimiterService {
    /// Create a new RateLimiterService
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }

    /// Generate Redis key for login rate limiting
    fn login_key(&self, identifier: &str) -> String {
        format!("rate:login:{}", identifier.to_lowercase())
    }

    /// Check and increment login rate limit
    ///
    /// Call this ONCE per login attempt (before processing).
    /// Returns Ok(remaining_attempts) if under limit, Err if rate limited.
    pub async fn check_login_rate(&self, identifier: &str) -> ApiResult<i64> {
        let key = self.login_key(identifier);
        let count = self
            .redis
            .incr_with_ttl(&key, LOGIN_RATE_LIMIT_TTL_SECS)
            .await?;

        if count > MAX_LOGIN_ATTEMPTS {
            let ttl = self.redis.ttl(&key).await?;
            return Err(ApiError::TooManyRequests(format!(
                "Too many login attempts. Please try again in {} seconds.",
                ttl.max(0)
            )));
        }

        Ok(MAX_LOGIN_ATTEMPTS - count)
    }

    /// Generic fixed-window limit for one action and one subject (for example a client
    /// address). The subject is hashed, so raw addresses never appear in Redis keys.
    pub async fn check_action_rate(
        &self,
        action: &str,
        subject: &str,
        max_attempts: i64,
        window_secs: u64,
    ) -> ApiResult<()> {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(subject.as_bytes()));
        let key = format!("rate:{action}:{}", &digest[..32]);
        let count = self.redis.incr_with_ttl(&key, window_secs).await?;
        if count > max_attempts {
            let ttl = self.redis.ttl(&key).await?;
            return Err(ApiError::TooManyRequests(format!(
                "Too many requests. Please try again in {} seconds.",
                ttl.max(0)
            )));
        }
        Ok(())
    }

    /// Clear login rate limit on successful login
    pub async fn clear_login_rate(&self, identifier: &str) -> ApiResult<()> {
        let key = self.login_key(identifier);
        self.redis.del(&key).await?;
        Ok(())
    }

    /// Gap audit A9: with a trusted client address, failed sign-ins are counted per
    /// (account, address), so one source cannot lock the owner out from everywhere, under
    /// an account-wide ceiling against distributed guessing
    /// ([`MAX_LOGIN_ATTEMPTS_ACCOUNT_WIDE`] per window). Without an address, the
    /// per-account counter of [`Self::check_login_rate`] applies.
    pub async fn check_login_rate_from(
        &self,
        identifier: &str,
        client: Option<&str>,
    ) -> ApiResult<()> {
        let Some(client) = client else {
            return self.check_login_rate(identifier).await.map(|_| ());
        };
        for (key, max) in [
            (
                self.login_client_key(identifier, client),
                MAX_LOGIN_ATTEMPTS,
            ),
            (
                self.login_account_key(identifier),
                MAX_LOGIN_ATTEMPTS_ACCOUNT_WIDE,
            ),
        ] {
            let count = self
                .redis
                .incr_with_ttl(&key, LOGIN_RATE_LIMIT_TTL_SECS)
                .await?;
            if count > max {
                let ttl = self.redis.ttl(&key).await?;
                return Err(ApiError::TooManyRequests(format!(
                    "Too many login attempts. Please try again in {} seconds.",
                    ttl.max(0)
                )));
            }
        }
        Ok(())
    }

    /// Clears the counters of [`Self::check_login_rate_from`] after a successful sign-in.
    pub async fn clear_login_rate_from(
        &self,
        identifier: &str,
        client: Option<&str>,
    ) -> ApiResult<()> {
        self.clear_login_rate(identifier).await?;
        if let Some(client) = client {
            self.redis
                .del(&self.login_client_key(identifier, client))
                .await?;
            self.redis.del(&self.login_account_key(identifier)).await?;
        }
        Ok(())
    }

    fn login_client_key(&self, identifier: &str, client: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(client.as_bytes()));
        format!("rate:login:{}:{}", identifier.to_lowercase(), &digest[..24])
    }

    fn login_account_key(&self, identifier: &str) -> String {
        format!("rate:login-account:{}", identifier.to_lowercase())
    }
}
