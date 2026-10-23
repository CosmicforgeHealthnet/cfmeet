//! Organizations (Section 50 §6 / §6.2): one public Organization per Platform Access
//! tenant. Creation delegates the canonical EXTERNAL tenant to Platform Access, then
//! records the profile, the creator's OWNER membership and the activity event in ONE
//! transaction. SUSPENDED / CLOSED organizations hold their tenant SUSPENDED in Platform
//! Access, so machine authentication is refused at the gateway and in every service.

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate, display_name, email, id_of, opaque_slug,
    pagination::Page,
    stmt, Caller, Principal,
};
use crate::{
    domain::rbac::{Action, OrganizationState},
    http::error::{ApiError, ErrorCode},
    platform_access::{DelegationActor, DelegationCall},
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

/// `POST /v1/organizations` — a human creates a workspace and becomes its OWNER.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    body: CreateOrganization,
) -> Result<(StatusCode, Value), ApiError> {
    let human = caller.human()?.clone();
    let name = display_name(&body.display_name)?;
    let support = body.support_email.as_deref().map(email).transpose()?;
    let tenant = delegate(
        state,
        caller,
        DelegationCall::CreateTenant {
            actor: DelegationActor::Human(human.user_id),
            slug: opaque_slug("org"),
            display_name: name.clone(),
        },
    )
    .await?;
    let tenant_id = id_of(&tenant)?;
    if tenant.get("classification").and_then(Value::as_str) != Some("EXTERNAL") {
        // Self-service never yields a first-party tenant.
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
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
    let v = view(&txn, tenant_id, Some("OWNER")).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
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
    let updated = txn
        .execute(stmt(
            "UPDATE devplat_organizations SET \
                 display_name = COALESCE($3, display_name), \
                 support_email = CASE WHEN $4 THEN $5 ELSE support_email END, \
                 updated_at = $6, version = version + 1 \
             WHERE tenant_id = $1 AND version = $2",
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
        .rows_affected();
    if updated != 1 {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
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
