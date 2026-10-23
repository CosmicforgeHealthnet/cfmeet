//! HTTP surface (contract `cfmeet.developer-platform.internal-api`, served at
//! `/openapi.json`).
//!
//! * `GET /health` — liveness; `GET /ready` — database reachable and migrations applied.
//! * `/internal/v1/**` — service-authenticated (every request is authenticated by
//!   construction, unknown paths included): [`internal`].
//!
//! There is no customer-facing route: customers reach the Developer Platform only through
//! the API Gateway. Extractor rejections are normalized to the error envelope.

pub mod error;
pub mod internal;
pub mod middleware;
pub mod operator;
pub mod public;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header::CONTENT_TYPE, StatusCode},
    middleware::{from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use developer_platform_migration::MigratorTrait;
use serde_json::json;

use self::{
    error::{error_response, ApiError, ErrorCode},
    middleware::{request_context, require_internal_service},
};
use crate::state::AppState;

/// Request bodies above this are refused before parsing.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

pub fn app(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(openapi))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(from_fn(request_context));
    let customer = Router::new()
        .nest(
            "/v1",
            public::routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(
            state.clone(),
            public::require_gateway_caller,
        ))
        .with_state(state.clone());
    let internal = Router::new()
        .nest(
            "/internal/v1",
            internal::routes()
                .merge(operator::routes())
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_internal_service))
        .with_state(state);
    public.merge(customer).merge(internal)
}

/// The served API description: exactly the checked-in contract
/// `cfmeet.developer-platform.internal-api`.
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/platform-developer/internal/v1.openapi.json");

async fn openapi() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "application/json"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=300"),
        ],
        OPENAPI,
    )
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "developer-platform" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let pending = developer_platform_migration::Migrator::get_pending_migrations(&state.db).await;
    let (ok, database, migrations) = match pending {
        Ok(p) if p.is_empty() => (true, "ok", "current"),
        Ok(_) => (false, "ok", "pending"),
        Err(_) => (false, "unavailable", "unknown"),
    };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "developer-platform",
            "database": database,
            "migrations": migrations,
            "gatewayApi": if state.flags.gateway_api { "enabled" } else { "disabled" },
        })),
    )
}

async fn normalize_rejections(response: Response) -> Response {
    let status = response.status();
    let json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if json || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let code = match status {
        StatusCode::PAYLOAD_TOO_LARGE => ErrorCode::PayloadTooLarge,
        StatusCode::NOT_FOUND => ErrorCode::NotFound,
        StatusCode::METHOD_NOT_ALLOWED => ErrorCode::MethodNotAllowed,
        s if s.is_client_error() => ErrorCode::ValidationError,
        _ => ErrorCode::InternalError,
    };
    error_response(code)
}

async fn not_found() -> Response {
    error_response(ErrorCode::NotFound)
}

async fn method_not_allowed() -> ApiError {
    ApiError(ErrorCode::MethodNotAllowed)
}
