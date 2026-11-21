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
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    control::{
        bridge_command::{self, BridgeCommand, Expect, Subject},
        commercial, operator,
        operator::{RegisterFirstParty, SetState},
        organizations,
        pagination::{Page, PageQuery},
    },
    domain::{current_context, rbac::OrganizationState, RequestContext},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/operator/organizations",
            post(register_first_party).get(list_organizations),
        )
        .route("/operator/activity", get(operator_activity))
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
        // ADR 0068 A4: the admin bridge sends every command as POST.
        .route(
            "/operator/organizations/:organization_id/support-profile",
            put(set_support_profile).post(set_support_profile),
        )
        .route(
            "/operator/organizations/:organization_id/incidents",
            post(record_incident),
        )
        // ADR 0068 A4: admin bridge only.
        .route(
            "/operator/organizations/:organization_id/push-providers/:push_provider_id/disable",
            post(disable_push_key),
        )
        .route(
            "/operator/organizations/:organization_id/webhook-deliveries/:delivery_id/retry",
            post(retry_webhook_delivery),
        )
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DisableBody {
    expected_version: Option<i64>,
    #[serde(default)]
    reason_code: Option<String>,
}

/// `POST …/organizations/{id}/push-providers/{pushProviderId}/disable` (admin bridge only,
/// R3: HQ's approver required).
async fn disable_push_key(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, push_key)): Path<(Uuid, Uuid)>,
    Json(body): Json<DisableBody>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    let bridge = BridgeCommand::required(
        &caller,
        &headers,
        body.expected_version,
        true,
        Expect::Version,
    )?;
    Ok(Json(
        crate::control::push_credentials::operator_disable(
            &s,
            &op,
            &ctx,
            id,
            push_key,
            body.reason_code,
            &bridge,
        )
        .await?,
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}

/// `POST …/organizations/{id}/webhook-deliveries/{deliveryId}/retry` (admin bridge only,
/// R2; unversioned: no `expectedVersion`).
async fn retry_webhook_delivery(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, delivery)): Path<(Uuid, Uuid)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    // No body or `{}`; anything else (an expectedVersion included) is refused.
    if !body.iter().all(u8::is_ascii_whitespace) {
        serde_json::from_slice::<EmptyBody>(&body)
            .map_err(|_| ApiError(ErrorCode::ValidationError))?;
    }
    let bridge = BridgeCommand::required(&caller, &headers, None, false, Expect::Nothing)?;
    let svc = s
        .webhooks
        .as_ref()
        .filter(|w| w.settings().enabled)
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(Json(
        svc.operator_retry(&op, &ctx, id, delivery, &bridge).await?,
    ))
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
    let decision = body.map(|Json(b)| b).unwrap_or_default();
    let bridge = crate::control::bridge_command::BridgeCommand::of(
        &caller,
        &headers,
        decision.expected_version,
        command.two_person(),
    )?;
    let v = crate::control::production::operator_decide(
        &s,
        &op,
        user,
        &ctx,
        id,
        command,
        decision,
        bridge.as_ref(),
    )
    .await?;
    // The admin bridge gets the case as HQ sees it (state and version included).
    if bridge.is_some() {
        use crate::control::verification;
        return Ok(Json(
            verification::read(
                &s.db,
                &verification::case_id(verification::PARTNER_GO_LIVE, id),
            )
            .await?,
        ));
    }
    Ok(Json(v))
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
    let (op, ctx) = operator_of(&caller, &headers)?;
    let from = crate::control::usage::parse_day(&b.from)?;
    let to = crate::control::usage::parse_day(&b.to)?;
    // ADR 0068 A4 (`partners.usage.reprocess`, R2): from the admin bridge the key is
    // required; the rollups are unversioned (no expectedVersion).
    let bridge = BridgeCommand::of_with(&caller, &headers, None, false, Expect::Nothing)?;
    const REPROCESS: &str = "partners.usage.reprocess";
    let subject = b.environment_id.unwrap_or_else(Uuid::nil);
    let resource = |out: Option<&Value>| {
        json!({
            "environmentId": b.environment_id, "from": b.from, "to": b.to,
            "removed": out.map(|o| o["removed"].clone()), "rebuilt": out.map(|o| o["rebuilt"].clone()),
        })
    };
    if let Some(cmd) = &bridge {
        if let Some((st, v)) =
            bridge_command::prior(&s.db, cmd, REPROCESS, Subject::UsageRollups, subject).await?
        {
            return Ok(Json(bridge_command::answer(&st, v, resource(None))));
        }
    }
    let txn = s.db.begin().await?;
    let out = crate::ledger::rebuild_rollups(&txn, from, to, b.environment_id).await?;
    operator::audit_command(
        &txn,
        &op,
        &ctx,
        "usage.rollups_rebuilt",
        "USAGE_ROLLUPS",
        &b.environment_id
            .map_or_else(|| "ALL".to_string(), |e| e.to_string()),
        json!({ "from": b.from, "to": b.to, "environmentId": b.environment_id }),
    )
    .await?;
    if let Some(cmd) = &bridge {
        let tenant = match b.environment_id {
            Some(e) => txn
                .query_one(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT tenant_id FROM devplat_environment_profiles WHERE environment_id = $1",
                    vec![e.into()],
                ))
                .await?
                .map(|r| r.try_get::<Uuid>("", "tenant_id"))
                .transpose()?
                .unwrap_or_else(Uuid::nil),
            None => Uuid::nil(),
        };
        bridge_command::record_result(
            &txn,
            cmd,
            REPROCESS,
            Subject::UsageRollups,
            subject,
            tenant,
            "REPROCESSED",
            None,
        )
        .await?;
        txn.commit().await?;
        return Ok(Json(bridge_command::answer(
            "REPROCESSED",
            None,
            resource(Some(&out)),
        )));
    }
    txn.commit().await?;
    Ok(Json(out))
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
    let (op, ctx) = operator_of(&caller, &headers)?;
    let before = crate::control::usage::parse_day(&b.day)?;
    if before > chrono::Utc::now().date_naive() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    // The backfill commits per environment and is idempotent; it is audited once it
    // returns (a failure of the audit write fails the request, and a re-run is safe).
    let out = crate::ledger::backfill_api_operations(&s, before).await?;
    operator::audit_command(
        &s.db,
        &op,
        &ctx,
        "usage.api_operations_backfilled",
        "USAGE_LEDGER",
        &b.day,
        json!({ "before": b.day }),
    )
    .await?;
    Ok(Json(out))
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
    let (op, ctx) = operator_of(&caller, &headers)?;
    let txn = s.db.begin().await?;
    let out = crate::ledger::reset_stream(&txn, &producer, &stream).await?;
    operator::audit_command(
        &txn,
        &op,
        &ctx,
        "usage.stream_reset",
        "USAGE_STREAM",
        &format!("{producer}/{stream}"),
        json!({ "producer": producer, "stream": stream }),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(out))
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
    // ADR 0068 A4 (R2): from the admin bridge, the key and the profile version (0: none
    // yet) are required.
    let bridge = BridgeCommand::of_with(
        &caller,
        &headers,
        body.expected_version,
        false,
        Expect::VersionOrAbsent,
    )?;
    Ok(Json(
        crate::control::support::operator_put(&s, &op, &ctx, id, body, bridge.as_ref()).await?,
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
    let bridge = BridgeCommand::of_with(
        &caller,
        &headers,
        body.expected_version,
        false,
        Expect::VersionOrAbsent,
    )?;
    Ok(Json(
        crate::control::support::operator_incident(&s, &op, &ctx, id, body, bridge.as_ref())
            .await?,
    ))
}

pub(super) fn operator_of(
    caller: &InternalCaller,
    headers: &HeaderMap,
) -> Result<(String, RequestContext), ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    // The admin bridge names an HQ staff member (`hq:<staff>`, verified by the middleware);
    // the operator console names a human operator in the legacy form.
    let name = match caller.bridge_operator() {
        Some(a) => a.operator.as_str().to_string(),
        None => headers
            .get("x-cf-operator")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| operator::valid_operator(v))
            .ok_or(ApiError(ErrorCode::OperatorRequired))?
            .to_string(),
    };
    let ctx = current_context().ok_or(ApiError(ErrorCode::InternalError))?;
    Ok((name, ctx))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrganizationsQuery {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// Section 15 (PLT-009): operator list / search of organizations (read-only; permission
/// AND a named operator, like every operator read of audit-grade data).
async fn list_organizations(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<OrganizationsQuery>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let page = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    })?;
    let state = q
        .state
        .as_deref()
        .map(|v| OrganizationState::parse(v).ok_or(ApiError(ErrorCode::ValidationError)))
        .transpose()?;
    let search = operator::OrganizationSearch {
        query: q.query,
        state,
    };
    Ok(Json(
        operator::list_organizations(&s.db, &search, &page).await?,
    ))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityQuery {
    #[serde(default)]
    organization_id: Option<Uuid>,
    #[serde(default)]
    operator: Option<String>,
    #[serde(default)]
    action: Option<String>,
    /// ADR 0068: optional window (RFC 3339), at most 31 days when both are given.
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// Section 15 (PLT-009): the operator audit trail (organization-scoped OPERATOR events
/// and platform-level operator commands), newest first.
async fn operator_activity(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ActivityQuery>,
) -> Result<Json<Value>, ApiError> {
    operator_of(&caller, &headers)?;
    let page = Page::from_query(&PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    })?;
    let (since, until) = super::operator_reads::window(q.since.as_deref(), q.until.as_deref())?;
    let filter = operator::OperatorActivityFilter {
        organization_id: q.organization_id,
        operator: q.operator,
        action: q.action,
        since,
        until,
    };
    Ok(Json(
        operator::operator_activity(&s.db, &filter, &page).await?,
    ))
}

