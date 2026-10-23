//! Developer Console session bridge (Part VI §50): `POST /internal/v1/user-sessions/verify`.
//!
//! The Meeting Service stays the ONLY authority of CF Meet human sessions (it issues and
//! revokes them). The Developer Platform must authorize browser-facing control-plane
//! actions with that same verified session, without ever holding the session signing
//! secret: it presents the session to this route over authenticated internal HTTP
//! (`user-session.verify`, registry-grantable to `cfmeet.developer-platform` only) and
//! receives the verified account id.
//!
//! Verification is the v2 [`resolve_actor`](super::actor::resolve_actor) rule, stricter:
//! the token must verify, must not be revoked — and the revocation store MUST be
//! reachable (fail closed) — and must name an active, non-guest account. Guest /
//! participant credentials never authenticate a developer.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `DEVELOPER_SESSION_BRIDGE_ENABLED` | `true` mounts the route (default off) |
//! | `CF_RUNTIME_ENVIRONMENT` | runtime environment of the trust bundle |
//! | `CF_SERVICE_TRUST_BUNDLE_FILE` | receiver trust bundle (`cfmeet.meeting-service`) |

use std::{path::PathBuf, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    InternalPermission, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, SystemClock,
    TrustPolicy,
};
use sea_orm::EntityTrait;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    config::env_bool,
    models::users::{self, AuthType, UserRole, UserStatus},
    state::AppState,
};

pub const VERIFY_PATH: &str = "/internal/v1/user-sessions/verify";

#[derive(Debug, Clone)]
pub struct SessionBridgeConfig {
    pub runtime: RuntimeEnvironment,
    pub trust_bundle_file: PathBuf,
}

impl SessionBridgeConfig {
    /// `Ok(None)`: off. `Err`: on but misconfigured (startup fails closed).
    pub fn from_env() -> Result<Option<Self>, String> {
        if !env_bool("DEVELOPER_SESSION_BRIDGE_ENABLED", false) {
            return Ok(None);
        }
        let var = |name: &str| match std::env::var(name) {
            Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
            _ => Err(format!(
                "{name} is required when DEVELOPER_SESSION_BRIDGE_ENABLED is on"
            )),
        };
        let runtime = RuntimeEnvironment::parse(&var("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        Ok(Some(Self {
            runtime,
            trust_bundle_file: PathBuf::from(var("CF_SERVICE_TRUST_BUNDLE_FILE")?),
        }))
    }

    pub fn verifier(&self) -> Result<Arc<ServiceTokenVerifier>, String> {
        let policy = TrustPolicy::from_file(
            &self.trust_bundle_file,
            &ServiceId::meeting_service(),
            self.runtime,
        )
        .map_err(|e| format!("session bridge trust bundle: {e}"))?;
        Ok(Arc::new(ServiceTokenVerifier::new(
            policy,
            Arc::new(SystemClock),
        )))
    }
}

#[derive(Clone)]
struct BridgeState {
    app: AppState,
    verifier: Arc<ServiceTokenVerifier>,
}

/// The bridge router (mounted only when enabled).
pub fn router(app: AppState, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(VERIFY_PATH, post(verify))
        .with_state(BridgeState { app, verifier })
}

fn error(status: StatusCode, code: &str, request_id: Option<&str>) -> Response {
    let mut r = (
        status,
        Json(json!({ "error": { "code": code, "message": "The session could not be verified.", "requestId": request_id } })),
    )
        .into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyBody {
    session_token: String,
}

async fn verify(State(s): State<BridgeState>, request: Request) -> Response {
    let ctx = match s.verifier.verify_headers(request.headers()) {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(
                reason = e.failure.as_str(),
                "session bridge: service authentication rejected"
            );
            return error(
                StatusCode::UNAUTHORIZED,
                "INTERNAL_AUTHENTICATION_FAILED",
                None,
            );
        }
    };
    let rid = ctx.request_id().as_str().to_string();
    if ctx
        .require_permission(InternalPermission::UserSessionVerify)
        .is_err()
    {
        return error(
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            Some(&rid),
        );
    }
    let bytes = match axum::body::to_bytes(request.into_body(), 8 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                Some(&rid),
            )
        }
    };
    let Ok(body) = serde_json::from_slice::<VerifyBody>(&bytes) else {
        return error(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", Some(&rid));
    };
    let token = body.session_token.trim();
    let invalid = || (StatusCode::OK, Json(json!({ "outcome": "REJECTED" }))).into_response();
    if token.is_empty() || token.len() > 4096 {
        return invalid();
    }
    let Ok(claims) = s.app.jwt_service.verify_token(token) else {
        return invalid();
    };
    if claims.role == "guest" {
        return invalid();
    }
    // Revocation must be CHECKED: an unreachable store fails closed here.
    match s.app.token_blacklist.is_blacklisted(token).await {
        Ok(false) => {}
        Ok(true) => return invalid(),
        Err(e) => {
            tracing::warn!("session bridge: revocation store unavailable: {e}");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                Some(&rid),
            );
        }
    }
    let Ok(user_id) = Uuid::parse_str(&claims.sub) else {
        return invalid();
    };
    let user = match users::Entity::find_by_id(user_id).one(&s.app.db).await {
        Ok(Some(u)) => u,
        Ok(None) => return invalid(),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                Some(&rid),
            )
        }
    };
    if user.status != UserStatus::Active || user.role == UserRole::Guest {
        return invalid();
    }
    let email_verified = user.auth_type == AuthType::Google
        || (user.status == UserStatus::Active && s.app.require_email_verification);
    tracing::info!(caller = %ctx.authenticated_service(), "session bridge: session verified");
    let mut r = Json(json!({
        "outcome": "VERIFIED",
        "userId": user.id,
        "displayName": user.username,
        "verifiedEmail": email_verified.then(|| user.email.to_ascii_lowercase()),
    }))
    .into_response();
    r.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    r
}
