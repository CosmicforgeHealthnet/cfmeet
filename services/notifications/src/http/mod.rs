//! HTTP surface of the Notification Service (private listener only; nothing is published
//! on a host port).
//!
//! * `GET /health` — liveness; `GET /ready` — database, migrations, worker heartbeats per
//!   lane, `routingProtocol`, configured provider kinds (never credentials).
//! * `GET /metrics` — Prometheus text (counters + database gauges), private network only.
//! * `GET /openapi.json` — the internal contract served by this build.
//! * `POST /api/v1/notifications/provider-events/resend` — the Resend callback (Svix
//!   signature; 404 until a secret is configured; 256 KiB cap). Reached publicly only
//!   through the Meeting-hosted ingress route added at activation.
//! * `/internal/v1/**` — service-authenticated (I1–I11); I12 uses its own bearer.

pub mod internal;
pub mod middleware;
pub mod provider;
pub mod user;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, StatusCode},
    middleware::{from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    db,
    error::{ApiError, ErrorCode},
    state::AppState,
};

pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/notifications/internal/v1.openapi.json");

pub fn app(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.json", get(openapi))
        .route(
            "/api/v1/notifications/provider-events/resend",
            post(provider::resend),
        )
        .fallback(not_found)
        .with_state(state.clone())
        .layer(from_fn(middleware::request_context));
    let internal = Router::new()
        .route("/notification-events", post(internal::ingest))
        .route("/notifications/dispatch", post(user::dispatch))
        .route("/push/call-invitations", post(internal::call_push))
        .route("/admin/deliveries", get(internal::deliveries))
        .route("/admin/dead-letters", get(internal::dead_letters))
        .route("/admin/dead-letters/replay", post(internal::replay))
        .route("/admin/shadow-report", get(internal::shadow_report))
        .route("/admin/routing", get(internal::routing))
        .route("/admin/email-suppressions", get(internal::suppressions))
        .route(
            "/admin/email-suppressions/:digest",
            delete(internal::remove_suppression),
        )
        .route("/admin/reroute/cancel", post(internal::reroute_cancel))
        .route("/operator-alerts", post(internal::operator_alerts))
        .fallback(not_found)
        .with_state(state.clone())
        .layer(from_fn_with_state(
            state.clone(),
            middleware::require_internal_service,
        ));
    Router::new()
        .nest("/internal/v1", internal)
        .merge(public)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn openapi() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        OPENAPI,
    )
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "notifications" }))
}

async fn not_found() -> Response {
    ApiError::not_found().into_response()
}

/// Readiness: database reachable, migrations current, and (when workers run) every
/// started worker has beaten within 60 s. Reports the routing protocol for the release
/// tooling's mixed-version checks (§9.6).
async fn ready(State(state): State<AppState>) -> Response {
    let pending = crate::pending_migrations(&state.db).await;
    let (mut ok, database, migrations) = match pending {
        Ok(p) if p.is_empty() => (true, "ok", "current"),
        Ok(_) => (false, "ok", "pending"),
        Err(_) => (false, "unavailable", "unknown"),
    };
    let beats = state.heartbeats.snapshot();
    let now = Utc::now();
    let mut lanes = serde_json::Map::new();
    for (worker, at) in &beats {
        let fresh = now - *at < chrono::Duration::seconds(60);
        if state.workers_enabled && !fresh {
            ok = false;
        }
        lanes.insert(
            (*worker).to_string(),
            json!({ "lastBeatAt": at, "fresh": fresh }),
        );
    }
    let held = if ok {
        db::scalar_i64(
            &state.db,
            "SELECT count(*)::bigint AS n FROM notif_event_inbox WHERE state = 'HELD'",
            vec![],
        )
        .await
        .ok()
    } else {
        None
    };
    let active = if ok {
        db::scalar_i64(
            &state.db,
            "SELECT count(*)::bigint AS n FROM notif_routing WHERE mode <> 'OFF'",
            vec![],
        )
        .await
        .ok()
    } else {
        None
    };
    (
        if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "notifications",
            "replica": state.replica_id,
            "database": database,
            "migrations": migrations,
            "routingProtocol": crate::ingest::SUPPORTED_PROTOCOLS,
            "workers": { "enabled": state.workers_enabled, "lanes": lanes },
            "emailProvider": state.email.provider.as_str(),
            "emailWebhook": state.email.resend_webhook_secret.is_some(),
            "pushPlatforms": state.push.as_ref().map(|p| ["APNS", "FCM", "WEB_PUSH", "LOCAL_TEST"].iter().filter(|x| p.supports(x)).copied().collect::<Vec<_>>()).unwrap_or_default(),
            "sealedOpening": state.opening.is_some(),
            "authorities": state.authorities.configured(),
            "routing": { "nonOffRows": active },
            "heldEvents": held,
        })),
    )
        .into_response()
}

