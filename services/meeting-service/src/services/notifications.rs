//! Meeting and organization notifications (Blueprint v1.0 Section 11: MTG-006, ACC-020).
//!
//! **Reuse.** Email goes through the unified delivery ledger (`EmailService` →
//! `email_deliveries` → email worker → Resend/SMTP); no separate notification transport.
//! Everything is OFF unless `NOTIFICATIONS_EMAIL_ENABLED=true`.
//!
//! **What is sent.**
//! * Meeting invitation emails, on request (`sendEmail: true` on an email-bound v2
//!   invitation). The link carries the one-time token in the URL fragment
//!   (`{APP_URL}/invite#<token>`), which browsers never send to a server. The token is never
//!   stored, so this email is produced synchronously after the invitation commits.
//! * Organization invitation emails, on request (`sendEmail: true` on the first-party relay
//!   of the Organizations Service). Organizations stays the only owner of the invitation;
//!   Meeting only delivers the email from the authoritative answer of the signed call
//!   (ADR 0056: no organization data is written here).
//! * Reschedule and cancellation notices — durable jobs (`meeting_notifications`) written in
//!   the same transaction as the change.
//! * Reminders — one job per conference and scheduled start, created by the worker when the
//!   start is `NOTIFICATIONS_REMINDER_LEAD_MINUTES` away (a reschedule gets a new reminder).
//!
//! **Recipients** (resolved at delivery): the space owner, active members (account or
//! email-bound), active unexhausted email-bound invitations for the space or that
//! conference, and registered webinar attendees with an account — never the person who made
//! the change, never inactive, suspended or guest accounts, at most
//! `NOTIFICATIONS_MAX_RECIPIENTS`. Times are shown in the recipient's profile time zone
//! (Section 9), otherwise the meeting's zone. HEALTHCARE / HIGH_SECURITY / appointment
//! titles become "CF Meet meeting" (same rule as the calendar files).
//!
//! **Preferences (ACC-020).** The recipient's Communications Hub preference
//! `meetingInvitations` governs every meeting email (invitations, changes, reminders). It
//! is read through a signed call; when Communications is configured but unreachable a job
//! is retried later (never sent against an unknown preference) and an invitation email is
//! reported `FAILED`. Without Communications no preference exists (everything allowed).
//! Organization invitations are transactional (explicitly requested by an administrator)
//! and are not covered by a Hub preference category.
//!
//! **Idempotency.** `dedupe_key` per job; a per-recipient ledger keyed by the SHA-256 of
//! the address, written before the email is queued, so a retried or re-claimed job never
//! emails the same person twice (at-most-once per recipient and job).
//!
//! **Routing fence (Notification Service roadmap N7 / N11, Document 4 §9).** Every place
//! that writes a legacy row of `MEETING_INVITATION`, `MEETING_NOTIFICATION` or
//! `ORGANIZATION_INVITATION` first reads the category's mode
//! ([`crate::services::notification_fence::current_mode`]). `LEGACY_ONLY` (or no fence
//! history) runs exactly today's code. Otherwise one transaction records the sticky
//! decision and writes the legacy row (`OLD`; plus one shadow publication in `SHADOW`) or
//! the Notification Service publication only (`NEW`,
//! [`crate::services::notification_publications`]). Decision keys are today's ledger
//! keys: `meeting-invitation:<invitation>`, `org-invitation:<invitation>:<n>` (the email's
//! ledger idempotency key once fenced) and the job's `dedupe_key` (`rescheduled:…`,
//! `cancelled:…`, `reminder:<conference>:<start>`; its per-recipient emails are covered by
//! the job's decision). A notification whose event cannot be published stays `OLD`
//! (`RECIPIENT_LIMIT` / `UNPUBLISHABLE`), never dropped.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use cf_notification_client::{SealedPlaintext, SealingKey};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait,
    FromQueryResult, Statement, TransactionTrait,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    config::notifications::NotificationConfig,
    domain::{AccessPolicy, ConferenceStatus, MeetingType, SecurityProfile, SpaceStatus},
    error::{ApiError, ApiResult},
    models::{conference_sessions, meeting_invitations, meeting_spaces},
    services::email::EmailService,
    services::notification_fence::{self as fence, DecisionReason, MeetingCategory, RoutingMode},
    services::notification_publications::{self as publications, event_type, EventDraft},
    services::ops_metrics::{inc, OPS_METRICS as OM},
    templates::email::meeting::{render, NotificationBody},
};

pub const KIND_RESCHEDULED: &str = "CONFERENCE_RESCHEDULED";
pub const KIND_CANCELLED: &str = "CONFERENCE_CANCELLED";
pub const KIND_REMINDER: &str = "CONFERENCE_REMINDER";

/// Delivery attempts before a job is marked FAILED.
pub const MAX_ATTEMPTS: i32 = 8;
const LEASE_SECS: i64 = 120;
const RETENTION_DAYS: i64 = 30;
const REMINDER_SCAN_LIMIT: i64 = 200;

// ---------------------------------------------------------------------------
// Transport (the existing email queue)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingEmail {
    /// Ledger category (`email_delivery::category`).
    pub category: &'static str,
    /// Ledger idempotency key (`None`: every send is a new message).
    pub idempotency_key: Option<String>,
    pub to: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    /// Gap audit M14: RFC 5545 calendar invite, attached as `invite.ics`.
    pub calendar: Option<String>,
}

/// Why an email was not handed to the delivery queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailError {
    /// The address can never be sent to (refused by the ledger's address check).
    InvalidAddress,
    /// The recipient is suppressed after a complaint or repeated bounces (N0-12).
    Suppressed,
    /// The queue is unavailable: retry later.
    Unavailable,
}

impl MailError {
    pub fn as_str(self) -> &'static str {
        match self {
            MailError::InvalidAddress => "INVALID_ADDRESS",
            MailError::Suppressed => "SUPPRESSED",
            MailError::Unavailable => "MAIL_QUEUE_UNAVAILABLE",
        }
    }
}

#[async_trait]
pub trait NotificationMail: Send + Sync {
    /// Hand one email to the delivery queue.
    async fn send(&self, email: OutgoingEmail) -> Result<(), MailError>;

    /// Hand one email to the delivery queue on the caller's transaction — the routed
    /// (fenced) path, where the ledger row is written with its routing decision (Document 4
    /// §9.3). Transports without a ledger (tests) send as [`NotificationMail::send`].
    async fn send_in(
        &self,
        txn: &DatabaseTransaction,
        email: OutgoingEmail,
    ) -> Result<(), MailError> {
        let _ = txn;
        self.send(email).await
    }
}

async fn enqueue_mail<C: ConnectionTrait>(conn: &C, email: OutgoingEmail) -> Result<(), MailError> {
    crate::services::email_delivery::enqueue_with_calendar(
        conn,
        crate::services::email_delivery::NewEmail {
            category: email.category,
            idempotency_key: email.idempotency_key,
            to: email.to,
            to_name: email.to_name,
            subject: email.subject,
            html: email.html,
            text: email.text,
            expires_at: None,
        },
        email.calendar,
    )
    .await
    .map(|_| ())
    .map_err(|e| match e {
        ApiError::BadRequest(_) => MailError::InvalidAddress,
        ApiError::Conflict(m) if m == crate::services::email_delivery::RECIPIENT_SUPPRESSED => {
            MailError::Suppressed
        }
        _ => MailError::Unavailable,
    })
}

#[async_trait]
impl NotificationMail for EmailService {
    async fn send(&self, email: OutgoingEmail) -> Result<(), MailError> {
        enqueue_mail(self.db(), email).await
    }

    async fn send_in(
        &self,
        txn: &DatabaseTransaction,
        email: OutgoingEmail,
    ) -> Result<(), MailError> {
        enqueue_mail(txn, email).await
    }
}

/// In-process transport for tests: records every email, can simulate a queue outage, and
/// applies the ledger's address check and a test suppression list.
#[derive(Default)]
pub struct RecordingMail {
    sent: Mutex<Vec<OutgoingEmail>>,
    failing: AtomicBool,
    suppressed: Mutex<HashSet<String>>,
}

