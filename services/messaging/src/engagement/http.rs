//! Engagement routes (nested under `/v1/conversations`; acting principal required).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /{id}/messages/{messageId}/reactions` | `messages.read` |
//! | `PUT, DELETE /{id}/messages/{messageId}/reactions/{reaction}` | `messages.send` (MANAGED only) |
//! | `GET /{id}/messages/{messageId}/thread` | `messages.read` |
//! | `GET /{id}/pins` | `messages.read` |
//! | `PUT, DELETE /{id}/pins/{messageId}` | `messages.send` (acting OWNER / ADMIN; optional `expiresAt`) |
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
        .route("/:conversation_id/messages/:message_id/thread", get(thread))
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

async fn thread(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::thread(&state, caller.context(), &acting, id, message).await?,
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

/// Optional body of `PUT /{id}/pins/{messageId}`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinBody {
    /// §57: the pin stops being listed (and counting toward the limit) at this time.
    #[serde(default)]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn pin(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    let body: PinBody = if body.iter().all(u8::is_ascii_whitespace) {
        PinBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| crate::error::Error::validation(format!("invalid pin body: {e}")))?
    };
    Ok(Json(
        super::pin(
            &state,
            caller.context(),
            &acting,
            id,
            message,
            Some(body.expires_at),
        )
        .await?,
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
        super::pin(&state, caller.context(), &acting, id, message, None).await?,
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
