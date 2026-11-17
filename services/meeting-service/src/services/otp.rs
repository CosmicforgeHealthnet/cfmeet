//! OTP Service
//!
//! Handles OTP storage and verification using Redis.
//!
//! Notification Service N0-8 (S-5): codes are never stored in clear. Each stored value is
//! `<salt hex>:<SHA-256(salt || code) hex>` with a fresh random 16-byte salt, compared in
//! constant time, and every key is derived from the SHA-256 of the lower-cased address,
//! so Redis holds neither the code nor the address. Account remains the only verifier.
//!
//! Migration: codes issued by the previous image (plaintext value under
//! `<prefix>:<email>`) stay valid until they expire (at most 24 h): `verify_code` and
//! `has_code` fall back to that legacy key once, delete it on success, and never write it.
//! After a rollback to the previous image, codes issued by this one are unreadable there;
//! the user requests a new code (the send path is unchanged).

use std::sync::Arc;

use sha2::{Digest, Sha256};

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
        format!("{}:h:{}", otp_type.key_prefix(), email_key(email))
    }

    /// The previous image's key (plaintext code under the address); read-only fallback.
    fn legacy_otp_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:{}", otp_type.key_prefix(), email.to_lowercase())
    }

    fn attempts_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:attempts:h:{}", otp_type.key_prefix(), email_key(email))
    }

    fn send_rate_key(&self, email: &str, otp_type: OtpType) -> String {
        format!("{}:send_rate:h:{}", otp_type.key_prefix(), email_key(email))
    }

    /// Store an OTP code (salted hash only). A new code replaces any earlier one, including
    /// a code issued by the previous image.
    pub async fn store_code(&self, email: &str, code: &str, otp_type: OtpType) -> ApiResult<()> {
        let key = self.otp_key(email, otp_type);
        self.redis
            .set(&key, &seal_code(code), Some(otp_type.ttl_secs()))
            .await?;
        self.redis
            .del(&self.legacy_otp_key(email, otp_type))
            .await?;
        Ok(())
    }

    /// Verify an OTP code
    pub async fn verify_code(&self, email: &str, code: &str, otp_type: OtpType) -> ApiResult<bool> {
        // Check rate limiting
        let attempts_key = self.attempts_key(email, otp_type);
        let attempts = self
            .redis
            .incr_with_ttl(&attempts_key, RATE_LIMIT_TTL_SECS)
            .await?;

        if attempts > MAX_OTP_ATTEMPTS {
            // Gap audit A8: the code is burned, not only paused: a new one must be
            // requested (sent to the real owner), so one code never takes more guesses.
            self.burn(email, otp_type).await?;
            return Err(ApiError::TooManyRequests(
                "Too many verification attempts. Please try again later.".to_string(),
            ));
        }

        // Get the stored (salted, hashed) code; fall back to a code issued by the
        // previous image (plaintext, until it expires).
        let key = self.otp_key(email, otp_type);
        let legacy_key = self.legacy_otp_key(email, otp_type);
        let valid_key = match self.redis.get(&key).await? {
            Some(stored) => code_matches(&stored, code).then_some(key),
            None => match self.redis.get(&legacy_key).await? {
                Some(stored) => {
                    constant_time_eq(stored.as_bytes(), code.as_bytes()).then_some(legacy_key)
                }
                None => None,
            },
        };
        match valid_key {
            Some(key) => {
                // Code valid - delete it
                self.redis.del(&key).await?;
                self.redis.del(&attempts_key).await?;
                Ok(true)
            }
            None => {
                if attempts >= MAX_OTP_ATTEMPTS {
                    self.burn(email, otp_type).await?;
                }
                Ok(false)
            }
        }
    }

    /// Gap audit A8: invalidates the current code (and any code of the previous image).
    async fn burn(&self, email: &str, otp_type: OtpType) -> ApiResult<()> {
        self.redis.del(&self.otp_key(email, otp_type)).await?;
        self.redis
            .del(&self.legacy_otp_key(email, otp_type))
            .await?;
        Ok(())
    }

    /// N0-9: checks a code WITHOUT consuming it, under the same attempt counter as
    /// [`Self::verify_code`] (so probing shares the brute-force budget). Performs the same
    /// Redis work whether or not an account or a code exists.
    pub async fn check_code(&self, email: &str, code: &str, otp_type: OtpType) -> ApiResult<bool> {
        let attempts_key = self.attempts_key(email, otp_type);
        let attempts = self
            .redis
            .incr_with_ttl(&attempts_key, RATE_LIMIT_TTL_SECS)
            .await?;
        if attempts > MAX_OTP_ATTEMPTS {
            self.burn(email, otp_type).await?;
            return Err(ApiError::TooManyRequests(
                "Too many verification attempts. Please try again later.".to_string(),
            ));
        }
        let hashed = self.redis.get(&self.otp_key(email, otp_type)).await?;
        let legacy = self
            .redis
            .get(&self.legacy_otp_key(email, otp_type))
            .await?;
        let ok = match (hashed, legacy) {
            (Some(stored), _) => code_matches(&stored, code),
            (None, Some(stored)) => constant_time_eq(stored.as_bytes(), code.as_bytes()),
            (None, None) => {
                // Same hashing work as a stored code (timing).
                let _ = code_matches(&seal_code(code), code);
                false
            }
        };
        if !ok && attempts >= MAX_OTP_ATTEMPTS {
            self.burn(email, otp_type).await?;
        }
        Ok(ok)
    }

    /// Check if an OTP exists
    pub async fn has_code(&self, email: &str, otp_type: OtpType) -> ApiResult<bool> {
        Ok(self.redis.exists(&self.otp_key(email, otp_type)).await?
            || self
                .redis
                .exists(&self.legacy_otp_key(email, otp_type))
                .await?)
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

/// Key component for an address: SHA-256 of the trimmed, lower-cased address.
pub fn email_key(email: &str) -> String {
    hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes()))
}

