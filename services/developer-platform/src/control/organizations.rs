//! Organizations (Section 50 §6 / §6.2): one public Organization per Platform Access
//! tenant. Creation delegates the canonical EXTERNAL tenant to Platform Access, then
//! records the profile, the creator's OWNER membership and the activity event in ONE
//! transaction. SUSPENDED / CLOSED organizations hold their tenant SUSPENDED in Platform
//! Access, so machine authentication is refused at the gateway and in every service.
//!
//! Section 15: a creation is recorded PENDING first and compensated or reconciled if its
//! local commit never happens (ORG-014, `devplat_organization_provisioning`); a rename is
//! propagated to the EXTERNAL tenant and retried until it holds (ORG-013).

use axum::http::StatusCode;
use cf_service_auth::CorrelationId;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate, display_name, email, id_of,
    pagination::Page,
    stmt, Caller, Principal,
};
use crate::{
    domain::rbac::{Action, OrganizationState},
    http::error::{ApiError, ErrorCode},
    platform_access::{Delegated, DelegationActor, DelegationCall, PlatformAccessPort},
    state::AppState,
};

/// The organization view (`myRole` for human members).
pub const ORG_VIEW: &str = "jsonb_build_object( \
    'id', o.tenant_id, 'displayName', o.display_name, 'state', o.state, \
    'stateReason', o.state_reason, 'supportEmail', o.support_email, \
    'createdAt', o.created_at, 'updatedAt', o.updated_at, 'closedAt', o.closed_at, \
    'version', o.version)";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOrganization {
    pub display_name: String,
    #[serde(default)]
    pub support_email: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateOrganization {
    pub expected_version: i64,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, with = "double_option")]
    pub support_email: Option<Option<String>>,
}

/// `null` clears, absent keeps.
pub mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

pub async fn view<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    my_role: Option<&str>,
) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            &format!("SELECT {ORG_VIEW} AS v FROM devplat_organizations o WHERE o.tenant_id = $1"),
            vec![tenant_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let mut v: Value = row.try_get("", "v")?;
    v["myRole"] = my_role.map(Value::from).unwrap_or(Value::Null);
    Ok(v)
}

/// The Platform Access creation slug of provisioning `id`. Deterministic, so the tenant
/// of a creation can always be found again (ORG-014 reconciliation).
pub fn creation_slug(id: Uuid) -> String {
    format!("org-{}", id.simple())
}

/// A provisioning row left PENDING longer than this is settled by the worker (far
/// longer than any request, so an in-flight creation is never touched).
pub const PROVISIONING_SETTLE_MINUTES: i64 = 10;

