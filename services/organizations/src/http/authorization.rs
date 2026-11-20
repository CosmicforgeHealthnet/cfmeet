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
    /// Exact action, e.g. `membership.updated`.
    #[serde(default)]
    pub action: Option<String>,
    /// Only changes made by this user.
    #[serde(default)]
    pub actor_user_id: Option<Uuid>,
    /// Inclusive lower bound (RFC 3339).
    #[serde(default)]
    pub from: Option<String>,
    /// Exclusive upper bound (RFC 3339).
    #[serde(default)]
    pub to: Option<String>,
}

fn audit_time(
    value: Option<&str>,
    name: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    value
        .map(|v| {
            chrono::DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|_| ApiError::validation(format!("{name} must be an RFC 3339 timestamp")))
        })
        .transpose()
}

/// One page of an organization's audit history with the query's filters (newest first).
async fn audit_page(state: &AppState, id: Uuid, q: &AuditQuery) -> ApiResult<Response> {
    let limit = page_limit(q.limit)?;
    if q.action.as_deref().is_some_and(|a| {
        a.is_empty()
            || a.len() > 64
            || !a
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')
    }) {
        return Err(ApiError::validation(
            "action must be an audit action such as membership.updated",
        ));
    }
    let from = audit_time(q.from.as_deref(), "from")?;
    let to = audit_time(q.to.as_deref(), "to")?;
    let rows: Vec<AuditRow> = db::all(
        &state.db,
        "SELECT seq, id, organization_id, actor_kind, actor_user_id, actor_service, action, target_type, target_id, summary, request_id, correlation_id, occurred_at \
         FROM organization_audit_events WHERE organization_id = $1 AND ($2::bigint IS NULL OR seq < $2) \
           AND ($4::text IS NULL OR action = $4) AND ($5::uuid IS NULL OR actor_user_id = $5) \
           AND ($6::timestamptz IS NULL OR occurred_at >= $6) AND ($7::timestamptz IS NULL OR occurred_at < $7) \
         ORDER BY seq DESC LIMIT $3",
        vec![
            id.into(),
            q.before.into(),
            (limit + 1).into(),
            q.action.clone().into(),
            q.actor_user_id.into(),
            from.into(),
            to.into(),
        ],
    )
    .await?;
    audit_items(&rows, limit)
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
    authorize(&state.db, id, user, Capability::AuditRead, false).await?;
    audit_page(&state, id, &q).await
}

/// `GET /internal/v1/admin/organizations/{id}/audit-events` — the operator's read of an
/// organization's audit history in any lifecycle state (`organization.administer`), with the
/// same filters and paging as the member read.
pub async fn admin_audit_events(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    audit_page(&state, id, &q).await
}

/// Query of the admin bridge's cross-organization audit read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAuditQuery {
    /// Only events with `seq` below this (the previous page's `nextCursor`).
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// Inclusive lower bound (RFC 3339).
    #[serde(default)]
    pub since: Option<String>,
    /// Exclusive upper bound (RFC 3339).
    #[serde(default)]
    pub until: Option<String>,
}

/// Longest `since`..`until` window of the cross-organization audit read.
pub const OPERATOR_AUDIT_MAX_WINDOW_DAYS: i64 = 31;

/// One page of the audit history of every organization (newest first, `seq` order),
/// for the admin bridge (`/internal/v1/operator/audit-events`).
pub(super) async fn operator_audit_page(
    state: &AppState,
    q: &OperatorAuditQuery,
) -> ApiResult<Response> {
    let limit = page_limit(q.limit)?;
    let since = audit_time(q.since.as_deref(), "since")?;
    let until = audit_time(q.until.as_deref(), "until")?;
    if let (Some(s), Some(u)) = (since, until) {
        if u <= s {
            return Err(ApiError::validation("until must be after since"));
        }
        if u - s > chrono::Duration::days(OPERATOR_AUDIT_MAX_WINDOW_DAYS) {
            return Err(ApiError::validation(
                "the since..until window must be at most 31 days",
            ));
        }
    }
    if q.cursor.is_some_and(|c| c < 1) {
        return Err(ApiError::validation("cursor must be a positive sequence"));
    }
    let rows: Vec<AuditRow> = db::all(
        &state.db,
        "SELECT seq, id, organization_id, actor_kind, actor_user_id, actor_service, action, target_type, target_id, summary, request_id, correlation_id, occurred_at          FROM organization_audit_events WHERE ($1::bigint IS NULL OR seq < $1)            AND ($3::timestamptz IS NULL OR occurred_at >= $3) AND ($4::timestamptz IS NULL OR occurred_at < $4)          ORDER BY seq DESC LIMIT $2",
        vec![
            q.cursor.into(),
            (limit + 1).into(),
            since.into(),
            until.into(),
        ],
    )
    .await?;
    audit_items(&rows, limit)
}

