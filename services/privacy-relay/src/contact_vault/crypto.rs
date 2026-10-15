//! Contact value encryption (§31): AES-256-GCM (RustCrypto `aes-gcm`; no primitive is
//! implemented here) with a fresh random 96-bit nonce per encryption and authenticated
//! additional data that binds every ciphertext to its owner:
//!
//! ```text
//! AAD = "cfmeet-contact-vault-v1" | tenant_id | application_id | environment_id
//!       | contact_reference | encryption_key_version
//! ```
//!
//! (UUIDs in canonical hyphenated form and the key version, `|`-separated; none of the
//! parts can contain `|`.) A ciphertext copied to another row, scope or key version fails
//! authentication instead of decrypting. Encryption is randomized: the same number
//! encrypts differently every time (equality uses the keyed fingerprint, §32).

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng, Payload},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    keyring::{ContactKeyProvider, KeyBytes, KeyVersion},
    normalization::CanonicalNumber,
};
use crate::scope::RelayScope;

/// Stored algorithm identifier.
pub const ALGORITHM: &str = "AES-256-GCM";
/// Domain separation label of the AAD. (ADR 0011 names it `cfmeet-contact-v1`, which is
/// the FINGERPRINT domain; the stored ciphertexts are bound to this exact string, so the
/// code label is authoritative and never changes.)
pub(crate) const AAD_LABEL: &str = "cfmeet-contact-vault-v1";
pub const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Ownership binding of one ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactAad<'a> {
    pub scope: RelayScope,
    pub contact_reference: Uuid,
    pub key_version: &'a KeyVersion,
}

impl ContactAad<'_> {
    fn bytes(&self) -> Vec<u8> {
        format!(
            "{AAD_LABEL}|{}|{}|{}|{}|{}",
            self.scope.tenant_id(),
            self.scope.application_id(),
            self.scope.environment_id(),
            self.contact_reference,
            self.key_version
        )
        .into_bytes()
    }
}

/// A sealed contact value as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: KeyVersion,
}

impl std::fmt::Debug for Sealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealed")
            .field("ciphertext_len", &self.ciphertext.len())
            .field("key_version", &self.key_version)
            .finish()
    }
}

/// Why a stored value cannot be opened. Fixed codes; never contains data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("the key version of the ciphertext is not available")]
    UnknownKeyVersion,
    #[error("the ciphertext is malformed")]
    Malformed,
    #[error("the ciphertext failed authentication")]
    Authentication,
    #[error("encryption failed")]
    Encryption,
}

fn cipher(key: &KeyBytes) -> Aes256Gcm {
    Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key.expose()))
}

fn seal_with_nonce(
    key: &KeyBytes,
    nonce: &Nonce<<Aes256Gcm as AeadCore>::NonceSize>,
    aad: &ContactAad<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    cipher(key)
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad.bytes(),
            },
        )
        .map_err(|_| CryptoError::Encryption)
}

/// Encrypts `number` for `scope` / `contact_reference` with the ACTIVE key.
pub fn seal(
    keys: &dyn ContactKeyProvider,
    scope: RelayScope,
    contact_reference: Uuid,
    number: &CanonicalNumber,
) -> Result<Sealed, CryptoError> {
    let (version, key) = keys.current_key();
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let aad = ContactAad {
        scope,
        contact_reference,
        key_version: version,
    };
    let ciphertext = seal_with_nonce(key, &nonce, &aad, number.expose_e164().as_bytes())?;
    Ok(Sealed {
        ciphertext,
        nonce: nonce.to_vec(),
        key_version: version.clone(),
    })
}

/// Decrypts and re-validates a stored value. The plaintext only exists zeroizing.
pub fn open(
    keys: &dyn ContactKeyProvider,
    scope: RelayScope,
    contact_reference: Uuid,
    sealed: &Sealed,
) -> Result<CanonicalNumber, CryptoError> {
    let key = keys
        .key_by_version(&sealed.key_version)
        .ok_or(CryptoError::UnknownKeyVersion)?;
    if sealed.nonce.len() != NONCE_LEN || sealed.ciphertext.len() <= TAG_LEN {
        return Err(CryptoError::Malformed);
    }
    let aad = ContactAad {
        scope,
        contact_reference,
        key_version: &sealed.key_version,
    };
    let nonce: [u8; NONCE_LEN] = sealed
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed)?;
    let plaintext = Zeroizing::new(
        cipher(key)
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad: &aad.bytes(),
                },
            )
            .map_err(|_| CryptoError::Authentication)?,
    );
    let text =
        Zeroizing::new(String::from_utf8(plaintext.to_vec()).map_err(|_| CryptoError::Malformed)?);
    CanonicalNumber::from_decrypted(text).map_err(|_| CryptoError::Malformed)
}