impl RecordingMail {
    pub fn sent(&self) -> Vec<OutgoingEmail> {
        self.sent.lock().expect("recording mail").clone()
    }
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
    /// Refuse mail to `address` as the ledger refuses a suppressed recipient.
    pub fn suppress(&self, address: &str) {
        self.suppressed
            .lock()
            .expect("recording mail")
            .insert(address.trim().to_ascii_lowercase());
    }
}

#[async_trait]
impl NotificationMail for RecordingMail {
    async fn send(&self, email: OutgoingEmail) -> Result<(), MailError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(MailError::Unavailable);
        }
        if !crate::config::email::valid_address(email.to.trim()) {
            return Err(MailError::InvalidAddress);
        }
        if self
            .suppressed
            .lock()
            .expect("recording mail")
            .contains(&email.to.trim().to_ascii_lowercase())
        {
            return Err(MailError::Suppressed);
        }
        self.sent.lock().expect("recording mail").push(email);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Preferences (Communications Hub `meetingInvitations`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreferencesUnavailable;

#[async_trait]
pub trait PreferenceSource: Send + Sync {
    /// The accounts among `users` that turned meeting notifications off.
    async fn meeting_opt_outs(
        &self,
        users: &[Uuid],
    ) -> Result<HashSet<Uuid>, PreferencesUnavailable>;
}

/// Communications is not configured: no notification preferences exist.
pub struct NoPreferenceStore;

#[async_trait]
impl PreferenceSource for NoPreferenceStore {
    async fn meeting_opt_outs(&self, _: &[Uuid]) -> Result<HashSet<Uuid>, PreferencesUnavailable> {
        Ok(HashSet::new())
    }
}

/// Fixed preferences for tests (optionally unavailable).
#[derive(Default)]
pub struct StaticPreferences {
    opted_out: Mutex<HashSet<Uuid>>,
    unavailable: AtomicBool,
}

impl StaticPreferences {
    pub fn opt_out(&self, user: Uuid) {
        self.opted_out.lock().expect("prefs").insert(user);
    }
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }
}

#[async_trait]
impl PreferenceSource for StaticPreferences {
    async fn meeting_opt_outs(
        &self,
        users: &[Uuid],
    ) -> Result<HashSet<Uuid>, PreferencesUnavailable> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(PreferencesUnavailable);
        }
        let set = self.opted_out.lock().expect("prefs");
        Ok(users.iter().filter(|u| set.contains(u)).copied().collect())
    }
}

/// Signed read of the Hub preferences (`communications-preferences.read`).
pub struct HubPreferences {
    bridge: Arc<crate::services::communications::Bridge>,
}

/// Private Hub route returning the account references that opted out of a category.
pub const HUB_PREFERENCES_PATH: &str =
    "/internal/v1/communications/notification-preferences/resolve";

