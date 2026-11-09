use crate::{
    handlers::{chat, meetings, participants},
    middleware::auth_middleware,
    state::AppState,
};
use axum::{
    middleware,
    routing::{get, post, MethodRouter},
    Router,
};

/// Layers a legacy create route: the create policy (`FEATURE_LEGACY_MEETING_CREATE_MODE`)
/// inside, deprecation headers outside (so a 410 refusal carries them too).
pub fn legacy_create(route: MethodRouter<AppState>, state: &AppState) -> MethodRouter<AppState> {
    route
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::legacy_write::legacy_create_guard,
        ))
        .layer(middleware::from_fn(
            crate::middleware::deprecation::legacy_meeting_create,
        ))
}

/// Creates meeting routes
///
/// Protected routes use auth middleware, public routes (join) do not.
#[allow(deprecated)]
pub fn meeting_routes(state: AppState) -> Router<AppState> {
    Router::new()
        // Protected routes (require authentication). The two creates carry the Section 16
        // create policy (refuse / mirror) and deprecation headers (MTG-017 / DATA-008).
        .route(
            "/instant",
            legacy_create(post(meetings::create_instant_meeting), &state),
        )
        .route(
            "/",
            legacy_create(post(meetings::create_meeting), &state).get(meetings::list_meetings),
        )
        .route(
            "/:id",
            get(meetings::get_meeting)
                .put(meetings::update_meeting)
                .delete(meetings::delete_meeting),
        )
        .route("/:id/end", post(meetings::end_meeting))
        // Participant management (meeting-scoped)
        .route("/:id/participants", get(participants::list_participants))
        .route("/:id/waiting", get(participants::list_waiting_participants))
        .route(
            "/:id/waiting/:participant_id/admit",
            post(participants::admit_participant),
        )
        .route(
            "/:id/waiting/:participant_id/deny",
            post(participants::deny_participant),
        )
        .route(
            "/:id/screen-share/start",
            post(participants::start_screen_share),
        )
        .route(
            "/:id/screen-share/stop",
            post(participants::stop_screen_share),
        )
        // Chat routes
        .route(
            "/:id/chat",
            get(chat::get_messages).post(chat::send_message),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // Public routes (no auth required)
        .route("/:id/join", post(meetings::join_meeting))
        .route(
            "/join/:meeting_identifier",
            post(meetings::join_meeting_by_identifier),
        )
        .route(
            "/public/:meeting_identifier",
            get(meetings::get_public_meeting_info),
        )
}
