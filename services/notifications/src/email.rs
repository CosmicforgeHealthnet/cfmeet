//! Unified email channel (roadmap N5; Document 4 §14.1): a port of the Meeting Service
//! ledger (`services/meeting-service/src/services/email_delivery.rs`) with its known
//! defects fixed.
//!
//! ```text
//! W1 (notification → EMAIL delivery) → enqueue (sealed subject/body/address, UUIDv5 id)
//!   → notif_email_messages (QUEUED) → EmailWorker per lane (CRITICAL / STANDARD;
//!     FOR UPDATE SKIP LOCKED, renewed leases, rate limit, provider breaker)
//!   → [CRITICAL with a route decision: X6 lease → send ≤ 20 s → X7 completion]
//!   → EmailProvider (Resend | SMTP | none) → provider webhooks (Svix) → DELIVERED / …
//! ```
//!
//! Preserved: the `EmailProvider` trait and its Resend / SMTP / recording adapters, the
//! error classification, the idempotency-key formats, the Svix verification and the
//! monotonic status rules. Fixed: deterministic message ids (UUIDv5 of the key, so a
//! re-created row reuses the provider `Idempotency-Key`), sealed subject / body /
//! address, keyed recipient digests, mandatory expiry, a CRITICAL lane that a STANDARD
//! backlog cannot starve, a suppression table, and per-row lease renewal.

use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, Value as DbValue};
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    authorities::Authorities,
    config::{valid_address, EmailConfig, EmailProviderKind},
    crypto::DataKeys,
    db,
    metrics::{label, Heartbeats, Metrics},
};

/// Namespace of deterministic message ids (UUIDv5 of the idempotency key).
pub const MESSAGE_NAMESPACE: Uuid = Uuid::from_u128(0x6c1f_8a0e_3b5d_4e47_9a2c_cf3e_e7a1_0001);

pub const FINAL_STATUSES: &[&str] = &[
    "ACCEPTED",
    "DELIVERED",
    "DELAYED",
    "BOUNCED",
    "COMPLAINED",
    "FAILED",
    "EXPIRED",
    "SUPPRESSED",
];

const LEASE_SECS: i64 = 120;
const BACKOFF_BASE_SECS: i64 = 5;
const BACKOFF_CAP_SECS: i64 = 15 * 60;
/// CRITICAL lease length requested from Account (X6) and the provider deadline inside it.
pub const CRITICAL_LEASE_SECS: u32 = 30;
const CRITICAL_SEND_TIMEOUT: StdDuration = StdDuration::from_secs(20);
const BREAKER_THRESHOLD: u32 = 5;
const BREAKER_OPEN_SECS: i64 = 60;

