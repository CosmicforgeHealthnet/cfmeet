//! HTTP routing.
//!
//! * `GET /health` is public (process alive + database reachable; no configuration).
//! * Everything else is nested under `/internal/v1` and runs behind internal service
//!   authentication BY CONSTRUCTION — including unknown internal paths, so internal routes
//!   cannot be enumerated without a valid service token. There is no customer route: the
//!   public CF Meet Privacy Relay API is served by the authenticated platform ingress.
//! * Every error, including unknown paths (404) and wrong methods (405), uses the
//!   platform error envelope.

use axum::{
    extract::State,
    http::StatusCode,
    middleware::{from_fn, from_fn_with_state},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use sea_orm::ConnectionTrait;
use serde_json::json;

use crate::{
    error::{ErrorCode, RelayError},
    middleware::{request_id_middleware, require_internal_service},
    state::AppState,
};

pub const INTERNAL_PREFIX: &str = "/internal/v1";

/// Builds the service router.
pub fn app(state: AppState, internal: Router<AppState>) -> Router {
    let public = Router::new()
        .route("/health", get(health))
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

/// Liveness + database reachability.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.relay.db().execute_unprepared("SELECT 1").await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ok", "service": "privacy-relay" })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "service": "privacy-relay" })),
        ),
    }
}

async fn not_found() -> RelayError {
    RelayError::Refused(ErrorCode::NotFound)
}

async fn method_not_allowed() -> RelayError {
    RelayError::Refused(ErrorCode::MethodNotAllowed)
}
