//! The protocol profile every PRIVATE_E2EE conversation and device uses.

use openmls::prelude::{Ciphersuite, ProtocolVersion};

/// Protocol identifier persisted with E2EE conversations and devices: MLS 1.0, RFC 9420.
pub const PROTOCOL_ID: &str = "MLS_1_0";

/// The RFC 9420 protocol version of every accepted object.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::Mls10;

/// Name of the single accepted cipher suite (RFC 9420 §17.1, mandatory to implement).
pub const CIPHERSUITE_NAME: &str = "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519";

/// The single accepted cipher suite.
pub const MANDATORY_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Length of an Ed25519 public key (the device signature key of this suite).
pub const SIGNATURE_PUBLIC_KEY_LEN: usize = 32;

/// Whether `name` is the accepted cipher suite (exact match; no aliases).
pub fn is_accepted_ciphersuite(name: &str) -> bool {
    name == CIPHERSUITE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_is_the_rfc_9420_mandatory_suite() {
        assert_eq!(u16::from(MANDATORY_CIPHERSUITE), 0x0001);
        assert!(is_accepted_ciphersuite(CIPHERSUITE_NAME));
        assert!(!is_accepted_ciphersuite(
            "MLS_128_DHKEMP256_AES128GCM_SHA256_P256"
        ));
        assert!(!is_accepted_ciphersuite(""));
    }
}