pub fn message_id(idempotency_key: &str) -> Uuid {
    Uuid::new_v5(&MESSAGE_NAMESPACE, idempotency_key.as_bytes())
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

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewEmail {
    pub category: String,
    /// `CRITICAL` or `STANDARD`.
    pub lane: &'static str,
    pub idempotency_key: String,
    pub route_decision_id: Option<Uuid>,
    pub sender_profile: String,
    pub delivery_id: Option<Uuid>,
    pub to: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub id: Uuid,
    pub created: bool,
    pub suppressed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    #[error("invalid recipient address")]
    InvalidAddress,
    #[error("empty subject")]
    EmptySubject,
    #[error(transparent)]
    Db(#[from] DbErr),
}

/// Whether new non-CRITICAL mail to this recipient digest is refused (complaint, hard
/// bounce, manual, or two bounces). The CRITICAL lane is never suppressed.
pub async fn suppressed<C: ConnectionTrait>(conn: &C, digest: &str) -> Result<bool, DbErr> {
    let row = conn
        .query_one(db::stmt(
            "SELECT 1 AS x FROM notif_email_suppressions
              WHERE recipient_digest = $1 AND (reason <> 'SOFT_BOUNCE' OR bounce_count >= 2)",
            vec![digest.into()],
        ))
        .await?;
    Ok(row.is_some())
}

/// Records a message (QUEUED, or SUPPRESSED without content). Idempotent on the key.
pub async fn enqueue<C: ConnectionTrait>(
    conn: &C,
    keys: &DataKeys,
    email: NewEmail,
) -> Result<Enqueued, EnqueueError> {
    let to = email.to.trim().to_string();
    if !valid_address(&to) {
        return Err(EnqueueError::InvalidAddress);
    }
    let subject = clean_header(&email.subject, 200);
    if subject.is_empty() {
        return Err(EnqueueError::EmptySubject);
    }
    let key = clean_header(&email.idempotency_key, 160);
    let id = message_id(&key);
    let digest = keys.email_digest(&to);
    let is_suppressed = email.lane != "CRITICAL" && suppressed(conn, &digest).await?;
    let owner = format!("email:{id}");
    let to_name = email
        .to_name
        .as_deref()
        .map(|n| clean_header(n, 120).replace(['<', '>', '"'], ""))
        .filter(|n| !n.is_empty());
    let recipient = json!({ "to": to, "name": to_name }).to_string();
    let body = json!({ "html": email.html, "text": email.text }).to_string();
    let (status, recipient_sealed, body_sealed, final_at): (
        &str,
        Option<String>,
        Option<String>,
        Option<DateTime<Utc>>,
    ) = if is_suppressed {
        ("SUPPRESSED", None, None, Some(Utc::now()))
    } else {
        (
            "QUEUED",
            Some(keys.seal(&recipient, &owner)),
            Some(keys.seal(&body, &owner)),
            None,
        )
    };
    let inserted = conn
        .query_one(db::stmt(
            "INSERT INTO notif_email_messages (id, delivery_id, category, lane, idempotency_key, route_decision_id,
                 sender_profile, recipient_digest, recipient_sealed, subject_sealed, body_sealed, status,
                 last_error_class, expires_at, final_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
             ON CONFLICT (idempotency_key) DO NOTHING
             RETURNING id",
            vec![
                id.into(),
                email.delivery_id.into(),
                email.category.clone().into(),
                email.lane.into(),
                key.clone().into(),
                email.route_decision_id.into(),
                email.sender_profile.clone().into(),
                digest.into(),
                recipient_sealed.into(),
                keys.seal(&subject, &owner).into(),
                body_sealed.into(),
                status.into(),
                is_suppressed.then_some("RECIPIENT_SUPPRESSED").into(),
                email.expires_at.into(),
                final_at.into(),
            ],
        ))
        .await?;
    Ok(Enqueued {
        id,
        created: inserted.is_some(),
        suppressed: is_suppressed,
    })
}

/// PII-free view of one message.
#[derive(Debug, Clone, FromQueryResult, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStatus {
    pub id: Uuid,
    pub category: String,
    pub lane: String,
    pub status: String,
    pub provider: Option<String>,
    pub attempts: i32,
    pub last_error_class: Option<String>,
    pub last_error_status: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

pub async fn status<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> Result<Option<MessageStatus>, DbErr> {
    db::one(
        conn,
        "SELECT id, category, lane, status, provider, attempts, last_error_class, last_error_status,
                created_at, accepted_at, delivered_at
           FROM notif_email_messages WHERE id = $1",
        vec![id.into()],
    )
    .await
}

// ---------------------------------------------------------------------------
// Providers (ported unchanged)
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
    Temporary {
        class: &'static str,
        status: Option<u16>,
        retry_after_secs: Option<i64>,
    },
    Permanent {
        class: &'static str,
        status: Option<u16>,
    },
}

impl ProviderError {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Temporary { class, .. } | Self::Permanent { class, .. } => class,
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Temporary { status, .. } | Self::Permanent { status, .. } => *status,
        }
    }
}

#[async_trait]
pub trait EmailProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, message: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError>;
}

/// RFC 2606 .invalid recipients are test fixtures, never public mail destinations.
/// Refuse before the provider hop rather than create hours of delivery-delay retries.
fn reject_reserved_recipient(recipient: &str) -> Result<(), ProviderError> {
    let domain = recipient
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('.').to_ascii_lowercase())
        .unwrap_or_default();
    if domain == "invalid" || domain.ends_with(".invalid") {
        return Err(ProviderError::Permanent {
            class: "RESERVED_INVALID_RECIPIENT",
            status: None,
        });
    }
    Ok(())
}

/// Resend HTTPS API (`POST /emails`), with the message id as `Idempotency-Key`.
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
            .user_agent("cfmeet-notifications/1")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build the Resend HTTP client".to_string())?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

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
        reject_reserved_recipient(m.to)?;
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
            return Ok(body
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 128)
                .map(str::to_string));
        }
        let name = body.get("name").and_then(Value::as_str).unwrap_or("");
        Err(Self::classify(status, retry_after, name))
    }
}

/// SMTP (no idempotency: a crash after acceptance can duplicate, recorded per §9.11).
pub struct SmtpProvider {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
}

