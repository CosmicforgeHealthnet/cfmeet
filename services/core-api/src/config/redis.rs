//! Redis Configuration
//!
//! Redis connection settings loaded from environment variables.

use crate::error::{ApiError, ApiResult};

/// Redis configuration
#[derive(Clone)]
pub struct RedisConfig {
    /// Redis connection URL (e.g., redis://localhost:6379)
    pub url: String,

    /// Connection pool size
    pub pool_size: usize,

    /// Connection timeout in seconds
    pub timeout_secs: u64,
}

impl RedisConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> ApiResult<Self> {
        Ok(Self {
            url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),

            pool_size: std::env::var("REDIS_POOL_SIZE")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .map_err(|_| ApiError::InternalError("Invalid REDIS_POOL_SIZE".to_string()))?,

            timeout_secs: std::env::var("REDIS_TIMEOUT_SECS")
                .unwrap_or_else(|_| "5".to_string())
                .parse()
                .map_err(|_| ApiError::InternalError("Invalid REDIS_TIMEOUT_SECS".to_string()))?,
        })
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".to_string(),
            pool_size: 10,
            timeout_secs: 5,
        }
    }
}