impl HubPreferences {
    pub fn new(bridge: Arc<crate::services::communications::Bridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl PreferenceSource for HubPreferences {
    async fn meeting_opt_outs(
        &self,
        users: &[Uuid],
    ) -> Result<HashSet<Uuid>, PreferencesUnavailable> {
        use cf_service_auth::{OutboundServiceRequest, ServiceId};
        let mut out = HashSet::new();
        for chunk in users.chunks(100) {
            let refs: Vec<String> = chunk.iter().map(|u| format!("cfmeet-user:{u}")).collect();
            let req = OutboundServiceRequest::new_operation(ServiceId::communications())
                .with_delegated_context(self.bridge.scope.clone());
            let r = self
                .bridge
                .client
                .post_json(
                    &req,
                    HUB_PREFERENCES_PATH,
                    &json!({"category":"MEETINGS","accountReferences":refs}),
                )
                .await
                .map_err(|_| PreferencesUnavailable)?;
            if r.status() != 200 {
                return Err(PreferencesUnavailable);
            }
            let v: Value = r.json().map_err(|_| PreferencesUnavailable)?;
            let list = v["optedOut"].as_array().ok_or(PreferencesUnavailable)?;
            for item in list {
                let id = item
                    .as_str()
                    .and_then(|s| s.strip_prefix("cfmeet-user:"))
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or(PreferencesUnavailable)?;
                if chunk.contains(&id) {
                    out.insert(id);
                }
            }
        }
        Ok(out)
    }
}

/// Transport + preference source (replaceable in tests).
pub struct Notifier {
    pub mail: Arc<dyn NotificationMail>,
    pub preferences: Arc<dyn PreferenceSource>,
    /// The Notification Service's public HPKE key (`CF_NOTIFICATIONS_SEALING_KEY`): seals
    /// invitation tokens / addresses into published events. `None`: invitation emails
    /// cannot be published and stay on the legacy path (`UNPUBLISHABLE`).
    pub sealing: Option<SealingKey>,
}

impl Notifier {
    pub fn new(mail: Arc<dyn NotificationMail>, preferences: Arc<dyn PreferenceSource>) -> Self {
        Self {
            mail,
            preferences,
            sealing: None,
        }
    }

    pub fn with_sealing(mut self, sealing: Option<SealingKey>) -> Self {
        self.sealing = sealing;
        self
    }
}

/// What happened to a requested invitation email.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EmailDelivery {
    /// Notification email is switched off on this deployment; share the link yourself.
    Disabled,
    /// Accepted for delivery (subject to the recipient's notification preferences).
    Accepted,
    /// Not sent (temporary failure); the invitation exists, share the link yourself.
    Failed,
}

/// Everything delivery needs (cheap to clone).
#[derive(Clone)]
pub struct NotificationContext {
    pub db: DatabaseConnection,
    pub app_url: String,
    pub config: NotificationConfig,
    pub notifier: Arc<Notifier>,
}

impl NotificationContext {
    pub fn from_state(state: &crate::state::AppState) -> Self {
        Self {
            db: state.db.clone(),
            app_url: state.app_url.trim_end_matches('/').to_string(),
            config: state.meeting.core.config.notifications.clone(),
            notifier: state.notifier.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

fn clean(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Titles of protected meetings are never written into an email.
pub fn presented_title(space: &meeting_spaces::Model) -> String {
    let protected = matches!(
        space.security_profile,
        SecurityProfile::Healthcare | SecurityProfile::HighSecurity
    ) || space.meeting_type == MeetingType::Healthcare
        || space.access_policy == AccessPolicy::HealthcareAppointment;
    if protected {
        "CF Meet meeting".into()
    } else {
        let t = clean(&space.title, 120);
        if t.is_empty() {
            "CF Meet meeting".into()
        } else {
            t
        }
    }
}

/// `Tue 10 Nov 2026, 14:30 (Europe/London)`; the recipient's zone when valid, otherwise
/// the meeting's zone, otherwise UTC.
pub fn format_time(at: DateTime<Utc>, preferred: Option<&str>, fallback: &str) -> String {
    let tz = preferred
        .and_then(|z| z.parse::<chrono_tz::Tz>().ok())
        .or_else(|| fallback.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::UTC);
    format!(
        "{} ({})",
        at.with_timezone(&tz).format("%a %-d %b %Y, %H:%M"),
        tz.name()
    )
}

/// The join path under the app URL (`/<meeting code>`): what a join link appends to
/// `APP_URL` (also answered by the X2 owner API, which never returns a full URL).
pub fn join_path(space: &meeting_spaces::Model) -> String {
    format!("/{}", space.meeting_code)
}

fn join_url(app_url: &str, space: &meeting_spaces::Model) -> String {
    format!("{}{}", app_url.trim_end_matches('/'), join_path(space))
}

fn digest(email: &str) -> String {
    hex::encode(Sha256::digest(email.trim().to_ascii_lowercase().as_bytes()))
}

const FOOTNOTE: &str = "You receive this email because you were invited to or take part in this CF Meet meeting. You can turn meeting notifications off in your CF Meet notification settings.";

// ---------------------------------------------------------------------------
// Invitation emails (synchronous: the token is never stored)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
struct AccountRow {
    id: Uuid,
    status: String,
    role: String,
    timezone: Option<String>,
    display_name: Option<String>,
    username: String,
}

impl AccountRow {
    fn eligible(&self) -> bool {
        !matches!(self.status.as_str(), "inactive" | "suspended") && self.role != "guest"
    }
    fn name(&self) -> String {
        self.display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.username.clone())
    }
}

/// The account registered under `email` (case-insensitive; the exact spelling first).
async fn account_for_email<C: ConnectionTrait>(
    db: &C,
    email: &str,
) -> ApiResult<Option<AccountRow>> {
    Ok(AccountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, status::text AS status, role::text AS role, timezone, display_name, username \
           FROM users WHERE lower(email) = lower($1) ORDER BY (email = $1) DESC, created_at LIMIT 1",
        [email.into()],
    ))
    .one(db)
    .await?)
}

/// Per-account hourly budget for requested invitation emails. `Ok(false)` = over budget.
pub async fn invitation_email_budget(
    state: &crate::state::AppState,
    user_id: Uuid,
) -> Result<bool, ()> {
    let per_hour = state
        .meeting
        .core
        .config
        .notifications
        .invite_emails_per_hour;
    match state
        .rate_limiter
        .check_action_rate("notify:invite-email", &user_id.to_string(), per_hour, 3600)
        .await
    {
        Ok(()) => Ok(true),
        Err(crate::error::ApiError::TooManyRequests(_)) => Ok(false),
        Err(_) => Err(()),
    }
}

/// Sends the invitation email for an email-bound meeting invitation.
pub async fn send_meeting_invitation(
    ctx: &NotificationContext,
    inviter_name: &str,
    invitation: &meeting_invitations::Model,
    token: &str,
) -> EmailDelivery {
    if !ctx.config.email_enabled {
        return EmailDelivery::Disabled;
    }
    match meeting_invitation_inner(ctx, inviter_name, invitation, token).await {
        Ok(d) => d,
        Err(_) => EmailDelivery::Failed,
    }
}

async fn meeting_invitation_inner(
    ctx: &NotificationContext,
    inviter_name: &str,
    invitation: &meeting_invitations::Model,
    token: &str,
) -> ApiResult<EmailDelivery> {
    let Some(email) = invitation.invitee_email.clone() else {
        return Ok(EmailDelivery::Failed);
    };
    let account = account_for_email(&ctx.db, &email).await?;
    if let Some(a) = &account {
        // Uniform answer: the inviter never learns whether the address has an account or
        // what its preferences are.
        if !a.eligible() {
            return Ok(EmailDelivery::Accepted);
        }
        match ctx.notifier.preferences.meeting_opt_outs(&[a.id]).await {
            Ok(out) if out.contains(&a.id) => return Ok(EmailDelivery::Accepted),
            Ok(_) => {}
            Err(_) => return Ok(EmailDelivery::Failed),
        }
    }
    let Some(space) = meeting_spaces::Entity::find_by_id(invitation.meeting_space_id)
        .one(&ctx.db)
        .await?
    else {
        return Ok(EmailDelivery::Failed);
    };
    let conference = match invitation.conference_id {
        Some(id) => {
            conference_sessions::Entity::find_by_id(id)
                .one(&ctx.db)
                .await?
        }
        None => None,
    };
    let title = presented_title(&space);
    let inviter = clean(inviter_name, 64);
    let inviter = if inviter.is_empty() {
        "A CF Meet user".to_string()
    } else {
        inviter
    };
    let mut paragraphs = vec![format!("{inviter} invited you to the meeting \"{title}\".")];
    if let Some(start) = conference.as_ref().and_then(|c| c.scheduled_start_at) {
        paragraphs.push(format!(
            "When: {}",
            format_time(
                start,
                account.as_ref().and_then(|a| a.timezone.as_deref()),
                &space.timezone
            )
        ));
    }
    paragraphs.push(format!(
        "This invitation is for {email} only: sign in with this verified address to use it. It expires on {}.",
        format_time(
            invitation.expires_at,
            account.as_ref().and_then(|a| a.timezone.as_deref()),
            &space.timezone
        )
    ));
    let link = format!("{}/invite#{token}", ctx.app_url);
    // Gap audit M14: a scheduled occurrence comes with its calendar invite. The link opens
    // CF Meet; the invitation token never goes into the calendar file.
    let calendar = conference.as_ref().and_then(|c| {
        let start = c.scheduled_start_at?;
        let end = c
            .scheduled_end_at
            .unwrap_or(start + chrono::Duration::hours(1));
        let event = crate::services::ics::IcsEvent {
            uid: format!("conference-{}@cfmeet.com", c.id.simple()),
            sequence: c.version.saturating_sub(1),
            summary: title.clone(),
            description: Some(format!(
                "{inviter} invited you. Open the invitation email to join."
            )),
            url: Some(ctx.app_url.clone()),
            start,
            end,
            status: crate::services::ics::IcsStatus::Confirmed,
            rrule: None,
            exdates: vec![],
            recurrence_id: None,
            created: None,
            last_modified: chrono::Utc::now(),
        };
        Some(crate::services::ics::render(
            &crate::services::ics::IcsCalendar {
                timezone: space.timezone.parse().ok(),
                name: Some(title.clone()),
                events: vec![event],
                timezone_until: Some(end),
            },
            chrono::Utc::now(),
        ))
    });
    let subject = clean(&format!("Invitation: {title}"), 160);
    let template = render(&NotificationBody {
        subject: &subject,
        greeting_name: account.as_ref().map(|a| a.name()).as_deref(),
        paragraphs,
        action: Some(("Open invitation", &link)),
        footnote: Some(
            "If you did not expect this invitation you can ignore this email. You can turn meeting notifications off in your CF Meet notification settings."
                .into(),
        ),
    });
    let outgoing = OutgoingEmail {
        category: crate::services::email_delivery::category::MEETING_INVITATION,
        idempotency_key: None,
        to: email,
        to_name: account.as_ref().map(|a| a.name()),
        subject,
        html: template.html,
        text: template.text,
        calendar,
    };
    // Routing fence (Document 4 §9.3): LEGACY_ONLY (or no fence history) is today's path.
    let sent = match fence::current_mode(&ctx.db, MeetingCategory::MeetingInvitation).await? {
        None | Some(RoutingMode::LegacyOnly) => ctx.notifier.mail.send(outgoing).await,
        Some(_) => {
            let key = format!("meeting-invitation:{}", invitation.id);
            let draft = meeting_invitation_draft(invitation, account.as_ref(), token, &key);
            send_routed(
                ctx,
                MeetingCategory::MeetingInvitation,
                RoutingKey::Fixed(key),
                outgoing,
                |_| draft,
            )
            .await
        }
    };
    Ok(if sent.is_ok() {
        EmailDelivery::Accepted
    } else {
        EmailDelivery::Failed
    })
}

/// Sends the invitation email for an organization invitation the Organizations Service
/// just created (its authoritative answer carries the invitation id and the one-time
/// token).
#[allow(clippy::too_many_arguments)]
pub async fn send_organization_invitation(
    ctx: &NotificationContext,
    inviter_name: &str,
    to_email: &str,
    organization_id: Uuid,
    invitation_id: Option<Uuid>,
    role: &str,
    token: &str,
    expires_at: Option<DateTime<Utc>>,
) -> EmailDelivery {
    if !ctx.config.email_enabled {
        return EmailDelivery::Disabled;
    }
    let email = to_email.trim().to_ascii_lowercase();
    if !email.contains('@') || email.len() > 320 || email.chars().any(char::is_control) {
        return EmailDelivery::Failed;
    }
    let account = account_for_email(&ctx.db, &email).await.ok().flatten();
    if account.as_ref().is_some_and(|a| !a.eligible()) {
        return EmailDelivery::Accepted;
    }
    // The read-only organization projection (refreshed from Organizations); never written here.
    let organization_name = crate::models::organizations::Entity::find_by_id(organization_id)
        .one(&ctx.db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name);
    let org = organization_name
        .as_deref()
        .map(|n| clean(n, 120))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "an organization".into());
    let inviter = clean(inviter_name, 64);
    let inviter = if inviter.is_empty() {
        "An administrator".to_string()
    } else {
        inviter
    };
    let role = clean(role, 16).to_ascii_lowercase();
    let mut paragraphs = vec![format!(
        "{inviter} invited you to join {org} on CF Meet as {} {role}.",
        if role.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        }
    )];
    paragraphs.push(format!(
        "This invitation is for {email} only: sign in with this verified address to accept or decline it."
    ));
    if let Some(at) = expires_at {
        paragraphs.push(format!(
            "It expires on {}.",
            format_time(
                at,
                account.as_ref().and_then(|a| a.timezone.as_deref()),
                "UTC"
            )
        ));
    }
    let link = format!("{}/organizations/invitations#{token}", ctx.app_url);
    let subject = clean(&format!("Invitation to join {org} on CF Meet"), 160);
    let template = render(&NotificationBody {
        subject: &subject,
        greeting_name: account.as_ref().map(|a| a.name()).as_deref(),
        paragraphs,
        action: Some(("Review invitation", &link)),
        footnote: Some("If you did not expect this invitation you can ignore this email.".into()),
    });
    let outgoing = OutgoingEmail {
        category: crate::services::email_delivery::category::ORGANIZATION_INVITATION,
        idempotency_key: None,
        to: email.clone(),
        to_name: account.as_ref().map(|a| a.name()),
        subject,
        html: template.html,
        text: template.text,
        calendar: None,
    };
    // Routing fence (Document 4 §9.3): LEGACY_ONLY (or no fence history) is today's path.
    let sent = match fence::current_mode(&ctx.db, MeetingCategory::OrganizationInvitation).await {
        Err(_) => Err(MailError::Unavailable),
        Ok(None | Some(RoutingMode::LegacyOnly)) => ctx.notifier.mail.send(outgoing).await,
        // Fenced: the decision key names the invitation, which the answer must carry.
        Ok(Some(_)) => match invitation_id {
            None => Err(MailError::Unavailable),
            Some(invitation_id) => {
                send_routed(
                    ctx,
                    MeetingCategory::OrganizationInvitation,
                    RoutingKey::OrgInvitation(invitation_id),
                    outgoing,
                    |key| {
                        organization_invitation_draft(
                            invitation_id,
                            organization_id,
                            &email,
                            token,
                            expires_at,
                            key,
                        )
                    },
                )
                .await
            }
        },
    };
    match sent {
        Ok(()) => EmailDelivery::Accepted,
        Err(_) => EmailDelivery::Failed,
    }
}

