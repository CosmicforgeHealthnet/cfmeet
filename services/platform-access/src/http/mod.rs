//! HTTP surface (contract `cfmeet.platform-access.internal-api`).
//!
//! * `GET /health` — liveness: the process serves requests (no dependency checks).
//! * `GET /ready` — readiness: database reachable and every migration applied.
//! * Everything else is nested under `/internal/v1` and runs behind internal service
//!   authentication BY CONSTRUCTION, including unknown paths.

pub mod admin;
pub mod error;
pub mod internal;
pub mod middleware;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::header::CONTENT_TYPE,
    http::StatusCode,
    middleware::{from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use platform_access_migration::MigratorTrait;
use serde_json::json;

use self::{
    error::{error_response, AccessError, ErrorCode},
    middleware::{request_id_middleware, require_internal_service},
};
use crate::state::AppState;

pub const INTERNAL_PREFIX: &str = "/internal/v1";

/// Request bodies above this are refused before parsing (identity bodies are small).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

pub fn app(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(from_fn(request_id_middleware));
    let internal = Router::new()
        .nest(
            INTERNAL_PREFIX,
            internal::routes()
                .merge(admin::routes())
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_internal_service))
        .with_state(state);
    public.merge(internal)
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "platform-access" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.platform.db();
    let pending = platform_access_migration::Migrator::get_pending_migrations(db).await;
    let (status, database, migrations) = match pending {
        Ok(p) if p.is_empty() => (StatusCode::OK, "ok", "current"),
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, "ok", "pending"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable", "unknown"),
    };
    (
        status,
        Json(json!({
            "status": if status == StatusCode::OK { "ready" } else { "not_ready" },
            "service": "platform-access",
            "database": database,
            "migrations": migrations,
            "verifierKeyVersion": state.platform.keyring().current_version(),
        })),
    )
}

/// Extractor rejections (malformed path ids, JSON bodies, oversized bodies) leave axum as
/// plain text that may echo the input; every such response becomes the error envelope.
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
    error_response(code, code.default_message())
}

async fn not_found() -> AccessError {
    AccessError::Refused(ErrorCode::NotFound)
}

async fn method_not_allowed() -> AccessError {
    AccessError::Refused(ErrorCode::MethodNotAllowed)
}
