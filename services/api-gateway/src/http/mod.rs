//! HTTP surface (contract `cfmeet.api-gateway.http-api`).
//!
//! * `GET /health`, `GET /ready` — probes (counts and flags only).
//! * everything else — the customer ingress ([`ingress`]): default deny, then Platform
//!   Access authentication, scope check and forwarding with a signed delegated context.
//!
//! There is no `/internal` surface, no provider callback, no operator route and no
//! `/openapi.json` passthrough to an upstream.

pub mod error;
pub mod ingress;

use axum::{
    extract::State,
    http::HeaderValue,
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;

use crate::{
    context::{scoped, RequestContext},
    state::AppState,
};

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .layer(from_fn(probe_context))
        .fallback(ingress::ingress)
        .with_state(state)
}

async fn probe_context(request: axum::extract::Request, next: Next) -> Response {
    let context = RequestContext::generate();
    let mut response = scoped(context.clone(), next.run(request)).await;
    if let Ok(v) = HeaderValue::from_str(context.request_id.as_str()) {
        response.headers_mut().insert("x-request-id", v);
    }
    response
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ready",
        "service": "api-gateway",
        "publicRouting": if state.flags.public_routing { "enabled" } else { "disabled" },
        "routes": state.routes.len(),
        "websockets": if state.flags.websockets { "enabled" } else { "disabled" },
        "quotas": if state.quotas.configured() { "configured" } else { "not_enforced" },
    }))
}
