//! Optional secure backup of a client's local state (feature `client`; Part V Appendix
//! I.3, H.4, ADR 0052 AD-07).
//!
//! Backups are opt-in and encrypted ON THE CLIENT before upload; the server stores an
//! opaque object it cannot decrypt (it can read only the public header below, which
//! carries ids and the key id — never key material or content).
//!
//! * A **backup root key** ([`BackupRootKey`], 256 random bits) is generated on the client
//!   and shown to the user once as a **recovery code** ([`BackupRootKey::recovery_code`]):
//!   RFC 4648 base32 (no padding) of the key plus a 2-byte checksum, in groups of five,
//!   prefixed `CFMB1`. A mistyped code is refused by the checksum before any decryption.
//! * Every backup derives a fresh **content key** with HKDF-SHA256 from the root key and a
//!   random 32-byte salt, so no (key, nonce) pair repeats across backups.
//! * The **container** is versioned and authenticated: a public header (magic `CFMB1`,
//!   salt, STREAM nonce prefix, key id, backup id, principal id, creation time) followed
//!   by XChaCha20-Poly1305 STREAM chunks (the construction of [`crate::attachment`]) whose
//!   associated data is the whole header. Tampering with any header byte or chunk, and
//!   truncating, reordering or dropping chunks, fails authentication.
//! * The **payload** is a list of named sections (e.g. `MESSAGES`, `MLS_STATE`,
//!   `ATTACHMENT_DESCRIPTORS`), length-prefixed, so a client includes exactly what its
//!   policy allows.
//! * **Rotation** ([`reencrypt`]) decrypts with the old root key and re-encrypts under a
//!   new one (new salt, new key id); after the old backup object is deleted the old
//!   recovery code recovers nothing.
//!
//! Without the root key nothing can be restored: there is no server-side escrow.
//! No primitive is implemented here; key bytes are zeroized on drop and never printed.
//! The header parser ([`inspect`]) and the constants are available without the `client`
//! feature (the backend validates uploads with them); everything holding a key is
//! client-only.

use uuid::Uuid;

#[cfg(feature = "client")]
pub use sealed::*;

/// Format identifier (also the first five header bytes).
pub const BACKUP_FORMAT_V1: &str = "CFMEET_BACKUP_XCHACHA20POLY1305_STREAM_V1";
/// Magic and version of the container.
pub const MAGIC: &[u8; 5] = b"CFMB1";
/// Plaintext bytes per chunk.
pub const CHUNK_SIZE: usize = 64 * 1024;
const TAG: usize = 16;
const SALT: usize = 32;
const NONCE_PREFIX: usize = 19;
const KEY_ID: usize = 16;
/// magic | salt | nonce prefix | key id | backup id | principal id | created (u64 BE)
pub const HEADER: usize = MAGIC.len() + SALT + NONCE_PREFIX + KEY_ID + 16 + 16 + 8;
/// Largest payload this module encrypts (bounded memory per call).
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub const MAX_PLAINTEXT: usize = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BackupError {
    #[error("backup too large")]
    TooLarge,
    #[error("not a CF Meet backup, or an unsupported version")]
    Malformed,
    #[error("the recovery code is invalid")]
    InvalidRecoveryCode,
    #[error("this backup was made with another root key")]
    WrongKey,
    #[error("this backup belongs to another principal")]
    WrongPrincipal,
    #[error("the backup failed authentication (tampered, truncated or reordered)")]
    Authentication,
}

/// The public header of a backup (what the server may read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupHeader {
    pub key_id: [u8; KEY_ID],
    pub backup_id: Uuid,
    pub principal_id: Uuid,
    pub created_at_unix: u64,
}

/// Parses the public header of a backup object (no key needed; never content).
pub fn inspect(bytes: &[u8]) -> Result<BackupHeader, BackupError> {
    if bytes.len() < HEADER + TAG || &bytes[..MAGIC.len()] != MAGIC {
        return Err(BackupError::Malformed);
    }
    let mut at = MAGIC.len() + SALT + NONCE_PREFIX;
    let mut key_id = [0u8; KEY_ID];
    key_id.copy_from_slice(&bytes[at..at + KEY_ID]);
    at += KEY_ID;
    let backup_id = Uuid::from_slice(&bytes[at..at + 16]).map_err(|_| BackupError::Malformed)?;
    at += 16;
    let principal_id = Uuid::from_slice(&bytes[at..at + 16]).map_err(|_| BackupError::Malformed)?;
    at += 16;
    let created_at_unix = u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
    Ok(BackupHeader {
        key_id,
        backup_id,
        principal_id,
        created_at_unix,
    })
}