/// Moves a PENDING provisioning row to its final state (no-op once settled).
async fn settle_provisioning<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    to: &str,
    tenant_id: Option<Uuid>,
    outcome: &str,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "UPDATE devplat_organization_provisioning \
             SET state = $2, tenant_id = COALESCE($3, tenant_id), outcome_code = $4, updated_at = $5 \
         WHERE id = $1 AND state = 'PENDING'",
        vec![
            id.into(),
            to.into(),
            tenant_id.into(),
            outcome.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Records a failed settlement attempt (the tenant id when known) and keeps it PENDING.
async fn note_provisioning_attempt<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    tenant_id: Option<Uuid>,
    outcome: &str,
) -> Result<(), ApiError> {
    db.execute(stmt(
        "UPDATE devplat_organization_provisioning \
             SET attempts = attempts + 1, tenant_id = COALESCE($2, tenant_id), outcome_code = $3, \
                 updated_at = $4 \
         WHERE id = $1 AND state = 'PENDING'",
        vec![
            id.into(),
            tenant_id.into(),
            outcome.into(),
            Utc::now().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Compensates a creation whose organization was never recorded: the EXTERNAL tenant is
/// suspended in Platform Access (machine access refused; nothing is deleted there) and
/// the provisioning row is closed COMPENSATED. If the organization DOES exist (the commit
/// succeeded after all) nothing is suspended and the row is closed COMPLETED. Returns
/// whether the row is settled.
pub async fn compensate_creation(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
    correlation: &CorrelationId,
    provisioning: Uuid,
    tenant_id: Uuid,
    actor: DelegationActor,
    outcome: &str,
) -> Result<bool, ApiError> {
    if super::organization_state(db, tenant_id).await?.is_some() {
        settle_provisioning(
            db,
            provisioning,
            "COMPLETED",
            Some(tenant_id),
            "ORGANIZATION_RECORDED",
        )
        .await?;
        return Ok(true);
    }
    let call = DelegationCall::SetTenantStatus {
        actor,
        tenant_id,
        suspended: true,
    };
    match platform.delegate(call, correlation).await {
        Ok(Delegated::Ok(_)) => {
            tracing::warn!(%provisioning, %tenant_id, outcome, "orphaned platform tenant suspended");
            settle_provisioning(db, provisioning, "COMPENSATED", Some(tenant_id), outcome).await?;
            Ok(true)
        }
        other => {
            tracing::warn!(%provisioning, %tenant_id, result = ?other.map(|_| ()).map_err(|e| e.to_string()),
                "orphaned platform tenant not yet suspended; the worker retries");
            note_provisioning_attempt(db, provisioning, Some(tenant_id), "COMPENSATION_PENDING")
                .await?;
            Ok(false)
        }
    }
}

/// `POST /v1/organizations` — a human creates a workspace and becomes its OWNER.
///
/// Section 15 (ORG-014): the creation is recorded PENDING before Platform Access is asked
/// for the tenant (under a slug derived from the provisioning id); the organization, its
/// OWNER, the activity event and the provisioning completion commit in ONE transaction.
/// A failure after the tenant exists is compensated at once (tenant suspended); a crash
/// leaves the row PENDING and the worker settles it ([`reconcile_provisioning`]). Either
/// way no ACTIVE tenant without an organization and an owner survives.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    body: CreateOrganization,
) -> Result<(StatusCode, Value), ApiError> {
    let human = caller.human()?.clone();
    let name = display_name(&body.display_name)?;
    let support = body.support_email.as_deref().map(email).transpose()?;
    let provisioning = Uuid::new_v4();
    let started = Utc::now();
    state
        .db
        .execute(stmt(
            "INSERT INTO devplat_organization_provisioning \
                 (id, requested_by, state, created_at, updated_at) VALUES ($1, $2, 'PENDING', $3, $3)",
            vec![
                provisioning.into(),
                caller.actor_ref().into(),
                started.into(),
            ],
        ))
        .await?;
    // Any failure here leaves the row PENDING: whether Platform Access created the tenant
    // is unknown, and the worker finds out by the creation slug.
    let tenant = delegate(
        state,
        caller,
        DelegationCall::CreateTenant {
            actor: DelegationActor::Human(human.user_id),
            slug: creation_slug(provisioning),
            display_name: name.clone(),
        },
    )
    .await?;
    let tenant_id = id_of(&tenant)?;
    let compensate = |outcome: &'static str| {
        compensate_creation(
            &state.db,
            state.platform_access.as_ref(),
            &caller.request.correlation_id,
            provisioning,
            tenant_id,
            DelegationActor::Human(human.user_id),
            outcome,
        )
    };
    if tenant.get("classification").and_then(Value::as_str) != Some("EXTERNAL") {
        // Self-service never yields a first-party tenant.
        let _ = compensate("NOT_EXTERNAL").await;
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    match record_created(
        state,
        caller,
        &human,
        provisioning,
        tenant_id,
        &name,
        support,
    )
    .await
    {
        Ok(v) => Ok((StatusCode::CREATED, v)),
        Err(e) => {
            if let Err(c) = compensate("LOCAL_COMMIT_FAILED").await {
                tracing::warn!(error = ?c, %provisioning, "compensation not recorded; the worker retries");
            }
            Err(e)
        }
    }
}

/// The local half of [`create`]: organization, OWNER membership, activity and the
/// provisioning completion, all or nothing.
async fn record_created(
    state: &AppState,
    caller: &Caller,
    human: &crate::session::HumanUser,
    provisioning: Uuid,
    tenant_id: Uuid,
    name: &str,
    support: Option<String>,
) -> Result<Value, ApiError> {
    let name = name.to_string();
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_organizations \
             (tenant_id, display_name, state, support_email, created_by, created_at, updated_at) \
         VALUES ($1, $2, 'ACTIVE', $3, $4, $5, $5)",
        vec![
            tenant_id.into(),
            name.clone().into(),
            support.into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    let member_id = Uuid::new_v4();
    txn.execute(stmt(
        "INSERT INTO devplat_workspace_members \
             (id, tenant_id, user_id, role, status, joined_via, created_at, updated_at) \
         VALUES ($1, $2, $3, 'OWNER', 'ACTIVE', 'CREATOR', $4, $4)",
        vec![
            member_id.into(),
            tenant_id.into(),
            human.user_id.into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "organization.created",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "displayName": name, "classification": "EXTERNAL", "ownerMemberId": member_id }),
        },
    )
    .await?;
    settle_provisioning(&txn, provisioning, "COMPLETED", Some(tenant_id), "CREATED").await?;
    let v = view(&txn, tenant_id, Some("OWNER")).await?;
    txn.commit().await?;
    Ok(v)
}

/// What one reconciliation pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Orphaned tenants suspended.
    pub compensated: u64,
    /// Creations Platform Access never completed (no tenant under the slug).
    pub abandoned: u64,
    /// Creations whose organization turned out to be recorded.
    pub completed: u64,
    /// Rows left PENDING for the next pass (authority unavailable).
    pub retried: u64,
    /// Organization names propagated to Platform Access.
    pub names_synced: u64,
}

/// Section 15 (ORG-014): settles organization creations left PENDING for longer than
/// [`PROVISIONING_SETTLE_MINUTES`] (a crash or failure between the Platform Access call
/// and the local commit). Run by the worker; idempotent and safe to run concurrently
/// (every transition is guarded by `state = 'PENDING'`, suspension is idempotent).
pub async fn reconcile_provisioning(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
    settle: chrono::Duration,
    batch: u64,
) -> Result<ReconcileReport, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT id, tenant_id FROM devplat_organization_provisioning \
             WHERE state = 'PENDING' AND created_at < $1 ORDER BY created_at LIMIT $2",
            vec![(Utc::now() - settle).into(), (batch as i64).into()],
        ))
        .await?;
    let mut report = ReconcileReport::default();
    for r in rows {
        let id: Uuid = r.try_get("", "id")?;
        let known: Option<Uuid> = r.try_get("", "tenant_id")?;
        let correlation = CorrelationId::generate();
        let tenant_id = match known {
            Some(t) => t,
            None => {
                let call = DelegationCall::GetTenantBySlug {
                    slug: creation_slug(id),
                };
                match platform.delegate(call, &correlation).await {
                    Ok(Delegated::Ok(v)) => match id_of(&v) {
                        Ok(t) => t,
                        Err(_) => {
                            note_provisioning_attempt(db, id, None, "MALFORMED_TENANT").await?;
                            report.retried += 1;
                            continue;
                        }
                    },
                    Ok(Delegated::Refused { status: 404, .. }) => {
                        settle_provisioning(db, id, "ABANDONED", None, "NO_TENANT").await?;
                        report.abandoned += 1;
                        continue;
                    }
                    _ => {
                        note_provisioning_attempt(db, id, None, "AUTHORITY_UNAVAILABLE").await?;
                        report.retried += 1;
                        continue;
                    }
                }
            }
        };
        if super::organization_state(db, tenant_id).await?.is_some() {
            settle_provisioning(
                db,
                id,
                "COMPLETED",
                Some(tenant_id),
                "ORGANIZATION_RECORDED",
            )
            .await?;
            report.completed += 1;
            continue;
        }
        // The reconciliation is a Developer Platform command (`o-<provisioning id>`).
        let settled = compensate_creation(
            db,
            platform,
            &correlation,
            id,
            tenant_id,
            DelegationActor::Operator(id),
            "RECONCILED_ORPHAN",
        )
        .await?;
        if settled {
            report.compensated += 1;
        } else {
            report.retried += 1;
        }
    }
    Ok(report)
}

