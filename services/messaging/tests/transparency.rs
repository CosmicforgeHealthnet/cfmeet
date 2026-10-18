//! Part V C.5: the key-transparency log — proofs a client can verify independently with
//! the RFC 9162 algorithms of `cf-messaging-crypto`, append-only storage, explicit
//! UNWITNESSED status.

mod common;

use axum::http::StatusCode;
use cf_messaging_crypto::transparency::{
    leaf_hash, tree_head_signing_input, verify_consistency, verify_inclusion,
    verify_tree_head_signature, Hash,
};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

fn hash(v: &Value) -> Hash {
    unb64(v).try_into().unwrap()
}

/// Verifies a served tree head against the log key a client pinned (Part V G.5).
fn signed_by(head: &Value, pinned: &[u8; 32]) -> bool {
    let input = tree_head_signing_input(
        head["logId"].as_str().unwrap(),
        head["treeSize"].as_u64().unwrap(),
        &hash(&head["rootHash"]),
        head["timestampMicros"].as_i64().unwrap(),
    );
    verify_tree_head_signature(pinned, &input, &unb64(&head["signature"]))
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
    let pinned = h.state.tree_heads.as_ref().unwrap().public_key();
    assert!(signed_by(&p0.json["treeHead"], &pinned));
    assert_eq!(p0.json["treeHead"]["logId"], "cfmeet-messaging:test");
    assert_eq!(p0.json["treeHead"]["signatureAlgorithm"], "Ed25519");
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
    assert!(signed_by(&p.json["treeHead"], &pinned));
    // A head presented with another root, size, log or time does not verify (a client
    // holding two validly signed heads for one size has proof of equivocation).
    for (field, value) in [
        ("rootHash", json!(b64(&[9u8; 32]))),
        ("treeSize", json!(6)),
        ("logId", json!("cfmeet-messaging:other")),
        (
            "timestampMicros",
            json!(p.json["treeHead"]["timestampMicros"].as_i64().unwrap() + 1),
        ),
    ] {
        let mut forged = p.json["treeHead"].clone();
        forged[field] = value;
        assert!(!signed_by(&forged, &pinned), "{field}");
    }
    // Another key (e.g. a substituted log) is not the pinned one.
    let other = messaging::transparency::TreeHeadSigner::generate("cfmeet-messaging:test");
    assert!(!signed_by(&p.json["treeHead"], &other.public_key()));
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

#[tokio::test]
async fn without_a_signing_key_no_tree_head_is_served() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = Harness::on(std::sync::Arc::new(db), |state| state.tree_heads = None);
    let c = h.customer(&["messages.send", "messages.read"]);
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await;
    for path in [
        format!(
            "/v1/messaging/key-transparency/proofs?deviceId={}",
            device.device_id()
        ),
        format!("/v1/messaging/principals/{alice}/devices"),
    ] {
        let r = h.get(&c, Some(alice), &path).await;
        assert_eq!(
            r.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {}",
            r.text
        );
        assert!(!r.text.contains("rootHash"));
    }
}
