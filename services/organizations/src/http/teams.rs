//! Teams (Blueprint Section 6): organization-scoped, flat by default with an optional
//! bounded parent (same organization, no cycles, depth ≤ [`MAX_DEPTH`]). Users join teams
//! through their ACTIVE organization membership and may be in several teams.
//!
//! * OWNER / ADMIN create, change and archive any team and assign TEAM_MANAGER (only to
//!   an organization MANAGER, ADMIN or OWNER).
//! * A MANAGER manages only the teams they are TEAM_MANAGER of: add / remove
//!   TEAM_MEMBERs, rename, step down. They create teams only when the policy allows it
//!   (`delegatedAdministration.managersMayCreateTeams`), becoming the new team's manager.
//!   Nobody raises their own team role.
//! * Anyone leaves a team themselves.
//!
//! Every change locks the organization row first, bumps the aggregate version, appends
//! team-member history (append-only) and writes the audit event and outbox event in the
//! same transaction.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    authorize, membership_not_found, middleware::Caller, page_limit, parse, respond, Access,
};
use crate::{
    db,
    domain::{Capability, LifecycleStatus, MembershipStatus, Role, TeamRole},
    error::{ApiError, ApiResult, ErrorCode},
    idempotency,
    state::AppState,
    store::{self, rfc3339, Event, OpContext},
};

/// Maximum team nesting depth (a root team has depth 1).
pub const MAX_DEPTH: i64 = 5;
/// ACTIVE teams per organization.
pub const MAX_TEAMS: i64 = 500;
/// ACTIVE members per team.
pub const MAX_TEAM_MEMBERS: i64 = 2000;

pub const TEAM_COLUMNS: &str = "id, organization_id, name, description, parent_team_id, status, created_by, created_at, updated_at, archived_at, version";

