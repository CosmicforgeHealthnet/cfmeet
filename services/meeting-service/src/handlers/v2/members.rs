//! Persistent membership endpoints.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{views, V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// List members (owner / host / co-host members).
///
/// Returns every membership of the meeting space (active and revoked), oldest first. Requires a
/// bearer user token; only the space owner, hosts and co-hosts may view members.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/members", tag = "Members (v2)", operation_id = "v2_list_members",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses((status = 200, description = "Members", body = MemberListResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_members(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<MemberListResponse>> {
    let items = state.meeting.members.list(&actor, id).await?;
    Ok(Json(MemberListResponse {
        items: items.iter().map(views::member).collect(),
    }))
}

/// Add a member. The granted role is bounded by the caller's grant authority.
///
/// Grants a persistent membership in an active meeting space to either a registered user (`userId`)
/// or an email address (`email`); provide exactly one. Only the owner or a host may add members,
/// and the role must be below the caller's own (assigning co-host also requires `cohost.assign`).
/// You cannot add yourself or the owner, and a principal can hold only one active membership.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/members", tag = "Members (v2)", operation_id = "v2_add_member",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = AddMemberRequest,
    responses((status = 201, description = "Added", body = MemberResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn add_member(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<AddMemberRequest>,
) -> ApiResult<(StatusCode, Json<MemberResponse>)> {
    let m = state.meeting.members.add(&actor, id, req).await?;
    Ok((StatusCode::CREATED, Json(views::member(&m))))
}

/// Add a team of the organization.
///
/// Gap audit G7: for a meeting space of an organization, adds every active member of one of
/// the organization's teams with the given role, under the same rules as adding one member
/// (managers only; a role you may grant). The Organizations Service decides whether you may see
/// the team. Team members who already belong, the owner, you and inactive accounts are skipped.
/// At most 500 team members.
#[utoipa::path(
    post, path = "/api/v2/meeting-spaces/{id}/members/team", tag = "Members (v2)", operation_id = "v2_add_team_members",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    request_body = AddTeamMembersRequest,
    responses(
        (status = 200, description = "Added", body = AddTeamMembersResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "MEETING_NOT_FOUND / TEAM_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn add_team_members(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<AddTeamMembersRequest>,
) -> ApiResult<Json<AddTeamMembersResponse>> {
    let (added, skipped) = state.meeting.members.add_team(&actor, id, req).await?;
    Ok(Json(AddTeamMembersResponse {
        added: added.iter().map(views::member).collect(),
        skipped,
    }))
}

/// Change a member's role.
///
/// Changes the role of an active membership. Send the member's current `version`; a stale version
/// returns MEETING_VERSION_CONFLICT. Only the owner or a host may change roles, only for members
/// less privileged than themselves and never their own, and the new role must be below the caller's
/// own (co-host also requires `cohost.assign`).
#[utoipa::path(
    patch, path = "/api/v2/meeting-spaces/{id}/members/{memberId}", tag = "Members (v2)", operation_id = "v2_update_member",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("memberId" = Uuid, Path, description = "Member id")),
    request_body = UpdateMemberRequest,
    responses((status = 200, description = "Updated", body = MemberResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn update_member(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, member_id)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<UpdateMemberRequest>,
) -> ApiResult<Json<MemberResponse>> {
    let m = state
        .meeting
        .members
        .update(&actor, id, member_id, req)
        .await?;
    Ok(Json(views::member(&m)))
}

/// Revoke a membership.
///
/// Marks the membership REVOKED and returns it; the record is kept. Only the owner or a host may
/// revoke, and only members less privileged than themselves (or their own membership). Revoking an
/// already revoked membership is a no-op that returns it unchanged.
#[utoipa::path(
    delete, path = "/api/v2/meeting-spaces/{id}/members/{memberId}", tag = "Members (v2)", operation_id = "v2_revoke_member",
    params(("id" = Uuid, Path, description = "Meeting space id"), ("memberId" = Uuid, Path, description = "Member id")),
    responses((status = 200, description = "Revoked", body = MemberResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn revoke_member(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, member_id)): V2Path<(Uuid, Uuid)>,
) -> ApiResult<Json<MemberResponse>> {
    let m = state.meeting.members.revoke(&actor, id, member_id).await?;
    Ok(Json(views::member(&m)))
}
