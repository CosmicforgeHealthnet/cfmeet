//! The key-transparency log (Part V C.5): an append-only RFC 9162 Merkle tree of device
//! key events, stored as leaves plus perfect subtrees ([`cf_messaging_crypto::transparency`]).
//!
//! * Appends happen INSIDE the transaction of the device change they record, under the
//!   tree-head row lock, so a device key exists exactly when its leaf does and the log
//!   order is the commit order. (One head serializes all appends — sufficient for the
//!   foundation; a sharded / batched log is a later capacity decision.)
//! * Proofs are computed from O(log² n) stored nodes, never by rehashing the log.
//! * Every tree head served is SIGNED ([`TreeHeadSigner`], Part V G.5): Ed25519 over
//!   [`cf_messaging_crypto::transparency::tree_head_signing_input`] with a dedicated
//!   server key (§142) — never the service-authentication key. Without a configured
//!   signer the proof / directory endpoints fail closed; an unsigned head is never served.
//! * Status is explicit: a served head is `WITNESSED` only when a quorum of distinct
//!   configured witnesses cosigned exactly that head ([`witness`]); otherwise it is
//!   [`WITNESS_STATUS`] (`UNWITNESSED`): verifiable (inclusion / consistency, signed
//!   heads) but not, by itself, a defence against equivocation. Which independent
//!   operators run witnesses is a deployment decision (AD-05, ADR 0045 / 0047).

pub mod witness;

use std::collections::{BTreeSet, HashMap};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_messaging_crypto::transparency::{self as kt, Hash, NodeId};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use sha2::Digest as _;
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
};

/// Reported with a tree head that lacks a witness quorum.
pub const WITNESS_STATUS: &str = "UNWITNESSED";

/// Reported with a tree head cosigned by a quorum of configured witnesses.
pub const WITNESSED: &str = "WITNESSED";

/// Kind of a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    DeviceRegistered,
    DeviceRevoked,
}

impl EntryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryKind::DeviceRegistered => "DEVICE_REGISTERED",
            EntryKind::DeviceRevoked => "DEVICE_REVOKED",
        }
    }
}

/// The canonical, versioned content of a leaf (serialized once, stored as bytes, and
/// returned verbatim so clients hash exactly what the log hashed).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeafEntry {
    pub version: u8,
    pub kind: &'static str,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub device_id: Uuid,
    pub protocol: &'static str,
    pub ciphersuite: &'static str,
    /// base64url (no padding) of the 32-byte Ed25519 signature public key.
    pub signature_public_key: String,
    /// base64url of the MLS credential identity.
    pub credential_identity: String,
    pub at: String,
}

impl LeafEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: EntryKind,
        tenant_id: Uuid,
        application_id: Uuid,
        environment_id: Uuid,
        principal_id: Uuid,
        device_id: Uuid,
        signature_public_key: &[u8],
        credential_identity: &[u8],
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            version: 1,
            kind: kind.as_str(),
            tenant_id,
            application_id,
            environment_id,
            principal_id,
            device_id,
            protocol: cf_messaging_crypto::profile::PROTOCOL_ID,
            ciphersuite: cf_messaging_crypto::profile::CIPHERSUITE_NAME,
            signature_public_key: URL_SAFE_NO_PAD.encode(signature_public_key),
            credential_identity: URL_SAFE_NO_PAD.encode(credential_identity),
            at: at.to_rfc3339_opts(SecondsFormat::Micros, true),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a leaf entry always serializes")
    }
}

/// The log's current state (signed when served: [`TreeHeadSigner::sign`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeHead {
    pub tree_size: u64,
    pub root_hash: Hash,
    /// When this head was produced (the append that last changed it), microseconds.
    pub timestamp: DateTime<Utc>,
}

#[derive(FromQueryResult)]
struct HeadRow {
    tree_size: i64,
    root_hash: Vec<u8>,
    updated_at: DateTime<Utc>,
}

/// Truncates to the microsecond precision PostgreSQL stores (and the signature covers).
fn micros(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(t.timestamp_micros()).unwrap_or(t)
}

/// Signs tree heads for one log with a dedicated Ed25519 key (a server secret, §142).
pub struct TreeHeadSigner {
    log_id: String,
    key: ed25519_dalek::SigningKey,
    key_id: String,
}

