//! Owner-delivered exports ([`crate::exports`]; ADR 0068 phase A4).
//!
//! * `POST /internal/v1/admin/organizations/{id}/exports` — the admin bridge's
//!   `organizations.organization.export` (R4: `organization.administer`, verified
//!   `hq:<staff>`, `Idempotency-Key`, the two-person approval). Builds the export, keeps
//!   it encrypted, queues one link notice per ACTIVE OWNER and answers the reference only.
//! * `GET /internal/v1/operator/organizations/{id}/exports` and `.../exports/{exportId}` —
//!   the bridge's reads of the export state (`organization.operator-read`).
//! * `POST /internal/v1/organizations/{id}/exports/{exportId}/download` — the owner's
//!   single-use download behind the emailed link, relayed with the verified acting user.

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    admin_bridge::{self, command, BridgeCommand, Outcome, Risk, Subject},
    authorization::export_document,
    invitations::{new_token, token_hash},
    middleware::Caller,
    parse, respond,
};
use crate::{
    db,
    domain::{MembershipStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    exports::{self, ExportNotice, ExportRow, NewDelivery, NewExport},
    state::AppState,
    store,
};

/// The bridge command's body: nothing to choose (the export is always the whole
/// organization document, delivered to its owners). `{}` or empty.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportCommandBody {}

fn outcome(e: &ExportRow, deliveries: &[exports::DeliveryRow], now: DateTime<Utc>) -> Outcome {
    Outcome::new(
        e.effective_state(now).to_string(),
        Some(e.version),
        exports::resource(e, deliveries, now),
    )
}

fn unavailable(reason: &'static str) -> ApiError {
    tracing::warn!(
        reason,
        "owner-delivered export refused: delivery unavailable"
    );
    ApiError::new(ErrorCode::ExportDeliveryUnavailable)
}

/// `POST /internal/v1/admin/organizations/{id}/exports` — see the module documentation.
/// Refusals (nothing built or stored): 400 (no key, a body field), 403
/// (`APPROVAL_REQUIRED`, not the bridge), 404 (unknown organization), 409
/// (`EXPORT_DELIVERY_UNAVAILABLE`, `EXPORT_NO_RECIPIENT`, `IDEMPOTENCY_CONFLICT`).
pub async fn admin_request_export(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let b = BridgeCommand::required(&caller, &headers, Risk::R4)?;
    let _: (ExportCommandBody, Value) = parse(&body)?;
    let txn = state.db.begin().await?;
    if let Some(replay) =
        admin_bridge::replayed(&txn, &b, command::EXPORT, Subject::OrganizationExport, None).await?
    {
        let e = exports::by_id(&txn, replay.subject_id)
            .await?
            .filter(|e| e.organization_id == id)
            .ok_or(ApiError::new(ErrorCode::IdempotencyConflict))?;
        let ds = exports::deliveries(&txn, e.id).await?;
        let now = db::now(&txn).await?;
        txn.commit().await?;
        return Ok(outcome(&e, &ds, now).response());
    }
    let delivery = state
        .exports
        .as_ref()
        .ok_or_else(|| unavailable("NOT_CONFIGURED"))?;
    delivery.notices.available().map_err(unavailable)?;
    let (approver, approval_ref) = b
        .approval
        .clone()
        .ok_or(ApiError::new(ErrorCode::ApprovalRequired))?;
    // The organization row lock keeps the document consistent with one moment.
    store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let owners = exports::active_owners(&txn, id).await?;
    if owners.is_empty() {
        return Err(ApiError::new(ErrorCode::ExportNoRecipient));
    }
    let now = db::now(&txn).await?;
    let expires_at = now + exports::export_ttl();
    let doc = export_document(&txn, id).await?;
    let clear = serde_json::to_vec(&doc).map_err(|_| ApiError::new(ErrorCode::InternalError))?;
    let sha256 = exports::sha256_hex(&clear);
    let size_bytes = clear.len() as i64;
    let export_id = Uuid::new_v4();
    let sealed = delivery
        .vault
        .seal(export_id, id, &clear)
        .map_err(|_| ApiError::new(ErrorCode::InternalError))?;
    drop(clear);
    drop(doc);
    let e = exports::insert_export(
        &txn,
        &NewExport {
            id: export_id,
            organization_id: id,
            idempotency_key: b.key,
            operator_ref: &b.operator,
            approver_ref: &approver,
            approval_ref: &approval_ref,
            size_bytes,
            sha256: &sha256,
            sealed: &sealed,
            recipients: owners.len() as i32,
            now,
            expires_at,
        },
    )
    .await?;
    for owner in &owners {
        let delivery_id = Uuid::new_v4();
        let (token, hash) = new_token();
        let notice = delivery
            .notices
            .notice(&ExportNotice {
                delivery_id,
                export_id,
                organization_id: id,
                recipient: *owner,
                token: &token,
                occurred_at: now,
                expires_at,
            })
            .map_err(unavailable)?;
        exports::insert_delivery(
            &txn,
            NewDelivery {
                id: delivery_id,
                export_id,
                organization_id: id,
                recipient: *owner,
                token_hash: &hash,
                notice,
                now,
                expires_at,
            },
        )
        .await?;
    }
    store::audit(
        &txn,
        &caller.as_operator(),
        now,
        Some(id),
        "organization.export.requested",
        "ORGANIZATION_EXPORT",
        Some(export_id),
        json!({ "recipients": owners.len(), "sizeBytes": e.size_bytes, "expiresAt": store::rfc3339(expires_at) }),
    )
    .await?;
    let ds = exports::deliveries(&txn, export_id).await?;
    let result = outcome(&e, &ds, now);
    admin_bridge::record(
        &txn,
        &b,
        command::EXPORT,
        Subject::OrganizationExport,
        export_id,
        Some(id),
        &result,
        now,
    )
    .await?;
    txn.commit().await?;
    tracing::info!(export_id = %export_id, recipients = owners.len(), "owner-delivered export prepared");
    Ok(result.response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportListQuery {
    /// The previous page's `nextCursor` (opaque).
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size 1–100 (default 50).
    #[serde(default)]
    pub limit: Option<i64>,
}

fn cursor_of(e: &ExportRow) -> String {
    format!("{}.{}", e.created_at.timestamp_micros(), e.id)
}

fn parse_cursor(raw: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    raw.split_once('.')
        .and_then(|(t, id)| {
            Some((
                DateTime::<Utc>::from_timestamp_micros(t.parse().ok()?)?,
                Uuid::parse_str(id).ok()?,
            ))
        })
        .ok_or_else(|| ApiError::validation("cursor must be a previous page's nextCursor"))
}

async fn existing_org(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    Ok(())
}

/// `GET /internal/v1/operator/organizations/{id}/exports?cursor&limit` — the
/// organization's exports, newest first (`limit` 1–100, default 50), each the reference
/// the command answered plus its `version`.
pub async fn operator_list(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<ExportListQuery>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    let limit = match q.limit {
        None => 50,
        Some(n) if (1..=100).contains(&n) => n,
        Some(_) => return Err(ApiError::validation("limit must be between 1 and 100")),
    };
    let after = q.cursor.as_deref().map(parse_cursor).transpose()?;
    existing_org(&state, id).await?;
    let (at, after_id) = match after {
        Some((t, i)) => (Some(t), Some(i)),
        None => (None, None),
    };
    let rows: Vec<ExportRow> = db::all(
        &state.db,
        &format!(
            "SELECT {} FROM organization_exports WHERE organization_id = $1 \
               AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3)) \
             ORDER BY created_at DESC, id DESC LIMIT $4",
            exports::EXPORT_COLUMNS
        ),
        vec![id.into(), at.into(), after_id.into(), (limit + 1).into()],
    )
    .await?;
    let now = db::now(&state.db).await?;
    let more = rows.len() as i64 > limit;
    let mut items = Vec::new();
    for e in rows.iter().take(limit as usize) {
        let ds = exports::deliveries(&state.db, e.id).await?;
        let mut v = exports::resource(e, &ds, now);
        v["version"] = json!(e.version);
        items.push(v);
    }
    let next = if more {
        rows.get(limit as usize - 1).map(cursor_of)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}

/// `GET /internal/v1/operator/organizations/{id}/exports/{exportId}` — one export's state
/// (the reference the command answered plus its `version`). 404 for an unknown
/// organization or export.
pub async fn operator_detail(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, export_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    existing_org(&state, id).await?;
    let e = exports::load(&state.db, id, export_id, false)
        .await?
        .ok_or_else(|| ApiError::with_message(ErrorCode::NotFound, "The export was not found."))?;
    let ds = exports::deliveries(&state.db, e.id).await?;
    let now = db::now(&state.db).await?;
    let mut v = exports::resource(&e, &ds, now);
    v["version"] = json!(e.version);
    Ok(respond(StatusCode::OK, v))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadBody {
    /// The token of the emailed link.
    pub token: String,
}

#[derive(Debug, FromQueryResult)]
struct ClaimRow {
    id: Uuid,
    recipient_user_id: Uuid,
    state: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct SealedRow {
    state: String,
    sha256: String,
    key_id: Option<String>,
    nonce: Option<Vec<u8>>,
    ciphertext: Option<Vec<u8>>,
}

fn link_invalid() -> ApiError {
    ApiError::new(ErrorCode::ExportLinkInvalid)
}

/// `POST /internal/v1/organizations/{id}/exports/{exportId}/download` — the owner behind
/// an emailed link (`organization.read`, verified acting user). Body `{"token"}`. The
/// acting user must be the link's recipient and still an ACTIVE OWNER (in any lifecycle
/// state of the organization); the link must be unused and unexpired. Answers the export
/// document exactly as stored (its SHA-256 in `X-CF-Export-Sha256`) and uses the link up.
/// 404 `EXPORT_LINK_INVALID` (unknown, someone else's or used), 410
/// `EXPORT_LINK_EXPIRED`, 409 `EXPORT_DELIVERY_UNAVAILABLE` (exports not configured).
pub async fn download(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, export_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let user = caller.user()?;
    let (req, _): (DownloadBody, Value) = parse(&body)?;
    if !(16..=512).contains(&req.token.len()) {
        return Err(link_invalid());
    }
    let delivery = state
        .exports
        .as_ref()
        .ok_or(ApiError::new(ErrorCode::ExportDeliveryUnavailable))?;
    let txn = state.db.begin().await?;
    let owner = store::current_membership(&txn, id, user, false)
        .await?
        .is_some_and(|m| m.role() == Role::Owner && m.status() == MembershipStatus::Active);
    if !owner {
        return Err(link_invalid());
    }
    let link = db::one::<ClaimRow, _>(
        &txn,
        "SELECT id, recipient_user_id, state, expires_at FROM organization_export_deliveries \
          WHERE token_hash = $1 AND export_id = $2 AND organization_id = $3 FOR UPDATE",
        vec![token_hash(&req.token).into(), export_id.into(), id.into()],
    )
    .await?
    .filter(|l| l.recipient_user_id == user)
    .ok_or_else(link_invalid)?;
    let now = db::now(&txn).await?;
    match link.state.as_str() {
        "PENDING" | "SENT" if link.expires_at > now => {}
        "PENDING" | "SENT" | "EXPIRED" => return Err(ApiError::new(ErrorCode::ExportLinkExpired)),
        _ => return Err(link_invalid()),
    }
    let sealed: SealedRow = db::one(
        &txn,
        "SELECT state, sha256, key_id, nonce, ciphertext FROM organization_exports WHERE id = $1 AND organization_id = $2 FOR UPDATE",
        vec![export_id.into(), id.into()],
    )
    .await?
    .ok_or_else(link_invalid)?;
    let (Some(key_id), Some(nonce), Some(ciphertext)) =
        (&sealed.key_id, &sealed.nonce, &sealed.ciphertext)
    else {
        return Err(ApiError::new(ErrorCode::ExportLinkExpired));
    };
    if !matches!(sealed.state.as_str(), "READY" | "DELIVERED") {
        return Err(ApiError::new(ErrorCode::ExportLinkExpired));
    }
    let clear = delivery
        .vault
        .open(key_id, nonce, ciphertext, export_id, id)
        .map_err(|e| {
            tracing::error!(export_id = %export_id, error = %e, "owner-delivered export does not open");
            ApiError::new(ErrorCode::ServiceUnavailable)
        })?;
    if exports::sha256_hex(&clear) != sealed.sha256 {
        tracing::error!(export_id = %export_id, "owner-delivered export digest mismatch");
        return Err(ApiError::new(ErrorCode::ServiceUnavailable));
    }
    db::exec(
        &txn,
        "UPDATE organization_export_deliveries SET state = 'CLAIMED', claimed_at = $2, settled_at = $2, notice = NULL \
          WHERE id = $1",
        vec![link.id.into(), now.into()],
    )
    .await?;
    db::exec(
        &txn,
        "UPDATE organization_exports SET state = 'DELIVERED', delivered_at = COALESCE(delivered_at, $2), \
                version = version + CASE WHEN state = 'DELIVERED' THEN 0 ELSE 1 END, updated_at = $2 \
          WHERE id = $1",
        vec![export_id.into(), now.into()],
    )
    .await?;
    store::audit(
        &txn,
        &caller.as_user(),
        now,
        Some(id),
        "organization.export.downloaded",
        "ORGANIZATION_EXPORT",
        Some(export_id),
        json!({ "deliveryRef": link.id }),
    )
    .await?;
    txn.commit().await?;
    let mut response = Response::new(Body::from(clear));
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&sealed.sha256) {
        h.insert("x-cf-export-sha256", v);
    }
    Ok(response)
}
