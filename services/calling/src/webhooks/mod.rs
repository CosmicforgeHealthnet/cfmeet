//! Customer webhooks and durable event publication (Part G §67 / §68, ADR 0028).
//!
//! ```text
//! authoritative transaction ─▶ state + calling_outbox_events (PENDING)      (Calling)
//! privacy_relay_outbox ─▶ Privacy Relay publisher ─▶ POST /internal/v1/platform/events
//!            │                                             │
//!            ▼                                             ▼
//!   publisher (SKIP LOCKED + lease, bounded retry, DEAD_LETTER, replay) ─▶ EventTransport(s)
//!            │   (local transport: project ─▶ webhook_events ─▶ matching subscriptions;
//!            │    Part VI §53: and / or the Developer Platform, see [`publication`])
//!            ▼
//!   webhook_deliveries (one per subscription × event) ─▶ delivery worker (SKIP LOCKED +
//!   lease) ─▶ SSRF-safe signed POST ─▶ SUCCEEDED | RETRY_SCHEDULED (jittered backoff)
//!   | FAILED (dead letter) ; manual replay = a NEW delivery referencing the original
//! ```
//!
//! Provider / carrier callbacks are inputs to the domain; only the normalized public
//! projection ([`catalog`]) ever reaches a customer endpoint.

pub mod catalog;
pub mod http;
pub mod ingest;
pub mod keys;
pub mod notices;
pub mod ownership_migration;
pub mod publication;
pub mod worker;

use std::{sync::Arc, time::Duration};

use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    calls::command::CallScope,
    config::ConfigError,
    domain::SharedClock,
    error::CallingError,
    error::ErrorCode,
    platform::{context::PlatformAuthContext, scopes::ScopeSet},
};

pub type WebhookResult<T> = Result<T, CallingError>;

pub(crate) trait DbResultExt<T> {
    fn db(self) -> WebhookResult<T>;
}

impl<T> DbResultExt<T> for Result<T, DbErr> {
    fn db(self) -> WebhookResult<T> {
        self.map_err(|e| {
            tracing::error!(error = %e, "webhook store error");
            CallingError::Internal
        })
    }
}

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn conflict(code: ErrorCode) -> CallingError {
    CallingError::Call {
        code,
        status: axum::http::StatusCode::CONFLICT,
    }
}

/// Deployment configuration of customer webhooks.
#[derive(Clone)]
pub struct WebhookSettings {
    /// Seals signing secrets at rest; without it webhooks are unavailable (fail closed).
    pub secret_box: Option<Arc<SecretBox>>,
    pub policy: EndpointPolicy,
    pub request_timeout: Duration,
    pub max_attempts: u32,
    /// Part VI §53 migration freeze (`CALLING_WEBHOOK_MUTATIONS_FROZEN`, default `false`):
    /// customer create / update / delete / rotate-secret / replay answer 503
    /// `SERVICE_UNAVAILABLE` with `CF-Webhook-Migration: frozen`; reads and deliveries
    /// continue.
    pub mutations_frozen: bool,
    /// `CALLING_WEBHOOK_DELIVERY_ENABLED` (default `true`): `false` after the cutover — the
    /// delivery worker sends nothing; deliveries stay recorded (a rollback resumes them).
    pub delivery_enabled: bool,
    /// `CALLING_WEBHOOK_AUTHORITY` (default `active`): `retired` — customer webhook routes
    /// answer 503 `SERVICE_UNAVAILABLE` with `CF-Webhook-Migration: moved`, the delivery
    /// worker is off; every record is retained.
    pub authority: ownership_migration::WebhookAuthority,
    /// The migration transfer key (`CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY`): signing
    /// secrets leave Calling only re-sealed under it. Unset: the export of subscriptions
    /// (secrets) is refused.
    pub transfer_key: Option<Arc<SecretBox>>,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            secret_box: None,
            policy: EndpointPolicy::STRICT,
            request_timeout: Duration::from_secs(10),
            max_attempts: cf_webhooks::retry::MAX_ATTEMPTS,
            mutations_frozen: false,
            delivery_enabled: true,
            authority: ownership_migration::WebhookAuthority::Active,
            transfer_key: None,
        }
    }
}

