//! Telephony's internal API (`/internal/v1`, Part E §165): Calling's commands and status
//! reads. Every route requires the receiver-authoritative permission Telephony's trust
//! bundle grants (only Calling holds them); bodies are bounded and strictly parsed.

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    commands::{originate::OriginateCommand, ControlCommand, Requester},
    domain::{CommandKind, RouteCapability},
    error::{ErrorCode, TelephonyError},
    middleware::InternalCaller,
    state::AppState,
};

const MAX_BODY_BYTES: usize = 16 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/legs/originate", post(originate))
        .route("/legs/:leg_id", get(get_leg))
        .route("/legs/:leg_id/terminate", post(terminate))
        .route("/legs/:leg_id/hold", post(hold))
        .route("/legs/:leg_id/resume", post(resume))
        .route("/legs/:leg_id/answer", post(answer))
        .route("/legs/:leg_id/prompt", post(prompt))
        .route("/legs/:leg_id/capture", post(capture))
        .route("/legs/:leg_id/capture-stop", post(capture_stop))
        .route("/bridges", post(create_bridge))
        .route("/bridges/:bridge_id", delete(end_bridge).get(get_bridge))
        .route("/routes/availability", get(availability))
        .route("/readiness", get(readiness))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

fn body<T>(raw: Result<Json<T>, JsonRejection>) -> Result<T, TelephonyError> {
    raw.map(|Json(b)| b)
        .map_err(|_| TelephonyError::Validation("The request body is invalid."))
}

fn requester(
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<Requester, TelephonyError> {
    caller.require_permission(permission)?;
    Ok(Requester {
        service: caller
            .context()
            .authenticated_service()
            .as_str()
            .to_string(),
        request_id: caller.request_id().as_str().to_string(),
        correlation_id: caller.correlation_id().as_str().to_string(),
        delegated: caller.context().delegated_platform_context().cloned(),
    })
}

fn respond(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(body),
    )
        .into_response()
}

fn leg_id(raw: &str) -> Result<Uuid, TelephonyError> {
    Uuid::parse_str(raw).map_err(|_| TelephonyError::Refused(ErrorCode::LegNotFound))
}

async fn originate(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<OriginateCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    let requester = requester(&caller, InternalPermission::TelephonyLegOriginate)?;
    let command = body(raw)?;
    let (status, body) = state.telephony.originate(&requester, command).await?;
    Ok(respond(status, body))
}

async fn control(
    state: &AppState,
    caller: &InternalCaller,
    permission: InternalPermission,
    kind: CommandKind,
    id: &str,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    let requester = requester(caller, permission)?;
    let id = leg_id(id)?;
    let command = body(raw)?;
    let (status, body) = state
        .telephony
        .control(&requester, kind, id, command)
        .await?;
    Ok(respond(status, body))
}

async fn terminate(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegTerminate,
        CommandKind::Terminate,
        &id,
        raw,
    )
    .await
}

async fn hold(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegHold,
        CommandKind::Hold,
        &id,
        raw,
    )
    .await
}

async fn resume(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegResume,
        CommandKind::Resume,
        &id,
        raw,
    )
    .await
}

/// Part F: answer an internal enterprise SIP leg (an IVR / voicemail needs media).
async fn answer(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegMedia,
        CommandKind::Answer,
        &id,
        raw,
    )
    .await
}

/// Part F: play one BUILT-IN prompt on an internal enterprise SIP leg.
async fn prompt(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegMedia,
        CommandKind::Prompt,
        &id,
        raw,
    )
    .await
}

/// Part F: capture the caller audio of an internal enterprise SIP leg (voicemail).
async fn capture(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegMedia,
        CommandKind::Capture,
        &id,
        raw,
    )
    .await
}

/// Part F: stop a running capture early (a call recording's stop). Never blocked.
async fn capture_stop(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
    raw: Result<Json<ControlCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    control(
        &state,
        &caller,
        InternalPermission::TelephonyLegMedia,
        CommandKind::CaptureStop,
        &id,
        raw,
    )
    .await
}

async fn get_leg(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, TelephonyError> {
    caller.require_permission(InternalPermission::TelephonyStatusRead)?;
    let id = leg_id(&id)?;
    Ok(respond(200, state.telephony.leg_view(id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AvailabilityQuery {
    country: String,
    capability: String,
}

async fn availability(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Result<Query<AvailabilityQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, TelephonyError> {
    caller.require_permission(InternalPermission::TelephonyStatusRead)?;
    let Query(q) =
        query.map_err(|_| TelephonyError::Validation("country and capability are required."))?;
    if q.country.len() != 2 || !q.country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(TelephonyError::Validation(
            "country must be an ISO 3166-1 alpha-2 code.",
        ));
    }
    let capability = RouteCapability::parse(&q.capability).ok_or(TelephonyError::Validation(
        "capability must be VOICE_OUTBOUND or VOICE_INBOUND.",
    ))?;
    Ok(respond(
        200,
        state.telephony.availability(&q.country, capability).await?,
    ))
}

async fn readiness(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Response, TelephonyError> {
    caller.require_permission(InternalPermission::TelephonyStatusRead)?;
    Ok(respond(200, state.telephony.readiness().await))
}

async fn create_bridge(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Json<crate::bridges::BridgeCommand>, JsonRejection>,
) -> Result<Response, TelephonyError> {
    let requester = requester(&caller, InternalPermission::TelephonyBridgeManage)?;
    let command = body(raw)?;
    let (status, body) = state.telephony.create_bridge(&requester, command).await?;
    Ok(respond(status, body))
}

async fn end_bridge(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, TelephonyError> {
    let requester = requester(&caller, InternalPermission::TelephonyBridgeManage)?;
    let id =
        Uuid::parse_str(&id).map_err(|_| TelephonyError::Refused(ErrorCode::BridgeNotFound))?;
    let (status, body) = state.telephony.end_bridge(&requester, id).await?;
    Ok(respond(status, body))
}

async fn get_bridge(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<String>,
) -> Result<Response, TelephonyError> {
    caller.require_permission(InternalPermission::TelephonyStatusRead)?;
    let id =
        Uuid::parse_str(&id).map_err(|_| TelephonyError::Refused(ErrorCode::BridgeNotFound))?;
    Ok(respond(200, state.telephony.bridge_view(id).await?))
}