/// Section 15 (ORG-013): pushes `name` to the organization's Platform Access tenant and
/// clears the pending marker if the name is still current. A permanent refusal
/// (FIRST_PARTY tenant, tenant unknown) also clears it — that name is operator-managed.
pub async fn push_display_name(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
    correlation: &CorrelationId,
    actor: DelegationActor,
    tenant_id: Uuid,
    name: &str,
) -> Result<bool, ApiError> {
    let call = DelegationCall::RenameTenant {
        actor,
        tenant_id,
        display_name: name.to_string(),
    };
    let clear = match platform.delegate(call, correlation).await {
        Ok(Delegated::Ok(_)) => true,
        Ok(Delegated::Refused { status, code }) if status == 404 || code == "POLICY_VIOLATION" => {
            tracing::info!(%tenant_id, status, %code, "tenant name is not delegable; left operator-managed");
            true
        }
        other => {
            tracing::warn!(%tenant_id, result = ?other.map(|_| ()).map_err(|e| e.to_string()),
                "organization name not yet propagated; the worker retries");
            false
        }
    };
    if clear {
        db.execute(stmt(
            "UPDATE devplat_organizations SET platform_name_sync_pending = FALSE \
             WHERE tenant_id = $1 AND display_name = $2 AND platform_name_sync_pending",
            vec![tenant_id.into(), name.into()],
        ))
        .await?;
    }
    Ok(clear)
}

