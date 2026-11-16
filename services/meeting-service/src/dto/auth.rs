use crate::services::account_devices::DeviceProof;
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

/// Message of the [`password_strength`] rule.
pub const PASSWORD_STRENGTH_MESSAGE: &str = "Password must contain at least one uppercase letter, one lowercase letter, one digit, and one special character (!@#$%^&*(),.?:{}|<>)";

/// Field-level password rule (A3): reported on the password field itself (`password` /
/// `new_password`), not as a whole-request `__all__` error. The length rule stays the
/// field's `length` validation.
fn password_strength(password: &str) -> Result<(), ValidationError> {
    if validate_password_strength(password) {
        Ok(())
    } else {
        Err(ValidationError::new("password_strength")
            .with_message(PASSWORD_STRENGTH_MESSAGE.into()))
    }
}

/// A struct-level check's error attributed to the request field `field` (its Rust name;
/// see [`crate::validation::FIELD_PARAM`]).
fn field_error(field: &'static str, code: &'static str, message: &'static str) -> ValidationError {
    let mut e = ValidationError::new(code).with_message(message.into());
    e.add_param(crate::validation::FIELD_PARAM.into(), &field);
    e
}

/// `confirm_password` must repeat the password (skipped while it is still empty: that is
/// reported by its own `length` rule).
fn confirmation_matches(password: &str, confirm_password: &str) -> Result<(), ValidationError> {
    if !confirm_password.is_empty() && password != confirm_password {
        return Err(field_error(
            "confirm_password",
            "passwords_mismatch",
            "Passwords do not match",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[validate(schema(function = "validate_register_passwords", skip_on_field_errors = false))]
pub struct RegisterRequest {
    #[validate(
        length(min = 3, max = 50, message = "Username must be between 3 and 50 characters"),
        regex(path = *USERNAME_REGEX, message = "Username can only contain letters, numbers, and underscores")
    )]
    pub username: String,

    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(
        length(
            min = 8,
            max = 72,
            message = "Password must be between 8 and 72 characters"
        ),
        custom(function = "password_strength")
    )]
    pub password: String,

    #[validate(length(min = 1, message = "Confirm password is required"))]
    pub confirm_password: String,
}

fn validate_register_passwords(req: &RegisterRequest) -> Result<(), ValidationError> {
    confirmation_matches(&req.password, &req.confirm_password)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterResponse {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    /// `pending_verification` (spelled `pendingverification` until
    /// `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
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
    /// `pending_verification` (spelled `pendingverification` until
    /// `ACCOUNT_STATUS_FORMAT=canonical`), `active`, `inactive` or `suspended`.
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
#[validate(schema(function = "validate_reset_passwords", skip_on_field_errors = false))]
pub struct ResetPasswordRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    #[validate(length(min = 1, message = "Reset token is required"))]
    pub token: String,

    #[validate(
        length(
            min = 8,
            max = 72,
            message = "Password must be between 8 and 72 characters"
        ),
        custom(function = "password_strength")
    )]
    pub new_password: String,

    #[validate(length(min = 1, message = "Confirm password is required"))]
    pub confirm_password: String,
}

fn validate_reset_passwords(req: &ResetPasswordRequest) -> Result<(), ValidationError> {
    confirmation_matches(&req.new_password, &req.confirm_password)
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

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[validate(schema(function = "validate_change_passwords", skip_on_field_errors = false))]
pub struct ChangePasswordRequest {
    #[validate(length(min = 1, message = "Current password is required"))]
    pub current_password: String,

    #[validate(
        length(
            min = 8,
            max = 72,
            message = "New password must be between 8 and 72 characters"
        ),
        custom(function = "password_strength")
    )]
    pub new_password: String,

    #[validate(length(min = 1, message = "Confirm password is required"))]
    pub confirm_password: String,
}

/// Cross-field rules, each reported on its field: the confirmation must match
/// (`confirm_password`), then the new password must differ from the current one
/// (`new_password`).
fn validate_change_passwords(req: &ChangePasswordRequest) -> Result<(), ValidationError> {
    confirmation_matches(&req.new_password, &req.confirm_password)?;
    if !req.new_password.is_empty() && req.current_password == req.new_password {
        return Err(field_error(
            "new_password",
            "same_password",
            "New password must be different from current password",
        ));
    }
    Ok(())
}

