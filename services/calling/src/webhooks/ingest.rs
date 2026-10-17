//! Internal platform-event ingress (`/internal/v1/platform/events`, Part G §67): committed
//! Privacy Relay and SMS Gateway (Part IV N / O) domain events, service-authenticated
//! (`platform-event.publish`, grantable only to those producers), projected into the
//! public catalog for customer webhooks.

use axum::{
    extract::rejection::JsonRejection, extract::State, http::StatusCode, routing::post, Json,
    Router,
};
use cf_service_auth::InternalPermission;
use serde_json::{json, Value};

use super::worker::{
    EventTransport, LocalWebhookTransport, PRODUCER_PRIVACY_RELAY, PRODUCER_SMS_GATEWAY,
};
use crate::{error::CallingError, internal_auth::middleware::InternalCaller, state::AppState};

/// Internal: committed platform events of other producers (service-authenticated).
pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/platform/events", post(ingest))
}

/// A committed producer event (platform envelope). Only Privacy Relay and the SMS Gateway
/// may publish, each only its OWN events (the envelope's producer is the authenticated
/// caller); the envelope is projected (never stored verbatim).
async fn ingest(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::PlatformEventPublish)?;
    let Json(envelope) = body.map_err(|_| CallingError::Validation("The event is invalid."))?;
    let caller_service = caller
        .context()
        .authenticated_service()
        .as_str()
        .to_string();
    let producer = [PRODUCER_PRIVACY_RELAY, PRODUCER_SMS_GATEWAY]
        .into_iter()
        .find(|p| *p == caller_service);
    let valid = producer.is_some()
        && envelope.get("producer_service").and_then(Value::as_str) == producer
        && envelope
            .get("event_id")
            .and_then(Value::as_str)
            .is_some_and(|e| e.len() <= 64 && e.starts_with("evt_"))
        && super::worker::ownership(&envelope).is_some();
    if !valid || envelope.to_string().len() > 16 * 1024 {
        return Err(CallingError::Validation("The event is invalid."));
    }
    LocalWebhookTransport::new((*state.webhooks).clone())
        .publish(producer.unwrap_or_default(), &envelope)
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    Ok((StatusCode::OK, Json(json!({ "status": "ACCEPTED" }))))
}
