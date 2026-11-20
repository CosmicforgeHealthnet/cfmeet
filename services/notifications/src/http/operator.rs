//! The admin bridge's operator reads (ADR 0068, design §6.13): `/internal/v1/operator/*`.
//!
//! Only `cfmeet.admin-bridge` can hold `notifications.administer`, and every request must
//! name the HQ staff member it reads for (`X-CF-Operator`). The reads are PII-free:
//! deliveries carry ids, channels, states and times (never content or addresses);
//! suppressions are found by address or digest but only the keyed digest is ever
//! returned; provider status never includes a credential.

use axum::{
    extract::{Query, State},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, TimeZone, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{json_ok, middleware::Caller};
use crate::{db, error::ApiError, error::ApiResult, state::AppState};

const MAX_WINDOW_DAYS: i64 = 31;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowQuery {
    since: Option<String>,
    until: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

fn time(v: Option<&str>, name: &str) -> ApiResult<Option<DateTime<Utc>>> {
    v.map(|v| {
        DateTime::parse_from_rfc3339(v)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| ApiError::validation(format!("{name} must be an RFC 3339 timestamp")))
    })
    .transpose()
}

/// Keyset cursor `<created_at micros>.<delivery id>` (newest first).
fn parse_cursor(c: &str) -> ApiResult<(DateTime<Utc>, Uuid)> {
    let bad = || ApiError::validation("cursor is invalid");
    let (micros, id) = c.split_once('.').ok_or_else(bad)?;
    let micros: i64 = micros.parse().map_err(|_| bad())?;
    let at = Utc.timestamp_micros(micros).single().ok_or_else(bad)?;
    Ok((at, Uuid::parse_str(id).map_err(|_| bad())?))
}

fn cursor_of(at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}.{}", at.timestamp_micros(), id)
}

