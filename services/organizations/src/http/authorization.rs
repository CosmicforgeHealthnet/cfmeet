//! Service authorization reads (membership resolution with team context, capability
//! evaluation under the organization policy, the role / capability matrix, verified
//! technical-scope resolution) and the organization audit log.

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
        effective_capabilities_with_policy, evaluate_with_policy as decide, role_grants,
        Capability, DecisionReason, LifecycleStatus, MembershipStatus, Role, TeamRole,
    },
    error::{ApiError, ApiResult, ErrorCode},
    provisioning,
    state::AppState,
    store,
};

/// SEC-020: a relayed acting user (only ever honoured from `organization.act-for-user`
/// callers) may ask these reads about themselves only; a differing body `userId` is refused
/// before any lookup, so the body can never substitute another user's membership for the
/// actor's. Service reads without an actor keep naming the subject in the body.
fn subject_is_actor(caller: &Caller, subject: Uuid) -> Result<(), ApiError> {
    match caller.acting_user {
        Some(actor) if actor != subject => Err(ApiError::new(ErrorCode::PermissionDenied)),
        _ => Ok(()),
    }
}

#[derive(Debug, FromQueryResult)]
struct TeamContextRow {
    membership_id: Uuid,
    team_id: Uuid,
    role: String,
}

/// The ACTIVE team memberships (of ACTIVE teams) of `memberships`, by membership id.
async fn team_context<C: sea_orm::ConnectionTrait>(
    c: &C,
    memberships: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<Value>>, ApiError> {
    let mut out: std::collections::HashMap<Uuid, Vec<Value>> = Default::default();
    if memberships.is_empty() {
        return Ok(out);
    }
    let rows: Vec<TeamContextRow> = db::all(
        c,
        "SELECT tm.membership_id, tm.team_id, tm.role FROM organization_team_members tm \
         JOIN organization_teams t ON t.id = tm.team_id AND t.status = 'ACTIVE' \
         WHERE tm.status = 'ACTIVE' AND tm.membership_id IN (SELECT (jsonb_array_elements_text($1::jsonb))::uuid) \
         ORDER BY tm.team_id",
        vec![json!(memberships).into()],
    )
    .await?;
    for r in rows {
        out.entry(r.membership_id)
            .or_default()
            .push(json!({ "teamId": r.team_id, "role": r.role }));
    }
    Ok(out)
}

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
    policy_document: Option<Value>,
    policy_revision: Option<i64>,
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
    subject_is_actor(&caller, req.user_id)?;
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
                o.lifecycle_status, o.provisioning_state, o.version AS organization_version, \
                p.document AS policy_document, p.revision AS policy_revision \
         FROM organization_memberships m JOIN organizations o ON o.id = m.organization_id \
         LEFT JOIN organization_policies p ON p.organization_id = o.id \
         WHERE m.user_id = $1 AND m.status = 'ACTIVE' \
           AND ($2::jsonb IS NULL OR m.organization_id IN (SELECT (jsonb_array_elements_text($2::jsonb))::uuid)) \
         ORDER BY m.organization_id",
        vec![req.user_id.into(), filter.into()],
    )
    .await?;
    let teams = team_context(
        &state.db,
        &rows.iter().map(|r| r.membership_id).collect::<Vec<_>>(),
    )
    .await?;
    let mut memberships: Vec<Value> = Vec::with_capacity(rows.len());
    for r in &rows {
        let role = Role::parse(&r.role).unwrap_or(Role::Guest);
        let status = MembershipStatus::parse(&r.status).unwrap_or(MembershipStatus::Suspended);
        let org_status =
            LifecycleStatus::parse(&r.lifecycle_status).unwrap_or(LifecycleStatus::Suspended);
        let policy = store::parse_policy(r.policy_document.as_ref())?;
        memberships.push(json!({
            "membershipId": r.membership_id,
            "organizationId": r.organization_id,
            "role": r.role,
            "status": r.status,
            "capabilities": effective_capabilities_with_policy(role, status, org_status, &policy),
            "organizationStatus": r.lifecycle_status,
            "provisioningState": r.provisioning_state,
            "membershipVersion": r.membership_version,
            "organizationVersion": r.organization_version,
            "policyRevision": r.policy_revision.unwrap_or(0),
            "teams": teams.get(&r.membership_id).cloned().unwrap_or_default(),
        }));
    }
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
    /// Team context (Section 6): with `organization.teams.manage`, a MANAGER is allowed
    /// only as TEAM_MANAGER of this team; the response carries the user's `teamRole`.
    #[serde(default)]
    pub team_id: Option<Uuid>,
}

