use crate::{handlers::api_keys, middleware::auth_middleware, state::AppState};
use axum::{middleware, routing::get, Router};

/// Creates API key routes
///
/// All routes require authentication.
pub fn api_key_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(api_keys::list_api_keys).post(api_keys::create_api_key),
        )
        .route(
            "/:id",
            get(api_keys::get_api_key).delete(api_keys::revoke_api_key),
        )
        .route_layer(middleware::from_fn_with_state(state, auth_middleware))
}
