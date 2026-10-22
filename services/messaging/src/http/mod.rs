//! HTTP surface (contract `cfmeet.messaging.http-api`, served at `/openapi.json`).
//!
//! * `GET /health` — liveness only; `GET /ready` — database reachable and every
//!   migration applied (503 otherwise), plus non-secret operational facts.
//! * `/v1/messaging/**` and `/v1/conversations/**` — the customer API: Platform Access authentication, scopes, a
//!   verified acting principal ([`middleware`]).
//! * `/internal/v1/**` — service-authenticated; no internal operation exists yet, so
//!   every request is authenticated by construction (unknown paths included) and 404s.
//! * Metrics are served on a separate listener ([`crate::metrics`]); there is no
//!   administrative HTTP route.
//!
//! Extractor rejections are normalized to the error envelope (they never echo input).

pub mod devices;
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
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;

use self::{
    error::{error_response, ApiError, ErrorCode},
    middleware::{request_context, require_customer, require_internal_service},
};
use crate::state::AppState;

/// Request bodies above this are refused before parsing (20 KeyPackages of ≤ 16 KiB,
/// base64url-encoded, fit).
pub const MAX_BODY_BYTES: usize = 512 * 1024;

/// Customer routes nested under `/v1/messaging`.
pub fn customer_routes() -> Router<AppState> {
    devices::routes()
        .merge(crate::conversations::http::policy_routes())
        .merge(crate::mls::http::device_routes())
        .merge(crate::safety::http::routes())
        .merge(crate::realtime::http::customer_routes())
        .merge(crate::compliance::http::routes())
        .merge(crate::moderation::http::customer_routes())
        .merge(crate::search::http::customer_routes())
        .merge(crate::sms_bridge::http::messaging_routes())
        .merge(crate::usage::http::routes())
        .merge(crate::backups::routes())
}

/// Routes nested under `/v1/conversations`.
pub fn conversation_tree() -> Router<AppState> {
    crate::conversations::http::conversation_routes()
        .merge(crate::membership::http::routes())
        .merge(crate::messages::http::routes())
        .merge(crate::mls::http::conversation_routes())
        .merge(crate::attachments::http::routes())
        .merge(crate::moderation::http::conversation_routes())
        .merge(crate::search::http::conversation_routes())
        .merge(crate::sms_bridge::http::conversation_routes())
        .merge(crate::engagement::http::routes())
}

pub fn app(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(openapi))
        .merge(crate::realtime::http::device_routes())
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(from_fn(request_context));
    let customer = Router::new()
        .nest(
            "/v1/messaging",
            customer_routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .nest(
            "/v1/conversations",
            conversation_tree()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_customer))
        .with_state(state.clone());
    // `/internal/v1/delegated/v1/...` mirrors the customer surface for first-party
    // backends acting through a peer service (`messaging-application.delegate`).
    let delegated = Router::new()
        .nest("/v1/messaging", customer_routes())
        .nest("/v1/conversations", conversation_tree())
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(
            state.clone(),
            middleware::require_delegated_application,
        ));
    let internal = Router::new()
        .nest(
            "/internal/v1",
            Router::new()
                .nest("/delegated", delegated)
                .merge(crate::integrations::http::internal_routes())
                .merge(crate::sms_bridge::http::internal_routes())
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(from_fn_with_state(state.clone(), require_internal_service))
        .with_state(state);
    public
        .merge(customer)
        .merge(internal)
        .layer(from_fn(crate::metrics::http_layer))
}

/// The served API description: exactly the checked-in contract
/// `cfmeet.messaging.http-api` (a test proves it lists exactly the registered routes).
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/messaging/http/v1.openapi.json");

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
    Json(json!({ "status": "ok", "service": "messaging" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let pending = messaging_migration::Migrator::get_pending_migrations(&state.db).await;
    let draining = state.realtime.hub.is_draining();
    let (ok, database, migrations) = match pending {
        Ok(p) if p.is_empty() && !draining => (true, "ok", "current"),
        Ok(p) if p.is_empty() => (false, "ok", "current"),
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
    let (outbox, dead) = if ok {
        (
            backlog("SELECT count(*) FROM messaging_outbox_events WHERE status = 'PENDING'").await,
            backlog("SELECT count(*) FROM messaging_outbox_events WHERE status = 'DEAD_LETTER'")
                .await,
        )
    } else {
        (None, None)
    };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "messaging",
            "database": database,
            "migrations": migrations,
            "e2eeDevices": if state.devices.enabled { "enabled" } else { "disabled" },
            "authorizationCache": if state.authority.is_caching() { "active" } else { "inactive" },
            "outboxPending": outbox,
            "outboxDeadLetter": dead,
            "realtime": {
                "draining": state.realtime.hub.is_draining(),
                "connections": state.realtime.hub.connections(),
                "broker": state.realtime.broker.backend(),
                "presence": state.realtime.presence.backend(),
                "clientTokens": if state.realtime.keys.is_some() { "enabled" } else { "disabled" },
            },
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
    error_response(code, code.default_message(), None)
}

/// No route matched (distinct from a resource that does not exist, same code).
pub const NO_ROUTE_MESSAGE: &str = "No such route.";

async fn not_found() -> Response {
    error_response(ErrorCode::NotFound, NO_ROUTE_MESSAGE, None)
}

async fn method_not_allowed() -> ApiError {
    ApiError::code(ErrorCode::MethodNotAllowed)
}
