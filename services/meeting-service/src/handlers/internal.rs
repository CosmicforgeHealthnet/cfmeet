//! Internal provider routes (spec Part E §7). Not part of the public API and not the
//! user webhook system (`/api/v1/webhooks`).
//!
//! - `POST /internal/rtc/livekit/webhook` — LiveKit callbacks. Accepted only with a valid
//!   LiveKit signature (JWT signed with the API secret whose `sha256` claim matches the
//!   body); anything else is rejected with 401 before touching state.
//! - `GET /internal/rtc/metrics` — control-plane counters; requires `X-Internal-Token`
//!   equal to `INTERNAL_API_TOKEN` (unset → the route answers 404).

use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{
    error::{ApiError, ErrorCode},
    services::{
        outbox,
        rtc_metrics::{inc, RTC_METRICS as M},
    },
    state::AppState,
};

fn not_found() -> Response {
    ApiError::NotFound("Not found".into()).into_response()
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

pub async fn rtc_metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let expected = std::env::var("INTERNAL_API_TOKEN").unwrap_or_default();
    let presented = headers
        .get("x-internal-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if expected.is_empty() || presented.len() != expected.len() || presented != expected {
        return not_found();
    }
    let (realtime, commands) = outbox::backlog(&state.db).await.unwrap_or((-1, -1));
    Json(json!({
        "counters": M.snapshot(),
        "outboxBacklog": { "realtime": realtime, "providerCommands": commands },
    }))
    .into_response()
}
