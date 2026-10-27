//! Operator administration API (`/internal/v1/admin/**`).
//!
//! Reachable only with a service token whose issuer this service's trust bundle grants
//! `platform-identity.administer` (registry: operator tooling, `cfmeet.operator-console`,
//! only) AND a named human operator in `x-cf-operator`; every mutation goes through the
//! audited `operator_*` domain operations (one audit row in the mutation's transaction).
//! It is the registration surface of the identity hierarchy; the full developer portal
//! (Part VI) is out of scope.
//!
//! Issued and rotated credentials return their raw secret exactly once, in the response
//! body of that request (`Cache-Control: no-store`); it is never stored or logged.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{
    domain::{
        ApplicationId, ApplicationStatus, CredentialId, EnvironmentId, EnvironmentKind,
        EnvironmentStatus, PrincipalId, PrincipalKind, PrincipalStatus, ServiceAccountId,
        ServiceAccountStatus, TenantClassification, TenantId, TenantStatus,
    },
    identity::{
        applications::NewApplication,
        credentials::{CredentialFilter, IssueCredential, IssuedCredential, RotateCredential},
        operator::OperatorContext,
        principals::NewPrincipal,
        service_accounts::NewServiceAccount,
        tenants::NewTenant,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/tenants", post(create_tenant))
        .route("/admin/tenants/:tenant_id", get(get_tenant))
        .route("/admin/tenants/:tenant_id/status", post(set_tenant_status))
        .route(
            "/admin/tenants/:tenant_id/applications",
            post(create_application),
        )
        .route("/admin/applications/:application_id", get(get_application))
        .route(
            "/admin/applications/:application_id/status",
            post(set_application_status),
        )
        .route(
            "/admin/applications/:application_id/environments",
            post(create_environment),
        )
        .route(
            "/admin/applications/:application_id/scopes",
            post(grant_scope).get(list_scopes),
        )
        .route(
            "/admin/applications/:application_id/scopes/:scope",
            axum::routing::delete(revoke_scope),
        )
        .route(
            "/admin/environments/:environment_id/status",
            post(set_environment_status),
        )
        .route(
            "/admin/environments/:environment_id/live-telecom",
            post(set_live_telecom),
        )
        .route(
            "/admin/environments/:environment_id/service-accounts",
            post(create_service_account),
        )
        .route(
            "/admin/environments/:environment_id/principals",
            post(register_principal).get(list_principals),
        )
        .route(
            "/admin/service-accounts/:service_account_id/status",
            post(set_service_account_status),
        )
        .route(
            "/admin/service-accounts/:service_account_id/credentials",
            post(issue_credential).get(list_credentials),
        )
        .route("/admin/credentials/:credential_id", get(get_credential))
        .route(
            "/admin/credentials/:credential_id/rotate",
            post(rotate_credential),
        )
        .route(
            "/admin/credentials/:credential_id/revoke",
            post(revoke_credential),
        )
        .route(
            "/admin/credentials/:credential_id/suspend",
            post(suspend_credential),
        )
        .route(
            "/admin/credentials/:credential_id/resume",
            post(resume_credential),
        )
        .route(
            "/admin/credentials/:credential_id/principal-restriction",
            put(restrict_credential),
        )
        .route(
            "/admin/principals/:principal_id/status",
            post(set_principal_status),
        )
        .route(
            "/admin/principals/:principal_id/tombstone",
            post(tombstone_principal),
        )
}

/// Authorizes an administration request: permission + named operator.
fn operator(caller: &InternalCaller, headers: &HeaderMap) -> AccessResult<OperatorContext> {
    caller.require_permission(InternalPermission::PlatformIdentityAdminister)?;
    let name = headers
        .get("x-cf-operator")
        .and_then(|v| v.to_str().ok())
        .ok_or(AccessError::Refused(ErrorCode::OperatorRequired))?;
    OperatorContext::new(name).map_err(|_| AccessError::Refused(ErrorCode::OperatorRequired))
}

fn read_only(caller: &InternalCaller) -> AccessResult<()> {
    caller.require_permission(InternalPermission::PlatformIdentityAdminister)
}

fn op_err(e: crate::identity::PlatformError) -> AccessError {
    AccessError::operator(e)
}

fn created<T: serde::Serialize>(value: T) -> impl IntoResponse {
    (StatusCode::CREATED, Json(value))
}

