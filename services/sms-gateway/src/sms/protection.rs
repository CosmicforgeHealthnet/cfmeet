//! Protection of message content and telephone numbers at rest.
//!
//! * Sealing: AES-256-GCM (`cf-webhooks` `SecretBox`, `v2.<kid>.<…>`), the associated data
//!   binds every seal to its purpose and record (`sms-content:<transport>`,
//!   `sms-remote:<transport>`, `sms-sender:<transport>`, `sms-quarantine:<id>`), so a
//!   sealed value moved to another row or column never opens. Key rotation: the previous
//!   key still opens, new seals use the current one.
//! * Fingerprints: HMAC-SHA256 under a SEPARATE key over the canonical E.164 (or the
//!   normalized content), hex — equality without the value (opt-out, threads, rate and
//!   abuse windows). A fingerprint cannot be reversed without the key.
//! * Masking: `+***` and the last four digits, the only form a number takes in responses,
//!   events and logs.

use std::fmt;

use cf_webhooks::secret_box::{SealingKey, SecretBox};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::config::{read_key_file, ConfigError, ProtectionFiles};

#[derive(Clone)]
pub struct Protection {
    boxed: std::sync::Arc<SecretBox>,
    fingerprint_key: std::sync::Arc<zeroize::Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Protection")
            .field("current_kid", &self.boxed.current_kid())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("sealed value cannot be opened")]
pub struct OpenError;

/// What a sealed value is (part of the associated data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Content,
    Remote,
    Sender,
    Quarantine,
}

impl Purpose {
    fn label(&self) -> &'static str {
        match self {
            Purpose::Content => "sms-content",
            Purpose::Remote => "sms-remote",
            Purpose::Sender => "sms-sender",
            Purpose::Quarantine => "sms-quarantine",
        }
    }
}

impl Protection {
    /// From raw keys (tests, and [`Protection::load`]). The content key is 32 bytes; the
    /// fingerprint key at least 32 bytes and different from the content key.
    pub fn new(
        kid: &str,
        content_key: &[u8],
        previous: Option<(&str, &[u8])>,
        fingerprint_key: &[u8],
    ) -> Result<Self, ConfigError> {
        let current = SealingKey::from_bytes(kid, content_key)
            .map_err(|e| ConfigError::invalid("SMS_GATEWAY_CONTENT_KEY_FILE", e.to_string()))?;
        let previous = previous
            .map(|(kid, key)| {
                SealingKey::from_bytes(kid, key).map_err(|e| {
                    ConfigError::invalid("SMS_GATEWAY_PREVIOUS_CONTENT_KEY_FILE", e.to_string())
                })
            })
            .transpose()?;
        let boxed = SecretBox::new(current, previous)
            .map_err(|e| ConfigError::invalid("SMS_GATEWAY_CONTENT_KEY_FILE", e.to_string()))?;
        if fingerprint_key.len() < 32 {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                "must hold at least 32 bytes",
            ));
        }
        if fingerprint_key == content_key {
            return Err(ConfigError::invalid(
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                "must differ from the content key",
            ));
        }
        Ok(Self {
            boxed: std::sync::Arc::new(boxed),
            fingerprint_key: std::sync::Arc::new(zeroize::Zeroizing::new(fingerprint_key.to_vec())),
        })
    }

    /// Loads the key files named by the configuration (fail closed).
    pub fn load(files: &ProtectionFiles) -> Result<Self, ConfigError> {
        let content = zeroize::Zeroizing::new(read_key_file(
            "SMS_GATEWAY_CONTENT_KEY_FILE",
            &files.content_key_file,
        )?);
        let previous = files
            .previous
            .as_ref()
            .map(|(kid, path)| {
                read_key_file("SMS_GATEWAY_PREVIOUS_CONTENT_KEY_FILE", path)
                    .map(|k| (kid.clone(), zeroize::Zeroizing::new(k)))
            })
            .transpose()?;
        let fingerprint = zeroize::Zeroizing::new(read_key_file(
            "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
            &files.fingerprint_key_file,
        )?);
        Self::new(
            &files.content_key_id,
            &content,
            previous.as_ref().map(|(k, v)| (k.as_str(), v.as_slice())),
            &fingerprint,
        )
    }

    fn owner(purpose: Purpose, record: Uuid) -> String {
        format!("{}:{}", purpose.label(), record.simple())
    }

    pub fn seal(&self, purpose: Purpose, record: Uuid, plaintext: &str) -> String {
        self.boxed.seal(plaintext, &Self::owner(purpose, record))
    }

    pub fn open(&self, purpose: Purpose, record: Uuid, sealed: &str) -> Result<String, OpenError> {
        self.boxed
            .open(sealed, &Self::owner(purpose, record))
            .map_err(|_| OpenError)
    }

    fn mac(&self, domain: &str, value: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.fingerprint_key)
            .expect("HMAC accepts any key length");
        mac.update(domain.as_bytes());
        mac.update(&[0]);
        mac.update(value.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// The keyed fingerprint of a canonical E.164 number.
    pub fn number_fingerprint(&self, e164: &str) -> String {
        self.mac("number", e164)
    }

    /// The keyed fingerprint of message content (whitespace-normalized, case-folded),
    /// for the identical-content abuse control.
    pub fn content_fingerprint(&self, text: &str) -> String {
        let normalized: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        self.mac("content", &normalized)
    }
}

