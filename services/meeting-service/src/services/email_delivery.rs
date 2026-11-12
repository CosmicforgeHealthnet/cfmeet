//! Unified transactional email delivery (final production completion, Section 2).
//!
//! ```text
//! business event (auth, meetings, organizations, developer platform …)
//!   → EmailService::enqueue (template rendered by the backend, idempotency key)
//!   → email_deliveries (PostgreSQL: durable queue + ledger, QUEUED)
//!   → EmailWorker (lease FOR UPDATE SKIP LOCKED, rate limit, retries)
//!   → EmailProvider (Resend HTTPS API | SMTP | none)
//!   → provider webhooks (Svix-signed) → DELIVERED / BOUNCED / COMPLAINED / FAILED
//! ```
//!
//! The Meeting Service is the single email authority of CF Meet (no other service sends
//! email), so every category goes through this one ledger and no event is emailed twice
//! (`idempotency_key` is unique; the provider receives the ledger id as its own
//! `Idempotency-Key`, so a crash between the provider call and the commit never sends a
//! second copy within the provider's idempotency window).
//!
//! Status semantics (never more than the evidence):
//! * `QUEUED` — recorded, waiting for the worker (or for a provider to be configured).
//! * `SENDING` — leased by a worker; a lease that expires is re-claimed (restart safe).
//! * `ACCEPTED` — the provider accepted the message (HTTP success). Not inbox receipt.
//! * `DELIVERED` / `DELAYED` / `BOUNCED` / `COMPLAINED` — provider webhook evidence.
//! * `FAILED` — permanent rejection or attempts exhausted; `EXPIRED` — the message (e.g.
//!   a one-time code) was no longer useful when it could be sent.
//!
//! Message content is erased from the row when it reaches a final state; addresses are
//! erased after `EMAIL_LEDGER_RETENTION_DAYS` (the SHA-256 of the address stays for audit).
//! Logs never contain addresses, codes or content.

use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, Value as DbValue,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    config::{email::valid_address, EmailConfig, EmailProviderKind},
    error::{ApiError, ApiResult},
    services::ops_metrics::{inc, OPS_METRICS as OM},
};

/// Email categories (stable, upper snake case; stored and used as provider tags).
pub mod category {
    pub const AUTH_VERIFICATION: &str = "AUTH_VERIFICATION";
    pub const AUTH_PASSWORD_RESET: &str = "AUTH_PASSWORD_RESET";
    pub const AUTH_WELCOME: &str = "AUTH_WELCOME";
    pub const SECURITY_PASSWORD_CHANGED: &str = "SECURITY_PASSWORD_CHANGED";
    pub const MEETING_INVITATION: &str = "MEETING_INVITATION";
    pub const MEETING_NOTIFICATION: &str = "MEETING_NOTIFICATION";
    pub const ORGANIZATION_INVITATION: &str = "ORGANIZATION_INVITATION";
    pub const DEVELOPER_PLATFORM: &str = "DEVELOPER_PLATFORM";
    pub const OPERATIONS_ACCEPTANCE: &str = "OPERATIONS_ACCEPTANCE";
    pub const NOTIFICATION: &str = "NOTIFICATION";
    pub const LEGACY_QUEUE: &str = "LEGACY_QUEUE";
}

/// Statuses that are final (content erased, never re-sent).
pub const FINAL_STATUSES: &[&str] = &[
    "ACCEPTED",
    "DELIVERED",
    "DELAYED",
    "BOUNCED",
    "COMPLAINED",
    "FAILED",
    "EXPIRED",
];

const LEASE_SECS: i64 = 90;
const BACKOFF_BASE_SECS: i64 = 5;
const BACKOFF_CAP_SECS: i64 = 15 * 60;

