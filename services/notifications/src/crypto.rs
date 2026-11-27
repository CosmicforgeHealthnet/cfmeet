//! Data protection at rest (Document 4 §8): one master key file derives
//! * the AES-256-GCM sealing key (`cf-webhooks::secret_box`, seals carry their key id) for
//!   addresses, subjects, bodies and push handles, bound to the owning row;
//! * the HMAC-SHA256 key of recipient digests (keyed, unlike the Meeting ledger's unsalted
//!   SHA-256), used for suppression, shadow observations and audit.
//!
//! Rotation: the previous master key opens existing seals; new seals use the current key.

use cf_webhooks::secret_box::{weak_key, SealingKey, SecretBox, SecretBoxError};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn derive(master: &[u8], label: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(master).expect("hmac accepts any key length");
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().into()
}

pub struct DataKeys {
    sealer: SecretBox,
    digest_key: [u8; 32],
}

impl std::fmt::Debug for DataKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DataKeys({}, [REDACTED])", self.sealer.current_kid())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("the master key is weak")]
    Weak,
    #[error("the sealing key is invalid: {0}")]
    Seal(#[from] SecretBoxError),
}

impl DataKeys {
    pub fn new(current: (&str, &[u8]), previous: Option<(&str, &[u8])>) -> Result<Self, KeyError> {
        if weak_key(current.1) || previous.is_some_and(|p| weak_key(p.1)) {
            return Err(KeyError::Weak);
        }
        let seal = SealingKey::from_bytes(
            current.0,
            &derive(current.1, "cfmeet.notifications.seal.v1"),
        )?;
        let prev = match previous {
            None => None,
            Some((kid, raw)) => Some(SealingKey::from_bytes(
                kid,
                &derive(raw, "cfmeet.notifications.seal.v1"),
            )?),
        };
        Ok(Self {
            sealer: SecretBox::new(seal, prev)?,
            digest_key: derive(current.1, "cfmeet.notifications.digest.v1"),
        })
    }

    pub fn kid(&self) -> &str {
        self.sealer.current_kid()
    }

    /// Seals `plaintext` for `owner` (e.g. `email:<message id>`); a seal moved to another
    /// row does not open.
    pub fn seal(&self, plaintext: &str, owner: &str) -> String {
        self.sealer.seal(plaintext, owner)
    }

    pub fn open(&self, sealed: &str, owner: &str) -> Result<String, SecretBoxError> {
        self.sealer.open(sealed, owner)
    }

    /// Keyed digest (64 lowercase hex) of a normalized value in a domain (`email`,
    /// `recipient`, `handle`).
    pub fn digest(&self, domain: &str, value: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.digest_key).expect("32-byte key");
        mac.update(domain.as_bytes());
        mac.update(&[0u8]);
        mac.update(value.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// The digest of an email address (trimmed, lowercase).
    pub fn email_digest(&self, address: &str) -> String {
        self.digest("email", &address.trim().to_ascii_lowercase())
    }
}

#[cfg(test)]
pub fn test_keys() -> DataKeys {
    let raw: Vec<u8> = (0u8..32)
        .map(|i| i.wrapping_mul(37).wrapping_add(11))
        .collect();
    DataKeys::new(("test-1", &raw), None).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_are_bound_to_their_owner_and_digests_are_keyed() {
        let k = test_keys();
        let s = k.seal("ada@example.com", "email:1");
        assert!(s.starts_with("v2.test-1."));
        assert!(!s.contains("ada"));
        assert_eq!(k.open(&s, "email:1").unwrap(), "ada@example.com");
        assert!(k.open(&s, "email:2").is_err());
        assert_eq!(
            k.email_digest("Ada@Example.com "),
            k.email_digest("ada@example.com")
        );
        let plain = hex::encode(<Sha256 as sha2::Digest>::digest(b"ada@example.com"));
        assert_ne!(k.email_digest("ada@example.com"), plain);
    }

    #[test]
    fn weak_master_keys_are_refused_and_rotation_opens_old_seals() {
        assert!(DataKeys::new(("a", &[7u8; 32]), None).is_err());
        let old_raw: Vec<u8> = (0u8..32)
            .map(|i| i.wrapping_mul(13).wrapping_add(5))
            .collect();
        let old = DataKeys::new(("old-1", &old_raw), None).unwrap();
        let sealed = old.seal("x", "o");
        let new_raw: Vec<u8> = (0u8..32)
            .map(|i| i.wrapping_mul(29).wrapping_add(3))
            .collect();
        let rotated = DataKeys::new(("new-1", &new_raw), Some(("old-1", &old_raw))).unwrap();
        assert_eq!(rotated.open(&sealed, "o").unwrap(), "x");
        assert!(rotated.seal("y", "o").starts_with("v2.new-1."));
    }
}