// ---------------------------------------------------------------------------
// Routing fence (Notification Service roadmap N7 / N11, Document 4 §9)
// ---------------------------------------------------------------------------

/// The decision key of a routed invitation email (today's ledger key formats, §9.3).
enum RoutingKey {
    /// `meeting-invitation:<invitation id>`.
    Fixed(String),
    /// `org-invitation:<invitation id>:<n>`: one per token Meeting emails for the
    /// invitation (an idempotent replay at Organizations re-issues a fresh token), `n`
    /// following the invitation's latest decision (serialized per invitation).
    OrgInvitation(Uuid),
}

async fn resolve_key(txn: &DatabaseTransaction, key: RoutingKey) -> Result<String, sea_orm::DbErr> {
    match key {
        RoutingKey::Fixed(k) => Ok(k),
        RoutingKey::OrgInvitation(id) => {
            let prefix = format!("org-invitation:{id}:");
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                [prefix.clone().into()],
            ))
            .await?;
            let last: i64 = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COALESCE(max(split_part(idempotency_key, ':', 3)::bigint), 0) AS last
                       FROM notification_route_decisions
                      WHERE category = 'ORGANIZATION_INVITATION' AND idempotency_key LIKE $1 || '%'",
                    [prefix.clone().into()],
                ))
                .await?
                .map(|r| r.try_get::<i64>("", "last"))
                .transpose()?
                .unwrap_or(0);
            Ok(format!("{prefix}{}", last + 1))
        }
    }
}

/// The routed (fenced) send of one invitation email: ONE transaction records the sticky
/// decision and writes the legacy ledger row (`OLD`, keyed by the decision key; plus one
/// shadow publication in `SHADOW`) or the publication only (`NEW`). Any failure rolls the
/// whole transaction back (nothing recorded; the email is reported `FAILED`).
async fn send_routed<F>(
    ctx: &NotificationContext,
    category: MeetingCategory,
    key: RoutingKey,
    mut email: OutgoingEmail,
    draft: F,
) -> Result<(), MailError>
where
    F: FnOnce(&str) -> Result<EventDraft, DecisionReason> + Send,
{
    let txn = ctx.db.begin().await.map_err(|_| MailError::Unavailable)?;
    let key = resolve_key(&txn, key)
        .await
        .map_err(|_| MailError::Unavailable)?;
    let routed = publications::route(
        &txn,
        &key,
        draft(&key),
        category,
        ctx.notifier.sealing.as_ref(),
        Utc::now(),
    )
    .await
    .map_err(|_| MailError::Unavailable)?;
    if routed.write_legacy {
        email.idempotency_key = Some(key);
        ctx.notifier.mail.send_in(&txn, email).await?;
    }
    txn.commit().await.map_err(|_| MailError::Unavailable)
}

/// The `meeting-service.invitation.email-requested` event of an email-bound meeting
/// invitation: the invitee's CF Meet account (the service reads its address through X1),
/// the token sealed. An invitee without an eligible account (the catalogue seals no
/// address for this type), or an invitation without a conference (`conferenceId` is
/// required), has no complete envelope.
fn meeting_invitation_draft(
    invitation: &meeting_invitations::Model,
    account: Option<&AccountRow>,
    token: &str,
    key: &str,
) -> Result<EventDraft, DecisionReason> {
    let account = account
        .filter(|a| a.eligible())
        .ok_or(DecisionReason::Unpublishable)?;
    let conference = invitation
        .conference_id
        .ok_or(DecisionReason::Unpublishable)?;
    Ok(EventDraft {
        category: MeetingCategory::MeetingInvitation,
        event_type: event_type::INVITATION,
        subject_id: invitation.id,
        subject_version: None,
        data: vec![
            ("conferenceId", json!(conference.to_string())),
            ("invitationId", json!(invitation.id.to_string())),
        ],
        recipients: vec![format!("cfmeet-user:{}", account.id)],
        dedupe_key: key.to_owned(),
        idempotency_key: key.to_owned(),
        cancel_key: None,
        delivery_expires_at: Some(invitation.expires_at),
        secret: Some(SealedPlaintext::default().with_token(token)),
        publish_until: invitation.expires_at,
    })
}

