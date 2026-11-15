//! Email Service — the single entry point of every transactional email of CF Meet.
//!
//! Templates are rendered here (backend-owned), then recorded in the durable delivery
//! ledger ([`crate::services::email_delivery`]) with a category and an idempotency key;
//! the email worker delivers them through the configured provider (Resend in production).

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::EmailConfig;
use crate::error::ApiResult;
use crate::services::email_delivery::{self, category, Enqueued, NewEmail};
use crate::templates::email::{
    notification_email, password_changed_email, password_reset_email, verification_email,
    welcome_email, EmailTemplate,
};

/// Verification codes are valid 24 hours (`services::otp`), reset codes 1 hour.
pub const VERIFICATION_TTL: Duration = Duration::hours(24);
pub const RESET_TTL: Duration = Duration::hours(1);

/// High-level email service for the application.
#[derive(Clone)]
pub struct EmailService {
    db: DatabaseConnection,
    app_url: String,
}

/// Digest used in idempotency keys so codes never appear in the ledger key.
fn digest(value: &str) -> String {
    hex::encode(&Sha256::digest(value.as_bytes())[..12])
}

impl EmailService {
    pub fn new(db: DatabaseConnection, config: &EmailConfig) -> Self {
        Self {
            db,
            app_url: config.app_url.clone(),
        }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Record one rendered message (idempotent on `key`).
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_template(
        &self,
        category: &'static str,
        key: Option<String>,
        to: &str,
        to_name: Option<&str>,
        subject: &str,
        template: EmailTemplate,
        expires_at: Option<DateTime<Utc>>,
    ) -> ApiResult<Enqueued> {
        email_delivery::enqueue(
            &self.db,
            NewEmail {
                category,
                idempotency_key: key,
                to: to.to_string(),
                to_name: to_name.map(str::to_string),
                subject: subject.to_string(),
                html: template.html,
                text: template.text,
                expires_at,
            },
        )
        .await
    }

    /// Registration / resend verification code.
    pub async fn send_verification_email(
        &self,
        to_email: &str,
        username: &str,
        verification_code: &str,
    ) -> ApiResult<Uuid> {
        let template = verification_email(username, verification_code, "24 hours");
        self.enqueue_template(
            category::AUTH_VERIFICATION,
            Some(format!(
                "otp:verify:{}:{}",
                email_delivery::recipient_hash(to_email),
                digest(verification_code)
            )),
            to_email,
            Some(username),
            "Your CF Meet verification code",
            template,
            Some(Utc::now() + VERIFICATION_TTL),
        )
        .await
        .map(|e| e.id)
    }

    /// Password reset code.
    pub async fn send_password_reset_email(
        &self,
        to_email: &str,
        username: &str,
        reset_code: &str,
    ) -> ApiResult<Uuid> {
        let template = password_reset_email(username, reset_code, "1 hour");
        self.enqueue_template(
            category::AUTH_PASSWORD_RESET,
            Some(format!(
                "otp:reset:{}:{}",
                email_delivery::recipient_hash(to_email),
                digest(reset_code)
            )),
            to_email,
            Some(username),
            "Reset your CF Meet password",
            template,
            Some(Utc::now() + RESET_TTL),
        )
        .await
        .map(|e| e.id)
    }

    /// Welcome email (once per account).
    pub async fn send_welcome_email(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
    ) -> ApiResult<Uuid> {
        let template = welcome_email(username, &self.app_url);
        self.enqueue_template(
            category::AUTH_WELCOME,
            Some(format!("welcome:{user_id}")),
            to_email,
            Some(username),
            "Welcome to CF Meet",
            template,
            Some(Utc::now() + Duration::days(2)),
        )
        .await
        .map(|e| e.id)
    }

    /// Security notice after a password change or reset.
    pub async fn send_password_changed_email(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
        how: &str,
    ) -> ApiResult<Uuid> {
        password_changed_notice_on(&self.db, to_email, username, user_id, how).await
    }

    /// D-12: someone tried to register with an address that already has an account. The
    /// owner is told (at most once per hour per account); nothing about the account changes.
    pub async fn send_registration_attempt_notice(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
    ) -> ApiResult<Uuid> {
        let now = Utc::now();
        let template = notification_email(
            username,
            "Someone tried to create a CF Meet account with your email",
            "Someone tried to create a new CF Meet account with this email address. You already \
             have an account: sign in, or use \"Forgot password\" if you no longer know your \
             password. If this was not you, you can ignore this email; your account was not changed.",
        );
        self.enqueue_template(
            category::NOTIFICATION,
            Some(format!(
                "register-existing:{user_id}:{}",
                now.format("%Y%m%d%H")
            )),
            to_email,
            Some(username),
            "Someone tried to create a CF Meet account with your email",
            template,
            Some(now + Duration::days(1)),
        )
        .await
        .map(|e| e.id)
    }

    /// Generic notification email.
    pub async fn send_notification_email(
        &self,
        to_email: &str,
        username: &str,
        subject: &str,
        message: &str,
    ) -> ApiResult<Uuid> {
        let template = notification_email(username, subject, message);
        self.enqueue_template(
            category::NOTIFICATION,
            None,
            to_email,
            Some(username),
            subject,
            template,
            None,
        )
        .await
        .map(|e| e.id)
    }
}

/// N0-10 (S-9): the password-changed security notice, recorded on the caller's connection
/// (the transaction that changes the password), so it is committed with the change.
pub async fn password_changed_notice_on<C: ConnectionTrait>(
    conn: &C,
    to_email: &str,
    username: &str,
    user_id: Uuid,
    how: &str,
) -> ApiResult<Uuid> {
    let now = Utc::now();
    let template = password_changed_email(username, how, &now.format("%Y-%m-%d %H:%M").to_string());
    email_delivery::enqueue(
        conn,
        NewEmail {
            category: category::SECURITY_PASSWORD_CHANGED,
            idempotency_key: Some(format!("password-changed:{user_id}:{}", now.timestamp())),
            to: to_email.to_string(),
            to_name: Some(username.to_string()),
            subject: "Your CF Meet password was changed".to_string(),
            html: template.html,
            text: template.text,
            expires_at: Some(now + Duration::days(2)),
        },
    )
    .await
    .map(|e| e.id)
}

/// N0-10 (S-9): a device security notice recorded on the caller's transaction, with the
/// deterministic key `device-linked:<link id>` (one notice per completed link).
pub async fn device_notice_on<C: ConnectionTrait>(
    conn: &C,
    to_email: &str,
    username: &str,
    link_id: Uuid,
    subject: &str,
    message: &str,
) -> ApiResult<Uuid> {
    let template = notification_email(username, subject, message);
    email_delivery::enqueue(
        conn,
        NewEmail {
            category: category::NOTIFICATION,
            idempotency_key: Some(format!("device-linked:{link_id}")),
            to: to_email.to_string(),
            to_name: Some(username.to_string()),
            subject: subject.to_string(),
            html: template.html,
            text: template.text,
            expires_at: Some(Utc::now() + Duration::days(2)),
        },
    )
    .await
    .map(|e| e.id)
}

/// A security notice must never block the security change itself because the stored
/// address is malformed (nothing could ever be sent to it): that case is logged and
/// skipped. Every other failure (database) aborts the transaction with the change.
pub fn tolerate_unsendable(result: ApiResult<Uuid>, user_id: Uuid, what: &str) -> ApiResult<()> {
    match result {
        Ok(_) => Ok(()),
        Err(crate::error::ApiError::BadRequest(_)) => {
            tracing::warn!(user_id = %user_id, notice = what, "security notice not queued: address unusable");
            Ok(())
        }
        Err(e) => Err(e),
    }
}
