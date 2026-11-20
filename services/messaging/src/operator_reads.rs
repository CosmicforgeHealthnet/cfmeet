//! The admin bridge's operator reads of CF Meet chats (ADR 0068, design §6.3; MSG-017):
//! `/internal/v1/operator/*`. Only `cfmeet.admin-bridge` can hold
//! `messaging-firstparty.administer`, and every request must name the HQ staff member it
//! reads for (`X-CF-Operator`, parsed and checked in the internal-auth middleware).
//!
//! | Route | Data |
//! |---|---|
//! | `GET /operator/reports?status&limit&cursor` | reports, newest first |
//! | `GET /operator/reports/{reportId}` | one report and its moderation actions |
//! | `GET /operator/holds?status&limit&cursor` | legal holds (ACTIVE by default) |
//! | `GET /operator/exports?limit&cursor` | MANAGED conversation exports |
//! | `GET /operator/deletion-requests?limit&cursor` | erasure / purge requests |
//! | `GET /operator/audit?since&until&limit&cursor` | tenant audit (no detail) |
//! | `GET /operator/switches` | emergency off-switches |
//! | `GET /operator/incidents?since&until&limit&cursor` | operational incidents |
//! | `GET /operator/failed-messages?since&until&limit&cursor` | outbox dead letters |
//! | `GET /operator/key-transparency` | tree head and witness state |
//!
//! Privacy (ADR 0068, design §12): ids, categories, states, counts and times only. Never
//! message content, ciphertext, MLS state, keys, attachments, report evidence (sealed or
//! referenced), audit `detail`, outbox payloads, export object keys or digests, legal case
//! references, free-text summaries or error strings. Read-only.

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, TimeZone, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    state::AppState,
};

/// Default and largest page size.
pub const DEFAULT_LIMIT: u64 = 50;
pub const MAX_LIMIT: u64 = 100;
/// Longest `since`..`until` window.
pub const MAX_WINDOW_DAYS: i64 = 31;

