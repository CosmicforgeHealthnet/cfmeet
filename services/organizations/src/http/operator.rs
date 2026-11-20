//! The admin bridge's operator reads (ADR 0068, design §7): `/internal/v1/operator/*`.
//!
//! Only `cfmeet.admin-bridge` can hold `organization.operator-read`, and every request must
//! name the HQ staff member it reads for (`X-CF-Operator`; refused otherwise). The reads
//! reuse the authoritative service reads (no acting user), so they expose exactly what a
//! service caller already sees: identifiers, roles and statuses, never an email.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use uuid::Uuid;

use super::{
    authorization::{self, OperatorAuditQuery},
    members,
    middleware::Caller,
    page_limit, policies, respond, teams,
};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    state::AppState,
    store,
};

async fn existing(state: &AppState, id: Uuid) -> Result<store::OrgRow, ApiError> {
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))
}

/// `GET /internal/v1/operator/organizations/{id}` — one organization (the same object as
/// an operator list item: no viewer, no acting user).
pub async fn organization(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    let org = existing(&state, id).await?;
    Ok(respond(
        StatusCode::OK,
        store::organization_json(&state.db, &org, None).await?,
    ))
}

/// `GET /internal/v1/operator/organizations/{id}/members?cursor&limit` — non-removed
/// memberships in id order (same rows as the service read of `members`).
pub async fn members(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<members::ListQuery>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    let limit = page_limit(q.limit)?;
    existing(&state, id).await?;
    members::page(&state, id, q.cursor, limit).await
}

/// `GET /internal/v1/operator/organizations/{id}/teams?status&cursor&limit` — teams in id
/// order with active member counts (`status` ACTIVE by default, or ARCHIVED).
pub async fn teams(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<teams::ListQuery>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    let limit = page_limit(q.limit)?;
    let status = q.status.as_deref().unwrap_or("ACTIVE");
    if !matches!(status, "ACTIVE" | "ARCHIVED") {
        return Err(ApiError::validation("status must be ACTIVE or ARCHIVED"));
    }
    existing(&state, id).await?;
    teams::page(&state, id, status, q.cursor, limit).await
}

/// `GET /internal/v1/operator/organizations/{id}/policies` — the stored policy document.
pub async fn policies(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    let org = existing(&state, id).await?;
    let row = store::policy_row(&state.db, id, false).await?;
    let doc = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    Ok(respond(
        StatusCode::OK,
        policies::policy_json(&org, row.as_ref(), &doc),
    ))
}

/// `GET /internal/v1/operator/audit-events?since&until&cursor&limit` — the audit history of
/// every organization, newest first.
pub async fn audit_events(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<OperatorAuditQuery>,
) -> ApiResult<Response> {
    caller.require_operator_read()?;
    authorization::operator_audit_page(&state, &q).await
}