#[derive(FromQueryResult)]
struct DeliveryRow {
    delivery_id: Uuid,
    notification_id: Uuid,
    category: String,
    lane: String,
    channel: String,
    status: String,
    attempts: i32,
    last_error_class: Option<String>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

async fn delivery_page(
    state: &AppState,
    q: &WindowQuery,
    failed_only: bool,
) -> ApiResult<Response> {
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let since = time(q.since.as_deref(), "since")?;
    let until = time(q.until.as_deref(), "until")?;
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s || u - s > Duration::days(MAX_WINDOW_DAYS) {
            return Err(ApiError::validation(
                "until must be after since, within 31 days",
            ));
        }
    }
    let after = q.cursor.as_deref().map(parse_cursor).transpose()?;
    let rows: Vec<DeliveryRow> = db::all(
        &state.db,
        "SELECT d.id AS delivery_id, n.id AS notification_id, n.category, d.lane, d.channel, d.status, d.attempts,
                d.last_error_class, d.created_at, d.completed_at
           FROM notif_deliveries d
           JOIN notif_notifications n ON n.id = d.notification_id
          WHERE ($1::timestamptz IS NULL OR d.created_at >= $1) AND ($2::timestamptz IS NULL OR d.created_at < $2)
            AND (NOT $3 OR d.status = 'FAILED')
            AND ($4::timestamptz IS NULL OR (d.created_at, d.id) < ($4, $5::uuid))
          ORDER BY d.created_at DESC, d.id DESC LIMIT $6",
        vec![
            since.into(),
            until.into(),
            failed_only.into(),
            after.map(|a| a.0).into(),
            after.map(|a| a.1).into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&DeliveryRow> = rows.iter().take(limit as usize).collect();
    let items: Vec<Value> = page
        .iter()
        .map(|r| {
            json!({
                "deliveryId": r.delivery_id, "notificationId": r.notification_id, "category": r.category,
                "lane": r.lane, "channel": r.channel, "status": r.status, "attempts": r.attempts,
                "lastErrorClass": r.last_error_class, "createdAt": r.created_at, "completedAt": r.completed_at,
            })
        })
        .collect();
    let next = if more {
        page.last().map(|r| cursor_of(r.created_at, r.delivery_id))
    } else {
        None
    };
    Ok(json_ok(json!({ "items": items, "nextCursor": next })))
}

/// `GET /internal/v1/operator/deliveries?since&until&cursor&limit` — deliveries newest first.
pub async fn deliveries(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<WindowQuery>,
) -> ApiResult<Response> {
    caller.require_operator(InternalPermission::NotificationsAdminister)?;
    delivery_page(&state, &q, false).await
}

/// `GET /internal/v1/operator/failed-deliveries?since&until&cursor&limit` — FAILED deliveries
/// newest first.
pub async fn failed_deliveries(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<WindowQuery>,
) -> ApiResult<Response> {
    caller.require_operator(InternalPermission::NotificationsAdminister)?;
    delivery_page(&state, &q, true).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuppressionQuery {
    address: Option<String>,
    digest: Option<String>,
}

/// `GET /internal/v1/operator/email-suppressions?address=…|digest=…` — the effective
/// suppression of one recipient. The address is digested here with the service's own key
/// and never stored, logged or returned; only the keyed digest leaves the service. `version`
/// is the row version a bridge removal must name (`expectedVersion`).
pub async fn suppressions(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<SuppressionQuery>,
) -> ApiResult<Response> {
    caller.require_operator(InternalPermission::NotificationsAdminister)?;
    let digest = match (q.address.as_deref(), q.digest.as_deref()) {
        (Some(a), None) => {
            let a = a.trim();
            if a.is_empty() || a.len() > 254 || !a.contains('@') {
                return Err(ApiError::validation("address must be an email address"));
            }
            state.keys.email_digest(a)
        }
        (None, Some(d)) if d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()) => {
            d.to_ascii_lowercase()
        }
        _ => {
            return Err(ApiError::validation(
                "exactly one of address or digest (64 hex) is required",
            ))
        }
    };
    #[derive(FromQueryResult)]
    struct Row {
        recipient_digest: String,
        reason: String,
        created_at: DateTime<Utc>,
        version: i64,
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "SELECT recipient_digest, CASE WHEN reason = 'SOFT_BOUNCE' THEN 'HARD_BOUNCE' ELSE reason END AS reason, created_at, version
           FROM notif_email_suppressions
          WHERE recipient_digest = $1 AND (reason <> 'SOFT_BOUNCE' OR bounce_count >= 2)",
        vec![digest.into()],
    )
    .await?;
    Ok(json_ok(json!({
        "items": rows.iter().map(|r| json!({ "digest": r.recipient_digest, "reason": r.reason, "createdAt": r.created_at, "version": r.version })).collect::<Vec<_>>(),
        "nextCursor": null,
    })))
}

/// `GET /internal/v1/operator/providers` — per-channel provider status from configuration
/// and the last hour of deliveries. Never a credential, key or provider response body.
pub async fn providers(State(state): State<AppState>, caller: Caller) -> ApiResult<Response> {
    caller.require_operator(InternalPermission::NotificationsAdminister)?;
    #[derive(FromQueryResult)]
    struct Stats {
        last_success_at: Option<DateTime<Utc>>,
        last_failure_at: Option<DateTime<Utc>>,
        last_error_class: Option<String>,
        recent: i64,
    }
    let now = Utc::now();
    let mut out = Vec::new();
    for (channel, configured) in [
        ("EMAIL", state.email_provider.is_some()),
        ("PUSH", state.push.is_some()),
    ] {
        let stats: Option<Stats> = db::one(
            &state.db,
            "SELECT
                max(COALESCE(completed_at, created_at)) FILTER (WHERE status IN ('SENT', 'DELIVERED')) AS last_success_at,
                max(COALESCE(completed_at, created_at)) FILTER (WHERE status = 'FAILED') AS last_failure_at,
                (array_agg(last_error_class ORDER BY COALESCE(completed_at, created_at) DESC) FILTER (WHERE status = 'FAILED'))[1] AS last_error_class,
                count(*) FILTER (WHERE created_at >= $2) AS recent
               FROM notif_deliveries WHERE channel = $1 AND created_at >= $3",
            vec![channel.into(), (now - Duration::hours(1)).into(), (now - Duration::days(1)).into()],
        )
        .await?;
        let s = stats.unwrap_or(Stats {
            last_success_at: None,
            last_failure_at: None,
            last_error_class: None,
            recent: 0,
        });
        let failing_now = match (s.last_failure_at, s.last_success_at) {
            (Some(f), Some(ok)) => f > ok && f >= now - Duration::hours(1),
            (Some(f), None) => f >= now - Duration::hours(1),
            _ => false,
        };
        let status = if !configured {
            "NOT_CONFIGURED"
        } else if failing_now {
            "DEGRADED"
        } else if s.last_success_at.is_some() {
            "OK"
        } else {
            "UNKNOWN"
        };
        out.push(json!({
            "channel": channel, "configured": configured, "status": status,
            "lastSuccessAt": s.last_success_at, "lastFailureAt": s.last_failure_at,
            "lastErrorClass": s.last_error_class, "recentDeliveries": s.recent,
        }));
    }
    Ok(json_ok(json!({ "providers": out, "checkedAt": now })))
}
