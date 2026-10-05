//! Internal provider routes (spec Part E §7). Not part of the public API and not the
//! user webhook system (`/api/v1/webhooks`).
//!
//! - `POST /internal/rtc/livekit/webhook` — LiveKit callbacks. Accepted only with a valid
//!   LiveKit signature (JWT signed with the API secret whose `sha256` claim matches the
//!   body); anything else is rejected with 401 before touching state.
//! - `GET /internal/rtc/metrics` — control-plane counters; requires `X-Internal-Token`
//!   equal to `INTERNAL_API_TOKEN` (unset → the route answers 404).
//! - `POST /internal/transcription/events` — provider-neutral transcription callbacks
//!   (Part G). Accepted only with `X-CF-Timestamp` + `X-CF-Signature`
//!   (`sha256=HMAC(TRANSCRIPTION_WEBHOOK_SECRET, ts + "." + body)`); unset secret → 404.
//! - `GET /internal/metrics` — Prometheus text exposition (Part I; bounded labels, no
//!   PII); `GET /internal/operations/...` — conference health and incidents. Both
//!   require `X-Internal-Token`.

use axum::{
    extract::{
        rejection::{PathRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{
    dto::v2::{
        CallbackAck, ConferenceHealthListResponse, IncidentListResponse, OperationsQuery,
        ProviderTranscriptionEvent,
    },
    error::{ApiError, ErrorCode},
    services::{
        artifacts::transcription::verify_callback,
        ops_metrics::OPS_METRICS,
        outbox,
        quality::sink::HealthState,
        rtc_metrics::{inc, RTC_METRICS as M},
    },
    state::AppState,
};

fn not_found() -> Response {
    ApiError::NotFound("Not found".into()).into_response()
}

fn invalid_query() -> Response {
    ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters").into_response()
}

pub async fn livekit_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let core = &state.meeting.core;
    if !core.flags.rtc_provider_control_v2 || !core.rtc.capabilities().webhooks {
        return not_found();
    }
    let authorization = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    let event = match state.meeting.rtc_webhooks.verify(&body, authorization) {
        Ok(e) => e,
        Err(e) => {
            inc(&M.webhooks_rejected);
            tracing::warn!("rejected provider webhook: {e}");
            return ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Invalid webhook signature",
            )
            .into_response();
        }
    };
    match state.meeting.rtc_webhooks.handle(&event).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(json!({ "received": true, "outcome": outcome.as_str() })),
        )
            .into_response(),
        // Transient failure: non-2xx makes the provider retry; dedupe absorbs replays.
        Err(e) => e.into_response(),
    }
}

/// Constant-time-length internal token check (unset token → route hidden).
fn internal_token_ok(headers: &HeaderMap) -> bool {
    let expected = std::env::var("INTERNAL_API_TOKEN").unwrap_or_default();
    let presented = headers
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    !expected.is_empty()
        && presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub async fn rtc_metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let (realtime, commands) = outbox::backlog(&state.db).await.unwrap_or((-1, -1));
    Json(json!({
        "counters": M.snapshot(),
        "collaboration": crate::services::collaboration::metrics::COLLAB_METRICS.snapshot(),
        "operations": OPS_METRICS.snapshot(),
        "outboxBacklog": { "realtime": realtime, "providerCommands": commands },
    }))
    .into_response()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

/// Provider transcription callback (status + segments). Signature-verified before any
/// parsing; replays are absorbed by `(job, provider segment id)` uniqueness.
pub async fn transcription_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(secret) = state
        .meeting
        .core
        .config
        .artifacts
        .transcription_webhook_secret
        .clone()
    else {
        return not_found();
    };
    let ts = header(&headers, "x-cf-timestamp");
    let sig = header(&headers, "x-cf-signature");
    if !verify_callback(&secret, ts, &body, sig) {
        crate::services::ops_metrics::inc(&OPS_METRICS.transcription_callbacks_rejected);
        return ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Invalid callback signature",
        )
        .into_response();
    }
    let event: ProviderTranscriptionEvent = match serde_json::from_str(&body) {
        Ok(e) => e,
        Err(_) => {
            return ApiError::domain(ErrorCode::ValidationError, "Invalid callback body")
                .into_response()
        }
    };
    match state.meeting.transcription.ingest(&event).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(CallbackAck {
                received: true,
                outcome: outcome.as_str().to_string(),
                job_id: None,
            }),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

fn prometheus_counters(out: &mut String, prefix: &str, snapshot: &serde_json::Value) {
    use std::fmt::Write;
    if let Some(map) = snapshot.as_object() {
        for (k, v) in map {
            if let Some(n) = v.as_u64() {
                let name = format!("{prefix}_{k}_total");
                let _ = writeln!(out, "# TYPE {name} counter\n{name} {n}");
            }
        }
    }
}

/// Prometheus exposition: telemetry sink histograms/counters plus the process-wide
/// control-plane counters. Labels are bounded enums only.
pub async fn prometheus_metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let mut out = state.meeting.core.telemetry.export();
    prometheus_counters(&mut out, "cfmeet_rtc", &M.snapshot());
    prometheus_counters(
        &mut out,
        "cfmeet_collab",
        &crate::services::collaboration::metrics::COLLAB_METRICS.snapshot(),
    );
    prometheus_counters(&mut out, "cfmeet_ops", &OPS_METRICS.snapshot());
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
        .into_response()
}

/// `None` = invalid filter value.
fn parse_health(state: Option<&str>) -> Option<Option<HealthState>> {
    match state {
        None => Some(None),
        Some("HEALTHY") => Some(Some(HealthState::Healthy)),
        Some("DEGRADED") => Some(Some(HealthState::Degraded)),
        Some("UNHEALTHY") => Some(Some(HealthState::Unhealthy)),
        Some("UNKNOWN") => Some(Some(HealthState::Unknown)),
        Some(_) => None,
    }
}

/// Active conference health (support / operations; internal token).
pub async fn operations_conferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<OperationsQuery>, QueryRejection>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let Ok(Query(q)) = query else {
        return invalid_query();
    };
    let Some(filter) = parse_health(q.state.as_deref()) else {
        return ApiError::domain(
            ErrorCode::ValidationError,
            "state must be HEALTHY, DEGRADED, UNHEALTHY or UNKNOWN",
        )
        .into_response();
    };
    match state
        .meeting
        .operations
        .conferences(filter, q.limit.unwrap_or(100))
        .await
    {
        Ok(conferences) => Json(ConferenceHealthListResponse { conferences }).into_response(),
        Err(e) => e.into_response(),
    }
}

/// One conference's evaluated health.
pub async fn operations_conference(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<uuid::Uuid>, PathRejection>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let Ok(Path(id)) = path else {
        return ApiError::domain(ErrorCode::ValidationError, "Invalid path parameter")
            .into_response();
    };
    match state.meeting.operations.evaluate(id).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Operational incidents (provider drift, failures, room loss).
pub async fn operations_incidents(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<OperationsQuery>, QueryRejection>,
) -> Response {
    if !internal_token_ok(&headers) {
        return not_found();
    }
    let Ok(Query(q)) = query else {
        return invalid_query();
    };
    match state
        .meeting
        .operations
        .incidents(q.status.as_deref(), q.limit.unwrap_or(100))
        .await
    {
        Ok(incidents) => Json(IncidentListResponse { incidents }).into_response(),
        Err(e) => e.into_response(),
    }
}
