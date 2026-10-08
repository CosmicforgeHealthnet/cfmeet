//! HTTP routing.
//!
//! * `GET /health` is public and safe: process alive + database reachable, nothing else.
//! * `POST /provider-callbacks/{provider}` is the provider event ingress: it terminates
//!   HERE, inside Telephony — never in Calling's public API. The body is limited BEFORE
//!   parsing and authenticated by the provider adapter (signature / token / peer) before
//!   anything is touched; refusals are uniform.
//! * Everything else is nested under `/internal/v1` and runs behind internal service
//!   authentication BY CONSTRUCTION, including unknown paths.

use std::collections::BTreeMap;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    middleware::{from_fn, from_fn_with_state},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sea_orm::ConnectionTrait;
use serde_json::json;

use crate::{
    error::{ErrorCode, TelephonyError},
    events::ingest::MAX_CALLBACK_BYTES,
    middleware::{request_id_middleware, require_internal_service},
    providers::ProviderIngress,
    state::AppState,
};

pub const INTERNAL_PREFIX: &str = "/internal/v1";

/// Request headers an adapter may use to authenticate a callback (lower-case).
const CALLBACK_HEADERS: &[&str] = &[
    "x-cfmeet-provider-signature",
    "x-provider-signature",
    "x-provider-timestamp",
    "content-type",
];

pub fn app(state: AppState, internal: Router<AppState>) -> Router {
    let callbacks = Router::new()
        .route("/provider-callbacks/:provider_id", post(provider_callback))
        .route(
            "/provider-callbacks/:provider_id/:token",
            post(provider_callback_with_token),
        )
        .layer(DefaultBodyLimit::max(MAX_CALLBACK_BYTES));
    let public = Router::new()
        .route("/health", get(health))
        .merge(callbacks)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(from_fn(request_id_middleware));
    let internal = Router::new()
        .nest(
            INTERNAL_PREFIX,
            internal
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(from_fn_with_state(state.clone(), require_internal_service))
        .with_state(state);
    public.merge(internal)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.telephony.db().execute_unprepared("SELECT 1").await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ok", "service": "telephony" })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "service": "telephony" })),
        ),
    }
}

async fn ingest(
    state: AppState,
    provider_id: String,
    token: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, TelephonyError> {
    let ok_id = provider_id.len() <= 32
        && provider_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok_id {
        return Err(TelephonyError::Refused(ErrorCode::ProviderEventRejected));
    }
    let mut selected = BTreeMap::new();
    for name in CALLBACK_HEADERS {
        if let Some(v) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            if v.len() <= 512 {
                selected.insert((*name).to_string(), v.to_string());
            }
        }
    }
    let ingress = ProviderIngress {
        provider_id,
        path_token: token,
        headers: selected,
        body: body.to_vec(),
        received_at: state.telephony.now(),
    };
    let dispositions = state.telephony.ingest_callback(ingress).await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "accepted": dispositions.len() })),
    ))
}

async fn provider_callback(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, TelephonyError> {
    ingest(state, provider_id, None, headers, body).await
}

async fn provider_callback_with_token(
    State(state): State<AppState>,
    Path((provider_id, token)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, TelephonyError> {
    ingest(state, provider_id, Some(token), headers, body).await
}

async fn not_found() -> TelephonyError {
    TelephonyError::Refused(ErrorCode::NotFound)
}

async fn method_not_allowed() -> TelephonyError {
    TelephonyError::Refused(ErrorCode::MethodNotAllowed)
}
