//! Allowed websites (ADR 0062; contract `docs/platform/build-internal-contracts.md` §2.3),
//! behind `PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED` (off: `404 NOT_FOUND` like an
//! unknown route).
//!
//! | Route | Permission | Result |
//! |---|---|---|
//! | `GET /environment-origins?after=&limit=` | `platform-environment-origins.read` | change feed `{items:[{environmentId, origins, environmentStatus, seq}], nextAfter}` |
//! | `GET /environments/{environmentId}/origins` | `platform-environment-origins.read` | `{environmentId, origins}` |
//! | `GET /delegation/environments/{environmentId}/allowed-origins` | `platform-identity.delegate` | `{environmentId, origins}` |
//! | `PUT /delegation/environments/{environmentId}/allowed-origins` `{origins}` | `platform-identity.delegate` + `X-CF-Delegated-Actor` (audited) | `{environmentId, origins}` |

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    delegation::{delegated, read_only},
    error::{AccessError, AccessResult, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    domain::{EnvironmentId, TenantClassification},
    identity::{environment_origins, environments::EnvironmentView},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/environment-origins", get(origin_feed))
        .route(
            "/environments/:environment_id/origins",
            get(environment_origins_read),
        )
        .route(
            "/delegation/environments/:environment_id/allowed-origins",
            get(console_get_origins).put(console_set_origins),
        )
}

fn enabled(state: &AppState) -> AccessResult<()> {
    if state.build.environment_origins_enabled {
        Ok(())
    } else {
        Err(AccessError::Refused(ErrorCode::NotFound))
    }
}

fn internal(err: sea_orm::DbErr) -> AccessError {
    AccessError::service(err.into())
}

async fn environment(state: &AppState, id: Uuid) -> AccessResult<EnvironmentView> {
    state
        .platform
        .environment(EnvironmentId::from_uuid(id))
        .await
        .map_err(AccessError::operator)
}

async fn origins_json(state: &AppState, environment_id: Uuid) -> AccessResult<serde_json::Value> {
    let origins = environment_origins::origins_of(state.platform.db(), environment_id)
        .await
        .map_err(internal)?;
    Ok(json!({ "environmentId": environment_id, "origins": origins }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedQuery {
    #[serde(default)]
    after: Option<i64>,
    #[serde(default)]
    limit: Option<u64>,
}

async fn origin_feed(
    State(state): State<AppState>,
    caller: InternalCaller,
    Query(q): Query<FeedQuery>,
) -> AccessResult<impl IntoResponse> {
    enabled(&state)?;
    caller.require_permission(InternalPermission::PlatformEnvironmentOriginsRead)?;
    let page = environment_origins::feed(state.platform.db(), q.after.unwrap_or(0), q.limit)
        .await
        .map_err(AccessError::operator)?;
    Ok(Json(page))
}

async fn environment_origins_read(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(environment_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    enabled(&state)?;
    caller.require_permission(InternalPermission::PlatformEnvironmentOriginsRead)?;
    environment(&state, environment_id).await?;
    Ok(Json(origins_json(&state, environment_id).await?))
}

async fn console_get_origins(
    State(state): State<AppState>,
    caller: InternalCaller,
    Path(environment_id): Path<Uuid>,
) -> AccessResult<impl IntoResponse> {
    enabled(&state)?;
    read_only(&caller)?;
    environment(&state, environment_id).await?;
    Ok(Json(origins_json(&state, environment_id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginsBody {
    origins: Vec<String>,
}

async fn console_set_origins(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<OriginsBody>,
) -> AccessResult<impl IntoResponse> {
    enabled(&state)?;
    let op = delegated(&caller, &headers)?;
    let env = environment(&state, environment_id).await?;
    let tenant = state
        .platform
        .tenant(env.tenant_id)
        .await
        .map_err(AccessError::operator)?;
    if tenant.classification != TenantClassification::External {
        return Err(AccessError::Operator(
            ErrorCode::PolicyViolation,
            "allowed websites are managed for EXTERNAL tenants only".to_string(),
        ));
    }
    let origins = state
        .platform
        .set_environment_origins(&op, &env, &body.origins)
        .await
        .map_err(AccessError::operator)?;
    Ok(Json(
        json!({ "environmentId": environment_id, "origins": origins }),
    ))
}
