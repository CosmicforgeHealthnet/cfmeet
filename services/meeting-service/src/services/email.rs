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
    email_change_code_email, email_changed_new_address_email, email_changed_old_address_email,
    email_in_use_notice_email, new_sign_in_email, notification_email, security_notice_email,
    sign_in_code_email, step_up_code_email, welcome_email, EmailTemplate, RequestDetails,
};

/// Every one-time code is valid 10 minutes (`services::otp::CODE_TTL_SECS`).
pub const CODE_TTL: Duration = Duration::minutes(10);
const CODE_TTL_TEXT: &str = "10 minutes";

/// How long the previous address can undo a self-service email change.
pub const EMAIL_CHANGE_UNDO_WINDOW: Duration = Duration::days(7);

/// `n***@example.com`: enough for the owner to recognise an address, not to harvest it.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".to_string(),
    }
}

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

    pub fn app_url(&self) -> &str {
        &self.app_url
    }

    /// Passwordless sign-in code, with an optional magic link (`link_token`, which the
    /// web app's `/auth/magic` page POSTs to `/api/v1/auth/otp/verify-link`).
    pub async fn send_sign_in_code(
        &self,
        to_email: &str,
        code: &str,
        link_token: Option<&str>,
        details: &RequestDetails,
    ) -> ApiResult<Uuid> {
        let link = link_token.map(|token| {
            format!(
                "{}/auth/magic?email={}&token={token}",
                self.app_url.trim_end_matches('/'),
                url_encode(to_email)
            )
        });
        let template = sign_in_code_email(code, CODE_TTL_TEXT, link.as_deref(), details);
        self.enqueue_template(
            category::AUTH_VERIFICATION,
            Some(format!(
                "otp:signin:{}:{}",
                email_delivery::recipient_hash(to_email),
                digest(code)
            )),
            to_email,
            None,
            "Your CF Meet sign-in code",
            template,
            Some(Utc::now() + CODE_TTL),
        )
        .await
        .map(|e| e.id)
    }

    /// Step-up confirmation code sent to the account's current address.
    pub async fn send_step_up_code(
        &self,
        to_email: &str,
        username: &str,
        code: &str,
        action: &str,
    ) -> ApiResult<Uuid> {
        let template = step_up_code_email(username, code, CODE_TTL_TEXT, action);
        self.enqueue_template(
            category::SECURITY_DEVICE_STEP_UP,
            Some(format!(
                "otp:stepup:{}:{}",
                email_delivery::recipient_hash(to_email),
                digest(code)
            )),
            to_email,
            Some(username),
            "Confirm it's you",
            template,
            Some(Utc::now() + CODE_TTL),
        )
        .await
        .map(|e| e.id)
    }

    /// Code proving ownership of the new address of an email change.
    pub async fn send_email_change_code(
        &self,
        to_new_email: &str,
        username: &str,
        code: &str,
    ) -> ApiResult<Uuid> {
        let template = email_change_code_email(username, code, CODE_TTL_TEXT);
        self.enqueue_template(
            category::AUTH_VERIFICATION,
            Some(format!(
                "otp:emailchange:{}:{}",
                email_delivery::recipient_hash(to_new_email),
                digest(code)
            )),
            to_new_email,
            Some(username),
            "Confirm your new CF Meet email address",
            template,
            Some(Utc::now() + CODE_TTL),
        )
        .await
        .map(|e| e.id)
    }

    /// Someone tried to move another account to an address that already has one (at most
    /// one notice per hour per address; no code is sent, nothing changes).
    pub async fn send_email_in_use_notice(&self, to_email: &str) -> ApiResult<Uuid> {
        let now = Utc::now();
        self.enqueue_template(
            category::NOTIFICATION,
            Some(format!(
                "email-in-use:{}:{}",
                email_delivery::recipient_hash(to_email),
                now.format("%Y%m%d%H")
            )),
            to_email,
            None,
            "Email change attempt on CF Meet",
            email_in_use_notice_email(),
            Some(now + Duration::days(1)),
        )
        .await
        .map(|e| e.id)
    }

    /// Security alert: a sign-in from a device not seen on this account recently.
    pub async fn send_new_sign_in_alert(
        &self,
        to_email: &str,
        username: &str,
        session_id: Uuid,
        details: &RequestDetails,
    ) -> ApiResult<Uuid> {
        let sessions_url = format!("{}/settings/sessions", self.app_url.trim_end_matches('/'));
        self.enqueue_template(
            category::NOTIFICATION,
            Some(format!("new-sign-in:{session_id}")),
            to_email,
            Some(username),
            "New sign-in to your CF Meet account",
            new_sign_in_email(username, details, &sessions_url),
            Some(Utc::now() + Duration::days(2)),
        )
        .await
        .map(|e| e.id)
    }

    /// Gap audit A13: one security notice (`dedupe` makes a retried event send once).
    pub async fn send_security_notice(
        &self,
        to_email: &str,
        username: &str,
        notice: SecurityNotice,
        dedupe: &str,
    ) -> ApiResult<Uuid> {
        let sessions_url = format!("{}/settings/sessions", self.app_url.trim_end_matches('/'));
        let (subject, what) = notice.text();
        self.enqueue_template(
            category::NOTIFICATION,
            Some(format!("security-notice:{}:{dedupe}", notice.key())),
            to_email,
            Some(username),
            subject,
            security_notice_email(username, subject, what, &sessions_url),
            Some(Utc::now() + Duration::days(2)),
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

/// Email change notices, recorded on the caller's transaction (the one that changes the
/// address), so a committed change always has both notices queued: the previous address
/// learns of it (with the undo link `undo_url`, or a support note when `None`) and the
/// new address gets a confirmation.
#[allow(clippy::too_many_arguments)]
pub async fn email_changed_notices_on<C: ConnectionTrait>(
    conn: &C,
    old_email: &str,
    new_email: &str,
    username: &str,
    change_id: Uuid,
    undo_url: Option<&str>,
) -> ApiResult<()> {
    let now = Utc::now();
    let old = email_changed_old_address_email(
        username,
        &mask_email(new_email),
        &now.format("%Y-%m-%d %H:%M").to_string(),
        undo_url,
        "7 days",
    );
    email_delivery::enqueue(
        conn,
        NewEmail {
            category: category::NOTIFICATION,
            idempotency_key: Some(format!("email-changed:old:{change_id}")),
            to: old_email.to_string(),
            to_name: Some(username.to_string()),
            subject: "Your CF Meet email address was changed".to_string(),
            html: old.html,
            text: old.text,
            expires_at: Some(now + EMAIL_CHANGE_UNDO_WINDOW),
        },
    )
    .await?;
    let new = email_changed_new_address_email(username);
    email_delivery::enqueue(
        conn,
        NewEmail {
            category: category::NOTIFICATION,
            idempotency_key: Some(format!("email-changed:new:{change_id}")),
            to: new_email.to_string(),
            to_name: Some(username.to_string()),
            subject: "Your CF Meet email address was updated".to_string(),
            html: new.html,
            text: new.text,
            expires_at: Some(now + Duration::days(2)),
        },
    )
    .await?;
    Ok(())
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

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod passwordless_tests {
    use super::*;

    #[test]
    fn addresses_are_masked_and_query_values_encoded() {
        assert_eq!(mask_email("nadia@example.com"), "n***@example.com");
        assert_eq!(mask_email("broken"), "***");
        assert_eq!(url_encode("a+b@x.com"), "a%2Bb%40x.com");
    }
}

/// Gap audit A13: security events the account owner is told of by email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityNotice {
    /// A Google identity was linked to the account.
    GoogleLinked,
    /// A replayed refresh token ended a sign-in (a copy of it was used elsewhere).
    RefreshTokenReuse,
    /// Every sign-in of the account was ended.
    SignedOutEverywhere,
    /// The account was deactivated.
    Deactivated,
    /// The first device of the account became trusted (gap audit A6).
    FirstDeviceTrusted,
    /// The account was deleted (gap audit A2).
    Deleted,
    /// Two-step sign-in was turned on (gap audit A1).
    MfaEnabled,
    /// Two-step sign-in was turned off (gap audit A1).
    MfaDisabled,
}

impl SecurityNotice {
    pub fn key(self) -> &'static str {
        match self {
            SecurityNotice::GoogleLinked => "google-linked",
            SecurityNotice::RefreshTokenReuse => "refresh-reuse",
            SecurityNotice::SignedOutEverywhere => "signed-out-everywhere",
            SecurityNotice::Deactivated => "deactivated",
            SecurityNotice::FirstDeviceTrusted => "first-device-trusted",
            SecurityNotice::Deleted => "deleted",
            SecurityNotice::MfaEnabled => "mfa-enabled",
            SecurityNotice::MfaDisabled => "mfa-disabled",
        }
    }

    /// Subject and the one-sentence account of what happened.
    pub fn text(self) -> (&'static str, &'static str) {
        match self {
            SecurityNotice::GoogleLinked => (
                "Google sign-in was added to your CF Meet account",
                "A Google account was just linked to your CF Meet account, so it can now be used to sign in.",
            ),
            SecurityNotice::RefreshTokenReuse => (
                "A CF Meet sign-in was ended for your protection",
                "A saved sign-in of your CF Meet account was used from two places at once, which can mean it was copied. We ended that sign-in.",
            ),
            SecurityNotice::SignedOutEverywhere => (
                "You were signed out of CF Meet everywhere",
                "Every sign-in of your CF Meet account was just ended, on every device.",
            ),
            SecurityNotice::Deactivated => (
                "Your CF Meet account was deactivated",
                "Your CF Meet account was just deactivated and every sign-in was ended. Contact support if you did not do this.",
            ),
            SecurityNotice::FirstDeviceTrusted => (
                "A device is now trusted on your CF Meet account",
                "The first device of your CF Meet account was just set up as a trusted device. Trusted devices can approve new devices and sign out others.",
            ),
            SecurityNotice::Deleted => (
                "Your CF Meet account was deleted",
                "Your CF Meet account was just deleted: your profile and sign-ins were removed and this address is no longer linked to it. Contact support at once if you did not do this.",
            ),
            SecurityNotice::MfaEnabled => (
                "Two-step sign-in was turned on for your CF Meet account",
                "Signing in to your CF Meet account now also needs a code from your authenticator app (or a recovery code).",
            ),
            SecurityNotice::MfaDisabled => (
                "Two-step sign-in was turned off for your CF Meet account",
                "Signing in to your CF Meet account no longer needs a code from your authenticator app.",
            ),
        }
    }
}