pub fn recipient_hash(address: &str) -> String {
    hex::encode(Sha256::digest(
        address.trim().to_ascii_lowercase().as_bytes(),
    ))
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

/// One message to enqueue. Content is rendered by the caller (backend-owned templates).
#[derive(Debug, Clone)]
pub struct NewEmail {
    pub category: &'static str,
    /// Business idempotency key (e.g. `otp:verify:<user>:<code digest>`); a random key is
    /// used when `None` (no deduplication beyond the row itself).
    pub idempotency_key: Option<String>,
    pub to: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    /// Do not send after this instant (e.g. the one-time code expired).
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub id: Uuid,
    /// `false`: the idempotency key was already recorded (no second email).
    pub created: bool,
}

fn clean_header(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Record a message in the durable ledger (QUEUED). Idempotent on `idempotency_key`.
pub async fn enqueue<C: ConnectionTrait>(conn: &C, email: NewEmail) -> ApiResult<Enqueued> {
    let to = email.to.trim().to_string();
    if !valid_address(&to) {
        return Err(ApiError::BadRequest(
            "Invalid recipient email address".into(),
        ));
    }
    let subject = clean_header(&email.subject, 200);
    if subject.is_empty() {
        return Err(ApiError::InternalError("Email subject is empty".into()));
    }
    let to_name = email
        .to_name
        .as_deref()
        .map(|n| clean_header(n, 120).replace(['<', '>', '"'], ""))
        .filter(|n| !n.is_empty());
    let id = Uuid::new_v4();
    let key = email
        .idempotency_key
        .map(|k| clean_header(&k, 160))
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| format!("msg:{id}"));
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO email_deliveries (id, category, idempotency_key, recipient_hash, recipient,
                 recipient_name, subject, html_body, text_body, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (idempotency_key) DO NOTHING
             RETURNING id",
            [
                id.into(),
                email.category.into(),
                key.clone().into(),
                recipient_hash(&to).into(),
                to.into(),
                to_name.into(),
                subject.into(),
                email.html.into(),
                email.text.into(),
                email.expires_at.into(),
            ],
        ))
        .await?;
    if row.is_some() {
        inc(&OM.email_enqueued);
        return Ok(Enqueued { id, created: true });
    }
    inc(&OM.email_enqueue_duplicates);
    let existing = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM email_deliveries WHERE idempotency_key = $1",
            [key.into()],
        ))
        .await?
        .ok_or_else(|| ApiError::InternalError("email ledger conflict without a row".into()))?;
    Ok(Enqueued {
        id: existing.try_get("", "id")?,
        created: false,
    })
}

/// Public, PII-free view of one delivery (operations and acceptance).
#[derive(Debug, Clone, FromQueryResult, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryStatus {
    pub id: Uuid,
    pub category: String,
    pub status: String,
    pub provider: Option<String>,
    pub attempts: i32,
    pub last_error_class: Option<String>,
    pub last_error_status: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

pub async fn status<C: ConnectionTrait>(conn: &C, id: Uuid) -> ApiResult<Option<DeliveryStatus>> {
    Ok(
        DeliveryStatus::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, category, status, provider, attempts, last_error_class, last_error_status,
                created_at, accepted_at, delivered_at
           FROM email_deliveries WHERE id = $1",
            [id.into()],
        ))
        .one(conn)
        .await?,
    )
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

pub struct ProviderMessage<'a> {
    pub id: Uuid,
    pub category: &'a str,
    pub from: &'a str,
    pub reply_to: Option<&'a str>,
    pub to: &'a str,
    pub to_name: Option<&'a str>,
    pub subject: &'a str,
    pub html: &'a str,
    pub text: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Retry later (network, timeout, 429, 5xx, provider idempotency race, configuration
    /// that an operator can still fix such as an unverified domain or a revoked key).
    Temporary {
        class: &'static str,
        status: Option<u16>,
        retry_after_secs: Option<i64>,
    },
    /// Never retry this message (invalid recipient or request).
    Permanent {
        class: &'static str,
        status: Option<u16>,
    },
}

impl ProviderError {
    fn class(&self) -> &'static str {
        match self {
            Self::Temporary { class, .. } | Self::Permanent { class, .. } => class,
        }
    }
    fn status(&self) -> Option<u16> {
        match self {
            Self::Temporary { status, .. } | Self::Permanent { status, .. } => *status,
        }
    }
}

#[async_trait]
pub trait EmailProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Send one message; `Ok(Some(id))` carries the provider message id.
    async fn send(&self, message: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError>;
}

