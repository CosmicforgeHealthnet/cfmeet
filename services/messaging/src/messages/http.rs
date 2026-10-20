//! Message routes (nested under `/v1/conversations`; acting principal required — message
//! content is read only by members, never through an administration view).
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /{id}/messages` | `messages.send` |
//! | `GET /{id}/messages` | `messages.read` |
//! | `GET /{id}/messages/{messageId}` (permalink resolution) | `messages.read` |
//! | `PATCH /{id}/messages/{messageId}` (MANAGED edit) | `messages.send` |
//! | `DELETE /{id}/messages/{messageId}?scope=EVERYONE\|ME` | `messages.send` |
//! | `PUT /{id}/read-cursor` | `messages.read` |
//! | `GET /{id}/events?after=` (durable event replay) | `messages.read` |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use uuid::Uuid;

use super::{DeleteQuery, EventsQuery, ListQuery, ReadCursorRequest, SendRequest};
use crate::{
    conversations::http::MESSAGES_READ,
    http::{
        error::ApiError,
        middleware::{require_acting, Acting, CustomerCaller},
    },
    membership::http::MESSAGES_SEND,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/:conversation_id/messages", get(list).post(send))
        .route(
            "/:conversation_id/messages/:message_id",
            get(resolve).patch(edit).delete(delete),
        )
        .route("/:conversation_id/read-cursor", put(read_cursor))
        .route("/:conversation_id/events", get(events))
}

pub async fn acting_with(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
    scope: &'static str,
) -> Result<Acting, ApiError> {
    caller.require_scope(scope)?;
    require_acting(state, caller, headers).await
}

async fn send(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SendRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    let (status, view) = super::send(&state, caller.context(), &acting, id, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::list(&state, caller.context(), &acting, id, q).await?,
    ))
}

async fn resolve(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::resolve(&state, caller.context(), &acting, id, message).await?,
    ))
}

async fn edit(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Json(body): Json<super::managed::EditRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::managed::edit(&state, caller.context(), &acting, id, message, body).await?,
    ))
}

async fn delete(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Query(q): Query<DeleteQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::delete(&state, caller.context(), &acting, id, message, q).await?,
    ))
}

async fn read_cursor(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReadCursorRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::set_read_cursor(&state, caller.context(), &acting, id, body).await?,
    ))
}

async fn events(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::replay(&state, caller.context(), &acting, id, q).await?,
    ))
}
