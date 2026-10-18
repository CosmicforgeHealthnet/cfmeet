//! MLS Delivery Service routes.
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /v1/conversations/{id}/mls/commits` | `messages.send` |
//! | `GET /v1/conversations/{id}/mls/state` | `messages.read` |
//! | `POST /v1/conversations/{id}/mls/key-package-claims` | `messages.send` |
//! | `GET /v1/messaging/devices/{deviceId}/welcomes` | `messages.read` |
//! | `POST /v1/messaging/devices/{deviceId}/welcomes/{welcomeId}/consume` | `messages.send` |

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use super::{ClaimRequest, CommitRequest};
use crate::{
    conversations::http::MESSAGES_READ,
    http::{error::ApiError, middleware::CustomerCaller},
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    state::AppState,
};

/// Nested under `/v1/conversations`.
pub fn conversation_routes() -> Router<AppState> {
    Router::new()
        .route("/:conversation_id/mls/commits", post(commit))
        .route("/:conversation_id/mls/state", get(group_state))
        .route("/:conversation_id/mls/key-package-claims", post(claim))
}

/// Nested under `/v1/messaging`.
pub fn device_routes() -> Router<AppState> {
    Router::new()
        .route("/devices/:device_id/welcomes", get(welcomes))
        .route(
            "/devices/:device_id/welcomes/:welcome_id/consume",
            post(consume),
        )
}

async fn commit(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CommitRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    let (status, view) = super::submit_commit(&state, caller.context(), &acting, id, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn group_state(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::group_state(&state, caller.context(), &acting, id).await?,
    ))
}

async fn claim(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ClaimRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::claim_key_packages(&state, caller.context(), &acting, id, body).await?,
    ))
}

async fn welcomes(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(device): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::pending_welcomes(&state, caller.context(), &acting, device).await?,
    ))
}

async fn consume(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((device, welcome)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::consume_welcome(&state, caller.context(), &acting, device, welcome).await?,
    ))
}
