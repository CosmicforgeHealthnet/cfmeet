//! `/api/v2` — Phase 2 meeting domain routes (gated by `meeting_v2_enabled`).
//!
//! Authentication is resolved per handler by the `ActorContext` extractor (registered
//! users via bearer token, guests via server-issued participant credential).

use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Router,
};

use crate::{
    error::{ApiError, ErrorCode},
    handlers::v2::{
        artifacts, breakouts, collaboration as collab, compat, conferences, engagement,
        invitations, members, organizations, participants, quality, realtime, spaces, specialized,
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
    let quality_body_limit = state.meeting.core.config.quality.max_body_bytes;
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
        .route(
            "/conferences/:id/participants/:participant_id/mute",
            post(participants::mute),
        )
        // Realtime (Part E)
        .route(
            "/conferences/:id/realtime/ticket",
            post(realtime::create_ticket),
        )
        .route("/conferences/:id/realtime", get(realtime::subscribe))
        .route("/conferences/:id/events", get(realtime::replay_events))
        // Part F — media controls
        .route(
            "/conferences/:id/participants/:participant_id/audio-lock",
            post(collab::audio_lock).delete(collab::audio_unlock),
        )
        .route(
            "/conferences/:id/participants/:participant_id/video-lock",
            post(collab::video_lock).delete(collab::video_unlock),
        )
        .route(
            "/conferences/:id/participants/:participant_id/screen-share-lock",
            post(collab::screen_share_lock).delete(collab::screen_share_unlock),
        )
        .route(
            "/conferences/:id/screen-share/:pid/stop",
            post(collab::stop_screen_share),
        )
        .route("/conferences/:id/spotlights", post(collab::spotlight))
        .route(
            "/conferences/:id/spotlights/:pid",
            delete(collab::remove_spotlight),
        )
        .route(
            "/conferences/:id/media-credential",
            post(collab::media_credential),
        )
        .route(
            "/conferences/:id/collaboration",
            get(collab::collaboration_state),
        )
        .route(
            "/conferences/:id/collaboration-policy",
            patch(collab::update_policy),
        )
        .route("/conferences/:id/pre-join", get(collab::pre_join))
        // Part F — chat
        .route(
            "/conferences/:id/messages",
            get(collab::list_messages).post(collab::send_message),
        )
        .route(
            "/conferences/:id/messages/:mid",
            delete(collab::delete_message),
        )
        .route(
            "/conferences/:id/chat-policy",
            patch(collab::update_chat_policy),
        )
        .route(
            "/conferences/:id/participants/:participant_id/chat-restriction",
            post(collab::restrict_chat).delete(collab::unrestrict_chat),
        )
        // Part F — reactions / hand raise
        .route("/conferences/:id/reactions", post(collab::send_reaction))
        .route(
            "/conferences/:id/hand-raise",
            get(collab::raised_hands)
                .post(collab::raise_hand)
                .delete(collab::lower_hand),
        )
        .route(
            "/conferences/:id/participants/:participant_id/hand-raise",
            delete(collab::lower_other_hand),
        )
        // Part F — breakouts
        .route(
            "/conferences/:id/breakouts",
            get(breakouts::list).post(breakouts::create),
        )
        .route("/conferences/:id/breakouts/assign", post(breakouts::assign))
        .route(
            "/conferences/:id/breakouts/move",
            post(breakouts::move_participant),
        )
        .route("/conferences/:id/breakouts/leave", post(breakouts::leave))
        .route(
            "/conferences/:id/breakouts/announcement",
            post(breakouts::announce_all),
        )
        .route(
            "/conferences/:id/breakouts/close-all",
            post(breakouts::close_all),
        )
        .route(
            "/conferences/:id/breakouts/:bid/assignments",
            patch(breakouts::patch_assignments),
        )
        .route(
            "/conferences/:id/breakouts/:bid/open",
            post(breakouts::open),
        )
        .route(
            "/conferences/:id/breakouts/:bid/enter",
            post(breakouts::enter),
        )
        .route(
            "/conferences/:id/breakouts/:bid/announcement",
            post(breakouts::announce),
        )
        .route(
            "/conferences/:id/breakouts/:bid/close",
            post(breakouts::close),
        )
        // Part F — polls
        .route(
            "/conferences/:id/polls",
            get(engagement::list_polls).post(engagement::create_poll),
        )
        .route("/conferences/:id/polls/:pid", get(engagement::get_poll))
        .route(
            "/conferences/:id/polls/:pid/open",
            post(engagement::open_poll),
        )
        .route(
            "/conferences/:id/polls/:pid/close",
            post(engagement::close_poll),
        )
        .route(
            "/conferences/:id/polls/:pid/archive",
            post(engagement::archive_poll),
        )
        .route(
            "/conferences/:id/polls/:pid/responses",
            post(engagement::respond),
        )
        .route(
            "/conferences/:id/polls/:pid/results",
            get(engagement::results),
        )
        // Part F — Q&A
        .route(
            "/conferences/:id/questions",
            get(engagement::list_questions).post(engagement::submit_question),
        )
        .route(
            "/conferences/:id/questions/:qid/vote",
            post(engagement::vote).delete(engagement::unvote),
        )
        .route(
            "/conferences/:id/questions/:qid/answer",
            post(engagement::answer),
        )
        .route(
            "/conferences/:id/questions/:qid/dismiss",
            post(engagement::dismiss),
        )
        .route(
            "/conferences/:id/questions/:qid/archive",
            post(engagement::archive),
        )
        .route(
            "/conferences/:id/questions/:qid/pin",
            post(engagement::pin).delete(engagement::unpin),
        )
        // Part G — recording and artefacts
        .route(
            "/conferences/:id/recordings",
            get(artifacts::list_recordings).post(artifacts::start_recording),
        )
        .route(
            "/conferences/:id/recordings/:rid",
            get(artifacts::get_recording),
        )
        .route(
            "/conferences/:id/recordings/:rid/stop",
            post(artifacts::stop_recording),
        )
        .route("/conferences/:id/artifacts", get(artifacts::list_artifacts))
        .route(
            "/conferences/:id/artifacts/reports",
            post(artifacts::generate_report),
        )
        .route(
            "/conferences/:id/artifacts/:aid",
            get(artifacts::get_artifact).delete(artifacts::delete_artifact),
        )
        .route(
            "/conferences/:id/artifacts/:aid/access",
            post(artifacts::artifact_access),
        )
        // Part G — consent
        .route(
            "/conferences/:id/consents/requirements",
            get(artifacts::consent_requirements),
        )
        .route(
            "/conferences/:id/consents",
            post(artifacts::record_consents),
        )
        .route(
            "/conferences/:id/consents/:consent_type/withdraw",
            post(artifacts::withdraw_consent),
        )
        // Part G — transcription and intelligence
        .route(
            "/conferences/:id/transcription",
            post(artifacts::start_transcription),
        )
        .route(
            "/conferences/:id/transcription/:jid/stop",
            post(artifacts::stop_transcription),
        )
        .route(
            "/conferences/:id/transcripts",
            get(artifacts::list_transcripts),
        )
        .route(
            "/conferences/:id/transcripts/:aid",
            get(artifacts::get_transcript),
        )
        .route(
            "/conferences/:id/transcripts/:aid/translations",
            post(artifacts::translate_transcript),
        )
        .route(
            "/conferences/:id/intelligence/requests",
            post(artifacts::request_intelligence),
        )
        // Part H — profiles, registration, backstage
        .route(
            "/meeting-spaces/:id/profile-policy",
            get(specialized::get_profile_policy).patch(specialized::update_profile_policy),
        )
        .route(
            "/meeting-spaces/:id/registrations",
            get(specialized::list_registrations).post(specialized::register),
        )
        .route(
            "/meeting-spaces/:id/registrations/:rid",
            get(specialized::get_registration).delete(specialized::cancel_registration),
        )
        .route(
            "/conferences/:id/backstage",
            post(specialized::enter_backstage).delete(specialized::leave_backstage),
        )
        // Part I — quality
        .route(
            "/conferences/:id/quality/samples",
            post(quality::submit_samples)
                .layer(axum::extract::DefaultBodyLimit::max(quality_body_limit)),
        )
        .route("/conferences/:id/quality", get(quality::conference_quality))
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
