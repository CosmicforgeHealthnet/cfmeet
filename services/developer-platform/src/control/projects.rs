//! Projects (Platform Access applications) and environments (Platform Access
//! environments) — Section 50 §6.3.
//!
//! * A project is created by delegating the canonical application to Platform Access;
//!   the Developer Platform stores customer-facing metadata keyed one-to-one to the
//!   returned `application_id`.
//! * Archive / suspension suspend the Platform Access application (machine access to it
//!   stops) and mark the profile; NOTHING owned by a communications service is deleted.
//! * SANDBOX / TEST environments are created and suspended here; PRODUCTION environments
//!   belong to the Section 55 production-access workflow (Platform Access refuses them on
//!   the delegation path too).
//! * First-party classification is the tenant's Platform Access classification, set only
//!   by an operator ([`super::operator`]); it is reported, never chosen here.

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate, display_name, email, id_of, opaque_slug,
    organizations::double_option,
    pagination::Page,
    stmt, Caller,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

const PROJECT_VIEW: &str = "jsonb_build_object( \
    'id', p.application_id, 'organizationId', p.tenant_id, 'displayName', p.display_name, \
    'description', p.description, 'supportEmail', p.support_email, 'labels', p.labels, \
    'status', p.status, 'statusReason', p.status_reason, 'createdAt', p.created_at, \
    'updatedAt', p.updated_at, 'archivedAt', p.archived_at, 'version', p.version)";

const ENVIRONMENT_VIEW: &str = "jsonb_build_object( \
    'id', e.environment_id, 'projectId', e.application_id, 'organizationId', e.tenant_id, \
    'kind', e.kind, 'displayName', e.display_name, 'status', e.status, \
    'productionAccessState', e.production_access_state, 'createdAt', e.created_at, \
    'updatedAt', e.updated_at, 'version', e.version)";

