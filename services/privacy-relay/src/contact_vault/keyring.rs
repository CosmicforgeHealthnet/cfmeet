//! Contact Vault key management (§31, §32).
//!
//! Two INDEPENDENT keyrings, each loaded from a deployment-managed secret file (there is
//! no generated, embedded or fallback key, and no key byte is ever stored in PostgreSQL —
//! rows record only the key VERSION):
//!
//! * `CONTACT_VAULT_KEYRING_FILE` — AES-256-GCM encryption keys (`purpose` =
//!   `CONTACT_ENCRYPTION`);
//! * `CONTACT_FINGERPRINT_KEYRING_FILE` — HMAC-SHA-256 equality-fingerprint keys
//!   (`purpose` = `CONTACT_FINGERPRINT`).
//!
//! A keyring holds several readable versions and exactly one ACTIVE version used for new
//! writes. Rotation = add the new version (old stays readable) → make it active →
//! re-encrypt / backfill in bounded batches → verify nothing references the old version →
//! remove it from the file (never automatic). The two keyrings must not share any key.
//!
//! File format (contract `cfmeet.privacy-relay.contact-keyring`, never committed):
//!
//! ```json
//! { "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "ck-2026-10",
//!   "keys": [ { "version": "ck-2026-10", "key": "<base64 of 32 random bytes>" } ] }
//! ```

use std::{collections::BTreeMap, fmt, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

/// Key material: exactly 32 bytes, zeroized on drop, never printable.
pub struct KeyBytes(Zeroizing<[u8; 32]>);

impl KeyBytes {
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// Test-only constructor (fixed vectors).
    #[cfg(test)]
    pub(crate) fn from_array(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

impl fmt::Debug for KeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyBytes([REDACTED])")
    }
}

/// What a keyring is for; a file of one purpose is refused where the other is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPurpose {
    Encryption,
    Fingerprint,
}

impl KeyPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeyPurpose::Encryption => "CONTACT_ENCRYPTION",
            KeyPurpose::Fingerprint => "CONTACT_FINGERPRINT",
        }
    }
}

/// Validated key version id (`^[a-z0-9][a-z0-9-]{0,62}$`); stored next to every
/// ciphertext / fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyVersion(String);

