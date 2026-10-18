//! Compliance routes under `/v1/messaging` (application administration, no acting
//! principal; every operation audited).
//!
//! | Route | Scope |
//! |---|---|
//! | `GET` / `POST /legal-holds`, `GET /legal-holds/{holdId}`, `POST /legal-holds/{holdId}/release` | `conversations.manage` |

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use super::holds::{self, CreateHold, ReleaseHold};
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
