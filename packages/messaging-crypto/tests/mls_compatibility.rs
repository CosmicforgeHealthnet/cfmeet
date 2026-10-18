//! Real OpenMLS compatibility evidence (Part V A.4): a client device generates RFC 9420
//! objects and the PUBLIC validator accepts exactly the genuine ones.
#![cfg(feature = "client")]

use std::time::{SystemTime, UNIX_EPOCH};

use cf_messaging_crypto::{
    binding,
    device::Device,
    key_package::{validate_key_package, KeyPackageError, LifetimePolicy},
};
use uuid::Uuid;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn a_device_key_package_validates_and_is_bound_to_its_device_and_key() {
    let id = Uuid::new_v4();
    let device = Device::generate(id).unwrap();
    let encoded = device.key_package().unwrap();
    let v = validate_key_package(&encoded, now(), &LifetimePolicy::default()).unwrap();
    assert_eq!(v.device_id, id);
    assert_eq!(v.signature_key, device.signature_public_key());
    assert_eq!(
        v.credential_identity,
        binding::device_credential_identity(id)
    );
    assert_eq!(v.key_package_ref.len(), 32);
    assert!(!v.last_resort);
    assert_eq!(v.application_id, None);
    // Canonical re-encoding is stable.
    assert_eq!(v.encoded, encoded);
    v.require_device(id, &device.signature_public_key())
        .unwrap();
    // Another device's key or id is a mismatch.
    let other = Device::generate(Uuid::new_v4()).unwrap();
    assert_eq!(
        v.require_device(id, &other.signature_public_key()),
        Err(KeyPackageError::DeviceMismatch)
    );
    assert_eq!(
        v.require_device(other.device_id(), &device.signature_public_key()),
        Err(KeyPackageError::DeviceMismatch)
    );
    // Two KeyPackages of one device have distinct refs (replay detection key).
    let second = validate_key_package(
        &device.key_package().unwrap(),
        now(),
        &LifetimePolicy::default(),
    )
    .unwrap();
    assert_ne!(second.key_package_ref, v.key_package_ref);
}

#[test]
fn the_registration_key_package_carries_the_signed_challenge_binding() {
    let device = Device::generate(Uuid::new_v4()).unwrap();
    let challenge = Uuid::new_v4();
    let nonce = [7u8; binding::NONCE_LEN];
    let encoded = device.registration_key_package(challenge, &nonce).unwrap();
    let v = validate_key_package(&encoded, now(), &LifetimePolicy::default()).unwrap();
    let app = v.application_id.expect("binding present");
    assert_eq!(
        binding::parse_registration_binding(&app),
        Some((challenge, nonce))
    );
}

#[test]
fn any_tampered_byte_breaks_validation_or_changes_nothing_security_relevant() {
    let device = Device::generate(Uuid::new_v4()).unwrap();
    let challenge = Uuid::new_v4();
    let nonce = [9u8; binding::NONCE_LEN];
    let encoded = device.registration_key_package(challenge, &nonce).unwrap();
    let genuine = validate_key_package(&encoded, now(), &LifetimePolicy::default()).unwrap();
    // Flip every byte in turn: the result is either refused, or (for a byte inside an
    // unsigned field such as the signature's own GREASE-free padding — none exist in
    // MLS) identical. Signed content can never change silently.
    for i in 0..encoded.len() {
        let mut forged = encoded.clone();
        forged[i] ^= 0x01;
        if let Ok(v) = validate_key_package(&forged, now(), &LifetimePolicy::default()) {
            assert_eq!(v.signature_key, genuine.signature_key, "byte {i}");
            assert_eq!(
                v.credential_identity, genuine.credential_identity,
                "byte {i}"
            );
            assert_eq!(v.application_id, genuine.application_id, "byte {i}");
            panic!("byte {i}: a modified KeyPackage validated");
        }
    }
}

#[test]
fn lifetime_policy_is_enforced_on_the_validators_clock() {
    let device = Device::generate(Uuid::new_v4()).unwrap();
    let t = now();
    let policy = LifetimePolicy::default();
    // Too long a total range (one year).
    let long = device
        .key_package_with_lifetime(t - 60, t + 365 * 86_400)
        .unwrap();
    assert!(validate_key_package(&long, t, &policy).is_err());
    // Expires within the minimum remaining validity.
    let short = device.key_package_with_lifetime(t - 60, t + 600).unwrap();
    assert_eq!(
        validate_key_package(&short, t, &policy),
        Err(KeyPackageError::LifetimeRejected)
    );
    // Valid now, but the validator's clock is past its expiry (OpenMLS checks the
    // system clock; our policy checks the server clock).
    let ok = device
        .key_package_with_lifetime(t - 60, t + 7 * 86_400)
        .unwrap();
    assert!(validate_key_package(&ok, t, &policy).is_ok());
    assert_eq!(
        validate_key_package(&ok, t + 8 * 86_400, &policy),
        Err(KeyPackageError::LifetimeRejected)
    );
}

#[test]
fn device_debug_output_never_contains_key_material() {
    let device = Device::generate(Uuid::new_v4()).unwrap();
    let text = format!("{device:?}");
    let public: String = device
        .signature_public_key()
        .iter()
        .map(|b| format!("{b}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(!text.contains(&public));
    assert!(text.contains("omitted"));
}