impl std::fmt::Debug for TreeHeadSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeHeadSigner")
            .field("log_id", &self.log_id)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// A tree head with its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTreeHead {
    pub head: TreeHead,
    pub log_id: String,
    pub key_id: String,
    pub signature: [u8; 64],
}

/// Whether `log_id` is an acceptable log identifier (`[a-z0-9][a-z0-9.:-]{2,63}`).
pub fn valid_log_id(log_id: &str) -> bool {
    (3..=64).contains(&log_id.len())
        && log_id.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || (i > 0 && matches!(b, b'.' | b':' | b'-'))
        })
}

impl TreeHeadSigner {
    /// `None` when `log_id` is not a valid log identifier.
    pub fn new(log_id: &str, key: ed25519_dalek::SigningKey) -> Option<Self> {
        if !valid_log_id(log_id) {
            return None;
        }
        let public = key.verifying_key().to_bytes();
        let digest: [u8; 32] = sha2::Sha256::digest(public).into();
        let key_id = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Some(Self {
            log_id: log_id.to_owned(),
            key,
            key_id,
        })
    }

    /// A signer with a fresh random key (tests and local tooling only).
    pub fn generate(log_id: &str) -> Self {
        use rand::RngCore;
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(seed.as_mut());
        Self::new(log_id, ed25519_dalek::SigningKey::from_bytes(&seed)).expect("valid log id")
    }

    pub fn log_id(&self) -> &str {
        &self.log_id
    }

    /// First 8 bytes (hex) of SHA-256 of the public key: which pinned key signed.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The 32-byte Ed25519 public key clients pin.
    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn sign(&self, head: &TreeHead) -> SignedTreeHead {
        use ed25519_dalek::Signer as _;
        let input = kt::tree_head_signing_input(
            &self.log_id,
            head.tree_size,
            &head.root_hash,
            head.timestamp.timestamp_micros(),
        );
        SignedTreeHead {
            head: *head,
            log_id: self.log_id.clone(),
            key_id: self.key_id.clone(),
            signature: self.key.sign(&input).to_bytes(),
        }
    }
}

#[derive(FromQueryResult)]
struct NodeRow {
    level: i16,
    node_index: i64,
    hash: Vec<u8>,
}

fn hash32(bytes: &[u8]) -> Result<Hash> {
    bytes
        .try_into()
        .map_err(|_| Error::code(ErrorCode::InternalError))
}

async fn head<C: ConnectionTrait>(db: &C, lock: bool) -> Result<TreeHead> {
    let sql = if lock {
        "SELECT tree_size, root_hash, updated_at FROM messaging_transparency_head WHERE id = 1 FOR UPDATE"
    } else {
        "SELECT tree_size, root_hash, updated_at FROM messaging_transparency_head WHERE id = 1"
    };
    let row: HeadRow = db::one(db, sql, vec![])
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(TreeHead {
        tree_size: row.tree_size as u64,
        root_hash: hash32(&row.root_hash)?,
        timestamp: micros(row.updated_at),
    })
}

/// The current tree head.
pub async fn tree_head<C: ConnectionTrait>(db: &C) -> Result<TreeHead> {
    head(db, false).await
}

/// Loads the stored perfect subtrees `ids`.
async fn nodes<C: ConnectionTrait>(
    db: &C,
    ids: &BTreeSet<NodeId>,
) -> Result<HashMap<NodeId, Hash>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let levels: Vec<i16> = ids.iter().map(|(l, _)| *l as i16).collect();
    let indexes: Vec<i64> = ids.iter().map(|(_, i)| *i as i64).collect();
    let rows: Vec<NodeRow> = db::all(
        db,
        "SELECT n.level, n.node_index, n.hash FROM messaging_transparency_nodes n \
         JOIN unnest($1::smallint[], $2::bigint[]) AS w(level, node_index) \
           ON w.level = n.level AND w.node_index = n.node_index",
        vec![levels.into(), indexes.into()],
    )
    .await?;
    rows.into_iter()
        .map(|r| Ok(((r.level as u32, r.node_index as u64), hash32(&r.hash)?)))
        .collect()
}

