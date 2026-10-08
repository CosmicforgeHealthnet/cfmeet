//! Privacy-preserving equality (§32): a keyed, domain-separated, SCOPED fingerprint.
//!
//! ```text
//! fingerprint = HMAC-SHA-256(fingerprint_key[version],
//!     "cfmeet-contact-v1" 0x1f tenant_id 0x1f application_id 0x1f environment_id 0x1f e164)
//! ```
//!
//! * Keyed (never a plain hash): the telephone-number space is small enough to
//!   enumerate, so without the separate fingerprint key the database alone reveals
//!   nothing.
//! * Scoped: tenant / application / environment are inside the MAC, so the same number
//!   in two scopes yields unrelated fingerprints — there is no global equality identity
//!   and no cross-tenant existence signal.
//! * Versioned: `contact_fingerprints` stores (key version, fingerprint) per contact;
//!   lookups match every readable version, so a fingerprint-key rotation (add → backfill
//!   → activate → prune) never causes a dedup outage.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::{
    keyring::{ContactKeyProvider, KeyVersion},
    normalization::CanonicalNumber,
};
use crate::scope::RelayScope;

const DOMAIN: &[u8] = b"cfmeet-contact-v1";
const SEPARATOR: u8 = 0x1f;

/// One (key version, fingerprint) of a number in a scope.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub key_version: KeyVersion,
    pub value: [u8; 32],
}

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fingerprint")
            .field("key_version", &self.key_version)
            .finish_non_exhaustive()
    }
}

fn mac(key: &[u8; 32], scope: RelayScope, number: &CanonicalNumber) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(DOMAIN);
    for part in [
        scope.tenant_id().to_string(),
        scope.application_id().to_string(),
        scope.environment_id().to_string(),
    ] {
        m.update(&[SEPARATOR]);
        m.update(part.as_bytes());
    }
    m.update(&[SEPARATOR]);
    m.update(number.expose_e164().as_bytes());
    m.finalize().into_bytes().into()
}

/// The fingerprint under one readable key version.
pub fn under(
    keys: &dyn ContactKeyProvider,
    version: &KeyVersion,
    scope: RelayScope,
    number: &CanonicalNumber,
) -> Option<Fingerprint> {
    keys.key_by_version(version).map(|key| Fingerprint {
        key_version: version.clone(),
        value: mac(key.expose(), scope, number),
    })
}

/// The fingerprints under EVERY readable version (active first): what a new contact
/// stores and what an equality lookup matches during a rotation overlap.
pub fn all(
    keys: &dyn ContactKeyProvider,
    scope: RelayScope,
    number: &CanonicalNumber,
) -> Vec<Fingerprint> {
    keys.versions()
        .iter()
        .filter_map(|v| under(keys, v, scope, number))
        .collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::contact_vault::{
        keyring::{tests::keyring, KeyPurpose},
        normalization::{normalize, RawContactValue},
    };

    fn scope(t: u128) -> RelayScope {
        RelayScope::from_row(Uuid::from_u128(t), Uuid::from_u128(2), Uuid::from_u128(3))
    }

    fn n(raw: &str) -> CanonicalNumber {
        normalize(&RawContactValue::new(raw), None).unwrap()
    }

    #[test]
    fn fingerprints_are_keyed_scoped_and_stable() {
        let keys = keyring(KeyPurpose::Fingerprint, "f1", &[("f1", 5)]);
        let v = KeyVersion::parse("f1").unwrap();
        let a = under(&keys, &v, scope(1), &n("+39 02 1234 5678")).unwrap();
        // Same number, same scope, different formatting: equal (canonical input).
        assert_eq!(a, under(&keys, &v, scope(1), &n("+390212345678")).unwrap());
        // Another tenant: unrelated.
        assert_ne!(
            a.value,
            under(&keys, &v, scope(9), &n("+390212345678"))
                .unwrap()
                .value
        );
        // Another number: different.
        assert_ne!(
            a.value,
            under(&keys, &v, scope(1), &n("+44 20 7946 0958"))
                .unwrap()
                .value
        );
        // Another key: unrelated (an unkeyed SHA-256 of the number is never produced).
        let other = keyring(KeyPurpose::Fingerprint, "f1", &[("f1", 77)]);
        assert_ne!(
            a.value,
            under(&other, &v, scope(1), &n("+390212345678"))
                .unwrap()
                .value
        );
        use sha2::Digest;
        let plain: [u8; 32] = Sha256::digest(b"+390212345678").into();
        assert_ne!(a.value, plain);
        assert!(!format!("{a:?}").contains(&hex::encode(a.value)));
    }

    #[test]
    fn every_readable_version_is_produced_active_first() {
        let keys = keyring(KeyPurpose::Fingerprint, "f2", &[("f1", 5), ("f2", 50)]);
        let all = all(&keys, scope(1), &n("+390212345678"));
        assert_eq!(
            all.iter()
                .map(|f| f.key_version.as_str())
                .collect::<Vec<_>>(),
            ["f2", "f1"]
        );
        assert_ne!(all[0].value, all[1].value);
    }
}
