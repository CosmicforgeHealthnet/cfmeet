use crate::{handlers::participants, middleware::auth_middleware, state::AppState};
use axum::{
    middleware,
    routing::{patch, post},
    Router,
};

/// Creates participant routes
///
/// These routes operate on specific participants regardless of meeting context.
pub fn participant_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/:participant_id/kick",
            post(participants::kick_participant),
        )
        .route("/:participant_id/audio", patch(participants::update_audio))
        .route("/:participant_id/video", patch(participants::update_video))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
}
