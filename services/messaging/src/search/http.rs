//! Search routes.
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /v1/conversations/{id}/search?q=` (MANAGED only; E2EE: `LOCAL_SEARCH_ONLY`) | `messages.read` |
//! | `GET /v1/messaging/search?q=` (the principal's MANAGED conversations) | `messages.read` |

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use super::SearchQuery;
use crate::{
    http::{error::ApiError, middleware::CustomerCaller},
    messages::http::acting_with,
    state::AppState,
};

const MESSAGES_READ: &str = "messages.read";

pub fn conversation_routes() -> Router<AppState> {
    Router::new().route("/:conversation_id/search", get(conversation))
}

pub fn customer_routes() -> Router<AppState> {
    Router::new().route("/search", get(across))
}

async fn conversation(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::conversation(&state, caller.context(), &acting, id, q).await?,
    ))
}

async fn across(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::across(&state, caller.context(), &acting, q).await?,
    ))
}
