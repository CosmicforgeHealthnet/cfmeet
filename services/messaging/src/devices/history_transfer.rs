//! Device-to-device history transfer (ADR 0052 AD-07): Messaging RELAYS an opaque sealed
//! history from a principal's existing device to its new device. It never sees a key
//! that opens it ([`cf_messaging_crypto::history_transfer`] is the client protocol).
//!
//! * `POST /v1/messaging/history-transfers` — the TARGET device's request: a single-use
//!   transfer public key and an expiry, signed by the target's DEVICE key. Both devices
//!   must be ACTIVE devices of the acting principal in the caller's environment; the
//!   signature is verified against the target's registered (transparency-logged) key.
//! * `PUT …/{id}/sealed-history` — the SOURCE device's sealed history; accepted only if
//!   its signature verifies under the source device key for exactly this transfer.
//! * `GET …/{id}/sealed-history`, `POST …/{id}/complete`, `POST …/{id}/cancel`.
//! * The sealed bytes are deleted on completion, cancellation, expiry (at most one hour)
//!   and when either device is revoked. Production E2EE stays disabled by configuration
//!   (`E2EE_DEVICES_DISABLED`).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use cf_messaging_crypto::history_transfer::{self as ht, TransferContext};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{load_device, DeviceRow};
use crate::{
    audit::{self, AuditEntry},
    conversations::service::{request_context, rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

/// Longest a transfer may stay open.
pub const MAX_TTL: chrono::Duration = chrono::Duration::hours(1);
const MIN_TTL: chrono::Duration = chrono::Duration::seconds(60);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTransfer {
    pub transfer_id: Uuid,
    pub source_device_id: Uuid,
    pub target_device_id: Uuid,
    pub transfer_public_key: String,
    pub expires_at: DateTime<Utc>,
    pub request_signature: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealedHistoryUpload {
    pub sealed_history: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelTransfer {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    principal_id: Uuid,
    source_device_id: Uuid,
    target_device_id: Uuid,
    transfer_public_key: Vec<u8>,
    request_signature: Vec<u8>,
    status: String,
    sealed_sha256: Option<Vec<u8>>,
    sealed_bytes: Option<i64>,
    end_reason: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    sealed_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, principal_id, source_device_id, target_device_id, transfer_public_key, \
     request_signature, status, sealed_sha256, sealed_bytes, end_reason, created_at, expires_at, sealed_at, ended_at";

fn view(r: &Row) -> Value {
    json!({
        "transferId": r.id,
        "principalId": r.principal_id,
        "sourceDeviceId": r.source_device_id,
        "targetDeviceId": r.target_device_id,
        "transferPublicKey": B64.encode(&r.transfer_public_key),
        "requestSignature": B64.encode(&r.request_signature),
        "status": r.status,
        "expiresAt": rfc3339(r.expires_at),
        "createdAt": rfc3339(r.created_at),
        "sealedAt": r.sealed_at.map(rfc3339),
        "endedAt": r.ended_at.map(rfc3339),
        "endReason": r.end_reason,
        "sealedBytes": r.sealed_bytes,
        "sealedSha256": r.sealed_sha256.as_ref().map(hex::encode),
    })
}

fn context(ctx: &TrustedContext, r: &Row) -> TransferContext {
    TransferContext {
        transfer_id: r.id,
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
        principal_id: r.principal_id,
        source_device_id: r.source_device_id,
        target_device_id: r.target_device_id,
    }
}

/// An ACTIVE device of the acting principal (anything else: NOT_FOUND).
async fn own_active<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
) -> Result<DeviceRow> {
    load_device(db, ctx, device_id, false)
        .await?
        .filter(|d| d.principal_id == acting.principal_id && d.status == "ACTIVE")
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// The acting principal's transfer (another principal's or scope's: NOT_FOUND).
async fn load<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    lock: bool,
) -> Result<Row> {
    db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_history_transfers WHERE id = $1 AND tenant_id = $2 AND application_id = $3 \
               AND environment_id = $4 AND principal_id = $5{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
        ],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

async fn audit_event(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    action: &'static str,
    id: Uuid,
    detail: Value,
) -> Result<()> {
    audit::record(
        &state.db,
        AuditEntry {
            ctx,
            principal_id: Some(acting.principal_id),
            action,
            target_kind: "HISTORY_TRANSFER",
            target_id: id,
            detail,
        },
        &request_context(),
        state.now(),
    )
    .await
}

/// `POST /v1/messaging/history-transfers`.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    req: CreateTransfer,
) -> Result<Value> {
    super::require_enabled(state)?;
    let now = state.now();
    if req.source_device_id == req.target_device_id {
        return Err(Error::validation("source and target devices must differ"));
    }
    if req.expires_at < now + MIN_TTL || req.expires_at > now + MAX_TTL {
        return Err(Error::validation(
            "expiresAt must be between 60 seconds and 1 hour from now",
        ));
    }
    let key = B64
        .decode(req.transfer_public_key.trim_end_matches('='))
        .ok()
        .filter(|k| k.len() == ht::TRANSFER_PUBLIC_KEY_LEN)
        .ok_or_else(|| Error::validation("transferPublicKey must be a 32-byte X25519 key"))?;
    let sig = B64
        .decode(req.request_signature.trim_end_matches('='))
        .map_err(|_| Error::validation("requestSignature must be base64url"))?;
    let _source = own_active(&state.db, ctx, acting, req.source_device_id).await?;
    let target = own_active(&state.db, ctx, acting, req.target_device_id).await?;
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
        &sig,
    ) {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "TRANSFER_SIGNATURE_INVALID",
        ));
    }
    let inserted = db::exec(
        &state.db,
        "INSERT INTO messaging_history_transfers (id, tenant_id, application_id, environment_id, principal_id, \
           source_device_id, target_device_id, transfer_public_key, request_signature, status, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'REQUESTED', $10, $11) ON CONFLICT DO NOTHING",
        vec![
            req.transfer_id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            req.source_device_id.into(),
            req.target_device_id.into(),
            key.into(),
            sig.into(),
            now.into(),
            req.expires_at.into(),
        ],
    )
    .await?;
    if inserted == 0 {
        // A replay of the same signed request is idempotent; anything else conflicts.
        if let Ok(existing) = load(&state.db, ctx, acting, req.transfer_id, false).await {
            if existing.target_device_id == req.target_device_id
                && existing.source_device_id == req.source_device_id
                && B64.encode(&existing.request_signature)
                    == req.request_signature.trim_end_matches('=')
            {
                return Ok(view(&existing));
            }
        }
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_ALREADY_OPEN"));
    }
    audit_event(
        state,
        ctx,
        acting,
        "HISTORY_TRANSFER_REQUESTED",
        req.transfer_id,
        json!({ "source_device_id": req.source_device_id, "target_device_id": req.target_device_id }),
    )
    .await?;
    crate::metrics::inc(
        "messaging_history_transfers_total",
        &[("event", "requested")],
    );
    Ok(view(
        &load(&state.db, ctx, acting, req.transfer_id, false).await?,
    ))
}

/// `GET /v1/messaging/history-transfers?deviceId=` — transfers of one own device.
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
) -> Result<Value> {
    super::require_enabled(state)?;
    own_active(&state.db, ctx, acting, device_id).await?;
    let rows: Vec<Row> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_history_transfers WHERE environment_id = $1 AND tenant_id = $2 \
               AND principal_id = $3 AND (source_device_id = $4 OR target_device_id = $4) ORDER BY created_at DESC LIMIT 50"
        ),
        vec![
            ctx.scope.environment_id.into(),
            ctx.scope.tenant_id.into(),
            acting.principal_id.into(),
            device_id.into(),
        ],
    )
    .await?;
    Ok(json!({ "transfers": rows.iter().map(view).collect::<Vec<_>>() }))
}

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    super::require_enabled(state)?;
    Ok(view(&load(&state.db, ctx, acting, id, false).await?))
}

