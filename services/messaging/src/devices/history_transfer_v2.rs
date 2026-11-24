//! Bounded immutable ciphertext relay for negotiated history transfer v2. This module
//! never receives an archive plaintext, transfer private key or MLS state. An independent
//! additive migration enables the capability without invalidating older image histories.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use cf_messaging_crypto::history_transfer::{paged as ht, TransferContext};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{history_transfer::CreateTransfer, load_device, DeviceRow};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

const COLUMNS: &str = "id,tenant_id,application_id,environment_id,principal_id,source_device_id,target_device_id,transfer_public_key,request_signature,status,snapshot_id,manifest_digest,chunk_count,total_decoded_bytes AS total_plaintext_bytes,stored_bytes,next_index,end_reason,created_at,expires_at,ended_at";
const MAX_STORED_BYTES: i64 = ht::MAX_TOTAL_PLAINTEXT as i64
    + (ht::MAX_CHUNKS as i64 + 1) * 121
    + ht::MAX_OBJECT_PLAINTEXT as i64;

#[derive(Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    principal_id: Uuid,
    source_device_id: Uuid,
    target_device_id: Uuid,
    transfer_public_key: Vec<u8>,
    request_signature: Vec<u8>,
    status: String,
    snapshot_id: Option<Uuid>,
    manifest_digest: Option<Vec<u8>>,
    chunk_count: Option<i32>,
    total_plaintext_bytes: Option<i64>,
    stored_bytes: i64,
    next_index: i32,
    end_reason: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

fn view(r: &Row) -> Value {
    json!({"protocolVersion":2,"transferId":r.id,"principalId":r.principal_id,
        "sourceDeviceId":r.source_device_id,"targetDeviceId":r.target_device_id,
        "transferPublicKey":B64.encode(&r.transfer_public_key),"requestSignature":B64.encode(&r.request_signature),
        "status":r.status,"snapshotId":r.snapshot_id,"manifestDigest":r.manifest_digest.as_ref().map(hex::encode),
        "chunkCount":r.chunk_count,"totalPlaintextBytes":r.total_plaintext_bytes,"storedBytes":r.stored_bytes,
        "nextIndex":r.next_index,"endReason":r.end_reason,"createdAt":rfc3339(r.created_at),
        "expiresAt":rfc3339(r.expires_at),"endedAt":r.ended_at.map(rfc3339)})
}

fn transfer_context(r: &Row) -> TransferContext {
    TransferContext {
        transfer_id: r.id,
        tenant_id: r.tenant_id,
        application_id: r.application_id,
        environment_id: r.environment_id,
        principal_id: r.principal_id,
        source_device_id: r.source_device_id,
        target_device_id: r.target_device_id,
    }
}

fn object_context(r: &Row, index: u32) -> Result<ht::ObjectContext> {
    let bad = || Error::reason(ErrorCode::Conflict, "TRANSFER_MANIFEST_REQUIRED");
    Ok(ht::ObjectContext {
        transfer: transfer_context(r),
        snapshot_id: r.snapshot_id.ok_or_else(bad)?,
        expires_at_unix: r.expires_at.timestamp(),
        manifest_digest: r
            .manifest_digest
            .as_deref()
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
        chunk_count: r.chunk_count.ok_or_else(bad)? as u32,
        total_plaintext_bytes: r.total_plaintext_bytes.ok_or_else(bad)? as u64,
        object_index: index,
    })
}

pub async fn capabilities(state: &AppState) -> Result<Value> {
    super::require_enabled(state)?;
    let ready = messaging_migration::history_transfer_v2_ready(&state.db).await?;
    Ok(
        json!({"historyTransferVersions":if ready {vec![1,2]} else {vec![1]},
        "maxChunkPlaintextBytes":ht::MAX_OBJECT_PLAINTEXT,"maxChunks":ht::MAX_CHUNKS,
        "maxTotalPlaintextBytes":ht::MAX_TOTAL_PLAINTEXT,"maxTtlSeconds":3600,
        "sourceApprovalRequired":true}),
    )
}

