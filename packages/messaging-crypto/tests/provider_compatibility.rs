//! The CF Meet OpenMLS provider (src/provider.rs) against the provider it replaced
//! (`openmls_rust_crypto` / `openmls_basic_credential` 0.6.0, RUSTSEC-2026-0387).
//!
//! `tests/vectors/provider_compat_v1.json` was produced by the FORMER provider
//! (generator: `tests/vectors/provider_compat_v1.generator.rs.txt`, run against
//! cf-messaging-crypto at 05af5f5). It holds TEST keys only:
//!
//! * primitive outputs (SHA-256, HKDF, HMAC, AES-128-GCM, Ed25519, HPKE key derivation,
//!   HPKE ciphertexts and an exporter secret for both HPKE AEADs CF Meet uses);
//! * persisted device states (`Device::export_state`) of a three-epoch-old group: Alice
//!   at epoch 4, Bob at epoch 3 with three messages from Alice still queued (an
//!   application message, Alice's update commit, an application message of the new
//!   epoch), and Dave holding an unused KeyPackage whose private key only his state has.
//!
//! The new provider must compute the same bytes, open what the old one sealed, import
//! the old states byte for byte, and continue every group operation from them.

use cf_messaging_crypto::provider::CfMlsCrypto;
use openmls::prelude::{AeadType, OpenMlsRand};
use openmls::prelude::{
    HashType, HpkeAeadType, HpkeCiphertext, HpkeConfig, HpkeKdfType, HpkeKemType, OpenMlsCrypto,
    SignatureScheme,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("vectors/provider_compat_v1.json")).unwrap()
}