/// Resend HTTPS API (`POST /emails`), with the ledger id as `Idempotency-Key`.
pub struct ResendProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ResendProvider {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(20))
            .connect_timeout(StdDuration::from_secs(10))
            .user_agent("cfmeet-email-worker/1")
            .build()
            .map_err(|_| "cannot build the Resend HTTP client".to_string())?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

    /// Request body (separate for tests).
    pub fn body(m: &ProviderMessage<'_>) -> Value {
        let to = match m.to_name {
            Some(name) => format!("{name} <{}>", m.to),
            None => m.to.to_string(),
        };
        let mut body = json!({
            "from": m.from,
            "to": [to],
            "subject": m.subject,
            "html": m.html,
            "text": m.text,
            "tags": [{ "name": "category", "value": m.category.to_ascii_lowercase() }],
        });
        if let Some(r) = m.reply_to {
            body["reply_to"] = json!(r);
        }
        body
    }

    /// Classify an HTTP status (Resend API errors).
    pub fn classify(status: u16, retry_after_secs: Option<i64>, error_name: &str) -> ProviderError {
        match status {
            429 => ProviderError::Temporary {
                class: "RATE_LIMITED",
                status: Some(status),
                retry_after_secs,
            },
            409 if error_name == "concurrent_idempotent_requests" => ProviderError::Temporary {
                class: "IDEMPOTENCY_IN_PROGRESS",
                status: Some(status),
                retry_after_secs: Some(10),
            },
            409 => ProviderError::Permanent {
                class: "IDEMPOTENCY_CONFLICT",
                status: Some(status),
            },
            // Missing/invalid/restricted key or unverified sending domain: an operator fix,
            // so retry with backoff (bounded by attempts and the message expiry).
            401 | 403 => ProviderError::Temporary {
                class: "PROVIDER_CONFIGURATION",
                status: Some(status),
                retry_after_secs: None,
            },
            400 | 404 | 405 | 413 | 422 => ProviderError::Permanent {
                class: "REJECTED",
                status: Some(status),
            },
            s if s >= 500 => ProviderError::Temporary {
                class: "PROVIDER_UNAVAILABLE",
                status: Some(status),
                retry_after_secs: None,
            },
            _ => ProviderError::Temporary {
                class: "UNEXPECTED_STATUS",
                status: Some(status),
                retry_after_secs: None,
            },
        }
    }
}

#[async_trait]
impl EmailProvider for ResendProvider {
    fn name(&self) -> &'static str {
        "resend"
    }

    async fn send(&self, m: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError> {
        let response = self
            .client
            .post(format!("{}/emails", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Idempotency-Key", m.id.to_string())
            .json(&Self::body(m))
            .send()
            .await
            .map_err(|e| ProviderError::Temporary {
                class: if e.is_timeout() { "TIMEOUT" } else { "NETWORK" },
                status: None,
                retry_after_secs: None,
            })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok());
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            let id = body
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 128)
                .map(str::to_string);
            return Ok(id);
        }
        let name = body.get("name").and_then(Value::as_str).unwrap_or("");
        Err(Self::classify(status, retry_after, name))
    }
}

/// SMTP (legacy transport; e.g. a Gmail relay).
pub struct SmtpProvider {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
}

impl SmtpProvider {
    pub fn new(config: &EmailConfig) -> Result<Self, String> {
        use lettre::{transport::smtp::authentication::Credentials, AsyncSmtpTransport};
        let credentials =
            Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());
        let builder = if config.smtp_port == 465 {
            AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&config.smtp_host)
        } else {
            AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&config.smtp_host)
        }
        .map_err(|_| "cannot create the SMTP transport".to_string())?;
        Ok(Self {
            mailer: builder
                .port(config.smtp_port)
                .credentials(credentials)
                .timeout(Some(StdDuration::from_secs(30)))
                .build(),
        })
    }
}

#[async_trait]
impl EmailProvider for SmtpProvider {
    fn name(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, m: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError> {
        use lettre::{
            message::{header::ContentType, Mailbox, MultiPart, SinglePart},
            AsyncTransport, Message,
        };
        let rejected = |class| ProviderError::Permanent {
            class,
            status: None,
        };
        let from: Mailbox = m.from.parse().map_err(|_| rejected("INVALID_SENDER"))?;
        let to: Mailbox = match m.to_name {
            Some(n) => format!("{n} <{}>", m.to),
            None => m.to.to_string(),
        }
        .parse()
        .map_err(|_| rejected("INVALID_RECIPIENT"))?;
        let mut builder = Message::builder()
            .from(from)
            .to(to)
            .subject(m.subject)
            .message_id(Some(format!("<{}@cfmeet.com>", m.id)));
        if let Some(r) = m.reply_to.and_then(|r| r.parse::<Mailbox>().ok()) {
            builder = builder.reply_to(r);
        }
        let email = builder
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(m.text.to_string()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(m.html.to_string()),
                    ),
            )
            .map_err(|_| rejected("INVALID_MESSAGE"))?;
        match self.mailer.send(email).await {
            Ok(_) => Ok(None),
            Err(e) if e.is_permanent() => {
                // 535 BadCredentials is permanent at the SMTP level but an operator fix.
                let auth = e.to_string().contains("535");
                Err(if auth {
                    ProviderError::Temporary {
                        class: "SMTP_AUTHENTICATION",
                        status: Some(535),
                        retry_after_secs: None,
                    }
                } else {
                    rejected("SMTP_REJECTED")
                })
            }
            Err(_) => Err(ProviderError::Temporary {
                class: "SMTP_TRANSIENT",
                status: None,
                retry_after_secs: None,
            }),
        }
    }
}