/// Size of the backup object of a `payload_size`-byte encoded payload.
pub fn ciphertext_size(payload_size: u64) -> u64 {
    let chunks = if payload_size == 0 {
        1
    } else {
        payload_size.div_ceil(CHUNK_SIZE as u64)
    };
    HEADER as u64 + payload_size + chunks * TAG as u64
}

#[cfg(feature = "client")]
mod sealed {
    use aead::rand_core::RngCore;
    use aead::{
        stream::{DecryptorBE32, EncryptorBE32},
        KeyInit, OsRng,
    };
    use chacha20poly1305::XChaCha20Poly1305;
    use hkdf::Hkdf;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::*;

    /// The client's backup root key (zeroized on drop; never printed).
    #[derive(Clone, PartialEq, Eq)]
    pub struct BackupRootKey([u8; 32]);

    impl Drop for BackupRootKey {
        fn drop(&mut self) {
            zeroize::Zeroize::zeroize(&mut self.0);
        }
    }

    impl std::fmt::Debug for BackupRootKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("BackupRootKey([redacted])")
        }
    }

    const MAX_SECTIONS: usize = 64;
    const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

    fn base32(bytes: &[u8]) -> String {
        let mut out = String::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for &b in bytes {
            acc = (acc << 8) | b as u32;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(B32[((acc >> bits) & 31) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(B32[((acc << (5 - bits)) & 31) as usize] as char);
        }
        out
    }

    fn unbase32(text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in text.bytes() {
            let v = B32.iter().position(|&x| x == c)? as u32;
            acc = (acc << 5) | v;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        // Leftover bits must be zero padding.
        if bits >= 5 || acc & ((1 << bits) - 1) != 0 {
            return None;
        }
        Some(out)
    }

    impl BackupRootKey {
        pub fn generate() -> Self {
            let mut k = [0u8; 32];
            OsRng.fill_bytes(&mut k);
            Self(k)
        }

        pub fn from_bytes(bytes: [u8; 32]) -> Self {
            Self(bytes)
        }

        pub fn as_bytes(&self) -> &[u8; 32] {
            &self.0
        }

        /// The public key id carried in every backup header (does not reveal the key).
        pub fn key_id(&self) -> [u8; KEY_ID] {
            let hk = Hkdf::<Sha256>::new(Some(b"cfmeet-backup:v1:key-id"), &self.0);
            let mut id = [0u8; KEY_ID];
            hk.expand(b"key-id", &mut id).expect("16 bytes");
            id
        }

        /// `CFMB1-XXXXX-XXXXX-…`: the key and a 2-byte SHA-256 checksum in base32.
        pub fn recovery_code(&self) -> String {
            let mut raw = self.0.to_vec();
            raw.extend_from_slice(&Sha256::digest(self.0)[..2]);
            let mut b = base32(&raw);
            zeroize::Zeroize::zeroize(&mut raw);
            let groups: Vec<String> = b
                .as_bytes()
                .chunks(5)
                .map(|g| String::from_utf8(g.to_vec()).expect("ascii"))
                .collect();
            let code = format!("CFMB1-{}", groups.join("-"));
            zeroize::Zeroize::zeroize(&mut b);
            code
        }

        /// Parses a recovery code (case, spaces and dashes are ignored).
        pub fn from_recovery_code(code: &str) -> Result<Self, BackupError> {
            let cleaned: String = code
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .collect::<String>()
                .to_ascii_uppercase();
            let body = cleaned
                .strip_prefix("CFMB1")
                .ok_or(BackupError::InvalidRecoveryCode)?;
            let raw = unbase32(body).ok_or(BackupError::InvalidRecoveryCode)?;
            if raw.len() != 34 {
                return Err(BackupError::InvalidRecoveryCode);
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&raw[..32]);
            if Sha256::digest(key)[..2] != raw[32..] {
                zeroize::Zeroize::zeroize(&mut key);
                return Err(BackupError::InvalidRecoveryCode);
            }
            Ok(Self(key))
        }

        fn content_key(&self, salt: &[u8; SALT]) -> zeroize::Zeroizing<[u8; 32]> {
            let hk = Hkdf::<Sha256>::new(Some(salt), &self.0);
            let mut k = zeroize::Zeroizing::new([0u8; 32]);
            hk.expand(b"cfmeet-backup:v1:content", k.as_mut())
                .expect("32 bytes");
            k
        }
    }

    /// The client-defined contents of a backup: named sections, in order.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct BackupPayload {
        pub sections: Vec<(String, Vec<u8>)>,
    }

    impl BackupPayload {
        pub(crate) fn encode(&self) -> Result<Vec<u8>, BackupError> {
            if self.sections.len() > MAX_SECTIONS {
                return Err(BackupError::TooLarge);
            }
            let mut out = Vec::new();
            out.extend_from_slice(&(self.sections.len() as u32).to_be_bytes());
            for (name, data) in &self.sections {
                if name.is_empty()
                    || name.len() > 64
                    || !name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                {
                    return Err(BackupError::Malformed);
                }
                out.extend_from_slice(&(name.len() as u32).to_be_bytes());
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(&(data.len() as u64).to_be_bytes());
                out.extend_from_slice(data);
            }
            if out.len() > MAX_PLAINTEXT {
                return Err(BackupError::TooLarge);
            }
            Ok(out)
        }

        fn decode(bytes: &[u8]) -> Result<Self, BackupError> {
            let mut at = 0usize;
            let mut take = |n: usize| -> Result<&[u8], BackupError> {
                let s = bytes.get(at..at + n).ok_or(BackupError::Malformed)?;
                at += n;
                Ok(s)
            };
            let count = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
            if count > MAX_SECTIONS {
                return Err(BackupError::Malformed);
            }
            let mut sections = Vec::with_capacity(count);
            for _ in 0..count {
                let n = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
                let name =
                    String::from_utf8(take(n)?.to_vec()).map_err(|_| BackupError::Malformed)?;
                let len = u64::from_be_bytes(take(8)?.try_into().unwrap()) as usize;
                sections.push((name, take(len)?.to_vec()));
            }
            if at != bytes.len() {
                return Err(BackupError::Malformed);
            }
            Ok(Self { sections })
        }
    }

    /// Encrypts a backup under `root` with fresh randomness.
    pub fn encrypt_backup(
        root: &BackupRootKey,
        header: &BackupHeader,
        payload: &BackupPayload,
    ) -> Result<Vec<u8>, BackupError> {
        let mut salt = [0u8; SALT];
        OsRng.fill_bytes(&mut salt);
        let mut prefix = [0u8; NONCE_PREFIX];
        OsRng.fill_bytes(&mut prefix);
        encrypt_backup_deterministic(root, header, payload, salt, prefix)
    }

    /// [`encrypt_backup`] with caller-chosen salt and nonce prefix: ONLY for published test
    /// vectors (reusing randomness across backups would break the format's guarantees).
    #[doc(hidden)]
    pub fn encrypt_backup_deterministic(
        root: &BackupRootKey,
        header: &BackupHeader,
        payload: &BackupPayload,
        salt: [u8; SALT],
        prefix: [u8; NONCE_PREFIX],
    ) -> Result<Vec<u8>, BackupError> {
        if header.key_id != root.key_id() {
            return Err(BackupError::WrongKey);
        }
        let plain = zeroize::Zeroizing::new(payload.encode()?);
        let mut out = Vec::with_capacity(ciphertext_size(plain.len() as u64) as usize);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&prefix);
        out.extend_from_slice(&header.key_id);
        out.extend_from_slice(header.backup_id.as_bytes());
        out.extend_from_slice(header.principal_id.as_bytes());
        out.extend_from_slice(&header.created_at_unix.to_be_bytes());
        let aad = out.clone();
        let key = root.content_key(&salt);
        let cipher = XChaCha20Poly1305::new(key.as_ref().into());
        let mut encryptor = EncryptorBE32::from_aead(cipher, prefix.as_slice().into());
        let mut chunks = plain.chunks(CHUNK_SIZE).peekable();
        if chunks.peek().is_none() {
            out.extend_from_slice(
                &encryptor
                    .encrypt_last(aead::Payload {
                        msg: &[],
                        aad: &aad,
                    })
                    .map_err(|_| BackupError::Malformed)?,
            );
            return Ok(out);
        }
        while let Some(chunk) = chunks.next() {
            let p = aead::Payload {
                msg: chunk,
                aad: &aad,
            };
            if chunks.peek().is_some() {
                out.extend_from_slice(
                    &encryptor
                        .encrypt_next(p)
                        .map_err(|_| BackupError::Malformed)?,
                );
            } else {
                out.extend_from_slice(
                    &encryptor
                        .encrypt_last(p)
                        .map_err(|_| BackupError::Malformed)?,
                );
                break;
            }
        }
        Ok(out)
    }

    /// Restores a backup: refuses another root key or another principal's backup before
    /// decrypting, then authenticates every chunk and the header.
    pub fn decrypt_backup(
        root: &BackupRootKey,
        expected_principal: Uuid,
        bytes: &[u8],
    ) -> Result<(BackupHeader, BackupPayload), BackupError> {
        let header = inspect(bytes)?;
        if header.key_id != root.key_id() {
            return Err(BackupError::WrongKey);
        }
        if header.principal_id != expected_principal {
            return Err(BackupError::WrongPrincipal);
        }
        let aad = &bytes[..HEADER];
        let mut salt = [0u8; SALT];
        salt.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + SALT]);
        let prefix = &bytes[MAGIC.len() + SALT..MAGIC.len() + SALT + NONCE_PREFIX];
        let key = root.content_key(&salt);
        let cipher = XChaCha20Poly1305::new(key.as_ref().into());
        let mut decryptor = DecryptorBE32::from_aead(cipher, prefix.into());
        let mut plain = zeroize::Zeroizing::new(Vec::with_capacity(bytes.len() - HEADER));
        let mut chunks = bytes[HEADER..].chunks(CHUNK_SIZE + TAG).peekable();
        while let Some(chunk) = chunks.next() {
            let p = aead::Payload { msg: chunk, aad };
            if chunks.peek().is_some() {
                plain.extend_from_slice(
                    &decryptor
                        .decrypt_next(p)
                        .map_err(|_| BackupError::Authentication)?,
                );
            } else {
                plain.extend_from_slice(
                    &decryptor
                        .decrypt_last(p)
                        .map_err(|_| BackupError::Authentication)?,
                );
                break;
            }
        }
        Ok((header, BackupPayload::decode(&plain)?))
    }

    /// Key rotation: decrypts with `old`, re-encrypts under `new` with a new backup id.
    pub fn reencrypt(
        old: &BackupRootKey,
        new: &BackupRootKey,
        principal_id: Uuid,
        new_backup_id: Uuid,
        created_at_unix: u64,
        bytes: &[u8],
    ) -> Result<Vec<u8>, BackupError> {
        let (_, payload) = decrypt_backup(old, principal_id, bytes)?;
        encrypt_backup(
            new,
            &BackupHeader {
                key_id: new.key_id(),
                backup_id: new_backup_id,
                principal_id,
                created_at_unix,
            },
            &payload,
        )
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;

    fn header(root: &BackupRootKey, principal: Uuid) -> BackupHeader {
        BackupHeader {
            key_id: root.key_id(),
            backup_id: Uuid::from_u128(7),
            principal_id: principal,
            created_at_unix: 1_790_000_000,
        }
    }

    fn payload(n: usize) -> BackupPayload {
        BackupPayload {
            sections: vec![
                ("MESSAGES".into(), (0..n).map(|i| (i % 251) as u8).collect()),
                ("MLS_STATE".into(), b"opaque mls state".to_vec()),
                ("ATTACHMENT_DESCRIPTORS".into(), Vec::new()),
            ],
        }
    }

    #[test]
    fn restores_with_the_key_and_the_recovery_code() {
        let root = BackupRootKey::generate();
        let p = Uuid::from_u128(1);
        for n in [0, 1, CHUNK_SIZE - 40, CHUNK_SIZE * 3 + 5] {
            let bytes = encrypt_backup(&root, &header(&root, p), &payload(n)).unwrap();
            assert_eq!(
                bytes.len() as u64,
                ciphertext_size(payload(n).encode().unwrap().len() as u64)
            );
            let restored = BackupRootKey::from_recovery_code(&root.recovery_code()).unwrap();
            let (h, got) = decrypt_backup(&restored, p, &bytes).unwrap();
            assert_eq!((h, got), (header(&root, p), payload(n)));
        }
    }

    #[test]
    fn fails_without_the_key_for_another_principal_and_on_bad_codes() {
        let root = BackupRootKey::generate();
        let p = Uuid::from_u128(1);
        let bytes = encrypt_backup(&root, &header(&root, p), &payload(10)).unwrap();
        assert_eq!(
            decrypt_backup(&BackupRootKey::generate(), p, &bytes).unwrap_err(),
            BackupError::WrongKey
        );
        assert_eq!(
            decrypt_backup(&root, Uuid::from_u128(2), &bytes).unwrap_err(),
            BackupError::WrongPrincipal
        );
        let code = root.recovery_code();
        assert!(code.starts_with("CFMB1-"));
        // One mistyped character is caught by the checksum.
        let mut chars: Vec<char> = code.chars().collect();
        let i = code.len() - 3;
        chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert_eq!(
            BackupRootKey::from_recovery_code(&typo).unwrap_err(),
            BackupError::InvalidRecoveryCode
        );
        assert!(BackupRootKey::from_recovery_code("CFMB1-AAAAA").is_err());
        assert_eq!(
            BackupRootKey::from_recovery_code(&code.to_lowercase().replace('-', " ")).unwrap(),
            root
        );
    }

    #[test]
    fn tampering_truncation_and_reordering_are_detected() {
        let root = BackupRootKey::generate();
        let p = Uuid::from_u128(1);
        let bytes = encrypt_backup(&root, &header(&root, p), &payload(CHUNK_SIZE * 2 + 9)).unwrap();
        // Every header byte (except the magic, which makes it unreadable) and samples of
        // the body are authenticated.
        for i in (MAGIC.len()..HEADER).chain([HEADER, HEADER + 100, bytes.len() - 1]) {
            let mut t = bytes.clone();
            t[i] ^= 1;
            let e = decrypt_backup(&root, p, &t).unwrap_err();
            assert!(
                matches!(
                    e,
                    BackupError::Authentication
                        | BackupError::WrongKey
                        | BackupError::WrongPrincipal
                ),
                "byte {i}: {e:?}"
            );
        }
        let chunk = CHUNK_SIZE + TAG;
        // Truncated to whole chunks (the last-chunk flag is missing).
        let t = &bytes[..HEADER + chunk];
        assert_eq!(
            decrypt_backup(&root, p, t).unwrap_err(),
            BackupError::Authentication
        );
        // Chunks swapped.
        let mut s = bytes[..HEADER].to_vec();
        s.extend_from_slice(&bytes[HEADER + chunk..HEADER + 2 * chunk]);
        s.extend_from_slice(&bytes[HEADER..HEADER + chunk]);
        s.extend_from_slice(&bytes[HEADER + 2 * chunk..]);
        assert_eq!(
            decrypt_backup(&root, p, &s).unwrap_err(),
            BackupError::Authentication
        );
        assert_eq!(
            decrypt_backup(&root, p, b"CFMB2garbage").unwrap_err(),
            BackupError::Malformed
        );
    }

    #[test]
    fn rotation_reencrypts_under_the_new_key_only() {
        let old = BackupRootKey::generate();
        let new = BackupRootKey::generate();
        let p = Uuid::from_u128(1);
        let bytes = encrypt_backup(&old, &header(&old, p), &payload(100)).unwrap();
        let rotated = reencrypt(&old, &new, p, Uuid::from_u128(8), 1_790_000_100, &bytes).unwrap();
        assert_eq!(decrypt_backup(&new, p, &rotated).unwrap().1, payload(100));
        assert_eq!(
            decrypt_backup(&old, p, &rotated).unwrap_err(),
            BackupError::WrongKey
        );
        assert_ne!(
            inspect(&rotated).unwrap().key_id,
            inspect(&bytes).unwrap().key_id
        );
    }

    #[test]
    fn the_header_reveals_ids_only() {
        let root = BackupRootKey::generate();
        let p = Uuid::from_u128(1);
        let secret = b"super secret message text".to_vec();
        let bytes = encrypt_backup(
            &root,
            &header(&root, p),
            &BackupPayload {
                sections: vec![("MESSAGES".into(), secret.clone())],
            },
        )
        .unwrap();
        assert_eq!(inspect(&bytes).unwrap(), header(&root, p));
        assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_slice()));
        assert!(!bytes.windows(32).any(|w| w == root.as_bytes()));
    }
}
