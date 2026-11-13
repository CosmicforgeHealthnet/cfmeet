//! Governed FIRST_PARTY organization scopes (`/internal/v1/organization-scopes/**`,
//! Blueprint v1.0 Section 7, ADR 0058).
//!
//! | Route | Rule |
//! |---|---|
//! | `PUT /organization-scopes/{organizationRef}/{capability}` | create (201) or return (200) the organization's FIRST_PARTY tenant + first-party application + PRODUCTION environment; idempotent on the path |
//! | `GET /organization-scopes/{organizationRef}/{capability}` | read back for server-side verification (404 when never provisioned) |
//! | `POST /organization-scopes/{organizationRef}/{capability}/status` | suspend / resume the organization's tenant |
//! | `PUT /organization-scopes/{organizationRef}/{capability}/service-bindings/{service}` | create (201) or return / renew (200) the organization's `calling` / `messaging` service account + credential inside its own triple (ADR 0058 Amendment A2) |
//! | `GET …/service-bindings/{service}` | read back with the server-side facts (404 when never bound) |
//! | `POST …/service-bindings/{service}/rotate` | successor credential; the predecessor stops authenticating at once |
//! | `DELETE …/service-bindings/{service}` | terminal revocation (credential revoked, service account REVOKED); idempotent |
//!
//! The scope routes are reachable only with `platform-identity.provision-first-party`
//! (registry-grantable to `cfmeet.organizations` only) AND only from the authenticated
//! `cfmeet.organizations` identity; they issue no credential, create no service account or
//! principal and grant no scope. The binding routes require
//! `platform-identity.bind-organization-services` (same Organizations-only rule) and act
//! only inside the organization's recorded triple, with the closed per-service scope set
//! of `identity::organization_bindings`; they never return a credential secret. Every
//! committed change is audited with operator id `cfmeet.organizations` and the
//! organization id as an opaque reference. EXTERNAL tenants and developer workspaces are
//! never reachable (a scope row only ever points at a tenant the scope path created).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{post, put},
    Json, Router,
};

use crate::identity::organization_bindings::BindingOutcome;
use cf_service_auth::{services, InternalPermission};
use serde::Deserialize;
use uuid::Uuid;

use super::error::{AccessError, AccessResult, ErrorCode};
use super::middleware::InternalCaller;
use crate::{
    domain::TenantStatus,
    identity::{operator::OperatorContext, PlatformError},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organization-scopes/:organization_ref/:capability",
            put(provision).get(read),
        )
        .route(
            "/organization-scopes/:organization_ref/:capability/status",
            post(set_status),
        )
        .route(
            "/organization-scopes/:organization_ref/:capability/service-bindings/:service",
            put(ensure_binding).get(read_binding).delete(revoke_binding),
        )
        .route(
            "/organization-scopes/:organization_ref/:capability/service-bindings/:service/rotate",
            post(rotate_binding),
        )
}

/// ADR 0058 Amendment A2: the binding routes need their own permission AND the
/// Organizations identity (the registry already makes the permission grantable to
/// Organizations only).
fn authorize_bindings(caller: &InternalCaller) -> AccessResult<OperatorContext> {
    caller.require_permission(InternalPermission::PlatformIdentityBindOrganizationServices)?;
    if caller.service() != services::ORGANIZATIONS {
        tracing::warn!(
            caller = caller.service(),
            "organization binding call from a non-Organizations identity refused"
        );
        return Err(AccessError::Refused(ErrorCode::InternalPermissionDenied));
    }
    OperatorContext::new(caller.service())
        .map_err(|_| AccessError::Refused(ErrorCode::InternalPermissionDenied))
}

async fn ensure_binding(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability, service)): Path<(Uuid, String, String)>,
) -> AccessResult<impl IntoResponse> {
    let op = authorize_bindings(&caller)?;
    let (binding, outcome) = s
        .platform
        .ensure_organization_binding(
            &op,
            caller.service(),
            organization_ref,
            &capability,
            &service,
        )
        .await
        .map_err(op_err)?;
    Ok((
        if outcome == BindingOutcome::Created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(binding.to_json()),
    ))
}

async fn read_binding(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability, service)): Path<(Uuid, String, String)>,
) -> AccessResult<impl IntoResponse> {
    authorize_bindings(&caller)?;
    let binding = s
        .platform
        .organization_binding(organization_ref, &capability, &service)
        .await
        .map_err(op_err)?
        .ok_or_else(|| op_err(PlatformError::NotFound("organization binding")))?;
    Ok(Json(binding.to_json()))
}

async fn rotate_binding(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability, service)): Path<(Uuid, String, String)>,
) -> AccessResult<impl IntoResponse> {
    let op = authorize_bindings(&caller)?;
    let binding = s
        .platform
        .rotate_organization_binding(&op, organization_ref, &capability, &service)
        .await
        .map_err(op_err)?;
    Ok(Json(binding.to_json()))
}

async fn revoke_binding(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability, service)): Path<(Uuid, String, String)>,
) -> AccessResult<impl IntoResponse> {
    let op = authorize_bindings(&caller)?;
    let binding = s
        .platform
        .revoke_organization_binding(&op, organization_ref, &capability, &service)
        .await
        .map_err(op_err)?;
    Ok(Json(binding.to_json()))
}

/// The permission AND the Organizations identity (defence in depth over the registry,
/// which already makes the permission grantable to Organizations only).
fn authorize(caller: &InternalCaller) -> AccessResult<OperatorContext> {
    caller.require_permission(InternalPermission::PlatformIdentityProvisionFirstParty)?;
    if caller.service() != services::ORGANIZATIONS {
        tracing::warn!(
            caller = caller.service(),
            "organization scope call from a non-Organizations identity refused"
        );
        return Err(AccessError::Refused(ErrorCode::InternalPermissionDenied));
    }
    OperatorContext::new(caller.service())
        .map_err(|_| AccessError::Refused(ErrorCode::InternalPermissionDenied))
}

fn op_err(e: PlatformError) -> AccessError {
    AccessError::operator(e)
}

async fn provision(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability)): Path<(Uuid, String)>,
) -> AccessResult<impl IntoResponse> {
    let op = authorize(&caller)?;
    let (scope, created) = s
        .platform
        .provision_organization_scope(&op, caller.service(), organization_ref, &capability)
        .await
        .map_err(op_err)?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(scope.to_json()),
    ))
}

async fn read(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability)): Path<(Uuid, String)>,
) -> AccessResult<impl IntoResponse> {
    authorize(&caller)?;
    let scope = s
        .platform
        .organization_scope(organization_ref, &capability)
        .await
        .map_err(op_err)?
        .ok_or_else(|| op_err(PlatformError::NotFound("organization scope")))?;
    Ok(Json(scope.to_json()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusBody {
    status: TenantStatus,
}

async fn set_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path((organization_ref, capability)): Path<(Uuid, String)>,
    Json(b): Json<StatusBody>,
) -> AccessResult<impl IntoResponse> {
    let op = authorize(&caller)?;
    let scope = s
        .platform
        .set_organization_scope_status(&op, organization_ref, &capability, b.status)
        .await
        .map_err(op_err)?;
    Ok(Json(scope.to_json()))
}
