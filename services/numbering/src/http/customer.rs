//! Customer reads (`numbers.read`): the caller's OWN assignment history and
//! reservations (assignment workflows: [`super::assignments`]) — always filtered by the trusted scope from Platform Access, so another
//! tenant's / environment's records are indistinguishable from missing ones. Provider
//! identifiers never appear.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{error::ApiError, middleware::CustomerCaller};
use crate::{
    numbers::{assignments::AssignmentRow, reservations::ReservationRow},
    state::AppState,
};

pub const NUMBERS_READ: &str = "numbers.read";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/numbers/assignments/:assignment_id/history",
            get(assignment_history),
        )
        .route(
            "/numbers/reservations/:reservation_id",
            get(get_reservation),
        )
        .merge(super::numbers::routes())
        .merge(super::assignments::routes())
        .merge(super::acquisitions::routes())
        .merge(super::catalog::customer_routes())
        .merge(super::catalog::admin_routes())
        .merge(super::kyc::customer_routes())
        .merge(super::kyc::admin_routes())
}

/// The customer view of an assignment (contract `cfmeet.numbering.common`
/// `DcnAssignment`).
pub fn assignment_view(a: &AssignmentRow) -> Value {
    json!({
        "assignmentId": a.id,
        "numberResourceId": a.number_resource_id,
        "status": a.status,
        "revision": a.revision,
        "ownership": {
            "ownershipClass": a.ownership_class,
            "ownerKind": a.owner_kind,
            "ownerRef": a.owner_ref,
        },
        "routing": a.routing_profile_id.map(|id| json!({
            "routingProfileId": id,
            "revision": a.routing_profile_revision,
        })),
        "capabilitySnapshot": a.capability_snapshot,
        "eligibilityDecisionRef": a.eligibility_decision_ref,
        "requestedAt": a.requested_at,
        "activatedAt": a.activated_at,
        "closedAt": a.closed_at,
        "closeReason": a.close_reason,
        "predecessorAssignmentId": a.predecessor_assignment_id,
        "successorAssignmentId": a.successor_assignment_id,
    })
}

pub fn reservation_view(r: &ReservationRow) -> Value {
    json!({
        "reservationId": r.id,
        "numberResourceId": r.number_resource_id,
        "status": r.status,
        "revision": r.revision,
        "expiresAt": r.expires_at,
        "providerReleaseState": r.provider_release_state,
        "holdBasis": r.hold_basis,
        "eligibilityDecisionId": r.eligibility_decision_ref,
        "requestedCapabilities": r.requested_capabilities,
        "convertedTo": r.converted_to,
        "createdAt": r.created_at,
        "endedAt": r.ended_at,
    })
}

async fn assignment_history(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(NUMBERS_READ)?;
    let rows = s.numbers.assignment_history(&ctx.owner_scope(), id).await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|h| {
            json!({
                "sequence": h.sequence,
                "eventKind": h.event_kind,
                "fromStatus": h.from_status,
                "toStatus": h.to_status,
                "revision": h.assignment_revision,
                "ownershipClass": h.ownership_class,
                "routingProfileId": h.routing_profile_id,
                "routingProfileRevision": h.routing_profile_revision,
                "actorKind": h.actor_kind,
                "reasonCode": h.reason_code,
                "occurredAt": h.occurred_at,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data })))
}

async fn get_reservation(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(NUMBERS_READ)?;
    Ok(Json(reservation_view(
        &s.numbers.reservation(&ctx.owner_scope(), id).await?,
    )))
}
