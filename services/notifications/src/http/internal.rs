//! Internal APIs I1, I3–I12 (Document 4 §3.3). Every response is PII-free: identifiers,
//! categories, lanes, statuses, error classes and keyed digests only.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{json_ok, middleware::Caller, parse};
use crate::{
    db,
    error::{ApiError, ApiResult, ErrorCode},
    ingest::{self, IngestError, IngestRequest},
    push, routing,
    state::AppState,
};

/// I1.
pub async fn ingest(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsEventPublish)?;
    let req: IngestRequest = parse(&body)?;
    match ingest::ingest(&state.db, &state.metrics, caller.service(), req).await {
        Ok(results) => Ok(json_ok(json!({ "results": results }))),
        Err(IngestError::BatchSize) => {
            Err(ApiError::validation("events must hold 1 to 100 envelopes"))
        }
        Err(IngestError::ProtocolUnsupported) => Err(ApiError {
            code: ErrorCode::ProtocolUnsupported,
            message: None,
            retry_after: Some(1),
        }),
        Err(IngestError::Backpressure) => Err(ApiError::unavailable(30)),
        Err(IngestError::Db(e)) => Err(e.into()),
    }
}

/// I3: the call-push fast lane.
pub async fn call_push(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsCallPushDispatch)?;
    let req: push::CallPushRequest = parse(&body)?;
    if req.push_target_refs.is_empty() || req.push_target_refs.len() > 20 {
        return Err(ApiError::validation(
            "pushTargetRefs holds 1 to 20 references",
        ));
    }
    // The contract payload is validated by Calling; only metadata is ever forwarded.
    if !req.payload.is_object() {
        return Err(ApiError::validation("payload must be an object"));
    }
    let modes = routing::category(&state.db, "CALL_INVITATIONS").await?;
    if routing::highest(&modes) != routing::Mode::Active {
        let results: Vec<Value> = req
            .push_target_refs
            .iter()
            .map(|r| json!({ "pushTargetRef": r, "outcome": "NOT_CONFIGURED" }))
            .collect();
        return Ok(json_ok(json!({ "results": results })));
    }
    let out = push::call_push(
        &state.db,
        &state.keys,
        state.push.as_ref(),
        &req,
        Utc::now(),
    )
    .await?;
    Ok(json_ok(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryQuery {
    notification_id: Option<Uuid>,
    delivery_id: Option<Uuid>,
    event_id: Option<String>,
    decision_id: Option<Uuid>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// I4: PII-free delivery search.
pub async fn deliveries(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<DeliveryQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsObservabilityRead)?;
    if q.notification_id.is_none()
        && q.delivery_id.is_none()
        && q.event_id.is_none()
        && q.decision_id.is_none()
    {
        return Err(ApiError::validation(
            "one of notificationId, deliveryId, eventId or decisionId is required",
        ));
    }
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let after = q.cursor.as_deref().and_then(|c| Uuid::parse_str(c).ok());
    #[derive(FromQueryResult)]
    struct Row {
        delivery_id: Uuid,
        notification_id: Uuid,
        category: String,
        lane: String,
        channel: String,
        status: String,
        attempts: i32,
        last_error_class: Option<String>,
        routing_version: i32,
        decision_id: Option<Uuid>,
        created_at: DateTime<Utc>,
        completed_at: Option<DateTime<Utc>>,
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "SELECT d.id AS delivery_id, n.id AS notification_id, n.category, d.lane, d.channel, d.status, d.attempts,
                d.last_error_class, d.routing_version, e.route_decision_id AS decision_id, d.created_at, d.completed_at
           FROM notif_deliveries d
           JOIN notif_notifications n ON n.id = d.notification_id
           LEFT JOIN notif_event_inbox e ON e.producer = n.source_producer AND e.event_id = n.source_event_id
          WHERE ($1::uuid IS NULL OR n.id = $1) AND ($2::uuid IS NULL OR d.id = $2)
            AND ($3::text IS NULL OR n.source_event_id = $3) AND ($4::uuid IS NULL OR e.route_decision_id = $4)
            AND ($5::uuid IS NULL OR d.id > $5)
          ORDER BY d.id LIMIT $6",
        vec![
            q.notification_id.into(),
            q.delivery_id.into(),
            q.event_id.clone().into(),
            q.decision_id.into(),
            after.into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "deliveryId": r.delivery_id, "notificationId": r.notification_id, "category": r.category,
                "lane": r.lane, "channel": r.channel, "status": r.status, "attempts": r.attempts,
                "lastErrorClass": r.last_error_class, "routingVersion": r.routing_version,
                "decisionId": r.decision_id, "createdAt": r.created_at, "completedAt": r.completed_at,
            })
        })
        .collect();
    let next = if more {
        rows.get(limit as usize - 1)
            .map(|r| r.delivery_id.to_string())
    } else {
        None
    };
    Ok(json_ok(json!({ "items": items, "nextCursor": next })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeadLetterQuery {
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

/// I5: FAILED deliveries, REJECTED and HELD events (and events parked after their retry
/// budget, reported as REJECTED with their error class).
pub async fn dead_letters(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<DeadLetterQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsObservabilityRead)?;
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let before: Option<DateTime<Utc>> = q
        .cursor
        .as_deref()
        .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
        .map(|t| t.with_timezone(&Utc));
    let kind = q.kind.as_deref();
    if kind.is_some_and(|k| !matches!(k, "FAILED_DELIVERY" | "REJECTED_EVENT" | "HELD_EVENT")) {
        return Err(ApiError::validation(
            "kind is FAILED_DELIVERY, REJECTED_EVENT or HELD_EVENT",
        ));
    }
    #[derive(FromQueryResult)]
    struct Row {
        kind: String,
        id: String,
        category: Option<String>,
        reason: String,
        at: DateTime<Utc>,
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "SELECT * FROM (
            SELECT 'FAILED_DELIVERY' AS kind, 'delivery:' || d.id::text AS id, n.route_category AS category,
                   COALESCE(d.last_error_class, 'FAILED') AS reason, COALESCE(d.completed_at, d.created_at) AS at
              FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id WHERE d.status = 'FAILED'
            UNION ALL
            SELECT 'REJECTED_EVENT', 'event:' || producer || ':' || event_id, route_category,
                   COALESCE(reject_reason, last_error_class, 'REJECTED'), received_at
              FROM notif_event_inbox WHERE state = 'REJECTED' OR (state = 'PENDING' AND attempts >= 30)
            UNION ALL
            SELECT 'HELD_EVENT', 'event:' || producer || ':' || event_id, route_category, 'CATEGORY_NOT_ACTIVE', received_at
              FROM notif_event_inbox WHERE state = 'HELD'
         ) x
         WHERE ($1::text IS NULL OR kind = $1) AND ($2::timestamptz IS NULL OR at < $2)
         ORDER BY at DESC LIMIT $3",
        vec![kind.map(str::to_string).into(), before.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&Row> = rows.iter().take(limit as usize).collect();
    let next = if more {
        page.last().map(|r| r.at.to_rfc3339())
    } else {
        None
    };
    Ok(json_ok(json!({
        "items": page.iter().map(|r| json!({ "kind": r.kind, "id": r.id, "category": r.category, "reason": r.reason, "at": r.at })).collect::<Vec<_>>(),
        "nextCursor": next,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {
    ids: Vec<String>,
}

/// I6: replays HELD events whose category is now routable and failed PUSH deliveries.
/// Refuses expired items, superseded ones, CRITICAL items (a code is never re-sent: the user
/// requests a new one) and email whose content was erased at its final state.
pub async fn replay(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsOperationsReplay)?;
    require_idempotency_key(&headers)?;
    let req: ReplayRequest = parse(&body)?;
    if req.ids.is_empty() || req.ids.len() > 100 {
        return Err(ApiError::validation("ids holds 1 to 100 identifiers"));
    }
    let mut results = Vec::new();
    for id in &req.ids {
        let outcome = replay_one(&state, id).await?;
        tracing::info!(operator = caller.service(), item = %id, outcome, "dead-letter replay");
        results.push(json!({ "id": id, "outcome": outcome }));
    }
    Ok(json_ok(json!({ "results": results })))
}

async fn replay_one(state: &AppState, id: &str) -> ApiResult<&'static str> {
    if let Some(rest) = id.strip_prefix("event:") {
        let Some((producer, event_id)) = rest.split_once(':') else {
            return Ok("NOT_FOUND");
        };
        #[derive(FromQueryResult)]
        struct Ev {
            state: String,
            lane: String,
            route_category: String,
            envelope: Value,
        }
        let ev: Option<Ev> = db::one(
            &state.db,
            "SELECT state, lane, route_category, envelope FROM notif_event_inbox WHERE producer = $1 AND event_id = $2",
            vec![producer.into(), event_id.into()],
        )
        .await?;
        let Some(ev) = ev else { return Ok("NOT_FOUND") };
        if ev.lane == "CRITICAL" {
            return Ok("REFUSED_EXPIRED");
        }
        let expired = ev
            .envelope
            .pointer("/delivery/expiresAt")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| t.with_timezone(&Utc) <= Utc::now());
        if expired {
            return Ok("REFUSED_EXPIRED");
        }
        match ev.state.as_str() {
            "HELD" | "PENDING" => {
                let modes = routing::category(&state.db, &ev.route_category).await?;
                if routing::highest(&modes) == routing::Mode::Off {
                    return Ok("REFUSED_ROUTE_NOT_NEW");
                }
                db::exec(
                    &state.db,
                    "UPDATE notif_event_inbox SET state = 'PENDING', attempts = 0, lease_id = NULL, lease_until = NULL
                      WHERE producer = $1 AND event_id = $2 AND state IN ('HELD', 'PENDING')",
                    vec![producer.into(), event_id.into()],
                )
                .await?;
                Ok("REPLAYED")
            }
            _ => Ok("REFUSED_SUPERSEDED"),
        }
    } else if let Some(rest) = id.strip_prefix("delivery:") {
        let Ok(delivery) = Uuid::parse_str(rest) else {
            return Ok("NOT_FOUND");
        };
        #[derive(FromQueryResult)]
        struct D {
            channel: String,
            status: String,
            lane: String,
            last_error_class: Option<String>,
            expires_at: Option<DateTime<Utc>>,
        }
        let d: Option<D> = db::one(
            &state.db,
            "SELECT d.channel, d.status, d.lane, d.last_error_class, n.expires_at
               FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id WHERE d.id = $1",
            vec![delivery.into()],
        )
        .await?;
        let Some(d) = d else { return Ok("NOT_FOUND") };
        if d.lane == "CRITICAL" || d.expires_at.is_some_and(|e| e <= Utc::now()) {
            return Ok("REFUSED_EXPIRED");
        }
        if d.status != "FAILED"
            || d.channel != "PUSH"
            || d.last_error_class.as_deref() == Some("INVALID_HANDLE")
        {
            return Ok("REFUSED_SUPERSEDED");
        }
        db::exec(
            &state.db,
            "UPDATE notif_deliveries SET status = 'PENDING', next_attempt_at = now(), attempts = 0, completed_at = NULL WHERE id = $1 AND status = 'FAILED'",
            vec![delivery.into()],
        )
        .await?;
        Ok("REPLAYED")
    } else {
        Ok("NOT_FOUND")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinceQuery {
    since: Option<DateTime<Utc>>,
}

/// I7: per category and channel, what the old path sent (producer shadow copies) and what
/// the service would have done, and the number of keys where they disagree.
pub async fn shadow_report(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<SinceQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsObservabilityRead)?;
    let since = q.since.unwrap_or_else(|| Utc::now() - Duration::hours(24));
    #[derive(FromQueryResult)]
    struct Row {
        category: String,
        channel: String,
        legacy: i64,
        service: i64,
        mismatches: i64,
        unresolved: i64,
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "WITH ns AS (
             SELECT category, channel, dedupe_key, recipient_digest, decision FROM notif_shadow_observations
              WHERE side = 'NS' AND observed_at >= $1),
         legacy AS (
             SELECT category, dedupe_key, recipient_digest FROM notif_shadow_observations
              WHERE side = 'LEGACY' AND observed_at >= $1)
         SELECT ns.category, ns.channel,
                count(l.dedupe_key)::bigint AS legacy,
                count(*) FILTER (WHERE ns.decision = 'WOULD_DELIVER')::bigint AS service,
                count(*) FILTER (WHERE (l.dedupe_key IS NULL) = (ns.decision = 'WOULD_DELIVER'))::bigint AS mismatches,
                count(*) FILTER (WHERE ns.decision = 'UNRESOLVED_ADDR')::bigint AS unresolved
           FROM ns LEFT JOIN legacy l ON l.category = ns.category AND l.dedupe_key = ns.dedupe_key AND l.recipient_digest = ns.recipient_digest
          GROUP BY ns.category, ns.channel ORDER BY 1, 2",
        vec![since.into()],
    )
    .await?;
    Ok(json_ok(json!({
        "since": since,
        "rows": rows.iter().map(|r| json!({
            "category": r.category, "channel": r.channel, "legacy": r.legacy, "service": r.service,
            "mismatches": r.mismatches, "unresolvedAddresses": r.unresolved,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoursQuery {
    hours: Option<i64>,
}

/// I8: routing state and the hourly route audit.
pub async fn routing(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<HoursQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsObservabilityRead)?;
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 14);
    let rows = routing::rows(&state.db).await?;
    let audit = ingest::route_audit(&state.db, None, hours).await?;
    Ok(json_ok(json!({
        "routingProtocol": ingest::SUPPORTED_PROTOCOLS,
        "routing": rows.iter().map(|r| json!({
            "category": r.category, "channel": r.channel, "mode": r.mode, "fenceAt": r.fence_at,
            "version": r.version, "canaryPercent": r.canary_percent, "reminderBoundary": r.reminder_boundary,
            "changedByRun": r.changed_by_run,
        })).collect::<Vec<_>>(),
        "audit": audit.as_array().cloned().unwrap_or_default().into_iter().map(|a| json!({
            "producer": a["producer"], "category": a["category"], "hour": a["hour"],
            "acceptedDecisions": a["count"], "digest": a["digest"],
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

/// I9: effective suppressions (keyed digests only).
pub async fn suppressions(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsOperationsReplay)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    #[derive(FromQueryResult)]
    struct Row {
        recipient_digest: String,
        reason: String,
        created_at: DateTime<Utc>,
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "SELECT recipient_digest, CASE WHEN reason = 'SOFT_BOUNCE' THEN 'HARD_BOUNCE' ELSE reason END AS reason, created_at
           FROM notif_email_suppressions
          WHERE (reason <> 'SOFT_BOUNCE' OR bounce_count >= 2) AND ($1::text IS NULL OR recipient_digest > $1)
          ORDER BY recipient_digest LIMIT $2",
        vec![q.cursor.clone().into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&Row> = rows.iter().take(limit as usize).collect();
    Ok(json_ok(json!({
        "items": page.iter().map(|r| json!({ "digest": r.recipient_digest, "reason": r.reason, "createdAt": r.created_at })).collect::<Vec<_>>(),
        "nextCursor": if more { page.last().map(|r| r.recipient_digest.clone()) } else { None },
    })))
}

/// I10 (audited).
pub async fn remove_suppression(
    State(state): State<AppState>,
    caller: Caller,
    Path(digest): Path<String>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsOperationsReplay)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::validation("digest is 64 hex characters"));
    }
    let n = db::exec(
        &state.db,
        "DELETE FROM notif_email_suppressions WHERE recipient_digest = $1",
        vec![digest.to_ascii_lowercase().into()],
    )
    .await?;
    tracing::info!(
        operator = caller.service(),
        removed = n,
        "email suppression removed"
    );
    if n == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RerouteRequest {
    decision_ids: Vec<Uuid>,
}

/// I11: first phase of the two-phase reroute (§9.8). A decision whose event was never
/// dispatched (unprocessed, or only never-started deliveries) becomes terminal
/// `CANCELLED_FOR_REROUTE`; anything in flight or sent is reported and never rerouted.
pub async fn reroute_cancel(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsOperationsReroute)?;
    require_idempotency_key(&headers)?;
    let req: RerouteRequest = parse(&body)?;
    if req.decision_ids.is_empty() || req.decision_ids.len() > 500 {
        return Err(ApiError::validation(
            "decisionIds holds 1 to 500 identifiers",
        ));
    }
    let mut results = Vec::new();
    for decision in &req.decision_ids {
        let outcome = reroute_one(&state, *decision).await?;
        results.push(json!({ "decisionId": decision, "outcome": outcome }));
    }
    tracing::info!(
        operator = caller.service(),
        count = results.len(),
        "reroute cancellation"
    );
    Ok(json_ok(json!({ "results": results })))
}

async fn reroute_one(state: &AppState, decision: Uuid) -> ApiResult<&'static str> {
    use sea_orm::TransactionTrait;
    let txn = state.db.begin().await?;
    let ev = txn
        .query_one(db::stmt(
            "SELECT producer, event_id, state FROM notif_event_inbox WHERE route_decision_id = $1 AND NOT shadow FOR UPDATE",
            vec![decision.into()],
        ))
        .await?;
    let Some(ev) = ev else {
        txn.rollback().await?;
        return Ok("UNKNOWN");
    };
    let producer: String = ev.try_get("", "producer").map_err(ApiError::from)?;
    let event_id: String = ev.try_get("", "event_id").map_err(ApiError::from)?;
    let st: String = ev.try_get("", "state").map_err(ApiError::from)?;
    if st == "REJECTED" {
        txn.rollback().await?;
        return Ok("CANCELLED");
    }
    let started = db::scalar_i64(
        &txn,
        "SELECT count(*)::bigint AS n FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
          LEFT JOIN notif_email_messages m ON m.delivery_id = d.id
          WHERE n.source_producer = $1 AND n.source_event_id = $2
            AND (d.status NOT IN ('PENDING', 'DEFERRED', 'SUPPRESSED', 'SHADOW', 'CANCELLED')
                 OR (m.id IS NOT NULL AND (m.status <> 'QUEUED' OR m.attempts > 0)))",
        vec![producer.clone().into(), event_id.clone().into()],
    )
    .await?;
    if started > 0 {
        txn.rollback().await?;
        return Ok("IN_FLIGHT_OR_SENT");
    }
    db::exec(
        &txn,
        "UPDATE notif_email_messages SET status = 'FAILED', last_error_class = 'CANCELLED_FOR_REROUTE', body_sealed = NULL, final_at = now()
          WHERE status = 'QUEUED' AND delivery_id IN (SELECT d.id FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
                                                      WHERE n.source_producer = $1 AND n.source_event_id = $2)",
        vec![producer.clone().into(), event_id.clone().into()],
    )
    .await?;
    db::exec(
        &txn,
        "UPDATE notif_deliveries SET status = 'CANCELLED', completed_at = now()
          WHERE status IN ('PENDING', 'DEFERRED') AND notification_id IN (SELECT id FROM notif_notifications WHERE source_producer = $1 AND source_event_id = $2)",
        vec![producer.clone().into(), event_id.clone().into()],
    )
    .await?;
    db::exec(
        &txn,
        "UPDATE notif_notifications SET state = 'CANCELLED_FOR_REROUTE' WHERE source_producer = $1 AND source_event_id = $2",
        vec![producer.clone().into(), event_id.clone().into()],
    )
    .await?;
    db::exec(
        &txn,
        "UPDATE notif_event_inbox SET state = 'REJECTED', reject_reason = 'CANCELLED_FOR_REROUTE', processed_at = now(),
                lease_id = NULL, lease_until = NULL, envelope = envelope - 'sealed'
          WHERE producer = $1 AND event_id = $2",
        vec![producer.into(), event_id.into()],
    )
    .await?;
    txn.commit().await?;
    Ok("CANCELLED")
}

#[derive(Debug, Deserialize)]
pub struct AlertmanagerPayload {
    version: String,
    status: String,
    #[serde(default)]
    alerts: Vec<Value>,
}

/// I12: Alertmanager webhook → operator group email (static bearer; 404 when unset).
pub async fn operator_alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let Some(token) = state.operator_alert_token.as_ref() else {
        return Err(ApiError::not_found());
    };
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let expected = token.expose_secret().as_bytes();
    let equal = presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !equal {
        return Err(ApiError::new(ErrorCode::AuthenticationRequired));
    }
    let payload: AlertmanagerPayload = serde_json::from_slice(&body)
        .map_err(|_| ApiError::validation("not an Alertmanager webhook"))?;
    if payload.version != "4"
        || !matches!(payload.status.as_str(), "firing" | "resolved")
        || payload.alerts.len() > 100
    {
        return Err(ApiError::validation("unsupported Alertmanager payload"));
    }
    let mut accepted = 0;
    for a in &payload.alerts {
        let name = a
            .pointer("/labels/alertname")
            .and_then(Value::as_str)
            .unwrap_or("alert");
        let severity = a
            .pointer("/labels/severity")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let status = a
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or(&payload.status);
        let fingerprint = a.get("fingerprint").and_then(Value::as_str).unwrap_or("");
        let starts = a.get("startsAt").and_then(Value::as_str).unwrap_or("");
        let clean = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .take(64)
                .collect::<String>()
        };
        let key = format!(
            "alert:{}:{}:{}",
            clean(fingerprint),
            clean(starts),
            clean(status)
        );
        let event_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()).to_string();
        let envelope = json!({
            "schemaVersion": 1, "eventId": event_id, "eventType": "operator.alert.fired", "eventVersion": 1,
            "producer": "alertmanager", "occurredAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "correlationId": format!("corr_{}", Uuid::new_v4().simple()),
            "recipients": [{ "ref": "operator-group:operators" }],
            "delivery": { "priority": "HIGH", "dedupeKey": key },
            "route": { "decisionId": Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()), "routingVersion": 1, "protocol": 1 },
            "data": { "alertName": clean(name), "severity": clean(severity), "status": clean(status) },
        });
        let r = ingest::ingest(
            &state.db,
            &state.metrics,
            "alertmanager",
            IngestRequest {
                events: vec![envelope],
                dry_run: false,
                shadow: false,
            },
        )
        .await;
        if matches!(&r, Ok(v) if v.iter().all(|x| x.outcome != "REJECTED")) {
            accepted += 1;
        }
    }
    Ok(json_ok(json!({ "accepted": accepted })))
}

fn require_idempotency_key(headers: &HeaderMap) -> ApiResult<()> {
    let ok = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|k| (8..=128).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_graphic()));
    if ok {
        Ok(())
    } else {
        Err(ApiError::validation("Idempotency-Key is required"))
    }
}
