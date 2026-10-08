//! Calling's internal Telephony routes (service authentication, `/internal/v1`):
//!
//! * `POST /telephony/events` — normalized Telephony events (`telephony-event.apply`),
//!   deduplicated and applied by Calling's own state machines. Every delivered event is
//!   answered 200 with its disposition (APPLIED / DUPLICATE / STALE / IGNORED_TERMINAL /
//!   UNMATCHED) so Telephony's outbox advances; only a failure to record it is retried.
//! * `POST /telephony/inbound-calls` — an inbound public call to a relay number
//!   (`telephony-inbound.offer`): Calling creates the canonical Call (PSTN_TO_APP) or
//!   refuses it (Telephony then rejects the dialog, revealing nothing to the caller).

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde_json::{json, Value};

use super::events::TelephonyEvent;
use crate::{error::CallingError, internal_auth::middleware::InternalCaller, state::AppState};

const MAX_EVENT_BYTES: usize = 16 * 1024;

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/telephony/events", post(apply_event))
        .route("/telephony/inbound-calls", post(inbound_call))
        .layer(DefaultBodyLimit::max(MAX_EVENT_BYTES))
}

async fn apply_event(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::CallsTelephonyEventApply)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The event is invalid."))?;
    let event: TelephonyEvent = serde_json::from_value(body)
        .map_err(|_| CallingError::Validation("The event is invalid."))?;
    let disposition = state.app.apply_telephony_event(&event).await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "eventId": event.event_id, "disposition": disposition.as_str() })),
    ))
}

async fn inbound_call(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::CallsTelephonyInboundOffer)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The offer is invalid."))?;
    let offer: crate::calls::app::public::InboundOffer = serde_json::from_value(body)
        .map_err(|_| CallingError::Validation("The offer is invalid."))?;
    let answer = state.app.accept_inbound_offer(&offer).await?;
    Ok((StatusCode::OK, Json(answer)))
}
