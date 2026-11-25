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
            installations::{InstallationBinding, InstallationChallenge, InstallationProof},
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

/// A verified CF Meet account whose token revocation was actually checked (and, where the
/// deployment enforces it, whose email address is verified: ACC-016).
fn verified_user(state: &AppState, actor: &ActorContext) -> ApiResult<Uuid> {
    let r = actor.require_registered()?;
    if !r.revocation_checked {
        // The login authority could not confirm the session is not revoked: fail closed.
        return Err(ApiError::domain(
            ErrorCode::ServiceUnavailable,
            "Your session cannot be verified right now; retry shortly",
        ));
    }
    state
        .auth_policy
        .require_verified_email_for_communications(
            state.require_email_verification,
            r.verified_email.as_deref(),
        )?;
    Ok(r.user_id)
}

async fn relay(
    state: &AppState,
    actor: &ActorContext,
    op: BridgeOperation,
    body: Option<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(state)?;
    let user = verified_user(state, actor)?;
    let (status, value) = um.bridge(user, op, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

/// Multi-device policy: a personal E2EE (MLS) device can be enrolled only from a sign-in
/// that belongs to a trusted account device (bootstrap, approval by a trusted device or
/// email step-up), unless the deployment switched the gate off. A sign-in code alone
/// never yields an E2EE identity that would receive the account's future messages.
async fn require_trusted_device_for_e2ee(state: &AppState, actor: &ActorContext) -> ApiResult<()> {
    if !state.auth_policy.devices.e2ee_requires_trusted_device {
        return Ok(());
    }
    let session = actor.registered().and_then(|r| r.account_session_id);
    let trusted = match session {
        Some(s) => {
            crate::services::account_devices::session_is_trusted_device(&state.db, s).await?
        }
        None => false,
    };
    if !trusted {
        return Err(ApiError::domain(
            ErrorCode::DeviceNotTrusted,
            "Link this device to your account (POST /api/v2/me/devices/current, then a device link) before enrolling end-to-end encryption",
        ));
    }
    Ok(())
}

/// [`relay`] for personal E2EE device enrollment: the same integration and account checks
/// first, then the trusted-device gate.
async fn relay_enrollment(
    state: &AppState,
    actor: &ActorContext,
    op: BridgeOperation,
    body: Option<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(state)?;
    let user = verified_user(state, actor)?;
    require_trusted_device_for_e2ee(state, actor).await?;
    let (status, value) = um.bridge(user, op, body).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

/// Start a device registration (the challenge the device signs with its own key).
///
/// First step of registering a Messaging device for the signed-in user; complete it with
/// `POST /api/v2/messaging/devices`. The JSON body is forwarded as-is to the Messaging service,
/// acting as the caller's own Messaging principal (created on first use), and Messaging's status
/// and body are returned unchanged. Requires a bearer user token whose revocation status could be
/// checked (the call fails closed otherwise) and the Messaging integration enabled.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices/registration-challenges", tag = "Messaging (v2)", operation_id = "v2_messaging_registration_challenge",
    request_body = Object,
    responses((status = 201, description = "Challenge (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "DEVICE_NOT_TRUSTED (link this device to the account first)", body = ErrorEnvelope), (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn registration_challenge(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_enrollment(
        &state,
        &actor,
        BridgeOperation::RegistrationChallenge,
        Some(body),
    )
    .await
}

/// Register a device with its proof-of-possession KeyPackage.
///
/// Completes registration of a Messaging device for the signed-in user. The JSON body, as defined
/// by the Messaging contract, is forwarded as-is to the Messaging service acting as the caller's
/// own principal, and Messaging's status and body are returned unchanged; no service credential is
/// ever returned. Requires a bearer user token whose revocation status could be checked and the
/// Messaging integration enabled.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices", tag = "Messaging (v2)", operation_id = "v2_messaging_register_device",
    request_body = Object,
    responses((status = 201, description = "Device (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 403, description = "DEVICE_NOT_TRUSTED (link this device to the account first)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn register_device(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_enrollment(&state, &actor, BridgeOperation::RegisterDevice, Some(body)).await
}

/// Challenge an existing Messaging installation
///
/// Issues a short-lived, one-use possession challenge bound to the current trusted
/// Account installation, sign-in and active Messaging identity. The client answers
/// using its existing MLS signature key; no history key is sent to the server.
#[utoipa::path(
    post,path="/api/v2/messaging/devices/{deviceId}/installation-challenges",tag="Messaging (v2)",operation_id="v2_messaging_installation_challenge",
    params(("deviceId"=Uuid,Path,description="Existing active Messaging identity")),
    request_body=Object,
    responses((status=201,description="Fresh proof bound to this trusted Account installation and sign-in",body=InstallationChallenge),(status=401,description="DEVICE_PROOF_INVALID / AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="DEVICE_NOT_TRUSTED",body=ErrorEnvelope),(status=503,description="Capability unavailable",body=ErrorEnvelope)),
    security(("bearer_auth"=[]))
)]
pub async fn installation_challenge(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
    V2Json(body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<InstallationChallenge>)> {
    let user = verified_user(&state, &actor)?;
    require_trusted_device_for_e2ee(&state, &actor).await?;
    if body != serde_json::json!({}) {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "An empty object is required",
        ));
    }
    let session = actor
        .registered()
        .and_then(|r| r.account_session_id)
        .ok_or_else(|| {
            ApiError::domain(
                ErrorCode::DeviceNotTrusted,
                "Declare this installation first",
            )
        })?;
    Ok((
        StatusCode::CREATED,
        Json(
            integration(&state)?
                .installation_challenge(user, session, device)
                .await?,
        ),
    ))
}

/// Bind a protected Messaging installation
///
/// Verifies the registration-bound KeyPackage proof and binds the existing active
/// Messaging identity to this trusted Account installation and sign-in. Terminal
/// revocations and immutable bindings remain enforced. Ordinary sign-out ends
/// session authority without revoking a proved persistent identity.
#[utoipa::path(
    post,path="/api/v2/messaging/devices/{deviceId}/installation",tag="Messaging (v2)",operation_id="v2_messaging_bind_installation",
    params(("deviceId"=Uuid,Path,description="Existing active Messaging identity")),
    request_body=InstallationProof,
    responses((status=200,description="Identity is bound to the proved protected installation",body=InstallationBinding),(status=401,description="DEVICE_PROOF_INVALID / AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="DEVICE_NOT_TRUSTED",body=ErrorEnvelope),(status=503,description="Capability unavailable",body=ErrorEnvelope)),
    security(("bearer_auth"=[]))
)]
pub async fn bind_installation(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
    V2Json(proof): V2Json<InstallationProof>,
) -> ApiResult<Json<InstallationBinding>> {
    let user = verified_user(&state, &actor)?;
    require_trusted_device_for_e2ee(&state, &actor).await?;
    let session = actor
        .registered()
        .and_then(|r| r.account_session_id)
        .ok_or_else(|| {
            ApiError::domain(
                ErrorCode::DeviceNotTrusted,
                "Declare this installation first",
            )
        })?;
    Ok(Json(
        integration(&state)?
            .bind_installation(&state, user, session, device, proof)
            .await?,
    ))
}

/// Read my Messaging installation binding
///
/// Reports whether the active Messaging identity belongs to the current trusted
/// Account installation. It does not unlock local history or grant a new sign-in
/// Messaging authority. A fresh possession challenge is required for its client token.
#[utoipa::path(
    get,path="/api/v2/messaging/devices/{deviceId}/installation",tag="Messaging (v2)",operation_id="v2_messaging_installation_status",
    params(("deviceId"=Uuid,Path,description="Existing Messaging identity")),
    responses((status=200,description="Binding matches this trusted Account installation; fresh MLS proof is still required for token issuance",body=InstallationBinding),(status=401,description="DEVICE_PROOF_INVALID / AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="DEVICE_NOT_TRUSTED",body=ErrorEnvelope),(status=503,description="Capability unavailable",body=ErrorEnvelope)),
    security(("bearer_auth"=[]))
)]
pub async fn installation_status(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let user = verified_user(&state, &actor)?;
    let session = actor
        .registered()
        .and_then(|r| r.account_session_id)
        .ok_or_else(|| {
            ApiError::domain(
                ErrorCode::DeviceNotTrusted,
                "Declare this installation first",
            )
        })?;
    Ok(Json(
        integration(&state)?
            .installation_status(user, session, device)
            .await?,
    ))
}

/// Publish one-time KeyPackages of the caller's device.
///
/// Uploads one-time KeyPackages for the given device of the signed-in user, so the device can be
/// added to conversations. The JSON body is forwarded as-is to the Messaging service acting as the
/// caller's own principal, and Messaging's status and body are returned unchanged. Requires a
/// bearer user token whose revocation status could be checked and the Messaging integration
/// enabled.
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
///
/// Optional `reason`: USER_REQUESTED (default), LOST_OR_STOLEN, COMPROMISED or REPLACED.
#[utoipa::path(
    post, path = "/api/v2/messaging/devices/{deviceId}/revoke", tag = "Messaging (v2)", operation_id = "v2_messaging_revoke_device",
    params(("deviceId" = Uuid, Path, description = "Device id")),
    request_body = Object,
    responses((status = 200, description = "Revoked (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn revoke_device(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(device): V2Path<Uuid>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let body =
        if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice::<Value>(&body).map_err(|_| {
                ApiError::domain(ErrorCode::ValidationError, "The body must be JSON")
            })?)
        };
    relay(&state, &actor, BridgeOperation::RevokeDevice(device), body).await
}

/// Issue a short-lived, device-bound Messaging client token (for the realtime gateway).
///
/// Requests a short-lived Messaging client token bound to one of the signed-in user's devices, for
/// connecting to the Messaging realtime gateway. The JSON body is forwarded as-is to the Messaging
/// service acting as the caller's own principal, and Messaging's status and body are returned
/// unchanged; no service credential is ever returned. Requires a bearer user token whose revocation
/// status could be checked and the Messaging integration enabled.
#[utoipa::path(
    post, path = "/api/v2/messaging/client-tokens", tag = "Messaging (v2)", operation_id = "v2_messaging_client_token",
    request_body = Object,
    responses((status = 201, description = "Client token (Messaging contract)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn client_token(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(mut body): V2Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    // Gap audit A11: the token is bound to the caller's own sign-in, so signing out of
    // this browser ends it while the account's other sign-ins keep theirs. The binding
    // is the relay's; whatever a client sends under that name is discarded.
    let session = actor.registered().and_then(|r| r.account_session_id);
    if let Some(device) = body
        .get("deviceId")
        .and_then(Value::as_str)
        .and_then(|v| Uuid::parse_str(v).ok())
    {
        um.require_installation_session(&state.db, user, session.unwrap_or(Uuid::nil()), device)
            .await?;
    }
    if let Some(map) = body.as_object_mut() {
        map.remove("accountSessionRef");
    }
    let bound = match (session, body.as_object_mut()) {
        (Some(session), Some(map)) => {
            map.insert(
                "accountSessionRef".into(),
                Value::String(um.account_session_ref(session)),
            );
            true
        }
        _ => false,
    };
    let (mut status, mut value) = um
        .bridge(user, BridgeOperation::IssueClientToken, Some(body.clone()))
        .await?;
    if bound && status == 400 {
        // A Messaging image that predates the binding refuses the unknown field: issue
        // the token unbound (it still ends with its device, revoke-all, or expiry).
        if let Some(map) = body.as_object_mut() {
            map.remove("accountSessionRef");
        }
        (status, value) = um
            .bridge(user, BridgeOperation::IssueClientToken, Some(body))
            .await?;
    }
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTokenRequest {
    /// Renew the token of this (still active) session device instead of creating one.
    #[serde(default)]
    pub session_device_id: Option<Uuid>,
    /// A shorter lifetime than Messaging's session-device maximum.
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

/// The account session of the presented bearer token and that token's expiry.
pub(crate) fn account_session(state: &AppState, headers: &HeaderMap) -> ApiResult<(Uuid, i64)> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .ok_or_else(|| {
            ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "A bearer token is required",
            )
        })?;
    let claims = state.jwt_service.verify_token(token).map_err(|_| {
        ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Invalid or expired token",
        )
    })?;
    let session = claims.session_id().ok_or_else(|| {
        ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Live updates need a session-bound sign-in; sign in again",
        )
    })?;
    Ok((session, claims.exp))
}

/// Issue a short-lived MANAGED-realtime session-device token (no E2EE device needed).
///
/// Blueprint Section 13 (ADR 0049 amendment A1): returns a Messaging client token bound to
/// a *session device* of the signed-in user. A session device is not an MLS device: it can
/// only open the realtime gateway and subscribe to MANAGED conversations; it cannot publish
/// KeyPackages, join MLS groups, receive PRIVATE_E2EE ciphertext, use device-direct REST
/// operations or count as an end-to-end encrypted device. It is bound to the caller's
/// account session: the token never outlives the presented access token, every renewal
/// re-verifies the session, and signing out (this session, everywhere, or a security
/// revocation) ends it. Send `sessionDeviceId` to renew the same session device (an open
/// connection then continues with `auth.refresh`). Answers FEATURE_DISABLED while
/// Messaging has session devices switched off.
#[utoipa::path(
    post, path = "/api/v2/messaging/session-tokens", tag = "Messaging (v2)", operation_id = "v2_messaging_session_token",
    request_body = Object,
    responses((status = 201, description = "Session-device client token (Messaging contract)", body = Object), (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope), (status = 401, description = "AUTHENTICATION_REQUIRED (no session-bound sign-in)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn session_token(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    let request: SessionTokenRequest = if body.is_empty() {
        SessionTokenRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| {
            ApiError::domain(
                ErrorCode::ValidationError,
                "The body may name only sessionDeviceId and ttlSeconds",
            )
        })?
    };
    let (session, expires) = account_session(&state, &headers)?;
    let not_after = chrono::DateTime::from_timestamp(expires, 0).ok_or_else(|| {
        ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Invalid or expired token",
        )
    })?;
    let (status, value) = um
        .issue_session_token(
            user,
            session,
            not_after,
            request.session_device_id,
            request.ttl_seconds,
        )
        .await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    ))
}

