//! Report and moderation routes.
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /v1/conversations/{id}/reports` (acting member) | `messages.send` |
//! | `GET /v1/messaging/reports`, `GET /v1/messaging/reports/{reportId}` (administration) | `moderation.manage` |
//! | `GET` / `POST /v1/messaging/moderation/actions` (administration) | `moderation.manage` |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use super::{actions, reports};
use crate::{
    compliance::http::administration,
    http::{error::ApiError, middleware::CustomerCaller},
    messages::http::acting_with,
    state::AppState,
};

pub const MODERATION_MANAGE: &str = "moderation.manage";

pub fn conversation_routes() -> Router<AppState> {
    Router::new().route("/:conversation_id/reports", post(report))
}

pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route("/reports", get(list_reports))
        .route("/reports/:report_id", get(get_report))
        .route("/moderation/actions", get(list_actions).post(act))
}

async fn report(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<reports::ReportRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let acting = acting_with(&state, &caller, &headers, "messages.send").await?;
    Ok((
        StatusCode::CREATED,
        Json(reports::create(&state, caller.context(), &acting, id, body).await?),
    ))
}

async fn list_reports(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<reports::ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MODERATION_MANAGE)?;
    Ok(Json(reports::list(&state, ctx, q).await?))
}

async fn get_report(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MODERATION_MANAGE)?;
    Ok(Json(reports::get(&state, ctx, id).await?))
}

async fn act(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<actions::ActionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MODERATION_MANAGE)?;
    Ok((
        StatusCode::CREATED,
        Json(actions::apply(&state, ctx, body).await?),
    ))
}

async fn list_actions(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<actions::ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = administration(&caller, &headers, MODERATION_MANAGE)?;
    Ok(Json(actions::list(&state, ctx, q).await?))
}