/// In-process provider for tests: records messages; can fail on demand.
#[derive(Default)]
pub struct RecordingProvider {
    pub sent: std::sync::Mutex<Vec<(Uuid, String, String, String)>>,
    pub fail_next: std::sync::Mutex<Vec<ProviderError>>,
}

impl RecordingProvider {
    /// (ledger id, to, subject, text) of every accepted message.
    pub fn sent(&self) -> Vec<(Uuid, String, String, String)> {
        self.sent.lock().expect("recording provider").clone()
    }
    pub fn fail_with(&self, errors: Vec<ProviderError>) {
        *self.fail_next.lock().expect("recording provider") = errors;
    }
}

#[async_trait]
impl EmailProvider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn send(&self, m: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError> {
        {
            let mut f = self.fail_next.lock().expect("recording provider");
            if !f.is_empty() {
                return Err(f.remove(0));
            }
        }
        self.sent.lock().expect("recording provider").push((
            m.id,
            m.to.to_string(),
            m.subject.to_string(),
            m.text.to_string(),
        ));
        Ok(Some(format!("rec_{}", m.id.simple())))
    }
}

/// The configured provider (`None`: messages stay QUEUED).
pub fn provider_from_config(
    config: &EmailConfig,
) -> Result<Option<Arc<dyn EmailProvider>>, String> {
    Ok(match config.provider {
        EmailProviderKind::Resend => Some(Arc::new(ResendProvider::new(
            &config.resend_api_url,
            &config.resend_api_key,
        )?)),
        EmailProviderKind::Smtp => Some(Arc::new(SmtpProvider::new(config)?)),
        EmailProviderKind::None => None,
    })
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WorkerSettings {
    pub from: String,
    pub reply_to: Option<String>,
    pub max_attempts: i32,
    pub max_per_second: u32,
    pub batch: u64,
    pub retention_days: i64,
}

impl WorkerSettings {
    pub fn from_config(c: &EmailConfig) -> Self {
        Self {
            from: c.from_header(),
            reply_to: c.reply_to.clone(),
            max_attempts: c.max_attempts,
            max_per_second: c.max_per_second,
            batch: c.batch,
            retention_days: c.retention_days,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: u64,
    pub accepted: u64,
    pub retried: u64,
    pub failed: u64,
    pub expired: u64,
}

#[derive(Debug, FromQueryResult)]
struct Claimed {
    id: Uuid,
    category: String,
    recipient: Option<String>,
    recipient_name: Option<String>,
    subject: String,
    html_body: Option<String>,
    text_body: Option<String>,
    attempts: i32,
    expires_at: Option<DateTime<Utc>>,
}

/// Exponential backoff with ±20 % jitter: 5 s, 10 s, 20 s … capped at 15 minutes.
pub fn backoff(attempts: i32) -> Duration {
    let exp = (attempts.max(1) - 1).min(16) as u32;
    let secs = (BACKOFF_BASE_SECS * 2i64.pow(exp)).min(BACKOFF_CAP_SECS);
    let jitter = (secs as f64 * 0.2 * (rand::random::<f64>() * 2.0 - 1.0)) as i64;
    Duration::seconds((secs + jitter).max(1))
}

pub struct EmailWorker {
    db: DatabaseConnection,
    provider: Arc<dyn EmailProvider>,
    settings: WorkerSettings,
    holder: String,
}

impl EmailWorker {
    pub fn new(
        db: DatabaseConnection,
        provider: Arc<dyn EmailProvider>,
        settings: WorkerSettings,
    ) -> Self {
        Self {
            db,
            provider,
            settings,
            holder: format!("email-{}", Uuid::new_v4().simple()),
        }
    }

    async fn claim(&self) -> ApiResult<Vec<Claimed>> {
        Ok(Claimed::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE email_deliveries d
                SET status = 'SENDING', lease_holder = $1,
                    lease_expires_at = now() + make_interval(secs => $2),
                    attempts = d.attempts + 1, updated_at = now()
              WHERE d.id IN (
                    SELECT id FROM email_deliveries
                     WHERE (status = 'QUEUED' AND next_attempt_at <= now())
                        OR (status = 'SENDING' AND lease_expires_at < now())
                     ORDER BY next_attempt_at
                     LIMIT $3
                     FOR UPDATE SKIP LOCKED)
          RETURNING d.id, d.category, d.recipient, d.recipient_name, d.subject, d.html_body,
                    d.text_body, d.attempts, d.expires_at",
            [
                self.holder.clone().into(),
                LEASE_SECS.into(),
                (self.settings.batch as i64).into(),
            ],
        ))
        .all(&self.db)
        .await?)
    }

    /// Final state: content erased, lease released. Only the lease holder may finish.
    async fn finish(
        &self,
        id: Uuid,
        status: &str,
        provider_message_id: Option<String>,
        error: Option<&ProviderError>,
    ) -> ApiResult<()> {
        let accepted = matches!(status, "ACCEPTED");
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE email_deliveries
                    SET status = $3, provider = $4,
                        provider_message_id = COALESCE($5, provider_message_id),
                        html_body = NULL, text_body = NULL,
                        lease_holder = NULL, lease_expires_at = NULL,
                        last_error_class = $6, last_error_status = $7,
                        accepted_at = CASE WHEN $8 THEN now() ELSE accepted_at END,
                        final_at = now(), updated_at = now()
                  WHERE id = $1 AND lease_holder = $2",
                [
                    id.into(),
                    self.holder.clone().into(),
                    status.into(),
                    self.provider.name().into(),
                    provider_message_id.into(),
                    error.map(|e| e.class().to_string()).into(),
                    error.and_then(|e| e.status()).map(|s| s as i32).into(),
                    accepted.into(),
                ],
            ))
            .await?;
        Ok(())
    }

    async fn reschedule(
        &self,
        id: Uuid,
        at: DateTime<Utc>,
        error: &ProviderError,
    ) -> ApiResult<()> {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE email_deliveries
                    SET status = 'QUEUED', next_attempt_at = $3, provider = $4,
                        lease_holder = NULL, lease_expires_at = NULL,
                        last_error_class = $5, last_error_status = $6, updated_at = now()
                  WHERE id = $1 AND lease_holder = $2",
                [
                    id.into(),
                    self.holder.clone().into(),
                    at.into(),
                    self.provider.name().into(),
                    error.class().into(),
                    error.status().map(|s| s as i32).into(),
                ],
            ))
            .await?;
        Ok(())
    }

    /// One pass: claim due messages, send them (rate limited), record the outcome.
    pub async fn tick(&self, now: DateTime<Utc>) -> ApiResult<TickReport> {
        let claimed = self.claim().await?;
        let mut report = TickReport {
            claimed: claimed.len() as u64,
            ..Default::default()
        };
        let spacing = StdDuration::from_millis(1000 / self.settings.max_per_second.max(1) as u64);
        for (n, job) in claimed.into_iter().enumerate() {
            if job.expires_at.is_some_and(|e| e <= now) {
                self.finish(job.id, "EXPIRED", None, None).await?;
                inc(&OM.email_expired);
                report.expired += 1;
                continue;
            }
            let (Some(to), Some(html), Some(text)) = (
                job.recipient.as_deref(),
                job.html_body.as_deref(),
                job.text_body.as_deref(),
            ) else {
                // Content already erased: nothing can be sent again.
                self.finish(
                    job.id,
                    "FAILED",
                    None,
                    Some(&ProviderError::Permanent {
                        class: "CONTENT_UNAVAILABLE",
                        status: None,
                    }),
                )
                .await?;
                report.failed += 1;
                continue;
            };
            if n > 0 {
                tokio::time::sleep(spacing).await;
            }
            let message = ProviderMessage {
                id: job.id,
                category: &job.category,
                from: &self.settings.from,
                reply_to: self.settings.reply_to.as_deref(),
                to,
                to_name: job.recipient_name.as_deref(),
                subject: &job.subject,
                html,
                text,
            };
            match self.provider.send(&message).await {
                Ok(provider_id) => {
                    self.finish(job.id, "ACCEPTED", provider_id, None).await?;
                    inc(&OM.email_provider_accepted);
                    tracing::info!(delivery_id = %job.id, category = %job.category,
                        provider = self.provider.name(), "email accepted by provider");
                    report.accepted += 1;
                }
                Err(e @ ProviderError::Permanent { .. }) => {
                    self.finish(job.id, "FAILED", None, Some(&e)).await?;
                    inc(&OM.email_provider_permanent_failures);
                    inc(&OM.email_failed);
                    tracing::warn!(delivery_id = %job.id, category = %job.category,
                        provider = self.provider.name(), error_class = e.class(),
                        status = ?e.status(), "email rejected permanently");
                    report.failed += 1;
                }
                Err(e @ ProviderError::Temporary { .. }) => {
                    inc(&OM.email_provider_temporary_failures);
                    let retry_after = match &e {
                        ProviderError::Temporary {
                            retry_after_secs: Some(s),
                            ..
                        } => Some(Duration::seconds((*s).clamp(1, BACKOFF_CAP_SECS))),
                        _ => None,
                    };
                    let next = now + retry_after.unwrap_or_else(|| backoff(job.attempts));
                    let expired = job.expires_at.is_some_and(|x| x <= next);
                    if job.attempts >= self.settings.max_attempts || expired {
                        self.finish(
                            job.id,
                            if expired { "EXPIRED" } else { "FAILED" },
                            None,
                            Some(&e),
                        )
                        .await?;
                        inc(if expired {
                            &OM.email_expired
                        } else {
                            &OM.email_failed
                        });
                        tracing::warn!(delivery_id = %job.id, category = %job.category,
                            provider = self.provider.name(), error_class = e.class(),
                            status = ?e.status(), attempts = job.attempts,
                            "email not delivered: attempts exhausted or expired");
                        report.failed += 1;
                    } else {
                        self.reschedule(job.id, next, &e).await?;
                        tracing::warn!(delivery_id = %job.id, category = %job.category,
                            provider = self.provider.name(), error_class = e.class(),
                            status = ?e.status(), attempts = job.attempts,
                            "email delivery failed temporarily; retry scheduled");
                        report.retried += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Erase addresses of old final rows; delete rows past twice the retention.
    pub async fn purge(&self) -> ApiResult<()> {
        let days = self.settings.retention_days;
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE email_deliveries SET recipient = NULL, recipient_name = NULL
                  WHERE final_at < now() - make_interval(days => $1) AND recipient IS NOT NULL",
                [days.into()],
            ))
            .await?;
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM email_deliveries WHERE final_at < now() - make_interval(days => $1)",
                [(days * 2).into()],
            ))
            .await?;
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM email_delivery_events WHERE received_at < now() - make_interval(days => $1)",
                [(days * 2).into()],
            ))
            .await?;
        Ok(())
    }

    pub async fn run(&self, poll: StdDuration, mut shutdown: watch::Receiver<bool>) {
        tracing::info!(provider = self.provider.name(), from = %self.settings.from,
            "email worker started");
        let mut last_purge = Utc::now() - Duration::hours(2);
        loop {
            if *shutdown.borrow() {
                break;
            }
            let busy = match self.tick(Utc::now()).await {
                Ok(r) => r.claimed > 0,
                Err(e) => {
                    tracing::error!("email worker tick failed: {e}");
                    false
                }
            };
            if Utc::now() - last_purge > Duration::hours(1) {
                if let Err(e) = self.purge().await {
                    tracing::warn!("email ledger purge failed: {e}");
                }
                last_purge = Utc::now();
            }
            if !busy {
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
        tracing::info!("email worker stopped");
    }
}

pub struct EmailWorkerHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl EmailWorkerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

pub fn spawn_email_worker(worker: EmailWorker, poll: StdDuration) -> EmailWorkerHandle {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move { worker.run(poll, shutdown_rx).await });
    EmailWorkerHandle { shutdown_tx }
}

