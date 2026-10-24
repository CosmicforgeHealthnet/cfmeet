//! Section 54 §10.2 — the scope registry and project scope grants.
//!
//! Platform Access owns the registry and every grant; the Developer Platform shows the
//! registry with its grantability and delegates grants:
//!
//! * Self-service (OWNER / ADMIN, machines `projects.manage`): STANDARD scopes only, for
//!   SANDBOX and TEST; PRODUCTION is added only for scopes of an approved, LIVE
//!   production capability (§55).
//! * FIRST_PARTY_ONLY scopes: an explicit, audited operator grant (Platform Access still
//!   verifies the tenant's FIRST_PARTY classification; never inferred from a name).
//! * PRIVILEGED_OPERATOR scopes are never granted here.

use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, delegate, delegate_with,
    production::{production_scope_approved, CAPABILITIES},
    stmt, Caller,
};
use crate::{
    domain::{rbac::Action, RequestContext},
    http::error::{ApiError, ErrorCode},
    platform_access::{DelegationActor, DelegationCall},
    state::AppState,
};

fn capability_of(scope: &str) -> Option<(&'static str, bool)> {
    CAPABILITIES
        .iter()
        .find(|c| c.scopes.contains(&scope))
        .map(|c| (c.name, c.regulated))
}

async fn trust_of(state: &AppState, scope: &str) -> Result<String, ApiError> {
    let registry = state
        .platform_access
        .scope_registry()
        .await
        .map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
    registry
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(scope))
        .and_then(|r| r.get("trust").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or(ApiError(ErrorCode::ValidationError))
}

/// `GET /v1/scopes` — the registry with self-service grantability and the production
/// capability each scope belongs to.
pub async fn registry(state: &AppState, caller: &Caller) -> Result<Value, ApiError> {
    let _ = caller;
    let registry = state
        .platform_access
        .scope_registry()
        .await
        .map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
    let items: Vec<Value> = registry
        .into_iter()
        .map(|r| {
            let name = r
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let trust = r
                .get("trust")
                .and_then(Value::as_str)
                .unwrap_or("PRIVILEGED_OPERATOR");
            let cap = capability_of(&name);
            json!({
                "name": name,
                "description": r.get("description").cloned().unwrap_or(Value::Null),
                "trust": trust,
                "selfServiceGrantable": trust == "STANDARD",
                "productionCapability": cap.map(|(c, _)| c),
                "regulated": cap.is_some_and(|(_, r)| r),
            })
        })
        .collect();
    Ok(json!({ "items": items, "nextCursor": null }))
}

async fn project_tenant<C: ConnectionTrait>(db: &C, project: Uuid) -> Result<Uuid, ApiError> {
    Ok(db
        .query_one(stmt(
            "SELECT tenant_id FROM devplat_project_profiles WHERE application_id = $1",
            vec![project.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?
        .try_get("", "tenant_id")?)
}

fn scope_name(s: &str) -> Result<(), ApiError> {
    let ok = (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    ok.then_some(()).ok_or(ApiError(ErrorCode::ValidationError))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantScope {
    pub scope: String,
}

/// `POST /v1/projects/{id}/scopes`.
pub async fn grant(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    body: GrantScope,
) -> Result<Value, ApiError> {
    scope_name(&body.scope)?;
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsManage).await?;
    if trust_of(state, &body.scope).await? != "STANDARD" {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let mut kinds = vec!["SANDBOX".to_string(), "TEST".to_string()];
    if production_scope_approved(&state.db, project_id, &body.scope).await? {
        kinds.push("PRODUCTION".into());
    }
    let v = delegate(
        state,
        caller,
        DelegationCall::GrantApplicationScope {
            actor: caller.delegation_actor(),
            application_id: project_id,
            scope: body.scope.clone(),
            environment_kinds: kinds.clone(),
        },
    )
    .await?;
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "scope.granted",
            target_type: "PROJECT",
            target_id: project_id.to_string(),
            summary: json!({ "scope": body.scope, "environmentKinds": kinds }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(
        json!({ "projectId": project_id, "scope": v.get("scope").cloned().unwrap_or(json!(body.scope)),
               "environmentKinds": v.get("allowedEnvironmentKinds").cloned().unwrap_or(json!(kinds)),
               "grantBasis": v.get("grantBasis").cloned().unwrap_or(Value::Null) }),
    )
}

/// `DELETE /v1/projects/{id}/scopes/{scope}` — Platform Access withdraws it from every
/// credential of the project at once.
pub async fn withdraw(
    state: &AppState,
    caller: &Caller,
    project_id: Uuid,
    scope: &str,
) -> Result<Value, ApiError> {
    scope_name(scope)?;
    let tenant_id = project_tenant(&state.db, project_id).await?;
    authorize(state, caller, tenant_id, Action::ProjectsManage).await?;
    if trust_of(state, scope).await? != "STANDARD" {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let v = delegate(
        state,
        caller,
        DelegationCall::WithdrawApplicationScope {
            actor: caller.delegation_actor(),
            application_id: project_id,
            scope: scope.to_string(),
        },
    )
    .await?;
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id,
            action: "scope.withdrawn",
            target_type: "PROJECT",
            target_id: project_id.to_string(),
            summary: json!({ "scope": scope }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(json!({ "projectId": project_id, "scope": scope,
               "removed": v.get("removed").cloned().unwrap_or(Value::Bool(false)) }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorGrant {
    pub scope: String,
    pub environment_kinds: Vec<String>,
    pub reason_code: String,
}

/// `POST /internal/v1/operator/projects/{id}/scope-grants` — the explicit, audited
/// first-party grant (FIRST_PARTY_ONLY scopes; STANDARD ones too).
pub async fn operator_grant(
    state: &AppState,
    operator: &str,
    request: &RequestContext,
    project_id: Uuid,
    body: OperatorGrant,
) -> Result<Value, ApiError> {
    scope_name(&body.scope)?;
    if !super::operator::valid_reason(&body.reason_code)
        || body.environment_kinds.is_empty()
        || body
            .environment_kinds
            .iter()
            .any(|k| !matches!(k.as_str(), "SANDBOX" | "TEST" | "PRODUCTION"))
    {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let tenant_id = project_tenant(&state.db, project_id).await?;
    let trust = trust_of(state, &body.scope).await?;
    if !matches!(trust.as_str(), "STANDARD" | "FIRST_PARTY_ONLY") {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    let command = Uuid::new_v4();
    let v = delegate_with(
        state,
        &request.correlation_id,
        DelegationCall::GrantApplicationScope {
            actor: DelegationActor::Operator(command),
            application_id: project_id,
            scope: body.scope.clone(),
            environment_kinds: body.environment_kinds.clone(),
        },
    )
    .await?;
    let txn = state.db.begin().await?;
    activity::record_operator(
        &txn,
        operator,
        request.request_id.as_str(),
        request.correlation_id.as_str(),
        Event {
            tenant_id,
            action: "scope.operator_granted",
            target_type: "PROJECT",
            target_id: project_id.to_string(),
            summary: json!({ "scope": body.scope, "trust": trust,
                             "environmentKinds": body.environment_kinds,
                             "reasonCode": body.reason_code, "commandId": command,
                             "at": Utc::now() }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(
        json!({ "projectId": project_id, "scope": body.scope, "trust": trust,
               "environmentKinds": v.get("allowedEnvironmentKinds").cloned().unwrap_or(Value::Null) }),
    )
}
