//! HTTP surface (contract `cfmeet.sms-gateway.http-api`, served at `/openapi.json`).
//!
//! * `GET /health` — liveness; `GET /ready` — database reachable, migrations applied,
//!   provider configured, outbox / attempt backlog (counts only).
//! * `/v1/sms/**` — the customer API, authenticated by Platform Access and scope-checked.
//! * `/provider-callbacks/v1/sms/**` — provider callbacks, authenticated by the adapter.
//! * `/internal/v1/**` — service-authenticated (every request is authenticated by
//!   construction, unknown paths included): the Part V H SMS Bridge ([`bridge`]).
//!
//! Extractor rejections are normalized to the error envelope (they never echo input).

pub mod bridge;
pub mod callbacks;
pub mod customer;
pub mod error;
pub mod middleware;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header::CONTENT_TYPE, StatusCode},
    middleware::{from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use sms_gateway_migration::MigratorTrait;

use self::{
    error::{error_response, ApiError, ErrorCode},
    middleware::{request_context, require_customer, require_internal_service},
};
use crate::state::AppState;

/// Request bodies above this are refused before parsing.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

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
            "/v1/sms",
            customer::routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_customer))
        .with_state(state.clone());
    let callbacks = Router::new()
        .nest(
            "/provider-callbacks/v1/sms",
            callbacks::routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn(request_context))
        .with_state(state.clone());
    let internal = Router::new()
        .nest(
            "/internal/v1",
            bridge::routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_internal_service))
        .with_state(state);
    public.merge(customer).merge(callbacks).merge(internal)
}

/// The served API description: exactly the checked-in contract
/// `cfmeet.sms-gateway.http-api` (a test proves every route it lists is registered).
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/sms-gateway/http/v1.openapi.json");

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
    Json(json!({ "status": "ok", "service": "sms-gateway" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let pending = sms_gateway_migration::Migrator::get_pending_migrations(&state.db).await;
    let (ok, database, migrations) = match pending {
        Ok(p) if p.is_empty() => (true, "ok", "current"),
        Ok(_) => (false, "ok", "pending"),
        Err(_) => (false, "unavailable", "unknown"),
    };
    let backlog = |sql: &'static str| {
        let db = state.db.clone();
        async move {
            db.query_one(Statement::from_string(DbBackend::Postgres, sql))
                .await
                .ok()
                .flatten()
                .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        }
    };
    let (outbox, dead, unknown) = if ok {
        (
            backlog("SELECT count(*) FROM sms_outbox_events WHERE status = 'PENDING'").await,
            backlog("SELECT count(*) FROM sms_outbox_events WHERE status = 'DEAD_LETTER'").await,
            backlog("SELECT count(*) FROM sms_transports WHERE status = 'UNKNOWN'").await,
        )
    } else {
        (None, None, None)
    };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "sms-gateway",
            "database": database,
            "migrations": migrations,
            "provider": if state.provider.is_some() { "configured" } else { "none" },
            "outboxPending": outbox,
            "outboxDeadLetter": dead,
            "transportsUnknown": unknown,
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
    error_response(code, code.default_message(), None, None)
}

/// No route matched (distinct from a resource that does not exist, same code).
pub const NO_ROUTE_MESSAGE: &str = "No such route.";

async fn not_found() -> Response {
    error_response(ErrorCode::NotFound, NO_ROUTE_MESSAGE, None, None)
}

async fn method_not_allowed() -> ApiError {
    ApiError::Code(ErrorCode::MethodNotAllowed)
}
