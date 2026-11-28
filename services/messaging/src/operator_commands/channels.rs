//! Channel verification and moderation by HQ through the admin bridge (ADR 0072, ADR
//! 0068): the only way a channel gets or loses its badge, is restricted, suspended, banned
//! or reinstated, loses a post, or has a report resolved. Channel owners apply and report
//! on the customer API (`crate::social::channels::safety`); they never decide.
//!
//! Reads (bridge only, `messaging-firstparty.administer` with a named HQ operator):
//!
//! | Route | Data |
//! |---|---|
//! | `GET /operator/channel-verifications?status&limit&cursor` | applications, newest first |
//! | `GET /operator/channel-reports?status&limit&cursor` | channel / post reports, newest first |
//! | `GET /operator/channels/{channelId}` | one channel's handle, codes, states, counts, version |
//! | `GET /operator/channel-actions?since&until&limit&cursor` | HQ's channel actions |
//!
//! Commands (all R3: two-person, versioned, idempotent; the common owner contract of
//! [`super`]):
//!
//! | Command | Route | Subject (version) | Status |
//! |---|---|---|---|
//! | `messaging.channel-verification.decide` | `POST /operator/channel-verifications/{applicationId}/decide` | CHANNEL_VERIFICATION (application) | `APPROVED` / `REJECTED` |
//! | `messaging.channel-verification.revoke` | `POST /operator/channels/{channelId}/verification/revoke` | CHANNEL (channel) | `REVOKED` |
//! | `messaging.channel.moderate` | `POST /operator/channels/{channelId}/moderate` | CHANNEL (channel) | the new state |
//! | `messaging.channel-post.takedown` | `POST /operator/channels/{channelId}/posts/{postId}/takedown` | CHANNEL_POST (channel) | `TAKEN_DOWN` |
//! | `messaging.channel-report.resolve` | `POST /operator/channel-reports/{reportId}/resolve` | CHANNEL_REPORT (report) | `ACTIONED` / `DISMISSED` |
//!
//! Privacy: ids, codes, states, counts and times. Never a channel's name, description or
//! posts, a report's note, a reporter or a follower.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{begin, body, expected, finish, reason, BridgeCommand, Outcome, Subject};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    operator_reads::{decode_cursor, encode_cursor, require_bridge, PageQuery},
    state::AppState,
};

pub const DECIDE_VERIFICATION: &str = "messaging.channel-verification.decide";
pub const REVOKE_VERIFICATION: &str = "messaging.channel-verification.revoke";
pub const MODERATE_CHANNEL: &str = "messaging.channel.moderate";
pub const TAKE_DOWN_POST: &str = "messaging.channel-post.takedown";
pub const RESOLVE_REPORT: &str = "messaging.channel-report.resolve";