fn labels(value: Option<Value>) -> Result<Value, ApiError> {
    let Some(v) = value else {
        return Ok(json!({}));
    };
    let map = v.as_object().ok_or(ApiError(ErrorCode::ValidationError))?;
    if map.len() > 32 {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut out = Map::new();
    for (k, v) in map {
        let ok_key = (1..=64).contains(&k.len())
            && k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
        let value = v
            .as_str()
            .filter(|s| s.chars().count() <= 128 && !s.chars().any(char::is_control));
        match (ok_key, value) {
            (true, Some(s)) => {
                out.insert(k.clone(), Value::from(s));
            }
            _ => return Err(ApiError(ErrorCode::ValidationError)),
        }
    }
    Ok(Value::Object(out))
}

fn description(value: Option<&str>) -> Result<Option<String>, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(d) if d.chars().count() <= 1000 => Ok(Some(d.to_string())),
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

/// The tenant of a project (404 when unknown).
async fn project_tenant<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<Uuid, ApiError> {
    db.query_one(stmt(
        "SELECT tenant_id FROM devplat_project_profiles WHERE application_id = $1",
        vec![project_id.into()],
    ))
    .await?
    .ok_or(ApiError(ErrorCode::NotFound))?
    .try_get::<Uuid>("", "tenant_id")
    .map_err(ApiError::from)
}

async fn project_view<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            &format!("SELECT {PROJECT_VIEW} AS v FROM devplat_project_profiles p WHERE p.application_id = $1"),
            vec![project_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(row.try_get("", "v")?)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProject {
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub support_email: Option<String>,
    #[serde(default)]
    pub labels: Option<Value>,
}

/// `POST /v1/organizations/{id}/projects`.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    body: CreateProject,
) -> Result<(StatusCode, Value), ApiError> {
    authorize(state, caller, tenant_id, Action::ProjectsManage).await?;
    let name = display_name(&body.display_name)?;
    let desc = description(body.description.as_deref())?;
    let support = body.support_email.as_deref().map(email).transpose()?;
    let labels = labels(body.labels)?;
    let app = delegate(
        state,
        caller,
        DelegationCall::CreateApplication {
            actor: caller.delegation_actor(),
            tenant_id,
            slug: opaque_slug("prj"),
            display_name: name.clone(),
        },
    )
    .await?;
    let application_id = id_of(&app)?;
    if app.get("tenantId").and_then(Value::as_str) != Some(tenant_id.to_string().as_str()) {
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_project_profiles \
             (application_id, tenant_id, display_name, description, support_email, labels, status, \
              created_by, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, $8)",
        vec![
            application_id.into(),
            tenant_id.into(),
            name.clone().into(),
            desc.into(),
            support.into(),
            labels.into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "project.created",
            target_type: "PROJECT",
            target_id: application_id.to_string(),
            summary: json!({ "displayName": name }),
        },
    )
    .await?;
    let v = project_view(&txn, application_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

/// `GET /v1/organizations/{id}/projects`.
pub async fn list(
    state: &AppState,
    caller: &Caller,
    tenant_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    authorize(state, caller, tenant_id, Action::ProjectsRead).await?;
    let (at, id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    if page.has_cursor() && at.is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT p.created_at AS k_at, p.application_id AS k_id, {PROJECT_VIEW} AS v \
                 FROM devplat_project_profiles p WHERE p.tenant_id = $1 \
                   AND ($2::timestamptz IS NULL OR (p.created_at, p.application_id) > ($2, $3)) \
                 ORDER BY p.created_at, p.application_id LIMIT $4"
            ),
            vec![
                tenant_id.into(),
                at.into(),
                id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    super::members::page_of(rows, page)
}

/// `GET /v1/projects/{id}`.
pub async fn get(state: &AppState, caller: &Caller, project_id: Uuid) -> Result<Value, ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsRead).await?;
    project_view(&state.db, project_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProject {
    pub expected_version: i64,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, with = "double_option")]
    pub description: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    pub support_email: Option<Option<String>>,
    #[serde(default)]
    pub labels: Option<Value>,
}

/// `PATCH /v1/projects/{id}` — metadata only (credential authority stays in Platform
/// Access).
pub async fn update(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: UpdateProject,
) -> Result<Value, ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsManage).await?;
    let name = body.display_name.as_deref().map(display_name).transpose()?;
    let desc = match &body.description {
        None => None,
        Some(d) => Some(description(d.as_deref())?),
    };
    let support = match &body.support_email {
        None => None,
        Some(None) => Some(None),
        Some(Some(e)) => Some(Some(email(e)?)),
    };
    let labels = body.labels.map(|l| labels(Some(l))).transpose()?;
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_project_profiles SET \
                 display_name = COALESCE($3, display_name), \
                 description = CASE WHEN $4 THEN $5 ELSE description END, \
                 support_email = CASE WHEN $6 THEN $7 ELSE support_email END, \
                 labels = COALESCE($8, labels), updated_at = $9, version = version + 1 \
             WHERE application_id = $1 AND version = $2 AND status <> 'ARCHIVED'",
            vec![
                project_id.into(),
                body.expected_version.into(),
                name.clone().into(),
                desc.is_some().into(),
                desc.flatten().into(),
                support.is_some().into(),
                support.flatten().into(),
                labels.into(),
                Utc::now().into(),
            ],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "project.updated",
            target_type: "PROJECT",
            target_id: project_id.to_string(),
            summary: json!({ "displayName": name }),
        },
    )
    .await?;
    let v = project_view(&txn, project_id).await?;
    txn.commit().await?;
    Ok(v)
}