#[derive(Debug, Clone, FromQueryResult)]
pub struct TeamRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub parent_team_id: Option<Uuid>,
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl TeamRow {
    pub fn to_json(&self, member_count: i64) -> Value {
        json!({
            "id": self.id,
            "organizationId": self.organization_id,
            "name": self.name,
            "description": self.description,
            "parentTeamId": self.parent_team_id,
            "status": self.status,
            "createdBy": self.created_by,
            "createdAt": rfc3339(self.created_at),
            "updatedAt": rfc3339(self.updated_at),
            "archivedAt": self.archived_at.map(rfc3339),
            "version": self.version,
            "memberCount": member_count,
        })
    }

    fn event(&self, event_type: &'static str, payload: Value) -> Event<'static> {
        Event {
            organization_id: self.organization_id,
            aggregate_type: "team",
            aggregate_id: self.id,
            aggregate_version: self.version,
            event_type,
            payload,
        }
    }
}

pub const TEAM_MEMBER_COLUMNS: &str = "id, organization_id, team_id, membership_id, user_id, role, status, added_by, created_at, updated_at, removed_at, version";

#[derive(Debug, Clone, FromQueryResult)]
pub struct TeamMemberRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub team_id: Uuid,
    pub membership_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    pub added_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl TeamMemberRow {
    pub fn role(&self) -> TeamRole {
        TeamRole::parse(&self.role).unwrap_or(TeamRole::TeamMember)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "organizationId": self.organization_id,
            "teamId": self.team_id,
            "membershipId": self.membership_id,
            "userId": self.user_id,
            "role": self.role,
            "status": self.status,
            "addedBy": self.added_by,
            "createdAt": rfc3339(self.created_at),
            "updatedAt": rfc3339(self.updated_at),
            "removedAt": self.removed_at.map(rfc3339),
            "version": self.version,
        })
    }

    fn event(&self, event_type: &'static str, payload: Value) -> Event<'static> {
        Event {
            organization_id: self.organization_id,
            aggregate_type: "team_member",
            aggregate_id: self.id,
            aggregate_version: self.version,
            event_type,
            payload,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Loaders and writers
// ---------------------------------------------------------------------------------------

async fn team<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<TeamRow>, DbErr> {
    db::one(
        c,
        &format!(
            "SELECT {TEAM_COLUMNS} FROM organization_teams WHERE organization_id = $1 AND id = $2{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![organization.into(), id.into()],
    )
    .await
}

async fn member_count<C: ConnectionTrait>(c: &C, team: Uuid) -> Result<i64, DbErr> {
    db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organization_team_members WHERE team_id = $1 AND status = 'ACTIVE'",
        vec![team.into()],
    )
    .await
}

/// The ACTIVE team role of `membership` in an ACTIVE `team` of `organization`.
pub async fn team_role_of<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    team: Uuid,
    membership: Uuid,
) -> Result<Option<TeamRole>, DbErr> {
    #[derive(FromQueryResult)]
    struct R {
        role: String,
    }
    let row: Option<R> = db::one(
        c,
        "SELECT tm.role FROM organization_team_members tm JOIN organization_teams t ON t.id = tm.team_id AND t.status = 'ACTIVE' \
         WHERE tm.organization_id = $1 AND tm.team_id = $2 AND tm.membership_id = $3 AND tm.status = 'ACTIVE'",
        vec![organization.into(), team.into(), membership.into()],
    )
    .await?;
    Ok(row.and_then(|r| TeamRole::parse(&r.role)))
}

/// Depth of `team` (root = 1), following parents.
async fn depth<C: ConnectionTrait>(c: &C, team: Uuid) -> Result<i64, DbErr> {
    db::scalar_i64(
        c,
        "WITH RECURSIVE up(id, parent, d) AS ( \
             SELECT id, parent_team_id, 1 FROM organization_teams WHERE id = $1 \
             UNION ALL SELECT t.id, t.parent_team_id, up.d + 1 FROM organization_teams t JOIN up ON t.id = up.parent WHERE up.d < 64) \
         SELECT max(d)::bigint AS n FROM up",
        vec![team.into()],
    )
    .await
}

/// Height of the ACTIVE subtree rooted at `team` (a leaf = 1).
async fn height<C: ConnectionTrait>(c: &C, team: Uuid) -> Result<i64, DbErr> {
    db::scalar_i64(
        c,
        "WITH RECURSIVE down(id, d) AS ( \
             SELECT id, 1 FROM organization_teams WHERE id = $1 \
             UNION ALL SELECT t.id, down.d + 1 FROM organization_teams t JOIN down ON t.parent_team_id = down.id WHERE t.status = 'ACTIVE' AND down.d < 64) \
         SELECT max(d)::bigint AS n FROM down",
        vec![team.into()],
    )
    .await
}

/// Whether `ancestor` is `team` or one of its ancestors.
async fn is_ancestor_or_self<C: ConnectionTrait>(
    c: &C,
    team: Uuid,
    ancestor: Uuid,
) -> Result<bool, DbErr> {
    Ok(db::scalar_i64(
        c,
        "WITH RECURSIVE up(id, parent, d) AS ( \
             SELECT id, parent_team_id, 1 FROM organization_teams WHERE id = $1 \
             UNION ALL SELECT t.id, t.parent_team_id, up.d + 1 FROM organization_teams t JOIN up ON t.id = up.parent WHERE up.d < 64) \
         SELECT count(*)::bigint AS n FROM up WHERE id = $2",
        vec![team.into(), ancestor.into()],
    )
    .await?
        > 0)
}

/// Validates a parent for `team` (`None` for a new team): same organization, ACTIVE, no
/// cycle, resulting depth ≤ [`MAX_DEPTH`].
async fn check_parent<C: ConnectionTrait>(
    c: &C,
    organization: Uuid,
    team_id: Option<Uuid>,
    parent: Uuid,
) -> Result<(), ApiError> {
    let p = team(c, organization, parent, false).await?.ok_or_else(|| {
        ApiError::with_message(ErrorCode::TeamNotFound, "The parent team was not found.")
    })?;
    if p.status != "ACTIVE" {
        return Err(ApiError::with_message(
            ErrorCode::TeamArchived,
            "The parent team is archived.",
        ));
    }
    let own_height = match team_id {
        Some(t) => {
            if is_ancestor_or_self(c, parent, t).await? {
                return Err(ApiError::validation(
                    "parentTeamId would create a cycle in the team hierarchy",
                ));
            }
            height(c, t).await?
        }
        None => 1,
    };
    if depth(c, parent).await? + own_height > MAX_DEPTH {
        return Err(ApiError::validation(format!(
            "teams nest at most {MAX_DEPTH} levels deep"
        )));
    }
    Ok(())
}

fn team_name(value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    let n = v.chars().count();
    if n == 0 || n > 120 || v.chars().any(char::is_control) {
        return Err(ApiError::validation(
            "name must be 1-120 characters without control characters",
        ));
    }
    Ok(v.to_string())
}

fn description(value: Option<&str>) -> Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if v.chars().count() <= 500 && !v.chars().any(|c| c.is_control() && c != '\n') => {
            Ok(Some(v.to_string()))
        }
        Some(_) => Err(ApiError::validation(
            "description must be at most 500 characters",
        )),
    }
}