impl WebhookSettings {
    /// `CALLING_WEBHOOK_SECRET_KEY` (base64, 32 bytes; from the secret manager) and
    /// `CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` (honoured ONLY in the `local` runtime).
    /// The key ring is read by [`WebhookSettings::secret_box_from_lookup`].
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
        local_runtime: bool,
    ) -> Result<Self, ConfigError> {
        let secret_box = Self::secret_box_from_lookup(get)?.map(Arc::new);
        let allow_private = matches!(
            get("CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS").as_deref(),
            Some("true") | Some("1")
        );
        if allow_private && !local_runtime {
            return Err(ConfigError::invalid(
                "CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "is permitted only in the local runtime environment".to_string(),
            ));
        }
        let migration = ownership_migration::MigrationFlags::from_lookup(get)?;
        Ok(Self {
            secret_box,
            policy: EndpointPolicy {
                allow_insecure_http: allow_private,
                allow_private_networks: allow_private,
            },
            mutations_frozen: migration.mutations_frozen,
            delivery_enabled: migration.delivery_enabled,
            authority: migration.authority,
            transfer_key: ownership_migration::transfer_key_from_lookup(get)?.map(Arc::new),
            ..Self::default()
        })
    }

    /// Whether the delivery worker sends (delivery enabled and the authority active).
    pub fn delivers(&self) -> bool {
        self.delivery_enabled && self.authority == ownership_migration::WebhookAuthority::Active
    }

    /// The signing-secret key ring (Part H §74.12). Every key may be a secret file
    /// (`<NAME>_FILE`, see [`crate::config::secret`]):
    ///
    /// | Variable | Meaning |
    /// |---|---|
    /// | `CALLING_WEBHOOK_SECRET_KEY` | current key (base64, 32 bytes, not weak); unset = webhooks unavailable |
    /// | `CALLING_WEBHOOK_SECRET_KEY_ID` | its id (default `k0`) |
    /// | `CALLING_WEBHOOK_SECRET_KEY_PREVIOUS` | previous key, opens only (rotation overlap) |
    /// | `CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID` | its id (required with the previous key) |
    /// | `CALLING_WEBHOOK_SECRET_LEGACY_KEY_ID` | the id legacy `v1.` seals were made with (default `k0`) |
    pub fn secret_box_from_lookup(
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<SecretBox>, ConfigError> {
        use cf_webhooks::secret_box::{SealingKey, SecretBoxError, DEFAULT_KEY_ID};
        use secrecy::ExposeSecret;
        let id = |var: &'static str| {
            get(var)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let key_error = |var: &'static str, e: SecretBoxError| {
            ConfigError::invalid(
                var,
                match e {
                    SecretBoxError::WeakKey => {
                        "is a weak key (repeated or low-entropy bytes); generate 32 random bytes"
                    }
                    SecretBoxError::KeyId => "has an invalid key id ([A-Za-z0-9_-], 1-32)",
                    _ => "must be 32 bytes, base64",
                }
                .to_string(),
            )
        };
        let current = crate::config::secret::read_secret(
            get,
            "CALLING_WEBHOOK_SECRET_KEY",
            "CALLING_WEBHOOK_SECRET_KEY_FILE",
        )?;
        let previous = crate::config::secret::read_secret(
            get,
            "CALLING_WEBHOOK_SECRET_KEY_PREVIOUS",
            "CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_FILE",
        )?;
        let Some(current) = current else {
            if previous.is_some() {
                return Err(ConfigError::Missing {
                    var: "CALLING_WEBHOOK_SECRET_KEY",
                });
            }
            return Ok(None);
        };
        let current_id =
            id("CALLING_WEBHOOK_SECRET_KEY_ID").unwrap_or_else(|| DEFAULT_KEY_ID.to_string());
        let current = SealingKey::from_base64(&current_id, current.expose_secret())
            .map_err(|e| key_error("CALLING_WEBHOOK_SECRET_KEY", e))?;
        let previous = match previous {
            None => None,
            Some(raw) => {
                let previous_id =
                    id("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID").ok_or(ConfigError::Missing {
                        var: "CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID",
                    })?;
                Some(
                    SealingKey::from_base64(&previous_id, raw.expose_secret())
                        .map_err(|e| key_error("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", e))?,
                )
            }
        };
        let secret_box = SecretBox::new(current, previous).map_err(|_| {
            ConfigError::invalid(
                "CALLING_WEBHOOK_SECRET_KEY_PREVIOUS",
                "must differ from the current key in both id and value".to_string(),
            )
        })?;
        let legacy = id("CALLING_WEBHOOK_SECRET_LEGACY_KEY_ID")
            .unwrap_or_else(|| DEFAULT_KEY_ID.to_string());
        let secret_box = secret_box
            .with_legacy_key_id(&legacy)
            .map_err(|e| key_error("CALLING_WEBHOOK_SECRET_LEGACY_KEY_ID", e))?;
        Ok(Some(secret_box))
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct SubscriptionRow {
    pub id: Uuid,
    pub endpoint_url: String,
    pub description: Option<String>,
    pub status: String,
    pub event_types: Vec<String>,
    pub secret_version: i32,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Never the sealed secret: views and checks do not load it.
const SUBSCRIPTION_COLUMNS: &str = "id, endpoint_url, description, status, event_types, \
    secret_version, version, created_at, updated_at";

/// The safe view: never the secret.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookView {
    pub webhook_id: Uuid,
    pub url: String,
    pub event_types: Vec<String>,
    pub description: Option<String>,
    pub status: String,
    pub secret_version: i32,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl From<SubscriptionRow> for WebhookView {
    fn from(r: SubscriptionRow) -> Self {
        Self {
            webhook_id: r.id,
            url: r.endpoint_url,
            event_types: r.event_types,
            description: r.description,
            status: r.status,
            secret_version: r.secret_version,
            version: r.version,
            created_at: ts(r.created_at),
            updated_at: ts(r.updated_at),
        }
    }
}

/// Creation / rotation result: the ONLY time the signing secret is returned.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookWithSecret {
    pub webhook: WebhookView,
    pub signing_secret: String,
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct DeliveryRow {
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

const DELIVERY_COLUMNS: &str = "d.id, d.subscription_id, e.event_id, e.event_type, d.replay_of, \
    d.status, d.attempt_count, d.next_attempt_at, d.last_http_status, d.failure_category, \
    d.created_at, d.completed_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryView {
    pub delivery_id: Uuid,
    pub webhook_id: Uuid,
    pub event_id: String,
    pub event_type: String,
    pub replay_of: Option<Uuid>,
    pub status: String,
    pub attempt_count: i32,
    pub next_attempt_at: Option<String>,
    pub last_http_status: Option<i32>,
    pub failure_category: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

impl From<DeliveryRow> for DeliveryView {
    fn from(r: DeliveryRow) -> Self {
        let open = r.completed_at.is_none();
        Self {
            delivery_id: r.id,
            webhook_id: r.subscription_id,
            event_id: r.event_id,
            event_type: r.event_type,
            replay_of: r.replay_of,
            status: r.status,
            attempt_count: r.attempt_count,
            next_attempt_at: open.then(|| ts(r.next_attempt_at)),
            last_http_status: r.last_http_status,
            failure_category: r.failure_category,
            created_at: ts(r.created_at),
            completed_at: r.completed_at.map(ts),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

/// A requested change of a subscription.
#[derive(Debug, Default, Clone)]
pub struct WebhookChange {
    pub url: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub description: Option<Option<String>>,
    pub status: Option<String>,
}

/// Durable audit of a subscription change (request / correlation id, actor, versions;
/// never the URL secret or the signing secret) in the caller's transaction.
async fn audit<C: ConnectionTrait>(
    db: &C,
    ctx: &PlatformAuthContext,
    id: Uuid,
    operation: crate::enterprise::audit::AuditOperation,
    versions: (Option<i64>, Option<i64>),
    at: DateTime<Utc>,
) -> WebhookResult<()> {
    crate::enterprise::audit::record(
        db,
        &crate::enterprise::AdminActor::of(ctx),
        crate::enterprise::audit::AuditFact {
            resource: crate::enterprise::audit::AuditResource::WebhookSubscription,
            resource_id: id,
            operation,
            old_version: versions.0,
            new_version: versions.1,
            details: crate::enterprise::audit::AuditDetails::new(),
        },
        at,
    )
    .await
    .map_err(|_| CallingError::Internal)
}

/// The customer webhook service (Calling-owned state; `cf_webhooks` primitives).
#[derive(Clone)]
pub struct WebhookService {
    db: DatabaseConnection,
    clock: SharedClock,
    settings: WebhookSettings,
    client: reqwest::Client,
}

impl WebhookService {
    pub fn new(db: DatabaseConnection, clock: SharedClock, settings: WebhookSettings) -> Self {
        let client = cf_webhooks::endpoint::client(settings.policy, settings.request_timeout);
        Self {
            db,
            clock,
            settings,
            client,
        }
    }

    /// Replaces the delivery client (tests inject a rebinding resolver).
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub(crate) fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    /// The configured signing-secret key ring (`None`: webhooks unavailable).
    pub fn key_ring(&self) -> Option<&SecretBox> {
        self.settings.secret_box.as_deref()
    }

    /// The deployment settings (migration flags included).
    pub fn settings(&self) -> &WebhookSettings {
        &self.settings
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn secret_box(&self) -> WebhookResult<&SecretBox> {
        self.settings
            .secret_box
            .as_deref()
            .ok_or(CallingError::Call {
                code: ErrorCode::CapabilityUnavailable,
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            })
    }

    fn validate_url(&self, url: &str) -> WebhookResult<String> {
        cf_webhooks::endpoint::validate_url(url.trim(), self.settings.policy)
            .map(|u| u.to_string())
            .map_err(|e| {
                CallingError::Validation(match e {
                    cf_webhooks::endpoint::EndpointError::Scheme => {
                        "url must be an https URL."
                    }
                    cf_webhooks::endpoint::EndpointError::Credentials => {
                        "url must not embed credentials."
                    }
                    cf_webhooks::endpoint::EndpointError::Port => "url uses a prohibited port.",
                    cf_webhooks::endpoint::EndpointError::Host
                    | cf_webhooks::endpoint::EndpointError::ProhibitedAddress => {
                        "url must be a public endpoint (no local, private, link-local or metadata address)."
                    }
                    cf_webhooks::endpoint::EndpointError::Invalid => "url is not a valid URL.",
                })
            })
    }

    /// Event types must exist in the public catalog and be readable by the credential.
    fn validate_event_types(types: &[String], scopes: &ScopeSet) -> WebhookResult<Vec<String>> {
        if types.is_empty() || types.len() > 64 {
            return Err(CallingError::Validation(
                "eventTypes must list 1-64 public event types.",
            ));
        }
        let mut out: Vec<String> = Vec::new();
        for t in types {
            let Some(def) = catalog::definition(t) else {
                return Err(CallingError::Validation(
                    "eventTypes contains an unknown or non-public event type.",
                ));
            };
            if !scopes.contains(def.scope) {
                return Err(CallingError::InsufficientScope {
                    required: def.scope.to_string(),
                });
            }
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
        out.sort();
        Ok(out)
    }

    fn description(d: Option<String>) -> WebhookResult<Option<String>> {
        match d.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
            Some(d) if d.chars().count() > 200 || d.chars().any(char::is_control) => Err(
                CallingError::Validation("description must be at most 200 printable characters."),
            ),
            other => Ok(other),
        }
    }

    pub async fn create(
        &self,
        ctx: &PlatformAuthContext,
        url: &str,
        event_types: &[String],
        description: Option<String>,
    ) -> WebhookResult<WebhookWithSecret> {
        let secret_box = self.secret_box()?;
        let url = self.validate_url(url)?;
        let types = Self::validate_event_types(event_types, ctx.scopes())?;
        let description = Self::description(description)?;
        let scope = CallScope::of_platform(ctx);
        let id = Uuid::new_v4();
        let secret = cf_webhooks::signing::generate_secret();
        let sealed = secret_box.seal(&secret, &id.to_string());
        let now = self.now();
        let txn = self.db.begin().await.db()?;
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "INSERT INTO webhook_subscriptions (id, tenant_id, application_id, environment_id, \
                   endpoint_url, description, status, event_types, secret_sealed, secret_version, \
                   created_by_credential_id, version, created_at, updated_at, secret_key_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, 1, $9, 1, $10, $10, $11) \
                 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![
                id.into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                url.into(),
                description.into(),
                types.into(),
                sealed.into(),
                ctx.credential_id().as_uuid().into(),
                now.into(),
                secret_box.current_kid().into(),
            ],
        ))
        .one(&txn)
        .await
        .db()?
        .ok_or(CallingError::Internal)?;
        audit(
            &txn,
            ctx,
            id,
            crate::enterprise::audit::AuditOperation::Create,
            (None, Some(1)),
            now,
        )
        .await?;
        txn.commit().await.db()?;
        tracing::info!(webhook_id = %id, request_id = %ctx.request_id().as_str(),
                       correlation_id = %ctx.correlation_id().as_str(), "webhook subscription created");
        Ok(WebhookWithSecret {
            webhook: row.into(),
            signing_secret: secret,
        })
    }

    async fn row<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        id: Uuid,
        lock: bool,
    ) -> WebhookResult<SubscriptionRow> {
        SubscriptionRow::find_by_statement(stmt(
            &format!(
                "SELECT {SUBSCRIPTION_COLUMNS} FROM webhook_subscriptions WHERE id = $1 \
                   AND tenant_id = $2 AND application_id = $3 AND environment_id = $4 \
                   AND status <> 'DELETED'{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![
                id.into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
            ],
        ))
        .one(db)
        .await
        .db()?
        .ok_or(CallingError::NotFound)
    }

    pub async fn get(&self, scope: CallScope, id: Uuid) -> WebhookResult<WebhookView> {
        Ok(self.row(&self.db, scope, id, false).await?.into())
    }

    pub async fn list(
        &self,
        scope: CallScope,
        limit: u64,
        cursor: Option<(DateTime<Utc>, Uuid)>,
    ) -> WebhookResult<Page<WebhookView>> {
        let (after_at, after_id) = cursor
            .map(|(a, i)| (Some(a), Some(i)))
            .unwrap_or((None, None));
        let rows = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "SELECT {SUBSCRIPTION_COLUMNS} FROM webhook_subscriptions \
                  WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                    AND status <> 'DELETED' \
                    AND ($4::timestamptz IS NULL OR (created_at, id) > ($4, $5)) \
                  ORDER BY created_at, id LIMIT $6"
            ),
            vec![
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                after_at.into(),
                after_id.into(),
                (limit as i64 + 1).into(),
            ],
        ))
        .all(&self.db)
        .await
        .db()?;
        Ok(page(rows, limit, |r| (r.created_at, r.id)))
    }

    pub async fn update(
        &self,
        ctx: &PlatformAuthContext,
        id: Uuid,
        expected_version: i64,
        change: WebhookChange,
    ) -> WebhookResult<WebhookView> {
        let scope = CallScope::of_platform(ctx);
        let url = change
            .url
            .as_deref()
            .map(|u| self.validate_url(u))
            .transpose()?;
        let types = change
            .event_types
            .as_deref()
            .map(|t| Self::validate_event_types(t, ctx.scopes()))
            .transpose()?;
        let description = change.description.map(Self::description).transpose()?;
        if let Some(s) = change.status.as_deref() {
            if !matches!(s, "ACTIVE" | "DISABLED") {
                return Err(CallingError::Validation(
                    "status must be ACTIVE or DISABLED.",
                ));
            }
        }
        let txn = self.db.begin().await.db()?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(conflict(ErrorCode::VersionConflict));
        }
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE webhook_subscriptions SET endpoint_url = COALESCE($2, endpoint_url), \
                   event_types = COALESCE($3, event_types), \
                   description = CASE WHEN $4 THEN $5 ELSE description END, \
                   status = COALESCE($6, status), version = version + 1, updated_at = $7 \
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
        .await
        .db()?
        .ok_or(CallingError::Internal)?;
        audit(
            &txn,
            ctx,
            id,
            crate::enterprise::audit::AuditOperation::Update,
            (Some(current.version), Some(row.version)),
            self.now(),
        )
        .await?;
        txn.commit().await.db()?;
        Ok(row.into())
    }

    /// Deletes the subscription; its open deliveries are CANCELLED (history is kept).
    pub async fn delete(
        &self,
        ctx: &PlatformAuthContext,
        id: Uuid,
        expected_version: i64,
    ) -> WebhookResult<WebhookView> {
        let scope = CallScope::of_platform(ctx);
        let txn = self.db.begin().await.db()?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(conflict(ErrorCode::VersionConflict));
        }
        let now = self.now();
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE webhook_subscriptions SET status = 'DELETED', deleted_at = $2, \
                   version = version + 1, updated_at = $2 WHERE id = $1 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![id.into(), now.into()],
        ))
        .one(&txn)
        .await
        .db()?
        .ok_or(CallingError::Internal)?;
        txn.execute(stmt(
            "UPDATE webhook_deliveries SET status = 'CANCELLED', completed_at = $2, \
               lease_until = NULL, updated_at = $2 \
             WHERE subscription_id = $1 AND status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING')",
            vec![id.into(), now.into()],
        ))
        .await
        .db()?;
        audit(
            &txn,
            ctx,
            id,
            crate::enterprise::audit::AuditOperation::Delete,
            (Some(current.version), Some(row.version)),
            now,
        )
        .await?;
        txn.commit().await.db()?;
        Ok(row.into())
    }

    /// A new signing secret (returned once); deliveries from now on use it.
    pub async fn rotate_secret(
        &self,
        ctx: &PlatformAuthContext,
        id: Uuid,
        expected_version: i64,
    ) -> WebhookResult<WebhookWithSecret> {
        let scope = CallScope::of_platform(ctx);
        let secret_box = self.secret_box()?;
        let txn = self.db.begin().await.db()?;
        let current = self.row(&txn, scope, id, true).await?;
        if current.version != expected_version {
            return Err(conflict(ErrorCode::VersionConflict));
        }
        let secret = cf_webhooks::signing::generate_secret();
        let row = SubscriptionRow::find_by_statement(stmt(
            &format!(
                "UPDATE webhook_subscriptions SET secret_sealed = $2, secret_key_id = $4, \
                   secret_version = secret_version + 1, version = version + 1, updated_at = $3 \
                 WHERE id = $1 RETURNING {SUBSCRIPTION_COLUMNS}"
            ),
            vec![
                id.into(),
                secret_box.seal(&secret, &id.to_string()).into(),
                self.now().into(),
                secret_box.current_kid().into(),
            ],
        ))
        .one(&txn)
        .await
        .db()?
        .ok_or(CallingError::Internal)?;
        audit(
            &txn,
            ctx,
            id,
            crate::enterprise::audit::AuditOperation::RotateSecret,
            (Some(current.version), Some(row.version)),
            self.now(),
        )
        .await?;
        txn.commit().await.db()?;
        Ok(WebhookWithSecret {
            webhook: row.into(),
            signing_secret: secret,
        })
    }

    pub async fn deliveries(
        &self,
        scope: CallScope,
        subscription: Uuid,
        status: Option<&str>,
        limit: u64,
        cursor: Option<(DateTime<Utc>, Uuid)>,
    ) -> WebhookResult<Page<DeliveryView>> {
        self.row(&self.db, scope, subscription, false).await?;
        if let Some(s) = status {
            if !matches!(
                s,
                "PENDING" | "DELIVERING" | "RETRY_SCHEDULED" | "SUCCEEDED" | "FAILED" | "CANCELLED"
            ) {
                return Err(CallingError::Validation("status is not a delivery status."));
            }
        }
        let (after_at, after_id) = cursor
            .map(|(a, i)| (Some(a), Some(i)))
            .unwrap_or((None, None));
        let rows = DeliveryRow::find_by_statement(stmt(
            &format!(
                "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries d \
                   JOIN webhook_events e ON e.id = d.webhook_event_id \
                  WHERE d.subscription_id = $1 AND d.tenant_id = $2 AND d.application_id = $3 \
                    AND d.environment_id = $4 AND ($5::text IS NULL OR d.status = $5) \
                    AND ($6::timestamptz IS NULL OR (d.created_at, d.id) > ($6, $7)) \
                  ORDER BY d.created_at, d.id LIMIT $8"
            ),
            vec![
                subscription.into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                status.map(str::to_string).into(),
                after_at.into(),
                after_id.into(),
                (limit as i64 + 1).into(),
            ],
        ))
        .all(&self.db)
        .await
        .db()?;
        Ok(page(rows, limit, |r| (r.created_at, r.id)))
    }

    /// Manual replay: a NEW delivery of the same event to the same (active) subscription,
    /// referencing the original. The domain action is never re-executed.
    pub async fn replay(
        &self,
        ctx: &PlatformAuthContext,
        delivery: Uuid,
    ) -> WebhookResult<DeliveryView> {
        let scope = CallScope::of_platform(ctx);
        let now = self.now();
        let id = Uuid::new_v4();
        let txn = self.db.begin().await.db()?;
        let inserted = txn
            .execute(stmt(
                "INSERT INTO webhook_deliveries (id, webhook_event_id, subscription_id, tenant_id, \
                   application_id, environment_id, replay_of, status, attempt_count, next_attempt_at, \
                   created_at, updated_at) \
                 SELECT $1, d.webhook_event_id, d.subscription_id, d.tenant_id, d.application_id, \
                        d.environment_id, d.id, 'PENDING', 0, $6, $6, $6 \
                   FROM webhook_deliveries d \
                   JOIN webhook_subscriptions s ON s.id = d.subscription_id AND s.status = 'ACTIVE' \
                  WHERE d.id = $2 AND d.tenant_id = $3 AND d.application_id = $4 AND d.environment_id = $5",
                vec![
                    id.into(),
                    delivery.into(),
                    scope.tenant_id().as_uuid().into(),
                    scope.application_id().as_uuid().into(),
                    scope.environment_id().as_uuid().into(),
                    now.into(),
                ],
            ))
            .await
            .db()?
            .rows_affected();
        if inserted == 0 {
            return Err(CallingError::NotFound);
        }
        let subscription: Uuid = txn
            .query_one(stmt(
                "SELECT subscription_id FROM webhook_deliveries WHERE id = $1",
                vec![id.into()],
            ))
            .await
            .db()?
            .ok_or(CallingError::Internal)?
            .try_get_by_index(0)
            .db()?;
        audit(
            &txn,
            ctx,
            subscription,
            crate::enterprise::audit::AuditOperation::Replay,
            (None, None),
            now,
        )
        .await?;
        txn.commit().await.db()?;
        crate::metrics::inc("calling_webhook_replays_total", &[]);
        self.delivery_view(id).await
    }

    pub(crate) async fn delivery_view(&self, id: Uuid) -> WebhookResult<DeliveryView> {
        Ok(DeliveryRow::find_by_statement(stmt(
            &format!(
                "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries d \
                   JOIN webhook_events e ON e.id = d.webhook_event_id WHERE d.id = $1"
            ),
            vec![id.into()],
        ))
        .one(&self.db)
        .await
        .db()?
        .ok_or(CallingError::NotFound)?
        .into())
    }

    /// Records a normalized public event (idempotent on its source; only when an ACTIVE
    /// subscription of the application wants its type) and fans it out to the matching
    /// ACTIVE subscriptions of the SAME tenant / application / environment —
    /// one delivery per (subscription, event), whatever the number of publications.
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_public_event<C: ConnectionTrait>(
        &self,
        db: &C,
        producer: &str,
        source_event_id: &str,
        scope: (Uuid, Uuid, Uuid),
        occurred_at: DateTime<Utc>,
        correlation_id: Option<&str>,
        event: &catalog::PublicEvent,
    ) -> WebhookResult<usize> {
        ingest_public_event(
            db,
            self.now(),
            producer,
            source_event_id,
            scope,
            occurred_at,
            correlation_id,
            event,
        )
        .await
    }
}

