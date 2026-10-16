//! Signing secrets at rest: AES-256-GCM (`ring`) under deployment keys injected from the
//! secret manager, with key identifiers so the deployment key can be rotated.
//!
//! * Stored form `v2.<kid>.<base64url(nonce || ciphertext || tag)>`; the associated data
//!   binds the ciphertext to its key id AND its owner (e.g. the subscription id), so a
//!   sealed secret copied to another row — or relabelled with another kid — does not open.
//! * Legacy form `v1.<base64url(…)>` (before key ids existed; AAD = owner) is still READ,
//!   with the key whose kid is declared legacy (default [`DEFAULT_KEY_ID`]).
//! * Sealing always uses the CURRENT key; opening selects the key by kid (current or
//!   previous). An unknown kid fails closed ([`SecretBoxError::UnknownKey`]).
//! * Weak keys are refused: all-equal bytes or fewer than [`MIN_DISTINCT_KEY_BYTES`]
//!   distinct byte values (a generated 32-byte key has ~28).

use base64::Engine;
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN},
    rand::{SecureRandom, SystemRandom},
};

const LEGACY_PREFIX: &str = "v1.";
const PREFIX: &str = "v2.";

/// The key id of a key configured without an explicit id, and of legacy `v1.` seals.
pub const DEFAULT_KEY_ID: &str = "k0";

/// Fewest distinct byte values a 32-byte key may have.
pub const MIN_DISTINCT_KEY_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretBoxError {
    #[error("the secret box key must be 32 bytes (base64)")]
    Key,
    #[error("the secret box key is weak (repeated or low-entropy bytes)")]
    WeakKey,
    #[error("the secret box key id must be 1-32 characters of [A-Za-z0-9_-]")]
    KeyId,
    #[error("the previous secret box key must differ from the current one (id and bytes)")]
    DuplicateKey,
    #[error("the sealed secret names a key that is not configured")]
    UnknownKey,
    #[error("the sealed secret could not be opened")]
    Open,
}

/// Whether `kid` is a valid key id (`[A-Za-z0-9][A-Za-z0-9_-]{0,31}`).
pub fn valid_key_id(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= 32
        && kid.as_bytes()[0].is_ascii_alphanumeric()
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether key bytes are weak: all equal, or fewer than [`MIN_DISTINCT_KEY_BYTES`]
/// distinct values.
pub fn weak_key(raw: &[u8]) -> bool {
    let mut seen = [false; 256];
    for b in raw {
        seen[*b as usize] = true;
    }
    seen.iter().filter(|s| **s).count() < MIN_DISTINCT_KEY_BYTES
}

/// Which key sealed a stored secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealedKey {
    /// A legacy `v1.` seal (attributed to the declared legacy kid).
    Legacy,
    /// A `v2.<kid>.` seal.
    Id(String),
}

/// The key reference of a stored seal, or `None` when it is not a seal at all.
pub fn sealed_key(sealed: &str) -> Option<SealedKey> {
    if sealed.starts_with(LEGACY_PREFIX) {
        return Some(SealedKey::Legacy);
    }
    let rest = sealed.strip_prefix(PREFIX)?;
    let (kid, _) = rest.split_once('.')?;
    valid_key_id(kid).then(|| SealedKey::Id(kid.to_string()))
}

/// One deployment key with its id. No `Debug` output of the key material.
pub struct SealingKey {
    kid: String,
    key: LessSafeKey,
    /// Only to detect a "previous == current" misconfiguration; never exposed.
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SealingKey({}, [REDACTED])", self.kid)
    }
}

impl SealingKey {
    /// From a base64 (standard or URL-safe) encoded 32-byte key.
    pub fn from_base64(kid: &str, encoded: &str) -> Result<Self, SecretBoxError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded.trim()))
            .map_err(|_| SecretBoxError::Key)?;
        Self::from_bytes(kid, &raw)
    }

    pub fn from_bytes(kid: &str, raw: &[u8]) -> Result<Self, SecretBoxError> {
        if !valid_key_id(kid) {
            return Err(SecretBoxError::KeyId);
        }
        if raw.len() != 32 {
            return Err(SecretBoxError::Key);
        }
        if weak_key(raw) {
            return Err(SecretBoxError::WeakKey);
        }
        let key = UnboundKey::new(&AES_256_GCM, raw).map_err(|_| SecretBoxError::Key)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, raw);
        let mut fingerprint = [0u8; 32];
        fingerprint.copy_from_slice(digest.as_ref());
        Ok(Self {
            kid: kid.to_string(),
            key: LessSafeKey::new(key),
            fingerprint,
        })
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }
}

