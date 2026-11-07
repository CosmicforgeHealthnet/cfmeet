//! Workspace context endpoints (Blueprint Section 8, ADR 0056 §5): the caller's
//! workspaces and the server-derived capabilities of one of them, computed per request.
//! There is deliberately no "switch workspace" endpoint and no stored active organization.

use axum::{
    extract::{Path, Query, State},
    http::{header::CACHE_CONTROL, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};

use crate::{
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{actor::ActorContext, organizations_authority::OrganizationsAuthority, workspaces},
    state::AppState,
};

/// The Organizations integration when it is authoritative (`None` in legacy / shadow mode).
fn authoritative(state: &AppState) -> Option<&OrganizationsAuthority> {
    state
        .meeting
        .core
        .organization_authority
        .as_deref()
        .filter(|a| a.is_authoritative())
}

/// Per-request answers are never cached.
fn no_store<T: serde::Serialize>(body: T) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// List the caller's workspaces (Personal and organizations).
///
/// Returns the caller's workspace contexts: `personal` first, then every organization in which
/// the caller holds an ACTIVE membership, ordered by id, each with the caller's role, the
/// organization's status (a SUSPENDED organization is listed and grants nothing), its
/// provisioning state and a small capability summary. Workspace context is per request: the
/// server stores no active organization, so organization-scoped calls keep naming their
/// organization. Once the Organizations Service is authoritative the list comes from it (all
/// roles, including GUEST; capabilities under the organization policy); before that it comes
/// from the meeting-service organization directory with the rule of `GET
/// /api/v2/me/organizations` and `provisioningState` is null. Paginate with `limit` (1-200,
/// default 50) and the opaque `nextCursor`.
#[utoipa::path(
    get, path = "/api/v2/me/workspaces", tag = "Organizations (v2)", operation_id = "v2_my_workspaces",
    params(
        ("cursor" = Option<String>, Query, description = "Opaque cursor from nextCursor"),
        ("limit" = Option<u32>, Query, description = "Page size, 1-200 (default 50)"),
    ),
    responses(
        (status = 200, description = "Workspaces", body = WorkspacePage),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable; never a partial list)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn my_workspaces(
    State(state): State<AppState>,
    actor: ActorContext,
    query: Result<Query<WorkspaceListQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Response> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let page = workspaces::list(&state.db, authoritative(&state), &actor, &q).await?;
    Ok(no_store(page))
}

/// Capabilities of one workspace, derived by the server.
///
/// Returns the features and every capability decision (granted, and why not:
/// CAPABILITY_NOT_GRANTED, POLICY_RESTRICTED, ORGANIZATION_SUSPENDED or FEATURE_DISABLED) for the
/// caller in `workspaceId` — `personal`, or an organization id from `GET /api/v2/me/workspaces`.
/// Computed on every request from the organization authority (role matrix and effective
/// organization policy once the Organizations Service is authoritative); owner services still
/// re-verify each operation. Unknown organizations, organizations the caller is not an ACTIVE
/// member of and malformed ids all answer ORGANIZATION_NOT_FOUND. Personal communications
/// capabilities stay with the Communications Hub (`personalCommunicationsCapabilities`).
#[utoipa::path(
    get, path = "/api/v2/me/workspaces/{workspaceId}/capabilities", tag = "Organizations (v2)", operation_id = "v2_my_workspace_capabilities",
    params(("workspaceId" = String, Path, description = "`personal`, or an organization id")),
    responses(
        (status = 200, description = "Capabilities", body = WorkspaceCapabilities),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "ORGANIZATION_NOT_FOUND", body = ErrorEnvelope),
        (status = 503, description = "SERVICE_UNAVAILABLE (Organizations unreachable)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn my_workspace_capabilities(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(workspace_id): Path<String>,
) -> ApiResult<Response> {
    let answer =
        workspaces::capabilities(&state.db, authoritative(&state), &actor, &workspace_id).await?;
    Ok(no_store(answer))
}
