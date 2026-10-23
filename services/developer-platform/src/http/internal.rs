//! Service-authenticated `/internal/v1` routes.
//!
//! | Route | Permission | Context |
//! |---|---|---|
//! | `GET /internal/v1/gateway/context` | `developer-request.forward` (API Gateway only) | signed delegated context, re-validated by Platform Access |
//! | `GET /internal/v1/entitlements/environments/{id}` | `developer-entitlement.read` | — |
//! | `POST /internal/v1/usage/api-requests` | `developer-usage.record` | idempotent batch |
//! | `GET /internal/v1/usage/api-requests/{environmentId}/{day}` | `developer-usage.record` | — |
//! | `POST /internal/v1/platform/events` | `developer-event.publish` (producers; the envelope's producer is the caller) | Section 53, behind `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` |
//!
//! `gateway/context` is the Part VI foundation route of the customer path: it returns the
//! canonical Platform Access ids of the caller's application environment as Platform
//! Access confirms them, and anchors that environment in `devplat_environment_refs`. It is
//! behind `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` (default off: 404). The functional
//! Developer Platform API (Sections 50+) is not implemented here.

use axum::{extract::State, routing::get, Json, Router};
use cf_service_auth::InternalPermission;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    domain::current_context,
    platform_access::{CanonicalContext, ContextOutcome},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(crate::webhooks::http::internal_routes())
        .route("/gateway/context", get(gateway_context))
        .route(
            "/entitlements/environments/:environment_id",
            get(environment_entitlements),
        )
        .route("/usage/api-requests", axum::routing::post(record_usage))
        .route("/usage/api-requests/:environment_id/:day", get(daily_usage))
}

/// `POST /internal/v1/usage/api-requests` — an idempotent batch of accepted public API
/// request counts (API Gateway quota accounting). Permission `developer-usage.record`.
async fn record_usage(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(batch): Json<crate::control::usage::UsageBatch>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperUsageRecord)?;
    let by = caller
        .context()
        .authenticated_service()
        .as_str()
        .to_string();
    Ok(Json(
        crate::control::usage::record(&state.db, &by, batch).await?,
    ))
}

/// `GET /internal/v1/usage/api-requests/{environmentId}/{day}` — the durable daily total
/// the gateway re-seeds its shared day window from. Permission `developer-usage.record`.
async fn daily_usage(
    State(state): State<AppState>,
    caller: InternalCaller,
    axum::extract::Path((environment_id, day)): axum::extract::Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperUsageRecord)?;
    Ok(Json(
        crate::control::usage::daily(&state.db, environment_id, &day).await?,
    ))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitlementQuery {
    /// The environment kind Platform Access confirmed to the caller (used only for an
    /// environment without a Developer Platform profile).
    kind: Option<String>,
}

/// `GET /internal/v1/entitlements/environments/{id}` — effective platform entitlements
/// (API Gateway quota configuration). Permission `developer-entitlement.read`.
async fn environment_entitlements(
    State(state): State<AppState>,
    caller: InternalCaller,
    axum::extract::Path(environment_id): axum::extract::Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<EntitlementQuery>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::DeveloperEntitlementRead)?;
    let kind = match query.kind.as_deref() {
        None => None,
        Some(k @ ("SANDBOX" | "TEST" | "PRODUCTION")) => Some(k),
        Some(_) => return Err(ApiError(ErrorCode::ValidationError)),
    };
    Ok(Json(
        crate::control::entitlements::effective(&state.db, environment_id, kind).await?,
    ))
}

async fn gateway_context(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Json<Value>, ApiError> {
    if !state.flags.gateway_api {
        return Err(ApiError(ErrorCode::NotFound));
    }
    caller.require_permission(InternalPermission::DeveloperRequestForward)?;
    let delegated = caller
        .context()
        .delegated_platform_context()
        .cloned()
        .ok_or(ApiError(ErrorCode::DelegatedContextRequired))?;
    let correlation = caller.context().correlation_id().clone();
    let confirmed = match state
        .platform_access
        .trusted_context(&delegated, &correlation)
        .await
    {
        Ok(ContextOutcome::Confirmed(c)) if c.matches(&delegated) => c,
        Ok(ContextOutcome::Confirmed(_)) => {
            tracing::warn!("platform access confirmed a different chain than delegated");
            return Err(ApiError(ErrorCode::DelegatedContextInvalid));
        }
        Ok(ContextOutcome::Refused { code }) => {
            tracing::info!(%code, "platform access refused the delegated context");
            return Err(ApiError(ErrorCode::DelegatedContextInvalid));
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            return Err(ApiError(ErrorCode::ServiceUnavailable));
        }
    };
    record_environment_ref(&state.db, &confirmed).await?;
    Ok(Json(json!({
        "tenantId": confirmed.tenant_id,
        "applicationId": confirmed.application_id,
        "environmentId": confirmed.environment_id,
        "environmentKind": confirmed.environment_kind,
        "serviceAccountId": confirmed.service_account_id,
        "authorityMode": confirmed.authority_mode,
        "authority": "PLATFORM_ACCESS",
        "forwardedBy": caller.context().authenticated_service().as_str(),
        "correlationId": current_context().map(|c| c.correlation_id.as_str().to_string()),
    })))
}

/// Anchors a CONFIRMED environment chain (canonical ids only; idempotent).
pub async fn record_environment_ref<C: ConnectionTrait>(
    db: &C,
    ctx: &CanonicalContext,
) -> Result<(), sea_orm::DbErr> {
    let now = Utc::now();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO devplat_environment_refs \
             (id, tenant_id, application_id, environment_id, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $5, $5) \
         ON CONFLICT (environment_id) DO UPDATE SET last_seen_at = EXCLUDED.last_seen_at",
        vec![
            Uuid::new_v4().into(),
            ctx.tenant_id.into(),
            ctx.application_id.into(),
            ctx.environment_id.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}
