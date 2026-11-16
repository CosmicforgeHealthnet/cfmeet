//! OTP Service
//!
//! One-time codes for passwordless sign-in, step-up confirmation and email change, stored
//! in Redis.
//!
//! Notification Service N0-8 (S-5): codes are never stored in clear. Each stored value is
//! `<salt hex>:<SHA-256(salt || code) hex>` with a fresh random 16-byte salt, compared in
//! constant time, and every key is derived from the SHA-256 of the lower-cased subject
//! (an address, or a user-scoped subject such as `user:<id>:CHANGE_EMAIL`), so Redis holds
//! neither the code nor the address.
//!
//! A sign-in challenge can also carry a magic-link token: the stored value then has a
//! third part, `:<SHA-256(token) hex>`. The code and the link are one challenge: whichever
//! is used first consumes both, and a new challenge replaces both.
//!
//! Brute force: every check (code or link) increments one attempt counter per subject and
//! type. Past [`MAX_OTP_ATTEMPTS`] in the window the pending challenge is deleted and the
//! caller gets 429. Issuing a new code does not reset the counter, so requesting codes
//! never buys more guesses.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};
use crate::services::RedisService;

/// Lifetime of every code (10 minutes).
pub const CODE_TTL_SECS: u64 = 10 * 60;

/// TTL for rate limiting window (15 minutes)
const RATE_LIMIT_TTL_SECS: u64 = 15 * 60;

/// Maximum checks per subject within the window before the challenge is destroyed.
pub const MAX_OTP_ATTEMPTS: i64 = 5;

/// Maximum codes sent per subject within the window.
const MAX_OTP_REQUESTS: i64 = 5;

/// OTP types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpType {
    /// Passwordless sign-in / sign-up (subject: the email address).
    SignIn,
    /// Confirmation of a sensitive action by the signed-in owner (subject:
    /// `user:<id>:<purpose>`).
    StepUp,
    /// Proof of ownership of a new address (subject: `user:<id>:<new address>`).
    EmailChange,
}

impl OtpType {
    fn key_prefix(&self) -> &'static str {
        match self {
            OtpType::SignIn => "otp:signin",
            OtpType::StepUp => "otp:stepup",
            OtpType::EmailChange => "otp:emailchange",
        }
    }
}

