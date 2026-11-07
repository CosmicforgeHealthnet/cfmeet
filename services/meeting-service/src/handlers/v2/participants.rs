//! Waiting room, roster, role change and moderation endpoints. Every participant id is
//! scoped to the conference in the path; cross-conference ids are rejected before any
//! state mutation or provider interaction.

use axum::{extract::State, Json};
use uuid::Uuid;

use super::{views, V2Json, V2OptionalJson, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// The caller's own participant state (registered user or participant token).
///
/// Returns the caller's participant record in this conference in whatever admission state it is
/// (e.g. WAITING, ADMITTED, DENIED), identified by the bearer user token or the participant token
/// issued at join. Returns PARTICIPANT_NOT_FOUND if the caller has not joined this conference.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/participants/me", tag = "Participants (v2)", operation_id = "v2_get_me",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Participant", body = ParticipantResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_me(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<ParticipantResponse>> {
    let p = state.meeting.admission.me(&actor, id).await?;
    Ok(Json(views::participant(&p)))
}

/// Admitted participants (roster).
///
/// Returns the participants currently ADMITTED to the conference, ordered by admission time.
/// Available to admitted participants and to the space owner and members; other callers (including
/// waiting, denied, removed or banned participants) get PERMISSION_DENIED.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/participants", tag = "Participants (v2)", operation_id = "v2_list_participants",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Roster", body = [ParticipantResponse]), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn list_participants(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<Vec<ParticipantResponse>>> {
    let list = state.meeting.admission.roster(&actor, id).await?;
    Ok(Json(list.iter().map(views::participant).collect()))
}

/// Waiting queue (requires `participant.admit`).
///
/// Returns the participants currently WAITING for admission, oldest request first. The caller's
/// conference role (from an admitted participant or a space membership) must hold
/// `participant.admit`. Returns FEATURE_DISABLED when the waiting room is not enabled.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/waiting-room", tag = "Waiting Room (v2)", operation_id = "v2_get_waiting_room",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses((status = 200, description = "Waiting participants", body = WaitingRoomResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn get_waiting_room(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<WaitingRoomResponse>> {
    let list = state.meeting.admission.waiting_room(&actor, id).await?;
    Ok(Json(WaitingRoomResponse {
        conference_id: id,
        participants: list.iter().map(views::participant).collect(),
    }))
}

