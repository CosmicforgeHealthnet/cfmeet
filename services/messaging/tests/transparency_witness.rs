//! Key-transparency witnesses (AD-05 interface): Messaging submits its signed tree head
//! to independent witnesses (here: reference witnesses served over real HTTP), stores
//! only cosignatures that verify under each witness's pinned key, and serves a head as
//! WITNESSED only with a quorum. Witnesses refuse forks and rollbacks; a witness that has
//! cosigned a larger tree than the database holds (a restored backup) raises the CRITICAL
//! incident and log appends fail closed. Real independent witness OPERATORS are a
//! deployment matter outside this test.

mod common;

use std::{collections::HashMap, sync::Arc};

use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use cf_messaging_crypto::transparency::{self as kt, Cosignature, SignedHead};
use common::{flows::*, *};
use messaging::transparency::{
    witness::{
        router, submit_once, AddCheckpoint, FileWitnessStore, MemoryWitnessStore, Witness,
        WitnessEndpoint, WitnessSet, WitnessStore,
    },
    TreeHeadSigner,
};
use serde_json::{json, Value};

const LOG: &str = "cfmeet-messaging:test";

fn witness_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&rand::random())
}

async fn serve(app: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, task)
}

struct Witnesses {
    endpoints: Vec<WitnessEndpoint>,
    /// Each witness is reached through a cuttable gate (a real outage: live connections
    /// drop, new ones are refused).
    gates: Vec<common::gate::TcpGate>,
}

async fn witnesses(log_key: [u8; 32], n: usize) -> Witnesses {
    let mut endpoints = Vec::new();
    let mut gates = Vec::new();
    for i in 0..n {
        let w = Witness::new(
            &format!("witness-{i}.test"),
            witness_key(),
            HashMap::from([(LOG.to_string(), log_key)]),
            Box::new(MemoryWitnessStore::default()),
        )
        .unwrap();
        let public_key = w.public_key();
        let (url, _task) = serve(router(Arc::new(w))).await;
        let gate =
            common::gate::TcpGate::start(url.trim_start_matches("http://").parse().unwrap()).await;
        endpoints.push(WitnessEndpoint {
            witness_id: format!("witness-{i}.test"),
            url: format!("http://127.0.0.1:{}", gate.port),
            public_key,
        });
        gates.push(gate);
    }
    Witnesses { endpoints, gates }
}