/// Section 15 (ORG-013): propagates renames still pending after `settle` (the request's
/// own propagation failed or the process stopped). Returns how many were propagated.
pub async fn sync_pending_names(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
    settle: chrono::Duration,
    batch: u64,
) -> Result<u64, ApiError> {
    let rows = db
        .query_all(stmt(
            "SELECT tenant_id, display_name FROM devplat_organizations \
             WHERE platform_name_sync_pending AND updated_at < $1 ORDER BY updated_at LIMIT $2",
            vec![(Utc::now() - settle).into(), (batch as i64).into()],
        ))
        .await?;
    let mut synced = 0;
    for r in rows {
        let tenant_id: Uuid = r.try_get("", "tenant_id")?;
        let name: String = r.try_get("", "display_name")?;
        let command = Uuid::new_v4();
        if push_display_name(
            db,
            platform,
            &CorrelationId::generate(),
            DelegationActor::Operator(command),
            tenant_id,
            &name,
        )
        .await?
        {
            synced += 1;
        }
    }
    Ok(synced)
}

/// One worker pass of the Section 15 reconciliations (provisioning, then names).
pub async fn reconcile_once(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
) -> Result<ReconcileReport, ApiError> {
    let mut report = reconcile_provisioning(
        db,
        platform,
        chrono::Duration::minutes(PROVISIONING_SETTLE_MINUTES),
        50,
    )
    .await?;
    report.names_synced =
        sync_pending_names(db, platform, chrono::Duration::minutes(1), 50).await?;
    Ok(report)
}

