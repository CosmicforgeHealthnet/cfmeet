//! The separately negotiated v2 relay. Every operation resolves current principal
//! authority; the relay also checks both persistent devices on every data operation.
use super::{
    devices::{MESSAGES_READ, MESSAGES_SEND},
    error::ApiError,
    middleware::{require_acting, CustomerCaller},
};
use crate::{
    devices::{history_transfer::CreateTransfer, history_transfer_v2 as relay},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/history-transfer-capabilities", get(capabilities))
        .route("/history-transfers-v2", post(create).get(list))
        .route("/history-transfers-v2/:id", get(status))
        .route(
            "/history-transfers-v2/:id/manifest",
            put(manifest_put).get(manifest_get),
        )
        .route(
            "/history-transfers-v2/:id/chunks/:index",
            put(chunk_put).get(chunk_get),
        )
        .route("/history-transfers-v2/:id/checkpoint", post(checkpoint))
        .route("/history-transfers-v2/:id/complete", post(complete))
        .route("/history-transfers-v2/:id/cancel", post(cancel))
}

async fn capabilities(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    caller.require_scope(MESSAGES_READ)?;
    require_acting(&state, &caller, &headers).await?;
    Ok(Json(relay::capabilities(&state).await?))
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateTransfer>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(relay::create(&state, ctx, &acting, body).await?),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    device_id: Uuid,
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::list(&state, ctx, &acting, query.device_id).await?,
    ))
}

async fn status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(relay::get(&state, ctx, &acting, id).await?))
}

async fn manifest_put(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<relay::ManifestUpload>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::put_manifest(&state, ctx, &acting, id, body).await?,
    ))
}

async fn manifest_get(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::get_object(&state, ctx, &acting, id, None).await?,
    ))
}

async fn chunk_put(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, index)): Path<(Uuid, u32)>,
    Json(body): Json<relay::ChunkUpload>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::put_chunk(&state, ctx, &acting, id, index, body).await?,
    ))
}

async fn chunk_get(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path((id, index)): Path<(Uuid, u32)>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::get_object(&state, ctx, &acting, id, Some(index)).await?,
    ))
}

async fn checkpoint(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<relay::Checkpoint>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(
        relay::checkpoint(&state, ctx, &acting, id, body).await?,
    ))
}

async fn complete(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(relay::end(&state, ctx, &acting, id, true).await?))
}

async fn cancel(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    Ok(Json(relay::end(&state, ctx, &acting, id, false).await?))
}
