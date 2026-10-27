//! Operator routes of the webhook migration (`/internal/v1/operator/webhook-migration/**`):
//! permission `developer-platform.administer` (operator console only) AND a named human
//! operator in `x-cf-operator`. Commands that change state write an append-only audit row;
//! reads (the dry-run inventory, reconcile, shadow report, cutover check, status) write
//! nothing and are logged with the operator.
//!
//! | Route | Command |
//! |---|---|
//! | `GET  …/status` | phase, latest run, counts |
//! | `GET  …/inventory` | dry run: what an import would do (writes nothing) |
//! | `POST …/import` | import / resume (`{"pageSize": 1-500}`) |
//! | `GET  …/reconcile` | inventory + digests + record drift |
//! | `GET  …/shadow-report?since=` | dual-ingestion parity of the shadow window |
//! | `GET  …/cutover-check` | the cutover preconditions |
//! | `POST …/rotation-acknowledgements` | acknowledge required customer rotations |
//! | `POST …/takeover` | the delivery switch (after Calling stopped sending) |
//! | `POST …/rollback-export` | push subscription changes back to Calling |
//! | `POST …/rollback` | stop sending here, settle / hand back open deliveries |

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::Value;

use super::{
    cutover::{Acknowledge, RollbackOptions},
    import::ImportOptions,
    Migration,
};
use crate::{
    control::operator::valid_operator,
    domain::current_context,
    http::{
        error::{ApiError, ErrorCode},
        middleware::InternalCaller,
    },
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operator/webhook-migration/status", get(status))
        .route("/operator/webhook-migration/inventory", get(inventory))
        .route("/operator/webhook-migration/import", post(import))
        .route("/operator/webhook-migration/reconcile", get(reconcile))
        .route(
            "/operator/webhook-migration/shadow-report",
            get(shadow_report),
        )
        .route(
            "/operator/webhook-migration/cutover-check",
            get(cutover_check),
        )
        .route(
            "/operator/webhook-migration/rotation-acknowledgements",
            post(acknowledge),
        )
        .route("/operator/webhook-migration/takeover", post(takeover))
        .route(
            "/operator/webhook-migration/rollback-export",
            post(rollback_export),
        )
        .route("/operator/webhook-migration/rollback", post(rollback))
}

type R = Result<Json<Value>, ApiError>;

/// The migration context of an authorized operator command.
fn migration<'a>(
    s: &'a AppState,
    caller: &InternalCaller,
    headers: &'a HeaderMap,
) -> Result<Migration<'a>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    let operator = headers
        .get("x-cf-operator")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| valid_operator(v))
        .ok_or(ApiError(ErrorCode::OperatorRequired))?;
    let webhooks = s
        .webhooks
        .as_ref()
        .ok_or(ApiError(ErrorCode::CapabilityUnavailable))?;
    let ctx = current_context().ok_or(ApiError(ErrorCode::InternalError))?;
    Ok(Migration {
        db: &s.db,
        webhooks,
        calling: s.calling_export.as_ref(),
        platform_access: s.platform_access.as_ref(),
        operator,
        request_id: ctx.request_id.as_str().to_string(),
        correlation: ctx.correlation_id.clone(),
    })
}

fn body<T: for<'de> Deserialize<'de> + Default>(
    b: Result<Json<Value>, JsonRejection>,
) -> Result<T, ApiError> {
    match b {
        Ok(Json(Value::Null)) | Err(JsonRejection::MissingJsonContentType(_)) => Ok(T::default()),
        Ok(Json(v)) => serde_json::from_value(v).map_err(|_| ApiError(ErrorCode::ValidationError)),
        Err(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

fn query(
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
    allowed: &[&str],
) -> Result<Vec<(String, String)>, ApiError> {
    let Query(pairs) = q.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    if pairs.iter().any(|(k, _)| !allowed.contains(&k.as_str())) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(pairs)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportBody {
    #[serde(default)]
    page_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AckBody {
    #[serde(default)]
    webhook_ids: Vec<uuid::Uuid>,
}

async fn status(State(s): State<AppState>, caller: InternalCaller, headers: HeaderMap) -> R {
    Ok(Json(migration(&s, &caller, &headers)?.status().await?))
}

async fn inventory(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> R {
    let m = migration(&s, &caller, &headers)?;
    let q = query(q, &["pageSize"])?;
    let page_size = match q.iter().find(|(k, _)| k == "pageSize") {
        None => 100,
        Some((_, v)) => v
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=500).contains(n))
            .ok_or(ApiError(ErrorCode::ValidationError))?,
    };
    Ok(Json(m.inventory(page_size).await?))
}

async fn import(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    b: Result<Json<Value>, JsonRejection>,
) -> R {
    let m = migration(&s, &caller, &headers)?;
    let b: ImportBody = body(b)?;
    Ok(Json(
        m.import(ImportOptions {
            page_size: b.page_size.unwrap_or(100),
            fail_after_pages: None,
        })
        .await?,
    ))
}

async fn reconcile(State(s): State<AppState>, caller: InternalCaller, headers: HeaderMap) -> R {
    Ok(Json(migration(&s, &caller, &headers)?.reconcile().await?))
}

async fn shadow_report(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> R {
    let m = migration(&s, &caller, &headers)?;
    let q = query(q, &["since"])?;
    let since = q
        .iter()
        .find(|(k, _)| k == "since")
        .map(|(_, v)| {
            chrono::DateTime::parse_from_rfc3339(v)
                .map(|t| t.with_timezone(&chrono::Utc))
                .map_err(|_| ApiError(ErrorCode::ValidationError))
        })
        .transpose()?;
    Ok(Json(m.shadow_report(since).await?))
}

async fn cutover_check(State(s): State<AppState>, caller: InternalCaller, headers: HeaderMap) -> R {
    Ok(Json(
        migration(&s, &caller, &headers)?.cutover_check().await?,
    ))
}

async fn acknowledge(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    b: Result<Json<Value>, JsonRejection>,
) -> R {
    let m = migration(&s, &caller, &headers)?;
    let b: AckBody = body(b)?;
    Ok(Json(
        m.acknowledge(Acknowledge {
            webhook_ids: b.webhook_ids,
        })
        .await?,
    ))
}

async fn takeover(State(s): State<AppState>, caller: InternalCaller, headers: HeaderMap) -> R {
    Ok(Json(migration(&s, &caller, &headers)?.takeover().await?))
}

async fn rollback_export(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
) -> R {
    Ok(Json(
        migration(&s, &caller, &headers)?.rollback_export().await?,
    ))
}

async fn rollback(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    b: Result<Json<Value>, JsonRejection>,
) -> R {
    let m = migration(&s, &caller, &headers)?;
    let opts: RollbackOptions = body(b)?;
    Ok(Json(m.rollback(opts).await?))
}