/// The `meeting-service.organization-invitation.email-requested` event: the invitation's
/// email principal (`meeting-email-principal:<invitation id>`), token and address sealed.
fn organization_invitation_draft(
    invitation_id: Uuid,
    organization_id: Uuid,
    address: &str,
    token: &str,
    expires_at: Option<DateTime<Utc>>,
    key: &str,
) -> Result<EventDraft, DecisionReason> {
    Ok(EventDraft {
        category: MeetingCategory::OrganizationInvitation,
        event_type: event_type::ORGANIZATION_INVITATION,
        subject_id: invitation_id,
        subject_version: None,
        data: vec![
            ("invitationId", json!(invitation_id.to_string())),
            ("organizationId", json!(organization_id.to_string())),
        ],
        recipients: vec![format!("meeting-email-principal:{invitation_id}")],
        dedupe_key: key.to_owned(),
        idempotency_key: key.to_owned(),
        cancel_key: None,
        delivery_expires_at: expires_at,
        secret: Some(
            SealedPlaintext::default()
                .with_token(token)
                .with_address(address),
        ),
        publish_until: expires_at.unwrap_or_else(|| Utc::now() + publications::LIFETIME),
    })
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The conference notice event of a job (`MEETING_NOTIFICATION`): the audience is the
/// worker's (same rule as X2), as CF Meet account references. An email-only participant
/// cannot be addressed from an envelope (the service reads addresses through X1, for
/// accounts only), so such an audience — or one larger than the configured maximum — has
/// no complete envelope and the notice stays on the legacy path.
#[allow(clippy::too_many_arguments)]
async fn conference_draft<C: ConnectionTrait>(
    conn: &C,
    config: &NotificationConfig,
    event_type: &'static str,
    conference: &conference_sessions::Model,
    dedupe_key: &str,
    actor_user_id: Option<Uuid>,
    starts_at: DateTime<Utc>,
    reminder: bool,
) -> ApiResult<Result<EventDraft, DecisionReason>> {
    let (rows, truncated) = conference_recipients(
        conn,
        conference.meeting_space_id,
        conference.id,
        config.max_recipients,
        actor_user_id,
    )
    .await?;
    if truncated {
        return Ok(Err(DecisionReason::RecipientLimit));
    }
    let mut recipients = Vec::with_capacity(rows.len());
    for r in rows {
        match r.user_id {
            Some(id) => recipients.push(format!("cfmeet-user:{id}")),
            None => return Ok(Err(DecisionReason::Unpublishable)),
        }
    }
    recipients.sort();
    recipients.dedup();
    Ok(Ok(EventDraft {
        category: MeetingCategory::MeetingNotification,
        event_type,
        subject_id: conference.id,
        subject_version: Some(i64::from(conference.version)),
        data: vec![
            ("conferenceId", json!(conference.id.to_string())),
            ("startsAt", json!(rfc3339(starts_at))),
        ],
        recipients,
        dedupe_key: dedupe_key.to_owned(),
        idempotency_key: format!("meeting-notification:{dedupe_key}"),
        cancel_key: reminder.then(|| format!("conference-reminder:{}", conference.id)),
        delivery_expires_at: reminder.then_some(starts_at),
        secret: None,
        publish_until: if reminder {
            starts_at
        } else {
            Utc::now() + publications::LIFETIME
        },
    }))
}

/// One conference job, routed (Document 4 §9.3) on the caller's connection — the
/// transaction of the change for reschedule / cancellation notices. LEGACY_ONLY (or no
/// fence history): today's insert. Otherwise the decision (key = the job's `dedupe_key`)
/// and the job (`OLD`, plus one shadow publication in `SHADOW`) or the publication only
/// (`NEW`) are written on `conn`.
#[allow(clippy::too_many_arguments)]
async fn record_job<C: ConnectionTrait>(
    conn: &C,
    config: &NotificationConfig,
    mode: Option<RoutingMode>,
    kind: &str,
    conference: &conference_sessions::Model,
    dedupe_key: &str,
    actor_user_id: Option<Uuid>,
    payload: Value,
    starts_at: DateTime<Utc>,
) -> ApiResult<u64> {
    let Some(mode) = mode.filter(|m| *m != RoutingMode::LegacyOnly) else {
        return insert_job(conn, kind, conference, dedupe_key, actor_user_id, payload).await;
    };
    let event_type = match kind {
        KIND_RESCHEDULED => event_type::RESCHEDULED,
        KIND_CANCELLED => event_type::CANCELLED,
        _ => event_type::SCHEDULED,
    };
    // FENCED_OLD routes OLD whatever the event: no audience is read for it.
    let draft = if matches!(mode, RoutingMode::Shadow | RoutingMode::FencedNew) {
        conference_draft(
            conn,
            config,
            event_type,
            conference,
            dedupe_key,
            actor_user_id,
            starts_at,
            kind == KIND_REMINDER,
        )
        .await?
    } else {
        Err(DecisionReason::Unpublishable)
    };
    let routed = publications::route(
        conn,
        dedupe_key,
        draft,
        MeetingCategory::MeetingNotification,
        None,
        Utc::now(),
    )
    .await?;
    if routed.write_legacy {
        insert_job(conn, kind, conference, dedupe_key, actor_user_id, payload).await
    } else {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Durable jobs: producers
// ---------------------------------------------------------------------------

async fn insert_job<C: ConnectionTrait>(
    conn: &C,
    kind: &str,
    conference: &conference_sessions::Model,
    dedupe_key: &str,
    actor_user_id: Option<Uuid>,
    payload: Value,
) -> ApiResult<u64> {
    Ok(conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO meeting_notifications (id, kind, meeting_space_id, conference_id, dedupe_key, actor_user_id, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (dedupe_key) DO NOTHING",
            [
                Uuid::new_v4().into(),
                kind.into(),
                conference.meeting_space_id.into(),
                conference.id.into(),
                dedupe_key.into(),
                actor_user_id.into(),
                payload.into(),
            ],
        ))
        .await?
        .rows_affected())
}

/// Records a reschedule notice on the caller's transaction (no-op when disabled).
pub async fn record_rescheduled<C: ConnectionTrait>(
    conn: &C,
    config: &NotificationConfig,
    before: &conference_sessions::Model,
    after: &conference_sessions::Model,
    actor_user_id: Option<Uuid>,
) -> ApiResult<()> {
    if !config.email_enabled || after.origin != crate::domain::RecordOrigin::V2 {
        return Ok(());
    }
    let Some(to) = after.scheduled_start_at else {
        return Ok(());
    };
    let mode = fence::current_mode(conn, MeetingCategory::MeetingNotification).await?;
    record_job(
        conn,
        config,
        mode,
        KIND_RESCHEDULED,
        after,
        &format!("rescheduled:{}:{}", after.id, after.version),
        actor_user_id,
        json!({
            "from": before.scheduled_start_at,
            "to": to,
            "scheduledEndAt": after.scheduled_end_at,
            "conferenceVersion": after.version,
        }),
        to,
    )
    .await?;
    Ok(())
}

/// Records a cancellation notice on the caller's transaction (no-op when disabled).
/// Only scheduled occurrences are announced; system expiry of stale occurrences is not.
pub async fn record_cancelled<C: ConnectionTrait>(
    conn: &C,
    config: &NotificationConfig,
    conference: &conference_sessions::Model,
    actor_user_id: Option<Uuid>,
) -> ApiResult<()> {
    if !config.email_enabled || conference.origin != crate::domain::RecordOrigin::V2 {
        return Ok(());
    }
    let Some(start) = conference.scheduled_start_at else {
        return Ok(());
    };
    let mode = fence::current_mode(conn, MeetingCategory::MeetingNotification).await?;
    record_job(
        conn,
        config,
        mode,
        KIND_CANCELLED,
        conference,
        &format!("cancelled:{}", conference.id),
        actor_user_id,
        json!({ "scheduledStartAt": start }),
        start,
    )
    .await?;
    Ok(())
}

/// Gap audit M3: how far ahead, and how many, occurrences a series-level change announces.
pub const SERIES_NOTICE_HORIZON_DAYS: i64 = 30;
pub const SERIES_NOTICE_MAX: u64 = 20;

/// Gap audit M3: the space's unstarted v2 occurrences within the notice horizon (soonest
/// first), captured before a series-level change.
pub async fn upcoming_occurrences<C: ConnectionTrait>(
    conn: &C,
    space_id: Uuid,
    now: DateTime<Utc>,
) -> ApiResult<Vec<conference_sessions::Model>> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
    Ok(conference_sessions::Entity::find()
        .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id))
        .filter(conference_sessions::Column::Status.eq(crate::domain::ConferenceStatus::Created))
        .filter(conference_sessions::Column::Origin.eq(crate::domain::RecordOrigin::V2))
        .filter(conference_sessions::Column::ScheduledStartAt.gt(now))
        .filter(
            conference_sessions::Column::ScheduledStartAt
                .lte(now + chrono::Duration::days(SERIES_NOTICE_HORIZON_DAYS)),
        )
        .order_by_asc(conference_sessions::Column::ScheduledStartAt)
        .limit(SERIES_NOTICE_MAX)
        .all(conn)
        .await?)
}

