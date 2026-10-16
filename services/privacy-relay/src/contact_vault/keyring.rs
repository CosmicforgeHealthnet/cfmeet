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
    #[error("key {0} is weak (too little byte diversity or a repeating pattern)")]
    WeakKey(String),
    #[error("two versions share the same key material")]
    DuplicateMaterial,
    #[error("the active version is not in the keyring")]
    UnknownActive,
    #[error("the encryption and fingerprint keyrings share key material")]
    SharedAcrossPurposes,
    #[error("key version {0} is the active version and cannot be retired")]
    ActiveVersion(String),
    #[error("key version {0} is still referenced by stored rows and cannot be retired")]
    ReferencedVersion(String),
    #[error("key version {0} is referenced by stored rows but is not readable")]
    UnreadableVersion(String),
}

/// Weak-key rule (§29): a 32-byte key is refused when it shows fewer than 16 distinct
/// byte values or is a repetition of a pattern of at most 4 bytes (all-zero, all-equal,
/// `abab…`, `0123 0123…`). Random keys pass this with overwhelming probability; the
/// rule only catches placeholder, truncated or deliberately degenerate material.
pub(crate) fn is_weak(bytes: &[u8; 32]) -> bool {
    let mut seen = [false; 256];
    let mut distinct = 0;
    for b in bytes {
        if !seen[*b as usize] {
            seen[*b as usize] = true;
            distinct += 1;
        }
    }
    if distinct < 16 {
        return true;
    }
    (1..=4).any(|period| {
        bytes
            .iter()
            .enumerate()
            .all(|(i, b)| *b == bytes[i % period])
    })
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
            if is_weak(&bytes) {
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

/// Removes a NON-active, NO-LONGER-REFERENCED version from a keyring document (rotation
/// step "retire"). `referenced` is the set of versions stored rows still name (the
/// caller reads it from the database); a referenced version is refused so that the
/// keyring never loses a key some ciphertext or fingerprint still needs.
pub fn retire_key_version(
    json: &str,
    purpose: KeyPurpose,
    version: &KeyVersion,
    referenced: &[String],
) -> Result<Zeroizing<String>, KeyringError> {
    let existing = FileKeyring::from_json(json, purpose)?;
    if !existing.keys.contains_key(version) {
        return Err(KeyringError::InvalidVersion(version.to_string()));
    }
    if existing.active == *version {
        return Err(KeyringError::ActiveVersion(version.to_string()));
    }
    if referenced.iter().any(|r| r == version.as_str()) {
        return Err(KeyringError::ReferencedVersion(version.to_string()));
    }
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| KeyringError::Malformed)?;
    let keys = value
        .get_mut("keys")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(KeyringError::Malformed)?;
    for k in keys.iter_mut() {
        if k.get("version").and_then(serde_json::Value::as_str) == Some(version.as_str()) {
            if let Some(serde_json::Value::String(s)) = k.get_mut("key") {
                s.zeroize();
            }
        }
    }
    keys.retain(|k| k.get("version").and_then(serde_json::Value::as_str) != Some(version.as_str()));
    let out =
        Zeroizing::new(serde_json::to_string_pretty(&value).map_err(|_| KeyringError::Malformed)?);
    value.zeroize_keys();
    // The result must still be a valid keyring.
    FileKeyring::from_json(&out, purpose)?;
    Ok(out)
}

/// The versions among `referenced` that `keys` cannot read (rotation safety: a service
/// must not start, and a version must not be retired, while stored rows name a version
/// the keyring no longer holds).
pub fn unreadable_versions(keys: &dyn ContactKeyProvider, referenced: &[String]) -> Vec<String> {
    let mut out: Vec<String> = referenced
        .iter()
        .filter(|r| {
            KeyVersion::parse(r)
                .map(|v| keys.key_by_version(&v).is_none())
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 32 pseudo-random bytes from a seeded SplitMix64 (deterministic fixtures that are
    /// never a structured pattern).
    pub(crate) fn fixture_key(seed: u8) -> [u8; 32] {
        let mut state =
            0x9e37_79b9_7f4a_7c15_u64 ^ (seed as u64).wrapping_mul(0x5851_f42d_4c95_7f2d);
        let mut out = [0u8; 32];
        for chunk in out.chunks_mut(8) {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
        out
    }

    pub(crate) fn keyring(
        purpose: KeyPurpose,
        active: &str,
        versions: &[(&str, u8)],
    ) -> FileKeyring {
        let keys: Vec<_> = versions
            .iter()
            .map(|(v, seed)| {
                serde_json::json!({ "version": v, "key": STANDARD.encode(fixture_key(*seed)) })
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
        let a = fixture_key(1);
        let doc = |purpose: &str, active: &str, keys: Vec<serde_json::Value>| {
            serde_json::json!({ "keyring_version": 1, "purpose": purpose, "active": active, "keys": keys }).to_string()
        };
        assert!(FileKeyring::from_json(
            &doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", a)]),
            KeyPurpose::Encryption
        )
        .is_ok());
        let all_zero = [0u8; 32];
        let mut two_values = [7u8; 32];
        two_values[0] = 1;
        let mut pattern4 = [0u8; 32];
        for (i, b) in pattern4.iter_mut().enumerate() {
            *b = [0x12, 0x34, 0x56, 0x78][i % 4];
        }
        let mut pattern2 = [0u8; 32];
        for (i, b) in pattern2.iter_mut().enumerate() {
            *b = [0xab, 0xcd][i % 2];
        }
        let cases: Vec<(String, KeyringError)> = vec![
            (
                doc("CONTACT_FINGERPRINT", "v1", vec![k("v1", a)]),
                KeyringError::WrongPurpose {
                    expected: "CONTACT_ENCRYPTION",
                    found: "CONTACT_FINGERPRINT".into(),
                },
            ),
            (
                doc("CONTACT_ENCRYPTION", "v2", vec![k("v1", a)]),
                KeyringError::UnknownActive,
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", [9u8; 32])]),
                KeyringError::WeakKey("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", all_zero)]),
                KeyringError::WeakKey("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", two_values)]),
                KeyringError::WeakKey("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", pattern4)]),
                KeyringError::WeakKey("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", pattern2)]),
                KeyringError::WeakKey("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![k("v1", a), k("v2", a)]),
                KeyringError::DuplicateMaterial,
            ),
            (
                doc(
                    "CONTACT_ENCRYPTION",
                    "v1",
                    vec![k("v1", a), k("v1", fixture_key(2))],
                ),
                KeyringError::DuplicateVersion("v1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "V1", vec![k("V1", a)]),
                KeyringError::InvalidVersion("V1".into()),
            ),
            (
                doc("CONTACT_ENCRYPTION", "v1", vec![]),
                KeyringError::KeyCount,
            ),
            (
                serde_json::json!({ "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [{ "version": "v1", "key": STANDARD.encode([1u8; 16]) }] })
                .to_string(),
                KeyringError::InvalidKey("v1".into()),
            ),
            (
                serde_json::json!({ "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [{ "version": "v1", "key": "not base64!" }] })
                .to_string(),
                KeyringError::InvalidKey("v1".into()),
            ),
            (
                serde_json::json!({ "keyring_version": 2, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [k("v1", a)] })
                .to_string(),
                KeyringError::UnsupportedVersion,
            ),
            ("{ not json".to_string(), KeyringError::Malformed),
            (
                serde_json::json!({ "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [k("v1", a)], "extra": true })
                .to_string(),
                KeyringError::Malformed,
            ),
            (
                serde_json::json!({ "keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",
                    "keys": [{ "version": "v1", "key": STANDARD.encode(a), "comment": "x" }] })
                .to_string(),
                KeyringError::Malformed,
            ),
        ];
        for (json, expected) in cases {
            let e = FileKeyring::from_json(&json, KeyPurpose::Encryption).unwrap_err();
            assert_eq!(e, expected, "{json}");
            // Errors never echo key material.
            assert!(!e.to_string().contains(&STANDARD.encode(a)));
        }
        // Nine keys: too many.
        let nine: Vec<_> = (0..9u8)
            .map(|i| k(&format!("v{i}"), fixture_key(10 + i)))
            .collect();
        assert_eq!(
            FileKeyring::from_json(
                &doc("CONTACT_ENCRYPTION", "v0", nine),
                KeyPurpose::Encryption
            )
            .unwrap_err(),
            KeyringError::KeyCount
        );
        // The weak-key rule itself.
        assert!(is_weak(&all_zero));
        assert!(is_weak(&two_values));
        assert!(is_weak(&pattern4));
        assert!(!is_weak(&a));
        assert!(!is_weak(&core::array::from_fn(|i| i as u8)));
    }

    #[test]
    fn retiring_a_version_is_refused_while_active_or_referenced() {
        let v1 = KeyVersion::parse("ck-1").unwrap();
        let v2 = KeyVersion::parse("ck-2").unwrap();
        let doc = generate_keyring_json(KeyPurpose::Encryption, &v1);
        let both = add_key_version(&doc, KeyPurpose::Encryption, &v2).unwrap();
        assert_eq!(
            retire_key_version(&both, KeyPurpose::Encryption, &v1, &[]).unwrap_err(),
            KeyringError::ActiveVersion("ck-1".into())
        );
        let activated = activate_key_version(&both, KeyPurpose::Encryption, &v2).unwrap();
        assert_eq!(
            retire_key_version(&activated, KeyPurpose::Encryption, &v1, &["ck-1".into()])
                .unwrap_err(),
            KeyringError::ReferencedVersion("ck-1".into())
        );
        assert_eq!(
            retire_key_version(
                &activated,
                KeyPurpose::Encryption,
                &KeyVersion::parse("ck-9").unwrap(),
                &[]
            )
            .unwrap_err(),
            KeyringError::InvalidVersion("ck-9".into())
        );
        let retired =
            retire_key_version(&activated, KeyPurpose::Encryption, &v1, &["ck-2".into()]).unwrap();
        let ring = FileKeyring::from_json(&retired, KeyPurpose::Encryption).unwrap();
        assert_eq!(ring.versions(), vec![v2.clone()]);
        assert!(ring.key_by_version(&v1).is_none());
        // The retired material is gone from the document.
        let old_key = serde_json::from_str::<serde_json::Value>(&doc).unwrap()["keys"][0]["key"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!retired.contains(&old_key));
        // Referenced-but-unreadable detection (service start-up and rotation-status).
        assert_eq!(
            unreadable_versions(&ring, &["ck-1".into(), "ck-2".into(), "ck-1".into()]),
            vec!["ck-1".to_string()]
        );
        assert!(unreadable_versions(&ring, &["ck-2".into()]).is_empty());
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