fn page_limit(q: &PageQuery) -> Result<u64, ApiError> {
    match q.limit {
        None => Ok(crate::operator_reads::DEFAULT_LIMIT),
        Some(n) if (1..=crate::operator_reads::MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(Error::validation("limit must be between 1 and 100")),
    }
}

fn page_cursor(q: &PageQuery) -> Result<(Option<DateTime<Utc>>, Option<Uuid>), ApiError> {
    match q.cursor.as_deref() {
        None => Ok((None, None)),
        Some(raw) => decode_cursor(raw)
            .map(|(at, id)| (Some(at), Some(id)))
            .ok_or_else(|| Error::validation("cursor is invalid")),
    }
}

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

fn opt(t: Option<DateTime<Utc>>) -> Value {
    t.map(|t| Value::String(rfc3339(t))).unwrap_or(Value::Null)
}

fn status_only(q: &PageQuery, allowed: &[&str], what: &str) -> Result<(), ApiError> {
    if q.since.is_some() || q.until.is_some() {
        return Err(Error::validation(format!(
            "{what} take status, limit and cursor only"
        )));
    }
    if let Some(s) = q.status.as_deref() {
        if !allowed.contains(&s) {
            return Err(Error::validation(format!(
                "status must be one of {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

// ---- Reads ---------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct ApplicationRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    channel_id: Uuid,
    basis: String,
    state: String,
    decision_reason: Option<String>,
    decided_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    version: i64,
    handle: Option<String>,
    category: Option<String>,
    country: Option<String>,
    language: Option<String>,
    follower_count: Option<i64>,
    post_count: Option<i64>,
    channel_created_at: Option<DateTime<Utc>>,
}

const APPLICATION_SELECT: &str = "SELECT v.id, v.tenant_id, v.application_id, v.environment_id, v.channel_id, v.basis, \
       v.state, v.decision_reason, v.decided_at, v.created_at, v.version, c.handle, c.category, c.country, c.language, \
       c.follower_count, (SELECT count(*) FROM messaging_channel_posts p WHERE p.channel_id = v.channel_id) AS post_count, \
       c.created_at AS channel_created_at \
  FROM messaging_channel_verifications v LEFT JOIN messaging_channels c ON c.id = v.channel_id";

impl ApplicationRow {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "channelId": self.channel_id,
            "basis": self.basis,
            "state": self.state,
            "decisionReason": self.decision_reason,
            "decidedAt": opt(self.decided_at),
            "createdAt": rfc3339(self.created_at),
            "version": self.version,
            "channelExists": self.channel_created_at.is_some(),
            "handle": self.handle,
            "category": self.category,
            "country": self.country,
            "language": self.language,
            "followerCount": self.follower_count.unwrap_or(0),
            "postCount": self.post_count.unwrap_or(0),
            "channelCreatedAt": opt(self.channel_created_at),
        })
    }
}

/// `GET /operator/channel-verifications`: applications, newest first.
pub async fn verifications(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    status_only(
        &q,
        &["PENDING", "APPROVED", "REJECTED", "REVOKED"],
        "channel verifications",
    )?;
    let limit = page_limit(&q)?;
    let (at, id) = page_cursor(&q)?;
    let rows: Vec<ApplicationRow> = db::all(
        &state.db,
        &format!(
            "{APPLICATION_SELECT} WHERE ($1::text IS NULL OR v.state = $1) \
               AND ($2::timestamptz IS NULL OR (v.created_at, v.id) < ($2, $3)) \
             ORDER BY v.created_at DESC, v.id DESC LIMIT $4"
        ),
        vec![
            q.status.clone().into(),
            at.into(),
            id.into(),
            ((limit + 1) as i64).into(),
        ],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        ApplicationRow::to_json,
    )))
}

#[derive(Debug, FromQueryResult)]
struct ReportRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    channel_id: Uuid,
    post_id: Option<Uuid>,
    category: String,
    status: String,
    resolution: Option<String>,
    resolved_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    version: i64,
}

const REPORT_COLUMNS: &str =
    "id, tenant_id, application_id, environment_id, channel_id, post_id, category, status, \
     resolution, resolved_at, created_at, version";

impl ReportRow {
    /// No reporter and no note: ids, category, states and times.
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "channelId": self.channel_id,
            "postId": self.post_id,
            "category": self.category,
            "status": self.status,
            "resolution": self.resolution,
            "resolvedAt": opt(self.resolved_at),
            "createdAt": rfc3339(self.created_at),
            "version": self.version,
        })
    }
}

