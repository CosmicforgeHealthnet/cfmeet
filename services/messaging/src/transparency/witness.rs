//! Key-transparency witnesses (AD-05): the interface that lets INDEPENDENT parties stop
//! the log from equivocating.
//!
//! * Protocol (`POST /witness/v1/add-checkpoint`): the log sends its signed tree head and
//!   a consistency proof from the size the witness last cosigned. The witness pins the
//!   log's public key, verifies the signature and the proof, refuses a smaller tree, a
//!   different root for a size it already cosigned, or a proof that does not verify, and
//!   only then cosigns ([`cf_messaging_crypto::transparency::witness_cosignature_input`])
//!   — after durably recording the new size. A witness therefore never cosigns two views.
//! * [`Witness`] is a reference witness (any operator can run it with their OWN key and
//!   storage: `messaging-kt-witness`); its independence comes from who runs it, not from
//!   this code. [`HttpWitness`] is the log-side client.
//! * The log side ([`submit_once`]) submits the current head to every configured witness,
//!   VERIFIES each returned cosignature under the witness's pinned key before storing it,
//!   and treats a witness's refusal (fork, rollback, invalid proof) or a witness that has
//!   cosigned a larger tree than the database holds as the CRITICAL key-transparency
//!   rollback incident — log appends then fail closed.
//! * Served tree heads carry the stored cosignatures of exactly that head; the status is
//!   `WITNESSED` only when a quorum of distinct configured witnesses cosigned it, and
//!   clients verify that themselves with
//!   [`cf_messaging_crypto::transparency::verify_witnessed_tree_head`].

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use cf_messaging_crypto::transparency::{self as kt, Hash};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};

use super::{valid_log_id, TreeHead};
use crate::{db, error::Result, state::AppState};

pub const ADD_CHECKPOINT_PATH: &str = "/witness/v1/add-checkpoint";

/// What the log submits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddCheckpoint {
    pub log_id: String,
    /// The size the log believes this witness last cosigned (0: never).
    pub old_size: u64,
    pub tree_size: u64,
    pub root_hash: String,
    pub timestamp_micros: i64,
    pub log_signature: String,
    pub consistency_proof: Vec<String>,
}

/// A witness's cosignature of one head.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CosignatureView {
    pub witness_id: String,
    pub tree_size: u64,
    pub root_hash: String,
    pub timestamp_micros: i64,
    pub cosigned_at_micros: i64,
    pub signature: String,
}

/// Why a witness refused to cosign.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    /// UNKNOWN_LOG, MALFORMED, BAD_LOG_SIGNATURE, SIZE_MISMATCH, ROLLBACK, FORK,
    /// INVALID_PROOF, STORAGE_UNAVAILABLE.
    pub error: String,
    /// With SIZE_MISMATCH / ROLLBACK: the size this witness has cosigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_tree_size: Option<u64>,
}

impl Refusal {
    fn new(error: &str) -> Self {
        Self {
            error: error.into(),
            witness_tree_size: None,
        }
    }
    fn at(error: &str, size: u64) -> Self {
        Self {
            error: error.into(),
            witness_tree_size: Some(size),
        }
    }
    /// A refusal that proves the log diverged (as opposed to a stale size or an outage).
    pub fn is_divergence(&self) -> bool {
        matches!(self.error.as_str(), "ROLLBACK" | "FORK" | "INVALID_PROOF")
    }
}

fn hash_of(s: &str) -> Option<Hash> {
    B64.decode(s).ok()?.try_into().ok()
}

// ---------------------------------------------------------------------------------------
// The witness
// ---------------------------------------------------------------------------------------

/// The last head a witness cosigned for one log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cosigned {
    pub tree_size: u64,
    #[serde(with = "hex_hash")]
    pub root_hash: Hash,
    pub timestamp_micros: i64,
}

mod hex_hash {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(h: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(h))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| serde::de::Error::custom("a 32-byte hex hash"))
    }
}

