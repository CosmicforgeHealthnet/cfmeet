//! Part V H — Unified Messaging endpoints of the CF Meet backend.
//!
//! * `/api/v2/messaging/**` — the §104 identity bridge for SIGNED-IN CF Meet users:
//!   device registration (client-held MLS keys, proof of possession), KeyPackages,
//!   device revocation, short-lived device-bound Messaging client tokens and "log out of
//!   every messaging session". Meeting verifies the login; Messaging's answer is relayed
//!   unchanged. No service credential is ever returned.
//! * `/api/v2/messaging/me`, `/directory`, `/directory/resolve`, `/principals/names` and
//!   `/relay/v1/**` — the CF Meet client's Messaging API: the caller's principal and
//!   scope ids, organization colleagues, and the allowlisted end-user operations relayed
//!   to Messaging with the caller's principal SIGNED (`services::unified_messaging::relay`).
//! * `/api/v2/admin/chat-migration/**` — the §100 migration controls (platform admins).

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::{V2Json, V2Path};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        compat::CompatService,
        unified_messaging::{
            identity::BridgeOperation,
            migration::{MigrationStatusView, ParityRunView, TransitionRequest},
            UnifiedMessaging,
        },
    },
    state::AppState,
};

fn integration(state: &AppState) -> ApiResult<&std::sync::Arc<UnifiedMessaging>> {
    state.meeting.core.unified.as_ref().ok_or_else(|| {
        ApiError::domain(
            ErrorCode::FeatureDisabled,
            "Unified Messaging is not enabled",
        )
    })
}

/// A verified CF Meet account whose token revocation was actually checked.
fn verified_user(actor: &ActorContext) -> ApiResult<Uuid> {
    let r = actor.require_registered()?;
    if !r.revocation_checked {
        // The login authority could not confirm the session is not revoked: fail closed.
        return Err(ApiError::domain(
            ErrorCode::ServiceUnavailable,
            "Your session cannot be verified right now; retry shortly",
        ));
    }
    Ok(r.user_id)
}

