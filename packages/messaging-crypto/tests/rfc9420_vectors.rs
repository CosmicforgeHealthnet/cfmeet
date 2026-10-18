//! RFC 9420 conformance with the MLS working group's published test vectors
//! (github.com/mlswg/mls-implementations, `test-vectors/`), Part V §144 / Appendix N
//! "Protocol conformance".
//!
//! The vectors are not vendored (their repository carries no licence file): CI downloads
//! them at a pinned commit and checks their SHA-256 (`scripts/messaging/fetch-mls-test-vectors.sh`),
//! then points `MLS_TEST_VECTORS_DIR` at them. Without the directory the tests skip with a
//! message, unless `MLS_TEST_VECTORS_REQUIRED=1` (set in CI), where they fail.
//!
//! * `messages.json` — every MLSMessage encoding (KeyPackage, Welcome, GroupInfo,
//!   PublicMessage application / proposal / commit, PrivateMessage) decodes with OpenMLS,
//!   exactly as the Messaging backend decodes published KeyPackages, and re-encodes to the
//!   identical bytes.
//! * `crypto-basics.json` — for the mandatory suite 0x0001 only: SignWithLabel
//!   verification (what KeyPackage / leaf-node signature checks rely on), RefHash
//!   (KeyPackageRef), ExpandWithLabel, DeriveSecret, DeriveTreeSecret and EncryptWithLabel
//!   (HPKE open), computed with the OpenMLS RustCrypto provider. The labelled encodings
//!   below are the RFC 9420 §5.1–§5.2 definitions, written out only to call the provider;
//!   no primitive is implemented here.

use std::path::PathBuf;

use openmls::prelude::{
    tls_codec::{Deserialize as _, Serialize as _, VLBytes},
    HashType, HpkeAeadType, HpkeCiphertext, HpkeConfig, HpkeKdfType, HpkeKemType, MlsMessageIn,
    OpenMlsCrypto, SignatureScheme,
};
use openmls_rust_crypto::RustCrypto;
use serde_json::Value;

const MANDATORY_SUITE: u64 = 1;

fn vectors(name: &str) -> Option<Value> {
    let Some(dir) = std::env::var_os("MLS_TEST_VECTORS_DIR") else {
        assert!(
            std::env::var("MLS_TEST_VECTORS_REQUIRED").as_deref() != Ok("1"),
            "MLS_TEST_VECTORS_DIR is not set (run scripts/messaging/fetch-mls-test-vectors.sh)"
        );
        eprintln!("SKIPPED: RFC 9420 test vectors (MLS_TEST_VECTORS_DIR unset)");
        return None;
    };
    let path = PathBuf::from(dir).join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    Some(serde_json::from_str(&text).unwrap())
}

