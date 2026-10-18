//! The key-transparency log (Part V C.5): an append-only RFC 9162 Merkle tree of device
//! key events, stored as leaves plus perfect subtrees ([`cf_messaging_crypto::transparency`]).
//!
//! * Appends happen INSIDE the transaction of the device change they record, under the
//!   tree-head row lock, so a device key exists exactly when its leaf does and the log
//!   order is the commit order. (One head serializes all appends — sufficient for the
//!   foundation; a sharded / batched log is a later capacity decision.)
//! * Proofs are computed from O(log² n) stored nodes, never by rehashing the log.
//! * Status is explicit: [`WITNESS_STATUS`] — the log is verifiable (inclusion /
//!   consistency) but NOT independently witnessed, so it does not by itself prevent
//!   equivocation and is not production-certified key transparency (ADR 0045 / 0047).

use std::collections::{BTreeSet, HashMap};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_messaging_crypto::transparency::{self as kt, Hash, NodeId};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
};

/// Reported with every tree head and proof.
pub const WITNESS_STATUS: &str = "UNWITNESSED";

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

/// A signed-off view of the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeHead {
    pub tree_size: u64,
    pub root_hash: Hash,
}

#[derive(FromQueryResult)]
struct HeadRow {
    tree_size: i64,
    root_hash: Vec<u8>,
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
        "SELECT tree_size, root_hash FROM messaging_transparency_head WHERE id = 1 FOR UPDATE"
    } else {
        "SELECT tree_size, root_hash FROM messaging_transparency_head WHERE id = 1"
    };
    let row: HeadRow = db::one(db, sql, vec![])
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(TreeHead {
        tree_size: row.tree_size as u64,
        root_hash: hash32(&row.root_hash)?,
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
        vec![(size as i64).into(), root.to_vec().into(), now.into()],
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