async fn relay(
    state: &AppState,
    actor: &ActorContext,
    op: BridgeOperation,
    body: Option<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(state)?;
    let user = verified_user(actor)?;
    let (status, value) = um.bridge(user, op, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

/// Start a device registration (the challenge the device signs with its own key).
#[utoipa::path(
    post, path = "/api/v2/messaging/devices/registration-challenges", tag = "Messaging (v2)", operation_id = "v2_messaging_registration_challenge",
    request_body = Object,
    responses((status = 201, description = "Challenge (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn registration_challenge(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(
        &state,
        &actor,
        BridgeOperation::RegistrationChallenge,
        Some(body),
    )
    .await
}

/// Register a device with its proof-of-possession KeyPackage.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices", tag = "Messaging (v2)", operation_id = "v2_messaging_register_device",
    request_body = Object,
    responses((status = 201, description = "Device (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn register_device(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(&state, &actor, BridgeOperation::RegisterDevice, Some(body)).await
}

/// Publish one-time KeyPackages of the caller's device.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices/{deviceId}/key-packages", tag = "Messaging (v2)", operation_id = "v2_messaging_publish_key_packages",
    params(("deviceId" = Uuid, Path, description = "Device id")),
    request_body = Object,
    responses((status = 201, description = "Published (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn publish_key_packages(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(
        &state,
        &actor,
        BridgeOperation::PublishKeyPackages(device),
        Some(body),
    )
    .await
}

/// Revoke one of the caller's devices.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices/{deviceId}/revoke", tag = "Messaging (v2)", operation_id = "v2_messaging_revoke_device",
    params(("deviceId" = Uuid, Path, description = "Device id")),
    responses((status = 200, description = "Revoked (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn revoke_device(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(&state, &actor, BridgeOperation::RevokeDevice(device), None).await
}

/// Issue a short-lived, device-bound Messaging client token (for the realtime gateway).
#[utoipa::path(
    post, path = "/api/v2/messaging/client-tokens", tag = "Messaging (v2)", operation_id = "v2_messaging_client_token",
    request_body = Object,
    responses((status = 201, description = "Client token (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn client_token(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(
        &state,
        &actor,
        BridgeOperation::IssueClientToken,
        Some(body),
    )
    .await
}

/// End every Messaging client session of the caller (all devices).
#[utoipa::path(
    post, path = "/api/v2/messaging/client-sessions/revoke-all", tag = "Messaging (v2)", operation_id = "v2_messaging_revoke_all_sessions",
    responses((status = 200, description = "Revoked (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay(&state, &actor, BridgeOperation::RevokeAllSessions, None).await
}

// ------------------------------------------------------------- client API

/// Largest relayed request body (sealed history uploads are the largest).
pub const RELAY_MAX_BODY_BYTES: usize = 1024 * 1024;

/// The caller's Messaging principal and the scope ids its devices bind into every
/// encrypted message.
#[utoipa::path(
    get, path = "/api/v2/messaging/me", tag = "Messaging (v2)", operation_id = "v2_messaging_me",
    responses((status = 200, description = "principalId, tenantId, applicationId, environmentId", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn me(State(state): State<AppState>, actor: ActorContext) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&actor)?;
    Ok(Json(um.me(user).await?))
}

#[derive(Debug, Deserialize)]
pub struct DirectoryQuery {
    pub q: Option<String>,
}

/// Colleagues (active members of the caller's organizations) the caller may message.
#[utoipa::path(
    get, path = "/api/v2/messaging/directory", tag = "Messaging (v2)", operation_id = "v2_messaging_directory",
    params(("q" = Option<String>, Query, description = "Name filter")),
    responses((status = 200, description = "people: userId, displayName, principalId (null until first use)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn directory(
    State(state): State<AppState>,
    actor: ActorContext,
    super::V2Query(q): super::V2Query<DirectoryQuery>,
) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&actor)?;
    Ok(Json(
        um.directory(user, &actor.verified_organizations, q.q.as_deref())
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveRequest {
    pub user_ids: Vec<Uuid>,
}

/// The Messaging principals of directory colleagues (so they can be added).
#[utoipa::path(
    post, path = "/api/v2/messaging/directory/resolve", tag = "Messaging (v2)", operation_id = "v2_messaging_directory_resolve",
    request_body = Object,
    responses((status = 200, description = "people: userId, principalId", body = Object), (status = 404, description = "NOT_FOUND (not a colleague)", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn resolve(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<ResolveRequest>,
) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&actor)?;
    Ok(Json(
        um.resolve_colleagues(user, &actor.verified_organizations, &body.user_ids)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamesRequest {
    pub principal_ids: Vec<Uuid>,
}

/// Display names of the principals the caller can see (self and colleagues).
#[utoipa::path(
    post, path = "/api/v2/messaging/principals/names", tag = "Messaging (v2)", operation_id = "v2_messaging_principal_names",
    request_body = Object,
    responses((status = 200, description = "principals: principalId, displayName", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn principal_names(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<NamesRequest>,
) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&actor)?;
    Ok(Json(
        um.name_principals(user, &actor.verified_organizations, &body.principal_ids)
            .await?,
    ))
}

fn parse_query(raw: Option<String>) -> ApiResult<Vec<(String, String)>> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(vec![]);
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.len() > 8 || pairs.iter().any(|(k, v)| k.len() > 32 || v.len() > 128) {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "Invalid query",
        ));
    }
    Ok(pairs)
}

/// The allowlisted end-user Messaging operations (see `relay::ROUTES`), relayed with the
/// caller's own principal signed in. Messaging's answer is returned unchanged.
async fn relay_operation(
    state: AppState,
    actor: ActorContext,
    method: Method,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(&state)?;
    let user = verified_user(&actor)?;
    let body =
        if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice::<Value>(&body).map_err(|_| {
                ApiError::domain(ErrorCode::ValidationError, "The body must be JSON")
            })?)
        };
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (status, value) = um
        .relay(
            user,
            crate::services::unified_messaging::relay::RelayRequest {
                method,
                path: &path,
                query: parse_query(query)?,
                idempotency_key,
                body,
            },
        )
        .await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

/// Relayed read (allowlisted path under `v1/`, e.g. `v1/conversations/{id}/messages`).
#[utoipa::path(
    get, path = "/api/v2/messaging/relay/{path}", tag = "Messaging (v2)", operation_id = "v2_messaging_relay_get",
    params(("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn relay_get(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_operation(
        state,
        actor,
        Method::GET,
        path,
        query,
        headers,
        Bytes::new(),
    )
    .await
}

/// Relayed create / action (allowlisted path under `v1/`).
#[utoipa::path(
    post, path = "/api/v2/messaging/relay/{path}", tag = "Messaging (v2)", operation_id = "v2_messaging_relay_post",
    params(("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn relay_post(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_operation(state, actor, Method::POST, path, query, headers, body).await
}

/// Relayed replace (allowlisted path under `v1/`, e.g. a read cursor or sealed history).
#[utoipa::path(
    put, path = "/api/v2/messaging/relay/{path}", tag = "Messaging (v2)", operation_id = "v2_messaging_relay_put",
    params(("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn relay_put(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_operation(state, actor, Method::PUT, path, query, headers, body).await
}

/// Relayed removal (allowlisted path under `v1/`, e.g. leaving a conversation).
#[utoipa::path(
    delete, path = "/api/v2/messaging/relay/{path}", tag = "Messaging (v2)", operation_id = "v2_messaging_relay_delete",
    params(("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn relay_delete(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_operation(
        state,
        actor,
        Method::DELETE,
        path,
        query,
        headers,
        Bytes::new(),
    )
    .await
}

// ---------------------------------------------------------------- migration

/// Chat migration state, gates and evidence.
#[utoipa::path(
    get, path = "/api/v2/admin/chat-migration", tag = "Messaging (v2)", operation_id = "v2_chat_migration_status",
    responses((status = 200, description = "Status", body = MigrationStatusView), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn migration_status(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<MigrationStatusView>> {
    CompatService::require_platform_admin(&actor)?;
    Ok(Json(integration(&state)?.migration_status().await?))
}

/// Request a migration transition (gated, version-checked, audited).
#[utoipa::path(
    post, path = "/api/v2/admin/chat-migration/transitions", tag = "Messaging (v2)", operation_id = "v2_chat_migration_transition",
    request_body = TransitionRequest,
    responses((status = 200, description = "New status", body = MigrationStatusView), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope), (status = 409, description = "CHAT_MIGRATION_REFUSED / MEETING_VERSION_CONFLICT", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn migration_transition(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<TransitionRequest>,
) -> ApiResult<Json<MigrationStatusView>> {
    CompatService::require_platform_admin(&actor)?;
    let user = actor.require_registered()?.user_id;
    Ok(Json(
        integration(&state)?
            .transition(&format!("user:{user}"), body)
            .await?,
    ))
}

/// Run a shadow-validation parity check now.
#[utoipa::path(
    post, path = "/api/v2/admin/chat-migration/parity-runs", tag = "Messaging (v2)", operation_id = "v2_chat_migration_parity",
    responses((status = 200, description = "Parity run", body = ParityRunView), (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn migration_parity(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<ParityRunView>> {
    CompatService::require_platform_admin(&actor)?;
    Ok(Json(integration(&state)?.run_parity().await?))
}
