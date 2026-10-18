//! Part V C.5: the key-transparency log — proofs a client can verify independently with
//! the RFC 9162 algorithms of `cf-messaging-crypto`, append-only storage, explicit
//! UNWITNESSED status.

mod common;

use axum::http::StatusCode;
use cf_messaging_crypto::transparency::{leaf_hash, verify_consistency, verify_inclusion, Hash};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

fn hash(v: &Value) -> Hash {
    unb64(v).try_into().unwrap()
}

#[tokio::test]
async fn device_entries_have_verifiable_inclusion_and_consistency_proofs() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.send", "messages.read"]);
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let first = h.active_device(&c, alice).await;
    // The tree head a client saw after the first registration.
    let p0 = h
        .get(
            &c,
            Some(bob),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}",
                first.device_id()
            ),
        )
        .await;
    assert_eq!(p0.status, StatusCode::OK, "{}", p0.text);
    let (old_size, old_root) = (
        p0.json["treeHead"]["treeSize"].as_u64().unwrap(),
        hash(&p0.json["treeHead"]["rootHash"]),
    );
    assert_eq!(old_size, 1);
    // More history: other devices, and a revocation of the first.
    for _ in 0..5 {
        h.active_device(&c, bob).await;
    }
    h.post(
        &c,
        Some(alice),
        &format!("/v1/messaging/devices/{}/revoke", first.device_id()),
        json!({ "reason": "REPLACED" }),
    )
    .await;
    let p = h
        .get(
            &c,
            Some(bob),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}&fromTreeSize={old_size}",
                first.device_id()
            ),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/TransparencyProof",
        &p.json,
    );
    assert_eq!(p.json["treeHead"]["witnessStatus"], "UNWITNESSED");
    let size = p.json["treeHead"]["treeSize"].as_u64().unwrap();
    let root = hash(&p.json["treeHead"]["rootHash"]);
    assert_eq!(size, 7);
    let entries = p.json["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let kinds: Vec<_> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["DEVICE_REGISTERED", "DEVICE_REVOKED"]);
    for e in entries {
        let data = unb64(&e["leafData"]);
        // The leaf names this device and its exact public key.
        let leaf: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(leaf["deviceId"], first.device_id().to_string());
        assert_eq!(
            unb64(&leaf["signaturePublicKey"]),
            first.signature_public_key()
        );
        let proof: Vec<Hash> = e["inclusionProof"]
            .as_array()
            .unwrap()
            .iter()
            .map(hash)
            .collect();
        assert!(verify_inclusion(
            e["leafIndex"].as_u64().unwrap(),
            size,
            &leaf_hash(&data),
            &proof,
            &root
        ));
        // A substituted key would not verify.
        let mut forged = leaf.clone();
        forged["signaturePublicKey"] = json!(b64(&[7u8; 32]));
        assert!(!verify_inclusion(
            e["leafIndex"].as_u64().unwrap(),
            size,
            &leaf_hash(&serde_json::to_vec(&forged).unwrap()),
            &proof,
            &root
        ));
    }
    // The current tree extends the one the client saw (no rewritten history).
    let consistency: Vec<Hash> = p.json["consistency"]["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect();
    assert_eq!(hash(&p.json["consistency"]["fromRootHash"]), old_root);
    assert!(verify_consistency(
        old_size,
        size,
        &old_root,
        &root,
        &consistency
    ));
    // fromTreeSize beyond the tree is a validation error.
    let r = h
        .get(
            &c,
            Some(bob),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}&fromTreeSize=999",
                first.device_id()
            ),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn proofs_are_scoped_to_the_callers_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.send", "messages.read"]);
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await;
    let other = h.customer(&["messages.read"]);
    let eve = h.principal(&other);
    let r = h
        .get(
            &other,
            Some(eve),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}",
                device.device_id()
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h
        .get(
            &c,
            Some(alice),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}",
                Uuid::new_v4()
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_log_is_append_only_in_the_database() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.send"]);
    let alice = h.principal(&c);
    h.active_device(&c, alice).await;
    h.active_device(&c, alice).await;
    for sql in [
        "UPDATE messaging_transparency_leaves SET leaf_data = 'x'",
        "DELETE FROM messaging_transparency_leaves",
        "UPDATE messaging_transparency_nodes SET hash = hash",
        "DELETE FROM messaging_transparency_nodes",
        "UPDATE messaging_transparency_head SET tree_size = 0",
        "DELETE FROM messaging_transparency_head",
        "UPDATE messaging_audit SET action = 'NOTHING_HAPPENED'",
    ] {
        assert!(h.sql(sql).await.is_err(), "{sql} was allowed");
    }
}
