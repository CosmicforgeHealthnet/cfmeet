//! Part V Appendix I.3 / H.4 / N ("Backups: correct restore with key; failure without
//! key; tamper detection; key rotation and deletion"): opt-in secure backups through the
//! REAL API. The client encrypts with the shared crypto core; the server stores, verifies
//! and serves an object it cannot decrypt.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use cf_messaging_crypto::backup::{self as b, BackupError, BackupRootKey};
use cf_object_storage::MemoryObjectStore;
use common::{flows::*, *};
use messaging::operations::{self, KillSwitch};
use serde_json::{json, Value};
use uuid::Uuid;

async fn harness() -> Option<(Harness, Arc<MemoryObjectStore>)> {
    let store = Arc::new(MemoryObjectStore::new());
    let s2 = store.clone();
    let h = Harness::build(move |s| s.storage = s2).await?;
    Some((h, store))
}

const SECRET: &[u8] = b"local message database: meet at the north gate 0731";

fn key_of(c: &Customer, id: &str) -> String {
    format!(
        "bak/{}/{}",
        c.context.scope.environment_id.simple(),
        Uuid::parse_str(id).unwrap().simple()
    )
}

#[tokio::test]
async fn backups_are_opaque_owner_only_restorable_with_the_key_and_retained() {
    let Some((h, store)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let device = h.active_device(&c, alice).await.device_id();
    let (root, bytes) = encrypted_backup(alice, &[("MESSAGES", SECRET), ("MLS_STATE", b"opaque")]);

    // Opt-in: refused until the application enables backups.
    let body = json!({ "deviceId": device, "size": bytes.len(), "sha256": cf_object_storage::sha256_hex(&bytes),
                       "format": b::BACKUP_FORMAT_V1 });
    let r = h
        .post(&c, Some(alice), "/v1/messaging/backups", body.clone())
        .await;
    assert_eq!(r.reason(), "BACKUPS_DISABLED", "{}", r.text);
    h.set_policy(
        &c,
        json!({ "backups": { "enabled": true, "retainCount": 2 } }),
    )
    .await;
    // Only the acting principal's own ACTIVE device.
    let r = h
        .post(&c, Some(bob), "/v1/messaging/backups", body.clone())
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // Upload, finalize, restore.
    let id = h.upload_backup(&c, &store, alice, device, &bytes).await;
    let f = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/messaging/backups/{id}/finalize"),
            json!({}),
        )
        .await;
    assert_eq!(f.status, StatusCode::OK, "{}", f.text);
    conforms("messaging/common/v1.schema.json#/$defs/Backup", &f.json);
    assert_eq!(f.json["status"], "READY");
    assert_eq!(f.json["serverCanDecrypt"], false);
    assert_eq!(f.json["keyId"], hex::encode(root.key_id()));
    let again = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/messaging/backups/{id}/finalize"),
            json!({}),
        )
        .await;
    assert_eq!(again.json, f.json, "finalize is idempotent");
    let latest = h.get(&c, Some(alice), "/v1/messaging/backups/latest").await;
    assert_eq!(latest.json["backupId"], json!(id), "{}", latest.text);
    let g = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/messaging/backups/{id}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/BackupDownloadGrant",
        &g.json,
    );
    let stored = store.get_raw(&key_of(&c, &id)).unwrap();
    assert_eq!(
        stored, bytes,
        "the server stores exactly the client's ciphertext"
    );
    // The user's recovery code restores it; no other key does (no server escrow).
    let restored = BackupRootKey::from_recovery_code(&root.recovery_code()).unwrap();
    let (_, payload) = b::decrypt_backup(&restored, alice, &stored).unwrap();
    assert_eq!(payload.sections[0].1, SECRET);
    assert_eq!(
        b::decrypt_backup(&BackupRootKey::generate(), alice, &stored).unwrap_err(),
        BackupError::WrongKey
    );

    // Nothing readable reaches the server's database or the stored object.
    let dump = h.dump().await;
    assert!(!dump.contains("north gate"), "plaintext in the database");
    assert!(!stored.windows(SECRET.len()).any(|w| w == SECRET));
    assert!(!dump.contains(&root.recovery_code()) && !dump.contains(&hex::encode(root.as_bytes())));

    // Other principals see nothing of it.
    assert_eq!(
        h.get(&c, Some(bob), "/v1/messaging/backups/latest")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    for path in [
        format!("/v1/messaging/backups/{id}/download-grants"),
        format!("/v1/messaging/backups/{id}/finalize"),
    ] {
        assert_eq!(
            h.post(&c, Some(bob), &path, json!({})).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        h.delete(&c, Some(bob), &format!("/v1/messaging/backups/{id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Key rotation: a new backup under a new root key; with retainCount 2 the oldest is
    // deleted once a third is READY, and its object is removed.
    let new_root = BackupRootKey::generate();
    let rotated = b::reencrypt(
        &root,
        &new_root,
        alice,
        Uuid::new_v4(),
        1_800_000_000,
        &stored,
    )
    .unwrap();
    let id2 = h.upload_backup(&c, &store, alice, device, &rotated).await;
    h.post(
        &c,
        Some(alice),
        &format!("/v1/messaging/backups/{id2}/finalize"),
        json!({}),
    )
    .await;
    let (_, third) = encrypted_backup(alice, &[("MESSAGES", b"later")]);
    let id3 = h.upload_backup(&c, &store, alice, device, &third).await;
    h.post(
        &c,
        Some(alice),
        &format!("/v1/messaging/backups/{id3}/finalize"),
        json!({}),
    )
    .await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_backups WHERE status = 'READY'")
            .await,
        2
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_backups WHERE id = '{id}' AND status = 'DELETED'"
        ))
        .await,
        1,
        "the oldest backup is superseded"
    );
    messaging::attachments::lifecycle::run_once(&h.state)
        .await
        .unwrap();
    assert!(
        store.get_raw(&key_of(&c, &id)).is_none(),
        "its object is deleted"
    );
    assert_eq!(
        b::decrypt_backup(&root, alice, &store.get_raw(&key_of(&c, &id2)).unwrap()).unwrap_err(),
        BackupError::WrongKey,
        "after rotation the old recovery code opens nothing that remains"
    );

    // Deletion by the owner.
    let d = h
        .delete(&c, Some(alice), &format!("/v1/messaging/backups/{id3}"))
        .await;
    assert_eq!(d.json["status"], "DELETED", "{}", d.text);
    let latest = h.get(&c, Some(alice), "/v1/messaging/backups/latest").await;
    assert_eq!(latest.json["backupId"], json!(id2));
}

#[tokio::test]
async fn uploads_that_are_not_the_principals_backup_are_rejected_and_removed() {
    let Some((h, store)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(
        &c,
        json!({ "backups": { "enabled": true, "maxBytes": 4096 } }),
    )
    .await;
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let device = h.active_device(&c, alice).await.device_id();
    let finalize = |id: String| {
        let h = &h;
        let c = &c;
        async move {
            h.post(
                c,
                Some(alice),
                &format!("/v1/messaging/backups/{id}/finalize"),
                json!({}),
            )
            .await
        }
    };
    // Another principal's backup uploaded by alice.
    let (_, bobs) = encrypted_backup(bob, &[("MESSAGES", b"x")]);
    let id = h.upload_backup(&c, &store, alice, device, &bobs).await;
    let r = finalize(id.clone()).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("INTEGRITY_CHECK_FAILED", "WRONG_PRINCIPAL"),
        "{}",
        r.text
    );
    // Not a backup at all (plaintext uploaded by a broken client).
    let plain = vec![b'A'; 200];
    let id2 = h.upload_backup(&c, &store, alice, device, &plain).await;
    assert_eq!(finalize(id2.clone()).await.reason(), "NOT_A_BACKUP");
    // A tampered backup still parses (the server cannot authenticate it): the client's
    // restore detects it.
    let (root, mut tampered) = encrypted_backup(alice, &[("MESSAGES", SECRET)]);
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    let id3 = h.upload_backup(&c, &store, alice, device, &tampered).await;
    assert_eq!(finalize(id3.clone()).await.json["status"], "READY");
    assert_eq!(
        b::decrypt_backup(&root, alice, &store.get_raw(&key_of(&c, &id3)).unwrap()).unwrap_err(),
        BackupError::Authentication
    );
    // Rejected uploads are never served and their objects are deleted.
    for rejected in [&id, &id2] {
        assert_eq!(
            h.post(
                &c,
                Some(alice),
                &format!("/v1/messaging/backups/{rejected}/download-grants"),
                json!({})
            )
            .await
            .reason(),
            "BACKUP_NOT_READY"
        );
    }
    messaging::attachments::lifecycle::run_once(&h.state)
        .await
        .unwrap();
    assert!(
        store.get_raw(&key_of(&c, &id)).is_none() && store.get_raw(&key_of(&c, &id2)).is_none()
    );
    // Size limit and format are checked at the intent.
    let r = h
        .post(&c, Some(alice), "/v1/messaging/backups",
              json!({ "deviceId": device, "size": 5000, "sha256": "0".repeat(64), "format": b::BACKUP_FORMAT_V1 }))
        .await;
    assert_eq!(r.reason(), "BACKUP_TOO_LARGE", "{}", r.text);
    let r = h
        .post(&c, Some(alice), "/v1/messaging/backups",
              json!({ "deviceId": device, "size": 500, "sha256": "0".repeat(64), "format": "PLAINTEXT" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

#[tokio::test]
async fn the_backup_kill_switch_stops_uploads_and_downloads() {
    let Some((h, store)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "backups": { "enabled": true } }))
        .await;
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await.device_id();
    let (_, bytes) = encrypted_backup(alice, &[("MESSAGES", b"x")]);
    let id = h.upload_backup(&c, &store, alice, device, &bytes).await;
    let env = c.context.scope.environment_id;
    let now = chrono::Utc::now();
    assert!(operations::set_switch(
        h.conn(),
        KillSwitch::Backups,
        Some(env),
        true,
        "INCIDENT",
        "ops-1",
        now
    )
    .await
    .unwrap());
    let refused: Vec<Value> = vec![
        h.post(&c, Some(alice), &format!("/v1/messaging/backups/{id}/finalize"), json!({})).await.json,
        h.get(&c, Some(alice), "/v1/messaging/backups/latest").await.json,
        h.post(&c, Some(alice), "/v1/messaging/backups",
               json!({ "deviceId": device, "size": bytes.len(), "sha256": cf_object_storage::sha256_hex(&bytes), "format": b::BACKUP_FORMAT_V1 }))
            .await
            .json,
    ];
    for r in refused {
        assert_eq!(r["error"]["reason"], "KILL_SWITCH_ENGAGED", "{r}");
    }
    assert!(operations::set_switch(
        h.conn(),
        KillSwitch::Backups,
        Some(env),
        false,
        "RESOLVED",
        "ops-1",
        now
    )
    .await
    .unwrap());
    let f = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/messaging/backups/{id}/finalize"),
            json!({}),
        )
        .await;
    assert_eq!(f.json["status"], "READY", "{}", f.text);
}
