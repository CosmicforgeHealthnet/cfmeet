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

/// Part V D.11 evidence: the client-known AAD is authenticated by MLS end to end, in a
/// real two-device group bound to a server-assigned 32-byte group id.
#[test]
fn mls_authenticates_the_application_message_aad_between_two_devices() {
    use cf_messaging_crypto::profile::MANDATORY_CIPHERSUITE;
    use openmls::prelude::{
        tls_codec::{Deserialize as _, Serialize as _},
        BasicCredential, CredentialWithKey, GroupId, KeyPackage, MlsGroup, MlsGroupCreateConfig,
        MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn, ProcessedMessageContent,
        ProtocolMessage, StagedWelcome,
    };
    use openmls_basic_credential::SignatureKeyPair;
    use openmls_rust_crypto::OpenMlsRustCrypto;

    let party = |device: Uuid| {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(MANDATORY_CIPHERSUITE.signature_algorithm()).unwrap();
        let credential = CredentialWithKey {
            credential: BasicCredential::new(binding::device_credential_identity(device)).into(),
            signature_key: signer.to_public_vec().into(),
        };
        (provider, signer, credential)
    };
    let (a_provider, a_signer, a_cred) = party(Uuid::new_v4());
    let (b_provider, b_signer, b_cred) = party(Uuid::new_v4());
    // Bob publishes a KeyPackage; the server validates it like any other.
    let bob_kp = KeyPackage::builder()
        .build(MANDATORY_CIPHERSUITE, &b_provider, &b_signer, b_cred)
        .unwrap();
    let encoded = openmls::prelude::MlsMessageOut::from(bob_kp.key_package().clone())
        .tls_serialize_detached()
        .unwrap();
    validate_key_package(&encoded, now(), &LifetimePolicy::default()).unwrap();
    // Alice creates the group with the conversation's server-assigned group id.
    let group_id = [0x5au8; 32];
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(MANDATORY_CIPHERSUITE)
        .use_ratchet_tree_extension(true)
        .build();
    let mut alice = MlsGroup::new_with_group_id(
        &a_provider,
        &a_signer,
        &config,
        GroupId::from_slice(&group_id),
        a_cred,
    )
    .unwrap();
    let (_commit, welcome, _) = alice
        .add_members(&a_provider, &a_signer, &[bob_kp.key_package().clone()])
        .unwrap();
    alice.merge_pending_commit(&a_provider).unwrap();
    let welcome_bytes = welcome.tls_serialize_detached().unwrap();
    let MlsMessageBodyIn::Welcome(welcome) = MlsMessageIn::tls_deserialize_exact(&welcome_bytes)
        .unwrap()
        .extract()
    else {
        panic!("not a welcome")
    };
    let mut bob =
        StagedWelcome::new_from_welcome(&b_provider, &MlsGroupJoinConfig::default(), welcome, None)
            .unwrap()
            .into_group(&b_provider)
            .unwrap();
    assert_eq!(bob.group_id().as_slice(), group_id);

    let bound = binding::ApplicationMessageBinding {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        conversation_id: Uuid::new_v4(),
        client_message_id: Uuid::new_v4(),
        sender_device_id: Uuid::new_v4(),
    };
    let aad = binding::application_message_aad(&bound);
    let send = |alice: &mut MlsGroup, text: &[u8]| -> Vec<u8> {
        alice.set_aad(aad.clone());
        alice
            .create_message(&a_provider, &a_signer, text)
            .unwrap()
            .tls_serialize_detached()
            .unwrap()
    };
    let receive = |bob: &mut MlsGroup, bytes: &[u8]| {
        let message = MlsMessageIn::tls_deserialize_exact(bytes).map_err(|_| ())?;
        let protocol: ProtocolMessage = message.try_into_protocol_message().map_err(|_| ())?;
        bob.process_message(&b_provider, protocol).map_err(|_| ())
    };
    let genuine = send(&mut alice, b"hello bob");
    // The plaintext never appears in what the server relays.
    assert!(!genuine.windows(9).any(|w| w == b"hello bob"));
    let processed = receive(&mut bob, &genuine).unwrap();
    assert_eq!(processed.aad(), aad.as_slice());
    assert_eq!(
        binding::parse_application_message_aad(processed.aad()),
        Some(bound)
    );
    let ProcessedMessageContent::ApplicationMessage(app) = processed.into_content() else {
        panic!("not an application message")
    };
    assert_eq!(app.into_bytes(), b"hello bob");
    // A server (or anyone) rewriting the AAD is detected by the recipient: re-attributing
    // the message to another device, transplanting it into another tenant (same length,
    // still a well-formed AAD) or relabelling its security profile.
    let tamper = |bob: &mut MlsGroup, alice: &mut MlsGroup, edit: &dyn Fn(&str) -> String| {
        let next = send(alice, b"next");
        let at = next
            .windows(aad.len())
            .position(|w| w == aad.as_slice())
            .expect("the AAD travels in the clear (authenticated, not encrypted)");
        let edited = edit(std::str::from_utf8(&aad).unwrap()).into_bytes();
        assert_eq!(edited.len(), aad.len(), "same-length rewrite");
        assert_ne!(edited, aad);
        let mut forged = next.clone();
        forged[at..at + aad.len()].copy_from_slice(&edited);
        assert!(
            receive(bob, &forged).is_err(),
            "a rewritten AAD was accepted"
        );
        // OpenMLS consumes the generation's key on the failed attempt, so the genuine
        // copy of THIS message can no longer be decrypted (an injected forgery is a
        // denial of service, never a confidentiality or integrity break), but the group
        // is not desynchronised: the next message decrypts.
        assert!(receive(bob, &next).is_err());
        let after = send(alice, b"after");
        assert!(receive(bob, &after).is_ok());
    };
    let other_tenant = Uuid::new_v4().hyphenated().to_string();
    let tenant = bound.tenant_id.hyphenated().to_string();
    tamper(&mut bob, &mut alice, &|a| a.replace(&tenant, &other_tenant));
    let device = bound.sender_device_id.hyphenated().to_string();
    let other_device = Uuid::new_v4().hyphenated().to_string();
    tamper(&mut bob, &mut alice, &|a| a.replace(&device, &other_device));
    tamper(&mut bob, &mut alice, &|a| {
        a.replace("PRIVATE_E2EE", "MANAGED_E2EE")
    });
}

