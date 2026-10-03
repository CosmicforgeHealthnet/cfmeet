//! OTP Service
//!
//! Handles OTP storage and verification using Redis.

use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::services::RedisService;

/// TTL for verification codes (24 hours)
const VERIFICATION_CODE_TTL_SECS: u64 = 24 * 60 * 60;

/// TTL for password reset codes (1 hour)
const RESET_CODE_TTL_SECS: u64 = 60 * 60;

/// TTL for rate limiting window (15 minutes)
const RATE_LIMIT_TTL_SECS: u64 = 15 * 60;

/// Maximum OTP attempts before rate limiting
const MAX_OTP_ATTEMPTS: i64 = 5;

/// Maximum OTP send requests per time window
const MAX_OTP_REQUESTS: i64 = 5;

/// OTP types
#[derive(Debug, Clone, Copy)]
pub enum OtpType {
    Verification,
    PasswordReset,
}

impl OtpType {
    fn key_prefix(&self) -> &'static str {
        match self {
            OtpType::Verification => "otp:verify",
            OtpType::PasswordReset => "otp:reset",
        }
    }

    fn ttl_secs(&self) -> u64 {
        match self {
            OtpType::Verification => VERIFICATION_CODE_TTL_SECS,
            OtpType::PasswordReset => RESET_CODE_TTL_SECS,
        }
    }
}

/// OTP Service
#[derive(Clone)]
pub struct OtpService {
    redis: Arc<RedisService>,
}

impl OtpService {
    /// Create a new OTP service
    pub fn new(redis: Arc<RedisService>) -> Self {
        Self { redis }
    }

    fn otp_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:{}", otp_type.key_prefix(), email.to_lowercase())
    }

    fn attempts_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:attempts:{}", otp_type.key_prefix(), email.to_lowercase())
    }

    fn send_rate_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:send_rate:{}", otp_type.key_prefix(), email.to_lowercase())
    }

    /// Store an OTP code
    pub async fn store_code(
        &self,
        email: &str,
        code: &str,
        otp_type: OtpType,
    ) -> ApiResult<()> {
        let key = self.otp_key(email, otp_type);
        self.redis.set(&key, code, Some(otp_type.ttl_secs())).await
    }

    /// Verify an OTP code
    pub async fn verify_code(
        &self,
        email: &str,
        code: &str,
        otp_type: OtpType,
    ) -> ApiResult<bool> {
        // Check rate limiting
        let attempts_key = self.attempts_key(email, otp_type);
        let attempts = self.redis.incr_with_ttl(&attempts_key, RATE_LIMIT_TTL_SECS).await?;

        if attempts > MAX_OTP_ATTEMPTS {
            return Err(ApiError::TooManyRequests(
                "Too many verification attempts. Please try again later.".to_string(),
            ));
        }

        // Get stored code
        let key = self.otp_key(email, otp_type);
        let stored_code = self.redis.get(&key).await?;

        match stored_code {
            Some(stored) if stored == code => {
                // Code valid - delete it
                self.redis.del(&key).await?;
                self.redis.del(&attempts_key).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Check if an OTP exists
    pub async fn has_code(&self, email: &str, otp_type: OtpType) -> ApiResult<bool> {
        let key = self.otp_key(email, otp_type);
        self.redis.exists(&key).await
    }

    /// Check rate limit for sending OTPs
    pub async fn check_send_rate(&self, email: &str, otp_type: OtpType) -> ApiResult<i64> {
        let key = self.send_rate_key(email, otp_type);
        let count = self.redis.incr_with_ttl(&key, RATE_LIMIT_TTL_SECS).await?;

        if count > MAX_OTP_REQUESTS {
            return Err(ApiError::TooManyRequests(
                "Too many OTP requests. Please try again later.".to_string(),
            ));
        }

        Ok(MAX_OTP_REQUESTS - count)
    }
}