/// Gap audit M3: after a series-level change (series edited or ended, timezone moved,
/// exception, space archived), announce what happened to each occurrence captured
/// before it: cancelled → a cancellation notice; moved, or replaced by a re-generated
/// occurrence → a reschedule notice (paired in order). Same durable jobs, dedupe keys and
/// Notification Service routing as single-occurrence notices; no-op when disabled.
pub async fn announce_series_change<C: ConnectionTrait>(
    conn: &C,
    config: &NotificationConfig,
    space_id: Uuid,
    before: &[conference_sessions::Model],
    actor_user_id: Option<Uuid>,
) -> ApiResult<()> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    if !config.email_enabled || before.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let ids: Vec<Uuid> = before.iter().map(|c| c.id).collect();
    let still: std::collections::HashMap<Uuid, conference_sessions::Model> =
        conference_sessions::Entity::find()
            .filter(conference_sessions::Column::Id.is_in(ids.clone()))
            .all(conn)
            .await?
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
    let mut replacements = upcoming_occurrences(conn, space_id, now)
        .await?
        .into_iter()
        .filter(|c| !ids.contains(&c.id));
    for old in before {
        match still.get(&old.id) {
            Some(now_row) if now_row.status == crate::domain::ConferenceStatus::Cancelled => {
                record_cancelled(conn, config, now_row, actor_user_id).await?;
            }
            Some(now_row) if now_row.scheduled_start_at != old.scheduled_start_at => {
                record_rescheduled(conn, config, old, now_row, actor_user_id).await?;
            }
            Some(_) => {}
            None => {
                if let Some(new) = replacements.next() {
                    if new.scheduled_start_at != old.scheduled_start_at {
                        record_rescheduled(conn, config, old, &new, actor_user_id).await?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// One reminder per conference and scheduled start, for v2 occurrences of active spaces
/// starting within the lead time. Idempotent across instances and ticks.
pub async fn enqueue_due_reminders(
    db: &DatabaseConnection,
    config: &NotificationConfig,
    now: DateTime<Utc>,
) -> ApiResult<u64> {
    if !config.reminders_enabled() {
        return Ok(0);
    }
    let horizon = now + Duration::minutes(config.reminder_lead_minutes);
    let mode = fence::current_mode(db, MeetingCategory::MeetingNotification)
        .await?
        .filter(|m| *m != RoutingMode::LegacyOnly);
    if let Some(mode) = mode {
        return enqueue_due_reminders_routed(db, config, mode, now, horizon).await;
    }
    let due = conference_sessions::Model::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT c.* FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id \
          WHERE c.scheduled_start_at > $1 AND c.scheduled_start_at <= $2 \
            AND c.status IN ('CREATED', 'PREPARING', 'WAITING') AND c.origin = 'V2' AND s.status = 'ACTIVE' \
            AND NOT EXISTS (SELECT 1 FROM meeting_notifications n WHERE n.dedupe_key = \
                'reminder:' || c.id::text || ':' || floor(extract(epoch FROM c.scheduled_start_at))::bigint::text) \
          ORDER BY c.scheduled_start_at LIMIT $3",
        [now.into(), horizon.into(), REMINDER_SCAN_LIMIT.into()],
    ))
    .all(db)
    .await?;
    let mut created = 0;
    for c in due {
        let Some(start) = c.scheduled_start_at else {
            continue;
        };
        created += insert_job(
            db,
            KIND_REMINDER,
            &c,
            &reminder_key(c.id, start),
            None,
            json!({ "scheduledStartAt": start }),
        )
        .await?;
    }
    Ok(created)
}

/// The reminder scan outside LEGACY_ONLY (§9.9 phase 4.3a: Meeting's scan stays the only
/// trigger; each reminder is routed under its `reminder_key`, one transaction each). A
/// reminder already decided (OLD job or NEW publication) is not found again. Returns the
/// legacy jobs created.
async fn enqueue_due_reminders_routed(
    db: &DatabaseConnection,
    config: &NotificationConfig,
    mode: RoutingMode,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> ApiResult<u64> {
    let due = conference_sessions::Model::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT c.* FROM conference_sessions c JOIN meeting_spaces s ON s.id = c.meeting_space_id \
          WHERE c.scheduled_start_at > $1 AND c.scheduled_start_at <= $2 \
            AND c.status IN ('CREATED', 'PREPARING', 'WAITING') AND c.origin = 'V2' AND s.status = 'ACTIVE' \
            AND NOT EXISTS (SELECT 1 FROM meeting_notifications n WHERE n.dedupe_key = \
                'reminder:' || c.id::text || ':' || floor(extract(epoch FROM c.scheduled_start_at))::bigint::text) \
            AND NOT EXISTS (SELECT 1 FROM notification_route_decisions d WHERE d.idempotency_key = \
                'reminder:' || c.id::text || ':' || floor(extract(epoch FROM c.scheduled_start_at))::bigint::text) \
          ORDER BY c.scheduled_start_at LIMIT $3",
        [now.into(), horizon.into(), REMINDER_SCAN_LIMIT.into()],
    ))
    .all(db)
    .await?;
    let mut created = 0;
    for c in due {
        let Some(start) = c.scheduled_start_at else {
            continue;
        };
        let txn = db.begin().await?;
        created += record_job(
            &txn,
            config,
            Some(mode),
            KIND_REMINDER,
            &c,
            &reminder_key(c.id, start),
            None,
            json!({ "scheduledStartAt": start }),
            start,
        )
        .await?;
        txn.commit().await?;
    }
    Ok(created)
}

pub fn reminder_key(conference_id: Uuid, start: DateTime<Utc>) -> String {
    format!("reminder:{conference_id}:{}", start.timestamp())
}

// ---------------------------------------------------------------------------
// Durable jobs: delivery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub struct JobRow {
    pub id: Uuid,
    pub kind: String,
    pub meeting_space_id: Uuid,
    pub conference_id: Uuid,
    pub actor_user_id: Option<Uuid>,
    pub payload: Value,
    pub attempts: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Sent { queued: i32, suppressed: i32 },
    Skipped(&'static str),
    Retry(&'static str),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryTick {
    pub reminders_created: u64,
    pub sent: u64,
    pub skipped: u64,
    pub retried: u64,
    pub failed: u64,
    pub purged: u64,
}

/// One worker pass: create due reminders, deliver due jobs, purge old ones.
pub async fn tick(ctx: &NotificationContext, now: DateTime<Utc>) -> ApiResult<DeliveryTick> {
    let mut t = DeliveryTick::default();
    if !ctx.config.email_enabled {
        return Ok(t);
    }
    t.reminders_created = enqueue_due_reminders(&ctx.db, &ctx.config, now).await?;
    let lease = Uuid::new_v4();
    let jobs = JobRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE meeting_notifications n SET lease_id = $1, lease_until = now() + make_interval(secs => $2), attempts = n.attempts + 1 \
          WHERE n.id IN (SELECT id FROM meeting_notifications \
                          WHERE status = 'PENDING' AND available_at <= now() AND (lease_until IS NULL OR lease_until < now()) \
                          ORDER BY available_at LIMIT $3 FOR UPDATE SKIP LOCKED) \
          RETURNING n.id, n.kind, n.meeting_space_id, n.conference_id, n.actor_user_id, n.payload, n.attempts",
        [lease.into(), (LEASE_SECS as f64).into(), ctx.config.batch.into()],
    ))
    .all(&ctx.db)
    .await?;
    for job in jobs {
        let outcome = match deliver(ctx, &job, now).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(notification_id = %job.id, "notification delivery error: {e}");
                Outcome::Retry("DELIVERY_ERROR")
            }
        };
        let (status, error, queued, suppressed) = match &outcome {
            Outcome::Sent { queued, suppressed } => {
                t.sent += 1;
                ("SENT", None, *queued, *suppressed)
            }
            Outcome::Skipped(reason) => {
                t.skipped += 1;
                ("SKIPPED", Some(*reason), 0, 0)
            }
            Outcome::Retry(reason) if job.attempts >= MAX_ATTEMPTS => {
                t.failed += 1;
                ("FAILED", Some(*reason), 0, 0)
            }
            Outcome::Retry(reason) => {
                t.retried += 1;
                ("PENDING", Some(*reason), 0, 0)
            }
        };
        let backoff = 2_i64.pow(job.attempts.clamp(0, 10) as u32).min(900);
        ctx.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE meeting_notifications SET status = $3, last_error = $4, \
                        recipients_queued = recipients_queued + $5, recipients_suppressed = recipients_suppressed + $6, \
                        available_at = CASE WHEN $3 = 'PENDING' THEN now() + make_interval(secs => $7) ELSE available_at END, \
                        completed_at = CASE WHEN $3 = 'PENDING' THEN NULL ELSE now() END, \
                        lease_id = NULL, lease_until = NULL \
                  WHERE id = $1 AND lease_id = $2",
                [
                    job.id.into(),
                    lease.into(),
                    status.into(),
                    error.map(str::to_owned).into(),
                    queued.into(),
                    suppressed.into(),
                    (backoff as f64).into(),
                ],
            ))
            .await?;
    }
    t.purged = ctx
        .db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM meeting_notifications WHERE id IN (SELECT id FROM meeting_notifications \
              WHERE completed_at IS NOT NULL AND completed_at < $1 LIMIT 500)",
            [(now - Duration::days(RETENTION_DAYS)).into()],
        ))
        .await?
        .rows_affected();
    Ok(t)
}