/// `+***` and the last four digits of a canonical E.164 number.
pub fn mask(e164: &str) -> String {
    let digits: Vec<char> = e164.chars().filter(char::is_ascii_digit).collect();
    let tail: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("+***{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> Vec<u8> {
        (0..32u8)
            .map(|i| i.wrapping_mul(seed).wrapping_add(seed))
            .collect()
    }

    fn protection() -> Protection {
        Protection::new("k1", &key(7), None, &key(9)).unwrap()
    }

    #[test]
    fn seals_are_bound_to_purpose_and_record() {
        let p = protection();
        let id = Uuid::new_v4();
        let sealed = p.seal(Purpose::Content, id, "meet at 10");
        assert!(!sealed.contains("meet"));
        assert_eq!(p.open(Purpose::Content, id, &sealed).unwrap(), "meet at 10");
        assert!(
            p.open(Purpose::Remote, id, &sealed).is_err(),
            "other purpose"
        );
        assert!(
            p.open(Purpose::Content, Uuid::new_v4(), &sealed).is_err(),
            "other record"
        );
    }

    #[test]
    fn rotation_keeps_old_seals_readable() {
        let old = Protection::new("k1", &key(7), None, &key(9)).unwrap();
        let id = Uuid::new_v4();
        let sealed = old.seal(Purpose::Remote, id, "+15550100123");
        let rotated = Protection::new("k2", &key(5), Some(("k1", &key(7))), &key(9)).unwrap();
        assert_eq!(
            rotated.open(Purpose::Remote, id, &sealed).unwrap(),
            "+15550100123"
        );
        assert!(rotated.seal(Purpose::Remote, id, "x").starts_with("v2.k2."));
    }

    #[test]
    fn fingerprints_are_keyed_and_masks_keep_four_digits() {
        let p = protection();
        let other = Protection::new("k1", &key(7), None, &key(3)).unwrap();
        assert_eq!(
            p.number_fingerprint("+15550100123"),
            p.number_fingerprint("+15550100123")
        );
        assert_ne!(
            p.number_fingerprint("+15550100123"),
            other.number_fingerprint("+15550100123")
        );
        assert_eq!(
            p.content_fingerprint("Hello  World"),
            p.content_fingerprint("hello world")
        );
        assert_eq!(mask("+15550100123"), "+***0123");
        assert!(
            Protection::new("k1", &key(7), None, &key(7)).is_err(),
            "same key"
        );
        assert!(
            Protection::new("k1", &key(7), None, &key(9)[..16]).is_err(),
            "short key"
        );
        assert!(
            Protection::new("k1", &[7u8; 32], None, &key(9)).is_err(),
            "weak key"
        );
    }
}
