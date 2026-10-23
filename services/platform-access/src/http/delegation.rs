//! Developer Platform delegation API (`/internal/v1/delegation/**`, Part VI §50).
//!
//! The Developer Platform is the human control plane of developer workspaces; Platform
//! Access stays the ONLY identity authority. These routes let the Developer Platform —
//! and nothing else (`platform-identity.delegate`, registry-grantable to
//! `cfmeet.developer-platform` only) — create and change the lifecycle of the canonical
//! hierarchy behind a workspace on behalf of a human member it has already authorized:
//!
//! | Route | Rule |
//! |---|---|
//! | `POST /delegation/tenants` | always an EXTERNAL tenant (first-party classification is operator-only) |
//! | `GET /delegation/tenants/{id}` | read |
//! | `POST /delegation/tenants/{id}/status` | ACTIVE / SUSPENDED (organization suspension / closure) |
//! | `POST /delegation/tenants/{id}/applications` | project = application |
//! | `GET /delegation/applications/{id}` | read |
//! | `POST /delegation/applications/{id}/status` | project suspension / archive |
//! | `GET /delegation/applications/{id}/environments` | read |
//! | `GET /delegation/applications/{id}/scopes` | effective scope grants (read) |
//! | `POST /delegation/applications/{id}/environments` | SANDBOX / TEST only — PRODUCTION is the Section 55 approval workflow |
//! | `GET /delegation/environments/{id}` | read |
//! | `POST /delegation/environments/{id}/status` | non-production environments only |
//! | `POST /delegation/environments/{id}/service-accounts` | non-production environments only |
//! | `GET /delegation/environments/{id}/service-accounts` | read |
//! | `GET /delegation/environments/{id}/service-accounts/{sa}` | read; a service account of another environment is NOT_FOUND |
//! | `POST /delegation/environments/{id}/service-accounts/{sa}/status` | ACTIVE / SUSPENDED only (revocation stays operator-only; never deleted), non-production environments only |
//!
//! Every mutation names the acting party (`x-cf-delegated-actor`: `u-<32 hex>` a human
//! member the Developer Platform authorized, or `s-<32 hex>` the Platform Access service
//! account of a scoped machine credential, or `o-<32 hex>` a Developer Platform operator
//! command) and goes through the audited `operator_*`
//! operations with operator id `devplat.<actor>`: one Platform Access audit row per
//! committed change. No credential, verifier, privileged scope, principal or live
//! telecom switch is reachable here: service accounts are created, read, suspended and
//! resumed, but their API credentials (issuance, rotation, revocation) are not delegated.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{
    domain::{
        ApplicationId, ApplicationStatus, EnvironmentId, EnvironmentKind, EnvironmentStatus,
        ServiceAccountId, ServiceAccountStatus, TenantClassification, TenantId, TenantStatus,
    },
    identity::{
        applications::NewApplication,
        operator::OperatorContext,
        service_accounts::{NewServiceAccount, ServiceAccountView},
        tenants::NewTenant,
    },
    state::AppState,
};

pub const ACTOR_HEADER: &str = "x-cf-delegated-actor";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/delegation/tenants", post(create_tenant))
        .route("/delegation/tenants/:tenant_id", get(get_tenant))
        .route(
            "/delegation/tenants/:tenant_id/status",
            post(set_tenant_status),
        )
        .route(
            "/delegation/tenants/:tenant_id/applications",
            post(create_application),
        )
        .route(
            "/delegation/applications/:application_id",
            get(get_application),
        )
        .route(
            "/delegation/applications/:application_id/status",
            post(set_application_status),
        )
        .route(
            "/delegation/applications/:application_id/environments",
            post(create_environment).get(list_environments),
        )
        .route(
            "/delegation/applications/:application_id/scopes",
            get(list_scopes),
        )
        .route(
            "/delegation/environments/:environment_id",
            get(get_environment),
        )
        .route(
            "/delegation/environments/:environment_id/status",
            post(set_environment_status),
        )
        .route(
            "/delegation/environments/:environment_id/service-accounts",
            post(create_service_account).get(list_service_accounts),
        )
        .route(
            "/delegation/environments/:environment_id/service-accounts/:service_account_id",
            get(get_service_account),
        )
        .route(
            "/delegation/environments/:environment_id/service-accounts/:service_account_id/status",
            post(set_service_account_status),
        )
}

