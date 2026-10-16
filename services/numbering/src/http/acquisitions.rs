//! Provisioning and activation prerequisites (Workstream I), in the caller's TRUSTED
//! scope:
//!
//! * `POST /numbers/acquisitions` (`numbers.manage`, `Idempotency-Key` required) —
//!   provision a reservation (202, PROVISIONING until the provider's acquisition is
//!   proven); eligibility is re-evaluated, the provider hold read back;
//! * `GET  /numbers/acquisitions`, `GET /numbers/acquisitions/{acquisitionId}`
//!   (`numbers.read`) — status, served capabilities and per-family route readiness;
//! * `POST /numbers/acquisitions/{acquisitionId}/routes` (`numbers.manage`,
//!   `Idempotency-Key` required) — provider voice / SMS route configuration.
//!
//! A PROVISIONED acquisition is a number the platform acquired, not an active customer
//! number: activation is an assignment (Workstream J) once routes are ready.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    customer::NUMBERS_READ,
    error::{ApiError, ErrorCode},
    middleware::CustomerCaller,
    numbers::{accepted, ctx, required_idempotency_key, NUMBERS_MANAGE},
};
use crate::{
    acquisition::provisioning::{AcquisitionStatusView, ProvisionRequest},
    identity::TrustedContext,
    numbers::{eligibility::Standing, Capability},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/numbers/acquisitions",
            get(list_acquisitions).post(create_acquisition),
        )
        .route(
            "/numbers/acquisitions/:acquisition_id",
            get(get_acquisition),
        )
        .route(
            "/numbers/acquisitions/:acquisition_id/routes",
            post(configure_routes),
        )
}

/// The caller's trusted standing (from the Platform Access authority chain).
pub(crate) fn standing(tc: &TrustedContext) -> Standing {
    Standing {
        active: tc.full_authority,
        first_party: tc.first_party,
        live_telecom_allowed: tc.live_telecom_allowed,
    }
}

/// The customer view of an acquisition (contract `Acquisition`). Provider identifiers and
/// route references never appear.
pub fn acquisition_view(v: &AcquisitionStatusView) -> Value {
    let a = &v.acquisition;
    json!({
        "acquisitionId": a.id,
        "reservationId": a.reservation_id,
        "numberResourceId": a.number_resource_id,
        "status": a.status,
        "revision": a.revision,
        "requestedCapabilities": a.requested_capabilities,
        "eligibilityDecisionId": a.eligibility_decision_ref,
        "failureCode": a.failure_code,
        "provisioning": v.provisioning.as_ref().map(|c| json!({
            "status": c.status,
            "attempts": c.attempts,
            "failureCode": c.failure_code,
        })),
        "servedCapabilities": v.served_capabilities,
        "routes": v.routes.iter().map(|r| json!({
            "family": r.family,
            "routeRevision": r.route_revision,
            "routeStatus": r.route_status,
            "acknowledgedBy": r.acknowledged_by,
            "ready": r.ready,
            "reason": r.reason,
        })).collect::<Vec<_>>(),
        "activationReady": v.activation_ready(),
        "liveRouting": v.live_routing,
        "createdAt": a.created_at,
        "updatedAt": a.updated_at,
        "completedAt": a.completed_at,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAcquisitionBody {
    pub reservation_id: Uuid,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub eligibility_decision_id: Option<Uuid>,
}

async fn create_acquisition(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateAcquisitionBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let key = required_idempotency_key(&headers)?;
    let a = s.acquisition();
    let out = a
        .provision(
            &ctx(),
            &tc.owner_scope(),
            standing(tc),
            ProvisionRequest {
                reservation_id: body.reservation_id,
                capabilities: body.capabilities,
                eligibility_decision_id: body.eligibility_decision_id,
            },
            &key,
        )
        .await?;
    let view = a.status_view(out.acquisition).await?;
    Ok(accepted(out.replayed, acquisition_view(&view)))
}

async fn get_acquisition(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    let view = s
        .acquisition()
        .acquisition_status(&tc.owner_scope(), id)
        .await?;
    Ok(Json(acquisition_view(&view)))
}

fn decode_cursor(cursor: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    let bad = || ApiError::Code(ErrorCode::ValidationError);
    if cursor.len() > 200 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| bad())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    let at = v["t"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .ok_or_else(bad)?
        .with_timezone(&Utc);
    let id = v["i"]
        .as_str()
        .and_then(|i| Uuid::parse_str(i).ok())
        .ok_or_else(bad)?;
    Ok((at, id))
}

async fn list_acquisitions(
    State(s): State<AppState>,
    caller: CustomerCaller,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let tc = caller.require_scope(NUMBERS_READ)?;
    if params
        .keys()
        .any(|k| !["limit", "cursor"].contains(&k.as_str()))
    {
        return Err(ApiError::Code(ErrorCode::ValidationError));
    }
    let limit = match params.get("limit") {
        None => 50,
        Some(l) => l
            .parse::<u64>()
            .ok()
            .filter(|l| (1..=100).contains(l))
            .ok_or(ApiError::Code(ErrorCode::ValidationError))?,
    };
    let before = params.get("cursor").map(|c| decode_cursor(c)).transpose()?;
    let a = s.acquisition();
    let rows = s
        .numbers
        .acquisitions(&tc.owner_scope(), before, limit + 1)
        .await?;
    let more = rows.len() as u64 > limit;
    let mut data = Vec::new();
    let mut last = None;
    for row in rows.into_iter().take(limit as usize) {
        last = Some((row.created_at, row.id));
        data.push(acquisition_view(&a.status_view(row).await?));
    }
    let next = more.then(|| {
        let (t, i) = last.expect("a full page");
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({ "t": t, "i": i }).to_string())
    });
    Ok(Json(json!({ "data": data, "nextCursor": next })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureRoutesBody {
    pub capabilities: Vec<Capability>,
}

async fn configure_routes(
    State(s): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ConfigureRoutesBody>,
) -> Result<Response, ApiError> {
    let tc = caller.require_scope(NUMBERS_MANAGE)?;
    let key = required_idempotency_key(&headers)?;
    let a = s.acquisition();
    let (_, replayed) = a
        .configure_routes(&ctx(), &tc.owner_scope(), id, &body.capabilities, &key)
        .await?;
    let view = a.acquisition_status(&tc.owner_scope(), id).await?;
    Ok(accepted(replayed, acquisition_view(&view)))
}
