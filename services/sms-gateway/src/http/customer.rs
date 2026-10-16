//! The customer API (`/v1/sms/**`, Platform Access authentication). Workstream N: message
//! and thread reads (`sms.read`). Sending (`sms.send`) is Workstream O.
//!
//! Every read is scoped to the credential's TRUSTED environment: another environment's
//! message or thread is NOT_FOUND. Content is returned to its owner only; the remote
//! party is masked; no provider identifier appears.

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{error::ApiError, middleware::CustomerCaller};
use crate::{
    sms::{
        reads::{self, Cursor, MessageFilter},
        store, threads, Direction, SmsError,
    },
    state::AppState,
};

pub const SMS_READ: &str = "sms.read";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/messages", get(list_messages))
        .route("/messages/:message_id", get(get_message))
        .route("/threads", get(list_threads))
        .route("/threads/:thread_id/messages", get(list_thread_messages))
}

fn uuid(raw: &str) -> Result<Uuid, ApiError> {
    // A malformed id is simply not a message of this environment.
    Uuid::parse_str(raw).map_err(|_| ApiError::Sms(SmsError::NotFound))
}

async fn get_message(
    State(state): State<AppState>,
    caller: CustomerCaller,
    Path(message_id): Path<String>,
) -> Result<Json<store::MessageView>, ApiError> {
    let ctx = caller.require_scope(SMS_READ)?;
    let id = uuid(&message_id)?;
    let row = store::load(&state.db, ctx.environment_id, id)
        .await?
        .ok_or(SmsError::NotFound)?;
    Ok(Json(store::view(&row, &state.protection)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageQuery {
    number_resource_id: Option<Uuid>,
    direction: Option<String>,
    cursor: Option<String>,
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageQuery {
    number_resource_id: Option<Uuid>,
    cursor: Option<String>,
    limit: Option<u64>,
}

fn direction(raw: Option<&str>) -> Result<Option<Direction>, ApiError> {
    match raw {
        None => Ok(None),
        Some("OUTBOUND") => Ok(Some(Direction::Outbound)),
        Some("INBOUND") => Ok(Some(Direction::Inbound)),
        Some(_) => Err(SmsError::Validation("direction must be OUTBOUND or INBOUND.").into()),
    }
}

fn message_page(
    state: &AppState,
    rows: &[crate::sms::TransportRow],
    next: Option<Cursor>,
) -> Value {
    json!({
        "messages": rows.iter().map(|r| store::view(r, &state.protection)).collect::<Vec<_>>(),
        "nextCursor": next.map(|c| c.encode()),
    })
}

async fn list_messages(
    State(state): State<AppState>,
    caller: CustomerCaller,
    query: Result<Query<MessageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(SMS_READ)?;
    let Query(q) = query.map_err(|_| SmsError::Validation("The query is invalid."))?;
    let filter = MessageFilter {
        number_resource_id: q.number_resource_id,
        direction: direction(q.direction.as_deref())?,
        thread_id: None,
    };
    let after = q.cursor.as_deref().map(Cursor::decode).transpose()?;
    let (rows, next) = reads::messages(
        &state.db,
        ctx.environment_id,
        filter,
        after,
        reads::limit(q.limit)?,
    )
    .await?;
    Ok(Json(message_page(&state, &rows, next)))
}

async fn list_threads(
    State(state): State<AppState>,
    caller: CustomerCaller,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(SMS_READ)?;
    let Query(q) = query.map_err(|_| SmsError::Validation("The query is invalid."))?;
    let after = q.cursor.as_deref().map(Cursor::decode).transpose()?;
    let (rows, next) = reads::threads(
        &state.db,
        ctx.environment_id,
        q.number_resource_id,
        after,
        reads::limit(q.limit)?,
    )
    .await?;
    Ok(Json(json!({
        "threads": rows.iter().map(threads::ThreadView::from).collect::<Vec<_>>(),
        "nextCursor": next.map(|c| c.encode()),
    })))
}

async fn list_thread_messages(
    State(state): State<AppState>,
    caller: CustomerCaller,
    Path(thread_id): Path<String>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(SMS_READ)?;
    let Query(q) = query.map_err(|_| SmsError::Validation("The query is invalid."))?;
    if q.number_resource_id.is_some() {
        return Err(SmsError::Validation("The query is invalid.").into());
    }
    let thread = threads::load(&state.db, ctx.environment_id, uuid(&thread_id)?)
        .await?
        .ok_or(SmsError::NotFound)?;
    let after = q.cursor.as_deref().map(Cursor::decode).transpose()?;
    let (rows, next) = reads::messages(
        &state.db,
        ctx.environment_id,
        MessageFilter {
            thread_id: Some(thread.id),
            ..Default::default()
        },
        after,
        reads::limit(q.limit)?,
    )
    .await?;
    Ok(Json(message_page(&state, &rows, next)))
}