async fn head_of(h: &Harness, c: &Customer, who: uuid::Uuid) -> Value {
    let r = h
        .get(
            c,
            Some(who),
            &format!("/v1/messaging/principals/{who}/devices"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/TreeHead",
        &r.json["treeHead"],
    );
    r.json["treeHead"].clone()
}

/// What a client does: verify the head and its cosignatures against keys IT pins.
fn client_verifies(
    head: &Value,
    log_key: &[u8; 32],
    set: &WitnessSet,
) -> Result<usize, kt::WitnessError> {
    let sig = B64.decode(head["signature"].as_str().unwrap()).unwrap();
    let cos: Vec<Cosignature> = head["cosignatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| Cosignature {
            witness_id: c["witnessId"].as_str().unwrap().into(),
            cosigned_at_micros: c["cosignedAtMicros"].as_i64().unwrap(),
            signature: B64.decode(c["signature"].as_str().unwrap()).unwrap(),
        })
        .collect();
    kt::verify_witnessed_tree_head(
        &SignedHead {
            log_id: head["logId"].as_str().unwrap(),
            tree_size: head["treeSize"].as_u64().unwrap(),
            root_hash: B64
                .decode(head["rootHash"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap(),
            timestamp_micros: head["timestampMicros"].as_i64().unwrap(),
            log_signature: &sig,
        },
        log_key,
        &cos,
        &set.trusted(),
        set.quorum,
    )
}

#[tokio::test]
async fn heads_are_witnessed_by_a_quorum_and_divergence_fails_closed() {
    let signer = Arc::new(TreeHeadSigner::generate(LOG));
    let log_key = signer.public_key();
    let ws = witnesses(log_key, 3).await;
    // A 4th "witness" that answers with an unverifiable cosignature.
    let liar = {
        async fn lie(axum::Json(req): axum::Json<AddCheckpoint>) -> axum::Json<Value> {
            axum::Json(
                json!({ "witnessId": "liar.test", "treeSize": req.tree_size, "rootHash": req.root_hash,
                "timestampMicros": req.timestamp_micros, "cosignedAtMicros": 1, "signature": B64.encode([7u8; 64]) }),
            )
        }
        let (url, _t) = serve(axum::Router::new().route(
            messaging::transparency::witness::ADD_CHECKPOINT_PATH,
            axum::routing::post(lie),
        ))
        .await;
        WitnessEndpoint {
            witness_id: "liar.test".into(),
            url,
            public_key: witness_key().verifying_key().to_bytes(),
        }
    };
    let mut endpoints = ws.endpoints.clone();
    endpoints.push(liar);
    let set = Arc::new(WitnessSet::new(endpoints, 2));
    let (s1, set1) = (signer.clone(), set.clone());
    let Some(h) = Harness::build(move |s| {
        s.tree_heads = Some(s1);
        s.witnesses = set1;
    })
    .await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let alice = h.principal(&c);
    h.active_device(&c, alice).await;
    h.active_device(&c, alice).await;

    // Before any submission: honest UNWITNESSED, the quorum is announced.
    let head = head_of(&h, &c, alice).await;
    assert_eq!(
        (
            head["witnessStatus"].as_str(),
            head["witnessQuorum"].as_u64()
        ),
        (Some("UNWITNESSED"), Some(2))
    );
    assert!(client_verifies(&head, &log_key, &set).is_err());

    // 1. First submission (size 0 -> n): three honest cosignatures; the liar's is dropped.
    let r = submit_once(&h.state, &set).await.unwrap();
    assert_eq!((r.cosigned, r.refused, r.divergence), (3, 1, 0), "{r:?}");
    let head = head_of(&h, &c, alice).await;
    assert_eq!(head["witnessStatus"], "WITNESSED", "{head}");
    assert_eq!(head["cosignatures"].as_array().unwrap().len(), 3);
    assert_eq!(client_verifies(&head, &log_key, &set), Ok(3));
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_transparency_cosignatures WHERE witness_id = 'liar.test'").await,
        0,
        "an unverifiable cosignature is never stored"
    );
    // A pass with nothing new is a no-op.
    assert_eq!(
        submit_once(&h.state, &set).await.unwrap().already_current,
        3
    );

    // 2. The log grows: the served head is UNWITNESSED until witnesses verify the
    // consistency proof from what they cosigned.
    let size_before = head["treeSize"].as_u64().unwrap();
    for _ in 0..5 {
        h.active_device(&c, alice).await;
    }
    let grown = head_of(&h, &c, alice).await;
    assert!(grown["treeSize"].as_u64().unwrap() > size_before);
    assert_eq!(grown["witnessStatus"], "UNWITNESSED");
    let r = submit_once(&h.state, &set).await.unwrap();
    assert_eq!(r.cosigned, 3, "{r:?}");
    assert_eq!(
        client_verifies(&head_of(&h, &c, alice).await, &log_key, &set),
        Ok(3)
    );

    // 3. One witness down: still a quorum (2 of 3); two down: truthfully UNWITNESSED.
    ws.gates[0].cut();
    h.active_device(&c, alice).await;
    let r = submit_once(&h.state, &set).await.unwrap();
    assert_eq!((r.cosigned, r.unreachable), (2, 1), "{r:?}");
    assert_eq!(
        client_verifies(&head_of(&h, &c, alice).await, &log_key, &set),
        Ok(2)
    );
    ws.gates[1].cut();
    h.active_device(&c, alice).await;
    let r = submit_once(&h.state, &set).await.unwrap();
    assert_eq!((r.cosigned, r.unreachable), (1, 2), "{r:?}");
    let head = head_of(&h, &c, alice).await;
    assert_eq!(head["witnessStatus"], "UNWITNESSED");
    assert_eq!(
        client_verifies(&head, &log_key, &set),
        Err(kt::WitnessError::BelowQuorum {
            valid: 1,
            quorum: 2
        })
    );
    let witnessed_size = head["treeSize"].as_u64().unwrap();

    // 4. A restored (older) database with the same log key: the surviving witness has
    // cosigned a larger tree, so Messaging raises the CRITICAL incident and appends stop.
    let (s2, set2) = (signer.clone(), set.clone());
    let Some(restored) = Harness::build(move |s| {
        s.tree_heads = Some(s2);
        s.witnesses = set2;
    })
    .await
    else {
        return;
    };
    let c2 = restored.customer(ALL_SCOPES);
    let bob = restored.principal(&c2);
    restored.active_device(&c2, bob).await;
    let r = submit_once(&restored.state, &set).await.unwrap();
    assert_eq!(r.divergence, 1, "{r:?}");
    assert_eq!(
        restored
            .scalar("SELECT count(*) FROM messaging_incidents WHERE category = 'KT_ROLLBACK_DETECTED' AND status = 'OPEN'")
            .await,
        1
    );
    let (_, reg) = restored.register_device(&c2, bob).await;
    assert_eq!(reg.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reg.text);
    assert!(
        reg.text.contains("KT_LOG_ROLLBACK_DETECTED"),
        "{}",
        reg.text
    );
    println!(
        "EVIDENCE kt_witness witnesses=3 quorum=2 witnessed_size={witnessed_size} forged_cosignature_rejected=1 rollback_detected=1"
    );
}

#[test]
fn witnesses_refuse_forks_rollbacks_bad_proofs_and_remember_across_restarts() {
    let log = TreeHeadSigner::generate(LOG);
    let other_log = TreeHeadSigner::generate(LOG);
    let dir = std::env::temp_dir().join(format!("cf-kt-witness-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = dir.join("state.json");
    let key = witness_key();
    let make = || {
        Witness::new(
            "witness-a.test",
            key.clone(),
            HashMap::from([(LOG.to_string(), log.public_key())]),
            Box::new(FileWitnessStore::new(state.clone())),
        )
        .unwrap()
    };
    // A small real tree to produce consistent heads / proofs.
    let leaves: Vec<kt::Hash> = (0u8..9).map(|i| kt::leaf_hash(&[i])).collect();
    let mut nodes: HashMap<kt::NodeId, kt::Hash> = HashMap::new();
    for (i, l) in leaves.iter().enumerate() {
        for (id, hsh) in kt::append(i as u64, *l, |id| {
            nodes
                .get(&id)
                .copied()
                .ok_or(kt::ProofError::MissingNode(id))
        })
        .unwrap()
        {
            nodes.insert(id, hsh);
        }
    }
    let root = |n: u64| {
        kt::root(n, |id| {
            nodes
                .get(&id)
                .copied()
                .ok_or(kt::ProofError::MissingNode(id))
        })
        .unwrap()
    };
    let proof = |a: u64, b: u64| {
        kt::consistency_proof(a, b, |id| {
            nodes
                .get(&id)
                .copied()
                .ok_or(kt::ProofError::MissingNode(id))
        })
        .unwrap()
    };
    let req = |signer: &TreeHeadSigner,
               old: u64,
               size: u64,
               root_hash: kt::Hash,
               ts: i64,
               proof: Vec<kt::Hash>| {
        let head = messaging::transparency::TreeHead {
            tree_size: size,
            root_hash,
            timestamp: chrono::DateTime::from_timestamp_micros(ts).unwrap(),
        };
        let s = signer.sign(&head);
        AddCheckpoint {
            log_id: LOG.into(),
            old_size: old,
            tree_size: size,
            root_hash: B64.encode(root_hash),
            timestamp_micros: ts,
            log_signature: B64.encode(s.signature),
            consistency_proof: proof.iter().map(|p| B64.encode(p)).collect(),
        }
    };
    let now = chrono::Utc::now();
    let w = make();
    let c = w
        .add_checkpoint(&req(&log, 0, 4, root(4), 10, vec![]), now)
        .unwrap();
    // The cosignature verifies under the witness key.
    let input =
        kt::witness_cosignature_input("witness-a.test", LOG, 4, &root(4), 10, c.cosigned_at_micros);
    assert!(kt::verify_tree_head_signature(
        &w.public_key(),
        &input,
        &B64.decode(&c.signature).unwrap()
    ));
    let err = |r: Result<_, messaging::transparency::witness::Refusal>| r.unwrap_err().error;
    // Another log key, a fork at the same size, a stale "old size", a bad proof.
    assert_eq!(
        err(w.add_checkpoint(&req(&other_log, 4, 6, root(6), 11, proof(4, 6)), now)),
        "BAD_LOG_SIGNATURE"
    );
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 4, 4, root(5), 10, vec![]), now)),
        "FORK"
    );
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 2, 6, root(6), 11, proof(2, 6)), now)),
        "SIZE_MISMATCH"
    );
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 4, 6, root(6), 11, proof(3, 6)), now)),
        "INVALID_PROOF"
    );
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 4, 7, [9u8; 32], 11, proof(4, 7)), now)),
        "INVALID_PROOF"
    );
    // A consistent extension is cosigned; a smaller tree afterwards is a rollback.
    w.add_checkpoint(&req(&log, 4, 9, root(9), 12, proof(4, 9)), now)
        .unwrap();
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 9, 6, root(6), 13, vec![]), now)),
        "ROLLBACK"
    );
    // Restarted from its durable state: it still remembers size 9.
    drop(w);
    let w = make();
    assert_eq!(
        err(w.add_checkpoint(&req(&log, 0, 6, root(6), 13, vec![]), now)),
        "ROLLBACK"
    );
    let again = w
        .add_checkpoint(&req(&log, 9, 9, root(9), 12, vec![]), now)
        .unwrap();
    assert_eq!(
        again.tree_size, 9,
        "re-cosigning the same head is idempotent"
    );
    assert!(FileWitnessStore::new(state.clone())
        .load(LOG)
        .unwrap()
        .is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
