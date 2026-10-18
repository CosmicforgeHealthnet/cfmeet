//! Validation of published MLS KeyPackages — PUBLIC material only (server-safe).
//!
//! [`validate_key_package`] accepts exactly one RFC 9420 `MLSMessage` with wire format
//! `mls_key_package` and checks, in order:
//!
//! 1. size bound and exact TLS decoding (no trailing bytes);
//! 2. the accepted cipher suite ([`crate::profile::MANDATORY_CIPHERSUITE`]);
//! 3. OpenMLS `KeyPackageIn::validate` (RFC 9420 §10.1): MLS 1.0, leaf-node signature,
//!    KeyPackage signature (both by the leaf signature key), init key ≠ encryption key,
//!    extension support, a lifetime that is valid now;
//! 4. our own lifetime policy on the server's clock: a bounded total range, no
//!    `not_before` in the future beyond the clock-skew allowance and a minimum remaining
//!    validity;
//! 5. a `BasicCredential` whose identity is a canonical device identity
//!    ([`crate::binding`]) and an Ed25519 signature key of the right length.
//!
//! The result carries only public facts (signature key, credential identity,
//! KeyPackageRef, lifetime, application id). Callers then bind them to a device:
//! [`ValidatedKeyPackage::require_device`].

use openmls::prelude::{
    tls_codec::{Deserialize as _, Serialize as _},
    BasicCredential, CredentialType, KeyPackageIn, MlsMessageBodyIn, MlsMessageIn,
};
use openmls_rust_crypto::RustCrypto;
use uuid::Uuid;

use crate::{
    binding,
    profile::{MANDATORY_CIPHERSUITE, PROTOCOL_VERSION, SIGNATURE_PUBLIC_KEY_LEN},
};

/// Hard upper bound of an encoded KeyPackage message (bytes).
pub const MAX_KEY_PACKAGE_BYTES: usize = 16 * 1024;

/// Lifetime policy applied on the validating party's clock (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifetimePolicy {
    /// Longest accepted `not_after - not_before`.
    pub max_total_seconds: u64,
    /// `not_before` may be at most this far in the future (clock skew).
    pub max_future_skew_seconds: u64,
    /// `not_after` must be at least this far in the future.
    pub min_remaining_seconds: u64,
}

impl Default for LifetimePolicy {
    /// 90 days of total range (+1 h skew margin, the OpenMLS default range), 5 minutes of
    /// skew and at least 1 hour of remaining validity.
    fn default() -> Self {
        Self {
            max_total_seconds: 60 * 60 * 24 * 90 + 60 * 60,
            max_future_skew_seconds: 300,
            min_remaining_seconds: 3_600,
        }
    }
}

/// Why a KeyPackage was refused. Stable codes ([`KeyPackageError::code`]); the messages
/// never echo the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyPackageError {
    #[error("the KeyPackage exceeds {MAX_KEY_PACKAGE_BYTES} bytes")]
    TooLarge,
    #[error("the bytes are not exactly one MLS KeyPackage message")]
    Malformed,
    #[error("the KeyPackage uses a cipher suite other than the accepted one")]
    UnsupportedCiphersuite,
    #[error("the KeyPackage failed RFC 9420 validation")]
    InvalidSignatureOrStructure,
    #[error("the KeyPackage lifetime is not acceptable")]
    LifetimeRejected,
    #[error("the KeyPackage credential is not a CF Meet device credential")]
    CredentialRejected,
    #[error("the KeyPackage is bound to another device or key")]
    DeviceMismatch,
    #[error("last-resort KeyPackages are not accepted")]
    LastResortRejected,
}

impl KeyPackageError {
    pub fn code(&self) -> &'static str {
        match self {
            KeyPackageError::TooLarge => "KEY_PACKAGE_TOO_LARGE",
            KeyPackageError::Malformed => "KEY_PACKAGE_MALFORMED",
            KeyPackageError::UnsupportedCiphersuite => "UNSUPPORTED_CIPHERSUITE",
            KeyPackageError::InvalidSignatureOrStructure => "KEY_PACKAGE_INVALID",
            KeyPackageError::LifetimeRejected => "KEY_PACKAGE_LIFETIME_REJECTED",
            KeyPackageError::CredentialRejected => "CREDENTIAL_REJECTED",
            KeyPackageError::DeviceMismatch => "DEVICE_MISMATCH",
            KeyPackageError::LastResortRejected => "LAST_RESORT_REJECTED",
        }
    }
}

