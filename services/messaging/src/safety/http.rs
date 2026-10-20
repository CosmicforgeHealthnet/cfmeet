//! Safety routes under `/v1/messaging`.
//!
//! | Route | Scope |
//! |---|---|
//! | `GET /blocks`, `POST /blocks`, `DELETE /blocks/{principalId}` | `messages.send` + acting principal (`GET`: `messages.read`) |

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
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
        .route("/blocks", get(list_blocks).post(create_block))
        .route("/blocks/:principal_id", delete(remove_block))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BlockRequest {
    principal_id: Uuid,
}

async fn list_blocks(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_READ).await?;
    Ok(Json(
        super::blocks::list(&state, caller.context(), &acting).await?,
    ))
}

async fn create_block(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<BlockRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    let (status, view) =
        super::blocks::create(&state, caller.context(), &acting, body.principal_id).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(view),
    ))
}

async fn remove_block(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(principal): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, MESSAGES_SEND).await?;
    Ok(Json(
        super::blocks::remove(&state, caller.context(), &acting, principal).await?,
    ))
}
