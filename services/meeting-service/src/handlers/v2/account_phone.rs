use super::{account_sessions::current_session, V2Json};
use crate::dto::auth::OtpVerifyResponse;
use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        account_phone::{
            self as phone, PhoneChallenge, PhoneConfirm, PhoneRequest, PhoneView, TwilioVerify,
        },
        actor::ActorContext,
        step_up::{self, StepUpPurpose},
    },
    state::AppState,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhoneBindRequest {
    pub phone_number: String,
    pub step_up_token: String,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhoneSettings {
    pub discoverable: bool,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhoneRemove {
    pub step_up_token: String,
}

fn owner(
    actor: &ActorContext,
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<(uuid::Uuid, uuid::Uuid)> {
    let user = actor.require_registered()?;
    let session = current_session(state, headers).ok_or_else(|| {
        ApiError::domain(
            ErrorCode::PermissionDenied,
            "Sign in again before changing your phone",
        )
    })?;
    Ok((user.user_id, session))
}

/// Get my linked phone
///
/// Returns the current account binding and provider availability. Phone verification is optional.
#[utoipa::path(get,path="/api/v2/me/phone",tag="Account security (v2)",operation_id="v2_get_my_phone",responses((status=200,body=PhoneView),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)),security(("bearer_auth"=[])))]
pub async fn get_my_phone(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<PhoneView>> {
    Ok(Json(
        phone::view(&state.db, actor.require_registered()?.user_id).await?,
    ))
}
/// Request phone binding
///
/// Consumes a session-bound email step-up token and sends an SMS verification challenge. No number is linked until provider approval.
#[utoipa::path(post,path="/api/v2/me/phone/verification",tag="Account security (v2)",operation_id="v2_request_phone_binding",request_body=PhoneBindRequest,responses((status=200,body=PhoneChallenge),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)),security(("bearer_auth"=[])))]
pub async fn request_binding(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<PhoneBindRequest>,
) -> ApiResult<Json<PhoneChallenge>> {
    let provider = TwilioVerify::from_env()?;
    let (user, session) = owner(&actor, &state, &headers)?;
    step_up::consume(
        &state,
        user,
        Some(session),
        StepUpPurpose::ManagePhone,
        Some(&req.step_up_token),
    )
    .await?;
    let id = phone::start(&state.db, &provider, user, Some(session), &req.phone_number).await?;
    Ok(Json(PhoneChallenge::sent(id)))
}
/// Confirm phone binding
///
/// Confirms a one-use SMS challenge belonging to this account and session. A successful provider check establishes the binding.
#[utoipa::path(post,path="/api/v2/me/phone/confirm",tag="Account security (v2)",operation_id="v2_confirm_phone_binding",request_body=PhoneConfirm,responses((status=200,body=PhoneView),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)),security(("bearer_auth"=[])))]
pub async fn confirm_binding(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<PhoneConfirm>,
) -> ApiResult<Json<PhoneView>> {
    let provider = TwilioVerify::from_env()?;
    let (user, session) = owner(&actor, &state, &headers)?;
    phone::verify(
        &state.db,
        &provider,
        req.challenge_id,
        Some((user, session)),
        &req.code,
    )
    .await?;
    Ok(Json(phone::view(&state.db, user).await?))
}
/// Update phone discovery
///
/// Sets explicit opt-in discovery for the signed-in account. New or changed bindings start private.
#[utoipa::path(patch,path="/api/v2/me/phone",tag="Account security (v2)",operation_id="v2_update_phone_privacy",request_body=PhoneSettings,responses((status=200,body=PhoneView),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)),security(("bearer_auth"=[])))]
pub async fn update_privacy(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Json(req): V2Json<PhoneSettings>,
) -> ApiResult<Json<PhoneView>> {
    let _provider = TwilioVerify::from_env()?;
    let user = actor.require_registered()?.user_id;
    phone::set_discoverable(&state.db, user, req.discoverable).await?;
    Ok(Json(phone::view(&state.db, user).await?))
}
/// Remove my phone
///
/// Consumes an email step-up token, removes the account binding and revokes every existing sign-in session.
#[utoipa::path(delete,path="/api/v2/me/phone",tag="Account security (v2)",operation_id="v2_remove_phone",request_body=PhoneRemove,responses((status=204),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)),security(("bearer_auth"=[])))]
pub async fn remove_phone(
    State(state): State<AppState>,
    actor: ActorContext,
    headers: HeaderMap,
    V2Json(req): V2Json<PhoneRemove>,
) -> ApiResult<StatusCode> {
    let (user, session) = owner(&actor, &state, &headers)?;
    step_up::consume(
        &state,
        user,
        Some(session),
        StepUpPurpose::ManagePhone,
        Some(&req.step_up_token),
    )
    .await?;
    phone::remove(&state.db, user).await?;
    crate::services::communications::credentials_ended(&state, user, "PHONE_REMOVED").await;
    Ok(StatusCode::NO_CONTENT)
}

/// Request phone sign-in code
///
/// Sends a code only for an active account with a verified binding. Always returns the same neutral response, including for unknown numbers.
#[utoipa::path(post,path="/api/v1/auth/phone/request",tag="Authentication",operation_id="request_phone_otp",request_body=PhoneRequest,responses((status=200,body=PhoneChallenge),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)))]
pub async fn request_signin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PhoneRequest>,
) -> ApiResult<Json<PhoneChallenge>> {
    let provider = TwilioVerify::from_env()?;
    crate::handlers::auth::limit_by_client(&state, &headers, "phone-request", 20, 900).await?;
    let number = phone::number(&req.phone_number)?;
    use sha2::{Digest, Sha256};
    state
        .rate_limiter
        .check_action_rate(
            "phone-request",
            &hex::encode(Sha256::digest(number.as_bytes())),
            3,
            900,
        )
        .await?;
    let mut id = uuid::Uuid::new_v4();
    if let Some(user) = phone::find(&state.db, &number, false).await? {
        // The response cannot reveal whether the account/number exists or the provider
        // refused that destination. No account is created by phone-number sign-in.
        if let Ok(started) = phone::start(&state.db, &provider, user, None, &number).await {
            id = started;
        }
    }
    Ok(Json(PhoneChallenge::sent(id)))
}
/// Verify phone sign-in code
///
/// Consumes a provider-approved phone challenge. Accounts with an authenticator must complete the existing second-factor flow.
#[utoipa::path(post,path="/api/v1/auth/phone/verify",tag="Authentication",operation_id="verify_phone_otp",request_body=PhoneConfirm,responses((status=200,body=OtpVerifyResponse),(status=400,description="Invalid phone request or verification",body=ErrorEnvelope),(status=401,description="AUTHENTICATION_REQUIRED",body=ErrorEnvelope),(status=403,description="Recent authentication required",body=ErrorEnvelope),(status=429,description="Too many attempts",body=ErrorEnvelope),(status=503,description="Phone provider unavailable",body=ErrorEnvelope)))]
pub async fn verify_signin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PhoneConfirm>,
) -> ApiResult<Json<OtpVerifyResponse>> {
    let provider = TwilioVerify::from_env()?;
    crate::handlers::auth::limit_by_client(&state, &headers, "phone-verify", 30, 900).await?;
    let user = phone::verify(&state.db, &provider, req.challenge_id, None, &req.code).await?;
    Ok(Json(
        crate::handlers::auth::phone_sign_in(&state, &headers, user).await?,
    ))
}