/// Stores one public event (deduplicated on `(producer, source_event_id)`) when at least
/// one ACTIVE subscription of its scope wants its type, and fans it out to one delivery
/// per such subscription — inside the caller's transaction or connection.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_public_event<C: ConnectionTrait>(
    db: &C,
    now: DateTime<Utc>,
    producer: &str,
    source_event_id: &str,
    scope: (Uuid, Uuid, Uuid),
    occurred_at: DateTime<Utc>,
    correlation_id: Option<&str>,
    event: &catalog::PublicEvent,
) -> WebhookResult<usize> {
    let event_id = format!("whe_{}", Uuid::new_v4().simple());
    let mut body = serde_json::json!({
        "id": event_id,
        "type": event.event_type,
        "apiVersion": "v1",
        "occurredAt": ts(occurred_at),
        "data": Value::Object(event.data.clone()),
    });
    if let Some(c) = correlation_id.filter(|c| {
        c.len() <= 64
            && c.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        body["correlationId"] = Value::String(c.to_string());
    }
    db.execute(stmt(
        "INSERT INTO webhook_events (id, event_id, source_event_id, producer, event_type, \
           tenant_id, application_id, environment_id, occurred_at, body, created_at) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11 \
          WHERE EXISTS (SELECT 1 FROM webhook_subscriptions s WHERE s.tenant_id = $6 \
                          AND s.application_id = $7 AND s.environment_id = $8 \
                          AND s.status = 'ACTIVE' AND $5 = ANY (s.event_types)) \
         ON CONFLICT (producer, source_event_id) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            event_id.into(),
            source_event_id.into(),
            producer.into(),
            event.event_type.into(),
            scope.0.into(),
            scope.1.into(),
            scope.2.into(),
            occurred_at.into(),
            body.into(),
            now.into(),
        ],
    ))
    .await
    .db()?;
    let fanned = db
        .execute(stmt(
            "INSERT INTO webhook_deliveries (id, webhook_event_id, subscription_id, tenant_id, \
               application_id, environment_id, status, attempt_count, next_attempt_at, \
               created_at, updated_at) \
             SELECT gen_random_uuid(), e.id, s.id, s.tenant_id, s.application_id, s.environment_id, \
                    'PENDING', 0, $3, $3, $3 \
               FROM webhook_events e JOIN webhook_subscriptions s \
                 ON s.tenant_id = e.tenant_id AND s.application_id = e.application_id \
                AND s.environment_id = e.environment_id \
              WHERE e.producer = $1 AND e.source_event_id = $2 AND s.status = 'ACTIVE' \
                AND e.event_type = ANY (s.event_types) \
             ON CONFLICT (subscription_id, webhook_event_id) WHERE replay_of IS NULL DO NOTHING",
            vec![producer.into(), source_event_id.into(), now.into()],
        ))
        .await
        .db()?
        .rows_affected();
    Ok(fanned as usize)
}

