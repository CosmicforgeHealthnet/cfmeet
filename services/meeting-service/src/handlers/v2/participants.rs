//! Waiting room, roster, role change and moderation endpoints. Every participant id is
//! scoped to the conference in the path; cross-conference ids are rejected before any
//! state mutation or provider interaction.

use axum::{extract::State, Json};
use uuid::Uuid;

use super::{views, V2Json, V2OptionalJson, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// The caller's own participant state (registered user or participant token).
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
    let p = state
        .meeting
        .admission
        .return_to_waiting(&actor, id, pid)
        .await?;
    Ok(Json(ModerationResponse {
        participant: views::participant(&p),
        ban_id: None,
        provider_enforcement: ProviderEnforcement::PendingProviderControl,
    }))
}

/// Change a participant's role (grant authority enforced).
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

/// Remove a participant (business authority; media disconnect is enforced in Part E).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/remove", tag = "Participants (v2)", operation_id = "v2_remove",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = RemoveParticipantRequest,
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
        provider_enforcement: ProviderEnforcement::PendingProviderControl,
    }))
}

/// Ban a participant (conference or space scope).
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/participants/{participantId}/ban", tag = "Participants (v2)", operation_id = "v2_ban",
    params(("id" = Uuid, Path, description = "Conference id"), ("participantId" = Uuid, Path, description = "Participant id")),
    request_body = BanParticipantRequest,
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
        provider_enforcement: ProviderEnforcement::PendingProviderControl,
    }))
}