/// Move messages left in the former volatile Redis list (`email:queue`) into the durable
/// ledger once, so nothing queued before the upgrade is lost. They expire after an hour
/// (codes in them may be stale).
pub async fn import_legacy_redis_queue(
    db: &DatabaseConnection,
    queue: &crate::queues::RedisEmailQueue,
) -> ApiResult<u64> {
    let mut moved = 0;
    while let Some(job) = queue.pop_nowait().await? {
        let r = enqueue(
            db,
            NewEmail {
                category: category::LEGACY_QUEUE,
                idempotency_key: Some(format!("legacy-redis:{}", job.id)),
                to: job.to_email,
                to_name: job.to_name,
                subject: job.subject,
                html: job.html_body,
                text: job.text_body,
                expires_at: Some(Utc::now() + Duration::hours(1)),
            },
        )
        .await;
        match r {
            Ok(_) => moved += 1,
            Err(ApiError::BadRequest(_)) => {}
            Err(e) => return Err(e),
        }
        if moved >= 10_000 {
            break;
        }
    }
    Ok(moved)
}

// ---------------------------------------------------------------------------
// Provider webhooks (Resend, Svix signatures)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookError {
    Unauthorized,
    Malformed,
}

/// Verify a Svix-signed webhook (`svix-id`, `svix-timestamp`, `svix-signature`):
/// HMAC-SHA256 over `{id}.{timestamp}.{body}` with the base64 secret after `whsec_`,
/// timestamp within ±5 minutes, any `v1,` signature in the header may match.
pub fn verify_svix(
    secret: &str,
    id: &str,
    timestamp: &str,
    signatures: &str,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<(), WebhookError> {
    let key = base64::engine::general_purpose::STANDARD
        .decode(
            secret
                .trim()
                .strip_prefix("whsec_")
                .ok_or(WebhookError::Unauthorized)?,
        )
        .map_err(|_| WebhookError::Unauthorized)?;
    let ts: i64 = timestamp
        .trim()
        .parse()
        .map_err(|_| WebhookError::Unauthorized)?;
    if (now.timestamp() - ts).abs() > 300 || id.is_empty() || id.len() > 128 {
        return Err(WebhookError::Unauthorized);
    }
    for candidate in signatures.split_whitespace() {
        let Some(sig) = candidate.strip_prefix("v1,") else {
            continue;
        };
        let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(sig) else {
            continue;
        };
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&key).map_err(|_| WebhookError::Unauthorized)?;
        mac.update(id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.trim().as_bytes());
        mac.update(b".");
        mac.update(body);
        if mac.verify_slice(&sig).is_ok() {
            return Ok(());
        }
    }
    Err(WebhookError::Unauthorized)
}

