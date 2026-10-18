//! Conversation routes (contract `cfmeet.messaging.http-api`).
//!
//! | Route | Access |
//! |---|---|
//! | `POST /v1/conversations` | `conversations.create` + acting principal (becomes the owner); `Idempotency-Key` required |
//! | `GET /v1/conversations`, `GET /v1/conversations/{id}` | with `CF-Acting-Principal`: `messages.read`, the principal's conversations only; without: `conversations.manage`, the environment's conversations |
//! | `PATCH /v1/conversations/{id}` | `conversations.manage`; with an acting principal it must be the owner |
//! | `GET /v1/messaging/policy`, `PUT /v1/messaging/policy` | `conversations.manage` |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use super::service::{self, CreateRequest, ListQuery, PatchRequest, PolicyUpdate, Reader};
use crate::{
    http::{
        error::{ApiError, ErrorCode},
        middleware::{acting_header, require_acting, verify_principal, CustomerCaller},
    },
    state::AppState,
};

pub const CONVERSATIONS_CREATE: &str = "conversations.create";
pub const CONVERSATIONS_MANAGE: &str = "conversations.manage";
pub const MESSAGES_READ: &str = "messages.read";

/// Routes nested under `/v1/conversations`.
pub fn conversation_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/:conversation_id", get(get_one).patch(patch))
}

/// Routes nested under `/v1/messaging`.
pub fn policy_routes() -> Router<AppState> {
    Router::new().route("/policy", get(get_policy).put(put_policy))
}

async fn reader(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
) -> Result<Reader, ApiError> {
    match acting_header(headers)? {
        Some(p) => {
            let ctx = caller.require_scope(MESSAGES_READ)?;
            Ok(Reader::Member(verify_principal(state, ctx, p).await?))
        }
        None => {
            caller.require_scope(CONVERSATIONS_MANAGE)?;
            Ok(Reader::Administration)
        }
    }
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(CONVERSATIONS_CREATE)?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::code(ErrorCode::IdempotencyKeyRequired))?
        .to_string();
    let acting = require_acting(&state, &caller, &headers).await?;
    let created = service::create(&state, ctx, &acting, body, &key).await?;
    Ok((
        StatusCode::from_u16(created.status).unwrap_or(StatusCode::OK),
        Json(created.body),
    ))
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let reader = reader(&state, &caller, &headers).await?;
    Ok(Json(
        service::list(&state, caller.context(), &reader, q).await?,
    ))
}

async fn get_one(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let reader = reader(&state, &caller, &headers).await?;
    Ok(Json(
        service::get(&state, caller.context(), &reader, id).await?,
    ))
}

async fn patch(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(CONVERSATIONS_MANAGE)?;
    let acting = match acting_header(&headers)? {
        Some(p) => Some(verify_principal(&state, ctx, p).await?),
        None => None,
    };
    Ok(Json(
        service::patch(&state, ctx, acting.as_ref(), id, body).await?,
    ))
}

async fn get_policy(
    State(state): State<AppState>,
    caller: CustomerCaller,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(CONVERSATIONS_MANAGE)?;
    Ok(Json(service::policy_view(&state, ctx).await?))
}

async fn put_policy(
    State(state): State<AppState>,
    caller: CustomerCaller,
    Json(body): Json<PolicyUpdate>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(CONVERSATIONS_MANAGE)?;
    Ok(Json(service::update_policy(&state, ctx, body).await?))
}