/// End every Messaging client session of the caller (all devices).
///
/// Asks the Messaging service to revoke every client session of the signed-in user's principal, on
/// all devices. No request body is used. Messaging's status and body are returned unchanged.
/// Requires a bearer user token whose revocation status could be checked and the Messaging
/// integration enabled.
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

/// The caller's Messaging principal and device binding scope.
///
/// Returns the caller's Messaging principal and the scope ids its devices bind into every
/// encrypted message.
#[utoipa::path(
    get, path = "/api/v2/messaging/me", tag = "Messaging (v2)", operation_id = "v2_messaging_me",
    responses((status = 200, description = "principalId, tenantId, applicationId, environmentId, displayName (the caller's own account display name)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn me(State(state): State<AppState>, actor: ActorContext) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    let mut me = um.me(user).await?;
    if migration::messaging_installations_ready(&state.db).await? {
        me["deviceLifecycle"] =
            serde_json::json!({"protocolVersion":1,"persistentInstallationBinding":true});
    }
    // The caller's own name, so a calling client can name itself on the calls it starts
    // (initiatorDisplayName), as the callee's incoming-call screen shows it.
    if let Some(registered) = actor.registered() {
        me["displayName"] = Value::String(registered.display_name.clone());
    }
    Ok(Json(me))
}

/// The caller's colleague scope (Blueprint Section 4: read from Organizations once it is
/// authoritative; an unreadable organization contributes nobody).
async fn colleague_scope(
    state: &AppState,
    actor: &ActorContext,
) -> crate::services::unified_messaging::relay::ColleagueScope {
    colleague_scope_for(state, actor, false).await
}

/// Gap audit G6: `search` applies each organization's `memberDirectory.searchable`.
async fn colleague_scope_for(
    state: &AppState,
    actor: &ActorContext,
    search: bool,
) -> crate::services::unified_messaging::relay::ColleagueScope {
    use crate::services::unified_messaging::relay::ColleagueScope;
    match (
        state
            .meeting
            .core
            .organization_authority
            .as_ref()
            .filter(|a| a.is_authoritative()),
        actor.registered(),
    ) {
        (Some(authority), Some(user)) if search => ColleagueScope::Users(
            crate::services::organizations_authority::searchable_colleagues(
                authority,
                user,
                &actor.verified_organizations,
            )
            .await,
        ),
        (Some(authority), Some(user)) => ColleagueScope::Users(
            crate::services::organizations_authority::authoritative_colleagues(
                authority,
                user,
                &actor.verified_organizations,
            )
            .await,
        ),
        _ => ColleagueScope::Organizations(actor.verified_organizations.clone()),
    }
}

#[derive(Debug, Deserialize)]
pub struct DirectoryQuery {
    pub q: Option<String>,
}

/// Colleagues (active members of the caller's organizations) the caller may message.
///
/// Returns up to 200 people who are active members of the caller's verified organizations (the
/// caller excluded), sorted by name, optionally filtered by `q` (case-insensitive substring match
/// on the name). `principalId` is null for people who have not used Messaging yet; use
/// `POST /api/v2/messaging/directory/resolve` to obtain it. Returns an empty list when the caller
/// has no verified organization. Requires a bearer user token whose revocation status could be
/// checked and the Messaging integration enabled.
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
    let user = verified_user(&state, &actor)?;
    Ok(Json(
        um.directory(
            user,
            &colleague_scope_for(&state, &actor, true).await,
            q.q.as_deref(),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveRequest {
    pub user_ids: Vec<Uuid>,
}

/// The Messaging principals of directory colleagues (so they can be added).
///
/// Returns the Messaging principal id of each user in `userIds` (1–100), creating principals on
/// first use, so the client can add those people to a conversation. Every user must be an active
/// member of one of the caller's verified organizations and not the caller; otherwise the request
/// fails with NOT_FOUND, exactly as for an unknown user. Requires a bearer user token whose
/// revocation status could be checked and the Messaging integration enabled.
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
    let user = verified_user(&state, &actor)?;
    Ok(Json(
        um.resolve_colleagues(user, &colleague_scope(&state, &actor).await, &body.user_ids)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamesRequest {
    pub principal_ids: Vec<Uuid>,
}

/// Display names of the principals the caller can see (self, colleagues and connected contacts).
///
/// Returns display names for up to 256 `principalIds`. Named are the caller's own principal, those of
/// active members of the caller's verified organizations, and the caller's connected personal
/// contacts (Communications Hub; the name the contact list shows, plus the caller's private `alias`
/// when set). A pending, revoked or blocked contact and any other id are left out of the answer
/// without an error. If the Hub cannot be reached, contacts are simply not named. Requires a bearer
/// user token whose revocation status could be checked and the Messaging integration enabled.
#[utoipa::path(
    post, path = "/api/v2/messaging/principals/names", tag = "Messaging (v2)", operation_id = "v2_messaging_principal_names",
    request_body = Object,
    responses((status = 200, description = "principals: principalId, displayName, alias (connected contacts only, when the caller set one)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn principal_names(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(body): V2Json<NamesRequest>,
) -> ApiResult<Json<Value>> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    let mut answer = um
        .name_principals(
            user,
            &colleague_scope(&state, &actor).await,
            &body.principal_ids,
        )
        .await?;
    let named: std::collections::HashSet<Uuid> = answer["principals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            p["principalId"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .collect();
    let mut rest: Vec<Uuid> = body
        .principal_ids
        .iter()
        .copied()
        .filter(|p| !named.contains(p))
        .collect();
    rest.sort();
    rest.dedup();
    if let (false, Some(registered)) = (rest.is_empty(), actor.registered()) {
        // Personal contacts are display-only: a Hub outage leaves them unnamed, never fails.
        match crate::services::communications::contact_names(&state, registered, &headers, &rest)
            .await
        {
            Ok(contacts) => {
                if let Some(list) = answer["principals"].as_array_mut() {
                    list.extend(contacts.into_iter().map(|c| {
                        let mut v = serde_json::json!({ "principalId": c.principal_id, "displayName": c.display_name });
                        if let Some(alias) = c.alias {
                            v["alias"] = Value::String(alias);
                        }
                        v
                    }));
                }
            }
            Err(error) => tracing::warn!(error = %error, "contact names unavailable"),
        }
    }
    Ok(Json(answer))
}

/// Messaging's relayed answer, never cached (it may carry a one-time download grant).
fn no_store(status: u16, value: Value) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut r = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(value),
    )
        .into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    r
}

/// Data exports waiting for me (Messaging)
///
/// ADR 0068 (owner-delivered exports): the Messaging exports CF Meet support requested for
/// the signed-in account, newest first (at most 20), as references (ids, state, size, SHA-256,
/// times; never the export or a link). Relayed to Messaging's delegated surface with the
/// caller's own principal signed in; Messaging answers only the recipient's own exports.
#[utoipa::path(
    get, path = "/api/v2/messaging/export-deliveries", tag = "Messaging (v2)", operation_id = "v2_messaging_export_deliveries",
    responses(
        (status = 200, description = "`{items: [ExportDelivery]}` (Messaging's answer)", body = Object),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "FEATURE_DISABLED (Unified Messaging is not enabled)", body = ErrorEnvelope),
        (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn export_deliveries(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<axum::response::Response> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    let (status, value) = um.export_deliveries(user).await?;
    Ok(no_store(status, value))
}

/// Claim a Messaging data export once
///
/// ADR 0068 (owner-delivered exports): the signed-in recipient's single claim of a READY
/// export: one short-lived forced-download grant (`{exportId, deliveryRef, method, url,
/// expiresAt, size, sha256}`) to use straight away; the export is DELIVERED at once. 409
/// EXPORT_NOT_READY, EXPORT_ALREADY_DELIVERED (nothing is re-sent) or EXPORT_EXPIRED; anyone
/// but the recipient gets 404. Relayed with the caller's own principal signed in.
#[utoipa::path(
    post, path = "/api/v2/messaging/exports/{exportId}/claim", tag = "Messaging (v2)", operation_id = "v2_messaging_claim_export",
    params(("exportId" = Uuid, Path, description = "The export's id")),
    responses(
        (status = 200, description = "The one download grant (Messaging's ExportClaimGrant)", body = Object),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 404, description = "Not the caller's export, or FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 409, description = "EXPORT_NOT_READY / EXPORT_ALREADY_DELIVERED / EXPORT_EXPIRED", body = ErrorEnvelope),
        (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn claim_export(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(export_id): V2Path<Uuid>,
) -> ApiResult<axum::response::Response> {
    let um = integration(&state)?;
    let user = verified_user(&state, &actor)?;
    let (status, value) = um.claim_export(user, export_id).await?;
    Ok(no_store(status, value))
}

pub(crate) fn parse_query(raw: Option<String>) -> ApiResult<Vec<(String, String)>> {
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
    let user = verified_user(&state, &actor)?;
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
///
/// Forwards an allowlisted GET of the Messaging client API (for example conversations, members,
/// messages, unread counts, MLS state, device welcomes or history transfers) to the Messaging
/// service, acting as the caller's own principal, which is never taken from the request. The path
/// must match the allowlist exactly, with lowercase canonical UUIDs; anything else returns
/// ENDPOINT_NOT_FOUND and is not forwarded. Only the query keys allowed for that path are accepted
/// (at most 8 pairs). Messaging's status and JSON body are returned unchanged; requires a bearer
/// user token whose revocation status could be checked.
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
///
/// Forwards an allowlisted POST of the Messaging client API (for example creating a conversation,
/// adding members, posting a message or MLS commit, claiming KeyPackages, attachment uploads and
/// grants, or history transfers) to the Messaging service, acting as the caller's own principal,
/// which is never taken from the request. The JSON body (at most 1 MiB; empty means `{}`) is
/// forwarded as-is, and an `Idempotency-Key` header is forwarded only where the operation accepts
/// one, such as creating a conversation. Paths outside the allowlist return ENDPOINT_NOT_FOUND and
/// are not forwarded. Messaging's status and JSON body are returned unchanged.
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
///
/// Forwards an allowlisted PUT of the Messaging client API (a conversation read cursor or the
/// sealed history of a history transfer) to the Messaging service, acting as the caller's own
/// principal, which is never taken from the request. The JSON body (at most 1 MiB; empty means
/// `{}`) is forwarded as-is. Paths outside the allowlist return ENDPOINT_NOT_FOUND and are not
/// forwarded. Messaging's status and JSON body are returned unchanged.
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

/// Relayed partial update (allowlisted path under `v1/`, e.g. editing a MANAGED message).
///
/// Forwards an allowlisted PATCH of the Messaging client API (updating a conversation's
/// lifecycle, expiry, retention or display title, changing a member's role, or editing a
/// MANAGED message with its `expectedEditCount`) to the Messaging service, acting as the
/// caller's own principal, which is never taken from the request. The JSON body (at most
/// 1 MiB; empty means `{}`) is forwarded as-is; Messaging enforces ownership, roles and
/// optimistic concurrency. Paths outside the allowlist return ENDPOINT_NOT_FOUND and are
/// not forwarded. Messaging's status and JSON body are returned unchanged.
#[utoipa::path(
    patch, path = "/api/v2/messaging/relay/{path}", tag = "Messaging (v2)", operation_id = "v2_messaging_relay_patch",
    params(("path" = String, Path, description = "An allowlisted Messaging customer path under v1/")),
    request_body = Object,
    responses((status = 200, description = "Messaging contract answer (status relayed)", body = Object), (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope), (status = 404, description = "ENDPOINT_NOT_FOUND (not relayed)", body = ErrorEnvelope), (status = 503, description = "CHAT_UNAVAILABLE", body = ErrorEnvelope)),
    security(("bearer_auth" = []))
)]
pub async fn relay_patch(
    State(state): State<AppState>,
    actor: ActorContext,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    relay_operation(state, actor, Method::PATCH, path, query, headers, body).await
}

/// Relayed removal (allowlisted path under `v1/`, e.g. leaving a conversation).
///
/// Forwards an allowlisted DELETE of the Messaging client API (removing a conversation member,
/// including leaving, or deleting a message) to the Messaging service, acting as the caller's own
/// principal, which is never taken from the request. No body is sent. Paths outside the allowlist
/// return ENDPOINT_NOT_FOUND and are not forwarded. Messaging's status and JSON body are returned
/// unchanged.
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
///
/// Returns the current meeting-chat migration state (LEGACY_PRIMARY, SHADOW_VALIDATION,
/// MESSAGING_PRIMARY or LEGACY_RETIRED) and its version, the 20 most recent transitions, counts of
/// messages still to mirror or copy back and of pending session revocations, the number of linked
/// conferences and the latest parity run. Platform administrators only; returns FEATURE_DISABLED
/// when the Messaging integration is not enabled.
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
///
/// Moves the meeting-chat migration to `targetState`; platform administrators only.
/// `expectedVersion` must equal the current version (MEETING_VERSION_CONFLICT otherwise) and
/// `reason` (3–500 characters) is recorded with the transition. Allowed steps are LEGACY_PRIMARY ↔
/// SHADOW_VALIDATION ↔ MESSAGING_PRIMARY → LEGACY_RETIRED, and gates are enforced: cutover to
/// MESSAGING_PRIMARY needs a different operator in `approvedBy`, no messages left to mirror and a
/// PASSED parity run newer than the current shadow period; LEGACY_RETIRED also needs `approvedBy`;
/// returning to LEGACY_PRIMARY needs every message copied back. A refused step returns
/// CHAT_MIGRATION_REFUSED; on success the new status is returned.
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
///
/// Runs a parity check immediately, comparing the legacy chat history of every linked conference
/// (main room, breakout rooms and private chats) with Messaging, records the run and returns it
/// with the numbers of conferences and messages checked and mismatches found. The run is PASSED
/// only when nothing differs; cutover to MESSAGING_PRIMARY requires a recent PASSED run. Platform
/// administrators only.
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
