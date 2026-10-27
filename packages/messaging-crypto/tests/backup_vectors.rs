//! Known-answer vectors of the secure backup format (Part V Appendix N "secure-backup
//! vectors"): a fixed root key, salt, nonce prefix and header must produce exactly the
//! recorded object, the recorded recovery code and key id, and restore the recorded
//! payload. Other implementations of the format (SDK wrappers) check against the same
//! file. The vector was produced by this implementation (a regression and
//! interoperability anchor, not an independent reference).
#![cfg(feature = "client")]

use cf_messaging_crypto::backup::{self as b, BackupHeader, BackupPayload, BackupRootKey};
use uuid::Uuid;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn vector() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors/backup_v1.json")).unwrap()
}

fn inputs(
    v: &serde_json::Value,
) -> (
    BackupRootKey,
    BackupHeader,
    BackupPayload,
    [u8; 32],
    [u8; 19],
) {
    let mut key = [0u8; 32];
    key.copy_from_slice(&unhex(v["rootKey"].as_str().unwrap()));
    let root = BackupRootKey::from_bytes(key);
    let header = BackupHeader {
        key_id: root.key_id(),
        backup_id: Uuid::parse_str(v["backupId"].as_str().unwrap()).unwrap(),
        principal_id: Uuid::parse_str(v["principalId"].as_str().unwrap()).unwrap(),
        created_at_unix: v["createdAtUnix"].as_u64().unwrap(),
    };
    let payload = BackupPayload {
        sections: v["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["name"].as_str().unwrap().to_string(),
                    unhex(s["dataHex"].as_str().unwrap()),
                )
            })
            .collect(),
    };
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&unhex(v["salt"].as_str().unwrap()));
    let mut prefix = [0u8; 19];
    prefix.copy_from_slice(&unhex(v["noncePrefix"].as_str().unwrap()));
    (root, header, payload, salt, prefix)
}

#[test]
fn the_recorded_vector_is_reproduced_and_restores() {
    let v = vector();
    let (root, header, payload, salt, prefix) = inputs(&v);
    assert_eq!(v["format"], b::BACKUP_FORMAT_V1);
    assert_eq!(hex(&root.key_id()), v["keyId"].as_str().unwrap());
    assert_eq!(root.recovery_code(), v["recoveryCode"].as_str().unwrap());
    let object = b::encrypt_backup_deterministic(&root, &header, &payload, salt, prefix).unwrap();
    assert_eq!(hex(&object), v["objectHex"].as_str().unwrap());
    let restored = BackupRootKey::from_recovery_code(v["recoveryCode"].as_str().unwrap()).unwrap();
    let (h, p) = b::decrypt_backup(
        &restored,
        header.principal_id,
        &unhex(v["objectHex"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!((h, p), (header, payload));
}

#[test]
fn the_recorded_object_fails_without_the_key_and_when_tampered() {
    let v = vector();
    let (root, header, _, _, _) = inputs(&v);
    let object = unhex(v["objectHex"].as_str().unwrap());
    assert_eq!(
        b::decrypt_backup(
            &BackupRootKey::from_bytes([7; 32]),
            header.principal_id,
            &object
        )
        .unwrap_err(),
        b::BackupError::WrongKey
    );
    let mut t = object.clone();
    let n = t.len();
    t[n - 5] ^= 0x80;
    assert_eq!(
        b::decrypt_backup(&root, header.principal_id, &t).unwrap_err(),
        b::BackupError::Authentication
    );
}

/// Prints a fresh vector (run with `-- --ignored --nocapture` to regenerate the file).
#[test]
#[ignore]
fn print_vector() {
    let root = BackupRootKey::from_bytes(core::array::from_fn(|i| i as u8));
    let header = BackupHeader {
        key_id: root.key_id(),
        backup_id: Uuid::from_u128(0x0123_4567_89ab_4cde_8f01_2345_6789_abcd),
        principal_id: Uuid::from_u128(0xfedc_ba98_7654_4321_8fed_cba9_8765_4321),
        created_at_unix: 1_790_000_000,
    };
    let payload = BackupPayload {
        sections: vec![
            ("MESSAGES".into(), b"vector message history".to_vec()),
            ("MLS_STATE".into(), vec![0xAA; 40]),
            ("ATTACHMENT_DESCRIPTORS".into(), Vec::new()),
        ],
    };
    let salt: [u8; 32] = core::array::from_fn(|i| 0x40 + i as u8);
    let prefix: [u8; 19] = core::array::from_fn(|i| 0x80 + i as u8);
    let object = b::encrypt_backup_deterministic(&root, &header, &payload, salt, prefix).unwrap();
    let v = serde_json::json!({
        "format": b::BACKUP_FORMAT_V1,
        "rootKey": hex(root.as_bytes()),
        "keyId": hex(&root.key_id()),
        "recoveryCode": root.recovery_code(),
        "backupId": header.backup_id.to_string(),
        "principalId": header.principal_id.to_string(),
        "createdAtUnix": header.created_at_unix,
        "salt": hex(&salt),
        "noncePrefix": hex(&prefix),
        "sections": payload.sections.iter().map(|(n, d)| serde_json::json!({ "name": n, "dataHex": hex(d) })).collect::<Vec<_>>(),
        "objectHex": hex(&object),
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}
