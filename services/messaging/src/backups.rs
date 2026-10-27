//! Opt-in secure backups (Part V Appendix I.3, H.4; ADR 0052 AD-07).
//!
//! A client encrypts its local state with the shared crypto core
//! (`cf_messaging_crypto::backup`, client feature) under a backup root key that only the
//! user holds (shown once as a recovery code) and uploads the resulting opaque object.
//! The server:
//!
//! * stores ciphertext it cannot decrypt — it never receives a key, a recovery code or
//!   content, and it reads only the object's public header (magic, root-key id, the
//!   client's backup id, the principal id);
//! * accepts an object only for the acting principal's own ACTIVE device, only when the
//!   tenant's policy enables backups, and only after verifying size, SHA-256, the format
//!   magic and that the header names the acting principal;
//! * keeps the newest `backups.retainCount` READY backups per principal and queues older
//!   ones for object deletion;
//! * serves `GET /latest` metadata and short-lived download grants to the same principal
//!   only.
//!
//! Without the root key nothing can be restored — there is no server-side escrow.
//!
//! | Route (under `/v1/messaging`) | Scope |
//! |---|---|
//! | `POST /backups` (upload intent) | `messages.send` |
//! | `POST /backups/{backupId}/finalize` | `messages.send` |
//! | `GET /backups/latest` | `messages.read` |
//! | `POST /backups/{backupId}/download-grants` | `messages.read` |
//! | `DELETE /backups/{backupId}` | `messages.send` |

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use cf_messaging_crypto::backup::{self as format, BACKUP_FORMAT_V1};

use crate::{
    conversations::{http::MESSAGES_READ, policy, service::rfc3339},
    db,
    error::{Error, ErrorCode, Result},
    http::{
        error::ApiError,
        middleware::{Acting, CustomerCaller},
    },
    identity::TrustedContext,
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    operations::{require_open, KillSwitch},
    state::AppState,
};

/// How long an upload intent stays valid.
const UPLOAD_TTL: chrono::Duration = chrono::Duration::minutes(30);
/// Unfinished intents a principal may hold at once.
const MAX_PENDING: i64 = 3;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/backups", post(create_http))
        .route("/backups/latest", get(latest_http))
        .route("/backups/:backup_id", delete(delete_http))
        .route("/backups/:backup_id/finalize", post(finalize_http))
        .route("/backups/:backup_id/download-grants", post(grant_http))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBackup {
    /// The acting principal's ACTIVE device that made the backup.
    pub device_id: Uuid,
    /// Exact byte size of the encrypted object.
    pub size: i64,
    /// Lower-case hex SHA-256 of the encrypted object.
    pub sha256: String,
    pub format: String,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct BackupRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub device_id: Uuid,
    pub format: String,
    pub size: i64,
    pub sha256: String,
    pub object_key: String,
    pub status: String,
    pub client_backup_id: Option<Uuid>,
    pub key_id: Option<String>,
    pub reject_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub upload_expires_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, principal_id, device_id, format, size, sha256, \
     object_key, status, client_backup_id, key_id, reject_reason, created_at, upload_expires_at, finalized_at";

fn view(b: &BackupRow) -> Value {
    json!({
        "backupId": b.id,
        "status": b.status,
        "format": b.format,
        "deviceId": b.device_id,
        "size": b.size,
        "sha256": b.sha256,
        "clientBackupId": b.client_backup_id,
        "keyId": b.key_id,
        "rejectReason": b.reject_reason,
        "createdAt": rfc3339(b.created_at),
        "finalizedAt": b.finalized_at.map(rfc3339),
        "serverCanDecrypt": false,
    })
}

fn object_key(environment: Uuid, id: Uuid) -> String {
    format!("bak/{}/{}", environment.simple(), id.simple())
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn enabled(state: &AppState, ctx: &TrustedContext) -> Result<policy::BackupPolicy> {
    require_open(&state.db, KillSwitch::Backups, ctx.scope.environment_id).await?;
    let p = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !p.backups.enabled {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "BACKUPS_DISABLED",
        ));
    }
    if !state.storage.configured() {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "BACKUP_STORAGE_NOT_CONFIGURED",
        ));
    }
    Ok(p.backups)
}

fn storage_error(e: cf_object_storage::StorageError) -> Error {
    match e {
        cf_object_storage::StorageError::NotConfigured => {
            Error::reason(ErrorCode::FeatureDisabled, "BACKUP_STORAGE_NOT_CONFIGURED")
        }
        _ => Error::code(ErrorCode::StorageUnavailable),
    }
}

/// The acting principal's own backup (anything else is NOT_FOUND).
async fn own(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<BackupRow> {
    db::one(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_backups WHERE id = $1 AND environment_id = $2 AND tenant_id = $3 \
               AND principal_id = $4 AND status <> 'DELETED'"
        ),
        vec![id.into(), ctx.scope.environment_id.into(), ctx.scope.tenant_id.into(), acting.principal_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))
}