async fn enabled(state: &AppState) -> Result<()> {
    super::require_enabled(state)?;
    if !messaging_migration::history_transfer_v2_ready(&state.db).await? {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "HISTORY_TRANSFER_V2_UNAVAILABLE",
        ));
    }
    Ok(())
}

async fn load<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    lock: bool,
) -> Result<Row> {
    db::one(db,&format!("SELECT {COLUMNS} FROM messaging_history_transfers_v2 WHERE id=$1 AND tenant_id=$2 AND application_id=$3 AND environment_id=$4 AND principal_id=$5{}",if lock {" FOR UPDATE"} else {""}),
        vec![id.into(),ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),acting.principal_id.into()]).await?.ok_or(Error::code(ErrorCode::NotFound))
}

async fn active<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    lock: bool,
) -> Result<DeviceRow> {
    load_device(db, ctx, id, lock)
        .await?
        .filter(|d| d.principal_id == acting.principal_id && d.status == "ACTIVE")
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// Device rows precede the transfer lock, matching terminal device revocation's order.
async fn checked(
    txn: &DatabaseTransaction,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(Row, DeviceRow, DeviceRow)> {
    let initial = load(txn, ctx, acting, id, false).await?;
    let (lo, hi) = if initial.source_device_id < initial.target_device_id {
        (initial.source_device_id, initial.target_device_id)
    } else {
        (initial.target_device_id, initial.source_device_id)
    };
    let first = active(txn, ctx, acting, lo, true).await?;
    let second = active(txn, ctx, acting, hi, true).await?;
    let (source, target) = if first.id == initial.source_device_id {
        (first, second)
    } else {
        (second, first)
    };
    let r = load(txn, ctx, acting, id, true).await?;
    if r.expires_at <= now {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_EXPIRED"));
    }
    if !matches!(r.status.as_str(), "REQUESTED" | "SEALED") {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_STATE"));
    }
    Ok((r, source, target))
}

fn decode(bytes: &str) -> Result<Vec<u8>> {
    if bytes.len() > (ht::MAX_OBJECT_SEALED * 4).div_ceil(3) + 4 {
        return Err(Error::code(ErrorCode::PayloadTooLarge));
    }
    let out = B64
        .decode(bytes.trim_end_matches('='))
        .map_err(|_| Error::validation("sealed object must be base64url"))?;
    if out.len() > ht::MAX_OBJECT_SEALED {
        return Err(Error::code(ErrorCode::PayloadTooLarge));
    }
    Ok(out)
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: CreateTransfer,
) -> Result<Value> {
    enabled(state).await?;
    let now = state.now();
    if req.source_device_id == req.target_device_id
        || req.expires_at < now + chrono::Duration::seconds(60)
        || req.expires_at > now + super::history_transfer::MAX_TTL
    {
        return Err(Error::validation(
            "different devices and an expiry between 60 seconds and 1 hour are required",
        ));
    }
    let key = decode(&req.transfer_public_key)?;
    let signature = decode(&req.request_signature)?;
    let txn = state.db.begin().await?;
    let (lo, hi) = if req.source_device_id < req.target_device_id {
        (req.source_device_id, req.target_device_id)
    } else {
        (req.target_device_id, req.source_device_id)
    };
    active(&txn, ctx, acting, lo, true).await?;
    active(&txn, ctx, acting, hi, true).await?;
    let target = active(&txn, ctx, acting, req.target_device_id, false).await?;
    let tc = TransferContext {
        transfer_id: req.transfer_id,
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
        principal_id: acting.principal_id,
        source_device_id: req.source_device_id,
        target_device_id: req.target_device_id,
    };
    if !ht::verify_request(
        &target.signature_public_key,
        &tc,
        &key,
        req.expires_at.timestamp(),
        &signature,
    ) {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "TRANSFER_SIGNATURE_INVALID",
        ));
    }
    // Account advisory lock makes the finite concurrent-transfer quota race-free.
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 724))",
        vec![format!(
            "{}:{}:{}",
            ctx.scope.environment_id, ctx.scope.application_id, acting.principal_id
        )
        .into()],
    )
    .await?;
    #[derive(FromQueryResult)]
    struct Count {
        count: i64,
    }
    let count:Count=db::one(&txn,"SELECT count(*) AS count FROM messaging_history_transfers_v2 WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 AND status IN ('REQUESTED','SEALED') AND expires_at>$5",vec![ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),acting.principal_id.into(),now.into()]).await?.ok_or(Error::code(ErrorCode::ServiceUnavailable))?;
    if let Ok(existing) = load(&txn, ctx, acting, req.transfer_id, false).await {
        if existing.request_signature == signature
            && existing.transfer_public_key == key
            && existing.source_device_id == req.source_device_id
            && existing.target_device_id == req.target_device_id
            && existing.expires_at == req.expires_at
        {
            txn.commit().await?;
            return Ok(view(&existing));
        }
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_ALREADY_OPEN"));
    }
    if count.count >= 2 {
        return Err(Error::code(ErrorCode::QuotaExceeded));
    }
    let n=db::exec(&txn,"INSERT INTO messaging_history_transfers_v2(id,tenant_id,application_id,environment_id,principal_id,source_device_id,target_device_id,transfer_public_key,request_signature,status,created_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'REQUESTED',$10,$11) ON CONFLICT DO NOTHING",
        vec![req.transfer_id.into(),ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),acting.principal_id.into(),req.source_device_id.into(),req.target_device_id.into(),key.into(),signature.into(),now.into(),req.expires_at.into()]).await?;
    if n == 0 {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_ALREADY_OPEN"));
    }
    let r = load(&txn, ctx, acting, req.transfer_id, false).await?;
    txn.commit().await?;
    Ok(view(&r))
}

pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device: Uuid,
) -> Result<Value> {
    enabled(state).await?;
    active(&state.db, ctx, acting, device, false).await?;
    let rows:Vec<Row>=db::all(&state.db,&format!("SELECT {COLUMNS} FROM messaging_history_transfers_v2 WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 AND (source_device_id=$5 OR target_device_id=$5) ORDER BY created_at DESC LIMIT 50"),vec![ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),acting.principal_id.into(),device.into()]).await?;
    Ok(json!({"transfers":rows.iter().map(view).collect::<Vec<_>>()}))
}

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    enabled(state).await?;
    let r = load(&state.db, ctx, acting, id, false).await?;
    active(&state.db, ctx, acting, r.source_device_id, false).await?;
    active(&state.db, ctx, acting, r.target_device_id, false).await?;
    Ok(view(&r))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestUpload {
    pub snapshot_id: Uuid,
    pub manifest_digest: String,
    pub chunk_count: u32,
    pub total_plaintext_bytes: u64,
    pub sealed_manifest: String,
}

pub async fn put_manifest(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    body: ManifestUpload,
) -> Result<Value> {
    enabled(state).await?;
    let sealed = decode(&body.sealed_manifest)?;
    let digest = hex::decode(&body.manifest_digest)
        .map_err(|_| Error::validation("manifestDigest must be SHA-256 hex"))?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| Error::validation("manifestDigest must be SHA-256 hex"))?;
    let txn = state.db.begin().await?;
    let (mut r, source, _) = checked(&txn, ctx, acting, id, state.now()).await?;
    let c = ht::ObjectContext {
        transfer: transfer_context(&r),
        snapshot_id: body.snapshot_id,
        expires_at_unix: r.expires_at.timestamp(),
        manifest_digest: digest,
        chunk_count: body.chunk_count,
        total_plaintext_bytes: body.total_plaintext_bytes,
        object_index: ht::MANIFEST_INDEX,
    };
    if !ht::verify_object(&source.signature_public_key, &c, &sealed) {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "SEALED_HISTORY_SIGNATURE_INVALID",
        ));
    }
    if r.snapshot_id.is_some() {
        let previous = object_bytes(&txn, id, None).await?;
        if object_context(&r, ht::MANIFEST_INDEX)? != c || previous != sealed {
            return Err(Error::reason(
                ErrorCode::Conflict,
                "TRANSFER_OBJECT_IMMUTABLE",
            ));
        }
    } else {
        db::exec(&txn,"UPDATE messaging_history_transfers_v2 SET snapshot_id=$2,manifest_digest=$3,chunk_count=$4,total_decoded_bytes=$5,sealed_manifest=$6,stored_bytes=$7,status='SEALED' WHERE id=$1",vec![id.into(),body.snapshot_id.into(),digest.to_vec().into(),(body.chunk_count as i32).into(),(body.total_plaintext_bytes as i64).into(),sealed.clone().into(),(sealed.len() as i64).into()]).await?;
        r = load(&txn, ctx, acting, id, false).await?;
    }
    txn.commit().await?;
    Ok(view(&r))
}

