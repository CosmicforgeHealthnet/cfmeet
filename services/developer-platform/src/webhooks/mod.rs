//! Section 53 — the central customer webhook platform.
//!
//! ```text
//! producer outbox ─▶ POST /internal/v1/platform/events (developer-event.publish; the
//!   envelope's producer IS the authenticated service) ─▶ project (catalog: allow-listed
//!   safe keys only) ─▶ devplat_webhook_events (dedupe on producer + source event id)
//!   ─▶ one delivery per ACTIVE matching subscription of the SAME environment
//!   ─▶ worker (SKIP LOCKED + lease) ─▶ SSRF-safe signed POST ─▶ SUCCEEDED
//!   | RETRY_SCHEDULED (jittered backoff) | FAILED (dead letter) ; replay = NEW delivery
//!   shadow mode: deliveries recorded as SHADOW, nothing is sent
//! ```
//!
//! Signing (the `CF-Webhook-*` v1 scheme), sealing of signing secrets at rest, the SSRF-safe
//! endpoint policy and retry classification are the shared `cf_webhooks` primitives: the
//! Developer Platform implements no webhook cryptography of its own. The customer API
//! (`/v1/webhooks…`, `/v1/webhook-deliveries…`) is exactly Calling's contract, so the API
//! Gateway can re-point those routes here at the cut-over; the Developer Console manages
//! the same subscriptions per environment (`/v1/environments/{id}/webhooks…`).
//!
//! The scope of every record is the Platform Access chain CONFIRMED for the caller (or the
//! console environment's own profile), never a client-supplied tenant id.

pub mod catalog;
pub mod http;
pub mod ingest;
pub mod worker;

use std::{fmt, sync::Arc, time::Duration};

use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    control::{activity, pagination::Page, stmt, Caller},
    http::error::{ApiError, ErrorCode},
};

/// Whether deliveries are sent (`Active`) or only recorded (`Shadow`, the default before
/// the cut-over from Calling: enabling the Developer Platform never double-delivers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    Active,
    Shadow,
}

impl DeliveryMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryMode::Active => "ACTIVE",
            DeliveryMode::Shadow => "SHADOW",
        }
    }
}

/// Deployment configuration of central webhooks (see [`crate::config::webhooks`]).
#[derive(Clone)]
pub struct WebhookSettings {
    pub enabled: bool,
    /// Seals signing secrets at rest; without it subscriptions are unavailable (fail
    /// closed) and nothing can be signed.
    pub key_ring: Option<Arc<SecretBox>>,
    pub policy: EndpointPolicy,
    pub mode: DeliveryMode,
    pub request_timeout: Duration,
    pub max_attempts: u32,
    /// Consecutive failed attempts after which an ACTIVE endpoint is disabled.
    pub auto_disable_after: i32,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            key_ring: None,
            policy: EndpointPolicy::STRICT,
            mode: DeliveryMode::Shadow,
            request_timeout: Duration::from_secs(10),
            max_attempts: cf_webhooks::retry::MAX_ATTEMPTS,
            auto_disable_after: 50,
        }
    }
}

impl fmt::Debug for WebhookSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookSettings")
            .field("enabled", &self.enabled)
            .field(
                "key_ring",
                &self.key_ring.as_ref().map(|r| r.current_kid().to_string()),
            )
            .field("policy", &self.policy)
            .field("mode", &self.mode)
            .field("request_timeout", &self.request_timeout)
            .field("max_attempts", &self.max_attempts)
            .field("auto_disable_after", &self.auto_disable_after)
            .finish()
    }
}

/// The time source (tests advance it).
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// The application environment a webhook record belongs to (Platform Access ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `SANDBOX` / `TEST` / `PRODUCTION`.
    pub environment_kind: String,
}

/// The central webhook service (Developer Platform-owned state; `cf_webhooks` primitives).
#[derive(Clone)]
pub struct WebhookService {
    db: DatabaseConnection,
    settings: Arc<WebhookSettings>,
    client: reqwest::Client,
    clock: Clock,
}

