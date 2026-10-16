//! Internal API (service tokens): the canonical, provider-independent view of a Number
//! Resource for Calling / Telephony (`number-resource.read`). Provider identifiers and
//! external references never leave Numbering: bindings appear as opaque binding refs.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
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
        .route(
            "/number-resources/:number_resource_id/routes",
            get(number_routes),
        )
        .route(
            "/number-resources/:number_resource_id/route-readiness",
            post(acknowledge_route),
        )
        .merge(super::grants::routes())
}

/// The current provider route configuration of every family of a number, with consumer
/// readiness (Workstream I): what a consumer needs to acknowledge a route revision.
async fn number_routes(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    if caller
        .context()
        .require_permission(InternalPermission::NumberRouteAcknowledge)
        .is_err()
    {
        caller.require_permission(InternalPermission::NumberResourceRead)?;
    }
    let resource = s.numbers.resource(id).await?;
    let routes = s.numbers.current_routes(id).await?;
    let caps: Vec<crate::numbers::Capability> = s
        .numbers
        .bindings(id)
        .await?
        .into_iter()
        .filter(|b| b.status == "ACTIVE")
        .flat_map(|b| b.capabilities)
        .filter_map(|c| crate::numbers::Capability::parse(&c))
        .collect();
    let readiness = s.numbers.activation_readiness(id, &caps).await?;
    Ok(Json(json!({
        "numberResourceId": resource.id,
        "bindingRevision": resource.binding_revision,
        "routingRevision": resource.routing_revision,
        "synthetic": resource.inventory_source == "SYNTHETIC_TEST",
        "routes": routes.iter().map(|r| json!({
            "family": r.family,
            "routeRef": r.route_ref,
            "routeRevision": r.route_revision,
            "bindingRevision": r.binding_revision,
            "status": r.status,
            "acknowledgedAt": r.acknowledged_at,
        })).collect::<Vec<_>>(),
        "readiness": readiness.iter().map(|f| json!({
            "family": f.family,
            "ready": f.ready,
            "reason": f.reason,
            "acknowledgedBy": f.acknowledged_by,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadinessBody {
    family: crate::numbers::acquisitions::RouteFamily,
    route_ref: Uuid,
    route_revision: i64,
}

/// The consumer of a family (Telephony: voice, SMS Gateway: SMS) acknowledges that it
/// routes a CONFIGURED revision (`number-route.acknowledge`).
async fn acknowledge_route(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
    Json(body): Json<ReadinessBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::NumberRouteAcknowledge)?;
    let ctx =
        crate::domain::current_context().unwrap_or_else(crate::domain::RequestContext::generate);
    let row = s
        .numbers
        .acknowledge_route(
            &ctx,
            caller.service(),
            id,
            body.family,
            body.route_ref,
            body.route_revision,
        )
        .await?;
    Ok(Json(json!({
        "numberResourceId": row.number_resource_id,
        "family": row.family,
        "routeRevision": row.route_revision,
        "consumerService": row.consumer_service,
        "status": row.status,
        "acknowledgedAt": row.acknowledged_at,
    })))
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