impl SmtpProvider {
    pub fn new(config: &EmailConfig) -> Result<Self, String> {
        use lettre::{transport::smtp::authentication::Credentials, AsyncSmtpTransport};
        let credentials = Credentials::new(
            config.smtp_username.clone(),
            config.smtp_password.expose_secret().to_string(),
        );
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
            Err(e) if e.is_permanent() => Err(if e.to_string().contains("535") {
                ProviderError::Temporary {
                    class: "SMTP_AUTHENTICATION",
                    status: Some(535),
                    retry_after_secs: None,
                }
            } else {
                rejected("SMTP_REJECTED")
            }),
            Err(_) => Err(ProviderError::Temporary {
                class: "SMTP_TRANSIENT",
                status: None,
                retry_after_secs: None,
            }),
        }
    }
}

/// In-process provider for tests: records messages; can fail or stall on demand.
#[derive(Default)]
pub struct RecordingProvider {
    pub sent: std::sync::Mutex<Vec<(Uuid, String, String, String)>>,
    pub from: std::sync::Mutex<Vec<String>>,
    pub fail_next: std::sync::Mutex<Vec<ProviderError>>,
    pub delay: std::sync::Mutex<Option<StdDuration>>,
}

impl RecordingProvider {
    /// (message id, to, subject, text) of every accepted message.
    pub fn sent(&self) -> Vec<(Uuid, String, String, String)> {
        self.sent.lock().expect("recording provider").clone()
    }
    pub fn fail_with(&self, errors: Vec<ProviderError>) {
        *self.fail_next.lock().expect("recording provider") = errors;
    }
    pub fn slow(&self, d: StdDuration) {
        *self.delay.lock().expect("recording provider") = Some(d);
    }
}

#[async_trait]
impl EmailProvider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn send(&self, m: &ProviderMessage<'_>) -> Result<Option<String>, ProviderError> {
        let delay = *self.delay.lock().expect("recording provider");
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        {
            let mut f = self.fail_next.lock().expect("recording provider");
            if !f.is_empty() {
                return Err(f.remove(0));
            }
        }
        self.from
            .lock()
            .expect("recording provider")
            .push(m.from.to_string());
        self.sent.lock().expect("recording provider").push((
            m.id,
            m.to.to_string(),
            m.subject.to_string(),
            m.text.to_string(),
        ));
        Ok(Some(format!("rec_{}", m.id.simple())))
    }
}

/// The configured provider (`None`: messages stay QUEUED until they expire).
pub fn provider_from_config(
    config: &EmailConfig,
) -> Result<Option<Arc<dyn EmailProvider>>, String> {
    Ok(match config.provider {
        EmailProviderKind::Resend => Some(Arc::new(ResendProvider::new(
            &config.resend_api_url,
            config.resend_api_key.expose_secret(),
        )?)),
        EmailProviderKind::Smtp => Some(Arc::new(SmtpProvider::new(config)?)),
        EmailProviderKind::None => None,
    })
}

// ---------------------------------------------------------------------------
// Worker (one per lane per replica)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    pub claimed: u64,
    pub accepted: u64,
    pub retried: u64,
    pub failed: u64,
    pub expired: u64,
    pub lease_lost: u64,
    pub lease_refused: u64,
    pub breaker_skipped: bool,
}

#[derive(Debug, FromQueryResult)]
struct Claimed {
    id: Uuid,
    delivery_id: Option<Uuid>,
    category: String,
    route_decision_id: Option<Uuid>,
    sender_profile: String,
    recipient_sealed: Option<String>,
    subject_sealed: String,
    body_sealed: Option<String>,
    attempts: i32,
    expires_at: DateTime<Utc>,
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
    keys: Arc<DataKeys>,
    provider: Arc<dyn EmailProvider>,
    authorities: Arc<dyn Authorities>,
    config: Arc<EmailConfig>,
    metrics: Arc<Metrics>,
    lane: &'static str,
    holder: String,
    lease_secs: i64,
    batch: i64,
    failures: std::sync::Mutex<(u32, Option<DateTime<Utc>>)>,
}