/// `POST /v1/messaging/backups` — an upload intent for one encrypted backup object.
pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: CreateBackup,
) -> Result<Value> {
    let device = db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_devices WHERE id = $1 AND principal_id = $2 AND environment_id = $3 AND status = 'ACTIVE'",
        vec![request.device_id.into(), acting.principal_id.into(), ctx.scope.environment_id.into()],
    )
    .await?;
    if device == 0 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    let policy = enabled(state, ctx).await?;
    if request.format != BACKUP_FORMAT_V1 {
        return Err(Error::validation(format!(
            "format must be {BACKUP_FORMAT_V1}"
        )));
    }
    if !valid_sha(&request.sha256) {
        return Err(Error::validation(
            "sha256 must be 64 lower-case hex characters",
        ));
    }
    if request.size < (format::HEADER + 16) as i64 {
        return Err(Error::validation(
            "size is smaller than an encrypted backup",
        ));
    }
    if request.size > policy.max_bytes {
        return Err(Error::reason(
            ErrorCode::PayloadTooLarge,
            "BACKUP_TOO_LARGE",
        ));
    }
    let now = state.now();
    let id = Uuid::new_v4();
    let key = object_key(ctx.scope.environment_id, id);
    let expires = now + UPLOAD_TTL;
    let txn = state.db.begin().await?;
    // One principal's intents are serialized (the pending limit is exact).
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!(
            "backup:{}:{}",
            ctx.scope.environment_id, acting.principal_id
        )
        .into()],
    )
    .await?;
    let pending = db::scalar_i64(
        &txn,
        "SELECT count(*) FROM messaging_backups WHERE environment_id = $1 AND principal_id = $2 AND status = 'PENDING' \
           AND upload_expires_at > $3",
        vec![ctx.scope.environment_id.into(), acting.principal_id.into(), now.into()],
    )
    .await?;
    if pending >= MAX_PENDING {
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "TOO_MANY_PENDING_BACKUPS",
        ));
    }
    db::exec(
        &txn,
        "INSERT INTO messaging_backups (id, tenant_id, application_id, environment_id, principal_id, device_id, format, \
           size, sha256, object_key, status, created_at, upload_expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'PENDING', $11, $12)",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            request.device_id.into(),
            BACKUP_FORMAT_V1.into(),
            request.size.into(),
            request.sha256.clone().into(),
            key.clone().into(),
            now.into(),
            expires.into(),
        ],
    )
    .await?;
    txn.commit().await?;
    let w = state
        .storage
        .signed_write(
            &key,
            UPLOAD_TTL.to_std().unwrap_or_default(),
            now,
            request.size as u64,
            &request.sha256,
            "application/octet-stream",
        )
        .map_err(storage_error)?;
    crate::metrics::inc("messaging_backups_total", &[("outcome", "intent")]);
    Ok(json!({
        "backupId": id,
        "status": "PENDING",
        "upload": {
            "method": w.method,
            "url": w.url,
            "headers": w.headers.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
            "expiresAt": rfc3339(w.expires_at),
        },
    }))
}