/// Public facts of a KeyPackage that passed [`validate_key_package`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedKeyPackage {
    /// The device the credential names.
    pub device_id: Uuid,
    /// The leaf signature public key (Ed25519, 32 bytes).
    pub signature_key: Vec<u8>,
    /// The `BasicCredential` identity bytes.
    pub credential_identity: Vec<u8>,
    /// RFC 9420 §5.2 KeyPackageRef (`MakeKeyPackageRef`): the KeyPackage's unique id.
    pub key_package_ref: Vec<u8>,
    pub not_before: u64,
    pub not_after: u64,
    /// The leaf-node `application_id` extension, if any.
    pub application_id: Option<Vec<u8>>,
    pub last_resort: bool,
    /// The canonical encoding (re-serialized `MLSMessage`), what is stored and served.
    pub encoded: Vec<u8>,
}

impl ValidatedKeyPackage {
    /// Requires the KeyPackage to belong to `device_id` with `signature_key`.
    pub fn require_device(
        &self,
        device_id: Uuid,
        signature_key: &[u8],
    ) -> Result<(), KeyPackageError> {
        if self.device_id == device_id && self.signature_key == signature_key {
            Ok(())
        } else {
            Err(KeyPackageError::DeviceMismatch)
        }
    }
}

/// Validates one encoded KeyPackage message at Unix time `now`.
pub fn validate_key_package(
    encoded: &[u8],
    now: u64,
    policy: &LifetimePolicy,
) -> Result<ValidatedKeyPackage, KeyPackageError> {
    if encoded.len() > MAX_KEY_PACKAGE_BYTES {
        return Err(KeyPackageError::TooLarge);
    }
    let message =
        MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| KeyPackageError::Malformed)?;
    let key_package_in: KeyPackageIn = match message.extract() {
        MlsMessageBodyIn::KeyPackage(kp) => kp,
        _ => return Err(KeyPackageError::Malformed),
    };
    let crypto = RustCrypto::default();
    let key_package = key_package_in
        .validate(&crypto, PROTOCOL_VERSION)
        .map_err(|_| KeyPackageError::InvalidSignatureOrStructure)?;
    // Checked after validation as well: validate() verifies with the suite the package
    // names, so the suite gate must hold on the verified object.
    if key_package.ciphersuite() != MANDATORY_CIPHERSUITE {
        return Err(KeyPackageError::UnsupportedCiphersuite);
    }
    let lifetime = key_package.life_time();
    let (not_before, not_after) = (lifetime.not_before(), lifetime.not_after());
    if not_after <= not_before
        || not_after - not_before > policy.max_total_seconds
        || not_before > now.saturating_add(policy.max_future_skew_seconds)
        || not_after < now.saturating_add(policy.min_remaining_seconds)
    {
        return Err(KeyPackageError::LifetimeRejected);
    }
    let leaf = key_package.leaf_node();
    let credential = leaf.credential();
    if credential.credential_type() != CredentialType::Basic {
        return Err(KeyPackageError::CredentialRejected);
    }
    let basic = BasicCredential::try_from(credential.clone())
        .map_err(|_| KeyPackageError::CredentialRejected)?;
    let device_id = binding::parse_device_credential_identity(basic.identity())
        .ok_or(KeyPackageError::CredentialRejected)?;
    let signature_key = leaf.signature_key().as_slice().to_vec();
    if signature_key.len() != SIGNATURE_PUBLIC_KEY_LEN {
        return Err(KeyPackageError::CredentialRejected);
    }
    let key_package_ref = key_package
        .hash_ref(&crypto)
        .map_err(|_| KeyPackageError::InvalidSignatureOrStructure)?
        .as_slice()
        .to_vec();
    let application_id = leaf
        .extensions()
        .application_id()
        .map(|a| a.as_slice().to_vec());
    let last_resort = key_package.last_resort();
    let encoded = openmls::prelude::MlsMessageOut::from(key_package)
        .tls_serialize_detached()
        .map_err(|_| KeyPackageError::Malformed)?;
    Ok(ValidatedKeyPackage {
        device_id,
        signature_key,
        credential_identity: basic.identity().to_vec(),
        key_package_ref,
        not_before,
        not_after,
        application_id,
        last_resort,
        encoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_and_oversized_input_is_refused_without_panicking() {
        let p = LifetimePolicy::default();
        assert_eq!(
            validate_key_package(&[], 0, &p),
            Err(KeyPackageError::Malformed)
        );
        assert_eq!(
            validate_key_package(b"{\"keyPackage\":\"trust me\"}", 0, &p),
            Err(KeyPackageError::Malformed)
        );
        assert_eq!(
            validate_key_package(&vec![0u8; MAX_KEY_PACKAGE_BYTES + 1], 0, &p),
            Err(KeyPackageError::TooLarge)
        );
        // Deterministic pseudo-random fuzzing of the decoder.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for len in 0..400usize {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            assert!(validate_key_package(&bytes, 0, &p).is_err());
        }
    }
}