/// Lifecycle change of a project: `SUSPENDED`, `ACTIVE` (resume) or `ARCHIVED`
/// (terminal). The Platform Access application is suspended FIRST for SUSPENDED /
/// ARCHIVED and reactivated only AFTER the profile records ACTIVE.
pub async fn set_status(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    to: &'static str,
) -> Result<Value, ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsManage).await?;
    let current: String = state
        .db
        .query_one(stmt(
            "SELECT status FROM devplat_project_profiles WHERE application_id = $1",
            vec![project_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "status")?;
    let allowed = matches!(
        (current.as_str(), to),
        ("ACTIVE", "SUSPENDED") | ("SUSPENDED", "ACTIVE") | ("ACTIVE" | "SUSPENDED", "ARCHIVED")
    );
    if !allowed {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    if to != "ACTIVE" {
        delegate(
            state,
            caller,
            DelegationCall::SetApplicationStatus {
                actor: caller.delegation_actor(),
                application_id: project_id,
                suspended: true,
            },
        )
        .await?;
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_project_profiles SET status = $3, \
                 status_reason = CASE WHEN $3 = 'ACTIVE' THEN NULL ELSE 'CUSTOMER_REQUEST' END, \
                 archived_at = CASE WHEN $3 = 'ARCHIVED' THEN $4 ELSE NULL END, \
                 updated_at = $4, version = version + 1 \
             WHERE application_id = $1 AND status = $2",
            vec![
                project_id.into(),
                current.clone().into(),
                to.into(),
                now.into(),
            ],
        ))
        .await?
        .rows_affected();
    if n != 1 {
        return Err(ApiError(ErrorCode::VersionConflict));
    }
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: match to {
                "ARCHIVED" => "project.archived",
                "SUSPENDED" => "project.suspended",
                _ => "project.resumed",
            },
            target_type: "PROJECT",
            target_id: project_id.to_string(),
            summary: json!({ "fromStatus": current, "toStatus": to, "domainDataDeleted": false }),
        },
    )
    .await?;
    let v = project_view(&txn, project_id).await?;
    txn.commit().await?;
    if to == "ACTIVE" {
        delegate(
            state,
            caller,
            DelegationCall::SetApplicationStatus {
                actor: caller.delegation_actor(),
                application_id: project_id,
                suspended: false,
            },
        )
        .await?;
    }
    Ok(v)
}