fn open_or_gone(r: &Row, now: DateTime<Utc>, wanted: &str) -> Result<()> {
    if r.expires_at <= now && matches!(r.status.as_str(), "REQUESTED" | "SEALED") {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_EXPIRED"));
    }
    if r.status != wanted {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_STATE"));
    }
    Ok(())
}

/// `PUT /v1/messaging/history-transfers/{id}/sealed-history` (the source device).
pub async fn upload_sealed(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    body: SealedHistoryUpload,
) -> Result<Value> {
    super::require_enabled(state)?;
    let sealed = B64
        .decode(body.sealed_history.trim_end_matches('='))
        .map_err(|_| Error::validation("sealedHistory must be base64url"))?;
    if sealed.len() > state.devices.history_transfer_max_bytes {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "SEALED_HISTORY_TOO_LARGE",
        ));
    }
    let st = state.clone();
    let (ctx2, acting2) = (ctx.clone(), acting.clone());
    let digest: [u8; 32] = Sha256::digest(&sealed).into();
    let row = state
        .db
        .transaction::<_, Row, Error>(move |txn| {
            Box::pin(async move {
                let r = load(txn, &ctx2, &acting2, id, true).await?;
                open_or_gone(&r, st.now(), "REQUESTED")?;
                let source = own_active(txn, &ctx2, &acting2, r.source_device_id).await?;
                own_active(txn, &ctx2, &acting2, r.target_device_id).await?;
                if !ht::verify_sealed(&source.signature_public_key, &context(&ctx2, &r), &sealed) {
                    return Err(Error::reason(
                        ErrorCode::ValidationError,
                        "SEALED_HISTORY_SIGNATURE_INVALID",
                    ));
                }
                db::exec(
                    txn,
                    "UPDATE messaging_history_transfers SET status = 'SEALED', sealed_history = $2, sealed_sha256 = $3, \
                       sealed_bytes = $4, sealed_at = $5 WHERE id = $1 AND status = 'REQUESTED'",
                    vec![
                        id.into(),
                        sealed.clone().into(),
                        digest.to_vec().into(),
                        (sealed.len() as i64).into(),
                        st.now().into(),
                    ],
                )
                .await?;
                load(txn, &ctx2, &acting2, id, false).await
            })
        })
        .await
        .map_err(Error::from)?;
    audit_event(
        state,
        ctx,
        acting,
        "HISTORY_TRANSFER_SEALED",
        id,
        json!({ "sealed_bytes": row.sealed_bytes }),
    )
    .await?;
    crate::metrics::inc("messaging_history_transfers_total", &[("event", "sealed")]);
    Ok(view(&row))
}