/// Rank of a provider-evidenced status (never downgrade).
fn rank(status: &str) -> i32 {
    match status {
        "QUEUED" | "SENDING" => 0,
        "ACCEPTED" => 1,
        "DELAYED" => 2,
        "DELIVERED" => 3,
        "BOUNCED" | "COMPLAINED" | "FAILED" => 4,
        _ => 0,
    }
}

/// Apply one verified provider event. Returns `false` for a duplicate event id.
pub async fn apply_provider_event(
    db: &DatabaseConnection,
    provider: &str,
    event_id: &str,
    payload: &Value,
) -> Result<bool, WebhookError> {
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| t.len() <= 48)
        .ok_or(WebhookError::Malformed)?;
    let message_id = payload
        .pointer("/data/email_id")
        .and_then(Value::as_str)
        .filter(|m| m.len() <= 128);
    let occurred_at = payload
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let target = match event_type {
        "email.delivered" => Some("DELIVERED"),
        "email.delivery_delayed" => Some("DELAYED"),
        "email.bounced" => Some("BOUNCED"),
        "email.complained" => Some("COMPLAINED"),
        "email.failed" => Some("FAILED"),
        _ => None, // email.sent, opened, clicked …: recorded, no status change
    };
    let internal = |_| WebhookError::Malformed;
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO email_delivery_events (provider, event_id, event_type, provider_message_id,
                 delivery_id, occurred_at)
             VALUES ($1, $2, $3, $4,
                 (SELECT id FROM email_deliveries WHERE provider = $1 AND provider_message_id = $4),
                 $5)
             ON CONFLICT (provider, event_id) DO NOTHING
             RETURNING delivery_id",
            [
                provider.into(),
                event_id.into(),
                event_type.into(),
                message_id.map(str::to_string).into(),
                occurred_at.into(),
            ],
        ))
        .await
        .map_err(internal)?;
    let Some(row) = row else {
        inc(&OM.email_webhook_duplicates);
        return Ok(false);
    };
    let delivery: Option<Uuid> = row.try_get("", "delivery_id").map_err(internal)?;
    if let (Some(id), Some(target)) = (delivery, target) {
        let lower: Vec<DbValue> = FINAL_STATUSES
            .iter()
            .chain(["QUEUED", "SENDING"].iter())
            .filter(|s| rank(s) < rank(target))
            .map(|s| (*s).into())
            .collect();
        let placeholders: Vec<String> = (0..lower.len()).map(|i| format!("${}", i + 3)).collect();
        let mut values: Vec<DbValue> = vec![id.into(), target.into()];
        values.extend(lower);
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE email_deliveries
                    SET status = $2, updated_at = now(),
                        delivered_at = CASE WHEN $2 = 'DELIVERED' THEN now() ELSE delivered_at END,
                        final_at = COALESCE(final_at, now()), html_body = NULL, text_body = NULL
                  WHERE id = $1 AND status IN ({})",
                placeholders.join(", ")
            ),
            values,
        ))
        .await
        .map_err(internal)?;
        match target {
            "DELIVERED" => inc(&OM.email_delivered),
            "BOUNCED" => inc(&OM.email_bounced),
            "COMPLAINED" => inc(&OM.email_complained),
            "FAILED" => inc(&OM.email_failed),
            _ => {}
        }
        tracing::info!(delivery_id = %id, event_type, "email provider event applied");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret_b64: &str, id: &str, ts: &str, body: &[u8]) -> String {
        let key = base64::engine::general_purpose::STANDARD
            .decode(secret_b64)
            .unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(format!("{id}.{ts}.").as_bytes());
        mac.update(body);
        format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        )
    }

    #[test]
    fn svix_signatures_are_verified() {
        let raw =
            base64::engine::general_purpose::STANDARD.encode(b"0123456789abcdef0123456789abcdef");
        let secret = format!("whsec_{raw}");
        let now = Utc::now();
        let ts = now.timestamp().to_string();
        let body = br#"{"type":"email.delivered"}"#;
        let good = sign(&raw, "msg_1", &ts, body);
        assert!(verify_svix(&secret, "msg_1", &ts, &format!("v1,AAAA {good}"), body, now).is_ok());
        assert_eq!(
            verify_svix(&secret, "msg_2", &ts, &good, body, now),
            Err(WebhookError::Unauthorized)
        );
        assert!(verify_svix(&secret, "msg_1", &ts, &good, b"{}", now).is_err());
        let old = (now.timestamp() - 600).to_string();
        let stale = sign(&raw, "msg_1", &old, body);
        assert!(verify_svix(&secret, "msg_1", &old, &stale, body, now).is_err());
        assert!(verify_svix("nope", "msg_1", &ts, &good, body, now).is_err());
    }

    #[test]
    fn resend_errors_are_classified() {
        assert!(matches!(
            ResendProvider::classify(429, Some(3), "rate_limit_exceeded"),
            ProviderError::Temporary {
                retry_after_secs: Some(3),
                ..
            }
        ));
        assert!(matches!(
            ResendProvider::classify(422, None, "validation_error"),
            ProviderError::Permanent { .. }
        ));
        assert!(matches!(
            ResendProvider::classify(403, None, "invalid_from_address"),
            ProviderError::Temporary {
                class: "PROVIDER_CONFIGURATION",
                ..
            }
        ));
        assert!(matches!(
            ResendProvider::classify(503, None, ""),
            ProviderError::Temporary { .. }
        ));
        assert!(matches!(
            ResendProvider::classify(409, None, "concurrent_idempotent_requests"),
            ProviderError::Temporary { .. }
        ));
        assert!(matches!(
            ResendProvider::classify(409, None, "invalid_idempotent_request"),
            ProviderError::Permanent { .. }
        ));
    }

    #[test]
    fn resend_body_has_both_parts_and_a_category_tag() {
        let id = Uuid::new_v4();
        let b = ResendProvider::body(&ProviderMessage {
            id,
            category: "AUTH_VERIFICATION",
            from: "CF Meet <no-reply@cfmeet.com>",
            reply_to: None,
            to: "ada@example.com",
            to_name: Some("Ada"),
            subject: "Verify",
            html: "<p>1</p>",
            text: "1",
        });
        assert_eq!(b["from"], "CF Meet <no-reply@cfmeet.com>");
        assert_eq!(b["to"][0], "Ada <ada@example.com>");
        assert_eq!(b["text"], "1");
        assert_eq!(b["tags"][0]["value"], "auth_verification");
        assert!(b.get("reply_to").is_none());
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert!(backoff(1).num_seconds() <= 6);
        assert!(backoff(4).num_seconds() >= 32);
        assert!(backoff(30).num_seconds() <= BACKOFF_CAP_SECS + BACKOFF_CAP_SECS / 5);
    }

    #[test]
    fn ranks_never_downgrade_delivered() {
        assert!(rank("DELAYED") < rank("DELIVERED"));
        assert!(rank("DELIVERED") < rank("COMPLAINED"));
    }

    #[test]
    fn recipient_hash_is_case_insensitive() {
        assert_eq!(
            recipient_hash("Ada@Example.com "),
            recipient_hash("ada@example.com")
        );
    }
}
