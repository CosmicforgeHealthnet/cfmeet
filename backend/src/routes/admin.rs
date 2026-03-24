use crate::{handlers::admin, middleware::auth_middleware, state::AppState};
use axum::{middleware, routing::get, Router};

/// Creates admin routes
///
/// All routes require authentication and admin role.
/// Role check is performed in each handler.
pub fn admin_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/users", get(admin::list_users))
        .route("/users/:id", axum::routing::patch(admin::update_user))
        .route("/meetings", get(admin::list_meetings))
        .route("/logs", get(admin::list_logs))
        .route_layer(middleware::from_fn_with_state(state, auth_middleware))
}