/// An optional `since` / `until` window.
pub type Window = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Routes nested under `/internal/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/reports", get(reports))
        .route("/operator/reports/:reportId", get(report))
        .route("/operator/holds", get(holds))
        .route("/operator/exports", get(exports))
        .route("/operator/deletion-requests", get(deletion_requests))
        .route("/operator/audit", get(audit))
        .route("/operator/switches", get(switches))
        .route("/operator/incidents", get(incidents))
        .route("/operator/failed-messages", get(failed_messages))
        .route("/operator/key-transparency", get(key_transparency))
}

/// The admin bridge with its grant and a named HQ operator, or 403.
fn require_bridge(caller: &InternalCaller) -> Result<(), ApiError> {
    caller.require_permission(InternalPermission::MessagingFirstpartyAdminister)?;
    if caller.operator().is_none() {
        return Err(Error::code(ErrorCode::InternalPermissionDenied));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

fn limit(q: &PageQuery) -> Result<u64, ApiError> {
    match q.limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(Error::validation("limit must be between 1 and 100")),
    }
}

/// Keyset cursor `<unix micros>.<uuid>` (the bridge checks the same form).
pub fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}.{}", at.timestamp_micros(), id)
}

pub fn decode_cursor(raw: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (micros, id) = raw.split_once('.')?;
    let micros: i64 = micros.parse().ok()?;
    let at = Utc.timestamp_micros(micros).single()?;
    let id = Uuid::parse_str(id).ok()?;
    (id.hyphenated().to_string() == id.to_string()).then_some((at, id))
}

fn cursor(q: &PageQuery) -> Result<(Option<DateTime<Utc>>, Option<Uuid>), ApiError> {
    match q.cursor.as_deref() {
        None => Ok((None, None)),
        Some(raw) => decode_cursor(raw)
            .map(|(at, id)| (Some(at), Some(id)))
            .ok_or_else(|| Error::validation("cursor is invalid")),
    }
}

fn time(value: Option<&str>, name: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
    value
        .map(|v| {
            DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| Error::validation(format!("{name} must be an RFC 3339 timestamp")))
        })
        .transpose()
}

/// Optional `since` / `until`; when both are given `until` is after `since` and the window
/// is at most 31 days.
pub fn window(q: &PageQuery) -> Result<Window, ApiError> {
    let (since, until) = (
        time(q.since.as_deref(), "since")?,
        time(q.until.as_deref(), "until")?,
    );
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(Error::validation(
                "until must be after since and the window at most 31 days",
            ));
        }
    }
    Ok((since, until))
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn opt_rfc(t: Option<DateTime<Utc>>) -> Value {
    t.map(|t| Value::String(rfc(t))).unwrap_or(Value::Null)
}

/// `{items, nextCursor}` from rows fetched with `limit + 1`.
fn page<T>(
    rows: Vec<T>,
    limit: u64,
    key: impl Fn(&T) -> (DateTime<Utc>, Uuid),
    item: impl Fn(&T) -> Value,
) -> Value {
    let more = rows.len() as u64 > limit;
    let shown: Vec<&T> = rows.iter().take(limit as usize).collect();
    let next = if more {
        shown.last().map(|r| {
            let (at, id) = key(r);
            Value::String(encode_cursor(at, id))
        })
    } else {
        None
    };
    json!({ "items": shown.iter().map(|r| item(r)).collect::<Vec<_>>(), "nextCursor": next.unwrap_or(Value::Null) })
}

// ---- Reports ---------------------------------------------------------------------

const REPORT_COLUMNS: &str = "id, tenant_id, application_id, environment_id, conversation_id, security_profile, \
     reporter_principal_id, reported_principal_id, message_ids, category, evidence_kind, status, resolution, \
     resolved_at, created_at";

#[derive(Debug, FromQueryResult)]
struct ReportRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    security_profile: String,
    reporter_principal_id: Uuid,
    reported_principal_id: Option<Uuid>,
    message_ids: Vec<Uuid>,
    category: String,
    evidence_kind: String,
    status: String,
    resolution: Option<String>,
    resolved_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl ReportRow {
    /// Ids, category, states and times. The evidence itself (a sealed reporter disclosure
    /// or a MANAGED message reference) is never relayed; `evidenceKind` only says which.
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "conversationId": self.conversation_id,
            "securityProfile": self.security_profile,
            "reporterPrincipalId": self.reporter_principal_id,
            "reportedPrincipalId": self.reported_principal_id,
            "messageIds": self.message_ids,
            "category": self.category,
            "evidenceKind": self.evidence_kind,
            "status": self.status,
            "resolution": self.resolution,
            "resolvedAt": opt_rfc(self.resolved_at),
            "createdAt": rfc(self.created_at),
        })
    }
}

async fn reports(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    if q.since.is_some() || q.until.is_some() {
        return Err(Error::validation(
            "reports take status, limit and cursor only",
        ));
    }
    if let Some(s) = q.status.as_deref() {
        if !matches!(s, "OPEN" | "ACTIONED" | "DISMISSED") {
            return Err(Error::validation(
                "status must be OPEN, ACTIONED or DISMISSED",
            ));
        }
    }
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    let rows: Vec<ReportRow> = db::all(
        &state.db,
        &format!(
            "SELECT {REPORT_COLUMNS} FROM messaging_reports \
             WHERE ($1::text IS NULL OR status = $1) AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3)) \
             ORDER BY created_at DESC, id DESC LIMIT $4"
        ),
        vec![q.status.clone().into(), at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        ReportRow::to_json,
    )))
}

#[derive(Debug, FromQueryResult)]
struct ActionRow {
    id: Uuid,
    kind: String,
    reason: String,
    conversation_id: Option<Uuid>,
    message_id: Option<Uuid>,
    principal_id: Option<Uuid>,
    effective_until: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

async fn report(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(report_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let row: ReportRow = db::one(
        &state.db,
        &format!("SELECT {REPORT_COLUMNS} FROM messaging_reports WHERE id = $1"),
        vec![report_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    let actions: Vec<ActionRow> = db::all(
        &state.db,
        "SELECT id, kind, reason, conversation_id, message_id, principal_id, effective_until, created_at \
         FROM messaging_moderation_actions WHERE report_id = $1 ORDER BY created_at, id LIMIT 100",
        vec![report_id.into()],
    )
    .await?;
    let mut doc = row.to_json();
    doc["actions"] = actions
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "kind": a.kind,
                "reason": a.reason,
                "conversationId": a.conversation_id,
                "messageId": a.message_id,
                "principalId": a.principal_id,
                "effectiveUntil": opt_rfc(a.effective_until),
                "createdAt": rfc(a.created_at),
            })
        })
        .collect();
    Ok(Json(doc))
}

// ---- Holds, exports, deletion requests ---------------------------------------------

#[derive(Debug, FromQueryResult)]
struct HoldRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    scope_kind: String,
    conversation_id: Option<Uuid>,
    principal_id: Option<Uuid>,
    reason_category: String,
    status: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    released_at: Option<DateTime<Utc>>,
    release_reason: Option<String>,
}

async fn holds(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    if q.since.is_some() || q.until.is_some() {
        return Err(Error::validation(
            "holds take status, limit and cursor only",
        ));
    }
    let status = q.status.as_deref().unwrap_or("ACTIVE");
    if !matches!(status, "ACTIVE" | "RELEASED") {
        return Err(Error::validation("status must be ACTIVE or RELEASED"));
    }
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    // The legal case reference is not relayed: the category says why.
    let rows: Vec<HoldRow> = db::all(
        &state.db,
        "SELECT id, tenant_id, application_id, environment_id, scope_kind, conversation_id, principal_id, \
         reason_category, status, created_at, expires_at, released_at, release_reason FROM messaging_legal_holds \
         WHERE status = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3)) \
         ORDER BY created_at DESC, id DESC LIMIT $4",
        vec![status.into(), at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        |r| {
            json!({
                "id": r.id, "tenantId": r.tenant_id, "applicationId": r.application_id,
                "environmentId": r.environment_id, "scopeKind": r.scope_kind,
                "conversationId": r.conversation_id, "principalId": r.principal_id,
                "reasonCategory": r.reason_category, "status": r.status,
                "createdAt": rfc(r.created_at), "expiresAt": opt_rfc(r.expires_at),
                "releasedAt": opt_rfc(r.released_at), "releaseReason": r.release_reason,
            })
        },
    )))
}

#[derive(Debug, FromQueryResult)]
struct ExportRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    status: String,
    size: Option<i64>,
    message_count: Option<i64>,
    failure: Option<String>,
    attempts: i32,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
}

fn no_filters(q: &PageQuery, what: &str) -> Result<(), ApiError> {
    if q.status.is_some() || q.since.is_some() || q.until.is_some() {
        return Err(Error::validation(format!(
            "{what} take limit and cursor only"
        )));
    }
    Ok(())
}

async fn exports(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    no_filters(&q, "exports")?;
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    // The export's object key and digest are not relayed.
    let rows: Vec<ExportRow> = db::all(
        &state.db,
        "SELECT id, tenant_id, application_id, environment_id, conversation_id, status, size, message_count, \
         failure, attempts, created_at, completed_at, expires_at FROM messaging_exports \
         WHERE ($1::timestamptz IS NULL OR (created_at, id) < ($1, $2)) ORDER BY created_at DESC, id DESC LIMIT $3",
        vec![at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        |r| {
            json!({
                "id": r.id, "tenantId": r.tenant_id, "applicationId": r.application_id,
                "environmentId": r.environment_id, "conversationId": r.conversation_id,
                "status": r.status, "size": r.size, "messageCount": r.message_count,
                "failure": r.failure, "attempts": r.attempts, "createdAt": rfc(r.created_at),
                "completedAt": opt_rfc(r.completed_at), "expiresAt": rfc(r.expires_at),
            })
        },
    )))
}

#[derive(Debug, FromQueryResult)]
struct DeletionRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    kind: String,
    principal_id: Option<Uuid>,
    conversation_id: Option<Uuid>,
    status: String,
    messages_purged: i64,
    attachments_deleted: i64,
    held_items: i64,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

async fn deletion_requests(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    no_filters(&q, "deletion requests")?;
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    let rows: Vec<DeletionRow> = db::all(
        &state.db,
        "SELECT id, tenant_id, application_id, environment_id, kind, principal_id, conversation_id, status, \
         messages_purged, attachments_deleted, held_items, created_at, completed_at FROM messaging_deletion_requests \
         WHERE ($1::timestamptz IS NULL OR (created_at, id) < ($1, $2)) ORDER BY created_at DESC, id DESC LIMIT $3",
        vec![at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        |r| {
            json!({
                "id": r.id, "tenantId": r.tenant_id, "applicationId": r.application_id,
                "environmentId": r.environment_id, "kind": r.kind, "principalId": r.principal_id,
                "conversationId": r.conversation_id, "status": r.status,
                "messagesPurged": r.messages_purged, "attachmentsDeleted": r.attachments_deleted,
                "heldItems": r.held_items, "createdAt": rfc(r.created_at),
                "completedAt": opt_rfc(r.completed_at),
            })
        },
    )))
}

// ---- Audit, switches, incidents, failed messages -----------------------------------

#[derive(Debug, FromQueryResult)]
struct AuditRow {
    id: Uuid,
    occurred_at: DateTime<Utc>,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    credential_id: Option<Uuid>,
    principal_id: Option<Uuid>,
    action: String,
    target_kind: String,
    target_id: Uuid,
    correlation_id: String,
}

fn windowed(q: &PageQuery, what: &str) -> Result<Window, ApiError> {
    if q.status.is_some() {
        return Err(Error::validation(format!(
            "{what} take since, until, limit and cursor only"
        )));
    }
    window(q)
}

async fn audit(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let (since, until) = windowed(&q, "audit reads")?;
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    // The audit `detail` object is not relayed.
    let rows: Vec<AuditRow> = db::all(
        &state.db,
        "SELECT id, occurred_at, tenant_id, application_id, environment_id, credential_id, principal_id, action, \
         target_kind, target_id, correlation_id FROM messaging_audit \
         WHERE ($1::timestamptz IS NULL OR occurred_at >= $1) AND ($2::timestamptz IS NULL OR occurred_at < $2) \
           AND ($3::timestamptz IS NULL OR (occurred_at, id) < ($3, $4)) \
         ORDER BY occurred_at DESC, id DESC LIMIT $5",
        vec![since.into(), until.into(), at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.occurred_at, r.id),
        |r| {
            json!({
                "id": r.id, "occurredAt": rfc(r.occurred_at), "tenantId": r.tenant_id,
                "applicationId": r.application_id, "environmentId": r.environment_id,
                "credentialId": r.credential_id, "principalId": r.principal_id, "action": r.action,
                "targetKind": r.target_kind, "targetId": r.target_id, "correlationId": r.correlation_id,
            })
        },
    )))
}

async fn switches(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let rows = crate::operations::switches(&state.db).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "switchName": r.switch_name, "scopeKey": r.scope_key, "engaged": r.engaged,
                "reason": r.reason, "changedBy": r.changed_by, "changedAt": rfc(r.changed_at),
                "version": r.version,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "nextCursor": null })))
}

#[derive(Debug, FromQueryResult)]
struct IncidentRow {
    id: Uuid,
    category: String,
    severity: String,
    status: String,
    occurrences: i32,
    first_seen_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    resolved_by: Option<String>,
}

async fn incidents(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let (since, until) = windowed(&q, "incident reads")?;
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    let rows: Vec<IncidentRow> = db::all(
        &state.db,
        "SELECT id, category, severity, status, occurrences, first_seen_at, last_seen_at, resolved_at, resolved_by \
         FROM messaging_incidents \
         WHERE ($1::timestamptz IS NULL OR last_seen_at >= $1) AND ($2::timestamptz IS NULL OR last_seen_at < $2) \
           AND ($3::timestamptz IS NULL OR (last_seen_at, id) < ($3, $4)) \
         ORDER BY last_seen_at DESC, id DESC LIMIT $5",
        vec![since.into(), until.into(), at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.last_seen_at, r.id),
        |r| {
            json!({
                "id": r.id, "category": r.category, "severity": r.severity, "status": r.status,
                "occurrences": r.occurrences, "firstSeenAt": rfc(r.first_seen_at),
                "lastSeenAt": rfc(r.last_seen_at), "resolvedAt": opt_rfc(r.resolved_at),
                "resolvedBy": r.resolved_by,
            })
        },
    )))
}

#[derive(Debug, FromQueryResult)]
struct DeadLetterRow {
    id: Uuid,
    sequence: i64,
    event_type: String,
    aggregate_type: String,
    aggregate_id: Uuid,
    tenant_id: Uuid,
    environment_id: Uuid,
    attempts: i32,
    occurred_at: DateTime<Utc>,
}

async fn failed_messages(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let (since, until) = windowed(&q, "failed-message reads")?;
    let limit = limit(&q)?;
    let (at, id) = cursor(&q)?;
    // Event payloads and error strings are not relayed.
    let rows: Vec<DeadLetterRow> = db::all(
        &state.db,
        "SELECT id, sequence, event_type, aggregate_type, aggregate_id, tenant_id, environment_id, attempts, occurred_at \
         FROM messaging_outbox_events WHERE status = 'DEAD_LETTER' \
           AND ($1::timestamptz IS NULL OR occurred_at >= $1) AND ($2::timestamptz IS NULL OR occurred_at < $2) \
           AND ($3::timestamptz IS NULL OR (occurred_at, id) < ($3, $4)) \
         ORDER BY occurred_at DESC, id DESC LIMIT $5",
        vec![since.into(), until.into(), at.into(), id.into(), ((limit + 1) as i64).into()],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.occurred_at, r.id),
        |r| {
            json!({
                "id": r.id, "sequence": r.sequence, "eventType": r.event_type,
                "aggregateType": r.aggregate_type, "aggregateId": r.aggregate_id,
                "tenantId": r.tenant_id, "environmentId": r.environment_id,
                "attempts": r.attempts, "occurredAt": rfc(r.occurred_at),
            })
        },
    )))
}

// ---- Key transparency ----------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct HeadRow {
    tree_size: i64,
    root_hash: Vec<u8>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct WitnessRow {
    witness_id: String,
    tree_size: i64,
    root_hash: Vec<u8>,
    updated_at: DateTime<Utc>,
    last_cosigned_size: Option<i64>,
}

async fn key_transparency(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let head: Option<HeadRow> = db::one(
        &state.db,
        "SELECT tree_size, root_hash, updated_at FROM messaging_transparency_head WHERE id = 1",
        vec![],
    )
    .await?;
    let witnesses: Vec<WitnessRow> = db::all(
        &state.db,
        "SELECT w.witness_id, w.tree_size, w.root_hash, w.updated_at, \
           (SELECT max(c.tree_size) FROM messaging_transparency_cosignatures c WHERE c.witness_id = w.witness_id) \
             AS last_cosigned_size \
         FROM messaging_transparency_witness_state w ORDER BY w.witness_id LIMIT 32",
        vec![],
    )
    .await?;
    Ok(Json(json!({
        "head": head.map(|h| json!({
            "treeSize": h.tree_size, "rootHash": hex::encode(h.root_hash), "updatedAt": rfc(h.updated_at),
        })),
        "witnesses": witnesses.iter().map(|w| json!({
            "witnessId": w.witness_id, "treeSize": w.tree_size, "rootHash": hex::encode(&w.root_hash),
            "updatedAt": rfc(w.updated_at), "lastCosignedSize": w.last_cosigned_size,
        })).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_garbage() {
        let at = Utc
            .timestamp_micros(1_795_000_000_123_456)
            .single()
            .unwrap();
        let id = Uuid::new_v4();
        assert_eq!(decode_cursor(&encode_cursor(at, id)), Some((at, id)));
        for bad in [
            "",
            "x.y",
            "1795000000",
            "abc.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
            "1.not-a-uuid",
        ] {
            assert!(decode_cursor(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn windows_are_bounded() {
        let q = |s: &str, u: &str| PageQuery {
            since: Some(s.into()),
            until: Some(u.into()),
            ..Default::default()
        };
        assert!(window(&q("2026-11-01T00:00:00Z", "2026-11-20T00:00:00Z")).is_ok());
        assert!(window(&q("2026-11-20T00:00:00Z", "2026-11-01T00:00:00Z")).is_err());
        assert!(window(&q("2026-09-01T00:00:00Z", "2026-11-20T00:00:00Z")).is_err());
        assert!(window(&q("yesterday", "2026-11-20T00:00:00Z")).is_err());
        assert!(window(&PageQuery::default()).is_ok());
    }
}
