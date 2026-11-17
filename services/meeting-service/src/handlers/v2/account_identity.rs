//! Step-up confirmation and secure email change for the signed-in account (passwordless
//! sign-in: the email address is the credential).

use axum::{extract::State, http::HeaderMap, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{account_sessions::current_session, V2Json};
use crate::{
    error::{ApiError, ApiResult},
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

/// Export my personal data
///
/// Gap audit A2: one JSON document of everything the Meeting Service holds about the
/// signed-in account (profile, sign-ins, linked devices, owned meeting spaces, memberships,
/// webinar registrations, conference participation and its own security events). Chats,
/// contacts and calls are held by Messaging, the Communications Hub and Calling, which the
/// document names. Requires a session-bound sign-in.
#[utoipa::path(
    get, path = "/api/v2/me/data-export", tag = "Account security (v2)", operation_id = "v2_export_my_data",
    responses(
        (status = 200, description = "The personal-data export", body = Object),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "REAUTHENTICATION_REQUIRED - the token has no session; sign in again", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn export_my_data(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (user, _) = signed_in(&state, &actor, &headers).await?;
    Ok(Json(
        crate::services::account_erasure::export(&state, user.id).await?,
    ))
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
/// credentials end with them. The previous address receives one confirmation email.
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