/// 32 bytes from the operating-system CSPRNG (grant secrets).
pub(crate) fn random_secret() -> zeroize::Zeroizing<[u8; 32]> {
    use aes_gcm::aead::rand_core::RngCore;
    let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(&mut *bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact_vault::{
        keyring::{tests::keyring, KeyPurpose},
        normalization::{normalize, RawContactValue},
    };

    fn scope() -> RelayScope {
        RelayScope::from_row(
            Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
            Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
            Uuid::parse_str("00000000-0000-4000-8000-000000000003").unwrap(),
        )
    }

    fn number(raw: &str) -> CanonicalNumber {
        normalize(&RawContactValue::new(raw), None).unwrap()
    }

    const CONTACT: &str = "00000000-0000-4000-8000-0000000000c1";
    /// AES-256-GCM(key = 00..1f, nonce = 42 * 12, AAD of `CONTACT` in `scope()` under
    /// `vector-1`, "+390212345678"): ciphertext || tag.
    const FIXED_VECTOR: &str = "31af40f6bfb497df2c471804455327bb36b54a5ad8b1c8510b2c52cb1b";

    #[test]
    fn fixed_vector_is_stable_and_bound_to_its_aad() {
        // Deterministic vector with a TEST-ONLY key and nonce (production nonces are random).
        let key = KeyBytes::from_array(core::array::from_fn(|i| i as u8));
        let nonce = &Nonce::from([0x42; NONCE_LEN]);
        let version = KeyVersion::parse("vector-1").unwrap();
        let aad = ContactAad {
            scope: scope(),
            contact_reference: Uuid::parse_str(CONTACT).unwrap(),
            key_version: &version,
        };
        let a = seal_with_nonce(&key, nonce, &aad, b"+390212345678").unwrap();
        let b = seal_with_nonce(&key, nonce, &aad, b"+390212345678").unwrap();
        assert_eq!(a, b, "same key + nonce + AAD is deterministic");
        assert_eq!(a.len(), 13 + TAG_LEN);
        assert_eq!(hex::encode(&a), FIXED_VECTOR);
        // A different AAD (another key version) yields a different tag.
        let other = KeyVersion::parse("vector-2").unwrap();
        let aad2 = ContactAad {
            key_version: &other,
            ..aad
        };
        assert_ne!(
            a,
            seal_with_nonce(&key, nonce, &aad2, b"+390212345678").unwrap()
        );
    }

    #[test]
    fn round_trip_with_random_nonces() {
        let keys = keyring(KeyPurpose::Encryption, "k1", &[("k1", 3)]);
        let id = Uuid::new_v4();
        let n = number("+390212345678");
        let a = seal(&keys, scope(), id, &n).unwrap();
        let b = seal(&keys, scope(), id, &n).unwrap();
        assert_ne!(a.nonce, b.nonce, "fresh nonce per encryption");
        assert_ne!(a.ciphertext, b.ciphertext, "randomized encryption");
        assert!(!a.ciphertext.windows(10).any(|w| w == b"0212345678"));
        let opened = open(&keys, scope(), id, &a).unwrap();
        assert_eq!(opened.expose_e164(), "+390212345678");
        assert_eq!(opened.country_iso2(), Some("IT"));
    }

    #[test]
    fn tampering_and_misbinding_fail_authentication() {
        let keys = keyring(KeyPurpose::Encryption, "k1", &[("k1", 3)]);
        let id = Uuid::new_v4();
        let sealed = seal(&keys, scope(), id, &number("+390212345678")).unwrap();
        let mut c = sealed.clone();
        c.ciphertext[0] ^= 1;
        assert_eq!(
            open(&keys, scope(), id, &c).unwrap_err(),
            CryptoError::Authentication
        );
        let mut t = sealed.clone();
        let last = t.ciphertext.len() - 1;
        t.ciphertext[last] ^= 1;
        assert_eq!(
            open(&keys, scope(), id, &t).unwrap_err(),
            CryptoError::Authentication
        );
        let mut n = sealed.clone();
        n.nonce[0] ^= 1;
        assert_eq!(
            open(&keys, scope(), id, &n).unwrap_err(),
            CryptoError::Authentication
        );
        let mut tr = sealed.clone();
        tr.ciphertext.truncate(10);
        assert_eq!(
            open(&keys, scope(), id, &tr).unwrap_err(),
            CryptoError::Malformed
        );
        // Moved to another contact or another scope.
        assert_eq!(
            open(&keys, scope(), Uuid::new_v4(), &sealed).unwrap_err(),
            CryptoError::Authentication
        );
        let other = RelayScope::from_row(
            Uuid::new_v4(),
            scope().application_id(),
            scope().environment_id(),
        );
        assert_eq!(
            open(&keys, other, id, &sealed).unwrap_err(),
            CryptoError::Authentication
        );
        // Wrong key under the same version name.
        let wrong = keyring(KeyPurpose::Encryption, "k1", &[("k1", 9)]);
        assert_eq!(
            open(&wrong, scope(), id, &sealed).unwrap_err(),
            CryptoError::Authentication
        );
        // A relabelled version (AAD mismatch) and an unknown version.
        let both = keyring(KeyPurpose::Encryption, "k1", &[("k1", 3), ("k2", 3 + 64)]);
        let mut relabelled = sealed.clone();
        relabelled.key_version = KeyVersion::parse("k2").unwrap();
        assert_eq!(
            open(&both, scope(), id, &relabelled).unwrap_err(),
            CryptoError::Authentication
        );
        let mut unknown = sealed;
        unknown.key_version = KeyVersion::parse("k9").unwrap();
        assert_eq!(
            open(&keys, scope(), id, &unknown).unwrap_err(),
            CryptoError::UnknownKeyVersion
        );
    }

    #[test]
    fn the_aad_and_fingerprint_labels_are_pinned() {
        // Changing either label would orphan every stored ciphertext / fingerprint.
        assert_eq!(AAD_LABEL, "cfmeet-contact-vault-v1");
        assert_eq!(
            crate::contact_vault::fingerprint::DOMAIN,
            b"cfmeet-contact-v1"
        );
        assert_ne!(
            AAD_LABEL.as_bytes(),
            crate::contact_vault::fingerprint::DOMAIN
        );
        let version = KeyVersion::parse("v").unwrap();
        let aad = ContactAad {
            scope: scope(),
            contact_reference: Uuid::parse_str(CONTACT).unwrap(),
            key_version: &version,
        };
        assert!(aad.bytes().starts_with(b"cfmeet-contact-vault-v1|"));
    }

    #[test]
    fn a_ciphertext_moved_to_a_row_differing_in_one_aad_part_fails_authentication() {
        let keys = keyring(KeyPurpose::Encryption, "k1", &[("k1", 3)]);
        let id = Uuid::new_v4();
        let sealed = seal(&keys, scope(), id, &number("+390212345678")).unwrap();
        let s = scope();
        // ONLY the application differs.
        let other_application =
            RelayScope::from_row(s.tenant_id(), Uuid::new_v4(), s.environment_id());
        assert_eq!(
            open(&keys, other_application, id, &sealed).unwrap_err(),
            CryptoError::Authentication
        );
        // ONLY the environment differs.
        let other_environment =
            RelayScope::from_row(s.tenant_id(), s.application_id(), Uuid::new_v4());
        assert_eq!(
            open(&keys, other_environment, id, &sealed).unwrap_err(),
            CryptoError::Authentication
        );
        // ONLY the key version label differs, with the SAME key bytes under the new label.
        let relabelled_keyring = keyring(KeyPurpose::Encryption, "k2", &[("k2", 3)]);
        assert_eq!(
            relabelled_keyring.current_key().1.expose(),
            keys.current_key().1.expose()
        );
        let mut relabelled = sealed.clone();
        relabelled.key_version = KeyVersion::parse("k2").unwrap();
        assert_eq!(
            open(&relabelled_keyring, scope(), id, &relabelled).unwrap_err(),
            CryptoError::Authentication
        );
        // Unchanged, it still opens.
        assert!(open(&keys, scope(), id, &sealed).is_ok());
    }
}
