//! RFC 9420 section 14: local epochs follow canonical acceptance, not submission.
#![cfg(feature = "client")]
use cf_messaging_crypto::{device::Device, group::Processed};
use uuid::Uuid;

#[test]
fn authenticated_member_cannot_claim_another_devices_identity_in_application_aad() {
    use cf_messaging_crypto::binding::{application_message_aad, ApplicationMessageBinding};
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let mut a = alice.create_group(&[83; 32]).unwrap();
    let add = alice
        .add_devices(&mut a, &[bob.key_package().unwrap()])
        .unwrap();
    let mut b = bob.join(add.welcome.as_ref().unwrap()).unwrap();
    let mut binding = ApplicationMessageBinding {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        conversation_id: Uuid::new_v4(),
        client_message_id: Uuid::new_v4(),
        sender_device_id: alice.device_id(),
    };
    let forgery = bob
        .encrypt(
            &mut b,
            &application_message_aad(&binding),
            b"forged attribution",
        )
        .unwrap();
    assert!(alice.process(&mut a, &forgery).is_err());
    binding.sender_device_id = bob.device_id();
    binding.client_message_id = Uuid::new_v4();
    let honest = bob
        .encrypt(
            &mut b,
            &application_message_aad(&binding),
            b"authenticated attribution",
        )
        .unwrap();
    assert!(matches!(
        alice.process(&mut a, &honest).unwrap(),
        Processed::Application { .. }
    ));
}

#[test]
fn refused_membership_commit_cannot_advance_or_encrypt_from_an_unaccepted_epoch() {
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let mut a = alice.create_group(&[81; 32]).unwrap();
    let package = bob.key_package().unwrap();
    alice
        .prepare_add_devices(&mut a, std::slice::from_ref(&package))
        .unwrap();
    assert_eq!(a.epoch(), 0);
    assert!(alice
        .encrypt(&mut a, b"aad", b"not yet authorized")
        .is_err());
    alice.discard_pending_commit(&mut a).unwrap();
    assert_eq!(a.epoch(), 0);
    assert_eq!(a.member_devices(), vec![alice.device_id()]);
    let accepted = alice.prepare_add_devices(&mut a, &[package]).unwrap();
    assert_eq!(a.epoch(), 0);
    alice.accept_pending_commit(&mut a).unwrap();
    assert_eq!(a.epoch(), 1);
    assert!(alice.accept_pending_commit(&mut a).is_err());
    let mut b = bob.join(accepted.welcome.as_ref().unwrap()).unwrap();
    let bytes = alice
        .encrypt(&mut a, b"aad", b"authorized after acceptance")
        .unwrap();
    assert!(matches!(
        bob.process(&mut b, &bytes).unwrap(),
        Processed::Application { .. }
    ));
}

#[test]
fn pending_state_survives_client_restart_and_competing_commit_recovery() {
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let mut a = alice.create_group(&[82; 32]).unwrap();
    let add = alice
        .add_devices(&mut a, &[bob.key_package().unwrap()])
        .unwrap();
    let mut b = bob.join(add.welcome.as_ref().unwrap()).unwrap();
    alice.prepare_update_keys(&mut a).unwrap();
    let saved = alice.export_state();
    let restored = Device::import_state(alice.device_id(), &saved).unwrap();
    let mut a = restored.load_group(&[82; 32]).unwrap().unwrap();
    assert_eq!(a.epoch(), 1);
    assert!(restored.encrypt(&mut a, b"aad", b"pending update").is_err());
    let winner = bob.prepare_update_keys(&mut b).unwrap();
    bob.accept_pending_commit(&mut b).unwrap();
    restored.discard_pending_commit(&mut a).unwrap();
    assert!(matches!(
        restored.process(&mut a, &winner.commit).unwrap(),
        Processed::CommitMerged { epoch: 2 }
    ));
    let message = bob.encrypt(&mut b, b"aad", b"after recovery").unwrap();
    assert!(matches!(
        restored.process(&mut a, &message).unwrap(),
        Processed::Application { .. }
    ));
    let remove = restored
        .prepare_remove_devices(&mut a, &[bob.device_id()])
        .unwrap();
    assert_eq!(a.epoch(), 2);
    assert_eq!(remove.epoch, 2);
    restored.discard_pending_commit(&mut a).unwrap();
    assert_eq!(a.member_devices().len(), 2);
}
