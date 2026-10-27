//! Property tests of every parser that reads untrusted bytes (Part V §144 / §158
//! "fuzz/property tests", "malformed ciphertext/parser fuzzing").
//!
//! Seeded, reproducible pseudo-random cases (xorshift64*; the seed is printed in every
//! failure), with no extra test framework in the dependency graph:
//!
//! * random bytes and truncated / bit-flipped valid inputs never panic the MLS framing
//!   inspector, the KeyPackage validator or the backup header parser;
//! * every single-byte mutation of a valid KeyPackage is rejected (the signature covers
//!   every byte);
//! * (feature `client`) a mutated MLS application message never decrypts to its
//!   plaintext, a mutated backup never restores and a mutated attachment never decrypts.

use cf_messaging_crypto::{backup, framing, key_package};

const CASES: usize = 2_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
    /// One of: flip a bit, overwrite a byte, truncate, insert a byte, delete a byte.
    /// Only the `client` properties build valid inputs to mutate.
    #[cfg(feature = "client")]
    fn mutate(&mut self, valid: &[u8]) -> Vec<u8> {
        let mut m = valid.to_vec();
        let at = self.below(m.len().max(1));
        match self.below(5) {
            0 if !m.is_empty() => m[at] ^= 1 << self.below(8),
            1 if !m.is_empty() => m[at] = m[at].wrapping_add(1 + self.below(255) as u8),
            2 => m.truncate(at),
            3 => m.insert(at, self.next() as u8),
            _ if !m.is_empty() => {
                m.remove(at);
            }
            _ => m.push(0),
        }
        m
    }
}

fn seed(name: &str) -> u64 {
    // Stable per property; override with PROPERTY_SEED to explore further.
    std::env::var("PROPERTY_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x1000_0000_01b3)
            })
        })
        | 1
}

#[test]
fn random_bytes_never_panic_any_public_parser() {
    let s = seed("random");
    let mut r = Rng(s);
    let policy = key_package::LifetimePolicy::default();
    for case in 0..CASES {
        let len = r.below(4096);
        let bytes = r.bytes(len);
        let _ = framing::inspect(&bytes);
        let _ = framing::is_welcome(&bytes);
        assert!(
            key_package::validate_key_package(&bytes, 1_790_000_000, &policy).is_err(),
            "seed {s} case {case}: random bytes validated as a KeyPackage"
        );
        assert!(
            backup::inspect(&bytes).is_err() || bytes.starts_with(b"CFMB1"),
            "seed {s} case {case}"
        );
    }
}

#[cfg(feature = "client")]
mod client {
    use super::*;
    use cf_messaging_crypto::{
        attachment,
        backup::{BackupHeader, BackupPayload, BackupRootKey},
        device::Device,
        group::Processed,
    };
    use uuid::Uuid;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn every_mutation_of_a_valid_key_package_is_rejected() {
        let s = seed("key-package");
        let mut r = Rng(s);
        let device = Device::generate(Uuid::new_v4()).unwrap();
        let valid = device.key_package().unwrap();
        let policy = key_package::LifetimePolicy::default();
        assert!(key_package::validate_key_package(&valid, now(), &policy).is_ok());
        for case in 0..CASES / 4 {
            let m = r.mutate(&valid);
            if m == valid {
                continue;
            }
            let _ = framing::inspect(&m);
            assert!(
                key_package::validate_key_package(&m, now(), &policy).is_err(),
                "seed {s} case {case}: a mutated KeyPackage validated"
            );
        }
    }

    #[test]
    fn a_mutated_application_message_never_decrypts_to_its_plaintext() {
        let s = seed("mls-message");
        let mut r = Rng(s);
        let gid = [41u8; 32];
        let (alice, bob) = (
            Device::generate(Uuid::new_v4()).unwrap(),
            Device::generate(Uuid::new_v4()).unwrap(),
        );
        let mut ga = alice.create_group(&gid).unwrap();
        let add = alice
            .add_devices(&mut ga, &[bob.key_package().unwrap()])
            .unwrap();
        let mut gb = bob.join(add.welcome.as_ref().unwrap()).unwrap();
        let aad = b"cfmeet-test-aad";
        for case in 0..200 {
            let secret = format!("secret {case}");
            let valid = alice.encrypt(&mut ga, aad, secret.as_bytes()).unwrap();
            let m = r.mutate(&valid);
            let _ = framing::inspect(&m);
            if m != valid {
                if let Ok(Processed::Application { plaintext, .. }) = bob.process(&mut gb, &m) {
                    panic!(
                        "seed {s} case {case}: a mutated message decrypted ({} bytes)",
                        plaintext.len()
                    );
                }
            }
            // The group still works afterwards (a refused message corrupts nothing).
            let next = alice.encrypt(&mut ga, aad, b"still in sync").unwrap();
            match bob.process(&mut gb, &next).unwrap() {
                Processed::Application { plaintext, .. } => {
                    assert_eq!(plaintext, b"still in sync", "seed {s} case {case}")
                }
                other => panic!("seed {s} case {case}: {other:?}"),
            }
        }
    }

    #[test]
    fn mutated_backups_and_attachments_never_open() {
        let s = seed("containers");
        let mut r = Rng(s);
        let root = BackupRootKey::generate();
        let principal = Uuid::new_v4();
        let header = BackupHeader {
            key_id: root.key_id(),
            backup_id: Uuid::new_v4(),
            principal_id: principal,
            created_at_unix: 1_790_000_000,
        };
        let payload = BackupPayload {
            sections: vec![
                ("HISTORY".into(), r.bytes(3000)),
                ("KEYS".into(), r.bytes(64)),
            ],
        };
        let valid = backup::encrypt_backup(&root, &header, &payload).unwrap();
        assert!(backup::decrypt_backup(&root, principal, &valid).is_ok());
        let file = r.bytes(70_000);
        let enc = attachment::encrypt_attachment(&file, "image/png", "a.png").unwrap();
        assert_eq!(
            attachment::decrypt_attachment(&enc.descriptor, &enc.ciphertext).unwrap(),
            file
        );
        for case in 0..CASES / 4 {
            let m = r.mutate(&valid);
            if m != valid {
                let _ = backup::inspect(&m);
                assert!(
                    backup::decrypt_backup(&root, principal, &m).is_err(),
                    "seed {s} case {case}: a mutated backup restored"
                );
            }
            let m = r.mutate(&enc.ciphertext);
            if m != enc.ciphertext {
                assert!(
                    attachment::decrypt_attachment(&enc.descriptor, &m).is_err(),
                    "seed {s} case {case}: a mutated attachment decrypted"
                );
            }
        }
    }
}