/// Admit one, selected or all waiting participants (transactional, idempotent).
///
/// Moves WAITING participants to ADMITTED: pass `participantIds` (up to 500) or `all: true` for
/// everyone currently waiting. Requires `participant.admit` and a LIVE conference, and respects the
/// meeting's capacity. Any id outside this conference fails the whole request; already admitted ids
/// are reported as `unchanged`, and banned or otherwise ineligible ones as `skipped`.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/waiting-room/admit", tag = "Waiting Room (v2)", operation_id = "v2_admit",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = AdmitRequest,
    responses(
        (status = 200, description = "Result", body = BulkAdmissionResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND (incl. ids of other conferences)", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE (e.g. ENDING)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn admit(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<AdmitRequest>,
) -> ApiResult<Json<BulkAdmissionResponse>> {
    let o = state
        .meeting
        .admission
        .admit(&actor, id, &req.participant_ids, req.all)
        .await?;
    Ok(Json(views::bulk(o)))
}

/// Deny one, selected or all waiting participants.
///
/// Moves WAITING participants to DENIED: pass `participantIds` (up to 500) or `all: true`. Optional
/// `reasonCode` (max 64 chars) and `message` (max 500 chars, shown to the denied participant), plus
/// an `internalNote` that is never shown to them. Requires `participant.deny`; any id outside this
/// conference fails the whole request, already denied ids are `unchanged` and others `skipped`.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/waiting-room/deny", tag = "Waiting Room (v2)", operation_id = "v2_deny",
    params(("id" = Uuid, Path, description = "Conference id")),
    request_body = DenyRequest,
    responses((status = 200, description = "Result", body = BulkAdmissionResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn deny(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<DenyRequest>,
) -> ApiResult<Json<BulkAdmissionResponse>> {
    let o = state.meeting.admission.deny(&actor, id, &req).await?;
    Ok(Json(views::bulk(o)))
}

/// Return an admitted participant to the waiting room.
///
/// Moves an ADMITTED participant back to WAITING and disconnects their active media sessions; they
/// must be admitted again to return. Requires `participant.return_to_waiting`, and the target must
/// be less privileged than the caller (never yourself). Idempotent if the participant is already
/// waiting; `providerEnforcement` tells whether the RTC provider has confirmed the disconnect.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/return-to-waiting", tag = "Waiting Room (v2)", operation_id = "v2_return_to_waiting",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    responses((status = 200, description = "Returned", body = ModerationResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn return_to_waiting(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ModerationResponse>> {
    let o = state
        .meeting
        .admission
        .return_to_waiting(&actor, id, pid)
        .await?;
    Ok(Json(ModerationResponse {
        participant: views::participant(&o.participant),
        ban_id: None,
        provider_enforcement: o.enforcement.into(),
    }))
}

/// Change a participant's role (grant authority enforced).
///
/// Assigns a new conference role to an ADMITTED or WAITING participant. Requires
/// `participant.role_change`; the target must be less privileged than the caller, you cannot change
/// your own or the owner's role, and the new role must be below your own (co-host also requires
/// `cohost.assign`; OWNER is never granted). Optionally send the participant's `version` for
/// optimistic concurrency; media permissions of connected sessions follow the new role.
#[utoipa::path(
    patch, path = "/api/v2/conferences/{id}/participants/{participantId}/role", tag = "Participants (v2)", operation_id = "v2_change_role",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = RoleChangeRequest,
    responses(
        (status = 200, description = "Changed", body = ParticipantResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "PARTICIPANT_STATE_CONFLICT", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn change_role(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<RoleChangeRequest>,
) -> ApiResult<Json<ParticipantResponse>> {
    let p = state
        .meeting
        .admission
        .change_role(&actor, id, pid, &req)
        .await?;
    Ok(Json(views::participant(&p)))
}

/// Remove a participant (business authority; media disconnect enforced by the RTC provider).
///
/// Sets the participant to REMOVED and disconnects their active media sessions; an optional
/// `reason` is recorded in the audit log. Requires `participant.remove`, and the target must be
/// less privileged than the caller (never yourself). Idempotent for participants already removed or
/// banned; `providerEnforcement` tells whether the RTC provider has confirmed the disconnect.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/remove", tag = "Participants (v2)", operation_id = "v2_remove",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = Option<RemoveParticipantRequest>,
    responses((status = 200, description = "Removed", body = ModerationResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn remove(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
    V2OptionalJson(req): V2OptionalJson<RemoveParticipantRequest>,
) -> ApiResult<Json<ModerationResponse>> {
    let o = state
        .meeting
        .admission
        .remove(&actor, id, pid, req.reason)
        .await?;
    Ok(Json(ModerationResponse {
        participant: views::participant(&o.participant),
        ban_id: None,
        provider_enforcement: o.enforcement.into(),
    }))
}

/// Ban a participant (conference or space scope).
///
/// Sets the participant to BANNED, disconnects their active sessions and records a ban on their
/// identity for this conference (default) or the whole meeting space (`scope: SPACE`, also blocks
/// future occurrences), with optional `reasonCode`, `reason` and future `expiresAt`. Requires
/// `participant.ban`, and the target must be less privileged than the caller (never yourself).
/// Idempotent for an already banned participant (no new `banId` is returned).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/ban", tag = "Participants (v2)", operation_id = "v2_ban",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = Option<BanParticipantRequest>,
    responses((status = 200, description = "Banned", body = ModerationResponse), (status = 404, description = "PARTICIPANT_NOT_FOUND", body = ErrorEnvelope)),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn ban(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
    V2OptionalJson(req): V2OptionalJson<BanParticipantRequest>,
) -> ApiResult<Json<ModerationResponse>> {
    let o = state.meeting.admission.ban(&actor, id, pid, &req).await?;
    Ok(Json(ModerationResponse {
        participant: views::participant(&o.participant),
        ban_id: o.ban_id,
        provider_enforcement: o.enforcement.into(),
    }))
}

/// Server-side mute of a participant's published tracks.
///
/// A provider-enforced primitive. Authorization and conference scoping are checked before any
/// provider call.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/mute", tag = "Participants (v2)", operation_id = "v2_mute_participant",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = MuteRequest,
    responses(
        (status = 200, description = "Muted", body = MuteResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "PARTICIPANT_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_NOT_LIVE", body = ErrorEnvelope),
        (status = 503, description = "RTC_PROVIDER_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn mute(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, pid)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<MuteRequest>,
) -> ApiResult<Json<MuteResponse>> {
    let o = state
        .meeting
        .rtc
        .mute(&actor, id, pid, req.track_kind)
        .await?;
    Ok(Json(MuteResponse {
        participant_id: pid,
        track_kind: req.track_kind,
        sessions: o.sessions,
        muted_tracks: o.muted_tracks,
    }))
}
