//! Part F transport: media controls, collaboration policy/state, meeting chat,
//! reactions, hand raise and the pre-join contract. Thin: identity comes from
//! `ActorContext`, every target id is scoped through the conference in the path.

use axum::{
    extract::{Query, State},
    Json,
};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        collaboration::{
            breakouts::MediaContextView,
            media::{LockKind, MediaOutcome},
            messaging::MessageScope,
            policy::CollaborationPolicyPatch,
        },
    },
    state::AppState,
};

fn lock_response(o: MediaOutcome) -> MediaLockResponse {
    MediaLockResponse {
        participant_id: o.participant.id,
        audio_locked: o.participant.audio_locked_at.is_some(),
        video_locked: o.participant.video_locked_at.is_some(),
        screen_share_locked: o.participant.screen_share_locked_at.is_some(),
        changed: o.changed,
        permissions: o.permissions.into(),
        provider_enforcement: o.enforcement.into(),
    }
}

macro_rules! lock_handler {
    ($name:ident, $del:ident, $path:literal, $kind:expr, $label:literal, $op:literal, $opdel:literal) => {
        #[doc = concat!("Lock the participant's ", $label, " (persistent, provider-enforced).")]
        #[utoipa::path(
            post, path = $path, tag = "Media controls (v2)", operation_id = $op,
            params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
            responses(
                (status = 200, description = "Locked (idempotent)", body = MediaLockResponse),
                (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
                (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
                (status = 404, description = "PARTICIPANT_NOT_FOUND / CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
                (status = 409, description = "CONFERENCE_STATE_CONFLICT / PARTICIPANT_STATE_CONFLICT", body = ErrorEnvelope),
            ),
            security(("bearer_auth" = []), ("participant_token" = []))
        )]
        pub async fn $name(
            State(state): State<AppState>,
            actor: ActorContext,
            V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
        ) -> ApiResult<Json<MediaLockResponse>> {
            Ok(Json(lock_response(state.meeting.media.set_lock(&actor, id, pid, $kind, true).await?)))
        }

        #[doc = concat!("Unlock the participant's ", $label, " (grants recomputed from role + policy).")]
        #[utoipa::path(
            delete, path = $path, tag = "Media controls (v2)", operation_id = $opdel,
            params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
            responses(
                (status = 200, description = "Unlocked (idempotent)", body = MediaLockResponse),
                (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
                (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
            ),
            security(("bearer_auth" = []), ("participant_token" = []))
        )]
        pub async fn $del(
            State(state): State<AppState>,
            actor: ActorContext,
            V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
        ) -> ApiResult<Json<MediaLockResponse>> {
            Ok(Json(lock_response(state.meeting.media.set_lock(&actor, id, pid, $kind, false).await?)))
        }
    };
}

lock_handler!(
    audio_lock,
    audio_unlock,
    "/api/v2/conferences/{id}/participants/{participantId}/audio-lock",
    LockKind::Audio,
    "microphone",
    "v2_audio_lock",
    "v2_audio_unlock"
);
lock_handler!(
    video_lock,
    video_unlock,
    "/api/v2/conferences/{id}/participants/{participantId}/video-lock",
    LockKind::Video,
    "camera",
    "v2_video_lock",
    "v2_video_unlock"
);
lock_handler!(
    screen_share_lock,
    screen_share_unlock,
    "/api/v2/conferences/{id}/participants/{participantId}/screen-share-lock",
    LockKind::ScreenShare,
    "future screen sharing",
    "v2_screen_share_lock",
    "v2_screen_share_unlock"
);

/// STOP SHARE NOW: terminate a participant's active screen share (not a lock).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/screen-share/{participantId}/stop", tag = "Media controls (v2)", operation_id = "v2_stop_screen_share",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Sharing participant id")),
    responses(
        (status = 200, description = "Provider confirmed the share ended", body = StopScreenShareResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "SCREEN_SHARE_NOT_ACTIVE / CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
        (status = 503, description = "RTC_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn stop_screen_share(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<StopScreenShareResponse>> {
    let n = state
        .meeting
        .media
        .stop_screen_share(&actor, id, pid)
        .await?;
    Ok(Json(StopScreenShareResponse {
        participant_id: pid,
        stopped_sessions: n,
    }))
}

async fn spotlight_list(
    state: &AppState,
    id: Uuid,
    changed: bool,
) -> ApiResult<Json<SpotlightListResponse>> {
    Ok(Json(SpotlightListResponse {
        changed,
        spotlights: state
            .meeting
            .media
            .list_spotlights(id)
            .await?
            .into_iter()
            .map(|s| SpotlightView {
                participant_id: s.participant_id,
                display_name: s.display_name,
                spotlighted_at: s.spotlighted_at,
            })
            .collect(),
    }))
}

/// Spotlight a participant (shared presentation metadata; personal pins stay client-side).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/spotlights", tag = "Media controls (v2)", operation_id = "v2_spotlight",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = SpotlightRequest,
    responses(
        (status = 200, description = "Current spotlights", body = SpotlightListResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn spotlight(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<SpotlightRequest>,
) -> ApiResult<Json<SpotlightListResponse>> {
    let changed = state
        .meeting
        .media
        .spotlight(&actor, id, req.participant_id, true)
        .await?;
    spotlight_list(&state, id, changed).await
}

/// Remove a spotlight.
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/spotlights/{participantId}", tag = "Media controls (v2)", operation_id = "v2_remove_spotlight",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    responses((status = 200, description = "Current spotlights", body = SpotlightListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn remove_spotlight(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<SpotlightListResponse>> {
    let changed = state
        .meeting
        .media
        .spotlight(&actor, id, pid, false)
        .await?;
    spotlight_list(&state, id, changed).await
}

async fn policy_response(
    state: &AppState,
    actor: &ActorContext,
    id: Uuid,
    version: i32,
    patch: CollaborationPolicyPatch,
) -> ApiResult<Json<CollaborationPolicyResponse>> {
    let (c, policy, enforcement) = state
        .meeting
        .media
        .update_policy(actor, id, version, patch)
        .await?;
    Ok(Json(CollaborationPolicyResponse {
        conference_id: c.id,
        collaboration_version: c.collaboration_version,
        policy,
        provider_enforcement: enforcement.into(),
    }))
}

/// Update the conference collaboration policy (media locks, screen-share policy, chat,
/// reactions, hand raise, Q&A, polls, breakouts, consents).
#[utoipa::path(
    patch, path = "/api/v2/conferences/{id}/collaboration-policy", tag = "Collaboration (v2)", operation_id = "v2_update_collaboration_policy",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = CollaborationPolicyUpdateRequest,
    responses(
        (status = 200, description = "Effective policy", body = CollaborationPolicyResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 409, description = "MEETING_VERSION_CONFLICT / CONFERENCE_STATE_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn update_policy(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<CollaborationPolicyUpdateRequest>,
) -> ApiResult<Json<CollaborationPolicyResponse>> {
    policy_response(&state, &actor, id, req.collaboration_version, req.changes).await
}

/// Update the chat subset of the collaboration policy.
#[utoipa::path(
    patch, path = "/api/v2/conferences/{id}/chat-policy", tag = "Chat (v2)", operation_id = "v2_update_chat_policy",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = ChatPolicyUpdateRequest,
    responses((status = 200, description = "Effective policy", body = CollaborationPolicyResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 409, description = "MEETING_VERSION_CONFLICT", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn update_chat_policy(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<ChatPolicyUpdateRequest>,
) -> ApiResult<Json<CollaborationPolicyResponse>> {
    let patch = CollaborationPolicyPatch {
        chat_enabled: req.chat_enabled,
        chat_send_mode: req.chat_send_mode,
        private_chat_mode: req.private_chat_mode,
        breakout_chat_enabled: req.breakout_chat_enabled,
        main_chat_visible_in_breakouts: req.main_chat_visible_in_breakouts,
        ..Default::default()
    };
    policy_response(&state, &actor, id, req.collaboration_version, patch).await
}

/// Canonical collaboration state (REST recovery source for media restrictions,
/// spotlight, hand raise, active screen shares and the caller's media context).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/collaboration", tag = "Collaboration (v2)", operation_id = "v2_collaboration_state",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "State", body = CollaborationStateResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn collaboration_state(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<CollaborationStateResponse>> {
    let st = state.meeting.media.state(&actor, id).await?;
    Ok(Json(CollaborationStateResponse {
        conference_id: id,
        collaboration_version: st.conference.collaboration_version,
        event_sequence: st.conference.event_seq,
        me: st.me.map(|(p, perms, ctx)| MyCollaborationState {
            participant_id: p.id,
            role: p.role,
            permissions: perms.into(),
            audio_locked: p.audio_locked_at.is_some(),
            video_locked: p.video_locked_at.is_some(),
            screen_share_locked: p.screen_share_locked_at.is_some(),
            chat_restricted: p.chat_restricted_at.is_some(),
            hand_raised_at: p.hand_raised_at,
            media_context: (&ctx).into(),
        }),
        policy: st.policy,
        spotlights: st
            .spotlights
            .into_iter()
            .map(|s| SpotlightView {
                participant_id: s.participant_id,
                display_name: s.display_name,
                spotlighted_at: s.spotlighted_at,
            })
            .collect(),
        raised_hands: st.raised_hands,
        active_screen_shares: st.active_screen_shares,
    }))
}

/// Fresh media credential for the caller's own session in its current context (main
/// or breakout) with current effective grants — used after moves and for refresh.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/media-credential", tag = "Media controls (v2)", operation_id = "v2_media_credential",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = MediaCredentialRequest,
    responses(
        (status = 200, description = "Credential", body = MediaCredentialResponse),
        (status = 404, description = "PARTICIPANT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE / PARTICIPANT_STATE_CONFLICT", body = ErrorEnvelope),
        (status = 503, description = "RTC_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn media_credential(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<MediaCredentialRequest>,
) -> ApiResult<Json<MediaCredentialResponse>> {
    let c = state
        .meeting
        .media
        .issue_media_credential(&actor, id, req.participant_session_id)
        .await?;
    Ok(Json(MediaCredentialResponse {
        rtc: RtcCredentialResponse {
            provider: c.credentials.provider.to_string(),
            url: c.credentials.url,
            credential: c.credentials.credential,
            expires_at: c.credentials.expires_at,
        },
        media_context: MediaContextView::from(&c.context),
        permissions: c.permissions.into(),
    }))
}

// ------------------------------- chat -------------------------------------

/// Meeting chat history (database cursor pagination, newest first).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/messages", tag = "Chat (v2)", operation_id = "v2_list_messages",
    params(
        ("id" = Uuid, Path, description = "Conference id"),
        ("scope" = Option<String>, Query, description = "MAIN (default), BREAKOUT or PRIVATE"),
        ("breakoutId" = Option<Uuid>, Query, description = "BREAKOUT scope room"),
        ("peerId" = Option<Uuid>, Query, description = "PRIVATE scope peer participant"),
        ("before" = Option<String>, Query, description = "Cursor"),
        ("limit" = Option<u64>, Query, description = "Page size (max 100)"),
    ),
    responses(
        (status = 200, description = "Page", body = MessagePage),
        (status = 403, description = "PERMISSION_DENIED / CHAT_DISABLED", body = ErrorEnvelope),
        (status = 404, description = "BREAKOUT_NOT_FOUND / PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_messages(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    query: Result<Query<MessageListQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<MessagePage>> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let scope = match (
        q.scope.as_deref().unwrap_or("MAIN"),
        q.breakout_id,
        q.peer_id,
    ) {
        ("MAIN", None, None) => MessageScope::Main,
        ("BREAKOUT", Some(b), None) => MessageScope::Breakout(b),
        ("PRIVATE", None, Some(p)) => MessageScope::Private(p),
        _ => {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "scope must be MAIN, BREAKOUT (with breakoutId) or PRIVATE (with peerId)",
            ))
        }
    };
    Ok(Json(
        state
            .meeting
            .messaging
            .list_messages(&actor, id, scope, q.before.as_deref(), q.limit)
            .await?,
    ))
}

/// Send a message (sender is always the caller's own participant).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/messages", tag = "Chat (v2)", operation_id = "v2_send_message",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = SendMessageRequest,
    responses(
        (status = 201, description = "Created", body = MessageView),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "CHAT_DISABLED / PRIVATE_CHAT_DISABLED / CHAT_PARTICIPANT_RESTRICTED / PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND / BREAKOUT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE / BREAKOUT_CLOSED", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn send_message(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<SendMessageRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<MessageView>)> {
    let m = state
        .meeting
        .messaging
        .send_message(&actor, id, req)
        .await?;
    Ok((axum::http::StatusCode::CREATED, Json(m)))
}

/// Delete / hide a message (moderators, or the sender).
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/messages/{messageId}", tag = "Chat (v2)", operation_id = "v2_delete_message",
    params(("id" = Uuid, Path, description = "Conference id"), ("messageId" = Uuid, Path, description = "Message id")),
    responses((status = 200, description = "Deleted (idempotent)", body = ChangedResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "MESSAGE_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn delete_message(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, mid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChangedResponse>> {
    Ok(Json(ChangedResponse {
        changed: state
            .meeting
            .messaging
            .delete_message(&actor, id, mid)
            .await?,
    }))
}

/// Disable chat for a participant.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/chat-restriction", tag = "Chat (v2)", operation_id = "v2_restrict_chat",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    responses((status = 200, description = "Restricted (idempotent)", body = ChangedResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn restrict_chat(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChangedResponse>> {
    Ok(Json(ChangedResponse {
        changed: state
            .meeting
            .messaging
            .moderate_participant_chat(&actor, id, pid, true)
            .await?,
    }))
}

/// Re-enable chat for a participant.
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/participants/{participantId}/chat-restriction", tag = "Chat (v2)", operation_id = "v2_unrestrict_chat",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    responses((status = 200, description = "Unrestricted (idempotent)", body = ChangedResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn unrestrict_chat(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChangedResponse>> {
    Ok(Json(ChangedResponse {
        changed: state
            .meeting
            .messaging
            .moderate_participant_chat(&actor, id, pid, false)
            .await?,
    }))
}

// --------------------------- reactions / hand -----------------------------

/// Send a transient reaction (no stored row).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/reactions", tag = "Engagement (v2)", operation_id = "v2_send_reaction",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = ReactionRequest,
    responses(
        (status = 200, description = "Accepted", body = ReactionResponse),
        (status = 403, description = "REACTIONS_DISABLED / PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn send_reaction(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<ReactionRequest>,
) -> ApiResult<Json<ReactionResponse>> {
    let delivered = state
        .meeting
        .engagement
        .react(&actor, id, &req.reaction)
        .await?;
    Ok(Json(ReactionResponse {
        reaction: req.reaction,
        delivered,
    }))
}

fn hand(p: crate::models::conference_participants::Model, changed: bool) -> HandRaiseResponse {
    HandRaiseResponse {
        participant_id: p.id,
        raised: p.hand_raised_at.is_some(),
        raised_at: p.hand_raised_at,
        changed,
    }
}

/// Raise the caller's own hand.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/hand-raise", tag = "Engagement (v2)", operation_id = "v2_raise_hand",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Raised (idempotent)", body = HandRaiseResponse), (status = 403, description = "HAND_RAISE_DISABLED", body = ErrorEnvelope), (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn raise_hand(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<HandRaiseResponse>> {
    let (p, changed) = state
        .meeting
        .engagement
        .set_own_hand(&actor, id, true)
        .await?;
    Ok(Json(hand(p, changed)))
}

/// Lower the caller's own hand.
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/hand-raise", tag = "Engagement (v2)", operation_id = "v2_lower_hand",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Lowered (idempotent)", body = HandRaiseResponse)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn lower_hand(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<HandRaiseResponse>> {
    let (p, changed) = state
        .meeting
        .engagement
        .set_own_hand(&actor, id, false)
        .await?;
    Ok(Json(hand(p, changed)))
}

/// Host lowers another participant's hand.
#[utoipa::path(
    delete, path = "/api/v2/conferences/{id}/participants/{participantId}/hand-raise", tag = "Engagement (v2)", operation_id = "v2_lower_other_hand",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    responses((status = 200, description = "Lowered (idempotent)", body = ChangedResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn lower_other_hand(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChangedResponse>> {
    Ok(Json(ChangedResponse {
        changed: state
            .meeting
            .engagement
            .lower_other(&actor, id, pid)
            .await?,
    }))
}

/// Raised hands, oldest first.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/hand-raise", tag = "Engagement (v2)", operation_id = "v2_raised_hands",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Queue", body = RaisedHandsResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn raised_hands(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<RaisedHandsResponse>> {
    crate::services::collaboration::Collab::load(&state.meeting.core, &actor, id).await?;
    Ok(Json(RaisedHandsResponse {
        hands: state.meeting.engagement.raised_hands(id).await?,
    }))
}

// ------------------------------- pre-join ---------------------------------

/// Pre-join contract: everything a pre-join screen needs before the media join.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/pre-join", tag = "Collaboration (v2)", operation_id = "v2_pre_join",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Pre-join view", body = PreJoinResponse), (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope)),
    security((), ("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn pre_join(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<PreJoinResponse>> {
    Ok(Json(state.meeting.prejoin.resolve(&actor, id).await?))
}