fn name_taken(e: DbErr) -> ApiError {
    if db::is_unique_violation(&e, "uq_organization_teams_active_name") {
        ApiError::new(ErrorCode::TeamNameTaken)
    } else {
        e.into()
    }
}

async fn team_history<C: ConnectionTrait>(
    c: &C,
    ctx: &OpContext,
    now: DateTime<Utc>,
    before: Option<&TeamMemberRow>,
    after: &TeamMemberRow,
    kind: &str,
) -> Result<(), DbErr> {
    db::exec(
        c,
        "INSERT INTO organization_team_member_history (id, team_member_id, team_id, organization_id, user_id, change_kind, from_role, to_role, from_status, to_status, team_member_version, actor_kind, actor_user_id, actor_service, request_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        vec![
            Uuid::new_v4().into(),
            after.id.into(),
            after.team_id.into(),
            after.organization_id.into(),
            after.user_id.into(),
            kind.into(),
            before.map(|b| b.role.clone()).into(),
            after.role.clone().into(),
            before.map(|b| b.status.clone()).into(),
            after.status.clone().into(),
            after.version.into(),
            ctx.kind.as_str().into(),
            ctx.user_id.into(),
            ctx.service.clone().into(),
            ctx.request_id.clone().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn insert_member(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    team: &TeamRow,
    membership: &store::MembershipRow,
    role: TeamRole,
) -> Result<TeamMemberRow, ApiError> {
    let row: TeamMemberRow = db::one(
        txn,
        &format!(
            "INSERT INTO organization_team_members (id, organization_id, team_id, membership_id, user_id, role, status, added_by, created_at, updated_at, version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, $8, 1) RETURNING {TEAM_MEMBER_COLUMNS}"
        ),
        vec![
            Uuid::new_v4().into(),
            team.organization_id.into(),
            team.id.into(),
            membership.id.into(),
            membership.user_id.into(),
            role.as_str().into(),
            ctx.user_id.into(),
            now.into(),
        ],
    )
    .await?
    .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
    team_history(txn, ctx, now, None, &row, "ADDED").await?;
    store::emit(
        txn,
        ctx,
        now,
        row.event(
            "team.member_added",
            json!({ "teamId": row.team_id, "userId": row.user_id, "role": row.role, "teamMemberVersion": row.version }),
        ),
    )
    .await?;
    store::audit(
        txn,
        ctx,
        now,
        Some(team.organization_id),
        "team.member_added",
        "team",
        Some(team.id),
        json!({ "teamMemberId": row.id, "userId": row.user_id, "role": row.role }),
    )
    .await?;
    Ok(row)
}

/// Marks an ACTIVE team membership REMOVED (history `kind`, `team.member_removed`).
async fn remove_member(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    before: &TeamMemberRow,
    kind: &str,
) -> Result<TeamMemberRow, ApiError> {
    let after: TeamMemberRow = db::one(
        txn,
        &format!("UPDATE organization_team_members SET status = 'REMOVED', removed_at = $2, updated_at = $2, version = version + 1 WHERE id = $1 RETURNING {TEAM_MEMBER_COLUMNS}"),
        vec![before.id.into(), now.into()],
    )
    .await?
    .ok_or_else(team_member_not_found)?;
    team_history(txn, ctx, now, Some(before), &after, kind).await?;
    store::emit(
        txn,
        ctx,
        now,
        after.event(
            "team.member_removed",
            json!({ "teamId": after.team_id, "userId": after.user_id, "reason": kind, "teamMemberVersion": after.version }),
        ),
    )
    .await?;
    store::audit(
        txn,
        ctx,
        now,
        Some(after.organization_id),
        "team.member_removed",
        "team",
        Some(after.team_id),
        json!({ "teamMemberId": after.id, "userId": after.user_id, "reason": kind }),
    )
    .await?;
    Ok(after)
}

/// Ends every ACTIVE team membership of an organization membership that was removed
/// (called in the membership change's own transaction).
pub async fn end_team_memberships(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    membership: Uuid,
) -> Result<(), ApiError> {
    let rows: Vec<TeamMemberRow> = db::all(
        txn,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE membership_id = $1 AND status = 'ACTIVE' ORDER BY id FOR UPDATE"),
        vec![membership.into()],
    )
    .await?;
    for r in &rows {
        remove_member(txn, ctx, now, r, "MEMBERSHIP_ENDED").await?;
    }
    Ok(())
}

fn team_not_found() -> ApiError {
    ApiError::new(ErrorCode::TeamNotFound)
}

fn team_member_not_found() -> ApiError {
    ApiError::with_message(ErrorCode::TeamNotFound, "The team member was not found.")
}

fn active(team: &TeamRow) -> Result<(), ApiError> {
    if team.status == "ACTIVE" {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::TeamArchived))
    }
}

