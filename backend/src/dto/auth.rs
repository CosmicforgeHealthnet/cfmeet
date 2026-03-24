use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidationError};

// Username: alphanumeric and underscores only
static USERNAME_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_]+$").unwrap());

// Verification code: 6 digits only
static VERIFICATION_CODE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{6}$").unwrap());

// Custom password validation function (Rust regex doesn't support lookahead)
fn validate_password_strength(password: &str) -> bool {
    let has_lowercase = password.chars().any(|c| c.is_lowercase());
    let has_uppercase = password.chars().any(|c| c.is_uppercase());
    let has_digit = password.chars().any(|c| c.is_numeric());
    let has_special = password.chars().any(|c| "!@#$%^&*(),.?:{}|<>".contains(c));

    has_lowercase && has_uppercase && has_digit && has_special
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[validate(schema(function = "validate_register_passwords"))]
pub struct RegisterRequest {
    #[validate(
        length(min = 3, max = 50, message = "Username must be between 3 and 50 characters"),
        regex(path = *USERNAME_REGEX, message = "Username can only contain letters, numbers, and underscores")
    )]
    pub username: String,

    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(length(
        min = 8,
        max = 128,
        message = "Password must be between 8 and 128 characters"
    ))]
    pub password: String,

    #[validate(length(min = 1, message = "Confirm password is required"))]
    pub confirm_password: String,
}

fn validate_register_passwords(req: &RegisterRequest) -> Result<(), ValidationError> {
    if req.password != req.confirm_password {
        return Err(ValidationError::new("passwords_mismatch")
            .with_message("Passwords do not match".into()));
    }

    if !validate_password_strength(&req.password) {
        return Err(ValidationError::new("password_strength")
            .with_message("Password must contain at least one uppercase letter, one lowercase letter, one digit, and one special character (!@#$%^&*(),.?:{}|<>)".into()));
    }

    Ok(())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterResponse {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub status: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct LoginRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(length(min = 1, message = "Password is required"))]
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    pub status: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct VerifyEmailRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(regex(path = *VERIFICATION_CODE_REGEX, message = "Verification code must be 6 digits"))]
    pub token: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct RefreshTokenRequest {
    #[validate(length(min = 1, message = "Refresh token is required"))]
    pub refresh_token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
}
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct ResendVerificationRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct ForgotPasswordRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[validate(schema(function = "validate_reset_passwords"))]
pub struct ResetPasswordRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(length(min = 1, message = "Reset token is required"))]
    pub token: String,

    #[validate(length(
        min = 8,
        max = 128,
        message = "Password must be between 8 and 128 characters"
    ))]
    pub new_password: String,

    #[validate(length(min = 1, message = "Confirm password is required"))]
    pub confirm_password: String,
}

fn validate_reset_passwords(req: &ResetPasswordRequest) -> Result<(), ValidationError> {
    if req.new_password != req.confirm_password {
        return Err(ValidationError::new("passwords_mismatch")
            .with_message("Passwords do not match".into()));
    }

    if !validate_password_strength(&req.new_password) {
        return Err(ValidationError::new("password_strength")
            .with_message("Password must contain at least one uppercase letter, one lowercase letter, one digit, and one special character (!@#$%^&*(),.?:{}|<>)".into()));
    }

    Ok(())
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct VerifyResetTokenRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(regex(path = *VERIFICATION_CODE_REGEX, message = "Reset token must be 6 digits"))]
    pub token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VerifyResetTokenResponse {
    pub message: String,
    pub valid: bool,
}