pub(crate) fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn ts_opt(at: Option<DateTime<Utc>>) -> Value {
    at.map(|t| Value::String(ts(t))).unwrap_or(Value::Null)
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct SubscriptionRow {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub url: String,
    pub description: Option<String>,
    pub event_types: Vec<String>,
    pub status: String,
    pub secret_version: i32,
    pub version: i64,
    pub consecutive_failures: i32,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub disabled_reason: Option<String>,
    pub origin: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Never the sealed secret: views and checks do not load it.
const SUBSCRIPTION_COLUMNS: &str = "id, environment_id, environment_kind, url, description, \
    event_types, status, secret_version, version, consecutive_failures, last_success_at, \
    last_failure_at, disabled_reason, origin, created_at, updated_at";

impl SubscriptionRow {
    /// The customer contract view (Calling's `Webhook`; never the secret).
    pub fn view(&self) -> Value {
        json!({
            "webhookId": self.id,
            "url": self.url,
            "eventTypes": self.event_types,
            "description": self.description,
            "status": self.status,
            "secretVersion": self.secret_version,
            "version": self.version,
            "createdAt": ts(self.created_at),
            "updatedAt": ts(self.updated_at),
        })
    }

    /// The Developer Console view: the customer view plus scope, origin and health.
    pub fn console_view(&self) -> Value {
        let mut v = self.view();
        v["environmentId"] = json!(self.environment_id);
        v["environmentKind"] = json!(self.environment_kind);
        v["origin"] = json!(self.origin);
        v["health"] = json!({
            "consecutiveFailures": self.consecutive_failures,
            "lastSuccessAt": ts_opt(self.last_success_at),
            "lastFailureAt": ts_opt(self.last_failure_at),
            "disabledReason": self.disabled_reason,
        });
        v
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct DeliveryRow {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event_id: String,
    pub event_type: String,
    pub replay_of: Option<Uuid>,
    pub status: String,
    pub attempt_count: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_http_status: Option<i32>,
    pub failure_category: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

const DELIVERY_COLUMNS: &str = "d.id, d.subscription_id, d.event_id, e.event_type, d.replay_of, \
    d.status, d.attempt_count, d.next_attempt_at, d.last_http_status, d.failure_category, \
    d.created_at, d.completed_at";

impl DeliveryRow {
    /// Calling's `WebhookDelivery` shape (status class and bounded category only).
    pub fn view(&self) -> Value {
        let open = self.completed_at.is_none();
        json!({
            "deliveryId": self.id,
            "webhookId": self.subscription_id,
            "eventId": self.event_id,
            "eventType": self.event_type,
            "replayOf": self.replay_of,
            "status": self.status,
            "attemptCount": self.attempt_count,
            "nextAttemptAt": if open { Value::String(ts(self.next_attempt_at)) } else { Value::Null },
            "lastHttpStatus": self.last_http_status,
            "failureCategory": self.failure_category,
            "createdAt": ts(self.created_at),
            "completedAt": ts_opt(self.completed_at),
        })
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct AttemptRow {
    pub attempt_number: i32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub http_status: Option<i32>,
    pub outcome: String,
    pub error_class: Option<String>,
}

impl AttemptRow {
    pub fn view(&self) -> Value {
        json!({
            "attemptNumber": self.attempt_number,
            "startedAt": ts(self.started_at),
            "finishedAt": ts(self.finished_at),
            "httpStatus": self.http_status,
            "outcome": self.outcome,
            "errorClass": self.error_class,
        })
    }
}

/// A requested change of a subscription (`description: Some(None)` clears it).
#[derive(Debug, Default, Clone)]
pub struct WebhookChange {
    pub url: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub description: Option<Option<String>>,
    pub status: Option<String>,
}

/// A keyset page: rows and the cursor of the next page.
pub struct Listed<T> {
    pub rows: Vec<T>,
    pub next_cursor: Option<String>,
}

const DELIVERY_STATUSES: &[&str] = &[
    "PENDING",
    "DELIVERING",
    "RETRY_SCHEDULED",
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "SHADOW",
];

fn keyset<T>(
    mut rows: Vec<T>,
    limit: usize,
    key: impl Fn(&T) -> (DateTime<Utc>, Uuid),
) -> Listed<T> {
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = more
        .then(|| {
            rows.last().map(|r| {
                let (at, id) = key(r);
                Page::key_cursor(at, id)
            })
        })
        .flatten();
    Listed { rows, next_cursor }
}

/// Activity summary of a subscription change: ids, versions and counts only — never the
/// endpoint URL (it may embed a customer token) and never the signing secret.
fn summary(scope: &Scope, row: &SubscriptionRow) -> Value {
    json!({
        "environmentId": scope.environment_id,
        "status": row.status,
        "version": row.version,
        "eventTypeCount": row.event_types.len(),
    })
}

impl WebhookService {
    pub fn new(db: DatabaseConnection, settings: WebhookSettings) -> Self {
        let client = cf_webhooks::endpoint::client(settings.policy, settings.request_timeout);
        Self {
            db,
            settings: Arc::new(settings),
            client,
            clock: Arc::new(Utc::now),
        }
    }

    /// Replaces the delivery client (tests inject a rebinding resolver).
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Replaces the time source (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn settings(&self) -> &WebhookSettings {
        &self.settings
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// The sealing key ring, or 503 CAPABILITY_UNAVAILABLE (fail closed).
    pub fn key_ring(&self) -> Result<&SecretBox, ApiError> {
        self.settings
            .key_ring
            .as_deref()
            .ok_or(ApiError(ErrorCode::CapabilityUnavailable))
    }

    pub(crate) fn validate_url(&self, url: &str) -> Result<String, ApiError> {
        cf_webhooks::endpoint::validate_url(url.trim(), self.settings.policy)
            .map(|u| u.to_string())
            .map_err(|_| ApiError(ErrorCode::ValidationError))
    }

    /// Event types must exist in the public catalog (1–64, deduplicated, sorted).
    pub fn validate_event_types(types: &[String]) -> Result<Vec<String>, ApiError> {
        if types.is_empty() || types.len() > 64 {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let mut out: Vec<String> = Vec::new();
        for t in types {
            if catalog::definition(t).is_none() {
                return Err(ApiError(ErrorCode::ValidationError));
            }
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
        out.sort();
        Ok(out)
    }

    /// The domain read scopes a machine credential needs to subscribe to `types`.
    pub fn domain_scopes(types: &[String]) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = types
            .iter()
            .filter_map(|t| catalog::definition(t).and_then(|d| d.scope))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    fn description(d: Option<String>) -> Result<Option<String>, ApiError> {
        match d.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
            Some(d) if d.chars().count() > 200 || d.chars().any(char::is_control) => {
                Err(ApiError(ErrorCode::ValidationError))
            }
            other => Ok(other),
        }
    }

    /// Creates a subscription with a server-generated signing secret, returned ONLY here.
    pub async fn create(
        &self,
        caller: &Caller,
        scope: &Scope,
        url: &str,
        event_types: &[String],
        description: Option<String>,
    ) -> Result<(SubscriptionRow, String), ApiError> {
        let key_ring = self.key_ring()?;
        let url = self.validate_url(url)?;
        let types = Self::validate_event_types(event_types)?;
        let description = Self::description(description)?;
        let id = Uuid::new_v4();
        let secret = cf_webhooks::signing::generate_secret();
        let sealed = key_ring.seal(&secret, &id.to_string());
        let now = self.now();
        let txn = self.db.begin().await?;
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "INSERT INTO devplat_webhook_subscriptions (id, tenant_id, application_id, \
                   environment_id, environment_kind, url, description, event_types, status, \
                   secret_sealed, secret_key_id, created_by, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'ACTIVE', $9, $10, $11, $12, $12) \
                 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![
                id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                scope.environment_kind.clone().into(),
                url.into(),
                description.into(),
                types.into(),
                sealed.into(),
                key_ring.current_kid().into(),
                caller.actor_ref().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
        self.audit(&txn, caller, scope, &row, "webhook.created")
            .await?;
        txn.commit().await?;
        tracing::info!(webhook_id = %id, environment_id = %scope.environment_id, "webhook subscription created");
        Ok((row, secret))
    }

    async fn audit<C: ConnectionTrait>(
        &self,
        db: &C,
        caller: &Caller,
        scope: &Scope,
        row: &SubscriptionRow,
        action: &str,
    ) -> Result<(), ApiError> {
        activity::record(
            db,
            caller,
            activity::Event {
                tenant_id: scope.tenant_id,
                action,
                target_type: "WEBHOOK",
                target_id: row.id.to_string(),
                summary: summary(scope, row),
            },
        )
        .await
    }

    async fn row<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: &Scope,
        id: Uuid,
        lock: bool,
    ) -> Result<SubscriptionRow, ApiError> {
        SubscriptionRow::find_by_statement(stmt(
            &format!(
                "SELECT {SUBSCRIPTION_COLUMNS} FROM devplat_webhook_subscriptions WHERE id = $1 \
                   AND tenant_id = $2 AND application_id = $3 AND environment_id = $4 \
                   AND status <> 'DELETED'{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![
                id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .one(db)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))
    }

    pub async fn get(&self, scope: &Scope, id: Uuid) -> Result<SubscriptionRow, ApiError> {
        self.row(&self.db, scope, id, false).await
    }

    pub async fn list(
        &self,
        scope: &Scope,
        page: &Page,
    ) -> Result<Listed<SubscriptionRow>, ApiError> {
        let (after_at, after_id) = cursor_of(page)?;
        let rows = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "SELECT {SUBSCRIPTION_COLUMNS} FROM devplat_webhook_subscriptions \
                  WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                    AND status <> 'DELETED' \
                    AND ($4::timestamptz IS NULL OR (created_at, id) > ($4, $5)) \
                  ORDER BY created_at, id LIMIT $6"
            ),
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                after_at.into(),
                after_id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .all(&self.db)
        .await?;
        Ok(keyset(rows, page.limit, |r| (r.created_at, r.id)))
    }

    /// Changes url / event types / description / status (optimistic concurrency).
    /// Re-enabling resets the endpoint health; any explicit status change clears an
    /// automatic disablement reason.
    pub async fn update(
        &self,
        caller: &Caller,
        scope: &Scope,
        id: Uuid,
        expected_version: i64,
        change: WebhookChange,
    ) -> Result<SubscriptionRow, ApiError> {
        let url = change
            .url
            .as_deref()
            .map(|u| self.validate_url(u))
            .transpose()?;
        let types = change
            .event_types
            .as_deref()
            .map(Self::validate_event_types)
            .transpose()?;
        let description = change.description.map(Self::description).transpose()?;
        if let Some(s) = change.status.as_deref() {
            if !matches!(s, "ACTIVE" | "DISABLED") {
                return Err(ApiError(ErrorCode::ValidationError));
            }
        }
        let txn = self.db.begin().await?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(ApiError(ErrorCode::VersionConflict));
        }
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE devplat_webhook_subscriptions SET url = COALESCE($2, url), \
                   event_types = COALESCE($3, event_types), \
                   description = CASE WHEN $4 THEN $5 ELSE description END, \
                   consecutive_failures = CASE WHEN $6::text = 'ACTIVE' AND status <> 'ACTIVE' \
                                               THEN 0 ELSE consecutive_failures END, \
                   disabled_reason = CASE WHEN $6::text IS NOT NULL THEN NULL ELSE disabled_reason END, \
                   status = COALESCE($6::text, status), version = version + 1, updated_at = $7 \
                 WHERE id = $1 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![
                id.into(),
                url.into(),
                types.into(),
                description.is_some().into(),
                description.flatten().into(),
                change.status.into(),
                self.now().into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
        self.audit(&txn, caller, scope, &row, "webhook.updated")
            .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Soft-deletes the subscription; its open deliveries are CANCELLED (history kept).
    pub async fn delete(
        &self,
        caller: &Caller,
        scope: &Scope,
        id: Uuid,
        expected_version: i64,
    ) -> Result<SubscriptionRow, ApiError> {
        let txn = self.db.begin().await?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(ApiError(ErrorCode::VersionConflict));
        }
        let now = self.now();
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE devplat_webhook_subscriptions SET status = 'DELETED', deleted_at = $2, \
                   version = version + 1, updated_at = $2 WHERE id = $1 \
                 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![id.into(), now.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
        txn.execute(stmt(
            "UPDATE devplat_webhook_deliveries SET status = 'CANCELLED', completed_at = $2, \
               lease_until = NULL, updated_at = $2 \
             WHERE subscription_id = $1 AND status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING')",
            vec![id.into(), now.into()],
        ))
        .await?;
        self.audit(&txn, caller, scope, &row, "webhook.deleted")
            .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// A new signing secret (returned once); deliveries from now on use it.
    pub async fn rotate_secret(
        &self,
        caller: &Caller,
        scope: &Scope,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(SubscriptionRow, String), ApiError> {
        let key_ring = self.key_ring()?;
        let txn = self.db.begin().await?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(ApiError(ErrorCode::VersionConflict));
        }
        let secret = cf_webhooks::signing::generate_secret();
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE devplat_webhook_subscriptions SET secret_sealed = $2, secret_key_id = $3, \
                   secret_version = secret_version + 1, version = version + 1, updated_at = $4 \
                 WHERE id = $1 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![
                id.into(),
                key_ring.seal(&secret, &id.to_string()).into(),
                key_ring.current_kid().into(),
                self.now().into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
        self.audit(&txn, caller, scope, &row, "webhook.secret_rotated")
            .await?;
        txn.commit().await?;
        Ok((row, secret))
    }

    /// The delivery history of a subscription (dead letters included).
    pub async fn deliveries(
        &self,
        scope: &Scope,
        subscription: Uuid,
        status: Option<&str>,
        page: &Page,
    ) -> Result<Listed<DeliveryRow>, ApiError> {
        self.row(&self.db, scope, subscription, false).await?;
        if status.is_some_and(|s| !DELIVERY_STATUSES.contains(&s)) {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let (after_at, after_id) = cursor_of(page)?;
        let rows = DeliveryRow::find_by_statement(stmt(
            &format!(
                "SELECT {DELIVERY_COLUMNS} FROM devplat_webhook_deliveries d \
                   JOIN devplat_webhook_events e ON e.event_id = d.event_id \
                  WHERE d.subscription_id = $1 AND d.tenant_id = $2 AND d.application_id = $3 \
                    AND d.environment_id = $4 AND ($5::text IS NULL OR d.status = $5) \
                    AND ($6::timestamptz IS NULL OR (d.created_at, d.id) > ($6, $7)) \
                  ORDER BY d.created_at, d.id LIMIT $8"
            ),
            vec![
                subscription.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                status.map(str::to_string).into(),
                after_at.into(),
                after_id.into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .all(&self.db)
        .await?;
        Ok(keyset(rows, page.limit, |r| (r.created_at, r.id)))
    }

    async fn delivery_in_scope<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: &Scope,
        id: Uuid,
    ) -> Result<DeliveryRow, ApiError> {
        DeliveryRow::find_by_statement(stmt(
            &format!(
                "SELECT {DELIVERY_COLUMNS} FROM devplat_webhook_deliveries d \
                   JOIN devplat_webhook_events e ON e.event_id = d.event_id \
                  WHERE d.id = $1 AND d.tenant_id = $2 AND d.application_id = $3 \
                    AND d.environment_id = $4"
            ),
            vec![
                id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .one(db)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))
    }

    /// The attempt history of one delivery (status class and error class only).
    pub async fn attempts(
        &self,
        scope: &Scope,
        delivery: Uuid,
        page: &Page,
    ) -> Result<(Vec<AttemptRow>, Option<String>), ApiError> {
        self.delivery_in_scope(&self.db, scope, delivery).await?;
        if page.has_cursor() && page.cursor_seq().is_none() {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        let after = page.cursor_seq().unwrap_or(0);
        let mut rows = AttemptRow::find_by_statement(stmt(
            "SELECT attempt_number, started_at, finished_at, http_status, outcome, error_class \
               FROM devplat_webhook_attempts WHERE delivery_id = $1 AND attempt_number > $2 \
              ORDER BY attempt_number LIMIT $3",
            vec![
                delivery.into(),
                (after as i32).into(),
                (page.limit as i64 + 1).into(),
            ],
        ))
        .all(&self.db)
        .await?;
        let more = rows.len() > page.limit;
        rows.truncate(page.limit);
        let next = more
            .then(|| {
                rows.last()
                    .map(|r| Page::seq_cursor(r.attempt_number as i64))
            })
            .flatten();
        Ok((rows, next))
    }

    /// Manual replay: a NEW delivery of the same event (same event id) to the same ACTIVE
    /// subscription, referencing the original — dead-lettered deliveries included. The
    /// domain action is never re-executed. In shadow mode the replay is recorded SHADOW.
    pub async fn replay(
        &self,
        caller: &Caller,
        scope: &Scope,
        delivery: Uuid,
    ) -> Result<DeliveryRow, ApiError> {
        let now = self.now();
        let id = Uuid::new_v4();
        let shadow = self.settings.mode == DeliveryMode::Shadow;
        let txn = self.db.begin().await?;
        let inserted = txn
            .execute(stmt(
                "INSERT INTO devplat_webhook_deliveries (id, subscription_id, event_id, tenant_id, \
                   application_id, environment_id, replay_of, status, attempt_count, next_attempt_at, \
                   created_at, updated_at, completed_at) \
                 SELECT $1, d.subscription_id, d.event_id, d.tenant_id, d.application_id, \
                        d.environment_id, d.id, CASE WHEN $7 THEN 'SHADOW' ELSE 'PENDING' END, 0, \
                        $6, $6, $6, CASE WHEN $7 THEN $6 END \
                   FROM devplat_webhook_deliveries d \
                   JOIN devplat_webhook_subscriptions s ON s.id = d.subscription_id AND s.status = 'ACTIVE' \
                  WHERE d.id = $2 AND d.tenant_id = $3 AND d.application_id = $4 AND d.environment_id = $5",
                vec![
                    id.into(),
                    delivery.into(),
                    scope.tenant_id.into(),
                    scope.application_id.into(),
                    scope.environment_id.into(),
                    now.into(),
                    shadow.into(),
                ],
            ))
            .await?
            .rows_affected();
        if inserted == 0 {
            return Err(ApiError(ErrorCode::NotFound));
        }
        let row = self.delivery_in_scope(&txn, scope, id).await?;
        activity::record(
            &txn,
            caller,
            activity::Event {
                tenant_id: scope.tenant_id,
                action: "webhook.delivery_replayed",
                target_type: "WEBHOOK_DELIVERY",
                target_id: id.to_string(),
                summary: json!({
                    "environmentId": scope.environment_id,
                    "webhookId": row.subscription_id,
                    "replayOf": delivery,
                }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }
}

fn cursor_of(page: &Page) -> Result<(Option<DateTime<Utc>>, Option<Uuid>), ApiError> {
    match page.cursor_key() {
        Some((at, id)) => Ok((Some(at), Some(id))),
        None if page.has_cursor() => Err(ApiError(ErrorCode::ValidationError)),
        None => Ok((None, None)),
    }
}

/// Startup check of an enabled deployment: every stored signing secret must be openable
/// by the configured key ring (a key rotated away too early is refused at startup, not
/// discovered at delivery time). Without a key ring, subscription routes fail closed.
pub async fn require_signable<C: ConnectionTrait>(
    db: &C,
    settings: &WebhookSettings,
) -> Result<(), String> {
    let Some(ring) = settings.key_ring.as_deref() else {
        tracing::warn!("webhooks enabled without a sealing key: subscription routes answer 503");
        return Ok(());
    };
    let unknown = unknown_key_ids(db, ring).await.map_err(|e| {
        format!(
            "cannot read stored key ids ({})",
            crate::db::summarize_db_error(&e)
        )
    })?;
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "stored signing secrets are sealed under key id(s) {} that the configured key ring does not hold",
            unknown.join(", ")
        ))
    }
}

/// Key ids referenced by stored seals that `key_ring` cannot open (startup refuses to
/// serve webhooks with any: every stored secret must stay signable).
pub async fn unknown_key_ids<C: ConnectionTrait>(
    db: &C,
    key_ring: &SecretBox,
) -> Result<Vec<String>, DbErr> {
    let rows = db
        .query_all(stmt(
            "SELECT DISTINCT secret_key_id FROM devplat_webhook_subscriptions \
              WHERE status <> 'DELETED' ORDER BY 1",
            vec![],
        ))
        .await?;
    let mut unknown = Vec::new();
    for r in rows {
        let kid: String = r.try_get_by_index(0)?;
        if !key_ring.knows(&kid) {
            unknown.push(kid);
        }
    }
    Ok(unknown)
}
