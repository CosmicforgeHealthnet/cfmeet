//! Organization directory endpoints (membership authority for ORGANIZATION_ONLY).

use axum::{extract::State, http::StatusCode, Json};
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    dto::v2::*,
    error::ApiResult,
    models::{organization_memberships, organizations},
    services::actor::ActorContext,
    state::AppState,
};

fn org(o: &organizations::Model) -> OrganizationResponse {
    OrganizationResponse {
        id: o.id,
        name: o.name.clone(),
        slug: o.slug.clone(),
        status: o.status,
        created_at: o.created_at,
    }
}

fn member(m: &organization_memberships::Model) -> OrganizationMemberResponse {
    OrganizationMemberResponse {
        id: m.id,
        organization_id: m.organization_id,
        user_id: m.user_id,
        role: m.role,
        status: m.status,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Create an organization (platform administrators).
#[utoipa::path(
    post, path = "/api/v2/admin/organizations", tag = "Organizations (v2)", operation_id = "v2_create_organization",
    request_body = CreateOrganizationRequest,
    responses((status = 201, description = "Created", body = OrganizationResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn create_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<CreateOrganizationRequest>,
) -> ApiResult<(StatusCode, Json<OrganizationResponse>)> {
    let o = state
        .meeting
        .organizations
        .create(&actor, &req.name, &req.slug, req.owner_user_id)
        .await?;
    Ok((StatusCode::CREATED, Json(org(&o))))
}

/// Suspend or reactivate an organization (platform administrators).
#[utoipa::path(
    patch, path = "/api/v2/admin/organizations/{id}", tag = "Organizations (v2)", operation_id = "v2_update_organization",
    params(("id" = Uuid, Path, description = "Organization id")),
    request_body = UpdateOrganizationRequest,
    responses((status = 200, description = "Updated", body = OrganizationResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn update_organization(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    V2Json(req): V2Json<UpdateOrganizationRequest>,
) -> ApiResult<Json<OrganizationResponse>> {
    let o = state
        .meeting
        .organizations
        .set_status(&actor, id, req.status)
        .await?;
    Ok(Json(org(&o)))
}

/// List members (platform admins, organization OWNER/ADMIN).
#[utoipa::path(
    get, path = "/api/v2/organizations/{id}/members", tag = "Organizations (v2)", operation_id = "v2_list_organization_members",
    params(("id" = Uuid, Path, description = "Organization id")),
    responses((status = 200, description = "Members", body = [OrganizationMemberResponse]), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn list_organization_members(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<Vec<OrganizationMemberResponse>>> {
    let list = state.meeting.organizations.list_members(&actor, id).await?;
    Ok(Json(list.iter().map(member).collect()))
}

/// Add or change a membership (platform admins, organization OWNER/ADMIN within limits).
#[utoipa::path(
    put, path = "/api/v2/organizations/{id}/members/{userId}", tag = "Organizations (v2)", operation_id = "v2_upsert_organization_member",
    params(("id" = Uuid, Path, description = "Organization id"), ("userId" = Uuid, Path, description = "User id")),
    request_body = UpsertOrganizationMemberRequest,
    responses((status = 200, description = "Membership", body = OrganizationMemberResponse), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn upsert_organization_member(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path((id, user_id)): V2Path<(Uuid, Uuid)>,
    V2Json(req): V2Json<UpsertOrganizationMemberRequest>,
) -> ApiResult<Json<OrganizationMemberResponse>> {
    let m = state
        .meeting
        .organizations
        .upsert_member(&actor, id, user_id, req.role, req.status)
        .await?;
    Ok(Json(member(&m)))
}

/// Organizations the caller verifiably belongs to.
#[utoipa::path(
    get, path = "/api/v2/me/organizations", tag = "Organizations (v2)", operation_id = "v2_my_organizations",
    responses((status = 200, description = "Organizations", body = [MyOrganizationResponse]), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn my_organizations(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<Vec<MyOrganizationResponse>>> {
    let list = state.meeting.organizations.mine(&actor).await?;
    Ok(Json(
        list.iter()
            .map(|(o, role)| MyOrganizationResponse {
                organization: org(o),
                role: *role,
            })
            .collect(),
    ))
}
