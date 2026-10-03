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
        let count = self.redis.incr_with_ttl(&key, LOGIN_RATE_LIMIT_TTL_SECS).await?;

        if count > MAX_LOGIN_ATTEMPTS {
            let ttl = self.redis.ttl(&key).await?;
            return Err(ApiError::TooManyRequests(format!(
                "Too many login attempts. Please try again in {} seconds.",
                ttl.max(0)
            )));
        }

        Ok(MAX_LOGIN_ATTEMPTS - count)
    }

    /// Clear login rate limit on successful login
    pub async fn clear_login_rate(&self, identifier: &str) -> ApiResult<()> {
        let key = self.login_key(identifier);
        self.redis.del(&key).await?;
        Ok(())
    }
}
