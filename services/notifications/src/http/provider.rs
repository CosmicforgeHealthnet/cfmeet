//! `POST /api/v1/notifications/provider-events/resend` (Document 4 §3.2): Svix-verified
//! Resend events. 404 until a webhook secret is configured; 256 KiB cap; dedupe by
//! `(provider, svix-id)`; monotonic status; events for messages this ledger never sent are
//! recorded and otherwise ignored.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use secrecy::ExposeSecret;
use serde_json::{json, Value};

use crate::{
    email::{apply_provider_event, verify_svix, WebhookError},
    error::{ApiError, ErrorCode},
    state::AppState,
};

pub const MAX_CALLBACK_BYTES: usize = 256 * 1024;

pub async fn resend(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(secret) = state.email.resend_webhook_secret.as_ref() else {
        return ApiError::not_found().into_response();
    };
    if body.len() > MAX_CALLBACK_BYTES {
        return ApiError::new(ErrorCode::PayloadTooLarge).into_response();
    }
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("");
    let id = h("svix-id");
    if verify_svix(
        secret.expose_secret(),
        id,
        h("svix-timestamp"),
        h("svix-signature"),
        &body,
        Utc::now(),
    )
    .is_err()
    {
        state.metrics.inc("notif_email_webhook_rejected_total", "");
        return ApiError::new(ErrorCode::AuthenticationRequired).into_response();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return ApiError::validation("The webhook body must be JSON.").into_response();
    };
    match apply_provider_event(&state.db, &state.metrics, "resend", id, &payload).await {
        Ok(applied) => (
            StatusCode::OK,
            Json(json!({ "received": true, "duplicate": !applied })),
        )
            .into_response(),
        Err(WebhookError::Malformed) => {
            ApiError::validation("Unrecognized provider event.").into_response()
        }
        Err(WebhookError::Unauthorized) => {
            ApiError::new(ErrorCode::AuthenticationRequired).into_response()
        }
    }
}
