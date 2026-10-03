use crate::{handlers::webhooks, middleware::auth_middleware, state::AppState};
use axum::{middleware, routing::get, Router};

/// Creates webhook routes
///
/// All routes require authentication.
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
}