/// Part V E/F evidence: the client group wrapper and the PUBLIC framing inspector the
/// Delivery Service uses. A removed device cannot read anything sent after the removal
/// commit, and the server-side inspector sees only group id, epoch and content type.
#[test]
fn group_lifecycle_with_removal_and_public_framing() {
    use cf_messaging_crypto::{
        framing::{inspect, is_welcome, FramedContent, FramedWireFormat},
        group::Processed,
    };
    let (a, b, c) = (
        Device::generate(Uuid::new_v4()).unwrap(),
        Device::generate(Uuid::new_v4()).unwrap(),
        Device::generate(Uuid::new_v4()).unwrap(),
    );
    let gid = [0x42u8; 32];
    let mut ga = a.create_group(&gid).unwrap();
    assert_eq!(ga.epoch(), 0);
    let add = a
        .add_devices(
            &mut ga,
            &[b.key_package().unwrap(), c.key_package().unwrap()],
        )
        .unwrap();
    assert_eq!(add.epoch, 0);
    assert_eq!(ga.epoch(), 1);
    let welcome = add.welcome.unwrap();
    assert!(is_welcome(&welcome));
    let f = inspect(&add.commit).unwrap();
    assert_eq!(
        (f.group_id.as_slice(), f.epoch, f.content),
        (&gid[..], 0, FramedContent::Commit)
    );
    let mut gb = b.join(&welcome).unwrap();
    let mut gc = c.join(&welcome).unwrap();
    assert_eq!(gb.epoch(), 1);
    let mut expected = vec![a.device_id(), b.device_id(), c.device_id()];
    expected.sort();
    assert_eq!(gb.member_devices(), expected);

    let m1 = a.encrypt(&mut ga, b"aad-1", b"hello all").unwrap();
    let f = inspect(&m1).unwrap();
    assert_eq!(f.content, FramedContent::Application);
    assert_eq!(f.wire_format, FramedWireFormat::Private);
    assert_eq!(f.epoch, 1);
    assert!(!m1.windows(9).any(|w| w == b"hello all"));
    for (dev, g) in [(&b, &mut gb), (&c, &mut gc)] {
        assert_eq!(
            dev.process(g, &m1).unwrap(),
            Processed::Application {
                aad: b"aad-1".to_vec(),
                plaintext: b"hello all".to_vec()
            }
        );
    }
    // Remove C.
    let rm = a.remove_devices(&mut ga, &[c.device_id()]).unwrap();
    assert_eq!(rm.epoch, 1);
    assert_eq!(inspect(&rm.commit).unwrap().content, FramedContent::Commit);
    assert_eq!(
        b.process(&mut gb, &rm.commit).unwrap(),
        Processed::CommitMerged { epoch: 2 }
    );
    // C processes its own removal and can decrypt nothing afterwards.
    let _ = c.process(&mut gc, &rm.commit);
    let m2 = a.encrypt(&mut ga, b"aad-2", b"after removal").unwrap();
    assert_eq!(inspect(&m2).unwrap().epoch, 2);
    assert!(matches!(
        b.process(&mut gb, &m2).unwrap(),
        Processed::Application { .. }
    ));
    assert!(
        c.process(&mut gc, &m2).is_err(),
        "a removed device decrypted a later message"
    );
    assert!(!gb.member_devices().contains(&c.device_id()));
    assert!(a.remove_devices(&mut ga, &[Uuid::new_v4()]).is_err());
}