fn hex(v: &Value) -> Vec<u8> {
    let s = v.as_str().expect("hex string");
    assert!(s.len() % 2 == 0);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// `opaque<V>`: the RFC 9420 variable-length vector (QUIC-style varint length).
fn vl(bytes: &[u8]) -> Vec<u8> {
    VLBytes::new(bytes.to_vec())
        .tls_serialize_detached()
        .unwrap()
}

fn mls_label(label: &str) -> Vec<u8> {
    format!("MLS 1.0 {label}").into_bytes()
}

/// RFC 9420 §5.1.3 KDFLabel, then HKDF-Expand.
fn expand_with_label(secret: &[u8], label: &str, context: &[u8], length: u16) -> Vec<u8> {
    let mut info = length.to_be_bytes().to_vec();
    info.extend(vl(&mls_label(label)));
    info.extend(vl(context));
    RustCrypto::default()
        .hkdf_expand(HashType::Sha2_256, secret, &info, length as usize)
        .unwrap()
        .as_slice()
        .to_vec()
}

#[test]
fn every_mls_message_encoding_round_trips() {
    let Some(cases) = vectors("messages.json") else {
        return;
    };
    let cases = cases.as_array().unwrap();
    assert!(
        cases.len() >= 100,
        "the published set has hundreds of cases"
    );
    let fields = [
        "mls_key_package",
        "mls_welcome",
        "mls_group_info",
        "public_message_application",
        "public_message_proposal",
        "public_message_commit",
        "private_message",
    ];
    let mut checked = 0;
    for (i, case) in cases.iter().enumerate() {
        for field in fields {
            let bytes = hex(&case[field]);
            let message = MlsMessageIn::tls_deserialize_exact(&bytes)
                .unwrap_or_else(|e| panic!("case {i} {field}: {e:?}"));
            assert_eq!(
                message.tls_serialize_detached().unwrap(),
                bytes,
                "case {i} {field}: re-encoding differs"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, cases.len() * fields.len());
}

#[test]
fn the_mandatory_suite_matches_the_crypto_basics_vectors() {
    let Some(cases) = vectors("crypto-basics.json") else {
        return;
    };
    let case = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["cipher_suite"].as_u64() == Some(MANDATORY_SUITE))
        .expect("suite 0x0001 is in the published vectors");
    let crypto = RustCrypto::default();

    // SignWithLabel (§5.1.2): SignContent { opaque label<V>; opaque content<V>; }.
    let v = &case["sign_with_label"];
    let mut content = vl(&mls_label(v["label"].as_str().unwrap()));
    content.extend(vl(&hex(&v["content"])));
    let (public, signature) = (hex(&v["pub"]), hex(&v["signature"]));
    crypto
        .verify_signature(SignatureScheme::ED25519, &content, &public, &signature)
        .expect("SignWithLabel vector verifies");
    let mut tampered = content.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(crypto
        .verify_signature(SignatureScheme::ED25519, &tampered, &public, &signature)
        .is_err());

    // RefHash (§5.2): Hash(RefHashInput { opaque label<V>; opaque value<V>; }) — the
    // KeyPackageRef the backend stores uses label "MLS 1.0 KeyPackage Reference".
    let v = &case["ref_hash"];
    let mut input = vl(v["label"].as_str().unwrap().as_bytes());
    input.extend(vl(&hex(&v["value"])));
    assert_eq!(
        crypto.hash(HashType::Sha2_256, &input).unwrap(),
        hex(&v["out"]),
        "RefHash"
    );

    // ExpandWithLabel / DeriveSecret / DeriveTreeSecret (§5.1.3, §9.1).
    let v = &case["expand_with_label"];
    assert_eq!(
        expand_with_label(
            &hex(&v["secret"]),
            v["label"].as_str().unwrap(),
            &hex(&v["context"]),
            v["length"].as_u64().unwrap() as u16
        ),
        hex(&v["out"]),
        "ExpandWithLabel"
    );
    let v = &case["derive_secret"];
    assert_eq!(
        expand_with_label(&hex(&v["secret"]), v["label"].as_str().unwrap(), &[], 32),
        hex(&v["out"]),
        "DeriveSecret"
    );
    let v = &case["derive_tree_secret"];
    let generation = (v["generation"].as_u64().unwrap() as u32).to_be_bytes();
    assert_eq!(
        expand_with_label(
            &hex(&v["secret"]),
            v["label"].as_str().unwrap(),
            &generation,
            v["length"].as_u64().unwrap() as u16
        ),
        hex(&v["out"]),
        "DeriveTreeSecret"
    );

    // EncryptWithLabel (§5.1.3): HPKE with info = EncryptContext { label<V>; context<V> }.
    let v = &case["encrypt_with_label"];
    let mut info = vl(&mls_label(v["label"].as_str().unwrap()));
    info.extend(vl(&hex(&v["context"])));
    let ciphertext = HpkeCiphertext {
        kem_output: hex(&v["kem_output"]).into(),
        ciphertext: hex(&v["ciphertext"]).into(),
    };
    let config = || {
        HpkeConfig(
            HpkeKemType::DhKem25519,
            HpkeKdfType::HkdfSha256,
            HpkeAeadType::AesGcm128,
        )
    };
    let plaintext = crypto
        .hpke_open(config(), &ciphertext, &hex(&v["priv"]), &info, &[])
        .expect("EncryptWithLabel vector opens");
    assert_eq!(plaintext, hex(&v["plaintext"]));
    let mut wrong = info.clone();
    *wrong.last_mut().unwrap() ^= 1;
    assert!(crypto
        .hpke_open(config(), &ciphertext, &hex(&v["priv"]), &wrong, &[])
        .is_err());
}