async fn object_bytes<C: ConnectionTrait>(db: &C, id: Uuid, index: Option<u32>) -> Result<Vec<u8>> {
    #[derive(FromQueryResult)]
    struct Bytes {
        bytes: Option<Vec<u8>>,
    }
    let row: Option<Bytes> = if let Some(i) = index {
        db::one(db,"SELECT sealed_chunk AS bytes FROM messaging_history_transfer_chunks_v2 WHERE transfer_id=$1 AND chunk_index=$2",vec![id.into(),(i as i32).into()]).await?
    } else {
        db::one(
            db,
            "SELECT sealed_manifest AS bytes FROM messaging_history_transfers_v2 WHERE id=$1",
            vec![id.into()],
        )
        .await?
    };
    row.and_then(|r| r.bytes)
        .ok_or(Error::code(ErrorCode::NotFound))
}

pub async fn get_object(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    index: Option<u32>,
) -> Result<Value> {
    enabled(state).await?;
    let txn = state.db.begin().await?;
    let (r, _, _) = checked(&txn, ctx, acting, id, state.now()).await?;
    let object_index = index.unwrap_or(ht::MANIFEST_INDEX);
    if !object_context(&r, object_index)?.valid() {
        return Err(Error::code(ErrorCode::NotFound));
    }
    let bytes = object_bytes(&txn, id, index).await?;
    txn.commit().await?;
    let mut out = view(&r);
    out["objectIndex"] = json!(object_index);
    out["sealedObject"] = json!(B64.encode(&bytes));
    out["sealedSha256"] = json!(hex::encode(Sha256::digest(&bytes)));
    Ok(out)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkUpload {
    pub sealed_chunk: String,
}

pub async fn put_chunk(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    index: u32,
    body: ChunkUpload,
) -> Result<Value> {
    enabled(state).await?;
    let sealed = decode(&body.sealed_chunk)?;
    let txn = state.db.begin().await?;
    let (mut r, source, _) = checked(&txn, ctx, acting, id, state.now()).await?;
    let c = object_context(&r, index)?;
    if index == ht::MANIFEST_INDEX || !ht::verify_object(&source.signature_public_key, &c, &sealed)
    {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "SEALED_HISTORY_SIGNATURE_INVALID",
        ));
    }
    if let Ok(previous) = object_bytes(&txn, id, Some(index)).await {
        if previous != sealed {
            return Err(Error::reason(
                ErrorCode::Conflict,
                "TRANSFER_OBJECT_IMMUTABLE",
            ));
        }
    } else {
        if r.stored_bytes + sealed.len() as i64 > MAX_STORED_BYTES {
            return Err(Error::code(ErrorCode::QuotaExceeded));
        }
        db::exec(&txn,"INSERT INTO messaging_history_transfer_chunks_v2(transfer_id,chunk_index,sealed_chunk,sealed_sha256) VALUES($1,$2,$3,$4)",vec![id.into(),(index as i32).into(),sealed.clone().into(),Sha256::digest(&sealed).to_vec().into()]).await?;
        db::exec(
            &txn,
            "UPDATE messaging_history_transfers_v2 SET stored_bytes=stored_bytes+$2 WHERE id=$1",
            vec![id.into(), (sealed.len() as i64).into()],
        )
        .await?;
        r = load(&txn, ctx, acting, id, false).await?;
    }
    txn.commit().await?;
    Ok(view(&r))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Checkpoint {
    pub next_index: u32,
    pub signature: String,
}