/// The deployment keyring: the current key (seals and opens) and optionally the previous
/// key (opens only) during a rotation. No `Debug` output of key material.
pub struct SecretBox {
    current: SealingKey,
    previous: Option<SealingKey>,
    legacy_kid: String,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox([REDACTED])")
    }
}

impl SecretBox {
    /// A single current key with the default id (from a base64-encoded 32-byte key).
    pub fn from_base64(encoded: &str) -> Result<Self, SecretBoxError> {
        Self::new(SealingKey::from_base64(DEFAULT_KEY_ID, encoded)?, None)
    }

    /// A single current key with the default id.
    pub fn from_bytes(raw: &[u8]) -> Result<Self, SecretBoxError> {
        Self::new(SealingKey::from_bytes(DEFAULT_KEY_ID, raw)?, None)
    }

    /// The current key and, during a rotation, the previous one (distinct id AND bytes).
    /// Legacy `v1.` seals are attributed to [`DEFAULT_KEY_ID`].
    pub fn new(current: SealingKey, previous: Option<SealingKey>) -> Result<Self, SecretBoxError> {
        if let Some(p) = &previous {
            if p.kid == current.kid || p.fingerprint == current.fingerprint {
                return Err(SecretBoxError::DuplicateKey);
            }
        }
        Ok(Self {
            current,
            previous,
            legacy_kid: DEFAULT_KEY_ID.to_string(),
        })
    }

    /// Declares which key id legacy `v1.` seals were made with.
    pub fn with_legacy_key_id(mut self, kid: &str) -> Result<Self, SecretBoxError> {
        if !valid_key_id(kid) {
            return Err(SecretBoxError::KeyId);
        }
        self.legacy_kid = kid.to_string();
        Ok(self)
    }

    pub fn current_kid(&self) -> &str {
        &self.current.kid
    }

    pub fn previous_kid(&self) -> Option<&str> {
        self.previous.as_ref().map(|p| p.kid.as_str())
    }

    pub fn legacy_kid(&self) -> &str {
        &self.legacy_kid
    }

    /// Whether a key with `kid` is configured (current or previous).
    pub fn knows(&self, kid: &str) -> bool {
        self.key(kid).is_some()
    }

    /// The key id `sealed` resolves to (legacy → the declared legacy kid).
    pub fn resolved_kid(&self, sealed: &SealedKey) -> String {
        match sealed {
            SealedKey::Legacy => self.legacy_kid.clone(),
            SealedKey::Id(kid) => kid.clone(),
        }
    }

    fn key(&self, kid: &str) -> Option<&SealingKey> {
        if self.current.kid == kid {
            Some(&self.current)
        } else {
            self.previous.as_ref().filter(|p| p.kid == kid)
        }
    }

    /// Seals under the CURRENT key: `v2.<kid>.<…>`.
    pub fn seal(&self, plaintext: &str, owner: &str) -> String {
        let key = &self.current;
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .expect("system randomness");
        let mut in_out = plaintext.as_bytes().to_vec();
        key.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(&key.kid, owner).as_bytes()),
                &mut in_out,
            )
            .expect("AES-GCM sealing");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&in_out);
        format!(
            "{PREFIX}{}.{}",
            key.kid,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
        )
    }

    /// Opens a `v2.` seal with the key its kid names, or a legacy `v1.` seal with the
    /// declared legacy key. A kid that is not configured is [`SecretBoxError::UnknownKey`].
    pub fn open(&self, sealed: &str, owner: &str) -> Result<String, SecretBoxError> {
        let (key, body, aad) = if let Some(body) = sealed.strip_prefix(LEGACY_PREFIX) {
            let key = self
                .key(&self.legacy_kid)
                .ok_or(SecretBoxError::UnknownKey)?;
            (key, body, owner.to_string())
        } else {
            let rest = sealed.strip_prefix(PREFIX).ok_or(SecretBoxError::Open)?;
            let (kid, body) = rest.split_once('.').ok_or(SecretBoxError::Open)?;
            if !valid_key_id(kid) {
                return Err(SecretBoxError::Open);
            }
            let key = self.key(kid).ok_or(SecretBoxError::UnknownKey)?;
            (key, body, aad(kid, owner))
        };
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| SecretBoxError::Open)?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(SecretBoxError::Open);
        }
        let (nonce, rest) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SecretBoxError::Open)?;
        let mut in_out = rest.to_vec();
        let plain = key
            .key
            .open_in_place(nonce, Aad::from(aad.as_bytes()), &mut in_out)
            .map_err(|_| SecretBoxError::Open)?;
        String::from_utf8(plain.to_vec()).map_err(|_| SecretBoxError::Open)
    }
}