/// `GET /v1/organizations` — the caller's organizations (a machine credential: its own).
pub async fn list(state: &AppState, caller: &Caller, page: &Page) -> Result<Value, ApiError> {
    match &caller.principal {
        Principal::Machine(m) => {
            let access =
                authorize(state, caller, m.context.tenant_id, Action::OrganizationRead).await?;
            Ok(
                json!({ "items": [view(&state.db, access.tenant_id, None).await?], "nextCursor": null }),
            )
        }
        Principal::Human(h) => {
            let (after_at, after_id) = page
                .cursor_key()
                .map(|(a, i)| (Some(a), Some(i)))
                .unwrap_or((None, None));
            if page.has_cursor() && after_at.is_none() {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            let rows = state
                .db
                .query_all(stmt(
                    &format!(
                        "SELECT m.created_at AS k_at, m.id AS k_id, m.role, {ORG_VIEW} AS v \
                         FROM devplat_workspace_members m JOIN devplat_organizations o ON o.tenant_id = m.tenant_id \
                         WHERE m.user_id = $1 AND m.status = 'ACTIVE' \
                           AND ($2::timestamptz IS NULL OR (m.created_at, m.id) > ($2, $3)) \
                         ORDER BY m.created_at, m.id LIMIT $4"
                    ),
                    vec![
                        h.user_id.into(),
                        after_at.into(),
                        after_id.into(),
                        (page.limit as i64 + 1).into(),
                    ],
                ))
                .await?;
            let mut items = Vec::new();
            let mut next = None;
            for (i, r) in rows.iter().enumerate() {
                if i == page.limit {
                    break;
                }
                let mut v: Value = r.try_get("", "v")?;
                v["myRole"] = Value::from(r.try_get::<String>("", "role")?);
                items.push(v);
                next = Some(Page::key_cursor(
                    r.try_get("", "k_at")?,
                    r.try_get("", "k_id")?,
                ));
            }
            Ok(
                json!({ "items": items, "nextCursor": if rows.len() > page.limit { next } else { None } }),
            )
        }
    }
}

/// `GET /v1/organizations/{id}` — includes Platform Access's classification.
pub async fn get(state: &AppState, caller: &Caller, tenant_id: Uuid) -> Result<Value, ApiError> {
    let access = authorize(state, caller, tenant_id, Action::OrganizationRead).await?;
    let mut v = view(&state.db, tenant_id, access.member.map(|(_, r)| r.as_str())).await?;
    let tenant = delegate(state, caller, DelegationCall::GetTenant { tenant_id }).await?;
    v["classification"] = tenant.get("classification").cloned().unwrap_or(Value::Null);
    Ok(v)
}

/// `PATCH /v1/organizations/{id}` (OWNER / ADMIN, or `organizations.manage`).
pub async fn update(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: UpdateOrganization,
) -> Result<Value, ApiError> {
    let access = authorize(state, caller, tenant_id, Action::OrganizationUpdate).await?;
    let name = body.display_name.as_deref().map(display_name).transpose()?;
    let support = match &body.support_email {
        None => None,
        Some(None) => Some(None),
        Some(Some(e)) => Some(Some(email(e)?)),
    };
    let txn = state.db.begin().await?;
    // Section 15 (ORG-013): a new name of a self-service organization is marked pending
    // until its Platform Access tenant carries it too (first-party names stay
    // operator-managed in Platform Access).
    let updated = txn
        .query_one(stmt(
            "UPDATE devplat_organizations SET \
                 platform_name_sync_pending = platform_name_sync_pending OR ( \
                     $3::text IS NOT NULL AND $3 <> display_name AND created_by NOT LIKE 'operator:%'), \
                 display_name = COALESCE($3, display_name), \
                 support_email = CASE WHEN $4 THEN $5 ELSE support_email END, \
                 updated_at = $6, version = version + 1 \
             WHERE tenant_id = $1 AND version = $2 \
             RETURNING platform_name_sync_pending AS pending, display_name",
            vec![
                tenant_id.into(),
                body.expected_version.into(),
                name.clone().into(),
                support.is_some().into(),
                support.clone().flatten().into(),
                Utc::now().into(),
            ],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::VersionConflict))?;
    let pending_name = updated
        .try_get::<bool>("", "pending")?
        .then(|| updated.try_get::<String>("", "display_name"))
        .transpose()?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "organization.updated",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "displayName": name, "supportContactChanged": support.is_some() }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id, access.member.map(|(_, r)| r.as_str())).await?;
    txn.commit().await?;
    if let Some(current) = pending_name {
        // Best effort: the rename is committed either way; a failure is retried by the
        // worker ([`sync_pending_names`]).
        if let Err(e) = push_display_name(
            &state.db,
            state.platform_access.as_ref(),
            &caller.request.correlation_id,
            caller.delegation_actor(),
            tenant_id,
            &current,
        )
        .await
        {
            tracing::warn!(error = ?e, %tenant_id, "organization name propagation deferred");
        }
    }
    Ok(v)
}