fn payload_time(payload: &Value, key: &str) -> Option<DateTime<Utc>> {
    payload[key]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// One recipient of a conference notice: an address, and the account registered under it
/// (none for an email-only participant).
#[derive(Debug, Clone, FromQueryResult)]
pub struct RecipientRow {
    pub email: String,
    pub user_id: Option<Uuid>,
    pub status: Option<String>,
    pub role: Option<String>,
    pub timezone: Option<String>,
    pub display_name: Option<String>,
    pub username: Option<String>,
}

/// Recipients of a conference notice (see module docs), deduplicated by address.
const RECIPIENTS_SQL: &str = r#"
WITH r AS (
    SELECT lower(u.email) AS email FROM meeting_spaces s JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1
    UNION SELECT lower(u.email) FROM meeting_members mm JOIN users u ON u.id = mm.user_id
           WHERE mm.meeting_space_id = $1 AND mm.status = 'ACTIVE'
    UNION SELECT lower(mm.principal_ref) FROM meeting_members mm
           WHERE mm.meeting_space_id = $1 AND mm.status = 'ACTIVE' AND mm.principal_type = 'EMAIL' AND mm.user_id IS NULL
    UNION SELECT lower(i.invitee_email) FROM meeting_invitations i
           WHERE i.meeting_space_id = $1 AND i.status = 'ACTIVE' AND i.invitee_email IS NOT NULL AND i.expires_at > now()
             AND (i.use_limit IS NULL OR i.usage_count < i.use_limit)
             AND (i.conference_id IS NULL OR i.conference_id = $2)
    UNION SELECT lower(u.email) FROM webinar_registrations w JOIN users u ON u.id = w.user_id
           WHERE w.meeting_space_id = $1 AND w.status = 'REGISTERED' AND (w.conference_id IS NULL OR w.conference_id = $2)
)
SELECT e.email, a.id AS user_id, a.status, a.role, a.timezone, a.display_name, a.username
  FROM (SELECT DISTINCT email FROM r WHERE email IS NOT NULL AND position('@' IN email) > 1) e
  LEFT JOIN LATERAL (
      SELECT x.id, x.status::text AS status, x.role::text AS role, x.timezone, x.display_name, x.username
        FROM users x WHERE lower(x.email) = e.email ORDER BY (x.email = e.email) DESC, x.created_at LIMIT 1
  ) a ON true
 ORDER BY e.email
 LIMIT $3
"#;

/// Recipients of a conference notice (module docs; one rule for the worker and the X2
/// owner API): deduplicated by address, at most `max` (the second value is `true` when
/// more existed), then without `actor` and without inactive, suspended or guest accounts.
pub async fn conference_recipients<C: ConnectionTrait>(
    db: &C,
    space_id: Uuid,
    conference_id: Uuid,
    max: usize,
    actor: Option<Uuid>,
) -> ApiResult<(Vec<RecipientRow>, bool)> {
    let limit = max as i64 + 1;
    let mut rows = RecipientRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        RECIPIENTS_SQL,
        [space_id.into(), conference_id.into(), limit.into()],
    ))
    .all(db)
    .await?;
    let truncated = rows.len() > max;
    if truncated {
        rows.truncate(max);
    }
    rows.retain(|r| {
        r.user_id.is_none()
            || (r.user_id != actor
                && !matches!(r.status.as_deref(), Some("inactive" | "suspended"))
                && r.role.as_deref() != Some("guest"))
    });
    Ok((rows, truncated))
}

/// A conference notice: the worker's durable job kinds and the X2 request kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// A newly scheduled occurrence (announced by the Notification Service only).
    Scheduled,
    Rescheduled,
    Cancelled,
    Reminder,
}

impl NoticeKind {
    /// The kind of a durable job (`None`: unknown kind).
    pub fn of_job(kind: &str) -> Option<Self> {
        match kind {
            KIND_REMINDER => Some(Self::Reminder),
            KIND_RESCHEDULED => Some(Self::Rescheduled),
            KIND_CANCELLED => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A notice is only sent while it is still true (one rule for the worker and the X2 owner
/// API). `announced_start` is the start the notice is about: the reminded start of a
/// reminder, the new start of a reschedule or schedule notice. `Err` names why not.
pub fn notice_still_true(
    kind: NoticeKind,
    space: &meeting_spaces::Model,
    conference: &conference_sessions::Model,
    announced_start: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), &'static str> {
    if space.status != SpaceStatus::Active {
        return Err("SPACE_INACTIVE");
    }
    let startable = matches!(
        conference.status,
        ConferenceStatus::Created | ConferenceStatus::Preparing | ConferenceStatus::Waiting
    );
    let still_true = match kind {
        NoticeKind::Reminder | NoticeKind::Scheduled => {
            startable
                && conference.scheduled_start_at == announced_start
                && announced_start.is_some_and(|s| s > now)
        }
        NoticeKind::Rescheduled => {
            conference.status == ConferenceStatus::Created
                && conference.scheduled_start_at == announced_start
        }
        NoticeKind::Cancelled => conference.status == ConferenceStatus::Cancelled,
    };
    if still_true {
        Ok(())
    } else {
        Err("SUPERSEDED")
    }
}

async fn deliver(
    ctx: &NotificationContext,
    job: &JobRow,
    now: DateTime<Utc>,
) -> ApiResult<Outcome> {
    let Some(space) = meeting_spaces::Entity::find_by_id(job.meeting_space_id)
        .one(&ctx.db)
        .await?
    else {
        return Ok(Outcome::Skipped("SPACE_GONE"));
    };
    if space.status != SpaceStatus::Active {
        return Ok(Outcome::Skipped("SPACE_INACTIVE"));
    }
    let Some(conference) = conference_sessions::Entity::find_by_id(job.conference_id)
        .one(&ctx.db)
        .await?
    else {
        return Ok(Outcome::Skipped("CONFERENCE_GONE"));
    };
    // A notice is only sent while it is still true.
    let Some(kind) = NoticeKind::of_job(&job.kind) else {
        return Ok(Outcome::Skipped("UNKNOWN_KIND"));
    };
    let announced = match kind {
        NoticeKind::Rescheduled => payload_time(&job.payload, "to"),
        _ => payload_time(&job.payload, "scheduledStartAt"),
    };
    if let Err(reason) = notice_still_true(kind, &space, &conference, announced, now) {
        return Ok(Outcome::Skipped(reason));
    }

    let (rows, truncated) = conference_recipients(
        &ctx.db,
        space.id,
        conference.id,
        ctx.config.max_recipients,
        job.actor_user_id,
    )
    .await?;
    if truncated {
        tracing::warn!(notification_id = %job.id, max = ctx.config.max_recipients, "notification recipients truncated");
    }
    if rows.is_empty() {
        return Ok(Outcome::Sent {
            queued: 0,
            suppressed: 0,
        });
    }
    let accounts: Vec<Uuid> = rows.iter().filter_map(|r| r.user_id).collect();
    let opted_out = if accounts.is_empty() {
        HashSet::new()
    } else {
        match ctx.notifier.preferences.meeting_opt_outs(&accounts).await {
            Ok(set) => set,
            Err(_) => return Ok(Outcome::Retry("PREFERENCES_UNAVAILABLE")),
        }
    };

    let title = presented_title(&space);
    let link = join_url(&ctx.app_url, &space);
    let (mut queued, mut suppressed) = (0, 0);
    for r in rows {
        let key = digest(&r.email);
        let suppress = r.user_id.is_some_and(|u| opted_out.contains(&u));
        let inserted = ctx
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO meeting_notification_recipients (notification_id, recipient_digest, outcome) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                [
                    job.id.into(),
                    key.clone().into(),
                    (if suppress { "SUPPRESSED" } else { "QUEUED" }).into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            continue; // handled by an earlier attempt
        }
        if suppress {
            suppressed += 1;
            continue;
        }
        let name = r
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| r.username.clone());
        let email = compose(job, &conference, &space, &title, &link, &r, name.as_deref());
        let sent = ctx
            .notifier
            .mail
            .send(OutgoingEmail {
                category: crate::services::email_delivery::category::MEETING_NOTIFICATION,
                idempotency_key: Some(format!("meeting-notification:{}:{}", job.id, key)),
                to: r.email.clone(),
                to_name: name,
                subject: email.0,
                html: email.1,
                text: email.2,
                calendar: None,
            })
            .await;
        if let Err(refused @ (MailError::InvalidAddress | MailError::Suppressed)) = sent {
            // N0-3 (S-11) / N0-12: this recipient can never be sent to (malformed address,
            // or suppressed after a complaint / repeated bounces). Record it as not sent and
            // continue with the others; it must not hold the whole notice in retry.
            ctx.db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE meeting_notification_recipients SET outcome = 'SUPPRESSED' \
                     WHERE notification_id = $1 AND recipient_digest = $2",
                    [job.id.into(), key.into()],
                ))
                .await?;
            if refused == MailError::InvalidAddress {
                inc(&OM.meeting_notification_invalid_recipients);
            }
            tracing::warn!(notification_id = %job.id, reason = refused.as_str(),
                "meeting notice recipient skipped");
            suppressed += 1;
            continue;
        }
        if sent.is_err() {
            // Not queued: release the ledger entry so the retry sends it.
            ctx.db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM meeting_notification_recipients WHERE notification_id = $1 AND recipient_digest = $2",
                    [job.id.into(), key.into()],
                ))
                .await?;
            if queued > 0 || suppressed > 0 {
                ctx.db
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "UPDATE meeting_notifications SET recipients_queued = recipients_queued + $2, \
                                recipients_suppressed = recipients_suppressed + $3 WHERE id = $1",
                        [job.id.into(), queued.into(), suppressed.into()],
                    ))
                    .await?;
            }
            return Ok(Outcome::Retry("MAIL_QUEUE_UNAVAILABLE"));
        }
        queued += 1;
    }
    Ok(Outcome::Sent { queued, suppressed })
}

