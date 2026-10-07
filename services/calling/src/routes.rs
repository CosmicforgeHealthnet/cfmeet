//! HTTP routing.
//!
//! Only `GET /health` is public. The Calling business API (Part C) does not exist yet;
//! when it does, its routes are mounted through [`app`] as `protected` routes so that
//! every one of them passes through platform credential authentication.

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
    error::CallingError,
    middleware::{platform_auth::require_platform_credential, request_id::request_id_middleware},
    state::AppState,
};

/// Builds the service router: public routes plus `protected` routes wrapped in platform
/// credential authentication, all inside the request-id layer.
pub fn app(state: AppState, protected: Router<AppState>) -> Router {
    // `route_layer` on a router without routes is rejected by axum; with no protected
    // routes there is nothing to wrap.
    let protected = if protected.has_routes() {
        protected.route_layer(from_fn_with_state(
            state.clone(),
            require_platform_credential,
        ))
    } else {
        protected
    };
    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .fallback(not_found)
        .with_state(state)
        .layer(from_fn(request_id_middleware))
}

/// Liveness + database reachability.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.execute_unprepared("SELECT 1").await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ok", "service": "calling" })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "service": "calling" })),
        ),
    }
}

async fn not_found() -> CallingError {
    CallingError::NotFound
}
