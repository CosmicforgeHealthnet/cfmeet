//! Account-wide linked devices (multi-device policy): one primary mobile, several web
//! devices, web-only accounts. See `services::account_devices` for the model.
//!
//! These endpoints authenticate the bearer themselves so that a new mobile whose
//! primary-mobile replacement is pending (refused everywhere else) can complete it.

use axum::{
    extract::{FromRequestParts, Query, State},
    http::request::Parts,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::V2Path;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        account_devices::{
            self as devices, ApproveLinkRequest, CompleteLinkRequest, DeclareDeviceRequest,
            DeviceCaller, DeviceLinkView, DeviceOutcome, DevicePolicyView, DeviceProof, DeviceView,
            PendingLinkView, ProofOnlyRequest,
        },
        account_security::{self as security, SecurityEventRecord},
    },
    state::AppState,
};

fn unauthenticated() -> ApiError {
    ApiError::domain(ErrorCode::AuthenticationRequired, "Sign in to continue")
}

#[axum::async_trait]
impl FromRequestParts<AppState> for DeviceCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthenticated)?;
        devices::resolve_caller(state, token).await
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceListQuery {
    /// Also return revoked devices (most recent 100 in total).
    #[serde(default)]
    pub include_revoked: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceView>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingLinkListResponse {
    pub links: Vec<PendingLinkView>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeDeviceRequest {
    /// Required to revoke ANOTHER device: purpose `REVOKE`, context = the target
    /// `deviceId`, signed by the calling (trusted) device. Omit to revoke your own device.
    pub proof: Option<DeviceProof>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MySecurityEventQuery {
    /// `nextCursor` from the previous page.
    pub before: Option<i64>,
    /// 1-100, default 50.
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MySecurityEventPage {
    pub events: Vec<SecurityEventRecord>,
    pub next_cursor: Option<i64>,
}

/// Read my device policy and state
///
/// The account's multi-device state (`WEB_ONLY`, `MOBILE_PRIMARY`,
/// `DEVICE_REPLACEMENT_PENDING`), the backend limits (one primary mobile,
/// `maxWebDevices` web devices), the linking methods available to a new device and the
/// status of the calling sign-in's device. Accepts a pending mobile's sign-in.
#[utoipa::path(
    get, path = "/api/v2/me/device-policy", tag = "Account devices (v2)", operation_id = "v2_get_my_device_policy",
    responses(
        (status = 200, description = "Policy and state", body = DevicePolicyView),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_device_policy(
    State(state): State<AppState>,
    caller: DeviceCaller,
) -> ApiResult<Json<DevicePolicyView>> {
    Ok(Json(devices::policy_view(&state, &caller).await?))
}

/// List my devices
///
/// Logical devices of the account (one per browser profile or mobile installation), with
/// platform, trust, primary-mobile flag, open sign-ins and revocation state. Sign-ins are
/// listed by `GET /api/v2/me/sessions` (`deviceId`).
#[utoipa::path(
    get, path = "/api/v2/me/devices", tag = "Account devices (v2)", operation_id = "v2_list_my_devices",
    params(DeviceListQuery),
    responses(
        (status = 200, description = "Devices", body = DeviceListResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_devices(
    State(state): State<AppState>,
    caller: DeviceCaller,
    Query(q): Query<DeviceListQuery>,
) -> ApiResult<Json<DeviceListResponse>> {
    Ok(Json(DeviceListResponse {
        devices: devices::list_devices(&state, &caller, q.include_revoked).await?,
    }))
}

/// Declare this sign-in's device
///
/// Binds the calling sign-in to a device right after signing in on it (within the
/// re-authentication window). The device proves possession of its Ed25519 key (purpose
/// `DECLARE`, context = the session id). The same installation signing in again reuses
/// its device (older sign-ins of it end: `SUPERSEDED`), never a new one.
///
/// * WEB: trusted and active right away (`BOOTSTRAP`: the fresh sign-in is the proof; no
///   verification step, ADR 0067). An existing untrusted browser declaring again is
///   trusted the same way. Beyond `maxWebDevices` active web devices, the least recently
///   used browser of the account is revoked (its sign-ins end) instead of refusing.
/// * The first device of an account that never had a trusted device is trusted
///   (`BOOTSTRAP`); a first mobile then becomes the primary mobile.
/// * Any other new MOBILE is `UNTRUSTED` and `REPLACEMENT_PENDING` (its sign-in is refused
///   elsewhere with `DEVICE_REPLACEMENT_PENDING`) until it completes a link: approval by a
///   trusted device or an email step-up.
#[utoipa::path(
    post, path = "/api/v2/me/devices/current", tag = "Account devices (v2)", operation_id = "v2_declare_my_device",
    request_body = DeclareDeviceRequest,
    responses(
        (status = 200, description = "Device declared", body = DeviceOutcome),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED / DEVICE_PROOF_INVALID", body = ErrorEnvelope),
        (status = 409, description = "DEVICE_DECLARATION_REFUSED / DEVICE_KEY_IN_USE", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn declare_device(
    State(state): State<AppState>,
    caller: DeviceCaller,
    Json(req): Json<DeclareDeviceRequest>,
) -> ApiResult<Json<DeviceOutcome>> {
    Ok(Json(devices::declare(&state, &caller, req).await?))
}

/// Revoke a device
///
/// Ends every sign-in of the device and every credential it holds downstream (Messaging
/// E2EE device — removed from its MLS groups at the next commit —, push registration,
/// Calling endpoint and live invitations, realtime connections, its meeting media
/// sessions). Other devices are untouched; revoking the primary mobile leaves the account
/// `WEB_ONLY`. Revoking another device needs a trusted calling device and a `REVOKE` proof.
#[utoipa::path(
    post, path = "/api/v2/me/devices/{deviceId}/revoke", tag = "Account devices (v2)", operation_id = "v2_revoke_my_device",
    params(("deviceId" = Uuid, Path, description = "Device id")),
    request_body = RevokeDeviceRequest,
    responses(
        (status = 200, description = "Revoked (or already revoked)", body = DeviceOutcome),
        (status = 401, description = "AUTHENTICATION_REQUIRED / DEVICE_PROOF_INVALID", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED", body = ErrorEnvelope),
        (status = 404, description = "DEVICE_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_device(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(device_id): V2Path<Uuid>,
    Json(req): Json<RevokeDeviceRequest>,
) -> ApiResult<Json<DeviceOutcome>> {
    Ok(Json(
        devices::revoke_device(&state, &caller, device_id, req.proof).await?,
    ))
}

/// Request to link this device
///
/// Called by a new, untrusted device (or a pending mobile): creates a one-time linking
/// challenge (default 5 minutes) bound to the account and this device. Proof purpose
/// `LINK_REQUEST`, context = this `deviceId`. The response carries the 6-digit comparison
/// code (and an optional QR payload) to show on this device; a trusted device must enter
/// it to approve. At most 10 requests per account per hour.
#[utoipa::path(
    post, path = "/api/v2/me/device-links", tag = "Account devices (v2)", operation_id = "v2_create_device_link",
    request_body = ProofOnlyRequest,
    responses(
        (status = 201, description = "Link challenge", body = DeviceLinkView),
        (status = 401, description = "DEVICE_PROOF_INVALID", body = ErrorEnvelope),
        (status = 404, description = "DEVICE_NOT_FOUND (declare the device first)", body = ErrorEnvelope),
        (status = 409, description = "DEVICE_DECLARATION_REFUSED (already trusted)", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    Json(req): Json<ProofOnlyRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<DeviceLinkView>)> {
    Ok((
        axum::http::StatusCode::CREATED,
        Json(devices::create_link(&state, &caller, req).await?),
    ))
}

/// List link requests awaiting approval
///
/// For a trusted device: open link requests of the account, newest first, with the
/// requesting device's platform, label and key thumbprint. Comparison codes are never
/// listed; the user reads it on the new device.
#[utoipa::path(
    get, path = "/api/v2/me/device-links", tag = "Account devices (v2)", operation_id = "v2_list_pending_device_links",
    responses(
        (status = 200, description = "Pending links", body = PendingLinkListResponse),
        (status = 403, description = "DEVICE_NOT_TRUSTED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_pending_links(
    State(state): State<AppState>,
    caller: DeviceCaller,
) -> ApiResult<Json<PendingLinkListResponse>> {
    Ok(Json(PendingLinkListResponse {
        links: devices::pending_links(&state, &caller).await?,
    }))
}

/// Read my link request
///
/// For the device that requested the link: its status (`PENDING`, `APPROVED`, ...).
#[utoipa::path(
    get, path = "/api/v2/me/device-links/{linkId}", tag = "Account devices (v2)", operation_id = "v2_get_device_link",
    params(("linkId" = Uuid, Path, description = "Link id")),
    responses(
        (status = 200, description = "Link", body = DeviceLinkView),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
) -> ApiResult<Json<DeviceLinkView>> {
    Ok(Json(devices::get_link(&state, &caller, link_id).await?))
}

/// Approve a link request
///
/// From a trusted device: enter the comparison code shown on the new device and sign with
/// this device's key (purpose `LINK_APPROVE`, context = `{linkId}|{keyThumbprint of the
/// new device}`). Five wrong codes reject the request.
#[utoipa::path(
    post, path = "/api/v2/me/device-links/{linkId}/approve", tag = "Account devices (v2)", operation_id = "v2_approve_device_link",
    params(("linkId" = Uuid, Path, description = "Link id")),
    request_body = ApproveLinkRequest,
    responses(
        (status = 200, description = "Approved", body = DeviceLinkView),
        (status = 400, description = "VALIDATION_ERROR (code mismatch)", body = ErrorEnvelope),
        (status = 401, description = "DEVICE_PROOF_INVALID", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED", body = ErrorEnvelope),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn approve_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
    Json(req): Json<ApproveLinkRequest>,
) -> ApiResult<Json<DeviceLinkView>> {
    Ok(Json(
        devices::approve_link(&state, &caller, link_id, req).await?,
    ))
}

/// Reject a link request
///
/// From a trusted device: refuses an open link request of the account (for example one the
/// user did not start). The requesting device stays untrusted and the request cannot be
/// approved or completed afterwards.
#[utoipa::path(
    post, path = "/api/v2/me/device-links/{linkId}/reject", tag = "Account devices (v2)", operation_id = "v2_reject_device_link",
    params(("linkId" = Uuid, Path, description = "Link id")),
    responses(
        (status = 200, description = "Rejected", body = DeviceLinkView),
        (status = 403, description = "DEVICE_NOT_TRUSTED", body = ErrorEnvelope),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
) -> ApiResult<Json<DeviceLinkView>> {
    Ok(Json(devices::reject_link(&state, &caller, link_id).await?))
}

/// Email me a step-up code for this link
///
/// For the requesting device when no trusted device is available (web-only recovery,
/// replacing a lost phone): sends a 6-digit code to the account's email address (once per
/// minute). Proves control of the account mailbox; never SMS.
#[utoipa::path(
    post, path = "/api/v2/me/device-links/{linkId}/email-code", tag = "Account devices (v2)", operation_id = "v2_send_device_link_email_code",
    params(("linkId" = Uuid, Path, description = "Link id")),
    responses(
        (status = 200, description = "Code sent", body = DeviceLinkView),
        (status = 404, description = "FEATURE_DISABLED", body = ErrorEnvelope),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
        (status = 429, description = "RATE_LIMITED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn send_email_code(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
) -> ApiResult<Json<DeviceLinkView>> {
    Ok(Json(
        devices::send_email_code(&state, &caller, link_id).await?,
    ))
}

/// Complete my link
///
/// The requesting device finishes its link after approval (or with `emailCode`), proving
/// possession of its key (purpose `LINK_COMPLETE`, context = `{linkId}`). A web device
/// becomes trusted. A pending mobile becomes the primary mobile atomically: the previous
/// primary mobile is revoked (its sign-ins, Messaging device, Calling endpoint, push and
/// meeting media end) and trusted web devices stay signed in. The account's email gets a
/// notice and the security log records it.
#[utoipa::path(
    post, path = "/api/v2/me/device-links/{linkId}/complete", tag = "Account devices (v2)", operation_id = "v2_complete_device_link",
    params(("linkId" = Uuid, Path, description = "Link id")),
    request_body = CompleteLinkRequest,
    responses(
        (status = 200, description = "Linked", body = DeviceOutcome),
        (status = 401, description = "DEVICE_PROOF_INVALID", body = ErrorEnvelope),
        (status = 403, description = "DEVICE_NOT_TRUSTED (not approved / wrong code)", body = ErrorEnvelope),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn complete_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
    Json(req): Json<CompleteLinkRequest>,
) -> ApiResult<Json<DeviceOutcome>> {
    Ok(Json(
        devices::complete_link(&state, &caller, link_id, req).await?,
    ))
}

/// Cancel my link request
///
/// For the device that requested the link: withdraws its own open request (for example to
/// start again with a new comparison code).
#[utoipa::path(
    post, path = "/api/v2/me/device-links/{linkId}/cancel", tag = "Account devices (v2)", operation_id = "v2_cancel_device_link",
    params(("linkId" = Uuid, Path, description = "Link id")),
    responses(
        (status = 200, description = "Cancelled", body = DeviceLinkView),
        (status = 410, description = "DEVICE_LINK_INVALID", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn cancel_link(
    State(state): State<AppState>,
    caller: DeviceCaller,
    V2Path(link_id): V2Path<Uuid>,
) -> ApiResult<Json<DeviceLinkView>> {
    Ok(Json(devices::cancel_link(&state, &caller, link_id).await?))
}

/// Read my security log
///
/// The account's own security events, newest first: sign-ins, device declarations, link
/// requests and approvals, primary-mobile replacements and device revocations. Surviving
/// devices use it to tell the user about an unexpected replacement. Never contains
/// passwords, tokens, email or network addresses.
#[utoipa::path(
    get, path = "/api/v2/me/security-events", tag = "Account devices (v2)", operation_id = "v2_list_my_security_events",
    params(MySecurityEventQuery),
    responses(
        (status = 200, description = "Events", body = MySecurityEventPage),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_my_security_events(
    State(state): State<AppState>,
    caller: DeviceCaller,
    Query(q): Query<MySecurityEventQuery>,
) -> ApiResult<Json<MySecurityEventPage>> {
    let (events, next_cursor) = security::list_events(
        &state.db,
        Some(caller.user.id),
        None,
        q.before,
        q.limit.unwrap_or(50).clamp(1, 100),
    )
    .await?;
    Ok(Json(MySecurityEventPage {
        events,
        next_cursor,
    }))
}
