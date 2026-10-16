//! Calling's internal Telephony routes (service authentication, `/internal/v1`):
//!
//! * `POST /telephony/events` — normalized Telephony events (`telephony-event.apply`),
//!   deduplicated and applied by Calling's own state machines. Every delivered event is
//!   answered 200 with its disposition (APPLIED / DUPLICATE / STALE / IGNORED_TERMINAL /
//!   UNMATCHED) so Telephony's outbox advances; only a failure to record it is retried.
//! * `POST /telephony/inbound-calls` — an inbound public call to a relay number
//!   (`telephony-inbound.offer`): Calling creates the canonical Call (PSTN_TO_APP) or
//!   refuses it (Telephony then rejects the dialog, revealing nothing to the caller).
//! * `POST /telephony/captures` — Part F: an edge media capture's audio (base64 WAV with
//!   size / duration / SHA-256) or its failure (`telephony-capture.deliver`). 200 = stored
//!   or acknowledged (Telephony may drop its transient spool); 503 = artifact storage
//!   unavailable (Telephony retries); 404 = no such capture; 400 / 422 = refused for good.

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

/// A capture delivery carries the audio base64-encoded (4/3 of the bound) plus metadata.
const MAX_CAPTURE_REPORT_BYTES: usize =
    crate::calls::app::enterprise::MAX_CAPTURE_BYTES / 3 * 4 + 16 * 1024;

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/telephony/events", post(apply_event))
        .route("/telephony/inbound-calls", post(inbound_call))
        .route("/telephony/enterprise-calls", post(enterprise_call))
        .route("/telephony/enterprise-keypad", post(enterprise_keypad))
        .layer(DefaultBodyLimit::max(MAX_EVENT_BYTES))
        .merge(
            Router::new()
                .route("/telephony/captures", post(capture_report))
                .layer(DefaultBodyLimit::max(MAX_CAPTURE_REPORT_BYTES)),
        )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaptureMediaBody {
    content_type: String,
    duration_ms: i32,
    size_bytes: u64,
    sha256: String,
    base64: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaptureReportBody {
    schema_version: String,
    capture_id: uuid::Uuid,
    telephony_leg_id: uuid::Uuid,
    call_id: uuid::Uuid,
    call_leg_id: uuid::Uuid,
    outcome: String,
    #[serde(default)]
    failure_code: Option<String>,
    #[serde(default)]
    media: Option<CaptureMediaBody>,
}

/// Part F: an edge media capture (`telephony-capture.deliver`). Never logged in full.
async fn capture_report(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<CaptureReportBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    use base64::Engine;
    caller.require_permission(InternalPermission::CallsTelephonyCaptureDeliver)?;
    let Json(body) =
        body.map_err(|_| CallingError::Validation("The capture report is invalid."))?;
    if body.schema_version != "1" {
        return Err(CallingError::Validation("The capture report is invalid."));
    }
    let _ = body.telephony_leg_id;
    if !state
        .app
        .is_sip_capture(body.capture_id, body.call_id, body.call_leg_id)
        .await?
    {
        return Err(CallingError::NotFound);
    }
    match (body.outcome.as_str(), body.media) {
        ("COMPLETED", Some(media)) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(media.base64.as_bytes())
                .map_err(|_| CallingError::Validation("The capture media is invalid."))?;
            if bytes.len() as u64 != media.size_bytes {
                return Err(CallingError::Validation("The capture size does not match."));
            }
            let delivery = state
                .app
                .deliver_capture(
                    body.capture_id,
                    body.call_id,
                    crate::calls::app::enterprise::CapturedMedia {
                        content_type: media.content_type,
                        duration_ms: media.duration_ms,
                        sha256: media.sha256,
                        bytes,
                    },
                )
                .await?;
            Ok((
                StatusCode::OK,
                Json(json!({ "captureId": body.capture_id, "status": delivery.as_str() })),
            ))
        }
        ("FAILED", None) => {
            state
                .app
                .capture_failed(
                    body.capture_id,
                    body.failure_code.as_deref().unwrap_or("CAPTURE_FAILED"),
                )
                .await?;
            Ok((
                StatusCode::OK,
                Json(json!({ "captureId": body.capture_id, "status": "ACKNOWLEDGED" })),
            ))
        }
        _ => Err(CallingError::Validation("The capture report is invalid.")),
    }
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

/// Part F: an internal enterprise SIP dialog (`telephony-enterprise.offer`).
async fn enterprise_call(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::CallsTelephonyEnterpriseOffer)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The offer is invalid."))?;
    let offer: crate::calls::app::enterprise::EnterpriseDialogOffer = serde_json::from_value(body)
        .map_err(|_| CallingError::Validation("The offer is invalid."))?;
    let answer = state.app.accept_enterprise_offer(&offer).await?;
    Ok((StatusCode::OK, Json(answer)))
}

/// Part F: one key of an enterprise SIP leg (`telephony-enterprise.offer`). Never logged.
async fn enterprise_keypad(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), CallingError> {
    caller.require_permission(InternalPermission::CallsTelephonyEnterpriseOffer)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The input is invalid."))?;
    let keypad: crate::calls::app::enterprise::EnterpriseKeypad = serde_json::from_value(body)
        .map_err(|_| CallingError::Validation("The input is invalid."))?;
    let accepted = state.app.accept_enterprise_keypad(&keypad).await?;
    Ok((StatusCode::OK, Json(json!({ "accepted": accepted }))))
}
