//! A member's device joins a group BY ITSELF from the group's GroupInfo (an MLS external
//! commit, RFC 9420 §12.4.3.2): no other member has to be online. Every member merges the
//! commit only for a joining device its joiner policy verified (a registered device of a
//! member principal, with exactly that key), and an external commit can never evict
//! another device.
#![cfg(feature = "client")]

use cf_messaging_crypto::{
    device::Device,
    framing::{self, FramedContent, FramedWireFormat},
    group::{GroupError, Processed},
};
use uuid::Uuid;

fn text(p: Processed) -> String {
    match p {
        Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
        other => panic!("expected an application message, got {other:?}"),
    }
}

#[test]
fn a_new_device_joins_from_the_group_info_without_any_member_online() {
    let gid = [7u8; 32];
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob_phone = Device::generate(Uuid::new_v4()).unwrap();
    let mut ga = alice.create_group(&gid).unwrap();
    let add = alice
        .add_devices(&mut ga, &[bob_phone.key_package().unwrap()])
        .unwrap();
    let mut gp = bob_phone.join(add.welcome.as_ref().unwrap()).unwrap();

    // The GroupInfo Alice publishes with her commit (public material only).
    let info = alice.export_group_info(&ga).unwrap();
    let header = framing::inspect_group_info(&info).unwrap();
    assert_eq!(header.group_id, gid.to_vec());
    assert_eq!(header.epoch, ga.epoch());
    let mut tree = header.member_devices.clone().unwrap();
    tree.sort();
    assert_eq!(
        tree,
        ga.member_devices(),
        "the tree's devices, read without any secret"
    );
    assert!(
        framing::inspect(&info).is_err(),
        "a GroupInfo is not a protocol message"
    );

    // Bob's new browser joins on its own, while Alice and Bob's phone are offline.
    let bob_web = Device::generate(Uuid::new_v4()).unwrap();
    let (mut gw, out) = bob_web.join_by_external_commit(&info).unwrap();
    assert_eq!(out.epoch, ga.epoch(), "created in the GroupInfo's epoch");
    assert!(out.welcome.is_none());
    assert_eq!(gw.epoch(), ga.epoch() + 1);
    let f = framing::inspect(&out.commit).unwrap();
    assert_eq!(f.content, FramedContent::Commit);
    assert_eq!(f.wire_format, FramedWireFormat::Public);
    assert!(f.new_member_commit);
    assert_eq!(f.group_id, gid.to_vec());
    assert_eq!(f.epoch, ga.epoch());
    // An ordinary member commit is not an external one.
    assert!(!framing::inspect(&add.commit).unwrap().new_member_commit);

    // Bob's browser can already write; members read it once they process the commit.
    let hello = bob_web
        .encrypt(&mut gw, b"aad", b"sent before anyone was online")
        .unwrap();
    let key = bob_web.signature_public_key();
    let web_id = bob_web.device_id();
    for (d, g) in [(&alice, &mut ga), (&bob_phone, &mut gp)] {
        match d
            .process_with_joiners(g, &out.commit, |dev, k| {
                dev == web_id && k == key.as_slice()
            })
            .unwrap()
        {
            Processed::ExternalJoin {
                device_id,
                signature_key,
                epoch,
            } => {
                assert_eq!(device_id, web_id);
                assert_eq!(signature_key, key);
                assert_eq!(epoch, gw.epoch());
            }
            other => panic!("expected an external join, got {other:?}"),
        }
        assert_eq!(
            text(d.process(g, &hello).unwrap()),
            "sent before anyone was online"
        );
        assert!(g.member_devices().contains(&web_id));
    }
    let back = alice
        .encrypt(&mut ga, b"aad", b"welcome, new browser")
        .unwrap();
    assert_eq!(
        text(bob_web.process(&mut gw, &back).unwrap()),
        "welcome, new browser"
    );

    // The device that joined externally adds devices with tree-carrying Welcomes too.
    let carol = Device::generate(Uuid::new_v4()).unwrap();
    let add = bob_web
        .add_devices(&mut gw, &[carol.key_package().unwrap()])
        .unwrap();
    for (d, g) in [(&alice, &mut ga), (&bob_phone, &mut gp)] {
        assert!(matches!(
            d.process(g, &add.commit).unwrap(),
            Processed::CommitMerged { .. }
        ));
    }
    let gc = carol.join(add.welcome.as_ref().unwrap()).unwrap();
    assert_eq!(gc.epoch(), ga.epoch());
}

#[test]
fn members_merge_an_external_commit_only_for_a_verified_joiner() {
    let gid = [8u8; 32];
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let mut ga = alice.create_group(&gid).unwrap();
    let info = alice.export_group_info(&ga).unwrap();
    let intruder = Device::generate(Uuid::new_v4()).unwrap();
    let (_, out) = intruder.join_by_external_commit(&info).unwrap();
    let before = ga.epoch();
    // Not (yet) verified: nothing is merged and the joiner is named.
    assert_eq!(
        alice
            .process_with_joiners(&mut ga, &out.commit, |_, _| false)
            .unwrap_err(),
        GroupError::JoinerNotAllowed(intruder.device_id())
    );
    assert_eq!(ga.epoch(), before);
    assert_eq!(ga.member_devices(), vec![alice.device_id()]);
    // A registered device id with ANOTHER key (an impostor) is refused the same way.
    let real_key = Device::generate(intruder.device_id())
        .unwrap()
        .signature_public_key();
    assert!(matches!(
        alice.process_with_joiners(&mut ga, &out.commit, |d, k| d == intruder.device_id()
            && k == real_key.as_slice()),
        Err(GroupError::JoinerNotAllowed(_))
    ));
    assert_eq!(ga.epoch(), before);
}

#[test]
fn a_stale_group_info_cannot_be_used_after_the_group_moved_on() {
    let gid = [9u8; 32];
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    let bob = Device::generate(Uuid::new_v4()).unwrap();
    let mut ga = alice.create_group(&gid).unwrap();
    let stale = alice.export_group_info(&ga).unwrap();
    let add = alice
        .add_devices(&mut ga, &[bob.key_package().unwrap()])
        .unwrap();
    let mut gb = bob.join(add.welcome.as_ref().unwrap()).unwrap();
    // Bob's device is later removed; whatever GroupInfo it kept is of an old epoch.
    let removal = alice.remove_devices(&mut ga, &[bob.device_id()]).unwrap();
    let _ = bob.process(&mut gb, &removal.commit);
    let evicted = Device::generate(bob.device_id()).unwrap();
    let (_, out) = evicted.join_by_external_commit(&stale).unwrap();
    assert!(alice.process(&mut ga, &out.commit).is_err());
    assert_eq!(ga.member_devices(), vec![alice.device_id()]);
}

#[test]
fn a_group_info_is_refused_as_garbage_and_from_other_messages() {
    let alice = Device::generate(Uuid::new_v4()).unwrap();
    assert!(alice.join_by_external_commit(b"nope").is_err());
    let kp = alice.key_package().unwrap();
    assert!(alice.join_by_external_commit(&kp).is_err());
    assert!(framing::inspect_group_info(&kp).is_err());
}
