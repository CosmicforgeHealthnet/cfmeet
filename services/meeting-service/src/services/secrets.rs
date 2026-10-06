//! Secret material for the meeting domain: bearer tokens, hashes, passcodes, codes.
//!
//! Raw bearer secrets (invitation tokens, participant credentials) are generated from
//! the OS CSPRNG, returned to the caller once, and only their SHA-256 is persisted.
//! Passcodes are low-entropy, so they use bcrypt.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, Rng, RngCore};
use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};

/// 256-bit random secret, URL-safe, with a type prefix (`inv_`, `pt_`).
pub fn generate_bearer_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Opaque public identifier (not a secret) — e.g. guest identity `gst_…`.
pub fn generate_opaque_id(prefix: &str) -> String {
    let mut bytes = [0u8; 18];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Hex SHA-256 used to store/look up high-entropy bearer secrets.
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_passcode(passcode: &str, cost: u32) -> ApiResult<String> {
    bcrypt::hash(passcode, cost)
        .map_err(|e| ApiError::InternalError(format!("passcode hash failed: {e}")))
}

/// Constant-time (bcrypt) passcode verification. Malformed hashes verify as `false`.
pub fn verify_passcode(passcode: &str, hash: &str) -> bool {
    bcrypt::verify(passcode, hash).unwrap_or(false)
}

/// v2 meeting code: `xxx-xxxx-xxx`, lowercase, unambiguous alphabet.
///
/// Legacy codes are 8 uppercase characters, so the two formats can never collide and
/// the compatibility resolver is unambiguous.
pub fn generate_meeting_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
    let mut rng = OsRng;
    let mut part = |n: usize| -> String {
        (0..n)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
            .collect()
    };
    format!("{}-{}-{}", part(3), part(4), part(3))
}

pub fn is_v2_meeting_code(code: &str) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    parts.len() == 3
        && [3, 4, 3].iter().zip(&parts).all(|(n, p)| p.len() == *n)
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_lowercase()))
}

pub fn is_legacy_meeting_code(code: &str) -> bool {
    code.len() == 8
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_secrets_are_unique_and_prefixed() {
        let a = generate_bearer_secret("inv_");
        let b = generate_bearer_secret("inv_");
        assert_ne!(a, b);
        assert!(a.starts_with("inv_"));
        assert!(a.len() >= 40);
    }

    #[test]
    fn secret_hash_is_stable_hex() {
        assert_eq!(hash_secret("x"), hash_secret("x"));
        assert_eq!(hash_secret("x").len(), 64);
        assert_ne!(hash_secret("x"), hash_secret("y"));
    }

    #[test]
    fn passcode_hash_never_contains_plaintext() {
        let h = hash_passcode("s3cret-code", 4).unwrap();
        assert!(!h.contains("s3cret-code"));
        assert!(verify_passcode("s3cret-code", &h));
        assert!(!verify_passcode("wrong", &h));
        assert!(!verify_passcode("s3cret-code", "not-a-hash"));
    }

    #[test]
    fn v2_and_legacy_code_formats_are_disjoint() {
        for _ in 0..200 {
            let c = generate_meeting_code();
            assert!(is_v2_meeting_code(&c), "{c}");
            assert!(!is_legacy_meeting_code(&c));
        }
        let legacy = crate::utils::generate_meeting_identifier();
        assert!(is_legacy_meeting_code(&legacy));
        assert!(!is_v2_meeting_code(&legacy));
    }
}