impl KeyVersion {
    pub fn parse(value: &str) -> Option<Self> {
        let ok = !value.is_empty()
            && value.len() <= 63
            && value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The key-provider abstraction (a file today; a KMS adapter can implement the same
/// interface without touching callers).
pub trait ContactKeyProvider: Send + Sync {
    fn purpose(&self) -> KeyPurpose;
    /// The active version, used for every new write.
    fn current_key(&self) -> (&KeyVersion, &KeyBytes);
    /// Any readable version (decryption / matching during rotation overlap).
    fn key_by_version(&self, version: &KeyVersion) -> Option<&KeyBytes>;
    /// Every readable version, active first.
    fn versions(&self) -> Vec<KeyVersion>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyringError {
    #[error("the keyring file cannot be read")]
    Unreadable,
    #[error("the keyring is malformed")]
    Malformed,
    #[error("unsupported keyring version")]
    UnsupportedVersion,
    #[error("the keyring purpose is {found}, expected {expected}")]
    WrongPurpose {
        expected: &'static str,
        found: String,
    },
    #[error("the keyring must contain 1..=8 keys")]
    KeyCount,
    #[error("key version `{0}` is invalid")]
    InvalidVersion(String),
    #[error("key version {0} appears more than once")]
    DuplicateVersion(String),
    #[error("key {0} is not exactly 32 bytes of base64")]
    InvalidKey(String),
    #[error("key {0} is weak (all bytes equal)")]
    WeakKey(String),
    #[error("two versions share the same key material")]
    DuplicateMaterial,
    #[error("the active version is not in the keyring")]
    UnknownActive,
    #[error("the encryption and fingerprint keyrings share key material")]
    SharedAcrossPurposes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeyring {
    keyring_version: u32,
    purpose: String,
    active: String,
    keys: Vec<RawKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    version: String,
    key: String,
}

impl Drop for RawKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// A file-backed keyring.
pub struct FileKeyring {
    purpose: KeyPurpose,
    active: KeyVersion,
    keys: BTreeMap<KeyVersion, KeyBytes>,
}

impl fmt::Debug for FileKeyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileKeyring")
            .field("purpose", &self.purpose)
            .field("active", &self.active)
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl FileKeyring {
    pub fn from_json(json: &str, purpose: KeyPurpose) -> Result<Self, KeyringError> {
        let raw: RawKeyring = serde_json::from_str(json).map_err(|_| KeyringError::Malformed)?;
        if raw.keyring_version != 1 {
            return Err(KeyringError::UnsupportedVersion);
        }
        if raw.purpose != purpose.as_str() {
            return Err(KeyringError::WrongPurpose {
                expected: purpose.as_str(),
                found: raw.purpose.chars().take(32).collect(),
            });
        }
        if raw.keys.is_empty() || raw.keys.len() > 8 {
            return Err(KeyringError::KeyCount);
        }
        let mut keys = BTreeMap::new();
        for k in &raw.keys {
            let version = KeyVersion::parse(&k.version).ok_or_else(|| {
                KeyringError::InvalidVersion(k.version.chars().take(64).collect())
            })?;
            let decoded = Zeroizing::new(
                STANDARD
                    .decode(k.key.trim())
                    .map_err(|_| KeyringError::InvalidKey(version.to_string()))?,
            );
            let bytes: [u8; 32] = decoded
                .as_slice()
                .try_into()
                .map_err(|_| KeyringError::InvalidKey(version.to_string()))?;
            let bytes = Zeroizing::new(bytes);
            if bytes.iter().all(|b| *b == bytes[0]) {
                return Err(KeyringError::WeakKey(version.to_string()));
            }
            if keys
                .values()
                .any(|existing: &KeyBytes| existing.expose() == &*bytes)
            {
                return Err(KeyringError::DuplicateMaterial);
            }
            if keys.insert(version.clone(), KeyBytes(bytes)).is_some() {
                return Err(KeyringError::DuplicateVersion(version.to_string()));
            }
        }
        let active = KeyVersion::parse(&raw.active).ok_or(KeyringError::UnknownActive)?;
        if !keys.contains_key(&active) {
            return Err(KeyringError::UnknownActive);
        }
        Ok(Self {
            purpose,
            active,
            keys,
        })
    }

    pub fn from_file(path: &Path, purpose: KeyPurpose) -> Result<Self, KeyringError> {
        let json =
            Zeroizing::new(std::fs::read_to_string(path).map_err(|_| KeyringError::Unreadable)?);
        Self::from_json(&json, purpose)
    }
}

impl ContactKeyProvider for FileKeyring {
    fn purpose(&self) -> KeyPurpose {
        self.purpose
    }

    fn current_key(&self) -> (&KeyVersion, &KeyBytes) {
        (&self.active, &self.keys[&self.active])
    }

    fn key_by_version(&self, version: &KeyVersion) -> Option<&KeyBytes> {
        self.keys.get(version)
    }

    fn versions(&self) -> Vec<KeyVersion> {
        let mut out = vec![self.active.clone()];
        out.extend(self.keys.keys().filter(|v| **v != self.active).cloned());
        out
    }
}

/// Refuses an encryption keyring and a fingerprint keyring that share key material.
pub fn ensure_independent(
    encryption: &dyn ContactKeyProvider,
    fingerprint: &dyn ContactKeyProvider,
) -> Result<(), KeyringError> {
    if encryption.purpose() != KeyPurpose::Encryption
        || fingerprint.purpose() != KeyPurpose::Fingerprint
    {
        return Err(KeyringError::Malformed);
    }
    for e in encryption.versions() {
        for f in fingerprint.versions() {
            let (Some(a), Some(b)) = (
                encryption.key_by_version(&e),
                fingerprint.key_by_version(&f),
            ) else {
                continue;
            };
            if a.expose() == b.expose() {
                return Err(KeyringError::SharedAcrossPurposes);
            }
        }
    }
    Ok(())
}

/// A new keyring document with one fresh random key (operator tooling: written to a
/// secret file by `privacy-relay-admin contact-vault generate-keyring`).
pub fn generate_keyring_json(purpose: KeyPurpose, version: &KeyVersion) -> Zeroizing<String> {
    use aes_gcm::aead::{rand_core::RngCore, OsRng};
    let mut key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(&mut *key);
    let encoded = Zeroizing::new(STANDARD.encode(*key));
    Zeroizing::new(format!(
        "{{\n  \"keyring_version\": 1,\n  \"purpose\": \"{}\",\n  \"active\": \"{}\",\n  \"keys\": [\n    {{ \"version\": \"{}\", \"key\": \"{}\" }}\n  ]\n}}\n",
        purpose.as_str(),
        version,
        version,
        encoded.as_str()
    ))
}

/// Adds a fresh key version to an existing keyring document (rotation step 1); the
/// active version is NOT changed.
pub fn add_key_version(
    json: &str,
    purpose: KeyPurpose,
    version: &KeyVersion,
) -> Result<Zeroizing<String>, KeyringError> {
    use aes_gcm::aead::{rand_core::RngCore, OsRng};
    let existing = FileKeyring::from_json(json, purpose)?;
    if existing.keys.contains_key(version) {
        return Err(KeyringError::DuplicateVersion(version.to_string()));
    }
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| KeyringError::Malformed)?;
    let mut key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(&mut *key);
    let keys = value
        .get_mut("keys")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(KeyringError::Malformed)?;
    keys.push(serde_json::json!({ "version": version.as_str(), "key": STANDARD.encode(*key) }));
    let out =
        Zeroizing::new(serde_json::to_string_pretty(&value).map_err(|_| KeyringError::Malformed)?);
    value.zeroize_keys();
    Ok(out)
}

trait ZeroizeKeys {
    fn zeroize_keys(&mut self);
}

impl ZeroizeKeys for serde_json::Value {
    fn zeroize_keys(&mut self) {
        if let Some(keys) = self
            .get_mut("keys")
            .and_then(serde_json::Value::as_array_mut)
        {
            for k in keys {
                if let Some(serde_json::Value::String(s)) = k.get_mut("key") {
                    s.zeroize();
                }
            }
        }
    }
}

/// Sets the active version of a keyring document (rotation step 2).
pub fn activate_key_version(
    json: &str,
    purpose: KeyPurpose,
    version: &KeyVersion,
) -> Result<Zeroizing<String>, KeyringError> {
    let existing = FileKeyring::from_json(json, purpose)?;
    if !existing.keys.contains_key(version) {
        return Err(KeyringError::UnknownActive);
    }
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| KeyringError::Malformed)?;
    value["active"] = serde_json::Value::String(version.to_string());
    let out =
        Zeroizing::new(serde_json::to_string_pretty(&value).map_err(|_| KeyringError::Malformed)?);
    value.zeroize_keys();
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn keyring(
        purpose: KeyPurpose,
        active: &str,
        versions: &[(&str, u8)],
    ) -> FileKeyring {
        let keys: Vec<_> = versions
            .iter()
            .map(|(v, seed)| {
                let mut bytes = [0u8; 32];
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = seed.wrapping_add(i as u8);
                }
                serde_json::json!({ "version": v, "key": STANDARD.encode(bytes) })
            })
            .collect();
        FileKeyring::from_json(
            &serde_json::json!({ "keyring_version": 1, "purpose": purpose.as_str(), "active": active, "keys": keys })
                .to_string(),
            purpose,
        )
        .unwrap()
    }

    #[test]
    fn keyrings_are_strictly_validated() {
        let k =
            |v: &str, b: [u8; 32]| serde_json::json!({ "version": v, "key": STANDARD.encode(b) });
        let mut a = [7u8; 32];
        a[0] = 1;
        let doc = |purpose: &str, active: &str, keys: Vec<serde_json::Value>| {
            serde_json::json!({ "keyring_version": 1, "purpose": purpose, "active": active, "keys": keys }).to_string()
        };
        assert!(FileKeyring::from_json(
            &doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", a)]),
            KeyPurpose::Encryption
        )
        .is_ok());
        for (json, err) in [
            (doc("CONTACT_FINGERPRINT", "v1", vec![k("v1", a)]), "purpose"),
            (doc("CONTACT_ENCRYPTION", "v2", vec![k("v1", a)]), "active"),
            (doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", [9u8; 32])]), "weak"),
            (doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", a), k("v2", a)]), "duplicate material"),
            (doc("CONTACT_ENCRYPTION", "V1", vec![k("V1", a)]), "version"),
            (doc("CONTACT_ENCRYPTION", "v1", vec![]), "count"),
            (
                serde_json::json!({ "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [{ "version": "v1", "key": STANDARD.encode([1u8; 16]) }] })
                .to_string(),
                "short key",
            ),
        ] {
            let e = FileKeyring::from_json(&json, KeyPurpose::Encryption).unwrap_err();
            // Errors never echo key material.
            assert!(!e.to_string().contains(&STANDARD.encode(a)), "{err}");
        }
    }

    #[test]
    fn the_two_purposes_must_not_share_keys() {
        let enc = keyring(KeyPurpose::Encryption, "e1", &[("e1", 1)]);
        let fp = keyring(KeyPurpose::Fingerprint, "f1", &[("f1", 2)]);
        assert!(ensure_independent(&enc, &fp).is_ok());
        let shared = keyring(KeyPurpose::Fingerprint, "f1", &[("f1", 1)]);
        assert_eq!(
            ensure_independent(&enc, &shared),
            Err(KeyringError::SharedAcrossPurposes)
        );
    }

    #[test]
    fn rotation_documents_add_then_activate() {
        let v1 = KeyVersion::parse("ck-1").unwrap();
        let v2 = KeyVersion::parse("ck-2").unwrap();
        let doc = generate_keyring_json(KeyPurpose::Encryption, &v1);
        let added = add_key_version(&doc, KeyPurpose::Encryption, &v2).unwrap();
        let ring = FileKeyring::from_json(&added, KeyPurpose::Encryption).unwrap();
        assert_eq!(ring.current_key().0, &v1, "adding never activates");
        assert_eq!(ring.versions(), vec![v1.clone(), v2.clone()]);
        let activated = activate_key_version(&added, KeyPurpose::Encryption, &v2).unwrap();
        let ring = FileKeyring::from_json(&activated, KeyPurpose::Encryption).unwrap();
        assert_eq!(ring.current_key().0, &v2);
        assert!(
            ring.key_by_version(&v1).is_some(),
            "the old version stays readable"
        );
        assert!(!format!("{ring:?}").contains("key\""));
    }
}
