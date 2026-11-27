//! Email Service — the single entry point of every transactional email of CF Meet.
//!
//! Templates are rendered here (backend-owned), then recorded in the durable delivery
//! ledger ([`crate::services::email_delivery`]) with a category and an idempotency key;
//! the email worker delivers them through the configured provider (Resend in production).

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::EmailConfig;
use crate::error::ApiResult;
use crate::services::critical_notifications::{self as critical, CriticalEvent};
use crate::services::email_delivery::{self, category, Enqueued, NewEmail};
use crate::services::notification_fence::MeetingCategory;
use crate::services::step_up::StepUpPurpose;
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
    ///
    /// CRITICAL lane (N12, `AUTH_VERIFICATION`): routed by the fence; `LEGACY_ONLY`
    /// records exactly this ledger row. Outside it the catalogue event is
    /// `account.sign-in-code.requested`: the address may have no account yet, so it names
    /// one email principal with the address, the code and the magic-link token sealed
    /// (the service rebuilds the same link from `NOTIFICATIONS_APP_URL`).
    pub async fn send_sign_in_code(
        &self,
        to_email: &str,
        code: &str,
        link_token: Option<&str>,
        details: &RequestDetails,
    ) -> ApiResult<Uuid> {
        let link = link_token.map(|token| magic_link(&self.app_url, to_email, token));
        let template = sign_in_code_email(code, CODE_TTL_TEXT, link.as_deref(), details);
        let event = CriticalEvent::sign_in_code(
            to_email,
            code,
            link_token,
            CODE_TTL.num_minutes(),
            details,
        );
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::AUTH_VERIFICATION,
                format!(
                    "otp:signin:{}:{}",
                    email_delivery::recipient_hash(to_email),
                    digest(code)
                ),
                to_email,
                None,
                "Your CF Meet sign-in code",
                template,
                Utc::now() + CODE_TTL,
            ),
            event,
        )
        .await
        .map(|e| e.id)
    }

    /// Step-up confirmation code for `purpose`, sent to the account's current address.
    /// CRITICAL lane (N12, `SECURITY_DEVICE_STEP_UP`): routed by the fence; `LEGACY_ONLY`
    /// records exactly this ledger row. Outside it the catalogue event is
    /// `account.step-up-code.requested` (the code sealed, `purpose` as data).
    pub async fn send_step_up_code(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
        code: &str,
        purpose: StepUpPurpose,
    ) -> ApiResult<Uuid> {
        let template = step_up_code_email(username, code, CODE_TTL_TEXT, purpose.action());
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::SECURITY_DEVICE_STEP_UP,
                format!(
                    "otp:stepup:{}:{}",
                    email_delivery::recipient_hash(to_email),
                    digest(code)
                ),
                to_email,
                Some(username),
                "Confirm it's you",
                template,
                Utc::now() + CODE_TTL,
            ),
            CriticalEvent::step_up_code(user_id, purpose.as_str(), code, CODE_TTL.num_minutes()),
        )
        .await
        .map(|e| e.id)
    }

    /// Code proving ownership of the new address of an email change. CRITICAL lane (N12,
    /// `AUTH_VERIFICATION`): routed by the fence; `LEGACY_ONLY` records exactly this ledger
    /// row. Outside it the catalogue event is `account.email-change-code.requested`: the
    /// address is not (yet) the account's, so it names one email principal with the new
    /// address and the code sealed, on behalf of the account (`user_id`, the subject).
    pub async fn send_email_change_code(
        &self,
        to_new_email: &str,
        username: &str,
        user_id: Uuid,
        code: &str,
    ) -> ApiResult<Uuid> {
        let template = email_change_code_email(username, code, CODE_TTL_TEXT);
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::AUTH_VERIFICATION,
                format!(
                    "otp:emailchange:{}:{}",
                    email_delivery::recipient_hash(to_new_email),
                    digest(code)
                ),
                to_new_email,
                Some(username),
                "Confirm your new CF Meet email address",
                template,
                Utc::now() + CODE_TTL,
            ),
            CriticalEvent::email_change_code(user_id, to_new_email, code, CODE_TTL.num_minutes()),
        )
        .await
        .map(|e| e.id)
    }

    /// Someone tried to move another account to an address that already has one (at most
    /// one notice per hour per address; no code is sent, nothing changes).
    ///
    /// CRITICAL lane (`ACCOUNT_NOTICES`): routed by the fence; `LEGACY_ONLY` records exactly
    /// this ledger row. Outside it the catalogue event is `account.security.email-in-use`:
    /// one email principal with the address sealed and no subject (the event never names
    /// the account the address belongs to).
    pub async fn send_email_in_use_notice(&self, to_email: &str) -> ApiResult<Uuid> {
        let now = Utc::now();
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::NOTIFICATION,
                format!(
                    "email-in-use:{}:{}",
                    email_delivery::recipient_hash(to_email),
                    now.format("%Y%m%d%H")
                ),
                to_email,
                None,
                "Email change attempt on CF Meet",
                email_in_use_notice_email(),
                now + Duration::days(1),
            ),
            CriticalEvent::email_in_use(to_email),
        )
        .await
        .map(|e| e.id)
    }

    /// Security alert: a sign-in from a device not seen on this account recently.
    ///
    /// CRITICAL lane (`ACCOUNT_NOTICES`): routed by the fence; `LEGACY_ONLY` records exactly
    /// this ledger row. Outside it the catalogue event is `account.security.new-sign-in`
    /// (the address, the client label and the masked network sealed; `requestedAt` as
    /// data; the service rebuilds the sessions link from `NOTIFICATIONS_APP_URL`).
    pub async fn send_new_sign_in_alert(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
        session_id: Uuid,
        details: &RequestDetails,
    ) -> ApiResult<Uuid> {
        let sessions_url = sessions_link(&self.app_url);
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::NOTIFICATION,
                format!("new-sign-in:{session_id}"),
                to_email,
                Some(username),
                "New sign-in to your CF Meet account",
                new_sign_in_email(username, details, &sessions_url),
                Utc::now() + Duration::days(2),
            ),
            CriticalEvent::new_sign_in(user_id, to_email, details),
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
    ///
    /// CRITICAL lane (`ACCOUNT_NOTICES`): routed by the fence; `LEGACY_ONLY` records exactly
    /// this ledger row (`AUTH_WELCOME`, suppressible as today). Outside it the catalogue
    /// event is `account.welcome.requested` (the address sealed, the account the subject).
    pub async fn send_welcome_email(
        &self,
        to_email: &str,
        username: &str,
        user_id: Uuid,
    ) -> ApiResult<Uuid> {
        let template = welcome_email(username, &self.app_url);
        critical::enqueue(
            &self.db,
            critical::installed(),
            rendered(
                category::AUTH_WELCOME,
                format!("welcome:{user_id}"),
                to_email,
                Some(username),
                "Welcome to CF Meet",
                template,
                Utc::now() + Duration::days(2),
            ),
            welcome_event(&self.app_url, user_id, to_email),
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

/// The welcome's catalogue event. The welcome shows `APP_URL` as configured, while the
/// Notification Service renders `NOTIFICATIONS_APP_URL` without a trailing `/`: an `APP_URL`
/// ending in `/` is a link the service cannot render exactly, so that welcome stays on the
/// legacy path (`UNPUBLISHABLE`).
fn welcome_event(app_url: &str, user_id: Uuid, to_email: &str) -> CriticalEvent {
    if app_url.ends_with('/') {
        CriticalEvent::not_in_catalogue(MeetingCategory::AccountNotices)
    } else {
        CriticalEvent::welcome(user_id, to_email)
    }
}

/// The undo window as the email-changed notice words it.
pub const EMAIL_CHANGE_UNDO_WINDOW_TEXT: &str = "7 days";

/// The single-use undo link of a self-service email change (the web app's
/// `/auth/email-change/undo` page). The Notification Service builds the same link from the
/// sealed token and `NOTIFICATIONS_APP_URL` (`services/notifications/src/templates/mod.rs`).
pub fn undo_link(app_url: &str, token: &str) -> String {
    format!(
        "{}/auth/email-change/undo?token={token}",
        app_url.trim_end_matches('/')
    )
}

/// The signed-in devices page linked from security alerts (the Notification Service builds
/// the same link from `NOTIFICATIONS_APP_URL`).
pub fn sessions_link(app_url: &str) -> String {
    format!("{}/settings/sessions", app_url.trim_end_matches('/'))
}

/// Email change notices, recorded on the caller's transaction (the one that changes the
/// address), so a committed change always has both notices queued: the previous address
/// learns of it (with the undo link built from `undo_token` and `app_url`, or a support
/// note when `None`) and the new address gets a confirmation.
///
/// CRITICAL lane (`ACCOUNT_NOTICES`): each notice is routed by the fence; `LEGACY_ONLY`
/// records exactly these ledger rows. Outside it each is an `account.security.email-changed`
/// event (`side` OLD / NEW) to one email principal with its address sealed, on behalf of the
/// account; the OLD notice also seals the masked new address and the undo token, never in
/// clear.
#[allow(clippy::too_many_arguments)]
pub async fn email_changed_notices_on<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    old_email: &str,
    new_email: &str,
    username: &str,
    user_id: Uuid,
    change_id: Uuid,
    app_url: &str,
    undo_token: Option<&str>,
) -> ApiResult<()> {
    let now = Utc::now();
    let masked = mask_email(new_email);
    let undo_url = undo_token.map(|t| undo_link(app_url, t));
    let old = email_changed_old_address_email(
        username,
        &masked,
        &now.format("%Y-%m-%d %H:%M").to_string(),
        undo_url.as_deref(),
        EMAIL_CHANGE_UNDO_WINDOW_TEXT,
    );
    critical::enqueue(
        conn,
        critical::installed(),
        rendered(
            category::NOTIFICATION,
            format!("email-changed:old:{change_id}"),
            old_email,
            Some(username),
            "Your CF Meet email address was changed",
            old,
            now + EMAIL_CHANGE_UNDO_WINDOW,
        ),
        CriticalEvent::email_changed_old(user_id, old_email, &masked, undo_token, now),
    )
    .await?;
    let new = email_changed_new_address_email(username);
    critical::enqueue(
        conn,
        critical::installed(),
        rendered(
            category::NOTIFICATION,
            format!("email-changed:new:{change_id}"),
            new_email,
            Some(username),
            "Your CF Meet email address was updated",
            new,
            now + Duration::days(2),
        ),
        CriticalEvent::email_changed_new(user_id, new_email, now),
    )
    .await?;
    Ok(())
}

/// One rendered legacy email of the CRITICAL lane (the row `LEGACY_ONLY` records; the
/// same fields `EmailService::enqueue_template` writes).
fn rendered(
    category: &'static str,
    key: String,
    to: &str,
    to_name: Option<&str>,
    subject: &str,
    template: EmailTemplate,
    expires_at: DateTime<Utc>,
) -> NewEmail {
    NewEmail {
        category,
        idempotency_key: Some(key),
        to: to.to_string(),
        to_name: to_name.map(str::to_string),
        subject: subject.to_string(),
        html: template.html,
        text: template.text,
        expires_at: Some(expires_at),
    }
}

/// N0-10 (S-9): a device security notice recorded on the caller's transaction, with the
/// deterministic key `device-linked:<link id>` (one notice per completed link). CRITICAL
/// lane (N12): routed by the `SECURITY_DEVICE_LINKED` fence mode ([`critical::enqueue`]).
#[allow(clippy::too_many_arguments)]
pub async fn device_notice_on<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    to_email: &str,
    username: &str,
    user_id: Uuid,
    device_id: Uuid,
    link_id: Uuid,
    subject: &str,
    message: &str,
) -> ApiResult<Uuid> {
    let template = notification_email(username, subject, message);
    critical::enqueue(
        conn,
        critical::installed(),
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
        CriticalEvent::device_linked(user_id, device_id, link_id),
    )
    .await
    .map(|e| e.id)
}

