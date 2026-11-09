use crate::{handlers::webhooks, middleware::auth_middleware, state::AppState};
use axum::{middleware, routing::get, Router};

/// Creates the legacy webhook routes — DEPRECATED, never delivered (PLT-003).
///
/// All routes require authentication. The deprecation-header layer is mounted outside
/// authentication, so every answer (401 / 400 / 404 included) carries `Deprecation`,
/// `Sunset` and `Link rel="deprecation"`.
#[allow(deprecated)]
pub fn webhook_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(webhooks::list_webhooks).post(webhooks::create_webhook),
        )
        .route(
            "/:id",
            axum::routing::patch(webhooks::update_webhook).delete(webhooks::delete_webhook),
        )
        .route_layer(middleware::from_fn_with_state(state, auth_middleware))
        .layer(middleware::from_fn(
            crate::middleware::deprecation::legacy_webhooks,
        ))
}
