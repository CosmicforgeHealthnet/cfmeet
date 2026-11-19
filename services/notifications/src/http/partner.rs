//! N-P1, the partner push provider vault (ADR 0063; contract
//! `cfmeet.notifications.internal-api` 1.3.0), Developer Console only
//! (`notifications-push-provider.manage`):
//!
//! * `PUT    /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}/{provider}`
//! * `GET    /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}`
//! * `DELETE /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}/{provider}`
//! * `POST   /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}/{provider}/verify`
//!
//! While `NOTIFICATIONS_PARTNER_PUSH_ENABLED` is off every route answers `404` exactly as
//! an unknown route does. Nothing here ever returns, logs or echoes the secret.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cf_service_auth::InternalPermission;
use uuid::Uuid;

use super::{json_ok, middleware::Caller, parse};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    partner::{self, PartnerPush, PutProviderRequest, Scope, VaultError},
    state::AppState,
};

fn enabled(state: &AppState) -> ApiResult<&PartnerPush> {
    state.partner.as_deref().ok_or_else(ApiError::not_found)
}

fn uuid(s: &str, what: &'static str) -> ApiResult<Uuid> {
    Uuid::parse_str(s)
        .ok()
        .filter(|u| u.hyphenated().to_string() == s.to_ascii_lowercase())
        .ok_or_else(|| ApiError::validation(what))
}

fn scope_of(state: &AppState, t: &str, a: &str, e: &str) -> ApiResult<Scope> {
    let scope = Scope {
        tenant_id: uuid(t, "tenantId is a UUID")?,
        application_id: uuid(a, "applicationId is a UUID")?,
        environment_id: uuid(e, "environmentId is a UUID")?,
    };
    if scope.environment_id.is_nil()
        || state.first_party_scope
            == Some((scope.tenant_id, scope.application_id, scope.environment_id))
    {
        return Err(ApiError::validation("not a partner environment"));
    }
    Ok(scope)
}

fn provider_of(p: &str) -> ApiResult<&'static str> {
    partner::PROVIDERS
        .iter()
        .copied()
        .find(|x| *x == p)
        .ok_or_else(|| ApiError::validation("provider is APNS, APNS_VOIP or FCM"))
}

fn vault_error(e: VaultError) -> ApiError {
    match e {
        VaultError::NotFound => ApiError::not_found(),
        VaultError::Invalid(m) => ApiError::validation(m),
        VaultError::ScopeConflict => ApiError::with_message(
            ErrorCode::VersionConflict,
            "this environment's credential belongs to another tenant or application",
        ),
        VaultError::Unavailable | VaultError::Db => ApiError::unavailable(5),
    }
}

/// `PUT …/{provider}`: store (or replace) a sealed credential.
pub async fn put_provider(
    State(state): State<AppState>,
    caller: Caller,
    Path((t, a, e, p)): Path<(String, String, String, String)>,
    body: Bytes,
) -> ApiResult<Response> {
    let partner = enabled(&state)?;
    caller.require(InternalPermission::NotificationsPushProviderManage)?;
    let scope = scope_of(&state, &t, &a, &e)?;
    let provider = provider_of(&p)?;
    let req: PutProviderRequest = parse(&body)?;
    let out = partner::put_provider(
        &state.db,
        &state.keys,
        state.opening.as_deref(),
        partner,
        &state.metrics,
        scope,
        provider,
        req,
    )
    .await
    .map_err(vault_error)?;
    Ok(json_ok(out))
}

/// `GET …/{environmentId}`: metadata, fingerprints and statuses.
pub async fn list_providers(
    State(state): State<AppState>,
    caller: Caller,
    Path((t, a, e)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    enabled(&state)?;
    caller.require(InternalPermission::NotificationsPushProviderManage)?;
    let scope = scope_of(&state, &t, &a, &e)?;
    Ok(json_ok(partner::list_providers(&state.db, scope).await?))
}

/// `DELETE …/{provider}` (idempotent `204`).
pub async fn delete_provider(
    State(state): State<AppState>,
    caller: Caller,
    Path((t, a, e, p)): Path<(String, String, String, String)>,
) -> ApiResult<Response> {
    let partner = enabled(&state)?;
    caller.require(InternalPermission::NotificationsPushProviderManage)?;
    let scope = scope_of(&state, &t, &a, &e)?;
    let provider = provider_of(&p)?;
    partner::delete_provider(&state.db, partner, &state.metrics, scope, provider).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST …/{provider}/verify`: `{status, lastError?}`.
pub async fn verify_provider(
    State(state): State<AppState>,
    caller: Caller,
    Path((t, a, e, p)): Path<(String, String, String, String)>,
) -> ApiResult<Response> {
    let partner = enabled(&state)?;
    caller.require(InternalPermission::NotificationsPushProviderManage)?;
    let scope = scope_of(&state, &t, &a, &e)?;
    let provider = provider_of(&p)?;
    let out = partner::verify_provider(&state.db, &state.keys, partner, scope, provider)
        .await
        .map_err(vault_error)?;
    Ok(json_ok(out))
}
