use crate::{handlers::users, middleware::auth_middleware, state::AppState};
use axum::{middleware, routing::{get, post}, Router};

/// Creates user routes
///
/// All routes require authentication.
pub fn users_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/me",
            get(users::get_current_user).patch(users::update_current_user),
        )
        .route("/me/deactivate",post(users::deactivate_account))
        .route_layer(middleware::from_fn_with_state(state, auth_middleware))
}
