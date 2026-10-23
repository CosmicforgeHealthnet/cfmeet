//! HTTP surface (contracts `cfmeet.numbering.internal-api`).
//!
//! * `GET /health` — liveness; `GET /ready` — database reachable, migrations applied,
//!   outbox / job backlog (counts only).
//! * `/v1/numbers/**` — customer reads of the caller's own assignments and reservations,
//!   authenticated by Platform Access and scope-checked (`numbers.read`). Full public
//!   search / provisioning APIs are later workstreams (H–I). This family alone is public
//!   through the API Gateway (Part VI §51): a gateway-forwarded request carries the
//!   gateway's service token (`numbering-request.forward`) instead of the credential, and
//!   Platform Access re-verifies the delegated credential live
//!   ([`middleware::require_customer`]).
//! * `/v1/admin/**` — Numbering administration (`numbers.admin`, first-party, an audited
//!   operator): a presented credential only, never a gateway-forwarded request.
//! * `/internal/v1/**` — service-authenticated (`number-resource.read`): the canonical,
//!   provider-independent view of a Number Resource.
//!
//! Every non-probe route runs behind its authentication BY CONSTRUCTION, including
//! unknown paths; extractor rejections are normalized to the error envelope.

pub mod acquisitions;
pub mod assignments;
pub mod callbacks;
pub mod catalog;
pub mod customer;
pub mod error;
pub mod grants;
pub mod internal;
pub mod kyc;
pub mod middleware;
pub mod numbers;
pub mod stewardship;
pub mod stewardship_internal;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header::CONTENT_TYPE, StatusCode},
    middleware::{from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use numbering_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;

use self::{
    error::{error_response, ApiError, ErrorCode},
    middleware::{request_context, require_customer, require_internal_service},
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
            customer::routes()
                .fallback(not_found)
                .method_not_allowed_fallback(method_not_allowed),
        )
        .layer(map_response(normalize_rejections))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), require_customer))
        .with_state(state.clone());
    // Provider callbacks: authenticated by the provider adapter's signature, never by a
    // customer credential or service token.
    let callbacks = Router::new()
        .nest(
            "/provider-callbacks/v1",
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
            internal::routes()
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
/// `cfmeet.numbering.internal-api` (contract-first; a test proves every route it lists is
/// registered and vice versa). Public, like the probes: it describes, never authorizes.
pub const OPENAPI: &str =
    include_str!("../../../../packages/contracts/numbering/internal/v1.openapi.json");

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
    Json(json!({ "status": "ok", "service": "numbering" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let pending = numbering_migration::Migrator::get_pending_migrations(&state.db).await;
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
    let outbox = if ok {
        backlog("SELECT count(*) FROM numbering_outbox_events WHERE status = 'PENDING'").await
    } else {
        None
    };
    let dead = if ok {
        backlog("SELECT count(*) FROM numbering_outbox_events WHERE status = 'DEAD_LETTER'").await
    } else {
        None
    };
    let jobs_unknown = if ok {
        backlog("SELECT count(*) FROM numbering_jobs WHERE status = 'UNKNOWN_OUTCOME'").await
    } else {
        None
    };
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "status": if ok { "ready" } else { "not_ready" },
            "service": "numbering",
            "database": database,
            "migrations": migrations,
            "outboxPending": outbox,
            "outboxDeadLetter": dead,
            "jobsUnknownOutcome": jobs_unknown,
        })),
    )
}

/// Extractor rejections (malformed path ids / bodies) leave axum as plain text that may
/// echo the input; every such response becomes the error envelope.
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

async fn not_found() -> ApiError {
    ApiError::Code(ErrorCode::NotFound)
}

async fn method_not_allowed() -> ApiError {
    ApiError::Code(ErrorCode::MethodNotAllowed)
}