/// (subject, html, text) for one recipient.
fn compose(
    job: &JobRow,
    conference: &conference_sessions::Model,
    space: &meeting_spaces::Model,
    title: &str,
    link: &str,
    r: &RecipientRow,
    name: Option<&str>,
) -> (String, String, String) {
    let tz = r.timezone.as_deref();
    let when = |t: DateTime<Utc>| format_time(t, tz, &space.timezone);
    let (subject, paragraphs, action) = match job.kind.as_str() {
        KIND_REMINDER => {
            let start = conference.scheduled_start_at.unwrap_or_else(Utc::now);
            (
                format!("Reminder: {title} starts at {}", when(start)),
                vec![format!("\"{title}\" starts at {}.", when(start))],
                Some(("Join meeting", link)),
            )
        }
        KIND_RESCHEDULED => {
            let to = conference.scheduled_start_at.unwrap_or_else(Utc::now);
            let mut p = vec![format!("\"{title}\" has been rescheduled.")];
            if let Some(from) = payload_time(&job.payload, "from") {
                p.push(format!("Previously: {}", when(from)));
            }
            p.push(format!("New time: {}", when(to)));
            if let Some(end) = conference.scheduled_end_at {
                p.push(format!("Ends: {}", when(end)));
            }
            (
                format!("Rescheduled: {title}"),
                p,
                Some(("Open meeting", link)),
            )
        }
        _ => {
            let mut p = vec![format!("\"{title}\" has been cancelled.")];
            if let Some(start) = payload_time(&job.payload, "scheduledStartAt") {
                p.push(format!("It was scheduled for {}.", when(start)));
            }
            (format!("Cancelled: {title}"), p, None)
        }
    };
    let subject = clean(&subject, 200);
    let template = render(&NotificationBody {
        subject: &subject,
        greeting_name: name,
        paragraphs,
        action,
        footnote: Some(FOOTNOTE.into()),
    });
    (subject, template.html, template.text)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

pub struct NotificationWorkerHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl NotificationWorkerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Starts the notification worker (only call when `email_enabled`).
pub fn spawn_worker(ctx: NotificationContext) -> NotificationWorkerHandle {
    let (shutdown_tx, mut shutdown) = tokio::sync::watch::channel(false);
    let poll = ctx.config.poll_secs.max(5);
    tokio::spawn(async move {
        tracing::info!("Notification worker started (polling every {poll}s)");
        loop {
            if *shutdown.borrow() {
                break;
            }
            match tick(&ctx, Utc::now()).await {
                Ok(t) if t != DeliveryTick::default() => tracing::info!(?t, "notification tick"),
                Ok(_) => {}
                Err(e) => tracing::error!("notification tick failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(poll)) => {}
                _ = shutdown.changed() => {}
            }
        }
        tracing::info!("Notification worker stopped");
    });
    NotificationWorkerHandle { shutdown_tx }
}

/// Counts per status (operations and tests).
pub async fn status_counts(db: &DatabaseConnection) -> ApiResult<BTreeMap<String, i64>> {
    #[derive(FromQueryResult)]
    struct Row {
        status: String,
        n: i64,
    }
    Ok(Row::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT status, count(*)::bigint AS n FROM meeting_notifications GROUP BY status"
            .to_owned(),
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|r| (r.status, r.n))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_use_the_recipient_zone_then_the_meeting_zone() {
        let at = DateTime::parse_from_rfc3339("2026-11-10T14:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            format_time(at, Some("Africa/Lagos"), "UTC"),
            "Tue 10 Nov 2026, 15:30 (Africa/Lagos)"
        );
        assert_eq!(
            format_time(at, None, "America/New_York"),
            "Tue 10 Nov 2026, 09:30 (America/New_York)"
        );
        assert_eq!(
            format_time(at, Some("Not/AZone"), "Bad/Zone"),
            "Tue 10 Nov 2026, 14:30 (UTC)"
        );
    }

    #[test]
    fn reminder_keys_change_with_the_start() {
        let id = Uuid::new_v4();
        let a = Utc::now();
        assert_eq!(reminder_key(id, a), reminder_key(id, a));
        assert_ne!(
            reminder_key(id, a),
            reminder_key(id, a + Duration::minutes(5))
        );
    }

    #[test]
    fn digests_ignore_case_and_spaces() {
        assert_eq!(digest(" Ada@Example.com "), digest("ada@example.com"));
        assert_eq!(digest("a@b.c").len(), 64);
    }

    #[tokio::test]
    async fn recording_mail_and_static_preferences() {
        let m = RecordingMail::default();
        let e = OutgoingEmail {
            category: "NOTIFICATION",
            idempotency_key: None,
            to: "a@b.c".into(),
            to_name: None,
            subject: "s".into(),
            html: "h".into(),
            text: "t".into(),
            calendar: None,
        };
        m.send(e.clone()).await.unwrap();
        m.set_failing(true);
        assert!(m.send(e).await.is_err());
        assert_eq!(m.sent().len(), 1);
        let p = StaticPreferences::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        p.opt_out(a);
        assert_eq!(
            p.meeting_opt_outs(&[a, b]).await.unwrap(),
            HashSet::from([a])
        );
        p.set_unavailable(true);
        assert!(p.meeting_opt_outs(&[a]).await.is_err());
        assert!(NoPreferenceStore
            .meeting_opt_outs(&[a])
            .await
            .unwrap()
            .is_empty());
    }
}