fn valid_actor(value: &str) -> bool {
    value.len() == 34
        && (value.starts_with("u-") || value.starts_with("s-") || value.starts_with("o-"))
        && value[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Authorizes a delegated mutation: permission + a named human actor.
fn delegated(caller: &InternalCaller, headers: &HeaderMap) -> AccessResult<OperatorContext> {
    caller.require_permission(InternalPermission::PlatformIdentityDelegate)?;
    let actor = headers
        .get(ACTOR_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| valid_actor(v))
        .ok_or(AccessError::Refused(ErrorCode::OperatorRequired))?;
    OperatorContext::new(&format!("devplat.{actor}"))
        .map_err(|_| AccessError::Refused(ErrorCode::OperatorRequired))
}

fn read_only(caller: &InternalCaller) -> AccessResult<()> {
    caller.require_permission(InternalPermission::PlatformIdentityDelegate)
}

fn op_err(e: crate::identity::PlatformError) -> AccessError {
    AccessError::operator(e)
}

fn policy(message: &str) -> AccessError {
    AccessError::Operator(ErrorCode::PolicyViolation, message.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewTenantBody {
    slug: String,
    display_name: String,
    #[serde(default)]
    billing_currency: Option<String>,
}

async fn create_tenant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(b): Json<NewTenantBody>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    // Self-service workspaces are EXTERNAL; FIRST_PARTY is an explicit operator decision.
    let t = s
        .platform
        .operator_create_tenant(
            &op,
            NewTenant {
                slug: b.slug,
                display_name: b.display_name,
                classification: TenantClassification::External,
                billing_currency: b.billing_currency,
            },
        )
        .await
        .map_err(op_err)?;
    Ok((StatusCode::CREATED, Json(t)))
}

async fn get_tenant(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(
        s.platform
            .tenant(TenantId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusBody<T> {
    status: T,
}

async fn set_tenant_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<TenantStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_tenant_status(&op, TenantId::from_uuid(id), b.status)
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewApplicationBody {
    slug: String,
    display_name: String,
}

async fn create_application(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(tenant_id): Path<Uuid>,
    Json(b): Json<NewApplicationBody>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    let a = s
        .platform
        .operator_create_application(
            &op,
            TenantId::from_uuid(tenant_id),
            NewApplication {
                slug: b.slug,
                display_name: b.display_name,
            },
        )
        .await
        .map_err(op_err)?;
    Ok((StatusCode::CREATED, Json(a)))
}

async fn get_application(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(
        s.platform
            .application(ApplicationId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

async fn set_application_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<ApplicationStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_application_status(&op, ApplicationId::from_uuid(id), b.status)
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KindBody {
    kind: EnvironmentKind,
}

async fn create_environment(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(b): Json<KindBody>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    if b.kind == EnvironmentKind::Production {
        return Err(policy(
            "PRODUCTION environments are activated by the production-access approval workflow",
        ));
    }
    let e = s
        .platform
        .operator_create_environment(&op, ApplicationId::from_uuid(application_id), b.kind)
        .await
        .map_err(op_err)?;
    Ok((StatusCode::CREATED, Json(e)))
}

async fn list_environments(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(json!({
        "environments": s.platform
            .list_environments(ApplicationId::from_uuid(application_id))
            .await
            .map_err(op_err)?
    })))
}

async fn list_scopes(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(application_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(json!({
        "scopes": s.platform
            .list_application_scopes(ApplicationId::from_uuid(application_id))
            .await
            .map_err(op_err)?
    })))
}

async fn get_environment(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(
        s.platform
            .environment(EnvironmentId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

async fn set_environment_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<EnvironmentStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    let id = EnvironmentId::from_uuid(id);
    let current = s.platform.environment(id).await.map_err(op_err)?;
    if current.kind == EnvironmentKind::Production {
        return Err(policy(
            "the status of a PRODUCTION environment is an operator / production-access decision",
        ));
    }
    Ok(Json(
        s.platform
            .operator_set_environment_status(&op, id, b.status)
            .await
            .map_err(op_err)?,
    ))
}

// ---------------------------------------------------------------------------
// Service accounts (Part VI §50: the Developer Platform delegates create / suspend)
// ---------------------------------------------------------------------------

/// Refuses delegated service-account changes in a PRODUCTION environment: like the
/// environment itself, its machine identities are an operator / production-access
/// decision (Section 55).
async fn non_production(s: &AppState, environment_id: EnvironmentId) -> AccessResult<()> {
    let environment = s
        .platform
        .environment(environment_id)
        .await
        .map_err(op_err)?;
    if environment.kind == EnvironmentKind::Production {
        return Err(policy(
            "service accounts of a PRODUCTION environment are an operator / production-access decision",
        ));
    }
    Ok(())
}

/// The service account `id` of exactly `environment_id` (any other: NOT_FOUND).
async fn scoped_service_account(
    s: &AppState,
    environment_id: EnvironmentId,
    id: ServiceAccountId,
) -> AccessResult<ServiceAccountView> {
    let sa = s.platform.service_account(id).await.map_err(op_err)?;
    if sa.environment_id != environment_id {
        return Err(op_err(crate::identity::PlatformError::NotFound(
            "service account",
        )));
    }
    Ok(sa)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewServiceAccountBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_service_account(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
    Json(b): Json<NewServiceAccountBody>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    let environment_id = EnvironmentId::from_uuid(environment_id);
    non_production(&s, environment_id).await?;
    let sa = s
        .platform
        .operator_create_service_account(
            &op,
            environment_id,
            NewServiceAccount {
                name: b.name,
                description: b.description,
            },
        )
        .await
        .map_err(op_err)?;
    Ok((StatusCode::CREATED, Json(sa)))
}

async fn list_service_accounts(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(environment_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    let environment_id = EnvironmentId::from_uuid(environment_id);
    // An unknown environment is NOT_FOUND, never an empty list.
    s.platform
        .environment(environment_id)
        .await
        .map_err(op_err)?;
    Ok(Json(json!({
        "serviceAccounts": s.platform
            .list_service_accounts(environment_id)
            .await
            .map_err(op_err)?
    })))
}

async fn get_service_account(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((environment_id, id)): Path<(Uuid, Uuid)>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(
        scoped_service_account(
            &s,
            EnvironmentId::from_uuid(environment_id),
            ServiceAccountId::from_uuid(id),
        )
        .await?,
    ))
}

async fn set_service_account_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((environment_id, id)): Path<(Uuid, Uuid)>,
    Json(b): Json<StatusBody<ServiceAccountStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = delegated(&caller, &headers)?;
    let environment_id = EnvironmentId::from_uuid(environment_id);
    let sa = scoped_service_account(&s, environment_id, ServiceAccountId::from_uuid(id)).await?;
    if b.status == ServiceAccountStatus::Revoked {
        return Err(policy(
            "service-account revocation is an operator decision; delegation suspends or resumes",
        ));
    }
    non_production(&s, environment_id).await?;
    Ok(Json(
        s.platform
            .operator_set_service_account_status(&op, sa.id, b.status)
            .await
            .map_err(op_err)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::valid_actor;

    #[test]
    fn actors_are_opaque_member_references() {
        assert!(valid_actor(&format!("u-{}", "a".repeat(32))));
        for bad in [
            "",
            "u-",
            "operator",
            &format!("u-{}", "A".repeat(32)),
            &format!("x-{}", "a".repeat(32)),
        ] {
            assert!(!valid_actor(bad), "{bad}");
        }
    }
}