/// Outcome of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpCheck {
    /// The code (or link) matched; the challenge is consumed.
    Valid,
    /// No challenge is pending (never issued, expired, used or replaced).
    Missing,
    /// A challenge is pending but the value did not match.
    Wrong,
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

    fn otp_key(&self, subject: &str, otp_type: OtpType) -> String {
        format!("{}:h:{}", otp_type.key_prefix(), email_key(subject))
    }

    fn attempts_key(&self, subject: &str, otp_type: OtpType) -> String {
        format!("{}:attempts:h:{}", otp_type.key_prefix(), email_key(subject))
    }

    fn send_rate_key(&self, subject: &str, otp_type: OtpType) -> String {
        format!("{}:send_rate:h:{}", otp_type.key_prefix(), email_key(subject))
    }

    /// Store a code (salted hash only), replacing any pending challenge of this type.
    pub async fn store_code(&self, subject: &str, code: &str, otp_type: OtpType) -> ApiResult<()> {
        self.store_challenge(subject, code, None, otp_type).await
    }

    /// Store a code and, optionally, a magic-link token as one challenge.
    pub async fn store_challenge(
        &self,
        subject: &str,
        code: &str,
        link_token: Option<&str>,
        otp_type: OtpType,
    ) -> ApiResult<()> {
        let mut value = seal_code(code);
        if let Some(token) = link_token {
            value.push(':');
            value.push_str(&hex::encode(Sha256::digest(token.as_bytes())));
        }
        self.redis
            .set(&self.otp_key(subject, otp_type), &value, Some(CODE_TTL_SECS))
            .await
    }

    /// Count one attempt; past the limit the pending challenge is destroyed (429).
    async fn count_attempt(&self, subject: &str, otp_type: OtpType) -> ApiResult<String> {
        let attempts_key = self.attempts_key(subject, otp_type);
        let attempts = self
            .redis
            .incr_with_ttl(&attempts_key, RATE_LIMIT_TTL_SECS)
            .await?;
        if attempts > MAX_OTP_ATTEMPTS {
            self.redis.del(&self.otp_key(subject, otp_type)).await?;
            return Err(ApiError::TooManyRequests(
                "Too many attempts. Please request a new code later.".to_string(),
            ));
        }
        Ok(attempts_key)
    }

    /// Atomically take the matched challenge: of two concurrent checks of the same code,
    /// only the one that removes exactly the value it matched wins.
    async fn consume(
        &self,
        subject: &str,
        otp_type: OtpType,
        attempts_key: &str,
        matched: &str,
    ) -> ApiResult<OtpCheck> {
        let taken = self.redis.take(&self.otp_key(subject, otp_type)).await?;
        if taken.as_deref() != Some(matched) {
            return Ok(OtpCheck::Missing);
        }
        self.redis.del(attempts_key).await?;
        Ok(OtpCheck::Valid)
    }

    /// Check a code; a match consumes the whole challenge (code and link).
    pub async fn check(&self, subject: &str, code: &str, otp_type: OtpType) -> ApiResult<OtpCheck> {
        let attempts_key = self.count_attempt(subject, otp_type).await?;
        let Some(stored) = self.redis.get(&self.otp_key(subject, otp_type)).await? else {
            // Same hashing work as a stored code (timing).
            let _ = code_matches(&seal_code(code), code);
            return Ok(OtpCheck::Missing);
        };
        if code_matches(&stored, code) {
            self.consume(subject, otp_type, &attempts_key, &stored).await
        } else {
            Ok(OtpCheck::Wrong)
        }
    }

    /// Check a magic-link token; a match consumes the whole challenge (code and link).
    pub async fn check_link(
        &self,
        subject: &str,
        token: &str,
        otp_type: OtpType,
    ) -> ApiResult<OtpCheck> {
        let attempts_key = self.count_attempt(subject, otp_type).await?;
        let Some(stored) = self.redis.get(&self.otp_key(subject, otp_type)).await? else {
            return Ok(OtpCheck::Missing);
        };
        if link_matches(&stored, token) {
            self.consume(subject, otp_type, &attempts_key, &stored).await
        } else {
            Ok(OtpCheck::Wrong)
        }
    }

    /// Verify a code: `true` only for a match (which consumes the challenge).
    pub async fn verify_code(&self, subject: &str, code: &str, otp_type: OtpType) -> ApiResult<bool> {
        Ok(self.check(subject, code, otp_type).await? == OtpCheck::Valid)
    }

    /// Whether a challenge is pending.
    pub async fn has_code(&self, subject: &str, otp_type: OtpType) -> ApiResult<bool> {
        self.redis.exists(&self.otp_key(subject, otp_type)).await
    }

    /// Check rate limit for sending OTPs
    pub async fn check_send_rate(&self, subject: &str, otp_type: OtpType) -> ApiResult<i64> {
        let key = self.send_rate_key(subject, otp_type);
        let count = self.redis.incr_with_ttl(&key, RATE_LIMIT_TTL_SECS).await?;

        if count > MAX_OTP_REQUESTS {
            return Err(ApiError::TooManyRequests(
                "Too many OTP requests. Please try again later.".to_string(),
            ));
        }

        Ok(MAX_OTP_REQUESTS - count)
    }
}

/// A uniformly random 6-digit code (leading zeros included), from the thread CSPRNG.
pub fn generate_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

/// A magic-link token: 32 random bytes, hex encoded.
pub fn generate_link_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// Key component for a subject: SHA-256 of the trimmed, lower-cased value.
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

/// Whether `code` matches a value written by [`seal_code`] (with or without a link part),
/// in constant time over the digest.
pub fn code_matches(stored: &str, code: &str) -> bool {
    let mut parts = stored.split(':');
    let (Some(salt), Some(digest)) = (parts.next(), parts.next()) else {
        return false;
    };
    match (hex::decode(salt), hex::decode(digest)) {
        (Ok(salt), Ok(digest)) if salt.len() == 16 && digest.len() == 32 => {
            constant_time_eq(&salted_digest(&salt, code), &digest)
        }
        _ => false,
    }
}

/// Whether `token` matches the link part of a stored challenge.
pub fn link_matches(stored: &str, token: &str) -> bool {
    let Some(link) = stored.split(':').nth(2) else {
        // Same digest work as a challenge with a link (timing).
        let _ = Sha256::digest(token.as_bytes());
        return false;
    };
    match hex::decode(link) {
        Ok(expected) if expected.len() == 32 => {
            constant_time_eq(&Sha256::digest(token.as_bytes()), &expected)
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
    fn a_challenge_with_a_link_verifies_both_the_code_and_the_link() {
        let token = generate_link_token();
        let stored = format!(
            "{}:{}",
            seal_code("012345"),
            hex::encode(Sha256::digest(token.as_bytes()))
        );
        assert!(code_matches(&stored, "012345"));
        assert!(!code_matches(&stored, "012346"));
        assert!(link_matches(&stored, &token));
        assert!(!link_matches(&stored, &generate_link_token()));
        assert!(!link_matches(&seal_code("012345"), &token), "no link part");
    }

    #[test]
    fn codes_are_six_digits_and_may_start_with_zero() {
        let mut leading_zero = false;
        for _ in 0..5000 {
            let c = generate_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
            leading_zero |= c.starts_with('0');
        }
        assert!(leading_zero, "the full 000000-999999 range is used");
        assert_eq!(generate_link_token().len(), 64);
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
