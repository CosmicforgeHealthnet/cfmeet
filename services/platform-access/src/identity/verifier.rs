//! Keyed credential verifier: HMAC-SHA-256 under a deployment pepper.
//!
//! The database stores `HMAC-SHA-256(pepper_v, message)` plus the algorithm name and the
//! pepper version `v`. Verification recomputes the MAC with the pepper of the persisted
//! version and compares in constant time (`Mac::verify_slice`). A database dump alone
//! therefore cannot be used to test guessed secrets offline: the pepper lives only in
//! the deployment's secret management.
//!
//! Pepper rotation: deploy the new pepper as `API_CREDENTIAL_PEPPER` with a higher
//! `API_CREDENTIAL_PEPPER_VERSION` and keep the old one as
//! `API_CREDENTIAL_PREVIOUS_PEPPER` (+ version) until every credential minted under it
//! has been rotated. New credentials always use the current version.

use std::{collections::BTreeMap, fmt};

use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretSlice};
use sha2::Sha256;

use crate::config::ConfigError;

type HmacSha256 = Hmac<Sha256>;

/// Persisted `verifier_algorithm` value.
pub const VERIFIER_ALGORITHM: &str = "HMAC_SHA256";
/// Length of an HMAC-SHA-256 verifier.
pub const VERIFIER_LEN: usize = 32;

/// Minimum pepper length in bytes.
pub const MIN_PEPPER_LEN: usize = 32;

/// Placeholder fragments that must never become a deployed pepper.
const PLACEHOLDER_FRAGMENTS: &[&str] = &[
    "changeme",
    "change-me",
    "change_me",
    "change-this",
    "changethis",
    "secret123",
    "development-secret",
    "dev-secret",
    "your-",
    "your_",
    "placeholder",
    "example",
    "replace-me",
    "replace_me",
    "password",
    "default",
];

/// A deployment pepper. `Debug` is redacted; memory is zeroized on drop.
pub struct CredentialPepper(SecretSlice<u8>);

impl CredentialPepper {
    /// Validates a pepper read from `var`. Rejects short, low-variety and placeholder
    /// values; there is no fallback pepper in any environment.
    pub fn parse(var: &'static str, raw: &str) -> Result<Self, ConfigError> {
        let value = raw.trim();
        if value.len() < MIN_PEPPER_LEN {
            return Err(ConfigError::invalid(
                var,
                format!("must be at least {MIN_PEPPER_LEN} bytes (generate with `openssl rand -base64 48`)"),
            ));
        }
        let lower = value.to_ascii_lowercase();
        if PLACEHOLDER_FRAGMENTS.iter().any(|p| lower.contains(p)) {
            return Err(ConfigError::invalid(
                var,
                "looks like a placeholder value; generate a random pepper".to_string(),
            ));
        }
        let mut distinct = [false; 256];
        value.bytes().for_each(|b| distinct[b as usize] = true);
        if distinct.iter().filter(|seen| **seen).count() < 12 {
            return Err(ConfigError::invalid(
                var,
                "has too little variety to be a random pepper".to_string(),
            ));
        }
        Ok(Self(SecretSlice::from(value.as_bytes().to_vec())))
    }
}

impl fmt::Debug for CredentialPepper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialPepper([REDACTED])")
    }
}

/// The current pepper (used for new credentials) plus, optionally, previous peppers still
/// accepted for verification during a pepper rotation.
pub struct VerifierKeyring {
    current_version: i32,
    keys: BTreeMap<i32, CredentialPepper>,
}

impl VerifierKeyring {
    pub fn new(
        current_version: i32,
        current: CredentialPepper,
        previous: Option<(i32, CredentialPepper)>,
    ) -> Result<Self, ConfigError> {
        if current_version <= 0 {
            return Err(ConfigError::invalid(
                "API_CREDENTIAL_PEPPER_VERSION",
                "must be a positive integer".to_string(),
            ));
        }
        let mut keys = BTreeMap::new();
        keys.insert(current_version, current);
        if let Some((version, pepper)) = previous {
            if version <= 0 || version == current_version {
                return Err(ConfigError::invalid(
                    "API_CREDENTIAL_PREVIOUS_PEPPER_VERSION",
                    "must be a positive integer different from API_CREDENTIAL_PEPPER_VERSION"
                        .to_string(),
                ));
            }
            keys.insert(version, pepper);
        }
        Ok(Self {
            current_version,
            keys,
        })
    }

    pub fn current_version(&self) -> i32 {
        self.current_version
    }

    pub fn has_version(&self, version: i32) -> bool {
        self.keys.contains_key(&version)
    }

