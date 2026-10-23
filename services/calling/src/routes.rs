//! HTTP routing.
//!
//! * `GET /health` is public.
//! * Customer routes ([`public_api`]: the call API, call control, endpoints, signaling
//!   credential minting) are passed to [`app`] as `protected` and every one of them runs
//!   behind platform credential authentication.
//! * Endpoint signaling routes (`/v1/calling/signaling`, `/v1/calling/invitations…`,
//!   `/v1/calling/history`,
//!   `/v1/calling/sessions/…/media-credentials`) authenticate with a short-lived signaling
//!   credential and never accept a platform API credential.
//! * The media provider callback (`POST /internal/provider/media/events`) is
//!   authenticated by the provider's signature only and accepts nothing else.
//! * The media connection gateway (`GET /v1/calling/media/rtc…`) is authenticated by the
//!   edge-sealed media credential and admits every connection against the database
//!   before the media server is contacted.
//! * Public Part III developer documentation is served at `GET /swagger-ui/` with
//!   public-only Calling and Privacy Relay OpenAPI documents under `/api-docs/*`.
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
    middleware::{
        json_body::require_object_body, platform_auth::require_platform_credential,
        request_id::request_id_middleware,
    },
    state::AppState,
};

/// The public customer API (platform credential): call reads, creation and control,
/// endpoint registry, signaling-credential minting and the Privacy Relay ingress.
/// Mounted as `protected` by `main`.
pub fn public_api() -> Router<AppState> {
    crate::calls::http::routes()
        .merge(crate::calls::app::http::routes())
        .merge(crate::endpoints::http::routes())
        .merge(crate::privacy_relay::http::routes())
        .merge(crate::enterprise::http::routes())
        .merge(crate::webhooks::http::routes())
        .merge(crate::usage::http::routes())
}

/// Internal routes (service authentication): Privacy Relay's active-call enforcement.
/// Mounted as `internal` by `main` (nested under `/internal/v1`).
pub fn internal_api() -> Router<AppState> {
    crate::privacy_relay::enforcement::internal_routes()
        .merge(crate::telephony::http::internal_routes())
        .merge(crate::webhooks::ingest::internal_routes())
        .merge(crate::webhooks::ownership_migration::internal_routes())
}

/// Builds the service router.
pub fn app(state: AppState, protected: Router<AppState>, internal: Router<AppState>) -> Router {
    // `route_layer` on a router without routes is rejected by axum; with no protected
    // routes there is nothing to wrap.
    // Authentication first (outer), then the JSON-object body rule (inner): a request
    // that does not authenticate learns nothing about body validation.
    let protected = if protected.has_routes() {
        protected
            .route_layer(from_fn(require_object_body))
            .route_layer(from_fn_with_state(
                state.clone(),
                require_platform_credential,
            ))
    } else {
        protected
    };
    let customer = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .merge(crate::signaling::http::routes().route_layer(from_fn(require_object_body)))
        .merge(crate::calls::app::http::callback_routes())
        .merge(crate::media::gateway::routes())
        .merge(crate::public_docs::routes())
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