/// `GET /v1/projects/{id}/scopes` — the project's scope grants as Platform Access reports
/// them (the effective scope set of each credential is Platform Access's decision).
pub async fn scopes(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
) -> Result<Value, ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsRead).await?;
    let v = delegate(
        state,
        caller,
        DelegationCall::ListApplicationScopes {
            application_id: project_id,
        },
    )
    .await?;
    Ok(json!({ "projectId": project_id, "scopes": v.get("scopes").cloned().unwrap_or(json!([])) }))
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironment {
    pub kind: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

async fn environment_view<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<Value, ApiError> {
    let row = db
        .query_one(stmt(
            &format!("SELECT {ENVIRONMENT_VIEW} AS v FROM devplat_environment_profiles e WHERE e.environment_id = $1"),
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(row.try_get("", "v")?)
}

/// `POST /v1/projects/{id}/environments` — SANDBOX or TEST only.
pub async fn create_environment(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: CreateEnvironment,
) -> Result<(StatusCode, Value), ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::EnvironmentsManage).await?;
    let kind = match body.kind.as_str() {
        "SANDBOX" | "TEST" => body.kind.clone(),
        // Production environments are the production-access workflow's (Section 55).
        "PRODUCTION" => return Err(ApiError(ErrorCode::PolicyViolation)),
        _ => return Err(ApiError(ErrorCode::ValidationError)),
    };
    let status: String = state
        .db
        .query_one(stmt(
            "SELECT status FROM devplat_project_profiles WHERE application_id = $1",
            vec![project_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "status")?;
    if status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let name = match body.display_name.as_deref() {
        Some(n) => display_name(n)?,
        None => {
            kind.chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default()
                + &kind[1..].to_ascii_lowercase()
        }
    };
    let env = delegate(
        state,
        caller,
        DelegationCall::CreateEnvironment {
            actor: caller.delegation_actor(),
            application_id: project_id,
            kind: kind.clone(),
        },
    )
    .await?;
    let environment_id = id_of(&env)?;
    if env.get("kind").and_then(Value::as_str) != Some(kind.as_str())
        || env.get("applicationId").and_then(Value::as_str) != Some(project_id.to_string().as_str())
    {
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    }
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "INSERT INTO devplat_environment_profiles \
             (environment_id, application_id, tenant_id, kind, display_name, status, \
              production_access_state, created_by, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, 'ACTIVE', 'NOT_APPLICABLE', $6, $7, $7)",
        vec![
            environment_id.into(),
            project_id.into(),
            tenant_id.into(),
            kind.clone().into(),
            name.into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    txn.execute(stmt(
        "INSERT INTO devplat_environment_refs (id, tenant_id, application_id, environment_id, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (environment_id) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            tenant_id.into(),
            project_id.into(),
            environment_id.into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "environment.created",
            target_type: "ENVIRONMENT",
            target_id: environment_id.to_string(),
            summary: json!({ "projectId": project_id, "kind": kind }),
        },
    )
    .await?;
    let v = environment_view(&txn, environment_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

/// `GET /v1/projects/{id}/environments`.
pub async fn list_environments(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    page: &Page,
) -> Result<Value, ApiError> {
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsRead).await?;
    let (at, id) = page
        .cursor_key()
        .map(|(a, i)| (Some(a), Some(i)))
        .unwrap_or((None, None));
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT e.created_at AS k_at, e.environment_id AS k_id, {ENVIRONMENT_VIEW} AS v \
                 FROM devplat_environment_profiles e WHERE e.application_id = $1 \
                   AND ($2::timestamptz IS NULL OR (e.created_at, e.environment_id) > ($2, $3)) \
                 ORDER BY e.created_at, e.environment_id LIMIT $4"
            ),
            vec![
                project_id.into(),
                at.into(),
                id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .await?;
    super::members::page_of(rows, page)
}

async fn environment_tenant<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<(Uuid, String), ApiError> {
    let row = db
        .query_one(stmt(
            "SELECT tenant_id, kind FROM devplat_environment_profiles WHERE environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok((row.try_get("", "tenant_id")?, row.try_get("", "kind")?))
}

/// `GET /v1/environments/{id}` — re-validated against Platform Access (its status is
/// the authority's).
pub async fn get_environment(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
) -> Result<Value, ApiError> {
    let (tenant_id, _) = environment_tenant(&state.db, environment_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsRead).await?;
    let mut v = environment_view(&state.db, environment_id).await?;
    let pa = delegate(
        state,
        caller,
        DelegationCall::GetEnvironment { environment_id },
    )
    .await?;
    v["authorityStatus"] = pa.get("status").cloned().unwrap_or(Value::Null);
    v["liveTelecomAllowed"] = pa
        .get("liveTelecomAllowed")
        .cloned()
        .unwrap_or(Value::Bool(false));
    Ok(v)
}

/// Suspends / resumes a NON-production environment.
pub async fn set_environment_status(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    suspend: bool,
) -> Result<Value, ApiError> {
    let (tenant_id, kind) = environment_tenant(&state.db, environment_id).await?;
    authorize(state, caller, tenant_id, Action::EnvironmentsManage).await?;
    if kind == "PRODUCTION" {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let (from, to) = if suspend {
        ("ACTIVE", "SUSPENDED")
    } else {
        ("SUSPENDED", "ACTIVE")
    };
    if suspend {
        delegate(
            state,
            caller,
            DelegationCall::SetEnvironmentStatus {
                actor: caller.delegation_actor(),
                environment_id,
                suspended: true,
            },
        )
        .await?;
    }
    let txn = state.db.begin().await?;
    let n = txn
        .execute(stmt(
            "UPDATE devplat_environment_profiles SET status = $3, updated_at = $4, version = version + 1 \
             WHERE environment_id = $1 AND status = $2",
            vec![
                environment_id.into(),
                from.into(),
                to.into(),
                Utc::now().into(),
            ],
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
            action: if suspend {
                "environment.suspended"
            } else {
                "environment.resumed"
            },
            target_type: "ENVIRONMENT",
            target_id: environment_id.to_string(),
            summary: json!({ "kind": kind }),
        },
    )
    .await?;
    let v = environment_view(&txn, environment_id).await?;
    txn.commit().await?;
    if !suspend {
        delegate(
            state,
            caller,
            DelegationCall::SetEnvironmentStatus {
                actor: caller.delegation_actor(),
                environment_id,
                suspended: false,
            },
        )
        .await?;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_bounded_string_maps() {
        assert_eq!(labels(None).unwrap(), json!({}));
        assert!(labels(Some(json!({ "team": "core", "tier/app": "web" }))).is_ok());
        assert!(labels(Some(json!({ "bad key": "x" }))).is_err());
        assert!(labels(Some(json!({ "k": 1 }))).is_err());
        assert!(labels(Some(json!(["x"]))).is_err());
        assert!(description(Some(&"x".repeat(1001))).is_err());
    }
}