/// `POST /v1/messaging/backups/{id}/finalize` — verifies the uploaded object and makes it
/// the principal's newest READY backup.
pub async fn finalize(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let row = own(state, ctx, acting, id).await?;
    let policy = enabled(state, ctx).await?;
    if row.status == "READY" {
        return Ok(view(&row)); // idempotent
    }
    if row.status != "PENDING" {
        return Err(Error::reason(ErrorCode::Conflict, "BACKUP_NOT_PENDING"));
    }
    let bytes = state.storage.get(&row.object_key, row.size as usize).await;
    let verdict: std::result::Result<format::BackupHeader, &'static str> = match bytes {
        Err(cf_object_storage::StorageError::Integrity) => Err("SIZE_MISMATCH"),
        Err(e) => return Err(storage_error(e)),
        Ok(None) => {
            return Err(Error::reason(ErrorCode::Conflict, "UPLOAD_MISSING"));
        }
        Ok(Some(b)) => {
            if b.len() as i64 != row.size {
                Err("SIZE_MISMATCH")
            } else if hex::encode(Sha256::digest(&b)) != row.sha256 {
                Err("DIGEST_MISMATCH")
            } else {
                match format::inspect(&b) {
                    Err(_) => Err("NOT_A_BACKUP"),
                    Ok(h) if h.principal_id != acting.principal_id => Err("WRONG_PRINCIPAL"),
                    Ok(h) => Ok(h),
                }
            }
        }
    };
    let now = state.now();
    let txn = state.db.begin().await?;
    match verdict {
        Err(reason) => {
            db::exec(
                &txn,
                "UPDATE messaging_backups SET status = 'REJECTED', reject_reason = $2, finalized_at = $3 WHERE id = $1 AND status = 'PENDING'",
                vec![row.id.into(), reason.into(), now.into()],
            )
            .await?;
            crate::attachments::lifecycle::enqueue_deletion(
                &txn,
                &row.object_key,
                row.environment_id,
                None,
                reason,
                now,
            )
            .await?;
            txn.commit().await?;
            crate::metrics::inc("messaging_backups_total", &[("outcome", "rejected")]);
            return Err(Error::reason(ErrorCode::IntegrityCheckFailed, reason));
        }
        Ok(header) => {
            db::exec(
                &txn,
                "UPDATE messaging_backups SET status = 'READY', client_backup_id = $2, key_id = $3, finalized_at = $4, \
                   ready_seq = nextval('messaging_backups_ready_seq') \
                 WHERE id = $1 AND status = 'PENDING'",
                vec![row.id.into(), header.backup_id.into(), hex::encode(header.key_id).into(), now.into()],
            )
            .await?;
            // Keep the newest `retainCount` READY backups of this principal.
            #[derive(FromQueryResult)]
            struct Old {
                id: Uuid,
                object_key: String,
            }
            let old: Vec<Old> = db::all(
                &txn,
                "UPDATE messaging_backups SET status = 'DELETED', deleted_at = $4 WHERE id IN ( \
                   SELECT id FROM messaging_backups WHERE environment_id = $1 AND principal_id = $2 AND status = 'READY' \
                   ORDER BY ready_seq DESC OFFSET $3) RETURNING id, object_key",
                vec![row.environment_id.into(), row.principal_id.into(), policy.retain_count.into(), now.into()],
            )
            .await?;
            for o in &old {
                crate::attachments::lifecycle::enqueue_deletion(
                    &txn,
                    &o.object_key,
                    row.environment_id,
                    None,
                    "BACKUP_SUPERSEDED",
                    now,
                )
                .await?;
                let _ = o.id;
            }
            txn.commit().await?;
            crate::metrics::inc("messaging_backups_total", &[("outcome", "ready")]);
        }
    }
    Ok(view(&own(state, ctx, acting, id).await?))
}

/// `GET /v1/messaging/backups/latest` — the acting principal's newest READY backup.
pub async fn latest(state: &AppState, ctx: &TrustedContext, acting: &Acting) -> Result<Value> {
    require_open(&state.db, KillSwitch::Backups, ctx.scope.environment_id).await?;
    let row: Option<BackupRow> = db::one(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM messaging_backups WHERE environment_id = $1 AND tenant_id = $2 AND principal_id = $3 \
               AND status = 'READY' ORDER BY ready_seq DESC LIMIT 1"
        ),
        vec![ctx.scope.environment_id.into(), ctx.scope.tenant_id.into(), acting.principal_id.into()],
    )
    .await?;
    row.map(|r| view(&r))
        .ok_or(Error::code(ErrorCode::NotFound))
}

/// `POST /v1/messaging/backups/{id}/download-grants` — a short-lived read of the opaque
/// object for its own principal.
pub async fn grant(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let row = own(state, ctx, acting, id).await?;
    require_open(&state.db, KillSwitch::Backups, ctx.scope.environment_id).await?;
    if row.status != "READY" {
        return Err(Error::reason(ErrorCode::Conflict, "BACKUP_NOT_READY"));
    }
    let now = state.now();
    let ttl = state.attachments.grant_ttl;
    let signed = state
        .storage
        .signed_read(&row.object_key, ttl, now)
        .map_err(storage_error)?;
    Ok(json!({
        "backupId": row.id,
        "method": "GET",
        "url": signed.url,
        "expiresAt": rfc3339(signed.expires_at),
        "size": row.size,
        "sha256": row.sha256,
    }))
}

/// `DELETE /v1/messaging/backups/{id}` — the object is queued for deletion at once.
pub async fn remove(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    id: Uuid,
) -> Result<Value> {
    let row = own(state, ctx, acting, id).await?;
    let now = state.now();
    let txn = state.db.begin().await?;
    db::exec(
        &txn,
        "UPDATE messaging_backups SET status = 'DELETED', deleted_at = $2 WHERE id = $1 AND status <> 'DELETED'",
        vec![row.id.into(), now.into()],
    )
    .await?;
    crate::attachments::lifecycle::enqueue_deletion(
        &txn,
        &row.object_key,
        row.environment_id,
        None,
        "BACKUP_DELETED",
        now,
    )
    .await?;
    txn.commit().await?;
    Ok(json!({ "backupId": row.id, "status": "DELETED" }))
}

// ------------------------------------------------------------------------- handlers

async fn create_http(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateBackup>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok((
        StatusCode::CREATED,
        Json(create(&state, caller.context(), &acting, body).await?),
    ))
}

async fn finalize_http(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(finalize(&state, caller.context(), &acting, id).await?))
}

async fn latest_http(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(latest(&state, caller.context(), &acting).await?))
}

async fn grant_http(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(grant(&state, caller.context(), &acting, id).await?))
}

async fn delete_http(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(remove(&state, caller.context(), &acting, id).await?))
}