/// `POST /internal/v1/organizations/authorization/evaluate` →
/// `{allowed, reason, organizationStatus, role, capability, teamId, teamRole}` under the
/// organization policy. A non-member (or unknown organization) gets `NOT_A_MEMBER` with
/// no status and no role (non-enumerating).
pub async fn evaluate(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let (req, _): (EvaluateBody, Value) = parse(&body)?;
    subject_is_actor(&caller, req.user_id)?;
    let capability = Capability::parse(&req.capability)
        .ok_or_else(|| ApiError::validation("capability is not a known capability"))?;
    let org = store::org(&state.db, req.organization_id, false).await?;
    let membership = match &org {
        Some(_) => {
            store::current_membership(&state.db, req.organization_id, req.user_id, false).await?
        }
        None => None,
    };
    let policy = match &org {
        Some(o) => store::policy(&state.db, o.id).await?,
        None => Default::default(),
    };
    let (mut reason, status, role) = match (&org, &membership) {
        (Some(o), Some(m)) => (
            decide(
                Some((m.role(), m.status())),
                o.status(),
                capability,
                &policy,
            ),
            Some(o.lifecycle_status.clone()),
            Some(m.role.clone()),
        ),
        _ => (
            decide(None, LifecycleStatus::Active, capability, &policy),
            None,
            None,
        ),
    };
    let mut team_role: Option<String> = None;
    if let (Some(team), Some(m)) = (req.team_id, &membership) {
        team_role = super::teams::team_role_of(&state.db, req.organization_id, team, m.id)
            .await?
            .map(|r| r.as_str().to_string());
        // Managers manage their own teams only (OWNER / ADMIN manage every team).
        if capability == Capability::TeamsManage
            && reason.allowed()
            && m.role() == Role::Manager
            && team_role.as_deref() != Some(TeamRole::TeamManager.as_str())
        {
            reason = DecisionReason::CapabilityNotGranted;
        }
    }
    Ok(respond(
        StatusCode::OK,
        json!({
            "allowed": reason.allowed(),
            "reason": reason.as_str(),
            "organizationStatus": status,
            "role": role,
            "capability": capability,
            "teamId": req.team_id,
            "teamRole": team_role,
        }),
    ))
}

