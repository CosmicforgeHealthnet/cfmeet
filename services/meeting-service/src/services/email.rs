//! Email Service — the single entry point of every transactional email of CF Meet.
//!
//! Templates are rendered here (backend-owned), then recorded in the durable delivery
//! ledger ([`crate::services::email_delivery`]) with a category and an idempotency key;
//! the email worker delivers them through the configured provider (Resend in production).

use chrono::{DateTime, Duration, Utc};
use sea_orm::DatabaseConnection;
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
        let now = Utc::now();
        let template =
            password_changed_email(username, how, &now.format("%Y-%m-%d %H:%M").to_string());
        self.enqueue_template(
            category::SECURITY_PASSWORD_CHANGED,
            Some(format!("password-changed:{user_id}:{}", now.timestamp())),
            to_email,
            Some(username),
            "Your CF Meet password was changed",
            template,
            Some(now + Duration::days(2)),
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