#[derive(FromQueryResult)]
struct SealedRow {
    sealed_history: Option<Vec<u8>>,
}

/// `GET /v1/messaging/history-transfers/{id}/sealed-history` (the target device).
pub async fn download_sealed(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    super::require_enabled(state)?;
    let r = load(&state.db, ctx, acting, id, false).await?;
    open_or_gone(&r, state.now(), "SEALED")?;
    own_active(&state.db, ctx, acting, r.target_device_id).await?;
    let s: SealedRow = db::one(
        &state.db,
        "SELECT sealed_history FROM messaging_history_transfers WHERE id = $1",
        vec![id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    let bytes = s
        .sealed_history
        .ok_or(Error::reason(ErrorCode::Conflict, "TRANSFER_STATE"))?;
    Ok(json!({
        "transferId": id,
        "sourceDeviceId": r.source_device_id,
        "sealedHistory": B64.encode(&bytes),
        "sealedSha256": r.sealed_sha256.as_ref().map(hex::encode),
    }))
}

async fn end(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    from: &[&str],
    to: &'static str,
    reason: Option<String>,
) -> Result<Value> {
    super::require_enabled(state)?;
    let now = state.now();
    let r = load(&state.db, ctx, acting, id, false).await?;
    if !from.contains(&r.status.as_str()) {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_STATE"));
    }
    if to == "COMPLETED" && r.expires_at <= now {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_EXPIRED"));
    }
    let n = db::exec(
        &state.db,
        "UPDATE messaging_history_transfers SET status = $2, sealed_history = NULL, end_reason = $3, ended_at = $4 \
         WHERE id = $1 AND status = ANY($5)",
        vec![
            id.into(),
            to.into(),
            reason.clone().into(),
            now.into(),
            from.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
        ],
    )
    .await?;
    if n == 0 {
        return Err(Error::reason(ErrorCode::Conflict, "TRANSFER_STATE"));
    }
    audit_event(
        state,
        ctx,
        acting,
        if to == "COMPLETED" {
            "HISTORY_TRANSFER_COMPLETED"
        } else {
            "HISTORY_TRANSFER_CANCELLED"
        },
        id,
        json!({ "reason": reason }),
    )
    .await?;
    crate::metrics::inc(
        "messaging_history_transfers_total",
        &[(
            "event",
            if to == "COMPLETED" {
                "completed"
            } else {
                "cancelled"
            },
        )],
    );
    Ok(view(&load(&state.db, ctx, acting, id, false).await?))
}

/// `POST …/{id}/complete` — the target received and opened the history.
pub async fn complete(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    end(state, ctx, acting, id, &["SEALED"], "COMPLETED", None).await
}

/// `POST …/{id}/cancel` — either device (e.g. the comparison codes differed).
pub async fn cancel(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
    req: CancelTransfer,
) -> Result<Value> {
    let reason = match req.reason {
        None => "USER_CANCELLED".to_string(),
        Some(r)
            if r.len() <= 32
                && r.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                && !r.is_empty() =>
        {
            r
        }
        Some(_) => {
            return Err(Error::validation(
                "reason must be 1-32 characters of [A-Z_]",
            ))
        }
    };
    end(
        state,
        ctx,
        acting,
        id,
        &["REQUESTED", "SEALED"],
        "CANCELLED",
        Some(reason),
    )
    .await
}

/// Ends every open transfer of a revoked device (inside the revocation transaction).
pub async fn cancel_for_device<C: ConnectionTrait>(
    db: &C,
    device_id: Uuid,
    now: DateTime<Utc>,
) -> Result<u64> {
    Ok(db::exec(
        db,
        "UPDATE messaging_history_transfers SET status = 'CANCELLED', sealed_history = NULL, end_reason = 'DEVICE_REVOKED', \
           ended_at = $2 WHERE (source_device_id = $1 OR target_device_id = $1) AND status IN ('REQUESTED', 'SEALED')",
        vec![device_id.into(), now.into()],
    )
    .await?)
}

/// Expires open transfers past their window (the sealed bytes are deleted).
pub async fn expire_due(state: &AppState) -> Result<u64> {
    let n = db::exec(
        &state.db,
        "UPDATE messaging_history_transfers SET status = 'EXPIRED', sealed_history = NULL, end_reason = 'EXPIRED', \
           ended_at = $1 WHERE status IN ('REQUESTED', 'SEALED') AND expires_at <= $1",
        vec![state.now().into()],
    )
    .await?;
    if n > 0 {
        crate::metrics::inc("messaging_history_transfers_total", &[("event", "expired")]);
    }
    Ok(n)
}