/// The device step-up code email (`account_devices`), recorded on the caller's
/// transaction with the key `device-step-up:<link id>:<code digest prefix>`. CRITICAL lane
/// (N12): routed by the `SECURITY_DEVICE_STEP_UP` fence mode ([`critical::enqueue`]); the
/// catalogue event `account.device-step-up-code.requested` carries the sealed code.
#[allow(clippy::too_many_arguments)]
pub async fn device_step_up_code_on<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    to_email: &str,
    username: &str,
    user_id: Uuid,
    device_id: Uuid,
    key: String,
    code: &str,
    template: EmailTemplate,
    expires_at: DateTime<Utc>,
) -> ApiResult<Enqueued> {
    let ttl_minutes = (expires_at - Utc::now()).num_minutes().max(1);
    critical::enqueue(
        conn,
        critical::installed(),
        NewEmail {
            category: category::SECURITY_DEVICE_STEP_UP,
            idempotency_key: Some(key),
            to: to_email.to_string(),
            to_name: Some(username.to_string()),
            subject: "Your CF Meet device verification code".into(),
            html: template.html,
            text: template.text,
            expires_at: Some(expires_at),
        },
        CriticalEvent::device_step_up_code(user_id, device_id, code, ttl_minutes),
    )
    .await
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

/// The sign-in magic link (the Notification Service builds the same link from the sealed
/// token and address, `services/notifications/src/templates/mod.rs`).
fn magic_link(app_url: &str, to_email: &str, token: &str) -> String {
    format!(
        "{}/auth/magic?email={}&token={token}",
        app_url.trim_end_matches('/'),
        url_encode(to_email)
    )
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

    /// SHA-256 over subject, NUL, text, NUL, html, NUL of the sign-in email for a fixed
    /// input. The Notification Service's renderer test (`templates::tests::
    /// SIGN_IN_PARITY_DIGEST`) asserts the same value for the same input, so a user sees
    /// the same email whichever path sends the code.
    const SIGN_IN_PARITY_DIGEST: &str =
        "611a75c830404a0965fb9a6feaa363b5b70ce86783b3b9ebc4882cf557919d53";

    #[test]
    fn the_sign_in_email_matches_the_notification_service_rendering() {
        let link = magic_link(
            "https://app.cfmeet.com/",
            "Ada+cf@Example.com",
            "tok_0123456789abcdefghij",
        );
        assert_eq!(
            link,
            "https://app.cfmeet.com/auth/magic?email=Ada%2Bcf%40Example.com&token=tok_0123456789abcdefghij"
        );
        let t = sign_in_code_email(
            "482913",
            CODE_TTL_TEXT,
            Some(&link),
            &RequestDetails {
                client: Some("TestBrowser/1.0".into()),
                address: Some("102.89.x.x".into()),
                when_utc: "2026-11-17 12:00".into(),
            },
        );
        let mut h = Sha256::new();
        for part in ["Your CF Meet sign-in code", &t.text, &t.html] {
            h.update(part.as_bytes());
            h.update([0u8]);
        }
        assert_eq!(hex::encode(h.finalize()), SIGN_IN_PARITY_DIGEST);
        assert_eq!(CODE_TTL_TEXT, "10 minutes");
        assert_eq!(CODE_TTL.num_minutes(), 10);
    }

    /// SHA-256 over subject, NUL, text, NUL, html, NUL of each account notice (`ACCOUNT_
    /// NOTICES`) for the fixed inputs below, in this order: welcome, new sign-in, email in
    /// use, email changed (previous address, self-service with the undo link), email
    /// changed (previous address, support-assisted), email changed (new address). The
    /// Notification Service's renderer test (`templates::tests::
    /// ACCOUNT_NOTICES_PARITY_DIGEST`) asserts the same value for the same input, so a user
    /// sees the same notice whichever path sends it.
    const ACCOUNT_NOTICES_PARITY_DIGEST: &str =
        "4f7c7439dacb3c0316d1809bb34b54c50949d0f95662bf6f290bacb214a8d28d";

    #[test]
    fn the_account_notices_match_the_notification_service_rendering() {
        let app_url = "https://app.cfmeet.com/";
        // Built, not written after `token=` (the repository secret scan reads that as a credential).
        let undo_fixture = "tok_0123456789abcdefghij";
        let undo = undo_link(app_url, undo_fixture);
        assert_eq!(
            undo,
            format!("https://app.cfmeet.com/auth/email-change/undo?token={undo_fixture}")
        );
        assert_eq!(
            sessions_link(app_url),
            "https://app.cfmeet.com/settings/sessions"
        );
        let masked = mask_email("nadia.new@example.com");
        assert_eq!(masked, "n***@example.com");
        let details = RequestDetails {
            client: Some("TestBrowser/1.0".into()),
            address: Some("102.89.x.x".into()),
            when_utc: "2026-11-17 12:00".into(),
        };
        let notices = [
            ("Welcome to CF Meet", welcome_email("ada", app_url)),
            (
                "New sign-in to your CF Meet account",
                new_sign_in_email("ada", &details, &sessions_link(app_url)),
            ),
            (
                "Email change attempt on CF Meet",
                email_in_use_notice_email(),
            ),
            (
                "Your CF Meet email address was changed",
                email_changed_old_address_email(
                    "ada",
                    &masked,
                    "2026-11-17 12:00",
                    Some(&undo),
                    EMAIL_CHANGE_UNDO_WINDOW_TEXT,
                ),
            ),
            (
                "Your CF Meet email address was changed",
                email_changed_old_address_email(
                    "ada",
                    &masked,
                    "2026-11-17 12:00",
                    None,
                    EMAIL_CHANGE_UNDO_WINDOW_TEXT,
                ),
            ),
            (
                "Your CF Meet email address was updated",
                email_changed_new_address_email("ada"),
            ),
        ];
        let mut h = Sha256::new();
        for (subject, t) in &notices {
            for part in [*subject, &t.text, &t.html] {
                h.update(part.as_bytes());
                h.update([0u8]);
            }
        }
        assert_eq!(hex::encode(h.finalize()), ACCOUNT_NOTICES_PARITY_DIGEST);
        // The service trims NOTIFICATIONS_APP_URL: a welcome showing an APP_URL with a
        // trailing slash is not the catalogue's email and stays legacy.
        let id = Uuid::new_v4();
        assert_eq!(
            welcome_event("https://app.cfmeet.com", id, "a@x.com").event_type,
            Some(critical::event_type::WELCOME)
        );
        assert_eq!(welcome_event(app_url, id, "a@x.com").event_type, None);
        assert_eq!(EMAIL_CHANGE_UNDO_WINDOW_TEXT, "7 days");
        assert_eq!(EMAIL_CHANGE_UNDO_WINDOW.num_days(), 7);
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
    /// The owner signed back in to an account they had deactivated (gap audit A16).
    Reactivated,
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
            SecurityNotice::Reactivated => "reactivated",
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
                "Your CF Meet account was just deactivated and every sign-in was ended. You can sign in again within 30 days to reactivate it. Contact support if you did not do this.",
            ),
            SecurityNotice::Reactivated => (
                "Your CF Meet account was reactivated",
                "You signed in to your deactivated CF Meet account, so it is active again. Contact support if this was not you.",
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
