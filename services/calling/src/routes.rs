//! HTTP routing.
//!
//! * `GET /health` is public.
//! * Customer routes ([`public_api`]: the read-only call API) are passed to [`app`] as
//!   `protected` and every one of them runs behind platform credential authentication.
//! * Internal routes are passed as `internal`, are nested under `/internal/v1` and run
//!   behind internal service authentication BY CONSTRUCTION — including unknown internal
//!   paths, so internal routes cannot be enumerated without a valid service token.
//!
//! Every error, including unknown paths (404) and unsupported methods (405), uses the
//! error envelope of the Calling HTTP contract (`packages/contracts/calling/http`).

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
    internal_auth::{middleware::require_internal_service, INTERNAL_PREFIX},
    middleware::{platform_auth::require_platform_credential, request_id::request_id_middleware},
    state::AppState,
};

/// The public customer API: today the read-only call routes (`GET /v1/calls`,
/// `GET /v1/calls/{call_id}`). Mounted as `protected` by `main`.
pub fn public_api() -> Router<AppState> {
    crate::calls::http::routes()
}

/// Builds the service router.
pub fn app(state: AppState, protected: Router<AppState>, internal: Router<AppState>) -> Router {
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
    let customer = Router::new()
        .route("/health", get(health))
        .merge(protected)
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

    customer.merge(internal)
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

async fn method_not_allowed() -> CallingError {
    CallingError::MethodNotAllowed
}
