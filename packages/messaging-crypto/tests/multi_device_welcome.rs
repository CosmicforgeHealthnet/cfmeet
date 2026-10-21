//! A device that joined a group by Welcome can itself add devices: the Welcomes it sends
//! carry the ratchet tree, exactly like the creator's. (Regression: joined devices used
//! OpenMLS's default join configuration, whose Welcomes omit the tree, so a new device
//! added by any member other than the creator could never join.)
#![cfg(feature = "client")]

use cf_messaging_crypto::{device::Device, group::Processed};
use uuid::Uuid;

fn text(p: Processed) -> String {
    match p {
        Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
        other => panic!("expected an application message, got {other:?}"),
    }
}

#[test]
fn a_member_that_joined_by_welcome_can_add_a_new_device() {
    let gid = [5u8; 32];
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let bob_second = Device::generate(Uuid::new_v4()).unwrap();
    let mut ga = alice.create_group(&gid).unwrap();
    let add = alice
        .add_devices(&mut ga, &[bob.key_package().unwrap()])
        .unwrap();
    let mut gb = bob.join(add.welcome.as_ref().unwrap()).unwrap();

    // Bob (a Welcome joiner) adds his second device.
    let add = bob
        .add_devices(&mut gb, &[bob_second.key_package().unwrap()])
        .unwrap();
    assert!(matches!(
        alice.process(&mut ga, &add.commit).unwrap(),
        Processed::CommitMerged { .. }
    ));
    let g2 = bob_second
        .join(add.welcome.as_ref().unwrap())
        .expect("a Welcome from a joined member carries the ratchet tree");
    assert_eq!(g2.epoch(), ga.epoch());

    // ...and so does every later joiner, after a restore too.
    let state = bob_second.export_state();
    let id = bob_second.device_id();
    drop(g2);
    drop(bob_second);
    let bob_second = Device::import_state(id, &state).unwrap();
    let mut g2 = bob_second.load_group(&gid).unwrap().unwrap();
    let carol = Device::generate(Uuid::new_v4()).unwrap();
    let add = bob_second
        .add_devices(&mut g2, &[carol.key_package().unwrap()])
        .unwrap();
    for (d, g) in [(&alice, &mut ga), (&bob, &mut gb)] {
        assert!(matches!(
            d.process(g, &add.commit).unwrap(),
            Processed::CommitMerged { .. }
        ));
    }
    let mut gc = carol.join(add.welcome.as_ref().unwrap()).unwrap();

    let m = carol.encrypt(&mut gc, b"aad", b"hello from carol").unwrap();
    for (d, g) in [(&alice, &mut ga), (&bob, &mut gb), (&bob_second, &mut g2)] {
        assert_eq!(text(d.process(g, &m).unwrap()), "hello from carol");
    }
}
