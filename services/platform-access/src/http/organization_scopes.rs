//! Governed FIRST_PARTY organization scopes (`/internal/v1/organization-scopes/**`,
//! Blueprint v1.0 Section 7, ADR 0058).
//!
//! | Route | Rule |
//! |---|---|
//! | `PUT /organization-scopes/{organizationRef}/{capability}` | create (201) or return (200) the organization's FIRST_PARTY tenant + first-party application + PRODUCTION environment; idempotent on the path |
//! | `GET /organization-scopes/{organizationRef}/{capability}` | read back for server-side verification (404 when never provisioned) |
//! | `POST /organization-scopes/{organizationRef}/{capability}/status` | suspend / resume the organization's tenant |
//!
//! Reachable only with `platform-identity.provision-first-party` (registry-grantable to
//! `cfmeet.organizations` only) AND only from the authenticated `cfmeet.organizations`
//! identity. Every committed change is audited with operator id `cfmeet.organizations`
//! and the organization id as an opaque reference. Nothing here issues a credential,
//! creates a service account or principal, or grants a scope; EXTERNAL tenants and
//! developer workspaces are never reachable (a scope row only ever points at a tenant this
//! path created).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{post, put},
    Json, Router,
};
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
