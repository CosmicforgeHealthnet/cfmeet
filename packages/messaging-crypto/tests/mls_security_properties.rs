//! Part V Appendix N — MLS security properties demonstrated with real OpenMLS client
//! devices in the test harness: forward secrecy (destroyed epoch / message secrets cannot
//! be reconstructed from later client state), post-compromise security (a copied device
//! state stops following the group once the device refreshes its keys), and a large
//! group (add, fan-out, removal — the removed device cannot decrypt later epochs).
//!
//! These exercise the reviewed OpenMLS 0.9.1 behaviour through the crypto core's client
//! wrapper; they are evidence for the in-repo harness gates, not an external
//! cryptographic assessment.
#![cfg(feature = "client")]

use cf_messaging_crypto::{
    device::Device,
    group::{ClientGroup, Processed},
};
use uuid::Uuid;

const AAD: &[u8] = b"cfmeet-test-aad";

fn text(p: Processed) -> String {
    match p {
        Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
        other => panic!("expected an application message, got {other:?}"),
    }
}

fn merged(p: Processed) {
    assert!(matches!(p, Processed::CommitMerged { .. }), "{p:?}");
}

/// A group of `n` devices created by the first one. Returns (devices, groups).
fn group_of(n: usize, gid: [u8; 32]) -> (Vec<Device>, Vec<ClientGroup>) {
    let devices: Vec<Device> = (0..n)
        .map(|_| Device::generate(Uuid::new_v4()).unwrap())
        .collect();
    let mut g0 = devices[0].create_group(&gid).unwrap();
    let packages: Vec<Vec<u8>> = devices[1..]
        .iter()
        .map(|d| d.key_package().unwrap())
        .collect();
    let mut groups = Vec::with_capacity(n);
    if packages.is_empty() {
        groups.push(g0);
        return (devices, groups);
    }
    let add = devices[0].add_devices(&mut g0, &packages).unwrap();
    groups.push(g0);
    for d in &devices[1..] {
        groups.push(d.join(add.welcome.as_ref().unwrap()).unwrap());
    }
    (devices, groups)
}

#[test]
fn forward_secrecy_consumed_and_past_epoch_secrets_are_not_reconstructible() {
    let (devices, mut groups) = group_of(3, [11; 32]);
    let (alice, bob) = (&devices[0], &devices[1]);
    let (ga, rest) = groups.split_first_mut().unwrap();
    let gb = &mut rest[0];
    let m1 = alice.encrypt(ga, AAD, b"epoch-n secret").unwrap();
    assert_eq!(text(bob.process(gb, &m1).unwrap()), "epoch-n secret");
    // Within the epoch: the message key was deleted after use — the same ciphertext does
    // not decrypt twice from the same (later) state.
    assert!(
        bob.process(gb, &m1).is_err(),
        "a consumed message key is gone"
    );

    // A new epoch (Alice removes Carol); Bob follows it.
    let rm = alice.remove_devices(ga, &[devices[2].device_id()]).unwrap();
    merged(bob.process(gb, &rm.commit).unwrap());
    // Bob's entire device state AFTER the epoch change, restored on a fresh instance:
    let state = bob.export_state();
    let restored = Device::import_state(bob.device_id(), &state).unwrap();
    let mut rg = restored.load_group(&[11; 32]).unwrap().unwrap();
    assert!(rg.epoch() > 0);
    // ... cannot decrypt the epoch-n message (its epoch secrets were destroyed).
    assert!(
        restored.process(&mut rg, &m1).is_err(),
        "past epoch secrets are not in later state"
    );
    // A message sent in epoch n but delivered late is equally unreadable now.
    let late_refused;
    {
        let (d, mut g) = group_of(2, [13; 32]);
        let early = d[0]
            .encrypt(&mut g[0], AAD, b"sent before the commit")
            .unwrap();
        let rm = d[0]
            .add_devices(
                &mut g[0],
                &[Device::generate(Uuid::new_v4())
                    .unwrap()
                    .key_package()
                    .unwrap()],
            )
            .unwrap();
        merged(d[1].process(&mut g[1], &rm.commit).unwrap());
        late_refused = d[1].process(&mut g[1], &early).is_err();
    }
    assert!(
        late_refused,
        "no past-epoch decryption is retained (max_past_epochs = 0)"
    );
}

