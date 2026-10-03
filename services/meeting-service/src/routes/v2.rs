//! `/api/v2` — Phase 2 meeting domain routes (gated by `meeting_v2_enabled`).
//!
//! Authentication is resolved per handler by the `ActorContext` extractor (registered
//! users via bearer token, guests via server-issued participant credential).

use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Router,
};

use crate::{
    error::{ApiError, ErrorCode},
    handlers::v2::{
        compat, conferences, invitations, members, organizations, participants, spaces,
    },
    state::AppState,
};

async fn require_meeting_v2(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.meeting.core.flags.meeting_v2_enabled {
        return ApiError::domain(ErrorCode::FeatureDisabled, "Meeting v2 is not enabled")
            .into_response();
    }
    next.run(request).await
}

pub fn v2_routes(state: AppState) -> Router<AppState> {
    Router::new()
        // Meeting spaces
        .route(
            "/meeting-spaces",
            get(spaces::list_meeting_spaces).post(spaces::create_meeting_space),
        )
        .route(
            "/meeting-spaces/:id",
            get(spaces::get_meeting_space).patch(spaces::update_meeting_space),
        )
        .route(
            "/meeting-spaces/:id/archive",
            post(spaces::archive_meeting_space),
        )
        .route(
            "/meeting-spaces/:id/duplicate",
            post(spaces::duplicate_meeting_space),
        )
        .route(
            "/meeting-spaces/:id/owner-transfer",
            post(spaces::transfer_owner),
        )
        .route(
            "/meeting-spaces/:id/recurrence",
            get(spaces::get_recurrence)
                .put(spaces::set_recurrence)
                .delete(spaces::end_recurrence),
        )
        .route(
            "/meeting-spaces/:id/recurrence/exceptions",
            get(spaces::list_recurrence_exceptions).put(spaces::set_recurrence_exception),
        )
        .route("/instant-meetings", post(spaces::create_instant_meeting))
        .route("/resolve/:identifier", get(spaces::resolve_identifier))
        // Members
        .route(
            "/meeting-spaces/:id/members",
            get(members::list_members).post(members::add_member),
        )
        .route(
            "/meeting-spaces/:id/members/:member_id",
            patch(members::update_member).delete(members::revoke_member),
        )
        // Invitations
        .route(
            "/meeting-spaces/:id/invitations",
            get(invitations::list_invitations).post(invitations::create_invitation),
        )
        .route(
            "/meeting-spaces/:id/invitations/:invitation_id/revoke",
            post(invitations::revoke_invitation),
        )
        .route(
            "/invitations/:token/resolve",
            post(invitations::resolve_invitation),
        )
        // Conferences
        .route(
            "/meeting-spaces/:id/conferences",
            get(conferences::list_conferences).post(conferences::create_conference),
        )
        .route("/conferences/:id", get(conferences::get_conference))
        .route(
            "/conferences/:id/prepare",
            post(conferences::prepare_conference),
        )
        .route(
            "/conferences/:id/start",
            post(conferences::start_conference),
        )
        .route("/conferences/:id/lock", post(conferences::lock_conference))
        .route(
            "/conferences/:id/unlock",
            post(conferences::unlock_conference),
        )
        .route("/conferences/:id/end", post(conferences::end_conference))
        .route(
            "/conferences/:id/cancel",
            post(conferences::cancel_conference),
        )
        .route(
            "/conferences/:id/reschedule",
            post(conferences::reschedule_conference),
        )
        .route("/conferences/:id/join", post(conferences::join_conference))
        .route(
            "/conferences/:id/leave",
            post(conferences::leave_conference),
        )
        // Participants / waiting room
        .route(
            "/conferences/:id/participants",
            get(participants::list_participants),
        )
        .route(
            "/conferences/:id/participants/me",
            get(participants::get_me),
        )
        .route(
            "/conferences/:id/waiting-room",
            get(participants::get_waiting_room),
        )
        .route(
            "/conferences/:id/waiting-room/admit",
            post(participants::admit),
        )
        .route(
            "/conferences/:id/waiting-room/deny",
            post(participants::deny),
        )
        .route(
            "/conferences/:id/participants/:participant_id/return-to-waiting",
            post(participants::return_to_waiting),
        )
        .route(
            "/conferences/:id/participants/:participant_id/role",
            patch(participants::change_role),
        )
        .route(
            "/conferences/:id/participants/:participant_id/remove",
            post(participants::remove),
        )
        .route(
            "/conferences/:id/participants/:participant_id/ban",
            post(participants::ban),
        )
        // Compatibility (platform admin)
        .route(
            "/admin/compat/legacy-meetings/:id/shadow",
            get(compat::shadow_compare),
        )
        .route("/admin/compat/backfill", post(compat::run_backfill))
        // Organizations
        .route(
            "/admin/organizations",
            post(organizations::create_organization),
        )
        .route(
            "/admin/organizations/:id",
            patch(organizations::update_organization),
        )
        .route(
            "/organizations/:id/members",
            get(organizations::list_organization_members),
        )
        .route(
            "/organizations/:id/members/:user_id",
            put(organizations::upsert_organization_member),
        )
        .route("/me/organizations", get(organizations::my_organizations))
        .route_layer(middleware::from_fn_with_state(state, require_meeting_v2))
}
