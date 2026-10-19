//! SMS Bridge routes (Part V H §§106–112).
//!
//! | Route | Scope / permission |
//! |---|---|
//! | `GET /v1/messaging/sms-bridge/disclosure` | `messages.read` |
//! | `POST /v1/conversations/{id}/sms-bridges` | `conversations.manage` (acting ADMIN / OWNER) |
//! | `GET /v1/conversations/{id}/sms-bridges` | `messages.read` (member) |
//! | `POST /v1/conversations/{id}/sms-bridges/{bridgeId}/close` | `conversations.manage` (acting ADMIN / OWNER) |
//! | `GET /v1/conversations/{id}/messages/{messageId}/sms-transport` | `messages.read` (member) |
//! | `POST /internal/v1/sms-bridge/inbound` | `messaging-sms-bridge.deliver` (SMS Gateway) |

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use uuid::Uuid;

use super::{CreateBridge, InboundSms};
use crate::{
    conversations::http::{CONVERSATIONS_MANAGE, MESSAGES_READ},
    http::{
        error::ApiError,
        middleware::{delegated_context, CustomerCaller, InternalCaller},
    },
    messages::http::acting_with,
    state::AppState,
};

/// Nested under `/v1/conversations`.
pub fn conversation_routes() -> Router<AppState> {
    Router::new()
        .route("/:conversation_id/sms-bridges", get(list).post(create))
        .route(
            "/:conversation_id/sms-bridges/:bridge_id/close",
            post(close),
        )
        .route(
            "/:conversation_id/messages/:message_id/sms-transport",
            get(transport),
        )
}

/// Nested under `/v1/messaging`.
pub fn messaging_routes() -> Router<AppState> {
    Router::new().route("/sms-bridge/disclosure", get(disclosure))
}

/// Nested under `/internal/v1`.
pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/sms-bridge/inbound", post(inbound))
}

async fn disclosure(caller: CustomerCaller) -> Result<impl IntoResponse, ApiError> {
    caller.require_scope(MESSAGES_READ)?;
    Ok(Json(super::disclosure()))
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateBridge>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, CONVERSATIONS_MANAGE).await?;
    Ok((
        StatusCode::CREATED,
        Json(super::create(&state, caller.context(), &acting, id, body).await?),
    ))
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::list(&state, caller.context(), &acting, id).await?,
    ))
}

async fn close(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, bridge)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, CONVERSATIONS_MANAGE).await?;
    Ok(Json(
        super::close(&state, caller.context(), &acting, id, bridge).await?,
    ))
}

async fn transport(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::message_transport(&state, caller.context(), &acting, id, message).await?,
    ))
}

async fn inbound(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<InboundSms>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_permission(InternalPermission::MessagingSmsBridgeDeliver)?;
    let ctx = delegated_context(&state, &caller).await?;
    Ok(Json(super::accept_inbound(&state, &ctx, body).await?))
}
