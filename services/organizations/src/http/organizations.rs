//! Organization create / read / update, lifecycle, the user's organizations, provisioning
//! state and operator administration.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{authorize, middleware::Caller, page_limit, parse, respond};
use crate::{
    db,
    domain::{effective_capabilities_with_policy, Capability, LifecycleStatus, Role},
    error::{ApiError, ApiResult, ErrorCode},
    idempotency, provisioning,
    state::AppState,
    store::{self, Event, MembershipRow, OpContext, OrgRow, MEMBERSHIP_COLUMNS, ORG_COLUMNS},
    validation::{self, ProfileInput, ProfileValues},
};

/// Self-service creations allowed per acting user in 24 hours (contract §4).
pub const CREATION_LIMIT_PER_DAY: i64 = 5;

fn replay(stored: idempotency::Stored) -> Response {
    respond(
        StatusCode::from_u16(stored.response_status as u16).unwrap_or(StatusCode::OK),
        stored.response_body,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBody {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub profile: Option<ProfileInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminCreateBody {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub profile: Option<ProfileInput>,
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
}

pub struct NewOrganization {
    pub name: String,
    pub slug: String,
    pub profile: ProfileValues,
    pub creation_source: &'static str,
    pub created_by: Option<Uuid>,
    /// The first OWNER and how it joined.
    pub owner: Option<(Uuid, &'static str)>,
}

fn slug_taken(e: sea_orm::DbErr) -> ApiError {
    if db::is_unique_violation(&e, "uq_organizations_slug") {
        ApiError::new(ErrorCode::SlugTaken)
    } else {
        e.into()
    }
}

async fn slug_free<C: ConnectionTrait>(
    c: &C,
    slug: &str,
    except: Option<Uuid>,
) -> Result<(), ApiError> {
    let n = db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organizations WHERE slug = $1 AND ($2::uuid IS NULL OR id <> $2)",
        vec![slug.into(), except.into()],
    )
    .await?;
    if n > 0 {
        Err(ApiError::new(ErrorCode::SlugTaken))
    } else {
        Ok(())
    }
}

/// Creates an organization (+ profile, + first OWNER) with audit and outbox events.
pub async fn create_organization(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    now: DateTime<Utc>,
    new: NewOrganization,
) -> Result<(OrgRow, Option<MembershipRow>), ApiError> {
    slug_free(txn, &new.slug, None).await?;
    let id = Uuid::new_v4();
    let org: OrgRow = db::one(
        txn,
        &format!(
            "INSERT INTO organizations (id, name, slug, lifecycle_status, provisioning_state, creation_source, created_by, created_at, updated_at, version) \
             VALUES ($1, $2, $3, 'ACTIVE', 'PENDING', $4, $5, $6, $6, 1) RETURNING {ORG_COLUMNS}"
        ),
        vec![
            id.into(),
            new.name.clone().into(),
            new.slug.clone().into(),
            new.creation_source.into(),
            new.created_by.into(),
            now.into(),
        ],
    )
    .await
    .map_err(slug_taken)?
    .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
    store::save_profile(txn, id, &new.profile, now).await?;
    store::emit(
        txn,
        ctx,
        now,
        Event {
            organization_id: id,
            aggregate_type: "organization",
            aggregate_id: id,
            aggregate_version: org.version,
            event_type: "organization.created",
            payload: json!({ "slug": org.slug, "lifecycleStatus": org.lifecycle_status, "provisioningState": org.provisioning_state, "creationSource": org.creation_source }),
        },
    )
    .await?;
    let mut owner_row = None;
    if let Some((user, joined_via)) = new.owner {
        let m: MembershipRow = db::one(
            txn,
            &format!(
                "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_by, created_at, updated_at, version) \
                 VALUES ($1, $2, $3, 'OWNER', 'ACTIVE', $4, $5, $6, $6, 1) RETURNING {MEMBERSHIP_COLUMNS}"
            ),
            vec![
                Uuid::new_v4().into(),
                id.into(),
                user.into(),
                joined_via.into(),
                ctx.user_id.into(),
                now.into(),
            ],
        )
        .await?
        .ok_or(ApiError::new(ErrorCode::ServiceUnavailable))?;
        store::history(txn, ctx, now, None, &m, "CREATED").await?;
        store::emit_membership_activated(txn, ctx, now, &m).await?;
        owner_row = Some(m);
    }
    store::audit(
        txn,
        ctx,
        now,
        Some(id),
        "organization.created",
        "organization",
        Some(id),
        json!({ "creationSource": new.creation_source, "slug": new.slug, "ownerUserId": new.owner.map(|o| o.0) }),
    )
    .await?;
    Ok((org, owner_row))
}

fn profile_of(input: Option<ProfileInput>) -> Result<ProfileValues, ApiError> {
    let mut profile = ProfileValues::default();
    if let Some(p) = input {
        profile.apply(p)?;
    }
    Ok(profile)
}

/// `POST /internal/v1/organizations` — the acting user creates an organization and
/// becomes its OWNER.
pub async fn create(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, raw): (CreateBody, Value) = parse(&body)?;
    let name = validation::name(&req.name)?;
    let slug = validation::slug(&req.slug)?;
    let profile = profile_of(req.profile)?;
    let idem = idempotency::from_headers(
        &headers,
        format!("organization.create:{}:{user}", caller.service()),
        &raw,
    )?;
    let txn = state.db.begin().await?;
    if let Some(i) = &idem {
        if let Some(stored) = idempotency::begin(&txn, i).await? {
            return Ok(replay(stored));
        }
    }
    // Per-actor creation limit, counted in the database and serialized per actor.
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("org-create:{user}").into()],
    )
    .await?;
    let recent = db::scalar_i64(
        &txn,
        "SELECT count(*)::bigint AS n FROM organizations WHERE creation_source = 'SELF_SERVICE' AND created_by = $1 AND created_at > now() - interval '24 hours'",
        vec![user.into()],
    )
    .await?;
    if recent >= CREATION_LIMIT_PER_DAY {
        return Err(ApiError::with_message(
            ErrorCode::RateLimited,
            "The organization creation limit was reached; retry later.",
        ));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_user();
    let (org, owner) = create_organization(
        &txn,
        &ctx,
        now,
        NewOrganization {
            name,
            slug,
            profile,
            creation_source: "SELF_SERVICE",
            created_by: Some(user),
            owner: Some((user, "CREATOR")),
        },
    )
    .await?;
    let doc = store::organization_json(&txn, &org, owner.as_ref()).await?;
    if let Some(i) = &idem {
        idempotency::finish(&txn, i, 201, &doc, Some(org.id), now).await?;
    }
    txn.commit().await?;
    Ok(respond(StatusCode::CREATED, doc))
}

/// `GET /internal/v1/organizations/{id}` — detail for the actor (404 unless a member), or
/// the authoritative snapshot for a service read without an actor.
pub async fn detail(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    let doc = match caller.acting_user {
        Some(user) => {
            let a = authorize(&state.db, id, user, Capability::OrganizationRead, false).await?;
            store::organization_json(&state.db, &a.org, Some(&a.membership)).await?
        }
        None => {
            let org = store::org(&state.db, id, false)
                .await?
                .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
            store::organization_json(&state.db, &org, None).await?
        }
    };
    Ok(respond(StatusCode::OK, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBody {
    pub expected_version: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub profile: Option<ProfileInput>,
}

/// `PATCH /internal/v1/organizations/{id}` — name, slug and profile (optimistic).
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let (req, _): (UpdateBody, Value) = parse(&body)?;
    let txn = state.db.begin().await?;
    let access = authorize(&txn, id, user, Capability::ProfileWrite, true).await?;
    let org = access.org;
    if org.version != req.expected_version {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let mut changed: Vec<&'static str> = Vec::new();
    let mut name = org.name.clone();
    if let Some(n) = &req.name {
        let n = validation::name(n)?;
        if n != name {
            name = n;
            changed.push("name");
        }
    }
    let mut slug = org.slug.clone();
    if let Some(s) = &req.slug {
        if *s != slug {
            slug = validation::slug(s)?;
            slug_free(&txn, &slug, Some(id)).await?;
            changed.push("slug");
        }
    }
    let mut profile = store::profile(&txn, id).await?;
    let mut profile_changed = false;
    if let Some(p) = req.profile {
        let fields = profile.apply(p)?;
        profile_changed = !fields.is_empty();
        changed.extend(fields);
    }
    if changed.is_empty() {
        let doc = store::organization_json(&txn, &org, Some(&access.membership)).await?;
        return Ok(respond(StatusCode::OK, doc));
    }
    let now = db::now(&txn).await?;
    let updated: OrgRow = db::one(
        &txn,
        &format!("UPDATE organizations SET name = $2, slug = $3, version = version + 1, updated_at = $4 WHERE id = $1 RETURNING {ORG_COLUMNS}"),
        vec![id.into(), name.into(), slug.into(), now.into()],
    )
    .await
    .map_err(slug_taken)?
    .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if profile_changed {
        store::save_profile(&txn, id, &profile, now).await?;
    }
    let ctx = caller.as_user();
    store::emit(
        &txn,
        &ctx,
        now,
        Event {
            organization_id: id,
            aggregate_type: "organization",
            aggregate_id: id,
            aggregate_version: updated.version,
            event_type: "organization.profile_updated",
            payload: json!({ "changedFields": changed }),
        },
    )
    .await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "organization.profile_updated",
        "organization",
        Some(id),
        json!({ "changedFields": changed }),
    )
    .await?;
    let doc = store::organization_json(&txn, &updated, Some(&access.membership)).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

// ---------------------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Suspend,
    Reactivate,
    Archive,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleBody {
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// The target `(status, reason)` of `transition`, `None` when already there (idempotent).
///
/// * OWNER (actor): suspends an ACTIVE organization (`OWNER_REQUEST`), reactivates or
///   archives only an organization the owners themselves suspended; an operator / import
///   suspension can only be lifted by an operator.
/// * Operator: any transition; a suspension it imposes becomes `OPERATOR_ACTION`.
/// * ARCHIVED is terminal.
pub fn plan(
    org: &OrgRow,
    transition: Transition,
    operator: bool,
) -> Result<Option<(LifecycleStatus, Option<&'static str>)>, ApiError> {
    use LifecycleStatus::*;
    let status = org.status();
    let owner_suspended = org.lifecycle_reason.as_deref() == Some("OWNER_REQUEST");
    match (transition, status) {
        (Transition::Archive, Archived) => Ok(None),
        (_, Archived) => Err(ApiError::new(ErrorCode::OrganizationArchived)),
        (Transition::Suspend, Active) => Ok(Some((
            Suspended,
            Some(if operator {
                "OPERATOR_ACTION"
            } else {
                "OWNER_REQUEST"
            }),
        ))),
        (Transition::Suspend, Suspended) if operator && owner_suspended => {
            Ok(Some((Suspended, Some("OPERATOR_ACTION"))))
        }
        (Transition::Suspend, Suspended) => Ok(None),
        (Transition::Reactivate, Active) => Ok(None),
        (Transition::Reactivate, Suspended) if operator || owner_suspended => {
            Ok(Some((Active, None)))
        }
        (Transition::Archive, Active) => Ok(Some((Archived, None))),
        (Transition::Archive, Suspended) if operator || owner_suspended => {
            Ok(Some((Archived, None)))
        }
        (_, Suspended) => Err(ApiError::new(ErrorCode::OrganizationSuspended)),
    }
}

async fn apply_transition(
    txn: &DatabaseTransaction,
    ctx: &OpContext,
    org: &OrgRow,
    transition: Transition,
    operator: bool,
    expected_version: Option<i64>,
) -> Result<OrgRow, ApiError> {
    if expected_version.is_some_and(|v| v != org.version) {
        return Err(ApiError::new(ErrorCode::VersionConflict));
    }
    let Some((status, reason)) = plan(org, transition, operator)? else {
        return Ok(org.clone());
    };
    let now = db::now(txn).await?;
    let updated: OrgRow = db::one(
        txn,
        &format!(
            "UPDATE organizations SET lifecycle_status = $2, lifecycle_reason = $3, archived_at = CASE WHEN $2 = 'ARCHIVED' THEN $4 ELSE NULL END, version = version + 1, updated_at = $4 WHERE id = $1 RETURNING {ORG_COLUMNS}"
        ),
        vec![org.id.into(), status.as_str().into(), reason.into(), now.into()],
    )
    .await?
    .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    // The technical scope follows in the same transaction (fail closed locally; the
    // Platform Access tenant status is applied by the provisioning worker with retries).
    provisioning::on_lifecycle(txn, ctx, now, org.id, status).await?;
    let (event_type, payload, action) = match status {
        LifecycleStatus::Suspended => (
            "organization.suspended",
            json!({ "reasonClass": reason }),
            "organization.suspended",
        ),
        LifecycleStatus::Active => (
            "organization.reactivated",
            json!({}),
            "organization.reactivated",
        ),
        LifecycleStatus::Archived => ("organization.archived", json!({}), "organization.archived"),
    };
    store::emit(
        txn,
        ctx,
        now,
        Event {
            organization_id: org.id,
            aggregate_type: "organization",
            aggregate_id: org.id,
            aggregate_version: updated.version,
            event_type,
            payload,
        },
    )
    .await?;
    store::audit(
        txn,
        ctx,
        now,
        Some(org.id),
        action,
        "organization",
        Some(org.id),
        json!({ "fromStatus": org.lifecycle_status, "toStatus": status.as_str(), "reasonClass": reason }),
    )
    .await?;
    Ok(updated)
}

async fn lifecycle(
    state: AppState,
    caller: Caller,
    id: Uuid,
    body: Bytes,
    transition: Transition,
) -> ApiResult<Response> {
    let (req, _): (LifecycleBody, Value) = parse(&body)?;
    if caller.acting_user.is_none() && caller.has(InternalPermission::OrganizationAdminister) {
        // Operator enforcement (no acting user: operator tooling).
        let txn = state.db.begin().await?;
        let org = store::org(&txn, id, true)
            .await?
            .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
        let updated = apply_transition(
            &txn,
            &caller.as_operator(),
            &org,
            transition,
            true,
            req.expected_version,
        )
        .await?;
        let doc = store::organization_json(&txn, &updated, None).await?;
        txn.commit().await?;
        return Ok(respond(StatusCode::OK, doc));
    }
    caller.require(InternalPermission::OrganizationWrite)?;
    let user = caller.user()?;
    let txn = state.db.begin().await?;
    // OWNERs keep organization.read in a suspended / archived organization; the lifecycle
    // capability itself is checked against the role (plan() applies the status rules).
    let access = authorize(&txn, id, user, Capability::OrganizationRead, true).await?;
    if access.membership.role() != Role::Owner {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let updated = apply_transition(
        &txn,
        &caller.as_user(),
        &access.org,
        transition,
        false,
        req.expected_version,
    )
    .await?;
    let doc = store::organization_json(&txn, &updated, Some(&access.membership)).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

pub async fn suspend(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, caller, id, body, Transition::Suspend).await
}

pub async fn reactivate(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, caller, id, body, Transition::Reactivate).await
}

pub async fn archive(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    lifecycle(state, caller, id, body, Transition::Archive).await
}

// ---------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct UserOrgRow {
    org_id: Uuid,
    org_name: String,
    org_slug: String,
    org_status: String,
    org_provisioning: String,
    org_version: i64,
    policy_document: Option<Value>,
    id: Uuid,
    organization_id: Uuid,
    user_id: Uuid,
    role: String,
    status: String,
    joined_via: String,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    removed_at: Option<DateTime<Utc>>,
    version: i64,
}

/// `GET /internal/v1/users/{userId}/organizations` — the user's memberships in ACTIVE and
/// SUSPENDED organizations (REMOVED memberships and ARCHIVED organizations excluded).
pub async fn user_organizations(
    State(state): State<AppState>,
    caller: Caller,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationRead)?;
    if caller.acting_user.is_some_and(|u| u != user_id) {
        return Err(ApiError::new(ErrorCode::PermissionDenied));
    }
    let rows: Vec<UserOrgRow> = db::all(
        &state.db,
        "SELECT o.id AS org_id, o.name AS org_name, o.slug AS org_slug, o.lifecycle_status AS org_status, o.provisioning_state AS org_provisioning, o.version AS org_version, \
                p.document AS policy_document, \
                m.id, m.organization_id, m.user_id, m.role, m.status, m.joined_via, m.created_by, m.created_at, m.updated_at, m.removed_at, m.version \
         FROM organization_memberships m JOIN organizations o ON o.id = m.organization_id \
         LEFT JOIN organization_policies p ON p.organization_id = o.id \
         WHERE m.user_id = $1 AND m.status <> 'REMOVED' AND o.lifecycle_status IN ('ACTIVE', 'SUSPENDED') \
         ORDER BY o.name, o.id",
        vec![user_id.into()],
    )
    .await?;
    let mut items: Vec<Value> = Vec::with_capacity(rows.len());
    for r in rows {
        let policy = store::parse_policy(r.policy_document.as_ref())?;
        items.push({
            let m = MembershipRow {
                id: r.id,
                organization_id: r.organization_id,
                user_id: r.user_id,
                role: r.role,
                status: r.status,
                joined_via: r.joined_via,
                created_by: r.created_by,
                created_at: r.created_at,
                updated_at: r.updated_at,
                removed_at: r.removed_at,
                version: r.version,
            };
            let org_status = LifecycleStatus::parse(&r.org_status).unwrap_or(LifecycleStatus::Suspended);
            json!({
                "organization": {
                    "id": r.org_id, "name": r.org_name, "slug": r.org_slug,
                    "lifecycleStatus": r.org_status, "provisioningState": r.org_provisioning, "version": r.org_version,
                },
                "membership": m.to_json(),
                "capabilities": effective_capabilities_with_policy(m.role(), m.status(), org_status, &policy),
            })
        });
    }
    Ok(respond(
        StatusCode::OK,
        json!({ "userId": user_id, "items": items }),
    ))
}

/// `GET /internal/v1/organizations/{id}/provisioning` — truthful provisioning state.
pub async fn provisioning(
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
    let doc = provisioning::provisioning_json(&state.db, &org, state.provisioning_enabled).await?;
    Ok(respond(StatusCode::OK, doc))
}

/// `POST /internal/v1/admin/organizations/{id}/provisioning/retry` — operator retry of a
/// FAILED (or cancelled) provisioning workflow; idempotent (READY / in-flight workflows
/// are left as they are). An ARCHIVED organization is never re-provisioned.
pub async fn provisioning_retry(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let txn = state.db.begin().await?;
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    if org.status() == LifecycleStatus::Archived {
        return Err(ApiError::new(ErrorCode::OrganizationArchived));
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_operator();
    let changed = provisioning::request_retry(&txn, &ctx, now, id).await?;
    store::audit(
        &txn,
        &ctx,
        now,
        Some(id),
        "provisioning.retry_requested",
        "organization",
        Some(id),
        json!({ "changed": changed, "fromState": org.provisioning_state }),
    )
    .await?;
    let org = store::org(&txn, id, false)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let doc = provisioning::provisioning_json(&txn, &org, state.provisioning_enabled).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

// ---------------------------------------------------------------------------------------
// Operator administration
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminListQuery {
    #[serde(default)]
    pub cursor: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /internal/v1/admin/organizations` — operator listing (id order, keyset cursor).
pub async fn admin_list(
    State(state): State<AppState>,
    caller: Caller,
    Query(q): Query<AdminListQuery>,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let limit = page_limit(q.limit)?;
    if let Some(s) = &q.status {
        LifecycleStatus::parse(s)
            .ok_or_else(|| ApiError::validation("status must be ACTIVE, SUSPENDED or ARCHIVED"))?;
    }
    let rows: Vec<OrgRow> = db::all(
        &state.db,
        &format!(
            "SELECT {ORG_COLUMNS} FROM organizations WHERE ($1::uuid IS NULL OR id > $1) AND ($2::text IS NULL OR lifecycle_status = $2) ORDER BY id LIMIT $3"
        ),
        vec![q.cursor.into(), q.status.clone().into(), (limit + 1).into()],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let mut items = Vec::new();
    for org in rows.iter().take(limit as usize) {
        items.push(store::organization_json(&state.db, org, None).await?);
    }
    let next = if more {
        rows.get(limit as usize - 1).map(|o| o.id)
    } else {
        None
    };
    Ok(respond(
        StatusCode::OK,
        json!({ "items": items, "nextCursor": next }),
    ))
}

/// `POST /internal/v1/admin/organizations` — operator create (`PLATFORM_ADMIN`), optional
/// first OWNER. A relayed platform administrator (`X-CF-Acting-User`) is recorded as the
/// creator and audit actor.
pub async fn admin_create(
    State(state): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let (req, raw): (AdminCreateBody, Value) = parse(&body)?;
    let name = validation::name(&req.name)?;
    let slug = validation::slug(&req.slug)?;
    let profile = profile_of(req.profile)?;
    let actor = caller
        .acting_user
        .map(|u| u.to_string())
        .unwrap_or_else(|| "-".into());
    let idem = idempotency::from_headers(
        &headers,
        format!("organization.admin-create:{}:{actor}", caller.service()),
        &raw,
    )?;
    let txn = state.db.begin().await?;
    if let Some(i) = &idem {
        if let Some(stored) = idempotency::begin(&txn, i).await? {
            return Ok(replay(stored));
        }
    }
    let now = db::now(&txn).await?;
    let ctx = caller.as_operator();
    let (org, _) = create_organization(
        &txn,
        &ctx,
        now,
        NewOrganization {
            name,
            slug,
            profile,
            creation_source: "PLATFORM_ADMIN",
            created_by: caller.acting_user,
            owner: req.owner_user_id.map(|u| (u, "OPERATOR")),
        },
    )
    .await?;
    let doc = store::organization_json(&txn, &org, None).await?;
    if let Some(i) = &idem {
        idempotency::finish(&txn, i, 201, &doc, Some(org.id), now).await?;
    }
    txn.commit().await?;
    Ok(respond(StatusCode::CREATED, doc))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdminUpdateBody {
    pub status: String,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// `PATCH /internal/v1/admin/organizations/{id}` — operator lifecycle (`{status}`: the
/// Meeting Service platform-admin facade relays `PATCH /api/v2/admin/organizations/{id}`).
pub async fn admin_update(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::OrganizationAdminister)?;
    let (req, _): (AdminUpdateBody, Value) = parse(&body)?;
    let transition = match req.status.as_str() {
        "ACTIVE" => Transition::Reactivate,
        "SUSPENDED" => Transition::Suspend,
        "ARCHIVED" => Transition::Archive,
        _ => {
            return Err(ApiError::validation(
                "status must be ACTIVE, SUSPENDED or ARCHIVED",
            ))
        }
    };
    let txn = state.db.begin().await?;
    let org = store::org(&txn, id, true)
        .await?
        .ok_or(ApiError::new(ErrorCode::OrganizationNotFound))?;
    let updated = apply_transition(
        &txn,
        &caller.as_operator(),
        &org,
        transition,
        true,
        req.expected_version,
    )
    .await?;
    let doc = store::organization_json(&txn, &updated, None).await?;
    txn.commit().await?;
    Ok(respond(StatusCode::OK, doc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(status: &str, reason: Option<&str>) -> OrgRow {
        OrgRow {
            id: Uuid::nil(),
            name: "A".into(),
            slug: "abc".into(),
            lifecycle_status: status.into(),
            lifecycle_reason: reason.map(str::to_string),
            provisioning_state: "PENDING".into(),
            creation_source: "SELF_SERVICE".into(),
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 1,
            archived_at: None,
            profile_policy: json!({}),
        }
    }

    #[test]
    fn owners_cannot_lift_an_operator_suspension_and_archived_is_terminal() {
        use LifecycleStatus::*;
        let active = org("ACTIVE", None);
        assert_eq!(
            plan(&active, Transition::Suspend, false).unwrap(),
            Some((Suspended, Some("OWNER_REQUEST")))
        );
        assert_eq!(plan(&active, Transition::Reactivate, false).unwrap(), None);
        let by_owner = org("SUSPENDED", Some("OWNER_REQUEST"));
        assert_eq!(
            plan(&by_owner, Transition::Reactivate, false).unwrap(),
            Some((Active, None))
        );
        assert_eq!(
            plan(&by_owner, Transition::Suspend, true).unwrap(),
            Some((Suspended, Some("OPERATOR_ACTION")))
        );
        let by_operator = org("SUSPENDED", Some("OPERATOR_ACTION"));
        assert_eq!(
            plan(&by_operator, Transition::Reactivate, false)
                .unwrap_err()
                .code,
            ErrorCode::OrganizationSuspended
        );
        assert_eq!(
            plan(&by_operator, Transition::Archive, false)
                .unwrap_err()
                .code,
            ErrorCode::OrganizationSuspended
        );
        assert_eq!(
            plan(&by_operator, Transition::Reactivate, true).unwrap(),
            Some((Active, None))
        );
        let imported = org("SUSPENDED", Some("IMPORTED"));
        assert!(plan(&imported, Transition::Reactivate, false).is_err());
        let archived = org("ARCHIVED", None);
        assert_eq!(plan(&archived, Transition::Archive, false).unwrap(), None);
        assert_eq!(
            plan(&archived, Transition::Reactivate, true)
                .unwrap_err()
                .code,
            ErrorCode::OrganizationArchived
        );
    }
}
