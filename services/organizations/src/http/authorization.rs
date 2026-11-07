//! Service authorization reads (membership resolution, capability evaluation) and the
//! organization audit log.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{authorize, middleware::Caller, page_limit, parse, respond};
use crate::{
    db,
    domain::{
        effective_capabilities, evaluate as decide, Capability, LifecycleStatus, MembershipStatus,
        Role,
    },
    error::{ApiError, ApiResult},
    state::AppState,
    store,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveBody {
    pub user_id: Uuid,
    #[serde(default)]
    pub organization_ids: Option<Vec<Uuid>>,
}

#[derive(Debug, FromQueryResult)]
struct ResolvedRow {
    membership_id: Uuid,
    organization_id: Uuid,
    role: String,
    status: String,
    membership_version: i64,
    lifecycle_status: String,
    provisioning_state: String,
    organization_version: i64,
}

/// `POST /internal/v1/organizations/memberships/resolve` — the user's ACTIVE memberships
/// with role, effective capabilities and organization status.
pub async fn resolve(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let (req, _): (ResolveBody, Value) = parse(&body)?;
    if req
        .organization_ids
        .as_ref()
        .is_some_and(|ids| ids.len() > 100)
    {
        return Err(ApiError::validation(
            "organizationIds is limited to 100 ids",
        ));
    }
    let filter = req.organization_ids.as_ref().map(|ids| json!(ids));
    let rows: Vec<ResolvedRow> = db::all(
        &state.db,
        "SELECT m.id AS membership_id, m.organization_id, m.role, m.status, m.version AS membership_version, \
                o.lifecycle_status, o.provisioning_state, o.version AS organization_version \
         FROM organization_memberships m JOIN organizations o ON o.id = m.organization_id \
         WHERE m.user_id = $1 AND m.status = 'ACTIVE' \
           AND ($2::jsonb IS NULL OR m.organization_id IN (SELECT (jsonb_array_elements_text($2::jsonb))::uuid)) \
         ORDER BY m.organization_id",
        vec![req.user_id.into(), filter.into()],
    )
    .await?;
    let memberships: Vec<Value> = rows
        .iter()
        .map(|r| {
            let role = Role::parse(&r.role).unwrap_or(Role::Guest);
            let status = MembershipStatus::parse(&r.status).unwrap_or(MembershipStatus::Suspended);
            let org_status =
                LifecycleStatus::parse(&r.lifecycle_status).unwrap_or(LifecycleStatus::Suspended);
            json!({
                "membershipId": r.membership_id,
                "organizationId": r.organization_id,
                "role": r.role,
                "status": r.status,
                "capabilities": effective_capabilities(role, status, org_status),
                "organizationStatus": r.lifecycle_status,
                "provisioningState": r.provisioning_state,
                "membershipVersion": r.membership_version,
                "organizationVersion": r.organization_version,
            })
        })
        .collect();
    Ok(respond(
        StatusCode::OK,
        json!({ "userId": req.user_id, "memberships": memberships }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluateBody {
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub capability: String,
}

/// `POST /internal/v1/organizations/authorization/evaluate` →
/// `{allowed, reason, organizationStatus, role}`. A non-member (or unknown organization)
/// gets `NOT_A_MEMBER` with no status and no role (non-enumerating).
pub async fn evaluate(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let (req, _): (EvaluateBody, Value) = parse(&body)?;
    let capability = Capability::parse(&req.capability)
        .ok_or_else(|| ApiError::validation("capability is not a known capability"))?;
    let org = store::org(&state.db, req.organization_id, false).await?;
    let membership = match &org {
        Some(_) => {
            store::current_membership(&state.db, req.organization_id, req.user_id, false).await?
        }
        None => None,
    };
    let (reason, status, role) = match (&org, &membership) {
        (Some(o), Some(m)) => (
            decide(Some((m.role(), m.status())), o.status(), capability),
            Some(o.lifecycle_status.clone()),
            Some(m.role.clone()),
        ),
        _ => (
            decide(None, LifecycleStatus::Active, capability),
            None,
            None,
        ),
    };
    Ok(respond(
        StatusCode::OK,
        json!({
            "allowed": reason.allowed(),
            "reason": reason.as_str(),
            "organizationStatus": status,
            "role": role,
            "capability": capability,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct AuditRow {
    seq: i64,
    id: Uuid,
    organization_id: Option<Uuid>,
    actor_kind: String,
    actor_user_id: Option<Uuid>,
    actor_service: String,
    action: String,
    target_type: String,
    target_id: Option<Uuid>,
    summary: Value,
    request_id: Option<String>,
    correlation_id: Option<String>,
    occurred_at: DateTime<Utc>,
}

/// `GET /internal/v1/organizations/{id}/audit-events?before&limit` — newest first;
/// requires `organization.audit.read` for the actor.
pub async fn audit_events(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let user = caller.user()?;
    let limit = page_limit(q.limit)?;
    authorize(&state.db, id, user, Capability::AuditRead, false).await?;
    let rows: Vec<AuditRow> = db::all(
        &state.db,
        "SELECT seq, id, organization_id, actor_kind, actor_user_id, actor_service, action, target_type, target_id, summary, request_id, correlation_id, occurred_at \
         FROM organization_audit_events WHERE organization_id = $1 AND ($2::bigint IS NULL OR seq < $2) ORDER BY seq DESC LIMIT $3",
        vec![id.into(), q.before.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "id": r.id,
                "seq": r.seq,
                "organizationId": r.organization_id,
                "action": r.action,
                "actor": { "kind": r.actor_kind, "userId": r.actor_user_id, "serviceId": r.actor_service },
                "targetType": r.target_type,
                "targetId": r.target_id,
                "summary": r.summary,
                "requestId": r.request_id,
                "correlationId": r.correlation_id,
                "occurredAt": store::rfc3339(r.occurred_at),
            })
        })
        .collect();
    let next = if more {
        rows.get(limit as usize - 1).map(|r| r.seq)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}
