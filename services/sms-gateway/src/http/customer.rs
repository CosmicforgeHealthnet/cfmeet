//! The customer API (`/v1/sms/**`, Platform Access authentication): message and thread
//! reads (`sms.read`, Workstream N); sending and opt-outs (`sms.send`, Workstream O).
//!
//! Every read is scoped to the credential's TRUSTED environment: another environment's
//! message or thread is NOT_FOUND. Content is returned to its owner only; the remote
//! party is masked; no provider identifier appears.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::CustomerCaller,
};
use crate::{
    domain::{current_context, RequestContext},
    sms::{
        controls, e164,
        events::EventScope,
        protection::mask,
        reads::{self, Cursor, MessageFilter},
        send::{self, SendOutcome, SendRequest},
        store, threads, Direction, SmsError,
    },
    state::AppState,
};

pub const SMS_READ: &str = "sms.read";
pub const SMS_SEND: &str = "sms.send";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/messages", get(list_messages).post(send_message))
        .route("/opt-outs", post(create_opt_out))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FromBody {
    number_resource_id: Uuid,
}

/// `SendMessageRequest`: the sender is a Number Resource only — a sender id or a raw
/// number cannot even be expressed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendBody {
    from: FromBody,
    to: Option<String>,
    thread_id: Option<Uuid>,
    text: String,
    principal_id: Option<Uuid>,
    validity_seconds: Option<i64>,
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let mut values = headers.get_all("idempotency-key").iter();
    match (values.next(), values.next()) {
        (Some(v), None) => v
            .to_str()
            .map(|s| s.trim().to_string())
            .map_err(|_| SmsError::Validation("Idempotency-Key is invalid.").into()),
        _ => Err(ApiError::Code(ErrorCode::IdempotencyKeyRequired)),
    }
}

async fn send_message(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    body: Result<Json<SendBody>, JsonRejection>,
) -> Result<(StatusCode, Json<store::MessageView>), ApiError> {
    let ctx = caller.require_scope(SMS_SEND)?;
    let key = idempotency_key(&headers)?;
    let Json(body) = body.map_err(|_| SmsError::Validation("The request body is invalid."))?;
    let rc = current_context().unwrap_or_else(RequestContext::generate);
    let request = SendRequest {
        from_number: body.from.number_resource_id,
        to: body.to,
        thread_id: body.thread_id,
        text: body.text,
        principal_id: body.principal_id,
        validity_seconds: body.validity_seconds,
    };
    match send::send(&state, ctx, &key, request, &rc).await? {
        SendOutcome::Accepted(row) => Ok((
            StatusCode::ACCEPTED,
            Json(store::view(&row, &state.protection)),
        )),
        SendOutcome::Replayed(row) => {
            Ok((StatusCode::OK, Json(store::view(&row, &state.protection))))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OptOutBody {
    to: String,
    number_resource_id: Option<Uuid>,
}

/// Blocks a recipient for one number (or every number) of the environment. Idempotent.
async fn create_opt_out(
    State(state): State<AppState>,
    caller: CustomerCaller,
    body: Result<Json<OptOutBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let ctx = caller.require_scope(SMS_SEND)?;
    let Json(body) = body.map_err(|_| SmsError::Validation("The request body is invalid."))?;
    let to =
        e164::normalize(&body.to).ok_or(SmsError::Validation("to must be an E.164 number."))?;
    let now = state.now();
    let scope = EventScope {
        tenant_id: ctx.tenant_id,
        application_id: ctx.application_id,
        environment_id: ctx.environment_id,
    };
    controls::block(
        &state.db,
        scope,
        body.number_resource_id,
        &state.protection.number_fingerprint(&to),
        "CUSTOMER_REQUEST",
        None,
        now,
    )
    .await?;
    Ok(Json(json!({
        "remote": { "masked": mask(&to) },
        "numberResourceId": body.number_resource_id,
        "reason": "CUSTOMER_REQUEST",
        "createdAt": threads::ts(now),
    })))
}