async fn register_first_party(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Json(body): Json<RegisterFirstParty>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    // ADR 0068 A4 (`partners.workspace.register-first-party`, R4): from the admin bridge,
    // the key and HQ's approver are required, no expectedVersion (the workspace does not
    // exist yet), and the owner invitation is delivered to the owner only.
    if let Some(cmd) = BridgeCommand::of_with(&caller, &headers, None, true, Expect::Nothing)? {
        return Ok((
            StatusCode::OK,
            Json(operator::register_first_party_bridge(&s, &op, &ctx, body, &cmd).await?),
        ));
    }
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
    // ADR 0068 A4 (R3): from the admin bridge, the key, the workspace version and HQ's
    // approver are required.
    let bridge = BridgeCommand::of(&caller, &headers, body.expected_version, true)?;
    Ok(Json(
        organizations::operator_set_state(&s, &op, &ctx, id, to, &body.reason, bridge.as_ref())
            .await?,
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
    let (op, ctx) = operator_of(&caller, &headers)?;
    let txn = s.db.begin().await?;
    let plan = commercial::operator_publish_plan(&txn, body).await?;
    operator::audit_command(
        &txn,
        &op,
        &ctx,
        "commercial_plan.published",
        "COMMERCIAL_PLAN",
        plan["id"].as_str().unwrap_or_default(),
        json!({ "code": plan["code"], "version": plan["version"] }),
    )
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

async fn retire_plan(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (op, ctx) = operator_of(&caller, &headers)?;
    let txn = s.db.begin().await?;
    let plan = commercial::operator_retire_plan(&txn, id).await?;
    operator::audit_command(
        &txn,
        &op,
        &ctx,
        "commercial_plan.retired",
        "COMMERCIAL_PLAN",
        &id.to_string(),
        json!({}),
    )
    .await?;
    txn.commit().await?;
    Ok(Json(plan))
}