/// Prometheus text: per-replica counters and shared queue gauges.
async fn metrics(State(state): State<AppState>) -> Response {
    let mut out = state.metrics.render_counters();
    #[derive(sea_orm::FromQueryResult)]
    struct Gauge {
        lane: String,
        pending: i64,
        oldest: Option<i64>,
    }
    let queries = [
        ("notif_event_backlog", "notif_event_backlog_oldest_seconds", "SELECT lane, count(*)::bigint AS pending, EXTRACT(EPOCH FROM now() - min(received_at))::bigint AS oldest FROM notif_event_inbox WHERE state = 'PENDING' AND attempts < 30 GROUP BY lane"),
        ("notif_email_backlog", "notif_email_backlog_oldest_seconds", "SELECT lane, count(*)::bigint AS pending, EXTRACT(EPOCH FROM now() - min(created_at))::bigint AS oldest FROM notif_email_messages WHERE status IN ('QUEUED', 'SENDING') GROUP BY lane"),
        ("notif_push_backlog", "notif_push_backlog_oldest_seconds", "SELECT lane, count(*)::bigint AS pending, EXTRACT(EPOCH FROM now() - min(created_at))::bigint AS oldest FROM notif_deliveries WHERE channel = 'PUSH' AND status IN ('PENDING', 'DEFERRED', 'DISPATCHING') GROUP BY lane"),
    ];
    for (name, oldest_name, sql) in queries {
        match db::all::<Gauge, _>(&state.db, sql, vec![]).await {
            Ok(rows) => {
                out.push_str(&format!(
                    "# TYPE {name} gauge\n# TYPE {oldest_name} gauge\n"
                ));
                for lane in ["CRITICAL", "HIGH", "STANDARD"] {
                    let r = rows.iter().find(|r| r.lane == lane);
                    out.push_str(&format!(
                        "{name}{{lane=\"{lane}\"}} {}\n",
                        r.map(|r| r.pending).unwrap_or(0)
                    ));
                    out.push_str(&format!(
                        "{oldest_name}{{lane=\"{lane}\"}} {}\n",
                        r.and_then(|r| r.oldest).unwrap_or(0)
                    ));
                }
            }
            Err(_) => out.push_str("notif_database_up 0\n"),
        }
    }
    for (name, sql) in [
        ("notif_held_events", "SELECT count(*)::bigint AS n FROM notif_event_inbox WHERE state = 'HELD'"),
        ("notif_dead_letter_events", "SELECT count(*)::bigint AS n FROM notif_event_inbox WHERE state = 'PENDING' AND attempts >= 30"),
        ("notif_failed_deliveries_1h", "SELECT count(*)::bigint AS n FROM notif_deliveries WHERE status = 'FAILED' AND completed_at > now() - interval '1 hour'"),
        ("notif_active_routes", "SELECT count(*)::bigint AS n FROM notif_routing WHERE mode = 'ACTIVE'"),
    ] {
        if let Ok(n) = db::scalar_i64(&state.db, sql, vec![]).await {
            out.push_str(&format!("# TYPE {name} gauge\n{name} {n}\n"));
        }
    }
    out.push_str("# TYPE notif_up gauge\nnotif_up 1\n");
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

/// Parses a JSON body into `T` (unknown fields refused by every request type); serde
/// messages may quote values, so the envelope never echoes them.
pub fn parse<T: DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| ApiError::validation("The request body must be JSON."))?;
    if !value.is_object() {
        return Err(ApiError::validation(
            "The request body must be a JSON object.",
        ));
    }
    serde_json::from_value(value).map_err(|_| {
        ApiError::validation("The request body has a missing, unknown or mistyped field.")
    })
}

pub fn json_ok(v: Value) -> Response {
    let mut r = (StatusCode::OK, Json(v)).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}

#[allow(dead_code)]
fn _codes() -> ErrorCode {
    ErrorCode::NotFound
}
