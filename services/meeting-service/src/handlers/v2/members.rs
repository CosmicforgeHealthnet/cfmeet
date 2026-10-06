//! Persistent membership endpoints.

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{views, V2Json, V2Path};
use crate::{dto::v2::*, error::ApiResult, services::actor::ActorContext, state::AppState};

/// List members (owner / host / co-host members).
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

/// Change a member's role.
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
