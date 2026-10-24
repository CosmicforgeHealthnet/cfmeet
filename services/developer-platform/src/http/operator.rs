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
        // Section 54: explicit overrides and first-party scope grants.
        .route(
            "/operator/organizations/:organization_id/entitlement-overrides",
            post(set_override),
        )
        .route(
            "/operator/organizations/:organization_id/entitlement-overrides/:override_id",
            axum::routing::delete(revoke_override),
        )
        .route(
            "/operator/projects/:project_id/scope-grants",
            post(operator_scope_grant),
        )
        // Section 55: production-access decisions.
        .route(
            "/operator/production-access-requests",
            get(list_production_requests),
        )
        .route(
            "/operator/production-access-requests/:request_id",
            get(get_production_request),
        )
        .route(
            "/operator/production-access-requests/:request_id/:command",
            post(decide_production_request),
        )
        // Section 55: usage ledger operations.
        .route("/operator/usage", get(usage_overview))
        .route("/operator/ingestion/health", get(ingestion_health))
        .route("/operator/usage/rollups/rebuild", post(rebuild_rollups))
        .route(
            "/operator/usage/backfill/api-operations",
            post(backfill_api_operations),
        )
        .route(
            "/operator/usage/reconcile/api-operations",
            post(reconcile_api_operations),
        )
        .route(
            "/operator/usage/streams/:producer/:stream/reset",
            post(reset_stream),
        )
        .route(
            "/operator/cost-centers/:cost_center/usage",
            get(cost_center_usage),
        )
        // Section 57: support / incident path.
        .route(
            "/operator/organizations/:organization_id/support-profile",
            put(set_support_profile),
        )
        .route(
            "/operator/organizations/:organization_id/incidents",
            post(record_incident),
        )
}

async fn set_override(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::control::entitlements::SetOverride>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(crate::control::entitlements::operator_set_override(&s, &op, &ctx, id, body).await?),
    ))
}

async fn revoke_override(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, override_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::entitlements::operator_revoke_override(&s, &op, &ctx, id, override_id)
            .await?,
    ))
}

async fn operator_scope_grant(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::control::scopes::OperatorGrant>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::scopes::operator_grant(&s, &op, &ctx, id, body).await?,
    ))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateQuery {
    state: Option<String>,
}

async fn list_production_requests(
    State(s): State<AppState>,
    caller: InternalCaller,
    axum::extract::Query(q): axum::extract::Query<StateQuery>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(
        crate::control::production::operator_list(&s.db, q.state.as_deref()).await?,
    ))
}

async fn get_production_request(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(
        crate::control::production::operator_get(&s.db, id).await?,
    ))
}

/// `POST …/{id}/{review|approve|reject|suspend|reinstate}`. Approve / reject need the
/// reviewing operator's CF Meet user id (`x-cf-operator-user-id`): a member of the
/// requesting organization can never decide its request.
async fn decide_production_request(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, command)): Path<(Uuid, String)>,
    body: Option<Json<crate::control::production::Decision>>,
) -> Result<Json<Value>, ApiError> {
    use crate::control::production::OperatorCommand as C;
    let (op, ctx) = operator_of(&caller, &headers)?;
    let command = match command.as_str() {
        "review" => C::Review,
        "approve" => C::Approve,
        "reject" => C::Reject,
        "suspend" => C::Suspend,
        "reinstate" => C::Reinstate,
        _ => return Err(ApiError(ErrorCode::NotFound)),
    };
    let user = headers
        .get("x-cf-operator-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| Uuid::parse_str(v.trim()).map_err(|_| ApiError(ErrorCode::OperatorRequired)))
        .transpose()?;
    Ok(Json(
        crate::control::production::operator_decide(
            &s,
            &op,
            user,
            &ctx,
            id,
            command,
            body.map(|Json(b)| b).unwrap_or_default(),
        )
        .await?,
    ))
}

async fn usage_overview(
    State(s): State<AppState>,
    caller: InternalCaller,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(crate::ledger::operator_overview(&s.db).await?))
}

/// `GET /internal/v1/operator/ingestion/health` — failed usage / event ingestion signals.
async fn ingestion_health(
    State(s): State<AppState>,
    caller: InternalCaller,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(crate::ledger::ingestion_health(&s.db).await?))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RebuildBody {
    from: String,
    to: String,
    #[serde(default)]
    environment_id: Option<Uuid>,
}

async fn rebuild_rollups(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(b): Json<RebuildBody>,
) -> Result<Json<Value>, ApiError> {
    let (op, _) = operator_of(&caller, &headers)?;
    let from = crate::control::usage::parse_day(&b.from)?;
    let to = crate::control::usage::parse_day(&b.to)?;
    tracing::info!(operator = %op, "usage rollups rebuilt from facts");
    Ok(Json(
        crate::ledger::rebuild_rollups(&s.db, from, to, b.environment_id).await?,
    ))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DayBody {
    day: String,
}

async fn backfill_api_operations(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(b): Json<DayBody>,
) -> Result<Json<Value>, ApiError> {
    let (op, _) = operator_of(&caller, &headers)?;
    let before = crate::control::usage::parse_day(&b.day)?;
    if before > chrono::Utc::now().date_naive() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    tracing::info!(operator = %op, "historical API_OPERATION usage backfilled");
    Ok(Json(
        crate::ledger::backfill_api_operations(&s, before).await?,
    ))
}

async fn reconcile_api_operations(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(b): Json<DayBody>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let day = crate::control::usage::parse_day(&b.day)?;
    Ok(Json(
        crate::ledger::reconcile_api_operations(&s.db, day).await?,
    ))
}

async fn reset_stream(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((producer, stream)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let (op, _) = operator_of(&caller, &headers)?;
    tracing::info!(operator = %op, %producer, %stream, "usage stream watermark reset (backfill)");
    Ok(Json(
        crate::ledger::reset_stream(&s.db, &producer, &stream).await?,
    ))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeriodQuery {
    from: String,
    to: String,
}

async fn cost_center_usage(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(cost_center): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PeriodQuery>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    let from = crate::control::usage::parse_day(&q.from)?;
    let to = crate::control::usage::parse_day(&q.to)?;
    Ok(Json(
        crate::ledger::cost_center_usage(&s.db, &cost_center, from, to).await?,
    ))
}

async fn set_support_profile(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::control::support::OperatorProfile>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::support::operator_put(&s, &op, &ctx, id, body).await?,
    ))
}

async fn record_incident(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::control::support::Incident>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    Ok(Json(
        crate::control::support::operator_incident(&s, &op, &ctx, id, body).await?,
    ))
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