fn aad(kid: &str, owner: &str) -> String {
    format!("cfmeet-webhook-secret|{kid}|{owner}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic, non-weak test keys (32 distinct bytes each).
    fn key_bytes(seed: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = seed.wrapping_add((i as u8).wrapping_mul(7));
        }
        k
    }

    fn key(kid: &str, seed: u8) -> SealingKey {
        SealingKey::from_bytes(kid, &key_bytes(seed)).unwrap()
    }

    /// A legacy `v1.` seal exactly as the previous implementation produced it.
    fn legacy_seal(raw: &[u8; 32], plaintext: &str, owner: &str) -> String {
        let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, raw).unwrap());
        let nonce = [9u8; NONCE_LEN];
        let mut in_out = plaintext.as_bytes().to_vec();
        k.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(owner.as_bytes()),
            &mut in_out,
        )
        .unwrap();
        let mut out = nonce.to_vec();
        out.extend_from_slice(&in_out);
        format!(
            "v1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
        )
    }

    #[test]
    fn sealed_secrets_open_only_with_the_key_and_owner() {
        let b = SecretBox::from_bytes(&key_bytes(7)).unwrap();
        let sealed = b.seal("whsec_abc", "sub-1");
        assert!(sealed.starts_with("v2.k0.") && !sealed.contains("whsec_abc"));
        assert_eq!(b.open(&sealed, "sub-1").unwrap(), "whsec_abc");
        assert_eq!(b.open(&sealed, "sub-2"), Err(SecretBoxError::Open));
        let other = SecretBox::from_bytes(&key_bytes(8)).unwrap();
        assert_eq!(other.open(&sealed, "sub-1"), Err(SecretBoxError::Open));
        assert_ne!(b.seal("whsec_abc", "sub-1"), sealed, "random nonce");
        assert!(SecretBox::from_bytes(&[1u8; 16]).is_err());
        assert!(format!("{b:?}").contains("REDACTED"));
        assert!(format!("{:?}", key("k1", 3)).contains("REDACTED"));
    }

    #[test]
    fn seal_uses_the_current_key_and_open_selects_by_kid() {
        let a = SecretBox::new(key("a", 1), None).unwrap();
        let under_a = a.seal("whsec_one", "sub");
        assert_eq!(sealed_key(&under_a), Some(SealedKey::Id("a".into())));
        // Rotation: B current, A previous. Old seals open; new seals use B.
        let rotated = SecretBox::new(key("b", 2), Some(key("a", 1))).unwrap();
        assert_eq!(rotated.open(&under_a, "sub").unwrap(), "whsec_one");
        let under_b = rotated.seal("whsec_two", "sub");
        assert!(under_b.starts_with("v2.b."));
        assert_eq!(rotated.open(&under_b, "sub").unwrap(), "whsec_two");
        assert_eq!(
            (rotated.current_kid(), rotated.previous_kid()),
            ("b", Some("a"))
        );
        // A dropped: B seals still open, A seals fail closed as UNKNOWN (not garbage).
        let only_b = SecretBox::new(key("b", 2), None).unwrap();
        assert_eq!(only_b.open(&under_b, "sub").unwrap(), "whsec_two");
        assert_eq!(
            only_b.open(&under_a, "sub"),
            Err(SecretBoxError::UnknownKey)
        );
        assert!(!only_b.knows("a") && only_b.knows("b"));
        // A seal relabelled with another configured kid does not open (kid is in the AAD).
        let relabelled = under_a.replacen("v2.a.", "v2.b.", 1);
        assert_eq!(rotated.open(&relabelled, "sub"), Err(SecretBoxError::Open));
    }

    #[test]
    fn unknown_or_malformed_kids_fail_closed() {
        let b = SecretBox::new(key("cur", 4), Some(key("prev", 5))).unwrap();
        let foreign = SecretBox::new(key("zzz", 6), None).unwrap().seal("x", "o");
        assert_eq!(b.open(&foreign, "o"), Err(SecretBoxError::UnknownKey));
        for garbage in [
            "",
            "v2.",
            "v2.cur",
            "v2..abc",
            "v3.cur.abc",
            "v2.c!r.abc",
            "plain",
        ] {
            assert!(b.open(garbage, "o").is_err(), "{garbage}");
        }
        assert_eq!(sealed_key("v2.c!r.abc"), None);
        assert_eq!(sealed_key("nope"), None);
    }

    #[test]
    fn weak_keys_and_bad_key_ids_are_refused() {
        assert_eq!(
            SealingKey::from_bytes("k", &[42u8; 32]).unwrap_err(),
            SecretBoxError::WeakKey
        );
        // Fifteen distinct values: still weak.
        let mut low = [0u8; 32];
        for (i, b) in low.iter_mut().enumerate() {
            *b = (i % 15) as u8;
        }
        assert_eq!(
            SealingKey::from_bytes("k", &low).unwrap_err(),
            SecretBoxError::WeakKey
        );
        let mut sixteen = [0u8; 32];
        for (i, b) in sixteen.iter_mut().enumerate() {
            *b = (i % 16) as u8;
        }
        assert!(SealingKey::from_bytes("k", &sixteen).is_ok());
        assert_eq!(
            SealingKey::from_bytes("k", &[1u8; 31]).unwrap_err(),
            SecretBoxError::Key
        );
        let long = "x".repeat(33);
        for bad in ["", "-a", "a.b", "a b", long.as_str()] {
            assert_eq!(
                SealingKey::from_bytes(bad, &key_bytes(1)).unwrap_err(),
                SecretBoxError::KeyId,
                "{bad}"
            );
        }
        assert!(SealingKey::from_base64("k", "not base64!").is_err());
        // Previous may not repeat the current id or the current bytes.
        assert_eq!(
            SecretBox::new(key("a", 1), Some(key("a", 2))).unwrap_err(),
            SecretBoxError::DuplicateKey
        );
        assert_eq!(
            SecretBox::new(key("a", 1), Some(key("b", 1))).unwrap_err(),
            SecretBoxError::DuplicateKey
        );
    }

    #[test]
    fn legacy_v1_seals_open_with_the_declared_legacy_key() {
        let raw = key_bytes(11);
        let legacy = legacy_seal(&raw, "whsec_old", "sub-9");
        assert_eq!(sealed_key(&legacy), Some(SealedKey::Legacy));
        // Upgraded deployment, same key, no id configured: k0 is the legacy key.
        let same = SecretBox::from_bytes(&raw).unwrap();
        assert_eq!(same.open(&legacy, "sub-9").unwrap(), "whsec_old");
        assert_eq!(same.open(&legacy, "sub-1"), Err(SecretBoxError::Open));
        // Rotated with the old key as previous (kid k0): still opens.
        let rotated = SecretBox::new(
            key("k1", 12),
            Some(SealingKey::from_bytes("k0", &raw).unwrap()),
        )
        .unwrap();
        assert_eq!(rotated.open(&legacy, "sub-9").unwrap(), "whsec_old");
        // Legacy kid declared explicitly.
        let declared = SecretBox::new(SealingKey::from_bytes("old", &raw).unwrap(), None)
            .unwrap()
            .with_legacy_key_id("old")
            .unwrap();
        assert_eq!(declared.open(&legacy, "sub-9").unwrap(), "whsec_old");
        assert_eq!(declared.resolved_kid(&SealedKey::Legacy), "old");
        // Legacy key no longer configured: fails closed.
        let gone = SecretBox::new(key("k1", 12), None).unwrap();
        assert_eq!(gone.open(&legacy, "sub-9"), Err(SecretBoxError::UnknownKey));
    }
}
