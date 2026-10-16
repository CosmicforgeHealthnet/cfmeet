//! Internal API (service tokens): the canonical, provider-independent view of a Number
//! Resource for Calling / Telephony (`number-resource.read`). Provider identifiers and
//! external references never leave Numbering: bindings appear as opaque binding refs.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{error::ApiError, middleware::InternalCaller};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/number-resources/:number_resource_id",
            get(number_resource),
        )
        .route(
            "/number-resources/:number_resource_id/capabilities",
            get(number_resource_capabilities),
        )
        .route(
            "/assignments/:assignment_id/kyc-status",
            get(assignment_kyc_status),
        )
}

/// Whether an assignment's KYC basis is in force (Workstream F), for consumers that must
/// not act on a number whose approval expired or was revoked.
async fn assignment_kyc_status(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberResourceRead)?;
    let status = s.kyc.assignment_kyc_status(id).await?;
    Ok(Json(serde_json::to_value(status).expect("serializable")))
}

async fn number_resource(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberResourceRead)?;
    let r = s.numbers.resource(id).await?;
    let capabilities = s.numbers.current_capabilities(id).await?;
    let bindings = s.numbers.bindings(id).await?;
    Ok(Json(json!({
        "numberResourceId": r.id,
        "e164": r.e164,
        "countryCode": r.country_code,
        "region": r.region,
        "numberType": r.number_type,
        "inventorySource": r.inventory_source,
        "state": r.state,
        "revision": r.revision,
        "stateChangedAt": r.state_changed_at,
        "capabilities": capabilities.iter().map(|c| json!({
            "capability": c.capability,
            "status": c.status,
            "source": c.source,
            "effectiveFrom": c.effective_from,
        })).collect::<Vec<_>>(),
        "activeBindings": bindings.iter().filter(|b| b.status == "ACTIVE").map(|b| b.public_ref()).collect::<Vec<_>>(),
        "bindingRevision": r.binding_revision,
        "routingRevision": r.routing_revision,
        "riskTier": r.risk_tier,
        "restrictionFlags": r.restriction_flags,
    })))
}

/// The resolved capabilities of a Number Resource (Workstream E), with provenance and
/// revisions consumers compare to reject stale decisions.
async fn number_resource_capabilities(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberResourceRead)?;
    Ok(Json(super::catalog::resolution_view(
        &s.numbers.resolve_capabilities(id).await?,
    )))
}
