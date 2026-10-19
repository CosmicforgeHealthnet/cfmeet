//! Part V H §§106–112 — the SMS Bridge internal API (service-authenticated).
//!
//! | Route | Permission |
//! |---|---|
//! | `POST /internal/v1/sms-bridge/bindings` | `sms-bridge.submit` (Messaging) |
//! | `POST /internal/v1/sms-bridge/bindings/{bridgeRef}/close` | `sms-bridge.submit` |
//! | `POST /internal/v1/sms-bridge/messages` | `sms-bridge.submit` |
//! | `GET /internal/v1/sms-bridge/messages/{smsMessageId}` | `sms-bridge.submit` |
//!
//! Every call needs the signed delegated platform context of the application
//! environment; the number's ownership in it is re-checked by Numbering on each send.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    domain::{current_context, RequestContext},
    identity::TrustedContext,
    sms::{
        bridge::{self, CreateBinding, Submission},
        send::SendOutcome,
        store,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sms-bridge/bindings", post(create_binding))
        .route(
            "/sms-bridge/bindings/:bridge_ref/close",
            post(close_binding),
        )
        .route("/sms-bridge/messages", post(submit))
        .route("/sms-bridge/messages/:sms_message_id", get(status))
}

fn scope(caller: &InternalCaller) -> Result<TrustedContext, ApiError> {
    caller.require_permission(InternalPermission::SmsBridgeSubmit)?;
    let delegated = caller
        .context()
        .delegated_platform_context()
        .ok_or(ApiError::Code(ErrorCode::DelegatedContextRequired))?;
    Ok(bridge::bridge_context(delegated))
}

async fn create_binding(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<CreateBinding>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = scope(&caller)?;
    let (created, row) = bridge::create_binding(&state, &ctx, body).await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(bridge::binding_view(&row)),
    ))
}

async fn close_binding(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(bridge_ref): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = scope(&caller)?;
    Ok(Json(bridge::binding_view(
        &bridge::close_binding(&state, &ctx, bridge_ref).await?,
    )))
}

async fn submit(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<Submission>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = scope(&caller)?;
    let rc = current_context().unwrap_or_else(RequestContext::generate);
    Ok(match bridge::submit(&state, &ctx, body, &rc).await? {
        SendOutcome::Accepted(row) => (
            StatusCode::ACCEPTED,
            Json(bridge::transport_view(&store::view(
                &row,
                &state.protection,
            ))),
        ),
        SendOutcome::Replayed(row) => (
            StatusCode::OK,
            Json(bridge::transport_view(&store::view(
                &row,
                &state.protection,
            ))),
        ),
    })
}

async fn status(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = scope(&caller)?;
    Ok(Json(bridge::transport_status(&state, &ctx, id).await?))
}