fn salted_digest(salt: &[u8], code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(code.as_bytes());
    h.finalize().into()
}

/// `<salt hex>:<SHA-256(salt || code) hex>` with a fresh random 16-byte salt.
pub fn seal_code(code: &str) -> String {
    let salt: [u8; 16] = rand::random();
    format!(
        "{}:{}",
        hex::encode(salt),
        hex::encode(salted_digest(&salt, code))
    )
}

/// Whether `code` matches a value written by [`seal_code`] (constant time over the digest).
pub fn code_matches(stored: &str, code: &str) -> bool {
    let Some((salt, digest)) = stored.split_once(':') else {
        return false;
    };
    match (hex::decode(salt), hex::decode(digest)) {
        (Ok(salt), Ok(digest)) if salt.len() == 16 && digest.len() == 32 => {
            constant_time_eq(&salted_digest(&salt, code), &digest)
        }
        _ => false,
    }
}

/// Constant-time equality (the length is not secret).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_codes_are_salted_hashes_that_verify_only_the_right_code() {
        let a = seal_code("482913");
        let b = seal_code("482913");
        assert_ne!(a, b, "a fresh salt per code");
        assert!(!a.contains("482913"));
        assert!(code_matches(&a, "482913"));
        assert!(!code_matches(&a, "482914"));
        assert!(!code_matches(&a, ""));
        for bad in ["482913", "zz:zz", "00:00", ""] {
            assert!(!code_matches(bad, "482913"), "{bad}");
        }
    }

    /// Shared with infra/calling/production/test_controlled_otp.py: acceptance tooling
    /// writes controlled codes in exactly this format.
    #[test]
    fn the_stored_format_matches_the_acceptance_tooling_vector() {
        let salt: Vec<u8> = (0u8..16).collect();
        assert_eq!(
            format!("{}:{}", hex::encode(&salt), hex::encode(salted_digest(&salt, "123456"))),
            "000102030405060708090a0b0c0d0e0f:99c90773ea9ce66a355bd6a2a6e55122c3caf5a164785c90b1d58c2d63bb8680"
        );
        assert!(code_matches(
            "000102030405060708090a0b0c0d0e0f:99c90773ea9ce66a355bd6a2a6e55122c3caf5a164785c90b1d58c2d63bb8680",
            "123456"
        ));
        assert_eq!(
            email_key(" Ada@Example.com "),
            "b5fc85e55755f9e0d030a10ab4429b6b2944855f9a0d60077fe832becbc41d72"
        );
    }

    #[test]
    fn keys_never_contain_the_address() {
        let k = email_key(" Ada@Example.com ");
        assert_eq!(k, email_key("ada@example.com"));
        assert_eq!(k.len(), 64);
        assert!(!k.contains('@'));
    }

    #[test]
    fn constant_time_eq_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