fn hex(v: &Value) -> Vec<u8> {
    let s = v.as_str().expect("hex string");
    assert!(s.len().is_multiple_of(2));
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const AES: HpkeConfig = HpkeConfig(
    HpkeKemType::DhKem25519,
    HpkeKdfType::HkdfSha256,
    HpkeAeadType::AesGcm128,
);
const CHACHA: HpkeConfig = HpkeConfig(
    HpkeKemType::DhKem25519,
    HpkeKdfType::HkdfSha256,
    HpkeAeadType::ChaCha20Poly1305,
);

#[test]
fn primitives_produce_the_former_providers_bytes() {
    let f = fixture();
    let p = &f["primitives"];
    let c = CfMlsCrypto;
    let msg = hex(&p["message"]);

    assert_eq!(c.hash(HashType::Sha2_256, &msg).unwrap(), hex(&p["sha256"]));

    let k = &p["hkdf"];
    let prk = c
        .hkdf_extract(HashType::Sha2_256, &hex(&k["salt"]), &hex(&k["ikm"]))
        .unwrap();
    assert_eq!(prk.as_slice(), hex(&k["prk"]));
    let okm = c
        .hkdf_expand(
            HashType::Sha2_256,
            prk.as_slice(),
            &hex(&k["info"]),
            k["length"].as_u64().unwrap() as usize,
        )
        .unwrap();
    assert_eq!(okm.as_slice(), hex(&k["okm"]));

    let m = &p["hmac_sha256"];
    assert_eq!(
        c.hmac(HashType::Sha2_256, &hex(&m["key"]), &msg)
            .unwrap()
            .as_slice(),
        hex(&m["mac"])
    );

    let a = &p["aes128gcm"];
    let (key, nonce, aad) = (hex(&a["key"]), hex(&a["nonce"]), hex(&a["aad"]));
    let ct = c
        .aead_encrypt(AeadType::Aes128Gcm, &key, &msg, &nonce, &aad)
        .unwrap();
    assert_eq!(ct, hex(&a["ciphertext"]));
    assert_eq!(
        c.aead_decrypt(AeadType::Aes128Gcm, &key, &ct, &nonce, &aad)
            .unwrap(),
        msg
    );
    let mut tampered = ct.clone();
    tampered[0] ^= 1;
    assert!(c
        .aead_decrypt(AeadType::Aes128Gcm, &key, &tampered, &nonce, &aad)
        .is_err());

    // Ed25519 is deterministic: same key, same signature.
    let e = &p["ed25519"];
    assert_eq!(
        c.sign(SignatureScheme::ED25519, &msg, &hex(&e["private"]))
            .unwrap(),
        hex(&e["signature"])
    );
    // A key pair the former provider GENERATED signs identically here, and its
    // signature verifies.
    let g = &p["ed25519_generated"];
    let (gsk, gpk) = (hex(&g["private"]), hex(&g["public"]));
    assert_eq!(
        c.sign(SignatureScheme::ED25519, &msg, &gsk).unwrap(),
        hex(&g["signature"])
    );
    c.verify_signature(SignatureScheme::ED25519, &msg, &gpk, &hex(&g["signature"]))
        .unwrap();
    // And the reverse: what this provider signs with a fresh key verifies (strictly).
    let (sk, pk) = c.signature_key_gen(SignatureScheme::ED25519).unwrap();
    let sig = c.sign(SignatureScheme::ED25519, &msg, &sk).unwrap();
    c.verify_signature(SignatureScheme::ED25519, &msg, &pk, &sig)
        .unwrap();
}

#[test]
fn hpke_derives_the_same_keys_and_opens_what_the_former_provider_sealed() {
    let f = fixture();
    for (config, v) in [
        (AES, &f["primitives"]["hpke"]),
        (CHACHA, &f["primitives"]["hpke_chacha20poly1305"]),
    ] {
        let config = || HpkeConfig(config.0, config.1, config.2);
        let c = CfMlsCrypto;
        let kp = c.derive_hpke_keypair(config(), &hex(&v["ikm"])).unwrap();
        assert_eq!(kp.private.as_ref(), hex(&v["private"]).as_slice());
        assert_eq!(kp.public, hex(&v["public"]));
        let sealed = HpkeCiphertext {
            kem_output: hex(&v["kem_output"]).into(),
            ciphertext: hex(&v["ciphertext"]).into(),
        };
        let (info, aad) = (hex(&v["info"]), hex(&v["aad"]));
        let opened = c
            .hpke_open(config(), &sealed, kp.private.as_ref(), &info, &aad)
            .unwrap();
        assert_eq!(opened, hex(&f["primitives"]["message"]));
        // Wrong AAD or a tampered ciphertext is refused.
        assert!(c
            .hpke_open(config(), &sealed, kp.private.as_ref(), &info, b"other")
            .is_err());
        // Round trip through this provider.
        let again = c
            .hpke_seal(config(), &kp.public, &info, &aad, b"round trip")
            .unwrap();
        assert_eq!(
            c.hpke_open(config(), &again, kp.private.as_ref(), &info, &aad)
                .unwrap(),
            b"round trip"
        );
    }
    // The exporter secret the former provider's sender derived.
    let v = &f["primitives"]["hpke"];
    let c = CfMlsCrypto;
    let exported = c
        .hpke_setup_receiver_and_export(
            AES,
            &hex(&v["export_enc"]),
            &hex(&v["private"]),
            &hex(&v["info"]),
            &hex(&v["exporter_context"]),
            32,
        )
        .unwrap();
    assert_eq!(exported.as_ref(), hex(&v["exported"]).as_slice());
    assert_eq!(c.random_vec(32).unwrap().len(), 32);
}

#[cfg(feature = "client")]
mod groups {
    use super::{fixture, hex};
    use cf_messaging_crypto::{
        device::Device,
        group::{ClientGroup, Processed},
        key_package::{validate_key_package, LifetimePolicy},
    };
    use serde_json::Value;
    use uuid::Uuid;

    fn id(v: &Value) -> Uuid {
        v["device_id"].as_str().unwrap().parse().unwrap()
    }

    fn import(v: &Value) -> Device {
        let state = hex(&v["state"]);
        let device = Device::import_state(id(v), &state).expect("a former-provider state imports");
        assert_eq!(device.signature_public_key(), hex(&v["public"]));
        // Byte for byte: the signature key pair and every storage entry keep their
        // encoding, so the state exported again is the state that was imported.
        assert_eq!(
            device.export_state().as_slice(),
            state.as_slice(),
            "re-export differs"
        );
        device
    }

    fn text(p: Processed) -> String {
        match p {
            Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
            other => panic!("expected an application message, got {other:?}"),
        }
    }

    fn merged(p: Processed) -> u64 {
        match p {
            Processed::CommitMerged { epoch } => epoch,
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    fn sorted(mut v: Vec<Uuid>) -> Vec<Uuid> {
        v.sort();
        v
    }

    fn send(from: &Device, g: &mut ClientGroup, to: &mut [(&Device, &mut ClientGroup)], t: &str) {
        let m = from.encrypt(g, b"compat", t.as_bytes()).unwrap();
        for (d, gr) in to.iter_mut() {
            assert_eq!(text(d.process(gr, &m).unwrap()), t);
        }
    }

    #[test]
    fn groups_persisted_by_the_former_provider_continue_through_every_operation() {
        let f = fixture();
        let gid = hex(&f["group_id"]);
        let alice = import(&f["alice"]);
        let bob = import(&f["bob"]);
        let dave = import(&f["dave"]);

        let mut ga = alice.load_group(&gid).unwrap().expect("Alice's group");
        let mut gb = bob.load_group(&gid).unwrap().expect("Bob's group");
        assert_eq!(ga.epoch(), f["alice"]["epoch"].as_u64().unwrap());
        assert_eq!(gb.epoch(), f["bob"]["epoch"].as_u64().unwrap());
        assert_eq!(gb.epoch() + 1, ga.epoch());
        let members = sorted(vec![alice.device_id(), bob.device_id()]);
        assert_eq!(ga.member_devices(), members);
        assert_eq!(gb.member_devices(), members);

        // Messages the former provider produced while Bob was offline: decrypted with the
        // epoch secrets it persisted, the commit merged, the next epoch's message read.
        let queued = f["queued_for_bob"].as_array().unwrap();
        let mut first = None;
        for q in queued {
            let msg = hex(&q["message"]);
            match q["kind"].as_str().unwrap() {
                "application" => match bob.process(&mut gb, &msg).unwrap() {
                    Processed::Application { aad, plaintext } => {
                        assert_eq!(aad, hex(&q["aad"]));
                        assert_eq!(plaintext, q["plaintext"].as_str().unwrap().as_bytes());
                    }
                    other => panic!("{other:?}"),
                },
                "commit" => assert_eq!(
                    merged(bob.process(&mut gb, &msg).unwrap()),
                    q["epoch_after"].as_u64().unwrap()
                ),
                k => panic!("{k}"),
            }
            first.get_or_insert(msg);
        }
        assert_eq!(gb.epoch(), ga.epoch());
        // Replay protection survives the provider change.
        assert!(bob.process(&mut gb, first.as_ref().unwrap()).is_err());

        // Both directions between the two restored members.
        send(&bob, &mut gb, &mut [(&alice, &mut ga)], "bob, new provider");
        send(
            &alice,
            &mut ga,
            &mut [(&bob, &mut gb)],
            "alice, new provider",
        );

        // Join: Dave's KeyPackage (and its HPKE private key, kept only in his persisted
        // state) was generated by the former provider. The backend validator accepts the
        // KeyPackage's signatures; Alice adds it; Dave opens the Welcome.
        let dave_kp = hex(&f["dave"]["key_package"]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let lenient = LifetimePolicy {
            max_total_seconds: u64::MAX,
            ..LifetimePolicy::default()
        };
        let validated = validate_key_package(&dave_kp, now, &lenient).unwrap();
        assert_eq!(validated.device_id, dave.device_id());
        let add = alice.add_devices(&mut ga, &[dave_kp]).unwrap();
        assert_eq!(
            merged(bob.process(&mut gb, &add.commit).unwrap()),
            ga.epoch()
        );
        let mut gd = dave.join(add.welcome.as_ref().unwrap()).unwrap();
        assert_eq!(gd.epoch(), ga.epoch());
        send(
            &dave,
            &mut gd,
            &mut [(&alice, &mut ga), (&bob, &mut gb)],
            "dave joined",
        );

        // Update (epoch transition) by a restored member.
        let up = bob.update_keys(&mut gb).unwrap();
        assert_eq!(
            merged(alice.process(&mut ga, &up.commit).unwrap()),
            gb.epoch()
        );
        assert_eq!(
            merged(dave.process(&mut gd, &up.commit).unwrap()),
            gb.epoch()
        );
        send(
            &alice,
            &mut ga,
            &mut [(&bob, &mut gb), (&dave, &mut gd)],
            "after update",
        );

        // A device created by the new provider joins the restored group.
        let erin = Device::generate(Uuid::new_v4()).unwrap();
        let add = bob
            .add_devices(&mut gb, &[erin.key_package().unwrap()])
            .unwrap();
        merged(alice.process(&mut ga, &add.commit).unwrap());
        merged(dave.process(&mut gd, &add.commit).unwrap());
        let mut ge = erin.join(add.welcome.as_ref().unwrap()).unwrap();
        send(
            &erin,
            &mut ge,
            &mut [(&alice, &mut ga), (&bob, &mut gb), (&dave, &mut gd)],
            "erin joined",
        );

        // Removal: Dave leaves the group and cannot read what follows.
        let rm = alice.remove_devices(&mut ga, &[dave.device_id()]).unwrap();
        merged(bob.process(&mut gb, &rm.commit).unwrap());
        merged(erin.process(&mut ge, &rm.commit).unwrap());
        let _ = dave.process(&mut gd, &rm.commit);
        let secret = alice.encrypt(&mut ga, b"compat", b"not for dave").unwrap();
        assert!(dave.process(&mut gd, &secret).is_err());
        assert_eq!(text(bob.process(&mut gb, &secret).unwrap()), "not for dave");
        assert_eq!(
            text(erin.process(&mut ge, &secret).unwrap()),
            "not for dave"
        );
        assert_eq!(
            ga.member_devices(),
            sorted(vec![alice.device_id(), bob.device_id(), erin.device_id()])
        );

        // The continued state persists and restores again under the new provider.
        let bob_id = bob.device_id();
        let state = bob.export_state();
        drop(gb);
        drop(bob);
        let bob = Device::import_state(bob_id, &state).unwrap();
        let mut gb = bob.load_group(&gid).unwrap().unwrap();
        send(
            &alice,
            &mut ga,
            &mut [(&bob, &mut gb), (&erin, &mut ge)],
            "after restore",
        );
    }

    #[test]
    fn a_tampered_former_provider_state_is_refused() {
        let f = fixture();
        let mut state = hex(&f["bob"]["state"]);
        // The private key of the TLS-encoded signature key pair starts after the magic,
        // the device id and the length prefix (8 + 16 + 4) and its own length byte.
        state[8 + 16 + 4 + 1] ^= 1;
        assert!(Device::import_state(id(&f["bob"]), &state).is_err());
        assert!(Device::import_state(Uuid::new_v4(), &hex(&f["bob"]["state"])).is_err());
    }
}
