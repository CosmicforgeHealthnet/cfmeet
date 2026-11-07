//! Meeting and organization notifications (Blueprint v1.0 Section 11: MTG-006, ACC-020).
//!
//! **Reuse.** Email goes through the existing email queue (`EmailService` → Redis
//! `email:queue` → `EmailWorker` → SMTP). No new transport, provider or notification
//! service is introduced. Everything is OFF unless `NOTIFICATIONS_EMAIL_ENABLED=true`.
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

use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult, Statement,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    config::notifications::NotificationConfig,
    domain::{AccessPolicy, ConferenceStatus, MeetingType, SecurityProfile, SpaceStatus},
    error::ApiResult,
    models::{conference_sessions, meeting_invitations, meeting_spaces},
    services::email::EmailService,
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
    pub to: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
}

#[async_trait]
pub trait NotificationMail: Send + Sync {
    /// Hand one email to the delivery queue.
    async fn send(&self, email: OutgoingEmail) -> Result<(), String>;
}

#[async_trait]
impl NotificationMail for EmailService {
    async fn send(&self, email: OutgoingEmail) -> Result<(), String> {
        self.queue()
            .enqueue(
                &email.to,
                email.to_name.as_deref(),
                &email.subject,
                &email.html,
                &email.text,
            )
            .await
            .map(|_| ())
            .map_err(|_| "MAIL_QUEUE_UNAVAILABLE".to_string())
    }
}

/// In-process transport for tests: records every email, can simulate a queue outage.
#[derive(Default)]
pub struct RecordingMail {
    sent: Mutex<Vec<OutgoingEmail>>,
    failing: AtomicBool,
}

impl RecordingMail {
    pub fn sent(&self) -> Vec<OutgoingEmail> {
        self.sent.lock().expect("recording mail").clone()
    }
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
}

#[async_trait]
impl NotificationMail for RecordingMail {
    async fn send(&self, email: OutgoingEmail) -> Result<(), String> {
        if self.failing.load(Ordering::SeqCst) {
            return Err("MAIL_QUEUE_UNAVAILABLE".into());
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
}

impl Notifier {
    pub fn new(mail: Arc<dyn NotificationMail>, preferences: Arc<dyn PreferenceSource>) -> Self {
        Self { mail, preferences }
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

fn join_url(app_url: &str, space: &meeting_spaces::Model) -> String {
    format!("{}/{}", app_url.trim_end_matches('/'), space.meeting_code)
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
    let sent = ctx
        .notifier
        .mail
        .send(OutgoingEmail {
            to: email,
            to_name: account.as_ref().map(|a| a.name()),
            subject,
            html: template.html,
            text: template.text,
        })
        .await;
    Ok(if sent.is_ok() {
        EmailDelivery::Accepted
    } else {
        EmailDelivery::Failed
    })
}

/// Sends the invitation email for an organization invitation the Organizations Service
/// just created (its authoritative answer carries the one-time token).
pub async fn send_organization_invitation(
    ctx: &NotificationContext,
    inviter_name: &str,
    to_email: &str,
    organization_id: Uuid,
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
    match ctx
        .notifier
        .mail
        .send(OutgoingEmail {
            to: email,
            to_name: account.as_ref().map(|a| a.name()),
            subject,
            html: template.html,
            text: template.text,
        })
        .await
    {
        Ok(()) => EmailDelivery::Accepted,
        Err(_) => EmailDelivery::Failed,
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
    insert_job(
        conn,
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
    insert_job(
        conn,
        KIND_CANCELLED,
        conference,
        &format!("cancelled:{}", conference.id),
        actor_user_id,
        json!({ "scheduledStartAt": start }),
    )
    .await?;
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

#[derive(Debug, Clone, FromQueryResult)]
struct RecipientRow {
    email: String,
    user_id: Option<Uuid>,
    status: Option<String>,
    role: Option<String>,
    timezone: Option<String>,
    display_name: Option<String>,
    username: Option<String>,
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
    let startable = matches!(
        conference.status,
        ConferenceStatus::Created | ConferenceStatus::Preparing | ConferenceStatus::Waiting
    );
    // A notice is only sent while it is still true.
    match job.kind.as_str() {
        KIND_REMINDER => {
            let start = payload_time(&job.payload, "scheduledStartAt");
            if !startable
                || conference.scheduled_start_at != start
                || start.is_none_or(|s| s <= now)
            {
                return Ok(Outcome::Skipped("SUPERSEDED"));
            }
        }
        KIND_RESCHEDULED => {
            if conference.status != ConferenceStatus::Created
                || conference.scheduled_start_at != payload_time(&job.payload, "to")
            {
                return Ok(Outcome::Skipped("SUPERSEDED"));
            }
        }
        KIND_CANCELLED => {
            if conference.status != ConferenceStatus::Cancelled {
                return Ok(Outcome::Skipped("SUPERSEDED"));
            }
        }
        _ => return Ok(Outcome::Skipped("UNKNOWN_KIND")),
    }

    let limit = ctx.config.max_recipients as i64 + 1;
    let mut rows = RecipientRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        RECIPIENTS_SQL,
        [space.id.into(), conference.id.into(), limit.into()],
    ))
    .all(&ctx.db)
    .await?;
    if rows.len() > ctx.config.max_recipients {
        tracing::warn!(notification_id = %job.id, max = ctx.config.max_recipients, "notification recipients truncated");
        rows.truncate(ctx.config.max_recipients);
    }
    rows.retain(|r| {
        r.user_id.is_none()
            || (r.user_id != job.actor_user_id
                && !matches!(r.status.as_deref(), Some("inactive" | "suspended"))
                && r.role.as_deref() != Some("guest"))
    });
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
        if ctx
            .notifier
            .mail
            .send(OutgoingEmail {
                to: r.email.clone(),
                to_name: name,
                subject: email.0,
                html: email.1,
                text: email.2,
            })
            .await
            .is_err()
        {
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
            to: "a@b.c".into(),
            to_name: None,
            subject: "s".into(),
            html: "h".into(),
            text: "t".into(),
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