/// Keyset page with `limit` (one extra row fetched to know whether more exist).
fn page<R, V: From<R>>(
    mut rows: Vec<R>,
    limit: u64,
    key: impl Fn(&R) -> (DateTime<Utc>, Uuid),
) -> Page<V> {
    let more = rows.len() as u64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = more
        .then(|| {
            rows.last().map(|r| {
                let (at, id) = key(r);
                crate::enterprise::cursor::encode(at, id)
            })
        })
        .flatten();
    Page {
        data: rows.into_iter().map(V::from).collect(),
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::Engine;

    use super::*;

    fn b64(seed: u8) -> String {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = seed.wrapping_add((i as u8).wrapping_mul(7));
        }
        base64::engine::general_purpose::STANDARD.encode(k)
    }

    fn lookup(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn ring(pairs: &[(&str, String)]) -> Result<Option<SecretBox>, ConfigError> {
        WebhookSettings::secret_box_from_lookup(&lookup(pairs))
    }

    #[test]
    fn the_key_ring_has_ids_a_previous_key_and_a_legacy_id() {
        assert!(ring(&[]).unwrap().is_none(), "no key: webhooks unavailable");
        let one = ring(&[("CALLING_WEBHOOK_SECRET_KEY", b64(1))])
            .unwrap()
            .unwrap();
        assert_eq!((one.current_kid(), one.previous_kid()), ("k0", None));
        assert_eq!(one.legacy_kid(), "k0");
        let rotated = ring(&[
            ("CALLING_WEBHOOK_SECRET_KEY", b64(2)),
            ("CALLING_WEBHOOK_SECRET_KEY_ID", "k1".into()),
            ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", b64(1)),
            ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "k0".into()),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (rotated.current_kid(), rotated.previous_kid()),
            ("k1", Some("k0"))
        );
        // Sealed under the old single-key deployment, opened by the rotated one.
        let sealed = one.seal("whsec_x", "sub");
        assert_eq!(rotated.open(&sealed, "sub").unwrap(), "whsec_x");
        let declared = ring(&[
            ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
            ("CALLING_WEBHOOK_SECRET_KEY_ID", "main".into()),
            ("CALLING_WEBHOOK_SECRET_LEGACY_KEY_ID", "main".into()),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(declared.legacy_kid(), "main");
    }

    #[test]
    fn weak_ambiguous_or_incomplete_key_rings_are_refused() {
        let weak = base64::engine::general_purpose::STANDARD.encode([42u8; 32]);
        let mut low = [0u8; 32];
        for (i, b) in low.iter_mut().enumerate() {
            *b = (i % 8) as u8;
        }
        let low = base64::engine::general_purpose::STANDARD.encode(low);
        for (case, pairs) in [
            (
                "weak current",
                vec![("CALLING_WEBHOOK_SECRET_KEY", weak.clone())],
            ),
            ("low entropy", vec![("CALLING_WEBHOOK_SECRET_KEY", low)]),
            (
                "short",
                vec![("CALLING_WEBHOOK_SECRET_KEY", "c2hvcnQ=".to_string())],
            ),
            (
                "bad id",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_ID", "a.b".to_string()),
                ],
            ),
            (
                "previous without id",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", b64(2)),
                ],
            ),
            (
                "previous without current",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", b64(2)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "old".to_string()),
                ],
            ),
            (
                "previous equals current id",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", b64(2)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "k0".to_string()),
                ],
            ),
            (
                "previous equals current key",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "old".to_string()),
                ],
            ),
            (
                "weak previous",
                vec![
                    ("CALLING_WEBHOOK_SECRET_KEY", b64(1)),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS", weak.clone()),
                    ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "old".to_string()),
                ],
            ),
        ] {
            let err = ring(&pairs).expect_err(case);
            for (_, value) in &pairs {
                assert!(!err.to_string().contains(value.as_str()), "{case}: {err}");
            }
        }
    }

    #[test]
    fn the_key_may_be_a_secret_file() {
        use crate::config::secret::tests::TempSecret;
        let file = TempSecret::new(&format!("{}\n", b64(5)), 0o600);
        let previous = TempSecret::new(&b64(6), 0o600);
        let pairs = [
            ("CF_RUNTIME_ENVIRONMENT", "production".to_string()),
            ("CALLING_WEBHOOK_SECRET_KEY_FILE", file.path()),
            ("CALLING_WEBHOOK_SECRET_KEY_ID", "k2".to_string()),
            ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_FILE", previous.path()),
            ("CALLING_WEBHOOK_SECRET_KEY_PREVIOUS_ID", "k1".to_string()),
        ];
        let ring_from_files = ring(&pairs).unwrap().unwrap();
        assert_eq!(ring_from_files.current_kid(), "k2");
        let mut both = pairs.to_vec();
        both.push(("CALLING_WEBHOOK_SECRET_KEY", b64(5)));
        assert!(ring(&both).is_err());
        let missing = [(
            "CALLING_WEBHOOK_SECRET_KEY_FILE",
            "/nonexistent/webhook-key".to_string(),
        )];
        assert!(ring(&missing).is_err(), "never falls back to 'no webhooks'");
        // The full settings parser applies the same ring.
        let settings = WebhookSettings::from_lookup(&lookup(&pairs), false).unwrap();
        assert_eq!(settings.secret_box.unwrap().previous_kid(), Some("k1"));
    }
}
