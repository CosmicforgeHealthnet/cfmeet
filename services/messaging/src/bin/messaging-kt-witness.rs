//! A reference key-transparency WITNESS (AD-05): cosigns a log's tree heads only after
//! verifying the log signature and a consistency proof from the last head it cosigned,
//! and never cosigns two views of one log. It is meant to be run by a party INDEPENDENT
//! of the Messaging operator, with its own key and its own durable state file.
//!
//! | variable | meaning |
//! |---|---|
//! | `KT_WITNESS_ID` | this witness's id (`[a-z0-9][a-z0-9.:-]{2,63}`), signed into every cosignature |
//! | `KT_WITNESS_SIGNING_KEY_FILE` | PKCS#8 PEM Ed25519 private key FILE (the witness's secret) |
//! | `KT_WITNESS_LOGS_FILE` | JSON array `[{ "logId", "publicKey" }]`: the logs this witness pins |
//! | `KT_WITNESS_STATE_FILE` | durable state (what was cosigned, per log); must survive restarts and must NOT be restored from an older backup |
//! | `KT_WITNESS_BIND_ADDRESS` | listen address (default `127.0.0.1:8099`; terminate TLS in front of it) |

use std::{collections::HashMap, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::pkcs8::DecodePrivateKey;
use messaging::transparency::witness::{router, FileWitnessStore, Witness};

fn fail(what: &str) -> ! {
    eprintln!("messaging-kt-witness: {what}");
    std::process::exit(2);
}

fn var(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| fail(&format!("{name} is not set")))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinnedLog {
    log_id: String,
    public_key: String,
}

#[tokio::main]
async fn main() {
    let id = var("KT_WITNESS_ID");
    let pem = std::fs::read_to_string(var("KT_WITNESS_SIGNING_KEY_FILE"))
        .unwrap_or_else(|_| fail("KT_WITNESS_SIGNING_KEY_FILE cannot be read"));
    let key = ed25519_dalek::SigningKey::from_pkcs8_pem(&pem)
        .unwrap_or_else(|_| fail("KT_WITNESS_SIGNING_KEY_FILE is not a PKCS#8 PEM Ed25519 key"));
    let logs: Vec<PinnedLog> = serde_json::from_slice(
        &std::fs::read(var("KT_WITNESS_LOGS_FILE"))
            .unwrap_or_else(|_| fail("KT_WITNESS_LOGS_FILE cannot be read")),
    )
    .unwrap_or_else(|_| fail("KT_WITNESS_LOGS_FILE is not a JSON array of {logId, publicKey}"));
    let mut pinned = HashMap::new();
    for l in logs {
        let k: [u8; 32] = URL_SAFE_NO_PAD
            .decode(l.public_key.trim_end_matches('='))
            .ok()
            .and_then(|k| k.try_into().ok())
            .unwrap_or_else(|| fail("a pinned log publicKey is not a base64url Ed25519 key"));
        pinned.insert(l.log_id, k);
    }
    let store = FileWitnessStore::new(var("KT_WITNESS_STATE_FILE").into());
    let witness = Witness::new(&id, key, pinned, Box::new(store))
        .unwrap_or_else(|| fail("invalid KT_WITNESS_ID or pinned log id"));
    let public = URL_SAFE_NO_PAD.encode(witness.public_key());
    let bind = std::env::var("KT_WITNESS_BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8099".into());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|_| fail("cannot bind KT_WITNESS_BIND_ADDRESS"));
    eprintln!("messaging-kt-witness: {id} listening on {bind}; public key {public}");
    axum::serve(listener, router(Arc::new(witness)))
        .await
        .unwrap_or_else(|_| fail("server stopped"));
}
