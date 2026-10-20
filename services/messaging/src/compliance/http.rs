//! Compliance routes under `/v1/messaging` (application administration, no acting
//! principal; every operation audited).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET` / `POST /legal-holds`, `GET /legal-holds/{holdId}`, `POST /legal-holds/{holdId}/release` | `conversations.manage` |
//! | `POST /exports`, `GET /exports/{exportId}`, `POST /exports/{exportId}/download-grants` (MANAGED only) | `messaging.export` |
//! | `POST /deletion-requests`, `GET /deletion-requests/{requestId}` | `conversations.manage` |
//! | `GET /audit` | `conversations.manage` |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use super::{
    audit_log, deletion, exports,
    holds::{self, CreateHold, ReleaseHold},
};
use crate::{
    conversations::http::CONVERSATIONS_MANAGE,
    error::{Error, ErrorCode},
    http::{
        error::ApiError,
        middleware::{acting_header, CustomerCaller},
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/legal-holds", get(list).post(create))
        .route("/legal-holds/:hold_id", get(one))
        .route("/legal-holds/:hold_id/release", post(release))
        .route("/exports", post(export))
        .route("/exports/:export_id", get(export_status))
        .route("/exports/:export_id/download-grants", post(export_grant))
        .route("/deletion-requests", post(deletion_request))
        .route("/deletion-requests/:request_id", get(deletion_status))
        .route("/audit", get(audit))
}

pub const MESSAGING_EXPORT: &str = "messaging.export";

async fn export(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<exports::ExportRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MESSAGING_EXPORT)?;
    Ok((
        StatusCode::CREATED,
        Json(exports::request(&state, ctx, body).await?),
    ))
}

async fn export_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MESSAGING_EXPORT)?;
    Ok(Json(exports::get(&state, ctx, id).await?))
}

async fn export_grant(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MESSAGING_EXPORT)?;
    Ok(Json(exports::grant(&state, ctx, id).await?))
}

async fn deletion_request(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<deletion::DeletionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(deletion::create(&state, ctx, body).await?),
    ))
}

async fn deletion_status(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok(Json(deletion::get(&state, ctx, id).await?))
}

async fn audit(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<audit_log::AuditQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok(Json(audit_log::list(&state, ctx, q).await?))
}

/// Compliance administration is an application-level authority, never a principal's.
pub fn administration<'a>(
    caller: &'a CustomerCaller,
    headers: &HeaderMap,
    scope: &'static str,
) -> Result<&'a crate::identity::TrustedContext, ApiError> {
    if acting_header(headers)?.is_some() {
        return Err(Error::reason(
            ErrorCode::PrincipalNotAuthorized,
            "ADMINISTRATION_ONLY",
        ));
    }
    caller.require_scope(scope)
}

async fn create(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<CreateHold>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok((
        StatusCode::CREATED,
        Json(holds::create(&state, ctx, body).await?),
    ))
}

async fn list(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok(Json(holds::list(&state, ctx).await?))
}

async fn one(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok(Json(holds::get(&state, ctx, id).await?))
}

async fn release(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<ReleaseHold>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, CONVERSATIONS_MANAGE)?;
    Ok(Json(holds::release(&state, ctx, id, body).await?))
}
