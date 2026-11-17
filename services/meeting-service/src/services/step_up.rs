//! Step-up confirmation for sensitive actions (deactivating the account, changing its
//! email address).
//!
//! With passwordless sign-in, a stolen session token must not be enough to take over or
//! destroy an account. The signed-in owner proves control of the account's CURRENT
//! mailbox again: `request` emails a code, `verify` exchanges it for a short-lived
//! step-up token bound to the user, the session and the purpose, and the sensitive
//! endpoint consumes that token exactly once.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::models::users;
use crate::services::otp::{generate_code, OtpCheck, OtpType};
use crate::state::AppState;

/// Lifetime of a step-up token once the code was verified.
pub const STEP_UP_TOKEN_TTL_SECS: u64 = 5 * 60;

/// What the confirmation is for; a token for one purpose never authorizes another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StepUpPurpose {
    DeactivateAccount,
    ChangeEmail,
    /// Gap audit A2: deleting (anonymizing) the account.
    DeleteAccount,
    /// Gap audit A1: turning the authenticator-app second factor off.
    DisableMfa,
}

impl StepUpPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            StepUpPurpose::DeactivateAccount => "DEACTIVATE_ACCOUNT",
            StepUpPurpose::ChangeEmail => "CHANGE_EMAIL",
            StepUpPurpose::DeleteAccount => "DELETE_ACCOUNT",
            StepUpPurpose::DisableMfa => "DISABLE_MFA",
        }
    }

    /// The action as shown in the email ("confirm that you want to …").
    fn action(self) -> &'static str {
        match self {
            StepUpPurpose::DeactivateAccount => "deactivate your CF Meet account",
            StepUpPurpose::ChangeEmail => "change the email address of your CF Meet account",
            StepUpPurpose::DeleteAccount => "permanently delete your CF Meet account",
            StepUpPurpose::DisableMfa => "turn off two-step sign-in for your CF Meet account",
        }
    }
}

fn subject(user_id: Uuid, purpose: StepUpPurpose) -> String {
    format!("user:{user_id}:{}", purpose.as_str())
}

fn token_key(token: &str) -> String {
    format!(
        "stepup:token:{}",
        hex::encode(Sha256::digest(token.as_bytes()))
    )
}

fn binding(user_id: Uuid, session_id: Uuid, purpose: StepUpPurpose) -> String {
    format!("{user_id}:{session_id}:{}", purpose.as_str())
}

/// The signed-in account (v2 handlers stay thin transport and never query entities).
pub async fn account(state: &AppState, user_id: Uuid) -> ApiResult<users::Model> {
    use sea_orm::EntityTrait;
    users::Entity::find_by_id(user_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Account is unavailable".into()))
}

/// `403 REAUTHENTICATION_REQUIRED`: the action needs a fresh step-up confirmation.
pub fn step_up_required() -> ApiError {
    ApiError::domain(
        ErrorCode::ReauthenticationRequired,
        "Confirm it's you: request a code with POST /api/v2/me/step-up/request and retry with the step_up_token",
    )
}

/// Email a step-up code to the account's current address (at most 5 per 15 minutes).
pub async fn request(
    state: &AppState,
    user: &users::Model,
    purpose: StepUpPurpose,
) -> ApiResult<()> {
    let subject = subject(user.id, purpose);
    state
        .otp_service
        .check_send_rate(&subject, OtpType::StepUp)
        .await?;
    let code = generate_code();
    state
        .otp_service
        .store_code(&subject, &code, OtpType::StepUp)
        .await?;
    state
        .email_service
        .send_step_up_code(&user.email, &user.username, &code, purpose.action())
        .await?;
    crate::services::ops_metrics::inc(
        &crate::services::ops_metrics::OPS_METRICS.auth_step_up_requested,
    );
    Ok(())
}

/// Exchange a step-up code for a single-use token bound to (user, session, purpose).
pub async fn verify(
    state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
    purpose: StepUpPurpose,
    code: &str,
) -> ApiResult<String> {
    match state
        .otp_service
        .check(&subject(user_id, purpose), code, OtpType::StepUp)
        .await?
    {
        OtpCheck::Valid => {}
        OtpCheck::Missing | OtpCheck::Wrong => {
            return Err(ApiError::BadRequest("Invalid or expired code".to_string()))
        }
    }
    let token = crate::services::otp::generate_link_token();
    state
        .redis
        .set(
            &token_key(&token),
            &binding(user_id, session_id, purpose),
            Some(STEP_UP_TOKEN_TTL_SECS),
        )
        .await?;
    crate::services::ops_metrics::inc(
        &crate::services::ops_metrics::OPS_METRICS.auth_step_up_issued,
    );
    Ok(token)
}

/// Consume a step-up token for this exact (user, session, purpose); anything else is
/// `403 REAUTHENTICATION_REQUIRED`. A token is consumed even when it does not match, so
/// a stolen token cannot be probed against other purposes.
pub async fn consume(
    state: &AppState,
    user_id: Uuid,
    session_id: Option<Uuid>,
    purpose: StepUpPurpose,
    token: Option<&str>,
) -> ApiResult<()> {
    let (Some(token), Some(session_id)) = (token.filter(|t| !t.is_empty()), session_id) else {
        return Err(step_up_required());
    };
    let stored = state.redis.take(&token_key(token)).await?;
    if stored.as_deref() == Some(binding(user_id, session_id, purpose).as_str()) {
        Ok(())
    } else {
        Err(step_up_required())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purposes_serialize_as_screaming_snake_case_and_bind_tokens() {
        assert_eq!(
            serde_json::to_string(&StepUpPurpose::ChangeEmail).unwrap(),
            "\"CHANGE_EMAIL\""
        );
        let u = Uuid::new_v4();
        let s = Uuid::new_v4();
        assert_ne!(
            binding(u, s, StepUpPurpose::ChangeEmail),
            binding(u, s, StepUpPurpose::DeactivateAccount)
        );
        assert!(!token_key("abc").contains("abc"));
    }
}
