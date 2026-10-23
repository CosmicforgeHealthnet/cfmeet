//! The integration boundary to Calling's controlled webhook export (Section 53 §9.1):
//! `GET /internal/v1/webhook-migration/**` (`webhook-migration.export`) and the rollback
//! routes (`webhook-migration.import`), over authenticated internal HTTP. The Developer
//! Platform never reads Calling's database.

use std::sync::Arc;

use axum::http::Method;
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// An exported entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Subscriptions,
    Events,
    Deliveries,
    Attempts,
}

impl Entity {
    pub const ALL: [Entity; 4] = [
        Entity::Subscriptions,
        Entity::Events,
        Entity::Deliveries,
        Entity::Attempts,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Entity::Subscriptions => "subscriptions",
            Entity::Events => "events",
            Entity::Deliveries => "deliveries",
            Entity::Attempts => "attempts",
        }
    }

    /// The import phase name of the entity.
    pub fn phase(self) -> &'static str {
        match self {
            Entity::Subscriptions => "SUBSCRIPTIONS",
            Entity::Events => "EVENTS",
            Entity::Deliveries => "DELIVERIES",
            Entity::Attempts => "ATTEMPTS",
        }
    }

    pub fn from_phase(phase: &str) -> Option<Entity> {
        Entity::ALL.into_iter().find(|e| e.phase() == phase)
    }

    pub fn next(self) -> Option<Entity> {
        match self {
            Entity::Subscriptions => Some(Entity::Events),
            Entity::Events => Some(Entity::Deliveries),
            Entity::Deliveries => Some(Entity::Attempts),
            Entity::Attempts => None,
        }
    }
}

/// One export page.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPage {
    pub data: Vec<Value>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("Calling is not configured (CF_CALLING_BASE_URL)")]
    NotConfigured,
    #[error("Calling is unavailable: {0}")]
    Unavailable(String),
    #[error("Calling refused the request: HTTP {status} {code}")]
    Refused { status: u16, code: String },
    #[error("Calling returned a malformed export")]
    Malformed,
}

impl SourceError {
    /// A bounded code for run records and responses.
    pub fn code(&self) -> &'static str {
        match self {
            SourceError::NotConfigured => "CALLING_NOT_CONFIGURED",
            SourceError::Unavailable(_) => "CALLING_UNAVAILABLE",
            SourceError::Refused { .. } => "CALLING_REFUSED",
            SourceError::Malformed => "CALLING_MALFORMED_EXPORT",
        }
    }
}

/// Calling's migration API.
#[async_trait::async_trait]
pub trait CallingMigrationPort: Send + Sync {
    async fn inventory(&self) -> Result<Value, SourceError>;
    async fn digest(&self) -> Result<Value, SourceError>;
    async fn page(
        &self,
        entity: Entity,
        cursor: Option<&str>,
        limit: u32,
        open_only: bool,
    ) -> Result<ExportPage, SourceError>;
    async fn rollback_subscriptions(&self, subscriptions: Vec<Value>)
        -> Result<Value, SourceError>;
    async fn settle_deliveries(&self, ids: &[Uuid]) -> Result<Value, SourceError>;
}

/// No Calling configured: every migration command fails closed.
pub struct UnconfiguredCalling;

#[async_trait::async_trait]
impl CallingMigrationPort for UnconfiguredCalling {
    async fn inventory(&self) -> Result<Value, SourceError> {
        Err(SourceError::NotConfigured)
    }
    async fn digest(&self) -> Result<Value, SourceError> {
        Err(SourceError::NotConfigured)
    }
    async fn page(
        &self,
        _: Entity,
        _: Option<&str>,
        _: u32,
        _: bool,
    ) -> Result<ExportPage, SourceError> {
        Err(SourceError::NotConfigured)
    }
    async fn rollback_subscriptions(&self, _: Vec<Value>) -> Result<Value, SourceError> {
        Err(SourceError::NotConfigured)
    }
    async fn settle_deliveries(&self, _: &[Uuid]) -> Result<Value, SourceError> {
        Err(SourceError::NotConfigured)
    }
}

/// Calling over authenticated internal HTTP.
pub struct HttpCallingMigration {
    client: Arc<InternalServiceClient>,
}