fn issued_json(issued: IssuedCredential) -> serde_json::Value {
    json!({
        "credential": issued.credential,
        // Shown exactly once; never persisted, never logged.
        "secret": issued.secret.expose_once(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewTenantBody {
    slug: String,
    display_name: String,
    classification: TenantClassification,
    #[serde(default)]
    billing_currency: Option<String>,
}

async fn create_tenant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(b): Json<NewTenantBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let t = s
        .platform
        .operator_create_tenant(
            &op,
            NewTenant {
                slug: b.slug,
                display_name: b.display_name,
                classification: b.classification,
                billing_currency: b.billing_currency,
            },
        )
        .await
        .map_err(op_err)?;
    Ok(created(t))
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
    let op = operator(&caller, &headers)?;
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
    let op = operator(&caller, &headers)?;
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
    Ok(created(a))
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
    let op = operator(&caller, &headers)?;
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
    let op = operator(&caller, &headers)?;
    let e = s
        .platform
        .operator_create_environment(&op, ApplicationId::from_uuid(application_id), b.kind)
        .await
        .map_err(op_err)?;
    Ok(created(e))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantScopeBody {
    scope: String,
    #[serde(default)]
    environment_kinds: Option<Vec<EnvironmentKind>>,
    /// An explicit PRIVILEGED operator grant (`numbers.admin`).
    #[serde(default)]
    privileged: bool,
}

async fn grant_scope(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(application_id): Path<Uuid>,
    Json(b): Json<GrantScopeBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let id = ApplicationId::from_uuid(application_id);
    let view = if b.privileged {
        s.platform
            .operator_grant_privileged_scope(&op, id, &b.scope, b.environment_kinds.as_deref())
            .await
    } else {
        s.platform
            .operator_grant_application_scope(&op, id, &b.scope, b.environment_kinds.as_deref())
            .await
    }
    .map_err(op_err)?;
    Ok(Json(view))
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

async fn revoke_scope(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((application_id, scope)): Path<(Uuid, String)>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let removed = s
        .platform
        .operator_revoke_application_scope(&op, ApplicationId::from_uuid(application_id), &scope)
        .await
        .map_err(op_err)?;
    Ok(Json(json!({ "removed": removed })))
}

async fn set_environment_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<EnvironmentStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_environment_status(&op, EnvironmentId::from_uuid(id), b.status)
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedBody {
    allowed: bool,
}

async fn set_live_telecom(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<AllowedBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_live_telecom_allowed(&op, EnvironmentId::from_uuid(id), b.allowed)
            .await
            .map_err(op_err)?,
    ))
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
    let op = operator(&caller, &headers)?;
    let sa = s
        .platform
        .operator_create_service_account(
            &op,
            EnvironmentId::from_uuid(environment_id),
            NewServiceAccount {
                name: b.name,
                description: b.description,
            },
        )
        .await
        .map_err(op_err)?;
    Ok(created(sa))
}

async fn set_service_account_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<ServiceAccountStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_service_account_status(&op, ServiceAccountId::from_uuid(id), b.status)
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IssueBody {
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    not_before: Option<DateTime<Utc>>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

async fn issue_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(service_account_id): Path<Uuid>,
    Json(b): Json<IssueBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let issued = s
        .platform
        .operator_issue_credential(
            &op,
            ServiceAccountId::from_uuid(service_account_id),
            IssueCredential {
                scopes: b.scopes,
                label: b.label,
                not_before: b.not_before,
                expires_at: b.expires_at,
            },
        )
        .await
        .map_err(op_err)?;
    Ok(created(issued_json(issued)))
}

async fn list_credentials(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(service_account_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(json!({
        "credentials": s.platform
            .list_credentials(CredentialFilter::ServiceAccount(ServiceAccountId::from_uuid(service_account_id)))
            .await
            .map_err(op_err)?
    })))
}

async fn get_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(
        s.platform
            .credential(CredentialId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RotateBody {
    #[serde(default)]
    overlap_minutes: Option<i64>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

async fn rotate_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<RotateBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let issued = s
        .platform
        .operator_rotate_credential(
            &op,
            CredentialId::from_uuid(id),
            RotateCredential {
                overlap: b.overlap_minutes.map(Duration::minutes),
                scopes: b.scopes,
                label: b.label,
                expires_at: b.expires_at,
            },
        )
        .await
        .map_err(op_err)?;
    Ok(created(issued_json(issued)))
}

async fn revoke_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_revoke_credential(&op, CredentialId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SuspendBody {
    reason_code: String,
}

/// Part VI §54: reversible credential suspension (audited).
async fn suspend_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<SuspendBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_suspend_credential(&op, CredentialId::from_uuid(id), &b.reason_code)
            .await
            .map_err(op_err)?,
    ))
}

async fn resume_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_resume_credential(&op, CredentialId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestrictionBody {
    principal_ids: Vec<Uuid>,
}

async fn restrict_credential(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<RestrictionBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let ids: Vec<PrincipalId> = b
        .principal_ids
        .into_iter()
        .map(PrincipalId::from_uuid)
        .collect();
    let restricted = s
        .platform
        .operator_restrict_credential_principals(&op, CredentialId::from_uuid(id), &ids)
        .await
        .map_err(op_err)?;
    Ok(Json(json!({
        "principalIds": restricted.iter().map(|p| p.as_uuid()).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterPrincipalBody {
    #[serde(default)]
    kind: Option<PrincipalKind>,
    #[serde(default)]
    external_reference: Option<String>,
    #[serde(default)]
    service_account_id: Option<Uuid>,
}

async fn register_principal(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
    Json(b): Json<RegisterPrincipalBody>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    let new = match (b.kind, b.external_reference, b.service_account_id) {
        (Some(kind), Some(external_reference), None) => NewPrincipal::External {
            kind,
            external_reference,
        },
        (None, None, Some(sa)) => NewPrincipal::ServiceAccount(ServiceAccountId::from_uuid(sa)),
        _ => {
            return Err(AccessError::Operator(
                ErrorCode::ValidationError,
                "give either kind + externalReference or serviceAccountId".to_string(),
            ))
        }
    };
    let (principal, was_created) = s
        .platform
        .operator_register_principal(&op, EnvironmentId::from_uuid(environment_id), new)
        .await
        .map_err(op_err)?;
    Ok((
        if was_created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({ "principal": principal, "created": was_created })),
    ))
}

async fn list_principals(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(environment_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    read_only(&caller)?;
    Ok(Json(json!({
        "principals": s.platform
            .list_principals(EnvironmentId::from_uuid(environment_id))
            .await
            .map_err(op_err)?
    })))
}

async fn set_principal_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StatusBody<PrincipalStatus>>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_set_principal_status(&op, PrincipalId::from_uuid(id), b.status)
            .await
            .map_err(op_err)?,
    ))
}

async fn tombstone_principal(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    let op = operator(&caller, &headers)?;
    Ok(Json(
        s.platform
            .operator_tombstone_principal(&op, PrincipalId::from_uuid(id))
            .await
            .map_err(op_err)?,
    ))
}
