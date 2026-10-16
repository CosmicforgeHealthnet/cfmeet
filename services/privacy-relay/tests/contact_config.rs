//! Part D §31 / §122: Contact Vault configuration fails closed — an enabled vault without
//! (or with invalid, swapped or shared) key material, or a delegation trust that reuses
//! a service-token key, stops the service at startup. No database needed.

use std::{io::Write, path::PathBuf, sync::Arc};

use cf_service_auth::{KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, TrustPolicy};
use privacy_relay::{
    config::ContactVaultSettings,
    contact_vault::keyring::{generate_keyring_json, KeyPurpose, KeyVersion},
};
use serde_json::json;

fn file(dir: &tempdir::Dir, name: &str, content: &str) -> PathBuf {
    let path = dir.0.join(name);
    std::fs::File::create(&path)
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    path
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "pr-contact-config-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn lookup(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
}

fn service_trust(keys: &[&ServiceSigningKey]) -> TrustPolicy {
    TrustPolicy::from_json(
        &json!({
            "bundle_version": 1, "receiver": "cfmeet.privacy-relay", "runtime_environment": "test",
            "trusted_issuers": [{ "issuer": "cfmeet.calling", "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
                "keys": keys.iter().map(|k| k.public_jwk()).collect::<Vec<_>>(), "permissions": ["communication.authorize"] }],
        })
        .to_string(),
        &ServiceId::privacy_relay(),
        RuntimeEnvironment::Test,
    )
    .unwrap()
}

fn contact_trust(keys: &[&ServiceSigningKey]) -> String {
    json!({
        "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS", "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
        "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": keys.iter().map(|k| k.public_jwk()).collect::<Vec<_>>(), "scopes": ["privacy.contacts.write"] }],
    })
    .to_string()
}

fn v(s: &str) -> KeyVersion {
    KeyVersion::parse(s).unwrap()
}

#[test]
fn an_enabled_vault_requires_every_file() {
    for missing in [
        "CONTACT_VAULT_KEYRING_FILE",
        "CONTACT_FINGERPRINT_KEYRING_FILE",
        "CONTACT_DELEGATION_TRUST_BUNDLE_FILE",
    ] {
        let mut vars = vec![
            ("CONTACT_VAULT_ENABLED", "true".to_string()),
            ("CONTACT_VAULT_KEYRING_FILE", "/x/e.json".to_string()),
            ("CONTACT_FINGERPRINT_KEYRING_FILE", "/x/f.json".to_string()),
            (
                "CONTACT_DELEGATION_TRUST_BUNDLE_FILE",
                "/x/t.json".to_string(),
            ),
        ];
        vars.retain(|(k, _)| *k != missing);
        let err = ContactVaultSettings::from_lookup(&lookup(vars)).unwrap_err();
        assert!(err.to_string().contains(missing), "{err}");
    }
    assert!(ContactVaultSettings::from_lookup(&lookup(vec![(
        "CONTACT_VAULT_ENABLED",
        "yes".into()
    )]))
    .is_err());
    // One keyring without the other is refused even while disabled.
    assert!(ContactVaultSettings::from_lookup(&lookup(vec![(
        "CONTACT_VAULT_KEYRING_FILE",
        "/x/e.json".into()
    )]))
    .is_err());
    // Disabled and unconfigured is a valid (closed) state.
    let s = ContactVaultSettings::from_lookup(&lookup(vec![])).unwrap();
    assert!(!s.enabled);
}

#[test]
fn key_material_is_validated_and_kept_separate() {
    let dir = tempdir::Dir::new();
    let calling_service = ServiceSigningKey::generate(KeyId::parse("calling-test-1").unwrap());
    let calling_contact =
        ServiceSigningKey::generate(KeyId::parse("calling-contact-test-1").unwrap());
    let enc = generate_keyring_json(KeyPurpose::Encryption, &v("ck-1"));
    let fp = generate_keyring_json(KeyPurpose::Fingerprint, &v("fk-1"));
    let load = |e: &str, f: &str, t: &str, service: &TrustPolicy| {
        let settings = ContactVaultSettings {
            enabled: true,
            keyring_file: Some(file(&dir, &format!("e-{}.json", uuid::Uuid::new_v4()), e)),
            fingerprint_keyring_file: Some(file(
                &dir,
                &format!("f-{}.json", uuid::Uuid::new_v4()),
                f,
            )),
            delegation_trust_file: Some(file(&dir, &format!("t-{}.json", uuid::Uuid::new_v4()), t)),
            number_grant_trust_file: None,
        };
        settings
            .load(
                RuntimeEnvironment::Test,
                service,
                Arc::new(cf_service_auth::SystemClock),
            )
            .map(|m| (m.keys.is_some(), m.delegation.is_some()))
    };
    let service = service_trust(&[&calling_service]);
    assert_eq!(
        load(&enc, &fp, &contact_trust(&[&calling_contact]), &service).unwrap(),
        (true, true)
    );
    // Swapped files (purpose mismatch).
    assert!(load(&fp, &enc, &contact_trust(&[&calling_contact]), &service).is_err());
    // The same key material for encryption and fingerprints.
    let same_fp = enc
        .replace("CONTACT_ENCRYPTION", "CONTACT_FINGERPRINT")
        .replace("ck-1", "fk-1");
    let err = load(
        &enc,
        &same_fp,
        &contact_trust(&[&calling_contact]),
        &service,
    )
    .unwrap_err();
    assert!(err.to_string().contains("share key material"), "{err}");
    // Malformed keyring; the error never contains key material.
    let key = serde_json::from_str::<serde_json::Value>(&enc).unwrap()["keys"][0]["key"]
        .as_str()
        .unwrap()
        .to_string();
    let broken = enc.replace(&key, &key[..20]);
    let err = load(&broken, &fp, &contact_trust(&[&calling_contact]), &service).unwrap_err();
    assert!(!err.to_string().contains(&key[..20]), "{err}");
    // A key trusted for service tokens may not be trusted for Contact Vault tokens.
    let err = load(&enc, &fp, &contact_trust(&[&calling_service]), &service).unwrap_err();
    assert!(err.to_string().contains("service tokens"), "{err}");
    // Unreadable files.
    let settings = ContactVaultSettings {
        enabled: true,
        keyring_file: Some(PathBuf::from("/nonexistent/e.json")),
        fingerprint_keyring_file: Some(PathBuf::from("/nonexistent/f.json")),
        delegation_trust_file: None,
        number_grant_trust_file: None,
    };
    assert!(settings
        .load(
            RuntimeEnvironment::Test,
            &service,
            Arc::new(cf_service_auth::SystemClock)
        )
        .is_err());
}