impl HttpCallingMigration {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, SourceError> {
        let request = OutboundServiceRequest::new_operation(ServiceId::calling());
        let response = self
            .client
            .exchange_json_query(&request, method, path, query, body)
            .await
            .map_err(|e| SourceError::Unavailable(e.to_string()))?;
        match response.status() {
            200 => response.json::<Value>().map_err(|_| SourceError::Malformed),
            status => Err(SourceError::Refused {
                status,
                code: response
                    .error_code()
                    .unwrap_or_default()
                    .chars()
                    .take(64)
                    .collect(),
            }),
        }
    }
}

#[async_trait::async_trait]
impl CallingMigrationPort for HttpCallingMigration {
    async fn inventory(&self) -> Result<Value, SourceError> {
        self.call(
            Method::GET,
            "/internal/v1/webhook-migration/inventory",
            &[],
            None,
        )
        .await
    }

    async fn digest(&self) -> Result<Value, SourceError> {
        self.call(
            Method::GET,
            "/internal/v1/webhook-migration/digest",
            &[],
            None,
        )
        .await
    }

    async fn page(
        &self,
        entity: Entity,
        cursor: Option<&str>,
        limit: u32,
        open_only: bool,
    ) -> Result<ExportPage, SourceError> {
        let limit = limit.to_string();
        let mut query = vec![("limit", limit.as_str())];
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        if open_only {
            query.push(("status", "open"));
        }
        let path = format!("/internal/v1/webhook-migration/{}", entity.as_str());
        let v = self.call(Method::GET, &path, &query, None).await?;
        serde_json::from_value(v).map_err(|_| SourceError::Malformed)
    }

    async fn rollback_subscriptions(
        &self,
        subscriptions: Vec<Value>,
    ) -> Result<Value, SourceError> {
        self.call(
            Method::POST,
            "/internal/v1/webhook-migration/rollback/subscriptions",
            &[],
            Some(&json!({ "subscriptions": subscriptions })),
        )
        .await
    }

    async fn settle_deliveries(&self, ids: &[Uuid]) -> Result<Value, SourceError> {
        self.call(
            Method::POST,
            "/internal/v1/webhook-migration/rollback/deliveries",
            &[],
            Some(&json!({ "deliveryIds": ids })),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Exported records (Calling's `cfmeet.calling.http-api` WebhookMigration* schemas).
// ---------------------------------------------------------------------------

/// A secret in transit: transfer-sealed or UNOPENABLE. No `Debug` output of the seal.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallingSecret {
    pub state: String,
    #[serde(default)]
    pub transfer_key_id: Option<String>,
    #[serde(default)]
    pub transfer_sealed: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub unopenable_reason: Option<String>,
}

impl std::fmt::Debug for CallingSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CallingSecret({}, [REDACTED])", self.state)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallingSubscription {
    pub webhook_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    #[serde(default)]
    pub environment_kind: Option<String>,
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    pub event_types: Vec<String>,
    pub status: String,
    pub secret_version: i32,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    pub secret: CallingSecret,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallingEvent {
    pub event_id: String,
    pub producer: String,
    pub source_event_id: String,
    pub event_type: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub body: Value,
    pub body_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallingDelivery {
    pub delivery_id: Uuid,
    pub webhook_id: Uuid,
    pub event_id: String,
    pub producer: String,
    pub source_event_id: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    #[serde(default)]
    pub replay_of: Option<Uuid>,
    pub status: String,
    pub attempt_count: i32,
    pub next_attempt_at: DateTime<Utc>,
    #[serde(default)]
    pub lease_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_http_status: Option<i32>,
    #[serde(default)]
    pub failure_category: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallingAttempt {
    pub delivery_id: Uuid,
    pub attempt_number: i32,
    pub outcome: String,
    #[serde(default)]
    pub http_status: Option<i32>,
    #[serde(default)]
    pub failure_category: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// Parses every record of a page (any malformed record refuses the page).
pub fn records<T: serde::de::DeserializeOwned>(page: &ExportPage) -> Result<Vec<T>, SourceError> {
    page.data
        .iter()
        .map(|v| serde_json::from_value(v.clone()).map_err(|_| SourceError::Malformed))
        .collect()
}
