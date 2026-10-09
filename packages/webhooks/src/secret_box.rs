//! Signing secrets at rest: AES-256-GCM (`ring`) under a deployment key injected from the
//! secret manager. The stored form is `v1.<base64url(nonce || ciphertext || tag)>`; the
//! associated data binds the ciphertext to its owner (e.g. the subscription id), so a
//! sealed secret copied to another row does not open.

use base64::Engine;
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN},
    rand::{SecureRandom, SystemRandom},
};

const PREFIX: &str = "v1.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretBoxError {
    #[error("the secret box key must be 32 bytes (base64)")]
    Key,
    #[error("the sealed secret could not be opened")]
    Open,
}

/// The deployment key. No `Debug` output of the key material.
pub struct SecretBox {
    key: LessSafeKey,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox([REDACTED])")
    }
}

impl SecretBox {
    /// From a base64 (standard or URL-safe) encoded 32-byte key.
    pub fn from_base64(encoded: &str) -> Result<Self, SecretBoxError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded.trim()))
            .map_err(|_| SecretBoxError::Key)?;
        Self::from_bytes(&raw)
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self, SecretBoxError> {
        if raw.len() != 32 {
            return Err(SecretBoxError::Key);
        }
        let key = UnboundKey::new(&AES_256_GCM, raw).map_err(|_| SecretBoxError::Key)?;
        Ok(Self {
            key: LessSafeKey::new(key),
        })
    }

    pub fn seal(&self, plaintext: &str, owner: &str) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .expect("system randomness");
        let mut in_out = plaintext.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(owner.as_bytes()),
                &mut in_out,
            )
            .expect("AES-GCM sealing");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&in_out);
        format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
        )
    }

    pub fn open(&self, sealed: &str, owner: &str) -> Result<String, SecretBoxError> {
        let raw = sealed
            .strip_prefix(PREFIX)
            .and_then(|b| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(b)
                    .ok()
            })
            .ok_or(SecretBoxError::Open)?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(SecretBoxError::Open);
        }
        let (nonce, rest) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SecretBoxError::Open)?;
        let mut in_out = rest.to_vec();
        let plain = self
            .key
            .open_in_place(nonce, Aad::from(owner.as_bytes()), &mut in_out)
            .map_err(|_| SecretBoxError::Open)?;
        String::from_utf8(plain.to_vec()).map_err(|_| SecretBoxError::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_secrets_open_only_with_the_key_and_owner() {
        let b = SecretBox::from_bytes(&[7u8; 32]).unwrap();
        let sealed = b.seal("whsec_abc", "sub-1");
        assert!(sealed.starts_with("v1.") && !sealed.contains("whsec_abc"));
        assert_eq!(b.open(&sealed, "sub-1").unwrap(), "whsec_abc");
        assert_eq!(b.open(&sealed, "sub-2"), Err(SecretBoxError::Open));
        let other = SecretBox::from_bytes(&[8u8; 32]).unwrap();
        assert_eq!(other.open(&sealed, "sub-1"), Err(SecretBoxError::Open));
        assert_ne!(b.seal("whsec_abc", "sub-1"), sealed, "random nonce");
        assert!(SecretBox::from_bytes(&[1u8; 16]).is_err());
        assert!(format!("{b:?}").contains("REDACTED"));
    }
}
