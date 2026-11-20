//! `GET /internal/v1/oauth/userinfo?accountId=&scopes=` (permission
//! `meeting-oauth-userinfo.read`, the Developer Console's `/v1/oauth/userinfo` backend):
//! `{displayName, avatarUrl, email?}` of an ACTIVE account. `email` only when `scopes`
//! (space- or comma-separated OAuth scopes) holds `profile:email` and the address is
//! verified; `profile:read` (or `profile:email`) is required at all. Unknown or inactive
//! accounts are `404 NOT_FOUND`. Mounted only with `MEETING_OAUTH_AUTHORIZE_ENABLED`.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use cf_service_auth::{InternalPermission, ServiceTokenVerifier};
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

pub const USERINFO_PATH: &str = "/internal/v1/oauth/userinfo";

#[derive(Clone)]
struct UserinfoState {
    app: AppState,
    verifier: Arc<ServiceTokenVerifier>,
}

pub fn router(app: AppState, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(USERINFO_PATH, get(userinfo))
        .with_state(UserinfoState { app, verifier })
}

fn error(status: StatusCode, code: &str, request_id: Option<&str>) -> Response {
    let mut r = (
        status,
        Json(json!({ "error": { "code": code, "message": code, "requestId": request_id } })),
    )
        .into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

async fn userinfo(State(s): State<UserinfoState>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    let ctx = match s
        .verifier
        .verify_request(&method, &path, request.headers())
        .await
    {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "oauth userinfo: service authentication rejected"
            );
            return error(
                StatusCode::UNAUTHORIZED,
                "INTERNAL_AUTHENTICATION_FAILED",
                None,
            );
        }
    };
    let rid = ctx.request_id().as_str().to_string();
    if crate::services::admin_operator::attribution_refused(
        ctx.authenticated_service(),
        request.headers(),
    ) {
        return error(
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            Some(&rid),
        );
    }
    if ctx
        .require_permission(InternalPermission::MeetingOauthUserinfoRead)
        .is_err()
    {
        return error(
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            Some(&rid),
        );
    }
    let mut account: Option<Uuid> = None;
    let mut scopes: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "accountId" if account.is_none() => account = Uuid::parse_str(&v).ok(),
            "scopes" if scopes.is_none() && v.len() <= 1024 => scopes = Some(v.into_owned()),
            _ => return error(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", Some(&rid)),
        }
    }
    let (Some(account), Some(scopes)) = (account, scopes) else {
        return error(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", Some(&rid));
    };
    let scopes: Vec<&str> = scopes.split([' ', ',']).filter(|s| !s.is_empty()).collect();
    if !scopes
        .iter()
        .any(|s| *s == "profile:read" || *s == "profile:email")
    {
        return error(StatusCode::FORBIDDEN, "SCOPE_REQUIRED", Some(&rid));
    }
    match super::service::userinfo(&s.app, account, &scopes).await {
        Ok(Some(body)) => {
            let mut r = (StatusCode::OK, Json(body)).into_response();
            r.headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            r
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "NOT_FOUND", Some(&rid)),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            Some(&rid),
        ),
    }
}
