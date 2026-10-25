//! Invitation endpoints.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{views, V2OptionalJson, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// Create an invitation. The raw token is returned once and never stored.
///
/// Issues a bearer invitation for an active meeting space, optionally limited to one conference
/// (`conferenceId`), bound to a verified account email (`inviteeEmail`) or capped by `useLimit`
/// (1–10000). It expires after `expiresInHours` (default 168, capped by server policy) and grants
/// `role` (default PARTICIPANT), which must be below the inviter's own role. Requires a bearer user
/// token with invite authority in the space (a role allowed to admit participants); store the token
/// from the response, it cannot be retrieved again.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/invitations", tag = "Invitations (v2)", operation_id = "v2_create_invitation",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = CreateInvitationRequest,
    responses(
        (status = 201, description = "Created", body = CreateInvitationResponse),
        (status = 403, description = "PERMISSION_DENIED (incl. role above grant authority)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2OptionalJson(req): V2OptionalJson<CreateInvitationRequest>,
) -> ApiResult<(StatusCode, Json<CreateInvitationResponse>)> {
    let (inv, token) = state.meeting.invitations.create(&actor, id, req).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreateInvitationResponse {
            invitation: views::invitation(&inv),
            token,
        }),
    ))
}

/// List invitations of a space (no tokens or hashes).
///
/// Returns all invitations of the meeting space, newest first, including revoked and expired ones,
/// with their status and usage count. Requires a bearer user token with invite authority in the
/// space (a role allowed to admit participants).
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/invitations", tag = "Invitations (v2)", operation_id = "v2_list_invitations",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses((status = 200, description = "Invitations", body = [InvitationResponse]), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_invitations(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<Vec<InvitationResponse>>> {
    let list = state.meeting.invitations.list(&actor, id).await?;
    Ok(Json(list.iter().map(views::invitation).collect()))
}

/// Revoke an invitation.
///
/// Marks the invitation REVOKED so it can no longer be resolved or used to join. Requires a bearer
/// user token with invite authority in the space (a role allowed to admit participants). Revoking
/// an already revoked invitation is a no-op that returns it unchanged.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke", tag = "Invitations (v2)", operation_id = "v2_revoke_invitation",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("invitationId" = Uuid, Path, description = "Invitation id")),
    responses((status = 200, description = "Revoked", body = InvitationResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn revoke_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, invitation_id)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<InvitationResponse>> {
    let inv = state
        .meeting
        .invitations
        .revoke(&actor, id, invitation_id)
        .await?;
    Ok(Json(views::invitation(&inv)))
}

/// Resolve an invitation token (read-only; does not consume a use).
///
/// Public endpoint, no authentication required. Returns the meeting space the token belongs to, the
/// conference it is limited to (if any), the role it grants, its expiry and remaining uses.
/// Unknown, revoked or used-up tokens return INVITATION_INVALID and expired ones
/// INVITATION_EXPIRED; a use is consumed only when the token is used to join a conference.
#[utoipa::path(
    post, path = "/api/v2/invitations/{token}/resolve", tag = "Invitations (v2)", operation_id = "v2_resolve_invitation",
    params(("token" = String, Path, description = "Raw invitation token")),
    responses(
        (status = 200, description = "Valid invitation", body = ResolveInvitationResponse),
        (status = 410, description = "INVITATION_INVALID / INVITATION_EXPIRED", body = ErrorEnvelope),
    ),
    security(())
)]
pub async fn resolve_invitation(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(token): V2Path<String>,
) -> ApiResult<Json<ResolveInvitationResponse>> {
    let (inv, space) = state.meeting.invitations.resolve(&token).await?;
    let view = state.meeting.spaces.view_of(&actor, space).await?;
    Ok(Json(ResolveInvitationResponse {
        meeting_space: views::space(&view),
        conference_id: inv.conference_id,
        requested_role: inv.requested_role,
        expires_at: inv.expires_at,
        remaining_uses: inv.use_limit.map(|l| l - inv.usage_count),
    }))
}