/// `GET /operator/channel-reports`: reports of channels and posts, newest first.
pub async fn reports(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    status_only(&q, &["OPEN", "ACTIONED", "DISMISSED"], "channel reports")?;
    let limit = page_limit(&q)?;
    let (at, id) = page_cursor(&q)?;
    let rows: Vec<ReportRow> = db::all(
        &state.db,
        &format!(
            "SELECT {REPORT_COLUMNS} FROM messaging_channel_reports WHERE ($1::text IS NULL OR status = $1) \
               AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3)) ORDER BY created_at DESC, id DESC LIMIT $4"
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

#[derive(Debug, Clone, FromQueryResult)]
struct ChannelRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    handle: Option<String>,
    category: Option<String>,
    country: Option<String>,
    language: Option<String>,
    visibility: String,
    moderation_state: String,
    moderation_reason: Option<String>,
    moderation_changed_at: Option<DateTime<Utc>>,
    verified_at: Option<DateTime<Utc>>,
    follower_count: i64,
    post_count: i64,
    open_reports: i64,
    created_at: DateTime<Utc>,
    version: i64,
}

const CHANNEL_SELECT: &str = "SELECT c.id, c.tenant_id, c.application_id, c.environment_id, c.handle, c.category, \
       c.country, c.language, c.visibility, c.moderation_state, c.moderation_reason, c.moderation_changed_at, \
       c.verified_at, c.follower_count, \
       (SELECT count(*) FROM messaging_channel_posts p WHERE p.channel_id = c.id) AS post_count, \
       (SELECT count(*) FROM messaging_channel_reports r WHERE r.channel_id = c.id AND r.status = 'OPEN') AS open_reports, \
       c.created_at, c.version FROM messaging_channels c";

impl ChannelRow {
    fn to_json(&self) -> Value {
        json!({
            "channelId": self.id,
            "tenantId": self.tenant_id,
            "applicationId": self.application_id,
            "environmentId": self.environment_id,
            "handle": self.handle,
            "category": self.category,
            "country": self.country,
            "language": self.language,
            "visibility": self.visibility,
            "moderationState": self.moderation_state,
            "moderationReason": self.moderation_reason,
            "moderationChangedAt": opt(self.moderation_changed_at),
            "verified": self.verified_at.is_some(),
            "verifiedAt": opt(self.verified_at),
            "followerCount": self.follower_count,
            "postCount": self.post_count,
            "openReports": self.open_reports,
            "createdAt": rfc3339(self.created_at),
            "version": self.version,
        })
    }
}

async fn channel_row<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    lock: bool,
) -> Result<Option<ChannelRow>, ApiError> {
    if lock {
        db::exec(
            db,
            "SELECT 1 FROM messaging_channels WHERE id = $1 FOR UPDATE",
            vec![id.into()],
        )
        .await?;
    }
    Ok(db::one(
        db,
        &format!("{CHANNEL_SELECT} WHERE c.id = $1"),
        vec![id.into()],
    )
    .await?)
}

/// `GET /operator/channels/{channelId}`.
pub async fn channel(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let row = channel_row(&state.db, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(Json(row.to_json()))
}

#[derive(Debug, FromQueryResult)]
struct ActionRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    channel_id: Uuid,
    post_id: Option<Uuid>,
    report_id: Option<Uuid>,
    verification_id: Option<Uuid>,
    action: String,
    resulting_state: Option<String>,
    reason: String,
    created_at: DateTime<Utc>,
}

/// `GET /operator/channel-actions`: HQ's channel actions, newest first (optional window of
/// at most 31 days).
pub async fn actions(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    if q.status.is_some() {
        return Err(Error::validation(
            "channel actions take since, until, limit and cursor only",
        ));
    }
    let (since, until) = crate::operator_reads::window(&q)?;
    let limit = page_limit(&q)?;
    let (at, id) = page_cursor(&q)?;
    let rows: Vec<ActionRow> = db::all(
        &state.db,
        "SELECT id, tenant_id, application_id, environment_id, channel_id, post_id, report_id, verification_id, action, \
           resulting_state, reason, created_at FROM messaging_channel_moderation_actions \
          WHERE ($1::timestamptz IS NULL OR created_at >= $1) AND ($2::timestamptz IS NULL OR created_at < $2) \
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4)) ORDER BY created_at DESC, id DESC LIMIT $5",
        vec![
            since.into(),
            until.into(),
            at.into(),
            id.into(),
            ((limit + 1) as i64).into(),
        ],
    )
    .await?;
    Ok(Json(page(
        rows,
        limit,
        |r| (r.created_at, r.id),
        |r| {
            json!({
                "id": r.id,
                "tenantId": r.tenant_id,
                "applicationId": r.application_id,
                "environmentId": r.environment_id,
                "channelId": r.channel_id,
                "postId": r.post_id,
                "reportId": r.report_id,
                "verificationId": r.verification_id,
                "action": r.action,
                "resultingState": r.resulting_state,
                "reason": r.reason,
                "createdAt": rfc3339(r.created_at),
            })
        },
    )))
}

// ---- Commands ------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decide {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
    /// `APPROVE` or `REJECT`.
    pub decision: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Versioned {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Moderate {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
    /// `ACTIVE` (reinstate), `RESTRICTED`, `SUSPENDED` or `BANNED`.
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolve {
    pub tenant_id: Uuid,
    pub expected_version: i64,
    pub reason_code: String,
    /// `ACTIONED` or `DISMISSED`.
    pub resolution: String,
}

/// One HQ action in the channel's moderation history (same transaction as the change).
#[allow(clippy::too_many_arguments)]
async fn record<C: ConnectionTrait>(
    txn: &C,
    c: &ChannelRow,
    post: Option<Uuid>,
    report: Option<Uuid>,
    verification: Option<Uuid>,
    action: &str,
    resulting: Option<&str>,
    reason_code: &str,
    cmd: &BridgeCommand,
    now: DateTime<Utc>,
) -> Result<Uuid, ApiError> {
    let id = Uuid::new_v4();
    db::exec(
        txn,
        "INSERT INTO messaging_channel_moderation_actions (id, tenant_id, application_id, environment_id, channel_id, post_id, \
           report_id, verification_id, action, resulting_state, reason, operator_ref, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        vec![
            id.into(),
            c.tenant_id.into(),
            c.application_id.into(),
            c.environment_id.into(),
            c.id.into(),
            post.into(),
            report.into(),
            verification.into(),
            action.into(),
            resulting.map(str::to_string).into(),
            reason_code.into(),
            cmd.operator.clone().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(id)
}

/// The channel of `tenant`, locked, at `version` (404 for another tenant, 409 otherwise).
async fn locked_channel<C: ConnectionTrait>(
    txn: &C,
    id: Uuid,
    tenant: Uuid,
    version: i64,
) -> Result<ChannelRow, ApiError> {
    let c = channel_row(txn, id, true)
        .await?
        .filter(|c| c.tenant_id == tenant)
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if c.version != version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    Ok(c)
}

/// `POST /operator/channel-verifications/{applicationId}/decide`.
pub async fn decide_verification(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(application): Path<Uuid>,
    raw: Result<Json<Decide>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let approve = match req.decision.as_str() {
        "APPROVE" => true,
        "REJECT" => false,
        _ => return Err(Error::validation("decision must be APPROVE or REJECT")),
    };
    let subject = Subject::new("CHANNEL_VERIFICATION", application);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, DECIDE_VERIFICATION, &subject).await? {
        return Ok(Json(done));
    }
    #[derive(FromQueryResult)]
    struct App {
        tenant_id: Uuid,
        channel_id: Uuid,
        state: String,
        version: i64,
    }
    let app: App = db::one(
        &txn,
        "SELECT tenant_id, channel_id, state, version FROM messaging_channel_verifications WHERE id = $1 FOR UPDATE",
        vec![application.into()],
    )
    .await?
    .filter(|a: &App| a.tenant_id == req.tenant_id)
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if app.version != version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if app.state != "PENDING" {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "APPLICATION_NOT_PENDING",
        ));
    }
    let c = channel_row(&txn, app.channel_id, true)
        .await?
        .ok_or(Error::reason(ErrorCode::Conflict, "CHANNEL_GONE"))?;
    if approve && c.moderation_state != "ACTIVE" {
        return Err(Error::reason(ErrorCode::Conflict, "CHANNEL_NOT_ACTIVE"));
    }
    let new_state = if approve { "APPROVED" } else { "REJECTED" };
    db::exec(
        &txn,
        "UPDATE messaging_channel_verifications SET state = $2, decision_reason = $3, decided_at = $4, version = version + 1 \
          WHERE id = $1",
        vec![
            application.into(),
            new_state.into(),
            req.reason_code.clone().into(),
            now.into(),
        ],
    )
    .await?;
    if approve {
        db::exec(
            &txn,
            "UPDATE messaging_channels SET verified_at = $2, version = version + 1, updated_at = $2 WHERE id = $1",
            vec![c.id.into(), now.into()],
        )
        .await?;
    }
    record(
        &txn,
        &c,
        None,
        None,
        Some(application),
        if approve {
            "VERIFY"
        } else {
            "REJECT_VERIFICATION"
        },
        None,
        &req.reason_code,
        &cmd,
        now,
    )
    .await?;
    let outcome = Outcome {
        state: new_state,
        version: Some(app.version + 1),
        resource: json!({
            "applicationId": application,
            "channelId": c.id,
            "verificationState": new_state,
            "verified": approve,
            "decidedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, DECIDE_VERIFICATION, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_channel_bridge_actions_total", &[]);
    Ok(Json(answer))
}

/// `POST /operator/channels/{channelId}/verification/revoke`.
pub async fn revoke_verification(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(channel): Path<Uuid>,
    raw: Result<Json<Versioned>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("CHANNEL", channel);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, REVOKE_VERIFICATION, &subject).await? {
        return Ok(Json(done));
    }
    let c = locked_channel(&txn, channel, req.tenant_id, version).await?;
    if c.verified_at.is_none() {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "CHANNEL_NOT_VERIFIED",
        ));
    }
    db::exec(
        &txn,
        "UPDATE messaging_channels SET verified_at = NULL, version = version + 1, updated_at = $2 WHERE id = $1",
        vec![channel.into(), now.into()],
    )
    .await?;
    #[derive(FromQueryResult)]
    struct Id {
        id: Uuid,
    }
    let approved: Option<Id> = db::one(
        &txn,
        "UPDATE messaging_channel_verifications SET state = 'REVOKED', decision_reason = $2, decided_at = $3, \
           version = version + 1 WHERE channel_id = $1 AND state = 'APPROVED' RETURNING id",
        vec![channel.into(), req.reason_code.clone().into(), now.into()],
    )
    .await?;
    record(
        &txn,
        &c,
        None,
        None,
        approved.map(|a| a.id),
        "REVOKE_VERIFICATION",
        None,
        &req.reason_code,
        &cmd,
        now,
    )
    .await?;
    let outcome = Outcome {
        state: "REVOKED",
        version: Some(c.version + 1),
        resource: json!({
            "channelId": channel,
            "verificationState": "REVOKED",
            "verified": false,
            "changedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, REVOKE_VERIFICATION, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_channel_bridge_actions_total", &[]);
    Ok(Json(answer))
}

/// `POST /operator/channels/{channelId}/moderate`.
pub async fn moderate(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(channel): Path<Uuid>,
    raw: Result<Json<Moderate>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let (new_state, action) = match req.state.as_str() {
        "ACTIVE" => ("ACTIVE", "REINSTATE"),
        "RESTRICTED" => ("RESTRICTED", "RESTRICT"),
        "SUSPENDED" => ("SUSPENDED", "SUSPEND"),
        "BANNED" => ("BANNED", "BAN"),
        _ => {
            return Err(Error::validation(
                "state must be ACTIVE, RESTRICTED, SUSPENDED or BANNED",
            ))
        }
    };
    let subject = Subject::new("CHANNEL", channel);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, MODERATE_CHANNEL, &subject).await? {
        return Ok(Json(done));
    }
    let c = locked_channel(&txn, channel, req.tenant_id, version).await?;
    if c.moderation_state == new_state {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "ALREADY_IN_STATE",
        ));
    }
    let stored_reason = (new_state != "ACTIVE").then(|| req.reason_code.clone());
    db::exec(
        &txn,
        "UPDATE messaging_channels SET moderation_state = $2, moderation_reason = $3, moderation_changed_at = $4, \
           version = version + 1, updated_at = $4 WHERE id = $1",
        vec![
            channel.into(),
            new_state.into(),
            stored_reason.clone().into(),
            now.into(),
        ],
    )
    .await?;
    // A banned channel cannot keep a pending application.
    if new_state == "BANNED" {
        db::exec(
            &txn,
            "UPDATE messaging_channel_verifications SET state = 'REJECTED', decision_reason = 'CHANNEL_BANNED', \
               decided_at = $2, version = version + 1 WHERE channel_id = $1 AND state = 'PENDING'",
            vec![channel.into(), now.into()],
        )
        .await?;
    }
    record(
        &txn,
        &c,
        None,
        None,
        None,
        action,
        Some(new_state),
        &req.reason_code,
        &cmd,
        now,
    )
    .await?;
    let outcome = Outcome {
        state: new_state,
        version: Some(c.version + 1),
        resource: json!({
            "channelId": channel,
            "moderationState": new_state,
            "moderationReason": stored_reason,
            "changedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, MODERATE_CHANNEL, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_channel_bridge_actions_total", &[]);
    Ok(Json(answer))
}

/// `POST /operator/channels/{channelId}/posts/{postId}/takedown`: the post is removed (its
/// reactions, votes, views and stars with it; its media goes with the next sweep) and its
/// open reports are actioned.
pub async fn take_down(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((channel, post)): Path<(Uuid, Uuid)>,
    raw: Result<Json<Versioned>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("CHANNEL_POST", post);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, TAKE_DOWN_POST, &subject).await? {
        return Ok(Json(done));
    }
    let c = locked_channel(&txn, channel, req.tenant_id, version).await?;
    let n = db::exec(
        &txn,
        "DELETE FROM messaging_channel_posts WHERE id = $1 AND channel_id = $2",
        vec![post.into(), channel.into()],
    )
    .await?;
    if n == 0 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    db::exec(
        &txn,
        "UPDATE messaging_channels SET version = version + 1, updated_at = $2 WHERE id = $1",
        vec![channel.into(), now.into()],
    )
    .await?;
    db::exec(
        &txn,
        "UPDATE messaging_channel_reports SET status = 'ACTIONED', resolution = $3, resolved_at = $4, version = version + 1 \
          WHERE channel_id = $1 AND post_id = $2 AND status = 'OPEN'",
        vec![
            channel.into(),
            post.into(),
            req.reason_code.clone().into(),
            now.into(),
        ],
    )
    .await?;
    record(
        &txn,
        &c,
        Some(post),
        None,
        None,
        "TAKEDOWN",
        None,
        &req.reason_code,
        &cmd,
        now,
    )
    .await?;
    let outcome = Outcome {
        state: "TAKEN_DOWN",
        version: Some(c.version + 1),
        resource: json!({
            "channelId": channel,
            "postId": post,
            "takenDownAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, TAKE_DOWN_POST, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_channel_bridge_actions_total", &[]);
    Ok(Json(answer))
}

/// `POST /operator/channel-reports/{reportId}/resolve`.
pub async fn resolve_report(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(report): Path<Uuid>,
    raw: Result<Json<Resolve>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let version = expected(req.expected_version)?;
    reason(&req.reason_code)?;
    let resolution = match req.resolution.as_str() {
        "ACTIONED" => "ACTIONED",
        "DISMISSED" => "DISMISSED",
        _ => {
            return Err(Error::validation(
                "resolution must be ACTIONED or DISMISSED",
            ))
        }
    };
    let subject = Subject::new("CHANNEL_REPORT", report);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, RESOLVE_REPORT, &subject).await? {
        return Ok(Json(done));
    }
    let r: ReportRow = db::one(
        &txn,
        &format!("SELECT {REPORT_COLUMNS} FROM messaging_channel_reports WHERE id = $1 FOR UPDATE"),
        vec![report.into()],
    )
    .await?
    .filter(|r: &ReportRow| r.tenant_id == req.tenant_id)
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if r.version != version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    if r.status != "OPEN" {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "REPORT_NOT_OPEN",
        ));
    }
    db::exec(
        &txn,
        "UPDATE messaging_channel_reports SET status = $2, resolution = $3, resolved_at = $4, version = version + 1 WHERE id = $1",
        vec![
            report.into(),
            resolution.into(),
            req.reason_code.clone().into(),
            now.into(),
        ],
    )
    .await?;
    if let Some(c) = channel_row(&txn, r.channel_id, false).await? {
        record(
            &txn,
            &c,
            r.post_id,
            Some(report),
            None,
            "RESOLVE_REPORT",
            None,
            &req.reason_code,
            &cmd,
            now,
        )
        .await?;
    }
    let outcome = Outcome {
        state: resolution,
        version: Some(r.version + 1),
        resource: json!({
            "reportId": report,
            "channelId": r.channel_id,
            "postId": r.post_id,
            "reportStatus": resolution,
            "resolvedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, RESOLVE_REPORT, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc("messaging_channel_bridge_actions_total", &[]);
    Ok(Json(answer))
}
