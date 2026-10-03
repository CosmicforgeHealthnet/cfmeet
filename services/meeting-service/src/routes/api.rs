//! API Routes for Third-Party Integrations
//!
//! These routes use Api-Key authentication for third-party access.

use crate::{handlers::meetings, middleware::api_key_middleware, state::AppState};
use axum::{middleware, routing::post, Router};

/// Creates API routes for third-party integrations
///
/// All routes require Api-Key authentication.
pub fn api_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/meetings", post(meetings::api_create_meeting))
        .route("/meetings/:id/join", post(meetings::api_join_meeting))
        .route_layer(middleware::from_fn_with_state(state, api_key_middleware))
}