/// Durable witness state: what it has cosigned, per log. Must be written BEFORE a
/// cosignature is released, and must survive restarts (a witness that forgets can be
/// shown a fork).
pub trait WitnessStore: Send + Sync {
    fn load(&self, log_id: &str) -> std::io::Result<Option<Cosigned>>;
    fn store(&self, log_id: &str, state: Cosigned) -> std::io::Result<()>;
}

#[derive(Default)]
pub struct MemoryWitnessStore(Mutex<HashMap<String, Cosigned>>);

impl WitnessStore for MemoryWitnessStore {
    fn load(&self, log_id: &str) -> std::io::Result<Option<Cosigned>> {
        Ok(self.0.lock().unwrap().get(log_id).copied())
    }
    fn store(&self, log_id: &str, state: Cosigned) -> std::io::Result<()> {
        self.0.lock().unwrap().insert(log_id.to_string(), state);
        Ok(())
    }
}

/// A JSON file (written to `<file>.tmp`, fsynced, then renamed; mode 0600).
pub struct FileWitnessStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileWitnessStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read_all(&self) -> std::io::Result<HashMap<String, Cosigned>> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e),
        }
    }
}

impl WitnessStore for FileWitnessStore {
    fn load(&self, log_id: &str) -> std::io::Result<Option<Cosigned>> {
        let _g = self.lock.lock().unwrap();
        Ok(self.read_all()?.get(log_id).copied())
    }

    fn store(&self, log_id: &str, state: Cosigned) -> std::io::Result<()> {
        use std::io::Write as _;
        let _g = self.lock.lock().unwrap();
        let mut all = self.read_all()?;
        all.insert(log_id.to_string(), state);
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(&all)?)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, &self.path)
    }
}

/// A reference witness: one Ed25519 key, the logs it pins, its durable state.
pub struct Witness {
    id: String,
    key: ed25519_dalek::SigningKey,
    logs: HashMap<String, [u8; 32]>,
    store: Box<dyn WitnessStore>,
    /// Serialises check-and-advance per witness.
    lock: Mutex<()>,
}

