//! Internal platform-event ingress (`/internal/v1/platform/events`, Part G §67): committed
//! Privacy Relay domain events, service-authenticated (`platform-event.publish`, grantable
//! only to Privacy Relay), projected into the public catalog for customer webhooks.

use axum::{
    extract::rejection::JsonRejection, extract::State, http::StatusCode, routing::post, Json,
    Router,
};
use cf_service_auth::InternalPermission;
use serde_json::{json, Value};

use super::worker::{EventTransport, LocalWebhookTransport, PRODUCER_PRIVACY_RELAY};
use crate::{error::CallingError, internal_auth::middleware::InternalCaller, state::AppState};

/// Internal: committed Privacy Relay events (service-authenticated).
pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/platform/events", post(ingest))
}

/// A committed Privacy Relay event (platform envelope). Only `cfmeet.privacy-relay` may
/// publish, only its own events; the envelope is projected (never stored verbatim).
async fn ingest(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::PlatformEventPublish)?;
    let Json(envelope) = body.map_err(|_| CallingError::Validation("The event is invalid."))?;
    let valid = envelope.get("producer_service").and_then(Value::as_str)
        == Some(PRODUCER_PRIVACY_RELAY)
        && caller.context().authenticated_service().as_str() == PRODUCER_PRIVACY_RELAY
        && envelope
            .get("event_id")
            .and_then(Value::as_str)
            .is_some_and(|e| e.len() <= 64 && e.starts_with("evt_"))
        && super::worker::ownership(&envelope).is_some();
    if !valid || envelope.to_string().len() > 16 * 1024 {
        return Err(CallingError::Validation("The event is invalid."));
    }
    LocalWebhookTransport::new((*state.webhooks).clone())
        .publish(PRODUCER_PRIVACY_RELAY, &envelope)
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    Ok((StatusCode::OK, Json(json!({ "status": "ACCEPTED" }))))
}
