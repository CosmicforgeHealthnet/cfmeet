//! Part V H — Unified Messaging endpoints of the CF Meet backend.
//!
//! * `/api/v2/messaging/**` — the §104 identity bridge for SIGNED-IN CF Meet users:
//!   device registration (client-held MLS keys, proof of possession), KeyPackages,
//!   device revocation, short-lived device-bound Messaging client tokens and "log out of
//!   every messaging session". Meeting verifies the login; Messaging's answer is relayed
//!   unchanged. No service credential is ever returned.
//! * `/api/v2/admin/chat-migration/**` — the §100 migration controls (platform admins).

use axum::{extract::State, http::StatusCode, Json};
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
