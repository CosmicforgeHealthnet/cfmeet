//! The admin bridge's operator commands (ADR 0068, design §6.13 and §17.6, phase A4):
//! `POST /internal/v1/operator/*` and the settlement read
//! `GET /internal/v1/operator/operations/{idempotencyKey}`.
//!
//! Every command:
//! * comes only from `cfmeet.admin-bridge` holding `notifications.administer`, naming the HQ
//!   staff member (`X-CF-Operator`); R3 commands (routing change, reroute cancellation) also
//!   name the approving second person (`X-CF-Approver`, `X-CF-Approval-Ref`,
//!   `X-CF-Intent-Digest`), else 403 and nothing is done;
//! * carries HQ's `Idempotency-Key` (a UUID, else 400);
//! * locks its subject row `FOR UPDATE`, and changes it only at `expectedVersion` where the
//!   subject has a version (routing rows, suppressions; else 409 `VERSION_CONFLICT`) or only
//!   in the state the command applies to (deliveries and route decisions have no version:
//!   the state precondition under the row lock is their guard);
//! * records itself in `notif_admin_operations` in the SAME transaction as the change. A
//!   repeated key for the same command and subject answers again without applying (the
//!   recorded status and version, the current resource); a key used for anything else is
//!   409 `IDEMPOTENCY_CONFLICT`. The table's primary key makes this hold across both
//!   replicas: a concurrent second use of a key waits on the subject lock, then finds the
//!   record (or loses the insert and answers from it);
//! * answers `{"status", "version", "resource"}` with ids, states and times only (never an
//!   address, phone number or notification content).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    internal::{self, RetryRefusal},
    json_ok,
    middleware::Caller,
    parse,
};
use crate::{
    catalogue, db,
    error::{ApiError, ApiResult, ErrorCode},
    routing::Mode,
    state::AppState,
};

/// What a recorded command acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Delivery,
    EmailSuppression,
    Routing,
    RouteDecision,
}

impl Subject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Subject::Delivery => "DELIVERY",
            Subject::EmailSuppression => "EMAIL_SUPPRESSION",
            Subject::Routing => "ROUTING",
            Subject::RouteDecision => "ROUTE_DECISION",
        }
    }
}