/// Who manages `team`: OWNER / ADMIN always; a MANAGER only as its TEAM_MANAGER.
async fn manage_access(
    txn: &DatabaseTransaction,
    organization: Uuid,
    user: Uuid,
    team_id: Uuid,
) -> Result<(Access, TeamRow, Option<TeamRole>), ApiError> {
    let access = authorize(txn, organization, user, Capability::TeamsManage, true).await?;
    let t = team(txn, organization, team_id, true)
        .await?
        .ok_or_else(team_not_found)?;
    let role = team_role_of(txn, organization, team_id, access.membership.id).await?;
    if access.membership.role() == Role::Manager && role != Some(TeamRole::TeamManager) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    Ok((access, t, role))
}

/// Whether `access` manages `team` now: `organization.teams.manage` under the policy and,
/// for a MANAGER, TEAM_MANAGER of that team.
async fn manages(txn: &DatabaseTransaction, access: &Access, team: Uuid) -> Result<bool, ApiError> {
    let policy = store::policy(txn, access.org.id).await?;
    let m = &access.membership;
    if !crate::domain::evaluate_with_policy(
        Some((m.role(), m.status())),
        access.org.status(),
        Capability::TeamsManage,
        &policy,
    )
    .allowed()
    {
        return Ok(false);
    }
    if m.role() == Role::Manager {
        return Ok(
            team_role_of(txn, access.org.id, team, m.id).await? == Some(TeamRole::TeamManager)
        );
    }
    Ok(true)
}

fn is_admin(access: &Access) -> bool {
    matches!(access.membership.role(), Role::Owner | Role::Admin)
}

