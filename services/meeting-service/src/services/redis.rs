//! Redis Service
//!
//! Provides Redis connection pool and basic helper methods.

use deadpool_redis::{Config, Pool, Runtime};
use redis::AsyncCommands;

use crate::config::RedisConfig;
use crate::error::{ApiError, ApiResult};

/// Lua script for atomic increment with TTL (sets TTL only on first increment)
const INCR_WITH_TTL_SCRIPT: &str = r#"
local value = redis.call('INCR', KEYS[1])
if value == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return value
"#;

/// Redis service for managing connections and operations
#[derive(Clone)]
pub struct RedisService {
    pool: Pool,
}

impl RedisService {
    /// Create a new RedisService with connection pool
    pub async fn new(config: &RedisConfig) -> ApiResult<Self> {
        let cfg = Config::from_url(&config.url);

        let pool = cfg
            .builder()
            .map_err(|e| ApiError::InternalError(format!("Redis pool builder error: {}", e)))?
            .max_size(config.pool_size)
            .runtime(Runtime::Tokio1)
            .build()
            .map_err(|e| ApiError::InternalError(format!("Redis pool creation error: {}", e)))?;

        // Test connection
        let mut conn = pool
            .get()
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis connection error: {}", e)))?;

        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis PING failed: {}", e)))?;

        Ok(Self { pool })
    }

    /// Get a connection from the pool
    pub async fn get_conn(&self) -> ApiResult<deadpool_redis::Connection> {
        self.pool
            .get()
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis connection error: {}", e)))
    }

    /// Set a key with optional TTL (in seconds)
    pub async fn set(&self, key: &str, value: &str, ttl_secs: Option<u64>) -> ApiResult<()> {
        let mut conn = self.get_conn().await?;

        if let Some(ttl) = ttl_secs {
            let _: () = conn
                .set_ex(key, value, ttl)
                .await
                .map_err(|e| ApiError::InternalError(format!("Redis SET error: {}", e)))?;
        } else {
            let _: () = conn
                .set(key, value)
                .await
                .map_err(|e| ApiError::InternalError(format!("Redis SET error: {}", e)))?;
        }

        Ok(())
    }

    /// Get a value by key
    pub async fn get(&self, key: &str) -> ApiResult<Option<String>> {
        let mut conn = self.get_conn().await?;

        let value: Option<String> = conn
            .get(key)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis GET error: {}", e)))?;

        Ok(value)
    }

    /// Delete a key
    pub async fn del(&self, key: &str) -> ApiResult<bool> {
        let mut conn = self.get_conn().await?;

        let deleted: i32 = conn
            .del(key)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis DEL error: {}", e)))?;

        Ok(deleted > 0)
    }

    /// Check if a key exists
    pub async fn exists(&self, key: &str) -> ApiResult<bool> {
        let mut conn = self.get_conn().await?;

        let exists: bool = conn
            .exists(key)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis EXISTS error: {}", e)))?;

        Ok(exists)
    }

    /// Increment a key with TTL (atomic - sets TTL only on first increment)
    pub async fn incr_with_ttl(&self, key: &str, ttl_secs: u64) -> ApiResult<i64> {
        let mut conn = self.get_conn().await?;

        let value: i64 = redis::Script::new(INCR_WITH_TTL_SCRIPT)
            .key(key)
            .arg(ttl_secs)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis INCR error: {}", e)))?;

        Ok(value)
    }

    /// Get TTL of a key in seconds (-1 if no TTL, -2 if key doesn't exist)
    pub async fn ttl(&self, key: &str) -> ApiResult<i64> {
        let mut conn = self.get_conn().await?;

        let ttl: i64 = conn
            .ttl(key)
            .await
            .map_err(|e| ApiError::InternalError(format!("Redis TTL error: {}", e)))?;

        Ok(ttl)
    }
}