impl Witness {
    /// `None` when the witness id or a pinned log id is invalid.
    pub fn new(
        id: &str,
        key: ed25519_dalek::SigningKey,
        logs: HashMap<String, [u8; 32]>,
        store: Box<dyn WitnessStore>,
    ) -> Option<Self> {
        if !valid_log_id(id) || logs.keys().any(|l| !valid_log_id(l)) {
            return None;
        }
        Some(Self {
            id: id.to_string(),
            key,
            logs,
            store,
            lock: Mutex::new(()),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Verifies and (only then) cosigns one checkpoint.
    pub fn add_checkpoint(
        &self,
        req: &AddCheckpoint,
        now: DateTime<Utc>,
    ) -> std::result::Result<CosignatureView, Refusal> {
        let Some(log_key) = self.logs.get(&req.log_id) else {
            return Err(Refusal::new("UNKNOWN_LOG"));
        };
        let (Some(root), Ok(sig)) = (hash_of(&req.root_hash), B64.decode(&req.log_signature))
        else {
            return Err(Refusal::new("MALFORMED"));
        };
        let proof: Option<Vec<Hash>> = req.consistency_proof.iter().map(|p| hash_of(p)).collect();
        let Some(proof) = proof else {
            return Err(Refusal::new("MALFORMED"));
        };
        let input =
            kt::tree_head_signing_input(&req.log_id, req.tree_size, &root, req.timestamp_micros);
        if !kt::verify_tree_head_signature(log_key, &input, &sig) {
            return Err(Refusal::new("BAD_LOG_SIGNATURE"));
        }
        let _g = self.lock.lock().unwrap();
        let last = self
            .store
            .load(&req.log_id)
            .map_err(|_| Refusal::new("STORAGE_UNAVAILABLE"))?;
        match last {
            None if req.old_size != 0 => return Err(Refusal::at("SIZE_MISMATCH", 0)),
            None => {}
            Some(l) if req.tree_size < l.tree_size => {
                return Err(Refusal::at("ROLLBACK", l.tree_size))
            }
            Some(l) if req.tree_size == l.tree_size => {
                if root != l.root_hash {
                    return Err(Refusal::at("FORK", l.tree_size));
                }
                if req.timestamp_micros != l.timestamp_micros {
                    return Err(Refusal::at("FORK", l.tree_size));
                }
            }
            Some(l) if req.old_size != l.tree_size => {
                return Err(Refusal::at("SIZE_MISMATCH", l.tree_size))
            }
            Some(l) => {
                if req.timestamp_micros < l.timestamp_micros {
                    return Err(Refusal::at("ROLLBACK", l.tree_size));
                }
                if !kt::verify_consistency(l.tree_size, req.tree_size, &l.root_hash, &root, &proof)
                {
                    return Err(Refusal::at("INVALID_PROOF", l.tree_size));
                }
            }
        }
        // Durable first: the cosignature exists only once the new state is recorded.
        self.store
            .store(
                &req.log_id,
                Cosigned {
                    tree_size: req.tree_size,
                    root_hash: root,
                    timestamp_micros: req.timestamp_micros,
                },
            )
            .map_err(|_| Refusal::new("STORAGE_UNAVAILABLE"))?;
        use ed25519_dalek::Signer as _;
        let at = now.timestamp_micros();
        let input = kt::witness_cosignature_input(
            &self.id,
            &req.log_id,
            req.tree_size,
            &root,
            req.timestamp_micros,
            at,
        );
        Ok(CosignatureView {
            witness_id: self.id.clone(),
            tree_size: req.tree_size,
            root_hash: req.root_hash.clone(),
            timestamp_micros: req.timestamp_micros,
            cosigned_at_micros: at,
            signature: B64.encode(self.key.sign(&input).to_bytes()),
        })
    }
}

/// The witness's HTTP surface (no authentication needed: it only cosigns heads that
/// verify under a pinned log key).
pub fn router(witness: Arc<Witness>) -> axum::Router {
    use axum::{
        extract::State,
        http::StatusCode,
        routing::{get, post},
        Json,
    };
    async fn add(
        State(w): State<Arc<Witness>>,
        Json(req): Json<AddCheckpoint>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        match w.add_checkpoint(&req, Utc::now()) {
            Ok(c) => (StatusCode::OK, Json(serde_json::to_value(c).unwrap())),
            Err(r) => {
                let status = match r.error.as_str() {
                    "SIZE_MISMATCH" => StatusCode::CONFLICT,
                    "STORAGE_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
                    "UNKNOWN_LOG" => StatusCode::NOT_FOUND,
                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                };
                (status, Json(serde_json::to_value(r).unwrap()))
            }
        }
    }
    async fn key(State(w): State<Arc<Witness>>) -> Json<serde_json::Value> {
        Json(
            serde_json::json!({ "witnessId": w.id(), "publicKey": B64.encode(w.public_key()),
            "signatureAlgorithm": "Ed25519" }),
        )
    }
    axum::Router::new()
        .route(ADD_CHECKPOINT_PATH, post(add))
        .route("/witness/v1/public-key", get(key))
        .with_state(witness)
}

// ---------------------------------------------------------------------------------------
// The log side
// ---------------------------------------------------------------------------------------

/// A witness this log submits to (its pinned key verifies what it returns).
#[derive(Debug, Clone)]
pub struct WitnessEndpoint {
    pub witness_id: String,
    pub url: String,
    pub public_key: [u8; 32],
}

/// The configured witnesses and the quorum a served head needs to be WITNESSED.
#[derive(Debug, Clone, Default)]
pub struct WitnessSet {
    pub endpoints: Vec<WitnessEndpoint>,
    pub quorum: usize,
    http: Option<reqwest::Client>,
}

impl WitnessSet {
    pub fn new(endpoints: Vec<WitnessEndpoint>, quorum: usize) -> Self {
        Self {
            endpoints,
            quorum,
            http: Some(
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("http client"),
            ),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.endpoints.is_empty() && self.quorum > 0
    }

    pub fn trusted(&self) -> Vec<kt::TrustedWitness> {
        self.endpoints
            .iter()
            .map(|e| kt::TrustedWitness {
                witness_id: e.witness_id.clone(),
                public_key: e.public_key,
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WitnessReport {
    pub cosigned: usize,
    pub already_current: usize,
    pub resynced: usize,
    pub unreachable: usize,
    pub refused: usize,
    pub divergence: usize,
}

#[derive(FromQueryResult)]
struct StateRow {
    tree_size: i64,
}

#[derive(FromQueryResult)]
pub struct CosignatureRow {
    pub witness_id: String,
    pub cosigned_at_micros: i64,
    pub signature: Vec<u8>,
}

/// The stored, verified cosignatures of exactly `head` from configured witnesses.
pub async fn cosignatures<C: ConnectionTrait>(
    db: &C,
    set: &WitnessSet,
    head: &TreeHead,
) -> Result<Vec<CosignatureRow>> {
    if set.endpoints.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = set.endpoints.iter().map(|e| e.witness_id.clone()).collect();
    Ok(db::all(
        db,
        "SELECT witness_id, cosigned_at_micros, signature FROM messaging_transparency_cosignatures \
          WHERE tree_size = $1 AND root_hash = $2 AND head_timestamp_micros = $3 AND witness_id = ANY($4) ORDER BY witness_id",
        vec![
            (head.tree_size as i64).into(),
            head.root_hash.to_vec().into(),
            head.timestamp.timestamp_micros().into(),
            ids.into(),
        ],
    )
    .await?)
}

async fn divergence(state: &AppState, witness_id: &str) {
    tracing::error!(
        witness_id,
        "a key-transparency witness disagrees with the database log"
    );
    crate::metrics::inc("messaging_kt_witness_divergence_total", &[]);
    crate::operations::raise(
        &state.db,
        crate::operations::IncidentCategory::KtRollbackDetected,
        &format!("witness:{witness_id}"),
        state.now(),
    )
    .await;
}

/// One submission pass of the current signed head to every configured witness.
pub async fn submit_once(state: &AppState, set: &WitnessSet) -> Result<WitnessReport> {
    let mut report = WitnessReport::default();
    let (Some(signer), Some(http)) = (&state.tree_heads, &set.http) else {
        return Ok(report);
    };
    let head = super::tree_head(&state.db).await?;
    let signed = signer.sign(&head);
    for w in &set.endpoints {
        let known: Option<StateRow> = db::one(
            &state.db,
            "SELECT tree_size FROM messaging_transparency_witness_state WHERE witness_id = $1",
            vec![w.witness_id.clone().into()],
        )
        .await?;
        let mut old = known.map(|k| k.tree_size as u64).unwrap_or(0);
        if old > head.tree_size {
            // The witness cosigned a larger tree than the database now holds.
            report.divergence += 1;
            divergence(state, &w.witness_id).await;
            continue;
        }
        let have = db::scalar_i64(
            &state.db,
            "SELECT count(*) FROM messaging_transparency_cosignatures WHERE witness_id = $1 AND tree_size = $2 \
               AND root_hash = $3 AND head_timestamp_micros = $4",
            vec![
                w.witness_id.clone().into(),
                (head.tree_size as i64).into(),
                head.root_hash.to_vec().into(),
                head.timestamp.timestamp_micros().into(),
            ],
        )
        .await?;
        if have > 0 {
            report.already_current += 1;
            continue;
        }
        // One resynchronisation round when the witness reports another size.
        let mut outcome = None;
        for _ in 0..2 {
            let proof = if old == 0 || old == head.tree_size {
                Vec::new()
            } else {
                super::consistency(&state.db, old, &head).await?
            };
            let req = AddCheckpoint {
                log_id: signed.log_id.clone(),
                old_size: old,
                tree_size: head.tree_size,
                root_hash: B64.encode(head.root_hash),
                timestamp_micros: head.timestamp.timestamp_micros(),
                log_signature: B64.encode(signed.signature),
                consistency_proof: proof.iter().map(|h| B64.encode(h)).collect(),
            };
            let r = http
                .post(format!(
                    "{}{ADD_CHECKPOINT_PATH}",
                    w.url.trim_end_matches('/')
                ))
                .json(&req)
                .send()
                .await;
            let Ok(r) = r else {
                outcome = Some(Err(None));
                break;
            };
            let status = r.status().as_u16();
            if status == 200 {
                outcome = Some(match r.json::<CosignatureView>().await {
                    Ok(c) => Ok(c),
                    Err(_) => Err(Some(Refusal::new("MALFORMED"))),
                });
                break;
            }
            let refusal: Refusal = r.json().await.unwrap_or(Refusal::new("MALFORMED"));
            match (refusal.error.as_str(), refusal.witness_tree_size) {
                ("SIZE_MISMATCH", Some(ws)) if ws <= head.tree_size && ws != old => {
                    report.resynced += 1;
                    old = ws;
                    continue;
                }
                ("SIZE_MISMATCH" | "ROLLBACK", Some(ws)) if ws > head.tree_size => {
                    outcome = Some(Err(Some(Refusal::at("ROLLBACK", ws))));
                    break;
                }
                _ => {
                    outcome = Some(Err(Some(refusal)));
                    break;
                }
            }
        }
        match outcome {
            Some(Ok(c)) => {
                // Never store what does not verify under the pinned witness key.
                let sig = B64.decode(&c.signature).unwrap_or_default();
                let input = kt::witness_cosignature_input(
                    &w.witness_id,
                    &signed.log_id,
                    head.tree_size,
                    &head.root_hash,
                    head.timestamp.timestamp_micros(),
                    c.cosigned_at_micros,
                );
                if c.witness_id != w.witness_id
                    || c.tree_size != head.tree_size
                    || !kt::verify_tree_head_signature(&w.public_key, &input, &sig)
                {
                    report.refused += 1;
                    tracing::warn!(witness_id = %w.witness_id, "witness returned an unverifiable cosignature");
                    continue;
                }
                let now = state.now();
                db::exec(
                    &state.db,
                    "INSERT INTO messaging_transparency_cosignatures (witness_id, tree_size, root_hash, head_timestamp_micros, \
                       cosigned_at_micros, signature, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
                    vec![
                        w.witness_id.clone().into(),
                        (head.tree_size as i64).into(),
                        head.root_hash.to_vec().into(),
                        head.timestamp.timestamp_micros().into(),
                        c.cosigned_at_micros.into(),
                        sig.into(),
                        now.into(),
                    ],
                )
                .await?;
                db::exec(
                    &state.db,
                    "INSERT INTO messaging_transparency_witness_state (witness_id, tree_size, root_hash, updated_at) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (witness_id) DO UPDATE SET tree_size = EXCLUDED.tree_size, \
                       root_hash = EXCLUDED.root_hash, updated_at = EXCLUDED.updated_at \
                     WHERE messaging_transparency_witness_state.tree_size <= EXCLUDED.tree_size",
                    vec![
                        w.witness_id.clone().into(),
                        (head.tree_size as i64).into(),
                        head.root_hash.to_vec().into(),
                        now.into(),
                    ],
                )
                .await?;
                report.cosigned += 1;
                crate::metrics::inc("messaging_kt_witness_cosignatures_total", &[]);
            }
            Some(Err(Some(r))) if r.is_divergence() => {
                report.divergence += 1;
                divergence(state, &w.witness_id).await;
            }
            Some(Err(Some(r))) => {
                report.refused += 1;
                tracing::warn!(witness_id = %w.witness_id, error = %r.error, "witness refused the checkpoint");
            }
            Some(Err(None)) | None => {
                report.unreachable += 1;
                tracing::warn!(witness_id = %w.witness_id, "witness unreachable");
            }
        }
    }
    Ok(report)
}

/// The witness submission worker.
pub async fn run(state: AppState, set: Arc<WitnessSet>, interval: Duration) {
    loop {
        if let Err(e) = submit_once(&state, &set).await {
            tracing::warn!(error = %e, "key-transparency witness pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