    fn mac(&self, version: i32) -> Option<HmacSha256> {
        let pepper = self.keys.get(&version)?;
        HmacSha256::new_from_slice(pepper.0.expose_secret()).ok()
    }

    /// MAC of `message` under the current pepper.
    pub(crate) fn compute_current(&self, message: &[u8]) -> [u8; VERIFIER_LEN] {
        let mut mac = self
            .mac(self.current_version)
            .expect("current pepper is always present");
        mac.update(message);
        mac.finalize().into_bytes().into()
    }

    /// Constant-time comparison of `expected` with the MAC of `message` under the pepper
    /// of `version`. An unknown version never verifies.
    pub(crate) fn verify(&self, version: i32, message: &[u8], expected: &[u8]) -> bool {
        match self.mac(version) {
            Some(mut mac) => {
                mac.update(message);
                mac.verify_slice(expected).is_ok()
            }
            None => {
                // Spend comparable work so an unknown version is not distinguishable by
                // timing from a wrong secret.
                let mut mac = self
                    .mac(self.current_version)
                    .expect("current pepper is always present");
                mac.update(message);
                let _ = mac.verify_slice(expected);
                false
            }
        }
    }
}

impl fmt::Debug for VerifierKeyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifierKeyring")
            .field("current_version", &self.current_version)
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER_A: &str = "unit-pepper-A-0123456789abcdefghijklmnopqrstuvwxyz";
    const PEPPER_B: &str = "unit-pepper-B-zyxwvutsrqponmlkjihgfedcba9876543210";

    fn keyring(previous: bool) -> VerifierKeyring {
        VerifierKeyring::new(
            2,
            CredentialPepper::parse("P", PEPPER_B).unwrap(),
            previous.then(|| (1, CredentialPepper::parse("P", PEPPER_A).unwrap())),
        )
        .unwrap()
    }

    #[test]
    fn rejects_weak_or_placeholder_peppers() {
        for bad in [
            "",
            "short",
            "changeme-changeme-changeme-changeme-1234",
            "secret123secret123secret123secret123",
            "development-secret-development-secret-x",
            "your-secret-key-change-this-in-production",
            &"a".repeat(64),
            "abababababababababababababababababab",
        ] {
            assert!(
                CredentialPepper::parse("API_CREDENTIAL_PEPPER", bad).is_err(),
                "{bad}"
            );
        }
        assert!(CredentialPepper::parse("API_CREDENTIAL_PEPPER", PEPPER_A).is_ok());
    }

    #[test]
    fn pepper_debug_is_redacted_and_error_does_not_echo_value() {
        let p = CredentialPepper::parse("API_CREDENTIAL_PEPPER", PEPPER_A).unwrap();
        assert!(!format!("{p:?}").contains("unit-pepper"));
        let err = CredentialPepper::parse(
            "API_CREDENTIAL_PEPPER",
            "changeme-changeme-changeme-changeme-1234",
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains("changeme-changeme"), "{err}");
        assert!(!format!("{:?}", keyring(true)).contains("unit-pepper"));
    }

    #[test]
    fn verifies_with_matching_version_only() {
        let ring = keyring(true);
        let mac = ring.compute_current(b"message");
        assert!(ring.verify(2, b"message", &mac));
        assert!(!ring.verify(2, b"other", &mac));
        assert!(!ring.verify(1, b"message", &mac));
        assert!(!ring.verify(99, b"message", &mac));
    }

    #[test]
    fn previous_pepper_still_verifies_during_rotation() {
        let old =
            VerifierKeyring::new(1, CredentialPepper::parse("P", PEPPER_A).unwrap(), None).unwrap();
        let minted_under_v1 = old.compute_current(b"m");
        assert!(keyring(true).verify(1, b"m", &minted_under_v1));
        assert!(!keyring(false).verify(1, b"m", &minted_under_v1));
    }

    #[test]
    fn different_peppers_produce_different_verifiers() {
        let a =
            VerifierKeyring::new(1, CredentialPepper::parse("P", PEPPER_A).unwrap(), None).unwrap();
        let b =
            VerifierKeyring::new(1, CredentialPepper::parse("P", PEPPER_B).unwrap(), None).unwrap();
        assert_ne!(a.compute_current(b"m"), b.compute_current(b"m"));
    }

    #[test]
    fn invalid_versions_are_rejected() {
        assert!(
            VerifierKeyring::new(0, CredentialPepper::parse("P", PEPPER_A).unwrap(), None).is_err()
        );
        assert!(VerifierKeyring::new(
            1,
            CredentialPepper::parse("P", PEPPER_A).unwrap(),
            Some((1, CredentialPepper::parse("P", PEPPER_B).unwrap()))
        )
        .is_err());
    }
}
