pub mod admin;
pub mod api;
pub mod api_keys;
pub mod auth;
pub mod meetings;
pub mod participants;
pub mod users;
pub mod v2;
pub mod webhooks;

use crate::{handlers, middleware::legacy_write::legacy_write_guard, state::AppState};
use axum::{routing::get, Router};

fn legacy_writes(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        legacy_write_guard,
    ))
}

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::root::get_app_info))
        .route("/health", get(handlers::root::health_check))
        .nest("/api/v1/auth", auth::auth_routes())
        // Legacy meeting surfaces: writes retire behind FEATURE_LEGACY_MEETING_WRITE_DISABLED.
        .nest(
            "/api/v1/meetings",
            legacy_writes(meetings::meeting_routes(state.clone()), &state),
        )
        .nest(
            "/api/v1/participants",
            legacy_writes(participants::participant_routes(state.clone()), &state),
        )
        .nest(
            "/api/v1/api-keys",
            api_keys::api_key_routes(state.clone()),
        )
        .nest(
            "/api/v1/api",
            legacy_writes(api::api_routes(state.clone()), &state),
        )
        .nest("/api/v1/users", users::users_routes(state.clone()))
        .nest(
            "/api/v1/webhooks",
            webhooks::webhook_routes(state.clone()),
        )
        .nest("/api/v1/admin", admin::admin_routes(state.clone()))
        .nest("/api/v2", v2::v2_routes(state.clone()))
        // Internal provider routes (signature / internal-token protected; Part E).
        .route(
            "/internal/rtc/livekit/webhook",
            axum::routing::post(handlers::internal::livekit_webhook),
        )
        .route(
            "/internal/rtc/metrics",
            get(handlers::internal::rtc_metrics),
        )
        // Parts G–I internal routes.
        .route(
            "/internal/transcription/events",
            axum::routing::post(handlers::internal::transcription_events),
        )
        .route("/internal/metrics", get(handlers::internal::prometheus_metrics))
        .route(
            "/internal/operations/conferences/health",
            get(handlers::internal::operations_conferences),
        )
        .route(
            "/internal/operations/conferences/:id/health",
            get(handlers::internal::operations_conference),
        )
        .route(
            "/internal/operations/incidents",
            get(handlers::internal::operations_incidents),
        )
        .layer(axum::middleware::from_fn(
            crate::middleware::request_id::request_id_middleware,
        ))
        .with_state(state)
}