fn lookup(
    map: &HashMap<NodeId, Hash>,
) -> impl FnMut(NodeId) -> std::result::Result<Hash, kt::ProofError> + '_ {
    move |id| map.get(&id).copied().ok_or(kt::ProofError::MissingNode(id))
}

fn proof_failure(e: kt::ProofError) -> Error {
    tracing::error!(error = %e, "transparency log inconsistent");
    Error::code(ErrorCode::InternalError)
}

/// Appends `entry` in the caller's transaction; returns its leaf index.
pub async fn append<C: ConnectionTrait>(
    db: &C,
    entry: &LeafEntry,
    now: DateTime<Utc>,
) -> Result<u64> {
    let current = head(db, true).await?;
    let index = current.tree_size;
    let data = entry.bytes();
    let leaf = kt::leaf_hash(&data);
    let siblings = nodes(db, &kt::required_for_append(index)).await?;
    let mut new_nodes = kt::append(index, leaf, lookup(&siblings)).map_err(proof_failure)?;
    db::exec(
        db,
        "INSERT INTO messaging_transparency_leaves (leaf_index, entry_kind, environment_id, principal_id, \
           device_id, leaf_data, leaf_hash, appended_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            (index as i64).into(),
            entry.kind.into(),
            entry.environment_id.into(),
            entry.principal_id.into(),
            entry.device_id.into(),
            data.into(),
            leaf.to_vec().into(),
            now.into(),
        ],
    )
    .await?;
    for ((level, node_index), hash) in &new_nodes {
        db::exec(
            db,
            "INSERT INTO messaging_transparency_nodes (level, node_index, hash) VALUES ($1, $2, $3)",
            vec![(*level as i16).into(), (*node_index as i64).into(), hash.to_vec().into()],
        )
        .await?;
    }
    let size = index + 1;
    let needed = kt::required_for_root(size);
    let mut map = nodes(db, &needed).await?;
    map.extend(new_nodes.drain(..));
    let root = kt::root(size, lookup(&map)).map_err(proof_failure)?;
    db::exec(
        db,
        "UPDATE messaging_transparency_head SET tree_size = $1, root_hash = $2, updated_at = $3 WHERE id = 1",
        vec![(size as i64).into(), root.to_vec().into(), micros(now).into()],
    )
    .await?;
    Ok(index)
}

/// An inclusion proof of one leaf against `head`.
pub async fn inclusion<C: ConnectionTrait>(
    db: &C,
    leaf_index: u64,
    head: &TreeHead,
) -> Result<Vec<Hash>> {
    let map = nodes(db, &kt::required_for_inclusion(leaf_index, head.tree_size)).await?;
    kt::inclusion_proof(leaf_index, head.tree_size, lookup(&map)).map_err(proof_failure)
}

/// A consistency proof between `old` and `head`.
pub async fn consistency<C: ConnectionTrait>(
    db: &C,
    old: u64,
    head: &TreeHead,
) -> Result<Vec<Hash>> {
    if old > head.tree_size {
        return Err(Error::validation(
            "fromTreeSize is larger than the current tree",
        ));
    }
    let map = nodes(db, &kt::required_for_consistency(old, head.tree_size)).await?;
    kt::consistency_proof(old, head.tree_size, lookup(&map)).map_err(proof_failure)
}

/// The root of an earlier tree size (for consistency checks by clients).
pub async fn root_at<C: ConnectionTrait>(db: &C, size: u64) -> Result<Hash> {
    let map = nodes(db, &kt::required_for_root(size)).await?;
    kt::root(size, lookup(&map)).map_err(proof_failure)
}

/// One stored leaf.
#[derive(Debug, Clone, FromQueryResult)]
pub struct LeafRow {
    pub leaf_index: i64,
    pub entry_kind: String,
    pub leaf_data: Vec<u8>,
}

/// The leaves of `device_id` within `environment_id`, oldest first.
pub async fn device_leaves<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    device_id: Uuid,
) -> Result<Vec<LeafRow>> {
    Ok(db::all(
        db,
        "SELECT leaf_index, entry_kind, leaf_data FROM messaging_transparency_leaves \
         WHERE environment_id = $1 AND device_id = $2 ORDER BY leaf_index LIMIT 16",
        vec![environment_id.into(), device_id.into()],
    )
    .await?)
}