/// One-time Google sign-in exchange (ACC-005): the callback redirect carries only a
/// short-lived, single-use `exchange_code`, never tokens.
#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct GoogleExchangeRequest {
    #[validate(length(min = 20, max = 128, message = "Invalid exchange code"))]
    pub exchange_code: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::validation::{ValidateRequest, ValidationIssue};

    const STRONG: &str = "Str0ng!pass";

    fn register(password: &str, confirm: &str) -> RegisterRequest {
        RegisterRequest {
            username: "ada_l".into(),
            email: "ada@example.com".into(),
            password: password.into(),
            confirm_password: confirm.into(),
        }
    }

    fn issues(r: Result<(), ApiError>) -> Vec<ValidationIssue> {
        match r {
            Err(ApiError::InvalidFields { details, .. }) => details,
            other => panic!("expected InvalidFields, got {other:?}"),
        }
    }

    fn on(issues: &[ValidationIssue], field: &str) -> Vec<String> {
        issues
            .iter()
            .filter(|i| i.field.as_deref() == Some(field))
            .map(|i| i.code.clone())
            .collect()
    }

    #[test]
    fn strong_passwords_are_accepted() {
        for p in [
            STRONG,
            "Aa1!aaaa",
            "Zz9?zzzzzzzz",
            &format!("Aa1!{}", "x".repeat(68)),
        ] {
            assert!(register(p, p).validate().is_ok(), "{p}");
        }
        let reset = ResetPasswordRequest {
            email: "ada@example.com".into(),
            token: "123456".into(),
            new_password: STRONG.into(),
            confirm_password: STRONG.into(),
        };
        assert!(reset.validate().is_ok());
        let change = ChangePasswordRequest {
            current_password: "0ld!Password".into(),
            new_password: STRONG.into(),
            confirm_password: STRONG.into(),
        };
        assert!(change.validate().is_ok());
    }

    #[test]
    fn weak_passwords_are_rejected_on_the_password_field_not_all() {
        // Each misses exactly one character class (length is fine).
        for weak in ["str0ng!pass", "STR0NG!PASS", "Strong!pass", "Str0ngpass1"] {
            let errors = register(weak, weak).validate().unwrap_err();
            let fe = errors.field_errors();
            assert!(!fe.contains_key("__all__"), "{weak}: {errors}");
            assert_eq!(fe["password"][0].code, "password_strength", "{weak}");

            let d = issues(register(weak, weak).validate_request());
            assert_eq!(
                d,
                vec![ValidationIssue {
                    field: Some("password".into()),
                    code: "password_strength".into(),
                    message: PASSWORD_STRENGTH_MESSAGE.into(),
                }],
                "{weak}"
            );
        }
        // Length bounds are unchanged (8..=72 characters).
        let short = "Aa1!aaa";
        assert_eq!(
            on(
                &issues(register(short, short).validate_request()),
                "password"
            ),
            ["length"]
        );
        let long = format!("Aa1!{}", "x".repeat(69));
        assert_eq!(
            on(
                &issues(register(&long, &long).validate_request()),
                "password"
            ),
            ["length"]
        );
        // Empty: both rules, still on `password`.
        let d = issues(register("", "").validate_request());
        assert_eq!(on(&d, "password"), ["length", "password_strength"]);
        assert_eq!(on(&d, "confirm_password"), ["length"]);
    }

    #[test]
    fn mismatches_belong_to_confirm_password_and_are_reported_with_strength() {
        let d = issues(register("weakpass", "other").validate_request());
        assert_eq!(on(&d, "password"), ["password_strength"]);
        assert_eq!(on(&d, "confirm_password"), ["passwords_mismatch"]);
        assert!(d.iter().all(|i| i.field.is_some()), "{d:?}");

        let reset = ResetPasswordRequest {
            email: "ada@example.com".into(),
            token: "123456".into(),
            new_password: "nouppercase1!".into(),
            confirm_password: STRONG.into(),
        };
        let d = issues(reset.validate_request());
        assert_eq!(on(&d, "new_password"), ["password_strength"]);
        assert_eq!(on(&d, "confirm_password"), ["passwords_mismatch"]);
    }

    #[test]
    fn change_password_rules_are_field_level() {
        let same = ChangePasswordRequest {
            current_password: STRONG.into(),
            new_password: STRONG.into(),
            confirm_password: STRONG.into(),
        };
        let d = issues(same.validate_request());
        assert_eq!(
            d,
            vec![ValidationIssue {
                field: Some("new_password".into()),
                code: "same_password".into(),
                message: "New password must be different from current password".into(),
            }]
        );
        let weak = ChangePasswordRequest {
            current_password: STRONG.into(),
            new_password: "alllowercase1!".into(),
            confirm_password: "alllowercase1!".into(),
        };
        assert_eq!(
            on(&issues(weak.validate_request()), "new_password"),
            ["password_strength"]
        );
    }
}