pub async fn checkpoint(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    body: Checkpoint,
) -> Result<Value> {
    enabled(state).await?;
    let signature = decode(&body.signature)?;
    let txn = state.db.begin().await?;
    let (r, _, target) = checked(&txn, ctx, acting, id, state.now()).await?;
    let c = object_context(&r, ht::MANIFEST_INDEX)?;
    if !ht::verify_checkpoint(
        &target.signature_public_key,
        &c,
        body.next_index,
        &signature,
    ) {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "TRANSFER_SIGNATURE_INVALID",
        ));
    }
    #[derive(FromQueryResult)]
    struct Count {
        count: i64,
    }
    let count:Count=db::one(&txn,"SELECT count(*) AS count FROM messaging_history_transfer_chunks_v2 WHERE transfer_id=$1 AND chunk_index<$2",vec![id.into(),(body.next_index as i32).into()]).await?.ok_or(Error::code(ErrorCode::ServiceUnavailable))?;
    if count.count != body.next_index as i64 {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_INCOMPLETE"));
    }
    db::exec(
        &txn,
        "UPDATE messaging_history_transfers_v2 SET next_index=GREATEST(next_index,$2) WHERE id=$1",
        vec![id.into(), (body.next_index as i32).into()],
    )
    .await?;
    let r = load(&txn, ctx, acting, id, false).await?;
    txn.commit().await?;
    Ok(view(&r))
}

pub async fn end(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    complete: bool,
) -> Result<Value> {
    enabled(state).await?;
    let txn = state.db.begin().await?;
    let (r, _, _) = checked(&txn, ctx, acting, id, state.now()).await?;
    if complete && (r.chunk_count.is_none() || r.next_index != r.chunk_count.unwrap_or(-1)) {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_INCOMPLETE"));
    }
    db::exec(
        &txn,
        "DELETE FROM messaging_history_transfer_chunks_v2 WHERE transfer_id=$1",
        vec![id.into()],
    )
    .await?;
    db::exec(&txn,"UPDATE messaging_history_transfers_v2 SET status=$2,sealed_manifest=NULL,stored_bytes=0,ended_at=$3,end_reason=$4 WHERE id=$1",vec![id.into(),if complete {"COMPLETED"} else {"CANCELLED"}.into(),state.now().into(),if complete {None::<String>}else{Some("USER_CANCELLED".to_owned())}.into()]).await?;
    let r = load(&txn, ctx, acting, id, false).await?;
    txn.commit().await?;
    Ok(view(&r))
}

/// Called inside terminal revocation's transaction, after its device row lock.
pub async fn cancel_for_device<C: ConnectionTrait>(
    db: &C,
    device: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    if !messaging_migration::history_transfer_v2_ready(db).await? {
        return Ok(());
    }
    db::exec(db,"UPDATE messaging_history_transfers_v2 SET status='CANCELLED',sealed_manifest=NULL,stored_bytes=0,ended_at=$2,end_reason='DEVICE_REVOKED' WHERE (source_device_id=$1 OR target_device_id=$1) AND status IN ('REQUESTED','SEALED')",vec![device.into(),now.into()]).await?;
    db::exec(db,"DELETE FROM messaging_history_transfer_chunks_v2 WHERE transfer_id IN (SELECT id FROM messaging_history_transfers_v2 WHERE (source_device_id=$1 OR target_device_id=$1) AND status='CANCELLED')",vec![device.into()]).await?;
    Ok(())
}

pub async fn expire_due(state: &AppState) -> Result<()> {
    if !messaging_migration::history_transfer_v2_ready(&state.db).await? {
        return Ok(());
    }
    let txn = state.db.begin().await?;
    db::exec(&txn,"UPDATE messaging_history_transfers_v2 SET status='EXPIRED',sealed_manifest=NULL,stored_bytes=0,ended_at=$1,end_reason='EXPIRED' WHERE expires_at<=$1 AND status IN ('REQUESTED','SEALED')",vec![state.now().into()]).await?;
    db::exec(&txn,"DELETE FROM messaging_history_transfer_chunks_v2 WHERE transfer_id IN (SELECT id FROM messaging_history_transfers_v2 WHERE status='EXPIRED')",vec![]).await?;
    txn.commit().await?;
    Ok(())
}