/// Command names in the operation record.
pub const DELIVERY_RETRY: &str = "delivery.retry";
pub const SUPPRESSION_REMOVE: &str = "suppression.remove";
pub const ROUTING_CHANGE: &str = "routing.change";
pub const REROUTE_CANCEL: &str = "reroute.cancel";

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeCommand {
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)` of an R3 command.
    pub approval: Option<(String, String)>,
}

impl BridgeCommand {
    /// The bridge (`notifications.administer` + `X-CF-Operator`), the approval when
    /// `two_person`, and the `Idempotency-Key` (UUID).
    pub fn of(caller: &Caller, headers: &HeaderMap, two_person: bool) -> ApiResult<Self> {
        caller.require_operator(InternalPermission::NotificationsAdminister)?;
        let attribution = caller
            .operator
            .as_ref()
            .ok_or(ApiError::new(ErrorCode::Forbidden))?;
        if two_person && attribution.require_second_person().is_err() {
            return Err(ApiError::with_message(
                ErrorCode::Forbidden,
                "This command requires a second-person approval (X-CF-Approver, X-CF-Approval-Ref, X-CF-Intent-Digest).",
            ));
        }
        let key = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v.trim()).ok())
            .ok_or_else(|| ApiError::validation("Idempotency-Key (a UUID) is required."))?;
        Ok(BridgeCommand {
            key,
            operator: attribution.operator.as_str().to_string(),
            approval: attribution
                .approval
                .as_ref()
                .map(|a| (a.approver.as_str().to_string(), a.approval_ref.clone())),
        })
    }
}

/// The recorded outcome of a key: `(resulting_state, resulting_version)`.
type Recorded = (String, Option<i64>);

/// A key already applied to the same command and subject: its recorded outcome. A key
/// recorded for anything else is `IDEMPOTENCY_CONFLICT`.
async fn replayed<C: ConnectionTrait>(
    conn: &C,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: &str,
) -> ApiResult<Option<Recorded>> {
    #[derive(FromQueryResult)]
    struct Row {
        command: String,
        subject_kind: String,
        subject_id: String,
        resulting_state: String,
        resulting_version: Option<i64>,
    }
    let row: Option<Row> = db::one(
        conn,
        "SELECT command, subject_kind, subject_id, resulting_state, resulting_version
           FROM notif_admin_operations WHERE idempotency_key = $1",
        vec![cmd.key.into()],
    )
    .await?;
    match row {
        None => Ok(None),
        Some(r)
            if r.command == command
                && r.subject_kind == subject.as_str()
                && r.subject_id == subject_id =>
        {
            Ok(Some((r.resulting_state, r.resulting_version)))
        }
        Some(_) => Err(ApiError::new(ErrorCode::IdempotencyConflict)),
    }
}

/// Records the applied command (same transaction as the change).
async fn record(
    txn: &DatabaseTransaction,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: &str,
    resulting_state: &str,
    resulting_version: Option<i64>,
) -> Result<(), DbErr> {
    let (approver, approval_ref) = match &cmd.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    db::exec(
        txn,
        "INSERT INTO notif_admin_operations (idempotency_key, command, subject_kind, subject_id, operator_ref,
                approver_ref, approval_ref, resulting_state, resulting_version, recorded_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())",
        vec![
            cmd.key.into(),
            command.into(),
            subject.as_str().into(),
            subject_id.into(),
            cmd.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            resulting_state.into(),
            resulting_version.into(),
        ],
    )
    .await?;
    Ok(())
}

/// The bridge's answer.
fn answer(status: &str, version: Option<i64>, resource: Value) -> Response {
    json_ok(json!({
        "status": status,
        "version": version.map(|v| v.to_string()),
        "resource": resource,
    }))
}

/// Records the change and commits. When another request (on any replica) recorded the same
/// key first, this transaction is dropped (rolled back) and the key's record answers.
#[allow(clippy::too_many_arguments)]
async fn record_and_commit(
    state: &AppState,
    txn: DatabaseTransaction,
    cmd: &BridgeCommand,
    command: &str,
    subject: Subject,
    subject_id: &str,
    resulting_state: &str,
    resulting_version: Option<i64>,
) -> ApiResult<Option<Recorded>> {
    match record(
        &txn,
        cmd,
        command,
        subject,
        subject_id,
        resulting_state,
        resulting_version,
    )
    .await
    {
        Ok(()) => {
            txn.commit().await?;
            Ok(None)
        }
        Err(e) if db::is_unique_violation(&e, "pk_notif_admin_operations") => {
            drop(txn);
            match replayed(&state.db, cmd, command, subject, subject_id).await? {
                Some(r) => Ok(Some(r)),
                None => Err(ApiError::unavailable(1)),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// An empty body or `{}` (commands without fields).
fn no_fields(body: &Bytes) -> ApiResult<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    parse::<Empty>(body).map(|_| ())
}

fn expected(version: i64) -> ApiResult<i64> {
    if version >= 1 {
        Ok(version)
    } else {
        Err(ApiError::validation(
            "expectedVersion is an integer of at least 1.",
        ))
    }
}

fn version_conflict(message: &str) -> ApiError {
    ApiError::with_message(ErrorCode::VersionConflict, message)
}

// --- notifications.delivery.retry (R2) --------------------------------------------------

#[derive(FromQueryResult)]
struct DeliveryRow {
    delivery_id: Uuid,
    notification_id: Uuid,
    channel: String,
    lane: String,
    status: String,
    attempts: i32,
    last_error_class: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    next_attempt_at: DateTime<Utc>,
}

const DELIVERY_SQL: &str =
    "SELECT d.id AS delivery_id, d.notification_id, d.channel, d.lane, d.status, d.attempts,
        d.last_error_class, n.expires_at, d.next_attempt_at
   FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id WHERE d.id = $1";

fn delivery_resource(d: &DeliveryRow) -> Value {
    json!({
        "deliveryId": d.delivery_id, "notificationId": d.notification_id, "channel": d.channel,
        "lane": d.lane, "deliveryStatus": d.status, "attempts": d.attempts,
        "lastErrorClass": d.last_error_class, "nextAttemptAt": d.next_attempt_at,
    })
}

async fn delivery<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
    lock: bool,
) -> ApiResult<Option<DeliveryRow>> {
    let sql = if lock {
        format!("{DELIVERY_SQL} FOR UPDATE OF d")
    } else {
        DELIVERY_SQL.to_string()
    };
    Ok(db::one(conn, &sql, vec![id.into()]).await?)
}

/// `POST /internal/v1/operator/deliveries/{deliveryId}/retry` — I6's delivery replay for one
/// FAILED push delivery: back to PENDING, due now, with a fresh attempt budget. Refused
/// (400) for CRITICAL or expired deliveries, email (its content is erased at its final
/// state) and invalid push handles; 409 when the delivery is not FAILED.
pub async fn retry_delivery(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let cmd = BridgeCommand::of(&caller, &headers, false)?;
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::not_found())?;
    no_fields(&body)?;
    let subject_id = id.to_string();
    let txn = state.db.begin().await?;
    let row = delivery(&txn, id, true).await?;
    if let Some((status, version)) =
        replayed(&txn, &cmd, DELIVERY_RETRY, Subject::Delivery, &subject_id).await?
    {
        txn.rollback().await?;
        let current = delivery(&state.db, id, false).await?;
        return Ok(answer(
            &status,
            version,
            current
                .as_ref()
                .map(delivery_resource)
                .unwrap_or(Value::Null),
        ));
    }
    let Some(d) = row else {
        return Err(ApiError::not_found());
    };
    match internal::delivery_retry_refusal(
        &d.lane,
        d.expires_at,
        &d.status,
        &d.channel,
        d.last_error_class.as_deref(),
    ) {
        Some(RetryRefusal::Expired) => {
            return Err(ApiError::validation(
                "The delivery cannot be retried: it is CRITICAL (a code is never re-sent) or past its expiry.",
            ))
        }
        Some(RetryRefusal::NotRetryable) => {
            return Err(ApiError::validation(
                "Only a push delivery whose handle is valid can be retried.",
            ))
        }
        Some(RetryRefusal::NotFailed) => return Err(version_conflict("The delivery is not FAILED.")),
        None => {}
    }
    db::exec(&txn, internal::RETRY_DELIVERY_SQL, vec![id.into()]).await?;
    let after = delivery(&txn, id, false)
        .await?
        .ok_or(ApiError::unavailable(1))?;
    if let Some((status, version)) = record_and_commit(
        &state,
        txn,
        &cmd,
        DELIVERY_RETRY,
        Subject::Delivery,
        &subject_id,
        "RETRIED",
        None,
    )
    .await?
    {
        let current = delivery(&state.db, id, false).await?;
        return Ok(answer(
            &status,
            version,
            current
                .as_ref()
                .map(delivery_resource)
                .unwrap_or(Value::Null),
        ));
    }
    tracing::info!(operator = %cmd.operator, delivery = %id, "bridge delivery retry");
    Ok(answer("RETRIED", None, delivery_resource(&after)))
}

// --- notifications.suppression.remove (R2) ----------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuppressionRemoval {
    expected_version: i64,
}

async fn suppression_resource<C: ConnectionTrait>(conn: &C, digest: &str) -> ApiResult<Value> {
    let suppressed = crate::email::suppressed(conn, digest).await?;
    Ok(json!({ "digest": digest, "suppressed": suppressed }))
}

/// `POST /internal/v1/operator/email-suppressions/{digest}/remove` — I10 for the bridge:
/// removes the suppression of one keyed digest at `expectedVersion` (the `version` of the
/// operator suppression read).
pub async fn remove_suppression(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(digest): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let cmd = BridgeCommand::of(&caller, &headers, false)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::validation("digest is 64 hex characters."));
    }
    let digest = digest.to_ascii_lowercase();
    let req: SuppressionRemoval = parse(&body)?;
    let expected_version = expected(req.expected_version)?;
    let txn = state.db.begin().await?;
    let row = txn
        .query_one(db::stmt(
            "SELECT version FROM notif_email_suppressions WHERE recipient_digest = $1 FOR UPDATE",
            vec![digest.clone().into()],
        ))
        .await?;
    if let Some((status, version)) = replayed(
        &txn,
        &cmd,
        SUPPRESSION_REMOVE,
        Subject::EmailSuppression,
        &digest,
    )
    .await?
    {
        txn.rollback().await?;
        return Ok(answer(
            &status,
            version,
            suppression_resource(&state.db, &digest).await?,
        ));
    }
    let Some(row) = row else {
        return Err(ApiError::not_found());
    };
    let version: i64 = row.try_get("", "version").map_err(ApiError::from)?;
    if version != expected_version {
        return Err(version_conflict(
            "The suppression changed since it was read; read it again.",
        ));
    }
    db::exec(
        &txn,
        "DELETE FROM notif_email_suppressions WHERE recipient_digest = $1",
        vec![digest.clone().into()],
    )
    .await?;
    if let Some((status, version)) = record_and_commit(
        &state,
        txn,
        &cmd,
        SUPPRESSION_REMOVE,
        Subject::EmailSuppression,
        &digest,
        "REMOVED",
        None,
    )
    .await?
    {
        return Ok(answer(
            &status,
            version,
            suppression_resource(&state.db, &digest).await?,
        ));
    }
    tracing::info!(operator = %cmd.operator, "bridge email suppression removed");
    Ok(answer(
        "REMOVED",
        None,
        suppression_resource(&state.db, &digest).await?,
    ))
}

// --- notifications.routing.change (R3) --------------------------------------------------

/// Categories whose ACTIVE needs the Account CRITICAL lane (codes and security notices):
/// the coordinated release's own list, plus every category the catalogue routes on the
/// CRITICAL lane.
const CRITICAL_CATEGORIES: [&str; 6] = [
    "AUTH_VERIFICATION",
    "AUTH_PASSWORD_RESET",
    "SECURITY_DEVICE_STEP_UP",
    "SECURITY_PASSWORD_CHANGED",
    "SECURITY_DEVICE_LINKED",
    "ACCOUNT_NOTICES",
];
/// Categories whose ACTIVE needs the owner APIs (blocks and privacy are asked at the Hub).
const SOCIAL_CATEGORIES: [&str; 5] = [
    "CONTACT_REQUESTS",
    "GROUP_INVITATIONS",
    "MISSED_CALLS",
    "VOICEMAIL",
    "MESSAGE_WAKEUPS",
];
const CHANNELS: [&str; 4] = ["EMAIL", "PUSH", "IN_APP", "OPERATOR"];

fn critical(category: &str) -> bool {
    CRITICAL_CATEGORIES.contains(&category)
        || catalogue::ROUTES.iter().any(|r| {
            r.route_category == category && r.lane() == cf_notification_client::Lane::Critical
        })
}

/// What the service itself knows of the activation gates.
#[derive(Debug, Clone, Default)]
pub struct GateFacts {
    pub email_provider: bool,
    pub push_provider: bool,
    pub authorities: Vec<&'static str>,
}

/// The coordinated release's routing gates (`activate-notifications-routing`), as far as
/// the service can verify them; anything it cannot verify is refused and stays with the
/// coordinated release:
/// * any row may move to OFF (events are HELD, never dropped) or SHADOW (no delivery);
/// * ACTIVE is refused for CRITICAL categories (the Account CRITICAL lane, its lease grant
///   and fallback key are release evidence) and for MEETING_NOTIFICATION EMAIL (the Meeting
///   Service's in-app-only path may still email those notices itself: a second email);
/// * EMAIL ACTIVE needs the service's email provider (configured only after gate E1);
///   PUSH ACTIVE needs its push provider; a social category needs the owner APIs.
pub fn routing_gate(
    category: &str,
    channel: &str,
    to: Mode,
    facts: &GateFacts,
) -> Option<&'static str> {
    if to != Mode::Active {
        return None;
    }
    if critical(category) {
        return Some("A CRITICAL category is activated only by the coordinated release (activate-critical-lane first).");
    }
    if category == "MEETING_NOTIFICATION" && channel == "EMAIL" {
        return Some("MEETING_NOTIFICATION EMAIL is activated only by the coordinated release (the Meeting Service may still send these emails itself).");
    }
    if channel == "EMAIL" && !facts.email_provider {
        return Some(
            "EMAIL ACTIVE needs the service's email provider (activate-notifications-email).",
        );
    }
    if channel == "PUSH" && !facts.push_provider {
        return Some(
            "PUSH ACTIVE needs the service's push provider (activate-notifications-push).",
        );
    }
    if SOCIAL_CATEGORIES.contains(&category) && !facts.authorities.contains(&"communications") {
        return Some(
            "This category's ACTIVE needs the owner APIs (activate-notifications-owners).",
        );
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingChange {
    mode: String,
    expected_version: i64,
}

#[derive(FromQueryResult)]
struct RoutingRow {
    mode: String,
    version: i32,
    canary_percent: i16,
    fence_at: DateTime<Utc>,
    changed_at: DateTime<Utc>,
}

async fn routing_row<C: ConnectionTrait>(
    conn: &C,
    category: &str,
    channel: &str,
    lock: bool,
) -> ApiResult<Option<RoutingRow>> {
    let sql = format!(
        "SELECT mode, version, canary_percent, fence_at, changed_at FROM notif_routing WHERE category = $1 AND channel = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    Ok(db::one(conn, &sql, vec![category.into(), channel.into()]).await?)
}

fn routing_resource(category: &str, channel: &str, r: Option<&RoutingRow>) -> Value {
    match r {
        None => Value::Null,
        Some(r) => json!({
            "category": category, "channel": channel, "mode": r.mode, "version": r.version,
            "canaryPercent": r.canary_percent, "fenceAt": r.fence_at, "changedAt": r.changed_at,
        }),
    }
}

/// `POST /internal/v1/operator/routing/{category}/{channel}/mode` — the governed API for
/// what `activate-notifications-routing` does with SQL: one `notif_routing` row moves to
/// `mode` at `expectedVersion` (the `version` of the routing read), bumping `version`,
/// moving `fenceAt` to now and recording `changedByRun` `admin-bridge:<key>`. Both replicas
/// read the table on every event, so the change applies at once everywhere. Gates:
/// [`routing_gate`] (400 when refused); the row already in `mode` is 400.
pub async fn change_routing(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path((category, channel)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let valid_category = (2..=40).contains(&category.len())
        && category.as_bytes()[0].is_ascii_uppercase()
        && category
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'_');
    if !valid_category || !CHANNELS.contains(&channel.as_str()) {
        return Err(ApiError::not_found());
    }
    let req: RoutingChange = parse(&body)?;
    let expected_version = expected(req.expected_version)?;
    let to = match req.mode.as_str() {
        "OFF" => Mode::Off,
        "SHADOW" => Mode::Shadow,
        "ACTIVE" => Mode::Active,
        _ => return Err(ApiError::validation("mode is OFF, SHADOW or ACTIVE.")),
    };
    let subject_id = format!("{category}/{channel}");
    let txn = state.db.begin().await?;
    let row = routing_row(&txn, &category, &channel, true).await?;
    if let Some((status, version)) =
        replayed(&txn, &cmd, ROUTING_CHANGE, Subject::Routing, &subject_id).await?
    {
        txn.rollback().await?;
        let current = routing_row(&state.db, &category, &channel, false).await?;
        return Ok(answer(
            &status,
            version,
            routing_resource(&category, &channel, current.as_ref()),
        ));
    }
    let Some(row) = row else {
        return Err(ApiError::not_found());
    };
    if i64::from(row.version) != expected_version {
        return Err(version_conflict(
            "The routing row changed since it was read; read it again.",
        ));
    }
    if row.mode == to.as_str() {
        return Err(ApiError::validation(
            "The routing row is already in that mode.",
        ));
    }
    let facts = GateFacts {
        email_provider: state.email_provider.is_some(),
        push_provider: state.push.is_some(),
        authorities: state.authorities.configured(),
    };
    if let Some(refusal) = routing_gate(&category, &channel, to, &facts) {
        return Err(ApiError::validation(refusal));
    }
    let run = format!("admin-bridge:{}", cmd.key);
    db::exec(
        &txn,
        "UPDATE notif_routing SET mode = $3, version = version + 1, fence_at = now(), changed_by_run = $4, changed_at = now()
          WHERE category = $1 AND channel = $2",
        vec![category.clone().into(), channel.clone().into(), to.as_str().into(), run.into()],
    )
    .await?;
    let after = routing_row(&txn, &category, &channel, false)
        .await?
        .ok_or(ApiError::unavailable(1))?;
    let new_version = i64::from(after.version);
    if let Some((status, version)) = record_and_commit(
        &state,
        txn,
        &cmd,
        ROUTING_CHANGE,
        Subject::Routing,
        &subject_id,
        to.as_str(),
        Some(new_version),
    )
    .await?
    {
        let current = routing_row(&state.db, &category, &channel, false).await?;
        return Ok(answer(
            &status,
            version,
            routing_resource(&category, &channel, current.as_ref()),
        ));
    }
    tracing::info!(operator = %cmd.operator, category = %category, channel = %channel, mode = to.as_str(), version = new_version, "bridge routing change");
    Ok(answer(
        to.as_str(),
        Some(new_version),
        routing_resource(&category, &channel, Some(&after)),
    ))
}

// --- notifications.reroute.cancel (R3) --------------------------------------------------

async fn decision_resource<C: ConnectionTrait>(conn: &C, decision: Uuid) -> ApiResult<Value> {
    #[derive(FromQueryResult)]
    struct Row {
        route_category: String,
        state: String,
        reject_reason: Option<String>,
        processed_at: Option<DateTime<Utc>>,
    }
    let row: Option<Row> = db::one(
        conn,
        "SELECT route_category, state, reject_reason, processed_at FROM notif_event_inbox
          WHERE route_decision_id = $1 AND NOT shadow",
        vec![decision.into()],
    )
    .await?;
    Ok(match row {
        None => Value::Null,
        Some(r) => json!({
            "decisionId": decision, "category": r.route_category, "eventState": r.state,
            "rejectReason": r.reject_reason, "processedAt": r.processed_at,
        }),
    })
}

/// `POST /internal/v1/operator/reroute/{decisionId}/cancel` — I11 for one decision: a
/// decision whose event was never dispatched becomes terminal `CANCELLED_FOR_REROUTE`
/// (status `CANCELLED`; an event already REJECTED is reported `CANCELLED` as I11 does);
/// 409 when any of its deliveries has started or been sent (never rerouted).
pub async fn cancel_reroute(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path(decision): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let decision = Uuid::parse_str(&decision).map_err(|_| ApiError::not_found())?;
    no_fields(&body)?;
    let subject_id = decision.to_string();
    let txn = state.db.begin().await?;
    let ev = internal::lock_decision(&txn, decision).await?;
    if let Some((status, version)) = replayed(
        &txn,
        &cmd,
        REROUTE_CANCEL,
        Subject::RouteDecision,
        &subject_id,
    )
    .await?
    {
        txn.rollback().await?;
        return Ok(answer(
            &status,
            version,
            decision_resource(&state.db, decision).await?,
        ));
    }
    let Some(ev) = ev else {
        return Err(ApiError::not_found());
    };
    if ev.state != "REJECTED" && !internal::cancel_for_reroute(&txn, &ev).await? {
        return Err(version_conflict(
            "A delivery of this decision has started or been sent; it is never rerouted.",
        ));
    }
    if let Some((status, version)) = record_and_commit(
        &state,
        txn,
        &cmd,
        REROUTE_CANCEL,
        Subject::RouteDecision,
        &subject_id,
        "CANCELLED",
        None,
    )
    .await?
    {
        return Ok(answer(
            &status,
            version,
            decision_resource(&state.db, decision).await?,
        ));
    }
    tracing::info!(operator = %cmd.operator, decision = %decision, "bridge reroute cancellation");
    Ok(answer(
        "CANCELLED",
        None,
        decision_resource(&state.db, decision).await?,
    ))
}

// --- settlement -------------------------------------------------------------------------

/// `GET /internal/v1/operator/operations/{idempotencyKey}` — the bridge's settlement read:
/// `{idempotency_key, outcome: APPLIED, source_status, source_version}` with the status and
/// version the command answered. 404 for an unknown key and for any caller but the bridge
/// with `notifications.administer` and `X-CF-Operator`.
pub async fn operation(
    State(state): State<AppState>,
    caller: Caller,
    Path(key): Path<String>,
) -> ApiResult<Response> {
    if caller
        .require_operator(InternalPermission::NotificationsAdminister)
        .is_err()
    {
        return Err(ApiError::not_found());
    }
    let key = Uuid::parse_str(&key).map_err(|_| ApiError::not_found())?;
    #[derive(FromQueryResult)]
    struct Row {
        resulting_state: String,
        resulting_version: Option<i64>,
    }
    let row: Option<Row> = db::one(
        &state.db,
        "SELECT resulting_state, resulting_version FROM notif_admin_operations WHERE idempotency_key = $1",
        vec![key.into()],
    )
    .await?;
    let row = row.ok_or_else(ApiError::not_found)?;
    Ok(json_ok(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": row.resulting_state,
        "source_version": row.resulting_version.map(|v| v.to_string()),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> GateFacts {
        GateFacts {
            email_provider: true,
            push_provider: true,
            authorities: vec![
                "account",
                "meeting",
                "messaging",
                "communications",
                "organizations",
            ],
        }
    }

    #[test]
    fn off_and_shadow_are_always_allowed() {
        let none = GateFacts::default();
        for category in [
            "AUTH_VERIFICATION",
            "CONTACT_REQUESTS",
            "MEETING_NOTIFICATION",
        ] {
            for channel in CHANNELS {
                assert_eq!(routing_gate(category, channel, Mode::Off, &none), None);
                assert_eq!(routing_gate(category, channel, Mode::Shadow, &none), None);
            }
        }
    }

    #[test]
    fn active_keeps_the_coordinated_release_gates() {
        let f = facts();
        for category in CRITICAL_CATEGORIES {
            assert!(
                routing_gate(category, "EMAIL", Mode::Active, &f).is_some(),
                "{category}"
            );
        }
        assert!(routing_gate("SECURITY_PASSWORD_CHANGED", "IN_APP", Mode::Active, &f).is_some());
        assert!(routing_gate("MEETING_NOTIFICATION", "EMAIL", Mode::Active, &f).is_some());
        assert_eq!(
            routing_gate("MEETING_NOTIFICATION", "IN_APP", Mode::Active, &f),
            None
        );
        assert_eq!(
            routing_gate("CONTACT_REQUESTS", "PUSH", Mode::Active, &f),
            None
        );
        assert_eq!(
            routing_gate("DEVELOPER_PLATFORM", "EMAIL", Mode::Active, &f),
            None
        );

        let no_email = GateFacts {
            email_provider: false,
            ..facts()
        };
        assert!(routing_gate("DEVELOPER_PLATFORM", "EMAIL", Mode::Active, &no_email).is_some());
        assert_eq!(
            routing_gate("DEVELOPER_PLATFORM", "IN_APP", Mode::Active, &no_email),
            None
        );
        let no_push = GateFacts {
            push_provider: false,
            ..facts()
        };
        assert!(routing_gate("CALL_INVITATIONS", "PUSH", Mode::Active, &no_push).is_some());
        let no_owners = GateFacts {
            authorities: vec![],
            ..facts()
        };
        assert!(routing_gate("CONTACT_REQUESTS", "IN_APP", Mode::Active, &no_owners).is_some());
        assert_eq!(
            routing_gate("ORGANIZATION_UPDATES", "IN_APP", Mode::Active, &no_owners),
            None
        );
    }

    #[test]
    fn every_catalogue_critical_category_is_refused() {
        let f = facts();
        for r in catalogue::ROUTES {
            if r.lane() == cf_notification_client::Lane::Critical {
                assert!(routing_gate(r.route_category, "EMAIL", Mode::Active, &f).is_some());
            }
        }
    }

    #[test]
    fn subjects_match_the_table_check() {
        let all = [
            Subject::Delivery,
            Subject::EmailSuppression,
            Subject::Routing,
            Subject::RouteDecision,
        ];
        let names: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            ["DELIVERY", "EMAIL_SUPPRESSION", "ROUTING", "ROUTE_DECISION"]
        );
        for c in [
            DELIVERY_RETRY,
            SUPPRESSION_REMOVE,
            ROUTING_CHANGE,
            REROUTE_CANCEL,
        ] {
            assert!(c.len() <= 48 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'.'));
        }
    }
}
