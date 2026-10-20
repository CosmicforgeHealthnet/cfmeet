//! Durable device state (Part V Appendix L): a device exported and imported again keeps
//! its identity, its unused KeyPackage private keys and every MLS group, continues the
//! conversation and still refuses replays; anything malformed or foreign is refused.
#![cfg(feature = "client")]

use cf_messaging_crypto::{
    device::{Device, DeviceError},
    group::Processed,
};
use uuid::Uuid;

fn text(p: Processed) -> String {
    match p {
        Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
        other => panic!("expected an application message, got {other:?}"),
    }
}

#[test]
fn an_imported_device_continues_its_groups_and_refuses_replays() {
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let gid = [7u8; 32];
    let mut ga = alice.create_group(&gid).unwrap();
    let add = alice
        .add_devices(&mut ga, &[bob.key_package().unwrap()])
        .unwrap();
    let mut gb = bob.join(add.welcome.as_ref().unwrap()).unwrap();
    let m1 = alice.encrypt(&mut ga, b"aad-1", b"before export").unwrap();
    assert_eq!(text(bob.process(&mut gb, &m1).unwrap()), "before export");

    // Bob's device is persisted and the process ends.
    let state = bob.export_state();
    let bob_id = bob.device_id();
    let bob_key = bob.signature_public_key();
    drop(gb);
    drop(bob);

    let bob = Device::import_state(bob_id, &state).unwrap();
    assert_eq!(bob.device_id(), bob_id);
    assert_eq!(bob.signature_public_key(), bob_key);
    let mut gb = bob.load_group(&gid).unwrap().expect("the group survives");
    assert_eq!(gb.epoch(), ga.epoch());
    assert!(bob.load_group(&[9u8; 32]).unwrap().is_none());
    // Both directions keep working after the restore.
    let m2 = alice.encrypt(&mut ga, b"aad-2", b"after import").unwrap();
    assert_eq!(text(bob.process(&mut gb, &m2).unwrap()), "after import");
    let m3 = bob.encrypt(&mut gb, b"aad-3", b"reply").unwrap();
    assert_eq!(text(alice.process(&mut ga, &m3).unwrap()), "reply");
    // A message processed before the export is still a replay afterwards.
    assert!(
        bob.process(&mut gb, &m1).is_err(),
        "replay refused after restore"
    );
    // Epoch changes are persisted too.
    let carol = Device::generate(Uuid::new_v4()).unwrap();
    let add = alice
        .add_devices(&mut ga, &[carol.key_package().unwrap()])
        .unwrap();
    assert!(matches!(
        bob.process(&mut gb, &add.commit).unwrap(),
        Processed::CommitMerged { .. }
    ));
    let state = bob.export_state();
    let bob = Device::import_state(bob_id, &state).unwrap();
    assert_eq!(bob.load_group(&gid).unwrap().unwrap().epoch(), ga.epoch());
}

#[test]
fn a_keypackage_published_before_export_can_still_be_welcomed() {
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let kp = bob.key_package().unwrap();
    let state = bob.export_state();
    let id = bob.device_id();
    drop(bob);
    let bob = Device::import_state(id, &state).unwrap();
    let mut ga = alice.create_group(&[1u8; 32]).unwrap();
    let add = alice.add_devices(&mut ga, &[kp]).unwrap();
    let mut gb = bob.join(add.welcome.as_ref().unwrap()).unwrap();
    let m = alice
        .encrypt(&mut ga, b"aad", b"welcome after restore")
        .unwrap();
    assert_eq!(
        text(bob.process(&mut gb, &m).unwrap()),
        "welcome after restore"
    );
}

#[test]
fn malformed_or_foreign_states_are_refused() {
    let a = Device::generate(Uuid::new_v4()).unwrap();
    let b = Device::generate(Uuid::new_v4()).unwrap();
    a.create_group(&[3u8; 32]).unwrap();
    let state = a.export_state().to_vec();
    let invalid = |r: Result<Device, DeviceError>| matches!(r, Err(DeviceError::InvalidState));
    assert!(
        invalid(Device::import_state(b.device_id(), &state)),
        "another device's id"
    );
    assert!(
        invalid(Device::import_state(
            a.device_id(),
            &state[..state.len() - 1]
        )),
        "truncated"
    );
    let mut trailing = state.clone();
    trailing.push(0);
    assert!(
        invalid(Device::import_state(a.device_id(), &trailing)),
        "trailing bytes"
    );
    let mut version = state.clone();
    version[7] ^= 0x01;
    assert!(
        invalid(Device::import_state(a.device_id(), &version)),
        "unknown version"
    );
    assert!(invalid(Device::import_state(a.device_id(), b"")), "empty");
    // A public key that does not belong to the private key (substitution). The key pair
    // is TLS-encoded after magic and id: [len u32][0x20 priv*32][0x20 pub*32][scheme u16].
    let other = b.export_state().to_vec();
    let at = 8 + 16 + 4;
    assert_eq!(
        (state[at], state[at + 33]),
        (0x20, 0x20),
        "Ed25519 key pair layout"
    );
    let mut swapped = state.clone();
    swapped[at + 34..at + 66].copy_from_slice(&other[at + 34..at + 66]);
    assert!(
        invalid(Device::import_state(a.device_id(), &swapped)),
        "mismatched key pair"
    );
    // The export holds secrets but never the device's public key alone: sanity of size.
    assert!(state.len() > 64);
    // A good state still imports.
    assert!(Device::import_state(a.device_id(), &state).is_ok());
}