/// `GET /internal/v1/organizations/{id}/roles/capabilities` — the shared default-deny
/// vocabulary for UIs: the role matrix, the team roles and what each role holds under
/// the organization's current policy and status. Members (or a service read).
pub async fn roles_capabilities(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let org = match caller.acting_user {
        Some(user) => {
            authorize(&state.db, id, user, Capability::OrganizationRead, false)
                .await?
                .org
        }
        None => store::org(&state.db, id, false)
            .await?
            .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?,
    };
    let row = store::policy_row(&state.db, id, false).await?;
    let policy = store::parse_policy(row.as_ref().map(|r| &r.document))?;
    let roles: Vec<Value> = Role::ALL
        .iter()
        .map(|role| {
            json!({
                "role": role,
                "capabilities": Capability::ALL.iter().filter(|c| role_grants(*role, **c)).collect::<Vec<_>>(),
                "effectiveCapabilities": effective_capabilities_with_policy(*role, MembershipStatus::Active, org.status(), &policy),
            })
        })
        .collect();
    Ok(respond(
        StatusCode::OK,
        json!({
            "organizationId": org.id,
            "lifecycleStatus": org.lifecycle_status,
            "policyRevision": row.as_ref().map(|r| r.revision).unwrap_or(0),
            "capabilities": Capability::ALL,
            "roles": roles,
            "teamRoles": [
                { "role": TeamRole::TeamManager, "description": "Manages the team's members (an organization MANAGER, ADMIN or OWNER; ADMIN / OWNER assign it)." },
                { "role": TeamRole::TeamMember, "description": "A member of the team." },
            ],
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeResolveBody {
    pub organization_id: Uuid,
    #[serde(default)]
    pub capability: Option<String>,
    /// When set, the user must hold an ACTIVE membership (non-enumerating 404 otherwise).
    #[serde(default)]
    pub user_id: Option<Uuid>,
}

/// `POST /internal/v1/organizations/scopes/resolve` — the VERIFIED technical scope
/// (tenant / application / environment) of an organization, only when the organization
/// is ACTIVE, provisioning is READY, the mapping is READY and its tenant status is applied
/// in Platform Access; otherwise 409 `PROVISIONING_PENDING` (or the lifecycle code).
pub async fn resolve_scope(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let (req, _): (ScopeResolveBody, Value) = parse(&body)?;
    if let Some(user) = req.user_id {
        subject_is_actor(&caller, user)?;
    }
    let capability = req
        .capability
        .as_deref()
        .unwrap_or(provisioning::CAPABILITY);
    if capability != provisioning::CAPABILITY {
        return Err(ApiError::validation("capability must be primary"));
    }
    let org = store::org(&state.db, req.organization_id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if let Some(user) = req.user_id.or(caller.acting_user) {
        let active = store::current_membership(&state.db, org.id, user, false)
            .await?
            .is_some_and(|m| m.status() == MembershipStatus::Active);
        if !active {
            return Err(ApiError::new(ErrorCode::OrganizationNotFound));
        }
    }
    match org.status() {
        LifecycleStatus::Active => {}
        LifecycleStatus::Suspended => return Err(ApiError::new(ErrorCode::OrganizationSuspended)),
        LifecycleStatus::Archived => return Err(ApiError::new(ErrorCode::OrganizationArchived)),
    }
    let mapping = provisioning::mapping(&state.db, org.id, capability, false).await?;
    let ready = org.provisioning_state == "READY"
        && mapping.as_ref().is_some_and(|m| {
            m.mapping_state == "READY"
                && m.verified_at.is_some()
                && m.tenant_status_desired.as_deref() == Some("ACTIVE")
                && m.in_sync()
        });
    let Some(m) = mapping.filter(|_| ready) else {
        return Err(ApiError::new(ErrorCode::ProvisioningPending));
    };
    // Section 12 follow-up (§8.5) and final closure: the operator-recorded Calling and
    // Messaging bindings inside this triple, ids only (`null` when none). Platform Access
    // re-verifies them at use.
    let calling = super::bindings::binding(
        &state.db,
        org.id,
        capability,
        super::bindings::CALLING,
        false,
    )
    .await?
    .map(|b| b.to_resolve_json());
    let messaging = super::bindings::binding(
        &state.db,
        org.id,
        capability,
        super::bindings::MESSAGING,
        false,
    )
    .await?
    .map(|b| b.to_resolve_json());
    Ok(respond(
        StatusCode::OK,
        json!({
            "organizationId": org.id,
            "capability": m.capability,
            "tenantId": m.tenant_id,
            "applicationId": m.application_id,
            "environmentId": m.environment_id,
            "mappingVersion": m.mapping_version,
            "verifiedAt": m.verified_at.map(store::rfc3339),
            "serviceBindings": { "calling": calling, "messaging": messaging },
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