fn audit_items(rows: &[AuditRow], limit: i64) -> ApiResult<Response> {
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

#[derive(Debug, FromQueryResult)]
struct ExportDoc {
    doc: Value,
}

/// Most audit events an export carries (newest first).
pub const EXPORT_AUDIT_LIMIT: i64 = 10_000;

/// Gap audit G11: everything the Organizations Service holds about one organization, as
/// one JSON document: the organization and its profile, policies, memberships (with
/// history), teams and team memberships, invitations (never their token) and the newest
/// audit events. Identifiers only for people: names and emails of accounts live with
/// the Account service.
async fn export_document(state: &AppState, id: Uuid) -> ApiResult<Value> {
    let row: Option<ExportDoc> = db::one(
        &state.db,
        "SELECT jsonb_build_object( \
           'schemaVersion', 1, \
           'exportedAt', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
           'organization', (SELECT to_jsonb(o) FROM organizations o WHERE o.id = $1), \
           'profile', (SELECT to_jsonb(p) FROM organization_profiles p WHERE p.organization_id = $1), \
           'policies', (SELECT COALESCE(jsonb_agg(to_jsonb(x)), '[]'::jsonb) FROM organization_policies x WHERE x.organization_id = $1), \
           'memberships', (SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.created_at), '[]'::jsonb) FROM organization_memberships m WHERE m.organization_id = $1), \
           'membershipHistory', (SELECT COALESCE(jsonb_agg(to_jsonb(h)), '[]'::jsonb) FROM organization_membership_history h WHERE h.organization_id = $1), \
           'teams', (SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.created_at), '[]'::jsonb) FROM organization_teams t WHERE t.organization_id = $1), \
           'teamMemberships', (SELECT COALESCE(jsonb_agg(to_jsonb(tm)), '[]'::jsonb) FROM organization_team_members tm WHERE tm.organization_id = $1), \
           'invitations', (SELECT COALESCE(jsonb_agg(to_jsonb(i) - 'token_hash' ORDER BY i.created_at), '[]'::jsonb) FROM organization_invitations i WHERE i.organization_id = $1), \
           'auditEvents', (SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.seq DESC), '[]'::jsonb) FROM \
               (SELECT * FROM organization_audit_events WHERE organization_id = $1 ORDER BY seq DESC LIMIT $2) a) \
         ) AS doc",
        vec![id.into(), EXPORT_AUDIT_LIMIT.into()],
    )
    .await?;
    let doc = row.map(|r| r.doc).unwrap_or(Value::Null);
    if doc["organization"].is_null() {
        return Err(ApiError::new(ErrorCode::OrganizationNotFound));
    }
    Ok(doc)
}

/// `GET /internal/v1/organizations/{id}/export` — gap audit G11: the organization's data
/// for its OWNER (`organization.lifecycle.manage`). Audited.
pub async fn export(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let user = caller.user()?;
    authorize(&state.db, id, user, Capability::LifecycleManage, false).await?;
    let doc = export_document(&state, id).await?;
    let _ = user;
    store::audit(
        &state.db,
        &caller.as_user(),
        Utc::now(),
        Some(id),
        "organization.exported",
        "ORGANIZATION",
        Some(id),
        json!({}),
    )
    .await?;
    Ok(respond(StatusCode::OK, doc))
}

/// `GET /internal/v1/admin/organizations/{id}/export` — gap audit G11: the operator's
/// export of an organization in any lifecycle state, archived included
/// (`organization.administer`). Audited.
pub async fn admin_export(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    store::org(&state.db, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let doc = export_document(&state, id).await?;
    store::audit(
        &state.db,
        &caller.as_operator(),
        Utc::now(),
        Some(id),
        "organization.exported",
        "ORGANIZATION",
        Some(id),
        json!({ "byOperator": true }),
    )
    .await?;
    Ok(respond(StatusCode::OK, doc))
}