/// Reads need `organization.members.read` for an actor (policy-aware); a service read
/// (no actor) gets the authoritative state.
async fn read_access(
    state: &AppState,
    caller: &Caller,
    organization: Uuid,
) -> Result<(), ApiError> {
    match caller.acting_user {
        Some(user) => {
            authorize(
                &state.db,
                organization,
                user,
                Capability::MembersRead,
                false,
            )
            .await?;
        }
        None => {
            store::org(&state.db, organization, false)
                .await?
                .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBody {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parent_team_id: Option<Uuid>,
}

/// `POST /internal/v1/organizations/{id}/teams`.
pub async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, raw): (CreateBody, Value) = parse(&body)?;
    let name = team_name(&req.name)?;
    let description = description(req.description.as_deref())?;
    let idem = idempotency::from_headers(
        &headers,
        format!("team.create:{}:{user}:{id}", caller.service()),
        &raw,
    )?;
    let txn = state.db.begin().await?;
    if let Some(i) = &idem {
        if let Some(stored) = idempotency::begin(&txn, i).await? {
            return Ok(respond(
                StatusCode::from_u16(stored.response_status as u16).unwrap_or(StatusCode::CREATED),
                stored.response_body,
            ));
        }
    }
    let access = authorize(&txn, id, user, Capability::TeamsManage, true).await?;
    let manager = access.membership.role() == Role::Manager;
    if manager {
        let policy = store::policy(&txn, id).await?;
        if !policy.delegated_administration.managers_may_create_teams {
            return Err(ApiError::new(ErrorCode::PolicyRestricted));
        }
        if req.parent_team_id.is_some() {
            // The hierarchy is an OWNER / ADMIN decision.
            return Err(ApiError::new(ErrorCode::PermissionDenied));
        }
    }
    let count = db::scalar_i64(
        &txn,
        "SELECT count(*)::bigint AS n FROM organization_teams WHERE organization_id = $1 AND status = 'ACTIVE'",
        vec![id.into()],
    )
    .await?;
    if count >= MAX_TEAMS {
        return Err(ApiError::validation(format!(
            "an organization has at most {MAX_TEAMS} active teams"
        )));
    }
    if let Some(parent) = req.parent_team_id {
        check_parent(&txn, id, None, parent).await?;
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let t: TeamRow = db::one(
        &txn,
        &format!(
            "INSERT INTO organization_teams (id, organization_id, name, description, parent_team_id, status, created_by, created_at, updated_at, version) \
             VALUES ($1, $2, $3, $4, $5, 'ACTIVE', $6, $7, $7, 1) RETURNING {TEAM_COLUMNS}"
        ),
        vec![
            Uuid::new_v4().into(),
            id.into(),
            name.into(),
            description.into(),
            req.parent_team_id.into(),
            user.into(),
            now.into(),
        ],
    )
    .await
    .map_err(name_taken)?
    .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
    store::emit(
        &txn,
        &ctx,
        now,
        t.event(
            "team.created",
            json!({ "teamId": t.id, "parentTeamId": t.parent_team_id, "teamVersion": t.version }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "team.created",
        "team",
        Some(t.id),
        json!({ "parentTeamId": t.parent_team_id }),
    )
    .await?;
    let mut members = 0;
    if manager {
        insert_member(
            &txn,
            &ctx,
            now,
            &t,
            &access.membership,
            TeamRole::TeamManager,
        )
        .await?;
        members = 1;
    }
    let doc = t.to_json(members);
    if let Some(i) = &idem {
        idempotency::finish(&txn, i, 201, &doc, Some(t.id), now).await?;
    }
    txn.commit().await?;
    Ok(respond(StatusCode::CREATED, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub cursor: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, FromQueryResult)]
struct TeamWithCount {
    id: Uuid,
    organization_id: Uuid,
    name: String,
    description: Option<String>,
    parent_team_id: Option<Uuid>,
    status: String,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
    version: i64,
    member_count: i64,
}

/// `GET /internal/v1/organizations/{id}/teams?status&cursor&limit` (id order).
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let limit = page_limit(q.limit)?;
    let status = q.status.as_deref().unwrap_or("ACTIVE");
    if !matches!(status, "ACTIVE" | "ARCHIVED") {
        return Err(ApiError::validation("status must be ACTIVE or ARCHIVED"));
    }
    read_access(&state, &caller, id).await?;
    let rows: Vec<TeamWithCount> = db::all(
        &state.db,
        "SELECT t.id, t.organization_id, t.name, t.description, t.parent_team_id, t.status, t.created_by, t.created_at, t.updated_at, t.archived_at, t.version, \
                (SELECT count(*) FROM organization_team_members m WHERE m.team_id = t.id AND m.status = 'ACTIVE')::bigint AS member_count \
         FROM organization_teams t WHERE t.organization_id = $1 AND t.status = $2 AND ($3::uuid IS NULL OR t.id > $3) ORDER BY t.id LIMIT $4",
        vec![id.into(), status.into(), q.cursor.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            TeamRow {
                id: r.id,
                organization_id: r.organization_id,
                name: r.name.clone(),
                description: r.description.clone(),
                parent_team_id: r.parent_team_id,
                status: r.status.clone(),
                created_by: r.created_by,
                created_at: r.created_at,
                updated_at: r.updated_at,
                archived_at: r.archived_at,
                version: r.version,
            }
            .to_json(r.member_count)
        })
        .collect();
    let next = if more {
        rows.get(limit as usize - 1).map(|r| r.id)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}

/// `GET /internal/v1/organizations/{id}/teams/{teamId}`.
pub async fn detail(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    read_access(&state, &caller, id).await?;
    let t = team(&state.db, id, team_id, false)
        .await?
        .ok_or_else(team_not_found)?;
    let n = member_count(&state.db, t.id).await?;
    Ok(respond(StatusCode::OK, t.to_json(n)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBody {
    pub expected_version: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `null` detaches from the parent; absent leaves it unchanged.
    #[serde(default)]
    pub parent_team_id: Option<Uuid>,
}

/// `PATCH /internal/v1/organizations/{id}/teams/{teamId}`.
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, raw): (UpdateBody, Value) = parse(&body)?;
    let parent_change: Option<Option<Uuid>> = raw.get("parentTeamId").map(|_| req.parent_team_id);
    let txn = state.db.begin().await?;
    let (access, t, _) = manage_access(&txn, id, user, team_id).await?;
    active(&t)?;
    if t.version != req.expected_version {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let mut changed: Vec<&'static str> = Vec::new();
    let mut name = t.name.clone();
    if let Some(n) = &req.name {
        let n = team_name(n)?;
        if n != name {
            name = n;
            changed.push("name");
        }
    }
    let mut desc = t.description.clone();
    if raw.get("description").is_some() {
        let d = description(req.description.as_deref())?;
        if d != desc {
            desc = d;
            changed.push("description");
        }
    }
    let mut parent = t.parent_team_id;
    if let Some(p) = parent_change {
        if p != parent {
            if !is_admin(&access) {
                return Err(ApiError::new(ErrorCode::PermissionDenied));
            }
            if let Some(p) = p {
                check_parent(&txn, id, Some(t.id), p).await?;
            }
            parent = p;
            changed.push("parentTeamId");
        }
    }
    if changed.is_empty() {
        let n = member_count(&txn, t.id).await?;
        return Ok(respond(StatusCode::OK, t.to_json(n)));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let after: TeamRow = db::one(
        &txn,
        &format!("UPDATE organization_teams SET name = $2, description = $3, parent_team_id = $4, version = version + 1, updated_at = $5 WHERE id = $1 RETURNING {TEAM_COLUMNS}"),
        vec![t.id.into(), name.into(), desc.into(), parent.into(), now.into()],
    )
    .await
    .map_err(name_taken)?
    .ok_or_else(team_not_found)?;
    store::emit(
        &txn,
        &ctx,
        now,
        after.event(
            "team.updated",
            json!({ "teamId": after.id, "changedFields": changed, "parentTeamId": after.parent_team_id, "teamVersion": after.version }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "team.updated",
        "team",
        Some(after.id),
        json!({ "changedFields": changed }),
    )
    .await?;
    let n = member_count(&txn, after.id).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json(n)))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveBody {
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// `POST /internal/v1/organizations/{id}/teams/{teamId}/archive` — OWNER / ADMIN;
/// idempotent; refused while ACTIVE child teams exist. Every ACTIVE team membership ends.
pub async fn archive(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (ArchiveBody, Value) = parse(&body)?;
    let txn = state.db.begin().await?;
    let (access, t, _) = manage_access(&txn, id, user, team_id).await?;
    if !is_admin(&access) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    if req.expected_version.is_some_and(|v| v != t.version) {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    if t.status == "ARCHIVED" {
        return Ok(respond(StatusCode::OK, t.to_json(0)));
    }
    let children = db::scalar_i64(
        &txn,
        "SELECT count(*)::bigint AS n FROM organization_teams WHERE organization_id = $1 AND parent_team_id = $2 AND status = 'ACTIVE'",
        vec![id.into(), t.id.into()],
    )
    .await?;
    if children > 0 {
        return Err(ApiError::validation(
            "archive or move the team's child teams first",
        ));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let members: Vec<TeamMemberRow> = db::all(
        &txn,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE team_id = $1 AND status = 'ACTIVE' ORDER BY id FOR UPDATE"),
        vec![t.id.into()],
    )
    .await?;
    for m in &members {
        remove_member(&txn, &ctx, now, m, "TEAM_ARCHIVED").await?;
    }
    let after: TeamRow = db::one(
        &txn,
        &format!("UPDATE organization_teams SET status = 'ARCHIVED', archived_at = $2, version = version + 1, updated_at = $2 WHERE id = $1 RETURNING {TEAM_COLUMNS}"),
        vec![t.id.into(), now.into()],
    )
    .await?
    .ok_or_else(team_not_found)?;
    store::emit(
        &txn,
        &ctx,
        now,
        after.event(
            "team.archived",
            json!({ "teamId": after.id, "teamVersion": after.version }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "team.archived",
        "team",
        Some(after.id),
        json!({ "membersRemoved": members.len() }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json(0)))
}

// ---------------------------------------------------------------------------------------
// Team members
// ---------------------------------------------------------------------------------------

/// `GET /internal/v1/organizations/{id}/teams/{teamId}/members?cursor&limit` — ACTIVE
/// team memberships (id order).
pub async fn members(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<super::members::ListQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let limit = page_limit(q.limit)?;
    read_access(&state, &caller, id).await?;
    team(&state.db, id, team_id, false)
        .await?
        .ok_or_else(team_not_found)?;
    let rows: Vec<TeamMemberRow> = db::all(
        &state.db,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE team_id = $1 AND status = 'ACTIVE' AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3"),
        vec![team_id.into(), q.cursor.into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(TeamMemberRow::to_json)
        .collect();
    let next = if more {
        rows.get(limit as usize - 1).map(|r| r.id)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddBody {
    #[serde(default)]
    pub membership_id: Option<Uuid>,
    #[serde(default)]
    pub user_id: Option<Uuid>,
    pub role: String,
}

fn team_role(value: &str) -> Result<TeamRole, ApiError> {
    TeamRole::parse(value)
        .ok_or_else(|| ApiError::validation("role must be TEAM_MANAGER or TEAM_MEMBER"))
}

/// TEAM_MANAGER is held only by organization MANAGERs, ADMINs and OWNERs.
fn can_hold(role: TeamRole, org_role: Role) -> bool {
    role == TeamRole::TeamMember || org_role.rank() >= Role::Manager.rank()
}

/// `POST /internal/v1/organizations/{id}/teams/{teamId}/members` `{membershipId | userId,
/// role}` — only ACTIVE organization members join; a team manager adds TEAM_MEMBERs only.
pub async fn add_member(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id)): Path<(Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (AddBody, Value) = parse(&body)?;
    let role = team_role(&req.role)?;
    let txn = state.db.begin().await?;
    let (access, t, _) = manage_access(&txn, id, user, team_id).await?;
    active(&t)?;
    if role == TeamRole::TeamManager && !is_admin(&access) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let target = match (req.membership_id, req.user_id) {
        (Some(m), None) => store::membership(&txn, id, m, true).await?,
        (None, Some(u)) => store::current_membership(&txn, id, u, true).await?,
        _ => {
            return Err(ApiError::validation(
                "exactly one of membershipId and userId is required",
            ))
        }
    }
    .filter(|m| m.status() != MembershipStatus::Removed)
    .ok_or_else(membership_not_found)?;
    if target.status() != MembershipStatus::Active {
        return Err(ApiError::validation(
            "only ACTIVE organization members can join a team",
        ));
    }
    if !can_hold(role, target.role()) {
        return Err(ApiError::validation(
            "TEAM_MANAGER is held only by organization MANAGERs, ADMINs and OWNERs",
        ));
    }
    let existing: Option<TeamMemberRow> = db::one(
        &txn,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE team_id = $1 AND membership_id = $2 AND status = 'ACTIVE' FOR UPDATE"),
        vec![t.id.into(), target.id.into()],
    )
    .await?;
    if let Some(e) = existing {
        if e.role() == role {
            return Ok(respond(StatusCode::OK, e.to_json()));
        }
        return Err(ApiError::validation(
            "already a member of the team; change the role with PATCH",
        ));
    }
    if member_count(&txn, t.id).await? >= MAX_TEAM_MEMBERS {
        return Err(ApiError::validation(format!(
            "a team has at most {MAX_TEAM_MEMBERS} active members"
        )));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let row = insert_member(&txn, &ctx, now, &t, &target, role).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::CREATED, row.to_json()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberUpdateBody {
    pub expected_version: i64,
    pub role: String,
}

async fn team_member(
    txn: &DatabaseTransaction,
    team_id: Uuid,
    id: Uuid,
) -> Result<TeamMemberRow, ApiError> {
    db::one(
        txn,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE team_id = $1 AND id = $2 AND status = 'ACTIVE' FOR UPDATE"),
        vec![team_id.into(), id.into()],
    )
    .await?
    .ok_or_else(team_member_not_found)
}

/// `PATCH /internal/v1/organizations/{id}/teams/{teamId}/members/{teamMemberId}`
/// `{expectedVersion, role}` — OWNER / ADMIN assign either role; a team manager only steps
/// down (own row → TEAM_MEMBER). Nobody raises their own team role.
pub async fn update_member(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id, member_id)): Path<(Uuid, Uuid, Uuid)>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (MemberUpdateBody, Value) = parse(&body)?;
    let role = team_role(&req.role)?;
    let txn = state.db.begin().await?;
    let (access, t, _) = manage_access(&txn, id, user, team_id).await?;
    active(&t)?;
    let target = team_member(&txn, t.id, member_id).await?;
    if target.version != req.expected_version {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    if target.role() == role {
        return Ok(respond(StatusCode::OK, target.to_json()));
    }
    let own = target.membership_id == access.membership.id;
    let allowed = if is_admin(&access) {
        // Admins already manage every team; raising oneself adds nothing but is refused
        // for symmetry (no self-elevation).
        !(own && role == TeamRole::TeamManager)
    } else {
        own && role == TeamRole::TeamMember
    };
    if !allowed {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let membership = store::membership(&txn, id, target.membership_id, false)
        .await?
        .ok_or_else(membership_not_found)?;
    if !can_hold(role, membership.role()) {
        return Err(ApiError::validation(
            "TEAM_MANAGER is held only by organization MANAGERs, ADMINs and OWNERs",
        ));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let after: TeamMemberRow = db::one(
        &txn,
        &format!("UPDATE organization_team_members SET role = $2, version = version + 1, updated_at = $3 WHERE id = $1 RETURNING {TEAM_MEMBER_COLUMNS}"),
        vec![target.id.into(), role.as_str().into(), now.into()],
    )
    .await?
    .ok_or_else(team_member_not_found)?;
    team_history(&txn, &ctx, now, Some(&target), &after, "ROLE_CHANGED").await?;
    store::emit(
        &txn,
        &ctx,
        now,
        after.event(
            "team.member_role_changed",
            json!({ "teamId": after.team_id, "userId": after.user_id, "fromRole": target.role, "toRole": after.role, "teamMemberVersion": after.version }),
        ),
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "team.member_role_changed",
        "team",
        Some(t.id),
        json!({ "teamMemberId": after.id, "userId": after.user_id, "fromRole": target.role, "toRole": after.role }),
    )
    .await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json()))
}

/// `POST /internal/v1/organizations/{id}/teams/{teamId}/members/{teamMemberId}/remove` —
/// OWNER / ADMIN remove anyone, a team manager removes TEAM_MEMBERs, everyone leaves
/// themselves. Idempotent.
pub async fn remove(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, team_id, member_id)): Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::OrganizationRead, true).await?;
    match access.org.status() {
        LifecycleStatus::Active => {}
        LifecycleStatus::Suspended => return Err(ApiError::new(ErrorCode::OrganizationSuspended)),
        LifecycleStatus::Archived => return Err(ApiError::new(ErrorCode::OrganizationArchived)),
    }
    let t = team(&txn, id, team_id, true)
        .await?
        .ok_or_else(team_not_found)?;
    let target: Option<TeamMemberRow> = db::one(
        &txn,
        &format!("SELECT {TEAM_MEMBER_COLUMNS} FROM organization_team_members WHERE team_id = $1 AND id = $2 FOR UPDATE"),
        vec![t.id.into(), member_id.into()],
    )
    .await?;
    let target = target.ok_or_else(team_member_not_found)?;
    if target.status == "REMOVED" {
        return Ok(respond(StatusCode::OK, target.to_json()));
    }
    let own = target.membership_id == access.membership.id;
    if !own {
        let manages = manages(&txn, &access, t.id).await?;
        let allowed = match access.membership.role() {
            Role::Owner | Role::Admin => manages,
            Role::Manager => manages && target.role() == TeamRole::TeamMember,
            _ => false,
        };
        if !allowed {
            return Err(ApiError::new(ErrorCode::PermissionDenied));
        }
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let after = remove_member(&txn, &ctx, now, &target, "REMOVED").await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, after.to_json()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_names_and_descriptions_are_bounded() {
        assert_eq!(team_name("  Platform  ").unwrap(), "Platform");
        assert!(team_name("").is_err());
        assert!(team_name(&"x".repeat(121)).is_err());
        assert!(team_name("bad\u{0}name").is_err());
        assert_eq!(description(Some("  ")).unwrap(), None);
        assert!(description(Some(&"x".repeat(501))).is_err());
    }

    #[test]
    fn team_managers_are_organization_managers_or_above() {
        assert!(can_hold(TeamRole::TeamManager, Role::Manager));
        assert!(can_hold(TeamRole::TeamManager, Role::Owner));
        assert!(!can_hold(TeamRole::TeamManager, Role::Member));
        assert!(!can_hold(TeamRole::TeamManager, Role::Guest));
        assert!(can_hold(TeamRole::TeamMember, Role::Guest));
    }
}