/// `POST /v1/organizations/{id}/close` (OWNER): terminal. The tenant is suspended in
/// Platform Access FIRST, so a failure after it can only leave access denied, never
/// granted.
pub async fn close(state: &AppState, caller: &Caller, tenant_id: Uuid) -> Result<Value, ApiError> {
    let access = authorize(state, caller, tenant_id, Action::OrganizationClose).await?;
    delegate(
        state,
        caller,
        DelegationCall::SetTenantStatus {
            actor: caller.delegation_actor(),
            tenant_id,
            suspended: true,
        },
    )
    .await?;
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_organizations SET state = 'CLOSED', state_reason = 'CLOSED_BY_OWNER', \
                 closed_at = $2, updated_at = $2, version = version + 1 \
             WHERE tenant_id = $1 AND state <> 'CLOSED'",
            vec![tenant_id.into(), now.into()],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "organization.closed",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "fromState": access.state.as_str() }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id, access.member.map(|(_, r)| r.as_str())).await?;
    txn.commit().await?;
    Ok(v)
}

/// Operator lifecycle transition (restrict / suspend / reactivate / close). Machine
/// access is withdrawn in Platform Access BEFORE the state records a suspension, and
/// restored only AFTER it records reactivation (fail closed both ways).
pub async fn operator_set_state(
    state: &AppState,
    operator: &str,
    request: &crate::domain::RequestContext,
    tenant_id: Uuid,
    to: OrganizationState,
    reason: &str,
) -> Result<Value, ApiError> {
    let from = super::organization_state(&state.db, tenant_id)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    if !from.operator_may_transition(to) {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let command = Uuid::new_v4();
    let withdraw = to.suspends_machine_access() && !from.suspends_machine_access();
    let restore = !to.suspends_machine_access() && from.suspends_machine_access();
    if withdraw {
        super::delegate_with(
            state,
            &request.correlation_id,
            DelegationCall::SetTenantStatus {
                actor: DelegationActor::Operator(command),
                tenant_id,
                suspended: true,
            },
        )
        .await?;
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "UPDATE devplat_organizations SET state = $2, state_reason = $3, \
             closed_at = CASE WHEN $2 = 'CLOSED' THEN $4 ELSE NULL END, \
             updated_at = $4, version = version + 1 WHERE tenant_id = $1",
        vec![
            tenant_id.into(),
            to.as_str().into(),
            reason.into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id,
            action: "organization.state_changed",
            target_type: "ORGANIZATION",
            target_id: tenant_id.to_string(),
            summary: json!({ "fromState": from.as_str(), "toState": to.as_str(), "reason": reason, "commandId": command }),
        },
    )
    .await?;
    let v = view(&txn, tenant_id, None).await?;
    txn.commit().await?;
    if restore {
        super::delegate_with(
            state,
            &request.correlation_id,
            DelegationCall::SetTenantStatus {
                actor: DelegationActor::Operator(command),
                tenant_id,
                suspended: false,
            },
        )
        .await?;
    }
    Ok(v)
}
