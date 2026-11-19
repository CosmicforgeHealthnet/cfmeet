//! ADR 0064 / 0065 routes, each registered only while its flag is on (off: exactly
//! today's 404 / 405 answers):
//!
//! * console (`DEVELOPER_PLATFORM_APPS_ENABLED`, gateway-forwarded human session):
//!   `/v1/projects/{projectId}/app*` — merged into the customer router;
//! * operator (`DEVELOPER_PLATFORM_APPS_ENABLED`): `/internal/v1/operator/apps*`;
//! * ANONYMOUS (`DEVELOPER_PLATFORM_OAUTH_ENABLED`, `DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED`):
//!   `/v1/oauth/*` and `/v1/app-directory*`. The API Gateway forwards these routes without
//!   a caller identity; the client secret or the OAuth token in the request is the
//!   authentication, verified by Platform Access. Also off unless
//!   `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED` (like every `/v1` route).

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::from_fn,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde_json::Value;
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::{request_context, InternalCaller},
};
use crate::{
    apps::{directory, oauth, registration, review},
    config::BuildFeatures,
    control::{
        idempotency,
        pagination::{Page, PageQuery},
        Caller,
    },
    domain::current_context,
    state::AppState,
};

type ApiResult<T> = Result<T, ApiError>;

/// Console routes (merged under the gateway-authenticated `/v1`).
pub fn console_routes(build: BuildFeatures) -> Router<AppState> {
    if !build.apps {
        return Router::new();
    }
    Router::new()
        .route("/projects/:project_id/app", get(get_app).put(put_app))
        .route("/projects/:project_id/app/secret", post(rotate_secret))
        .route("/projects/:project_id/app/submit", post(submit))
        .route("/projects/:project_id/app/grants", get(grants))
        .route(
            "/projects/:project_id/app/test-accounts",
            put(put_test_accounts),
        )
}

/// Operator routes (merged under `/internal/v1`).
pub fn operator_routes(build: BuildFeatures) -> Router<AppState> {
    if !build.apps {
        return Router::new();
    }
    Router::new()
        .route("/operator/apps", get(operator_list))
        .route("/operator/apps/:app_id", get(operator_get))
        .route("/operator/apps/:app_id/:command", post(operator_decide))
}

/// The anonymous routes (full paths; merged at the top level).
pub fn anonymous_routes(state: AppState) -> Router {
    let build = state.build;
    let mut r = Router::new();
    if build.oauth {
        r = r
            .route("/v1/oauth/token", post(token))
            .route("/v1/oauth/revoke", post(revoke))
            .route("/v1/oauth/userinfo", get(userinfo))
            .route("/v1/oauth/metadata", get(metadata));
    }
    if build.app_directory {
        r = r
            .route("/v1/app-directory", get(directory_list))
            .route("/v1/app-directory/:app_id", get(directory_get));
    }
    r.layer(DefaultBodyLimit::max(super::MAX_BODY_BYTES))
        .layer(from_fn(request_context))
        .with_state(state)
}

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn get_app(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(registration::get(&s, &c, id).await?))
}

async fn put_app(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let (status, v) = registration::put(&s, &c, id, &body).await?;
    Ok((status, Json(v)).into_response())
}

async fn rotate_secret(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let scope = format!("{}:{id}:app.secret", c.actor_ref());
    let digest = idempotency::digest(&[scope.as_bytes()]);
    let key = idem_key(&headers);
    // A replay returns the secret's metadata and NEVER the secret again.
    let (status, body, replayed) = idempotency::run(
        &s.db,
        &scope,
        key.as_deref(),
        &digest,
        registration::redact_secret,
        || registration::rotate_secret(&s, &c, id),
    )
    .await?;
    let mut r = (status, Json(body)).into_response();
    let h = r.headers_mut();
    h.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    if replayed {
        h.insert(
            axum::http::HeaderName::from_static("idempotent-replayed"),
            axum::http::HeaderValue::from_static("true"),
        );
    }
    Ok(r)
}

async fn submit(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(registration::submit(&s, &c, id).await?))
}

async fn grants(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    q: Result<Query<registration::GrantsQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<Value>> {
    let Query(q) = q.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    Ok(Json(registration::grants(&s, &c, id, q).await?))
}

async fn put_test_accounts(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<Uuid>,
    Json(body): Json<registration::PutTestAccounts>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        registration::put_test_accounts(&s, &c, id, body).await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Operator review
// ---------------------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusQuery {
    status: Option<String>,
}

async fn operator_list(
    State(s): State<AppState>,
    caller: InternalCaller,
    q: Result<Query<StatusQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<Value>> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    let Query(q) = q.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    Ok(Json(review::operator_list(&s.db, q.status.as_deref()).await?))
}

async fn operator_get(
    State(s): State<AppState>,
    caller: InternalCaller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    Ok(Json(review::operator_get(&s.db, id).await?))
}

/// `POST …/{appId}/{review|approve|reject|publish|suspend}`. Every command but suspend
/// needs a named reviewer and the reviewing operator's CF Meet user id
/// (`x-cf-operator-user-id`; a member of the app's workspace never decides it).
async fn operator_decide(
    State(s): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path((id, command)): Path<(Uuid, String)>,
    body: Option<Json<review::Decision>>,
) -> ApiResult<Json<Value>> {
    caller.require_permission(InternalPermission::DeveloperPlatformAdminister)?;
    let operator = headers
        .get("x-cf-operator")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| crate::control::operator::valid_operator(v))
        .ok_or(ApiError(ErrorCode::OperatorRequired))?
        .to_string();
    let ctx = current_context().ok_or(ApiError(ErrorCode::InternalError))?;
    let command = review::Command::parse(&command).ok_or(ApiError(ErrorCode::NotFound))?;
    let user = headers
        .get("x-cf-operator-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| Uuid::parse_str(v.trim()).map_err(|_| ApiError(ErrorCode::OperatorRequired)))
        .transpose()?;
    Ok(Json(
        review::operator_decide(
            &s,
            &operator,
            user,
            &ctx,
            id,
            command,
            body.map(|Json(b)| b).unwrap_or_default(),
        )
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Anonymous: OAuth and the directory
// ---------------------------------------------------------------------------------------

fn gateway_on(s: &AppState) -> Option<Response> {
    (!s.flags.gateway_api).then(|| ApiError(ErrorCode::NotFound).into_response())
}

async fn token(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    oauth::token(&s, &headers, &body).await
}

async fn revoke(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    oauth::revoke(&s, &headers, &body).await
}

async fn userinfo(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    oauth::userinfo(&s, &headers).await
}

async fn metadata(State(s): State<AppState>) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    oauth::metadata(&s)
}

async fn directory_list(
    State(s): State<AppState>,
    q: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    let result = async {
        let Query(q) = q.map_err(|_| ApiError(ErrorCode::ValidationError))?;
        directory::list(&s, &Page::from_query(&q)?).await
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn directory_get(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if let Some(r) = gateway_on(&s) {
        return r;
    }
    let Ok(id) = Uuid::parse_str(&id) else {
        return ApiError(ErrorCode::NotFound).into_response();
    };
    match directory::get(&s, id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}