impl EmailWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: DatabaseConnection,
        keys: Arc<DataKeys>,
        provider: Arc<dyn EmailProvider>,
        authorities: Arc<dyn Authorities>,
        config: Arc<EmailConfig>,
        metrics: Arc<Metrics>,
        lane: &'static str,
        replica: &str,
    ) -> Self {
        Self {
            db,
            keys,
            provider,
            authorities,
            config,
            metrics,
            lane,
            holder: format!(
                "{replica}:{}:{}",
                lane.to_ascii_lowercase(),
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            lease_secs: LEASE_SECS,
            batch: if lane == "CRITICAL" { 4 } else { 8 },
            failures: std::sync::Mutex::new((0, None)),
        }
    }

    pub fn with_lease_secs(mut self, secs: i64) -> Self {
        self.lease_secs = secs.max(1);
        self
    }

    pub fn lane(&self) -> &'static str {
        self.lane
    }

    async fn renew(&self, id: Uuid) -> Result<bool, DbErr> {
        let rows = self
            .db
            .query_all(db::stmt(
                "UPDATE notif_email_messages
                    SET lease_expires_at = now() + make_interval(secs => $2), updated_at = now()
                  WHERE lease_holder = $1 AND status = 'SENDING'
              RETURNING id",
                vec![self.holder.clone().into(), self.lease_secs.into()],
            ))
            .await?;
        Ok(rows
            .iter()
            .any(|r| r.try_get::<Uuid>("", "id").ok() == Some(id)))
    }

    async fn claim(&self) -> Result<Vec<Claimed>, DbErr> {
        db::all(
            &self.db,
            "UPDATE notif_email_messages m
                SET status = 'SENDING', lease_holder = $1,
                    lease_expires_at = now() + make_interval(secs => $2),
                    attempts = m.attempts + 1, updated_at = now()
              WHERE m.id IN (
                    SELECT id FROM notif_email_messages
                     WHERE lane = $4 AND ((status = 'QUEUED' AND next_attempt_at <= now())
                        OR (status = 'SENDING' AND lease_expires_at < now()))
                     ORDER BY next_attempt_at
                     LIMIT $3
                     FOR UPDATE SKIP LOCKED)
          RETURNING m.id, m.delivery_id, m.category, m.route_decision_id, m.sender_profile, m.recipient_sealed,
                    m.subject_sealed, m.body_sealed, m.attempts, m.expires_at",
            vec![self.holder.clone().into(), self.lease_secs.into(), self.batch.into(), self.lane.into()],
        )
        .await
    }

    /// Final state: content erased, lease released, the linked delivery updated.
    async fn finish(
        &self,
        id: Uuid,
        delivery: Option<Uuid>,
        status: &str,
        provider_id: Option<String>,
        error: Option<&ProviderError>,
    ) -> Result<(), DbErr> {
        let accepted = status == "ACCEPTED";
        let n = db::exec(
            &self.db,
            "UPDATE notif_email_messages
                SET status = $3, provider = $4, provider_message_id = COALESCE($5, provider_message_id),
                    body_sealed = NULL, lease_holder = NULL, lease_expires_at = NULL,
                    last_error_class = $6, last_error_status = $7,
                    accepted_at = CASE WHEN $8 THEN now() ELSE accepted_at END,
                    final_at = now(), updated_at = now()
              WHERE id = $1 AND lease_holder = $2",
            vec![
                id.into(),
                self.holder.clone().into(),
                status.into(),
                self.provider.name().into(),
                provider_id.into(),
                error.map(|e| e.class().to_string()).into(),
                error.and_then(|e| e.status()).map(|s| s as i32).into(),
                accepted.into(),
            ],
        )
        .await?;
        if n == 1 {
            if let Some(d) = delivery {
                let delivery_status = match status {
                    "ACCEPTED" => "SENT",
                    "EXPIRED" => "EXPIRED",
                    _ => "FAILED",
                };
                db::exec(
                    &self.db,
                    "UPDATE notif_deliveries SET status = $2, completed_at = now(), last_error_class = $3,
                            attempts = LEAST(attempts + 1, 64)
                      WHERE id = $1 AND status IN ('PENDING', 'DEFERRED')",
                    vec![d.into(), delivery_status.into(), error.map(|e| e.class().to_string()).into()],
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn reschedule(
        &self,
        id: Uuid,
        at: DateTime<Utc>,
        error: &ProviderError,
    ) -> Result<(), DbErr> {
        db::exec(
            &self.db,
            "UPDATE notif_email_messages
                SET status = 'QUEUED', next_attempt_at = $3, provider = $4,
                    lease_holder = NULL, lease_expires_at = NULL,
                    last_error_class = $5, last_error_status = $6, updated_at = now()
              WHERE id = $1 AND lease_holder = $2",
            vec![
                id.into(),
                self.holder.clone().into(),
                at.into(),
                self.provider.name().into(),
                error.class().into(),
                error.status().map(|s| s as i32).into(),
            ],
        )
        .await?;
        Ok(())
    }

    fn breaker_open(&self, now: DateTime<Utc>) -> bool {
        let f = self.failures.lock().expect("breaker");
        f.1.is_some_and(|until| until > now)
    }

    fn record_outcome(&self, ok: bool, now: DateTime<Utc>) {
        let mut f = self.failures.lock().expect("breaker");
        if ok {
            *f = (0, None);
            self.metrics.breaker_open.store(0, Ordering::Relaxed);
        } else {
            f.0 += 1;
            if f.0 >= BREAKER_THRESHOLD {
                f.1 = Some(now + Duration::seconds(BREAKER_OPEN_SECS));
                self.metrics.breaker_open.store(1, Ordering::Relaxed);
            }
        }
    }

    /// One pass: claim due messages of this lane, send them (rate limited), record.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<TickReport, DbErr> {
        let mut report = TickReport::default();
        if self.breaker_open(now) {
            report.breaker_skipped = true;
            return Ok(report);
        }
        let claimed = self.claim().await?;
        report.claimed = claimed.len() as u64;
        let spacing = StdDuration::from_millis(1000 / self.config.max_per_second.max(1) as u64);
        let lane_label = format!("lane=\"{}\"", self.lane);
        for (n, job) in claimed.into_iter().enumerate() {
            let owner = format!("email:{}", job.id);
            if job.expires_at <= now {
                self.finish(job.id, job.delivery_id, "EXPIRED", None, None)
                    .await?;
                self.metrics
                    .inc("notif_email_expired_total", lane_label.clone());
                report.expired += 1;
                continue;
            }
            let opened = (
                job.recipient_sealed
                    .as_deref()
                    .and_then(|s| self.keys.open(s, &owner).ok()),
                self.keys.open(&job.subject_sealed, &owner).ok(),
                job.body_sealed
                    .as_deref()
                    .and_then(|s| self.keys.open(s, &owner).ok()),
            );
            let (Some(recipient), Some(subject), Some(body)) = opened else {
                self.finish(
                    job.id,
                    job.delivery_id,
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
            let recipient: Value = serde_json::from_str(&recipient).unwrap_or(Value::Null);
            let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let (Some(to), Some(html), Some(text)) = (
                recipient["to"].as_str(),
                body["html"].as_str(),
                body["text"].as_str(),
            ) else {
                self.finish(
                    job.id,
                    job.delivery_id,
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
            if !self.renew(job.id).await? {
                self.metrics
                    .inc("notif_email_lease_lost_total", lane_label.clone());
                report.lease_lost += 1;
                continue;
            }
            // CRITICAL lane: send only under a granted Account lease (X6, Document 4 §9.8).
            let critical_decision = (self.lane == "CRITICAL")
                .then_some(job.route_decision_id)
                .flatten();
            if let Some(decision) = critical_decision {
                match self
                    .authorities
                    .claim_leases(&[decision], CRITICAL_LEASE_SECS)
                    .await
                {
                    Ok(results) => match results
                        .iter()
                        .find(|r| r.decision_id == decision)
                        .map(|r| r.outcome.as_str())
                    {
                        Some("GRANTED") => {}
                        Some("EXPIRED") => {
                            self.finish(
                                job.id,
                                job.delivery_id,
                                "EXPIRED",
                                None,
                                Some(&ProviderError::Permanent {
                                    class: "CODE_EXPIRED",
                                    status: None,
                                }),
                            )
                            .await?;
                            report.expired += 1;
                            continue;
                        }
                        Some("REVOKED") => {
                            // Rerouted to the retained old path: never send here.
                            self.finish(
                                job.id,
                                job.delivery_id,
                                "FAILED",
                                None,
                                Some(&ProviderError::Permanent {
                                    class: "ROUTE_REVOKED",
                                    status: None,
                                }),
                            )
                            .await?;
                            self.metrics
                                .inc("notif_critical_lease_revoked_total", lane_label.clone());
                            report.lease_refused += 1;
                            continue;
                        }
                        _ => {
                            self.reschedule(
                                job.id,
                                now + Duration::seconds(5),
                                &ProviderError::Temporary {
                                    class: "LEASE_UNAVAILABLE",
                                    status: None,
                                    retry_after_secs: None,
                                },
                            )
                            .await?;
                            report.lease_refused += 1;
                            continue;
                        }
                    },
                    Err(_) => {
                        // Account unreachable: send nothing (the decision stays never-leased,
                        // so Account's AUTO failover may reroute it without duplicates).
                        self.reschedule(
                            job.id,
                            now + Duration::seconds(5),
                            &ProviderError::Temporary {
                                class: "LEASE_UNAVAILABLE",
                                status: None,
                                retry_after_secs: None,
                            },
                        )
                        .await?;
                        report.lease_refused += 1;
                        continue;
                    }
                }
            } else if self.lane == "CRITICAL"
                && job.route_decision_id.is_none()
                && job.category != "OPERATIONS_ACCEPTANCE"
            {
                // A CRITICAL message without a decision would be a send without a lease.
                self.metrics
                    .critical_send_without_lease
                    .fetch_add(1, Ordering::Relaxed);
            }
            let from = self.config.from_for(&job.sender_profile).to_string();
            let message = ProviderMessage {
                id: job.id,
                category: &job.category,
                from: &from,
                reply_to: self.config.reply_to.as_deref(),
                to,
                to_name: recipient["name"].as_str(),
                subject: &subject,
                html,
                text,
            };
            let sent = if critical_decision.is_some() {
                match tokio::time::timeout(CRITICAL_SEND_TIMEOUT, self.provider.send(&message))
                    .await
                {
                    Ok(r) => r,
                    Err(_) => Err(ProviderError::Temporary {
                        class: "TIMEOUT",
                        status: None,
                        retry_after_secs: None,
                    }),
                }
            } else {
                self.provider.send(&message).await
            };
            match sent {
                Ok(provider_id) => {
                    self.record_outcome(true, now);
                    self.finish(job.id, job.delivery_id, "ACCEPTED", provider_id, None)
                        .await?;
                    if let Some(decision) = critical_decision {
                        // X7: terminal, never reroutable. A failure here is retried by the
                        // decision audit; the message itself is already accepted.
                        if self.authorities.complete(&[decision]).await.is_err() {
                            self.metrics.inc(
                                "notif_critical_completion_failures_total",
                                lane_label.clone(),
                            );
                        }
                    }
                    self.metrics.inc(
                        "notif_email_accepted_total",
                        format!("{lane_label},category=\"{}\"", label(&job.category)),
                    );
                    tracing::info!(message_id = %job.id, category = %job.category, lane = self.lane,
                        provider = self.provider.name(), "email accepted by provider");
                    report.accepted += 1;
                }
                Err(e @ ProviderError::Permanent { .. }) => {
                    self.record_outcome(true, now);
                    self.finish(job.id, job.delivery_id, "FAILED", None, Some(&e))
                        .await?;
                    self.metrics.inc(
                        "notif_email_failed_total",
                        format!("{lane_label},class=\"{}\"", label(e.class())),
                    );
                    tracing::warn!(message_id = %job.id, category = %job.category, error_class = e.class(), "email rejected permanently");
                    report.failed += 1;
                }
                Err(e @ ProviderError::Temporary { .. }) => {
                    self.record_outcome(false, now);
                    let retry_after = match &e {
                        ProviderError::Temporary {
                            retry_after_secs: Some(s),
                            ..
                        } => Some(Duration::seconds((*s).clamp(1, BACKOFF_CAP_SECS))),
                        _ => None,
                    };
                    let next = now + retry_after.unwrap_or_else(|| backoff(job.attempts));
                    let expired = job.expires_at <= next;
                    if job.attempts >= self.config.max_attempts || expired {
                        let st = if expired { "EXPIRED" } else { "FAILED" };
                        self.finish(job.id, job.delivery_id, st, None, Some(&e))
                            .await?;
                        self.metrics.inc(
                            "notif_email_failed_total",
                            format!("{lane_label},class=\"{}\"", label(e.class())),
                        );
                        report.failed += 1;
                    } else {
                        self.reschedule(job.id, next, &e).await?;
                        self.metrics.inc(
                            "notif_email_retried_total",
                            format!("{lane_label},class=\"{}\"", label(e.class())),
                        );
                        report.retried += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Erase addresses of old final rows; delete rows past twice the retention; drop old
    /// provider events. Non-final rows past their expiry are expired (no content lingers).
    pub async fn purge(&self) -> Result<(), DbErr> {
        let days = self.config.retention_days;
        db::exec(
            &self.db,
            "UPDATE notif_email_messages SET status = 'EXPIRED', body_sealed = NULL, final_at = now(), updated_at = now(),
                    lease_holder = NULL, lease_expires_at = NULL
              WHERE status = 'QUEUED' AND expires_at < now()",
            vec![],
        )
        .await?;
        db::exec(
            &self.db,
            "UPDATE notif_email_messages SET recipient_sealed = NULL
              WHERE final_at < now() - make_interval(days => $1::int) AND recipient_sealed IS NOT NULL",
            vec![days.into()],
        )
        .await?;
        db::exec(&self.db, "DELETE FROM notif_email_messages WHERE final_at < now() - make_interval(days => $1::int)", vec![(days * 2).into()]).await?;
        db::exec(&self.db, "DELETE FROM notif_email_provider_events WHERE received_at < now() - make_interval(days => $1::int)", vec![(days * 2).into()]).await?;
        Ok(())
    }

    pub async fn run(
        &self,
        poll: StdDuration,
        heartbeats: Arc<Heartbeats>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tracing::info!(
            provider = self.provider.name(),
            lane = self.lane,
            "email worker started"
        );
        let beat: &'static str = if self.lane == "CRITICAL" {
            "email-critical"
        } else {
            "email-standard"
        };
        let mut last_purge = Utc::now() - Duration::hours(2);
        loop {
            if *shutdown.borrow() {
                break;
            }
            heartbeats.beat(beat);
            let busy = match self.tick(Utc::now()).await {
                Ok(r) => r.claimed > 0,
                Err(e) => {
                    tracing::error!(summary = %db::summarize_db_error(&e), lane = self.lane, "email worker tick failed");
                    false
                }
            };
            if self.lane == "STANDARD" && Utc::now() - last_purge > Duration::hours(1) {
                if let Err(e) = self.purge().await {
                    tracing::warn!(summary = %db::summarize_db_error(&e), "email ledger purge failed");
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
        tracing::info!(lane = self.lane, "email worker stopped");
    }
}

// ---------------------------------------------------------------------------
// Provider webhooks (Resend, Svix signatures)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookError {
    Unauthorized,
    Malformed,
}

/// Verify a Svix-signed webhook (ported unchanged).
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

fn rank(status: &str) -> i32 {
    match status {
        "QUEUED" | "SENDING" => 0,
        "ACCEPTED" => 1,
        "DELAYED" => 2,
        "DELIVERED" => 3,
        "BOUNCED" | "FAILED" => 4,
        "COMPLAINED" => 5,
        _ => 0,
    }
}

/// Apply one verified provider event. `Ok(false)`: a duplicate event id. Events for
/// message ids this ledger never sent are recorded and otherwise ignored (during the drain
/// Resend posts every event to both endpoints; each applies only its own messages).
pub async fn apply_provider_event(
    db_conn: &DatabaseConnection,
    metrics: &Metrics,
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
        _ => None,
    };
    let internal = |_| WebhookError::Malformed;
    let row = db_conn
        .query_one(db::stmt(
            "INSERT INTO notif_email_provider_events (provider, event_id, event_type, provider_message_id, message_id, occurred_at)
             VALUES ($1, $2, $3, $4,
                 (SELECT id FROM notif_email_messages WHERE provider = $1 AND provider_message_id = $4), $5)
             ON CONFLICT (provider, event_id) DO NOTHING
             RETURNING message_id",
            vec![provider.into(), event_id.into(), event_type.into(), message_id.map(str::to_string).into(), occurred_at.into()],
        ))
        .await
        .map_err(internal)?;
    let Some(row) = row else {
        metrics.inc("notif_email_webhook_duplicates_total", "");
        return Ok(false);
    };
    let message: Option<Uuid> = row.try_get("", "message_id").map_err(internal)?;
    metrics.inc(
        "notif_email_webhook_events_total",
        format!("type=\"{}\"", label(event_type)),
    );
    if let (Some(id), Some(target)) = (message, target) {
        let lower: Vec<DbValue> = FINAL_STATUSES
            .iter()
            .chain(["QUEUED", "SENDING"].iter())
            .filter(|s| rank(s) < rank(target))
            .map(|s| (*s).into())
            .collect();
        let placeholders: Vec<String> = (0..lower.len()).map(|i| format!("${}", i + 3)).collect();
        let mut values: Vec<DbValue> = vec![id.into(), target.into()];
        values.extend(lower);
        let updated = db_conn
            .query_one(db::stmt(
                &format!(
                    "UPDATE notif_email_messages
                        SET status = $2, updated_at = now(),
                            delivered_at = CASE WHEN $2 = 'DELIVERED' THEN now() ELSE delivered_at END,
                            final_at = COALESCE(final_at, now()), body_sealed = NULL,
                            lease_holder = NULL, lease_expires_at = NULL
                      WHERE id = $1 AND status IN ({})
                  RETURNING recipient_digest, lane, delivery_id",
                    placeholders.join(", ")
                ),
                values,
            ))
            .await
            .map_err(internal)?;
        if let Some(u) = updated {
            let digest: String = u.try_get("", "recipient_digest").map_err(internal)?;
            let delivery: Option<Uuid> = u.try_get("", "delivery_id").map_err(internal)?;
            if let Some(d) = delivery {
                let ds = match target {
                    "DELIVERED" => "DELIVERED",
                    "BOUNCED" | "COMPLAINED" | "FAILED" => "FAILED",
                    _ => "SENT",
                };
                db::exec(db_conn, "UPDATE notif_deliveries SET status = $2 WHERE id = $1 AND status IN ('SENT', 'DELIVERED', 'PENDING')", vec![d.into(), ds.into()])
                    .await
                    .map_err(internal)?;
            }
            match target {
                "COMPLAINED" => {
                    db::exec(
                        db_conn,
                        "INSERT INTO notif_email_suppressions (recipient_digest, reason) VALUES ($1, 'COMPLAINED')
                         ON CONFLICT (recipient_digest) DO UPDATE SET reason = 'COMPLAINED', updated_at = now()",
                        vec![digest.into()],
                    )
                    .await
                    .map_err(internal)?;
                }
                "BOUNCED" => {
                    let reason = if payload.pointer("/data/bounce/type").and_then(Value::as_str)
                        == Some("Permanent")
                    {
                        "HARD_BOUNCE"
                    } else {
                        "SOFT_BOUNCE"
                    };
                    db::exec(
                        db_conn,
                        "INSERT INTO notif_email_suppressions (recipient_digest, reason, bounce_count) VALUES ($1, $2, 1)
                         ON CONFLICT (recipient_digest) DO UPDATE SET bounce_count = notif_email_suppressions.bounce_count + 1,
                             reason = CASE WHEN notif_email_suppressions.reason IN ('COMPLAINED','MANUAL','HARD_BOUNCE')
                                           THEN notif_email_suppressions.reason ELSE EXCLUDED.reason END,
                             updated_at = now()",
                        vec![digest.into(), reason.into()],
                    )
                    .await
                    .map_err(internal)?;
                }
                _ => {}
            }
            metrics.inc(
                "notif_email_provider_status_total",
                format!("status=\"{target}\""),
            );
            tracing::info!(message_id = %id, event_type, "email provider event applied");
        }
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
    fn resend_errors_are_classified_as_before() {
        assert!(matches!(
            ResendProvider::classify(429, Some(3), ""),
            ProviderError::Temporary {
                retry_after_secs: Some(3),
                ..
            }
        ));
        assert!(matches!(
            ResendProvider::classify(422, None, ""),
            ProviderError::Permanent { .. }
        ));
        assert!(matches!(
            ResendProvider::classify(403, None, ""),
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
    fn message_ids_are_deterministic_per_key() {
        assert_eq!(message_id("otp:verify:a:b"), message_id("otp:verify:a:b"));
        assert_ne!(message_id("otp:verify:a:b"), message_id("otp:verify:a:c"));
        assert_eq!(message_id("x").get_version_num(), 5);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert!(backoff(1).num_seconds() <= 6);
        assert!(backoff(4).num_seconds() >= 32);
        assert!(backoff(30).num_seconds() <= BACKOFF_CAP_SECS + BACKOFF_CAP_SECS / 5);
    }

    #[test]
    fn resend_body_carries_the_configured_sender() {
        let b = ResendProvider::body(&ProviderMessage {
            id: Uuid::new_v4(),
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
        assert_eq!(b["tags"][0]["value"], "auth_verification");
    }
}

#[cfg(test)]
mod reserved_recipient_tests {
    use super::*;
    #[test]
    fn reserved_invalid_addresses_never_reach_the_live_email_provider() {
        for recipient in [
            "fixture@example.invalid",
            "fixture@INVALID",
            "fixture@sub.example.INVALID.",
        ] {
            assert!(matches!(
                reject_reserved_recipient(recipient),
                Err(ProviderError::Permanent {
                    class: "RESERVED_INVALID_RECIPIENT",
                    ..
                })
            ));
        }
        for recipient in [
            "delivered@resend.dev",
            "user@example.invalid.com",
            "user@gmail.com",
        ] {
            assert!(reject_reserved_recipient(recipient).is_ok());
        }
    }
}
