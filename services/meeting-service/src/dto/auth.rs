use crate::services::account_devices::DeviceProof;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

// One-time code: exactly 6 digits (leading zeros allowed).
static OTP_CODE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{6}$").unwrap());

// Magic-link token: 64 lower-case hex characters.
static LINK_TOKEN_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").unwrap());

/// Start a passwordless sign-in (or sign-up): a one-time code is emailed.
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct OtpRequest {
    #[validate(
        email(message = "Invalid email address"),
        length(max = 255, message = "Invalid email address")
    )]
    #[schema(example = "ada@example.com")]
    pub email: String,
}

/// Finish a passwordless sign-in with the emailed 6-digit code.
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct OtpVerifyRequest {
    #[validate(
        email(message = "Invalid email address"),
        length(max = 255, message = "Invalid email address")
    )]
    #[schema(example = "ada@example.com")]
    pub email: String,

    #[validate(regex(path = *OTP_CODE_REGEX, message = "The code must be 6 digits"))]
    #[schema(example = "048213")]
    pub code: String,
}

/// Finish a passwordless sign-in with the magic link from the same email (the email
/// address and token both come from the link).
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct OtpVerifyLinkRequest {
    #[validate(
        email(message = "Invalid email address"),
        length(max = 255, message = "Invalid email address")
    )]
    pub email: String,

    #[validate(regex(path = *LINK_TOKEN_REGEX, message = "Invalid or expired link"))]
    pub token: String,
}

/// Tokens and account of a completed sign-in.
#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
}

/// A completed passwordless sign-in.
#[derive(Debug, Serialize, ToSchema)]
pub struct OtpVerifyResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
    /// True when this sign-in created the account (first sign-in with this address).
    pub is_new_account: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    /// `pending_verification` (spelled `pendingverification` until
    /// `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
    pub status: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct RefreshTokenRequest {
    #[validate(length(min = 1, message = "Refresh token is required"))]
    pub refresh_token: String,
    /// Required when the sign-in is bound to a linked device (multi-device policy): a
    /// proof of the device key with purpose `REFRESH` and context = the session id.
    #[serde(default, alias = "deviceProof")]
    pub device_proof: Option<DeviceProof>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
}

/// One-time Google sign-in exchange (ACC-005): the callback redirect carries only a
/// short-lived, single-use `exchange_code`, never tokens.
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct GoogleExchangeRequest {
    #[validate(length(min = 20, max = 128, message = "Invalid exchange code"))]
    pub exchange_code: String,
}

/// Undo a self-service email change with the token mailed to the previous address.
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct EmailChangeRevertRequest {
    #[validate(regex(path = *LINK_TOKEN_REGEX, message = "Invalid or expired link"))]
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_must_be_exactly_six_digits_and_may_start_with_zero() {
        let req = |code: &str| OtpVerifyRequest {
            email: "ada@example.com".into(),
            code: code.into(),
        };
        assert!(req("012345").validate().is_ok());
        assert!(req("999999").validate().is_ok());
        for bad in ["", "12345", "1234567", "12345a", " 12345", "١٢٣٤٥٦"] {
            assert!(req(bad).validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn emails_and_link_tokens_are_validated() {
        assert!(OtpRequest {
            email: "not-an-email".into()
        }
        .validate()
        .is_err());
        assert!(OtpRequest {
            email: format!("{}@example.com", "a".repeat(250))
        }
        .validate()
        .is_err());
        let link = |t: &str| OtpVerifyLinkRequest {
            email: "ada@example.com".into(),
            token: t.into(),
        };
        assert!(link(&"a1".repeat(32)).validate().is_ok());
        assert!(link(&"A1".repeat(32)).validate().is_err());
        assert!(link("abc").validate().is_err());
    }
}
