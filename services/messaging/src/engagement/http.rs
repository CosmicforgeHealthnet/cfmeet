//! Engagement routes (nested under `/v1/conversations`; acting principal required).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /{id}/messages/{messageId}/reactions` | `messages.read` |
//! | `PUT, DELETE /{id}/messages/{messageId}/reactions/{reaction}` | `messages.send` (MANAGED only) |
//! | `GET /{id}/pins` | `messages.read` |
//! | `PUT, DELETE /{id}/pins/{messageId}` | `messages.send` (acting OWNER / ADMIN) |
//! | `GET /{id}/unread` | `messages.read` |

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use uuid::Uuid;

use crate::{
    conversations::http::MESSAGES_READ,
    http::{error::ApiError, middleware::CustomerCaller},
    membership::http::MESSAGES_SEND,
    messages::http::acting_with,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/:conversation_id/messages/:message_id/reactions",
            get(reactions),
        )
        .route(
            "/:conversation_id/messages/:message_id/reactions/:reaction",
            put(add_reaction).delete(remove_reaction),
        )
        .route("/:conversation_id/pins", get(pins))
        .route("/:conversation_id/pins/:message_id", put(pin).delete(unpin))
        .route("/:conversation_id/unread", get(unread))
}

async fn reactions(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::reactions(&state, caller.context(), &acting, id, message).await?,
    ))
}

async fn add_reaction(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message, reaction)): Path<(Uuid, Uuid, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::react(
            &state,
            caller.context(),
            &acting,
            id,
            message,
            reaction,
            true,
        )
        .await?,
    ))
}

async fn remove_reaction(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message, reaction)): Path<(Uuid, Uuid, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::react(
            &state,
            caller.context(),
            &acting,
            id,
            message,
            reaction,
            false,
        )
        .await?,
    ))
}

async fn pins(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::pins(&state, caller.context(), &acting, id).await?,
    ))
}

async fn pin(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::pin(&state, caller.context(), &acting, id, message, true).await?,
    ))
}

async fn unpin(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::pin(&state, caller.context(), &acting, id, message, false).await?,
    ))
}

async fn unread(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::unread(&state, caller.context(), &acting, id).await?,
    ))
}
