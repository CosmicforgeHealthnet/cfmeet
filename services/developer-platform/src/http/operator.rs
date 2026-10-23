//! Operator administration routes (`/internal/v1/operator/**`): permission
//! `developer-platform.administer` (operator console only) and a named human operator in
//! `x-cf-operator`. See [`crate::control::operator`].

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde_json::Value;
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    control::{
        commercial, operator,
        operator::{RegisterFirstParty, SetState},
        organizations,
    },
    domain::{current_context, rbac::OrganizationState, RequestContext},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/organizations", post(register_first_party))
        .route("/operator/organizations/:organization_id", get(overview))
        .route(
            "/operator/organizations/:organization_id/state",
            post(set_state),
        )
        .route(
            "/operator/organizations/:organization_id/plan-assignments",
            post(assign_plan),
        )
        .route(
            "/operator/organizations/:organization_id/cost-center",
            put(set_cost_center),
        )
        .route("/operator/commercial-plans", post(publish_plan))
        .route(
            "/operator/commercial-plans/:plan_id/retire",
            post(retire_plan),
        )
}

fn operator_of(
    caller: &InternalCaller,
    headers: &HeaderMap,
) -> Result<(String, RequestContext), ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    let name = headers
        .get("x-cf-operator")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| operator::valid_operator(v))
        .ok_or(ApiError(ErrorCode::OperatorRequired))?;
    let ctx = current_context().ok_or(ApiError(ErrorCode::InternalError))?;
    Ok((name.to_string(), ctx))
}

async fn register_first_party(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<RegisterFirstParty>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(operator::register_first_party(&s, &op, &ctx, body).await?),
    ))
}

async fn overview(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(operator::organization_overview(&s.db, id).await?))
}

async fn set_state(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SetState>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    let to = OrganizationState::parse(&body.state).ok_or(ApiError(ErrorCode::ValidationError))?;
    if !operator::valid_reason(&body.reason) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(Json(
        organizations::operator_set_state(&s, &op, &ctx, id, to, &body.reason).await?,
    ))
}

async fn assign_plan(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<commercial::AssignPlan>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(commercial::operator_assign_plan(&s, &op, &ctx, id, body).await?),
    ))
}

async fn set_cost_center(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<commercial::CostCenter>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok(Json(
        commercial::operator_set_cost_center(&s, &op, &ctx, id, body).await?,
    ))
}

async fn publish_plan(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<commercial::PublishPlan>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (op, _) = operator_of(&caller, &headers)?;
    tracing::info!(operator = %op, "commercial plan version published");
    Ok((
        StatusCode::CREATED,
        Json(commercial::operator_publish_plan(&s.db, body).await?),
    ))
}

async fn retire_plan(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (op, _) = operator_of(&caller, &headers)?;
    tracing::info!(operator = %op, "commercial plan version retired");
    Ok(Json(commercial::operator_retire_plan(&s.db, id).await?))
}