#[test]
fn post_compromise_security_a_copied_device_state_stops_following_after_a_key_update() {
    let (devices, mut groups) = group_of(3, [21; 32]);
    let (alice, bob) = (&devices[0], &devices[1]);
    // The attacker copies Bob's complete device state (compromise).
    let stolen_state = bob.export_state();
    let attacker = Device::import_state(bob.device_id(), &stolen_state).unwrap();
    let mut attacker_group = attacker.load_group(&[21; 32]).unwrap().unwrap();
    {
        let (ga, rest) = groups.split_first_mut().unwrap();
        // Before recovery the copy reads the group (that is what compromise means).
        let m = alice.encrypt(ga, AAD, b"before recovery").unwrap();
        assert_eq!(
            text(attacker.process(&mut attacker_group, &m).unwrap()),
            "before recovery"
        );
        assert_eq!(
            text(bob.process(&mut rest[0], &m).unwrap()),
            "before recovery"
        );
    }
    // Bob recovers: a self-update commit with fresh leaf secrets.
    let copy_epoch = attacker_group.epoch();
    let update = bob.update_keys(&mut groups[1]).unwrap();
    for i in [0usize, 2] {
        merged(devices[i].process(&mut groups[i], &update.commit).unwrap());
    }
    // Whatever the copy makes of Bob's update commit (fresh path secrets it never held),
    // it reads nothing of the new epoch, while every real member does.
    let followed = attacker.process(&mut attacker_group, &update.commit);
    assert!(
        !matches!(followed, Ok(Processed::CommitMerged { .. })),
        "the copy merged Bob's update: {followed:?}"
    );
    assert_eq!(
        attacker_group.epoch(),
        copy_epoch,
        "the copy is left behind"
    );
    let m = alice
        .encrypt(&mut groups[0], AAD, b"after recovery")
        .unwrap();
    assert!(
        attacker.process(&mut attacker_group, &m).is_err(),
        "post-compromise security"
    );
    assert_eq!(
        text(bob.process(&mut groups[1], &m).unwrap()),
        "after recovery"
    );
    assert_eq!(
        text(devices[2].process(&mut groups[2], &m).unwrap()),
        "after recovery"
    );
}

fn large_group(n: usize) {
    let started = std::time::Instant::now();
    let (devices, mut groups) = group_of(n, [31; 32]);
    let formed = started.elapsed();
    let m = devices[0]
        .encrypt(&mut groups[0], AAD, b"to every device")
        .unwrap();
    for i in 1..n {
        assert_eq!(
            text(devices[i].process(&mut groups[i], &m).unwrap()),
            "to every device"
        );
    }
    // Remove the last tenth in one commit; everyone else follows.
    let keep = n - n / 10;
    let removed: Vec<Uuid> = devices[keep..].iter().map(|d| d.device_id()).collect();
    let rm = devices[0].remove_devices(&mut groups[0], &removed).unwrap();
    for i in 1..keep {
        merged(devices[i].process(&mut groups[i], &rm.commit).unwrap());
    }
    assert_eq!(groups[0].member_devices().len(), keep);
    let m = devices[1]
        .encrypt(&mut groups[1], AAD, b"after the removal")
        .unwrap();
    for i in (0..keep).filter(|i| *i != 1) {
        assert_eq!(
            text(devices[i].process(&mut groups[i], &m).unwrap()),
            "after the removal"
        );
    }
    for i in keep..n {
        // A removed device is stuck in the old epoch: it decrypts nothing later.
        assert!(
            devices[i].process(&mut groups[i], &m).is_err(),
            "removed device {i} decrypted"
        );
    }
    println!(
        "LARGE_GROUP devices={n} formed_in={formed:?} total={:?}",
        started.elapsed()
    );
}

#[test]
fn a_32_device_group_adds_fans_out_and_removed_devices_cannot_decrypt_later_epochs() {
    large_group(32);
}

/// The same with 100 devices (about two minutes in an unoptimized build):
/// `cargo test --features client --test mls_security_properties -- --ignored`.
#[test]
#[ignore]
fn a_100_device_group_adds_fans_out_and_removed_devices_cannot_decrypt_later_epochs() {
    large_group(100);
}
