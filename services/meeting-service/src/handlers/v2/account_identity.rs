//! Step-up confirmation and secure email change for the signed-in account (passwordless
//! sign-in: the email address is the credential).

use axum::{
    body::Body,
    extract::State,
    http::{
        header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE},
        HeaderMap, HeaderValue,
    },
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{account_sessions::current_session, V2Json, V2Query};
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    models::users,
    services::{
        actor::ActorContext,
        step_up::{self, StepUpPurpose, STEP_UP_TOKEN_TTL_SECS},
    },
    state::AppState,
};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepUpRequest {
    pub purpose: StepUpPurpose,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepUpVerifyRequest {
    pub purpose: StepUpPurpose,
    /// The 6-digit code emailed to the account's current address.
    #[schema(example = "048213")]
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodeSentResponse {
    pub message: String,
    /// How long the emailed code stays valid.
    pub expires_in_secs: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepUpTokenResponse {
    /// Single-use token for the purpose, bound to this session. Send it with the action.
    pub step_up_token: String,
    pub expires_in_secs: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmailChangeRequest {
    #[schema(example = "new.address@example.com")]
    pub new_email: String,
    /// From `POST /api/v2/me/step-up/verify` with purpose `CHANGE_EMAIL`.
    pub step_up_token: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmailChangeConfirmRequest {
    pub new_email: String,
    /// The 6-digit code emailed to the NEW address.
    pub code: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmailChangedResponse {
    pub email: String,
    /// Other sessions were signed out; this one stays signed in.
    pub other_sessions_ended: bool,
}

fn six_digits(code: &str) -> ApiResult<()> {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ApiError::BadRequest("The code must be 6 digits".into()))
    }
}

async fn signed_in(
    state: &AppState,
    actor: &ActorContext,
    headers: &HeaderMap,
) -> ApiResult<(users::Model, uuid::Uuid)> {
    let registered = actor.require_registered()?;
    let user = step_up::account(state, registered.user_id).await?;
    // Step-up tokens are bound to a session: pre-session tokens sign in again first.
    let session = current_session(state, headers).ok_or_else(step_up::step_up_required)?;
    Ok((user, session))
}

/// Request a step-up code
///
/// Emails a 6-digit code (valid 10 minutes) to the signed-in account's CURRENT address, to confirm
/// a sensitive action: `DEACTIVATE_ACCOUNT` or `CHANGE_EMAIL`. Exchange it with `POST
/// /api/v2/me/step-up/verify`. At most 5 codes per purpose within 15 minutes (429).
#[utoipa::path(
    post, path = "/api/v2/me/step-up/request", tag = "Account security (v2)", operation_id = "v2_request_step_up",
    request_body = StepUpRequest,
    responses(
        (status = 200, description = "Code sent", body = CodeSentResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - the token has no session; sign in again", body = ErrorEnvelope),
        (status = 429, description = "Too many codes requested", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn request_step_up(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<StepUpRequest>,
) -> ApiResult<Json<CodeSentResponse>> {
    let (user, _) = signed_in(&state, &actor, &headers).await?;
    step_up::request(&state, &user, req.purpose).await?;
    Ok(Json(CodeSentResponse {
        message: "A confirmation code was sent to your email address.".into(),
        expires_in_secs: crate::services::otp::CODE_TTL_SECS,
    }))
}

/// Verify a step-up code
///
/// Exchanges the emailed code for a single-use `stepUpToken` (valid 5 minutes), bound to this
/// session and purpose. A wrong or expired code gets 400; after 5 checks within 15 minutes the code
/// is destroyed (429).
#[utoipa::path(
    post, path = "/api/v2/me/step-up/verify", tag = "Account security (v2)", operation_id = "v2_verify_step_up",
    request_body = StepUpVerifyRequest,
    responses(
        (status = 200, description = "Confirmed", body = StepUpTokenResponse),
        (status = 400, description = "Invalid or expired code", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 429, description = "Too many attempts", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn verify_step_up(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<StepUpVerifyRequest>,
) -> ApiResult<Json<StepUpTokenResponse>> {
    six_digits(&req.code)?;
    let (user, session) = signed_in(&state, &actor, &headers).await?;
    let token = step_up::verify(&state, user.id, session, req.purpose, &req.code).await?;
    Ok(Json(StepUpTokenResponse {
        step_up_token: token,
        expires_in_secs: STEP_UP_TOKEN_TTL_SECS,
    }))
}

/// Start an email change
///
/// Needs a `stepUpToken` for `CHANGE_EMAIL` (proof of the CURRENT mailbox). Emails a 6-digit code
/// (valid 10 minutes) to the NEW address; finish with `POST /api/v2/me/email/change/verify`. The
/// answer is the same whether or not the new address already has an account (that address gets a
/// notice instead of a code, and the change cannot complete). The address can be changed once
/// every 7 days (429).
#[utoipa::path(
    post, path = "/api/v2/me/email/change/request", tag = "Account security (v2)", operation_id = "v2_request_email_change",
    request_body = EmailChangeRequest,
    responses(
        (status = 200, description = "Code sent to the new address", body = CodeSentResponse),
        (status = 400, description = "Invalid address, or already the account's address", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - missing, used or foreign step-up token", body = ErrorEnvelope),
        (status = 429, description = "Changed within the last 7 days, or too many codes", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn request_email_change(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<EmailChangeRequest>,
) -> ApiResult<Json<CodeSentResponse>> {
    let (user, session) = signed_in(&state, &actor, &headers).await?;
    step_up::consume(
        &state,
        user.id,
        Some(session),
        StepUpPurpose::ChangeEmail,
        Some(&req.step_up_token),
    )
    .await?;
    crate::services::email_change::request(&state, &user, &req.new_email).await?;
    Ok(Json(CodeSentResponse {
        message: "If the new address can receive email, a confirmation code is on its way.".into(),
        expires_in_secs: crate::services::otp::CODE_TTL_SECS,
    }))
}

/// Confirm an email change
///
/// Commits the change with the code sent to the new address. The new address is verified; every
/// OTHER session is signed out (this one stays); the previous address gets a notice with a
/// single-use link to undo the change within 7 days (`POST /api/v1/auth/email-change/revert`),
/// and the new address a confirmation.
#[utoipa::path(
    post, path = "/api/v2/me/email/change/verify", tag = "Account security (v2)", operation_id = "v2_confirm_email_change",
    request_body = EmailChangeConfirmRequest,
    responses(
        (status = 200, description = "Email address changed", body = EmailChangedResponse),
        (status = 400, description = "Invalid or expired code", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 409, description = "The address was taken in the meantime", body = ErrorEnvelope),
        (status = 429, description = "Too many attempts", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn confirm_email_change(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<EmailChangeConfirmRequest>,
) -> ApiResult<Json<EmailChangedResponse>> {
    six_digits(&req.code)?;
    let (user, session) = signed_in(&state, &actor, &headers).await?;
    let updated =
        crate::services::email_change::confirm(&state, &user, session, &req.new_email, &req.code)
            .await?;
    Ok(Json(EmailChangedResponse {
        email: updated.email,
        other_sessions_ended: true,
    }))
}

/// ADR 0068 A7: which owner-delivered export to download (the emailed link's `export`).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataExportQuery {
    /// An export CF Meet support requested for this account (from the emailed download link;
    /// its `token` goes in the `X-CF-Data-Export-Token` header). Omitted: the live export.
    pub export_id: Option<uuid::Uuid>,
}

/// The header carrying the single-use token of an emailed download link.
pub const DATA_EXPORT_TOKEN_HEADER: &str = "x-cf-data-export-token";

/// Export my personal data
///
/// Gap audit A2: one JSON document of everything the Meeting Service holds about the
/// signed-in account (profile, sign-ins, linked devices, owned meeting spaces, memberships,
/// webinar registrations, conference participation and its own security events) and, under
/// `elsewhere`, what Messaging (conversations), the Communications Hub (profile, CF Meet ID,
/// contacts, lists, saved messages, settings) and Calling (call history) hold about it; an
/// owner that cannot answer is reported as unavailable. Requires a session-bound sign-in.
///
/// ADR 0068 A7: with `exportId` and the `X-CF-Data-Export-Token` header (both from the emailed
/// link of an export CF Meet support requested for the owner), the stored copy of THAT export
/// is answered instead, once (it is deleted at once): this needs a sign-in made within the
/// re-authentication window (otherwise 403 `REAUTHENTICATION_REQUIRED`: sign in again), and a
/// used, foreign, wrong or expired link (7 days) is 404.
#[utoipa::path(
    get, path = "/api/v2/me/data-export", tag = "Account security (v2)", operation_id = "v2_export_my_data",
    params(
        DataExportQuery,
        ("X-CF-Data-Export-Token" = Option<String>, Header, description = "The single-use token of the emailed download link (with exportId)"),
    ),
    responses(
        (status = 200, description = "The personal-data export", body = Object),
        (status = 400, description = "VALIDATION_ERROR - exportId without X-CF-Data-Export-Token (or the reverse)", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - the token has no session, or (exportId) the sign-in is not recent; sign in again", body = ErrorEnvelope),
        (status = 404, description = "NOT_FOUND - (exportId) the link is invalid, already used or expired", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn export_my_data(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Query(query): V2Query<DataExportQuery>,
) -> ApiResult<Response> {
    let (user, _) = signed_in(&state, &actor, &headers).await?;
    let token = headers
        .get(DATA_EXPORT_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    match (query.export_id, token) {
        (None, None) => Ok(
            Json(crate::services::account_erasure::export(&state, user.id).await?).into_response(),
        ),
        (Some(export_id), Some(token)) => {
            let recent = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .and_then(|t| state.jwt_service.verify_token(t).ok())
                .is_some_and(|claims| {
                    state
                        .auth_policy
                        .recently_authenticated(&claims, chrono::Utc::now().timestamp())
                });
            if !recent {
                return Err(ApiError::domain(
                    ErrorCode::ReauthenticationRequired,
                    "Downloading your data needs a recent sign-in; sign in again",
                ));
            }
            let document =
                crate::services::account_export::download(&state, user.id, export_id, token)
                    .await?;
            let mut response = Response::new(Body::from(document.to_vec()));
            let h = response.headers_mut();
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            h.insert(
                CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"cfmeet-data-export.json\""),
            );
            h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(response)
        }
        _ => Err(ApiError::BadRequest(
            "exportId and X-CF-Data-Export-Token go together".into(),
        )),
    }
}

/// Gap audit A2: the confirmation of an account deletion.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteAccountRequest {
    /// A step-up token for purpose `DELETE_ACCOUNT` (`POST /api/v2/me/step-up/verify`).
    pub step_up_token: String,
}

/// Delete my account
///
/// Gap audit A2: permanently deletes (anonymizes) the signed-in account after a
/// `DELETE_ACCOUNT` step-up: the address, username, name, avatar, time zone, locale and any
/// password are removed and the account can never sign in again; the Google link is removed,
/// meeting memberships end, owned meeting spaces are archived and webinar registrations are
/// cancelled; every sign-in and device ends, and Messaging, the Communications Hub and Calling
/// credentials end with them. The person's messages, Hub profile, CF Meet ID, contacts,
/// lists, saved messages and call history are then erased at those services (durable,
/// retried until each confirms). The previous address receives one confirmation email.
#[utoipa::path(
    post, path = "/api/v2/me/account/delete", tag = "Account security (v2)", operation_id = "v2_delete_my_account",
    request_body = DeleteAccountRequest,
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - missing, wrong, used or expired step-up token", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_my_account(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<DeleteAccountRequest>,
) -> ApiResult<axum::http::StatusCode> {
    let (user, session) = signed_in(&state, &actor, &headers).await?;
    step_up::consume(
        &state,
        user.id,
        Some(session),
        StepUpPurpose::DeleteAccount,
        Some(req.step_up_token.as_str()),
    )
    .await?;
    crate::services::account_erasure::erase(&state, user.id, Some(session)).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Gap audit A1: the authenticator enrolment secret (shown once).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MfaSetupResponse {
    /// The base32 secret for manual entry.
    pub secret: String,
    /// The `otpauth://` URI to show as a QR code.
    pub otpauth_uri: String,
}

/// Gap audit A1: a code from the authenticator app.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MfaConfirmRequest {
    pub code: String,
}

/// Gap audit A1: the recovery codes (shown once).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MfaRecoveryCodes {
    pub recovery_codes: Vec<String>,
}

/// Gap audit A1: the confirmation of turning two-step sign-in off.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MfaDisableRequest {
    /// A step-up token for purpose `DISABLE_MFA`.
    pub step_up_token: String,
}

/// My two-step sign-in state
///
/// Gap audit A1: whether an authenticator app is enabled and how many unused recovery codes
/// remain.
#[utoipa::path(
    get, path = "/api/v2/me/mfa", tag = "Account security (v2)", operation_id = "v2_get_my_mfa",
    responses(
        (status = 200, description = "{totpEnabled, recoveryCodesRemaining}", body = Object),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_my_mfa(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<serde_json::Value>> {
    let user = actor.require_registered()?;
    Ok(Json(
        crate::services::mfa::status(&state.db, user.user_id).await?,
    ))
}

/// Start enrolling an authenticator app
///
/// Gap audit A1: returns a new secret (and its `otpauth://` URI for a QR code). Nothing changes
/// at sign-in until `POST /api/v2/me/mfa/totp/confirm` with a current code. 409 when an
/// authenticator is already enabled.
#[utoipa::path(
    post, path = "/api/v2/me/mfa/totp/setup", tag = "Account security (v2)", operation_id = "v2_setup_my_totp",
    responses(
        (status = 200, description = "Secret", body = MfaSetupResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - the token has no session; sign in again", body = ErrorEnvelope),
        (status = 409, description = "Already enabled", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn setup_my_totp(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
) -> ApiResult<Json<MfaSetupResponse>> {
    let (user, _) = signed_in(&state, &actor, &headers).await?;
    let (secret, otpauth_uri) =
        crate::services::mfa::setup(&state.db, user.id, &user.username).await?;
    Ok(Json(MfaSetupResponse {
        secret,
        otpauth_uri,
    }))
}

/// Enable the authenticator app
///
/// Gap audit A1: confirms the enrolment with a current 6-digit code and returns 10 single-use
/// recovery codes (shown once). From now on every sign-in also needs a code. The account's
/// address receives a security email.
#[utoipa::path(
    post, path = "/api/v2/me/mfa/totp/confirm", tag = "Account security (v2)", operation_id = "v2_confirm_my_totp",
    request_body = MfaConfirmRequest,
    responses(
        (status = 200, description = "Enabled", body = MfaRecoveryCodes),
        (status = 400, description = "Invalid or expired code", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - the token has no session; sign in again", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn confirm_my_totp(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<MfaConfirmRequest>,
) -> ApiResult<Json<MfaRecoveryCodes>> {
    let (user, _) = signed_in(&state, &actor, &headers).await?;
    let recovery_codes = crate::services::mfa::confirm(&state.db, user.id, &req.code).await?;
    crate::services::account_security::notify_security(
        &state,
        user.id,
        crate::services::email::SecurityNotice::MfaEnabled,
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    Ok(Json(MfaRecoveryCodes { recovery_codes }))
}

/// Turn two-step sign-in off
///
/// Gap audit A1: removes the authenticator and its recovery codes after a `DISABLE_MFA` step-up
/// (an emailed code). The account's address receives a security email.
#[utoipa::path(
    post, path = "/api/v2/me/mfa/disable", tag = "Account security (v2)", operation_id = "v2_disable_my_mfa",
    request_body = MfaDisableRequest,
    responses(
        (status = 204, description = "Turned off"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - missing, wrong, used or expired step-up token", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn disable_my_mfa(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<MfaDisableRequest>,
) -> ApiResult<axum::http::StatusCode> {
    let (user, session) = signed_in(&state, &actor, &headers).await?;
    step_up::consume(
        &state,
        user.id,
        Some(session),
        StepUpPurpose::DisableMfa,
        Some(req.step_up_token.as_str()),
    )
    .await?;
    crate::services::mfa::disable(&state.db, user.id).await?;
    crate::services::account_security::notify_security(
        &state,
        user.id,
        crate::services::email::SecurityNotice::MfaDisabled,
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
