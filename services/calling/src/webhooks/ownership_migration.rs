//! Part VI §53 — the controlled migration of customer webhooks to the Developer Platform
//! (Section 53 §9.1 / §19.3). Calling stays the authority until the cutover; this module
//! only EXPORTS its records (and applies a rollback), never changes the customer contract.
//!
//! ```text
//! Calling (authority)                              Developer Platform (importer)
//!   GET  /internal/v1/webhook-migration/inventory   counts, flags, unopenable secrets
//!   GET  /internal/v1/webhook-migration/digest      per-entity sha256 over canonical lines
//!   GET  /internal/v1/webhook-migration/{subscriptions|events|deliveries|attempts}
//!        keyset pages (deterministic order, opaque cursor, ids preserved)
//!   POST /internal/v1/webhook-migration/rollback/subscriptions   (webhook-migration.import)
//!   POST /internal/v1/webhook-migration/rollback/deliveries      (webhook-migration.import)
//! ```
//!
//! Exports need `webhook-migration.export`, the rollback routes `webhook-migration.import`
//! (both registry-grantable to `cfmeet.developer-platform` only).
//!
//! **Secret custody.** A signing secret leaves Calling only re-sealed under the dedicated
//! migration transfer key (`CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY`, held by the Developer
//! Platform under its own variable name): Calling opens the stored seal with its key ring
//! and seals the plaintext again with `cf_webhooks::secret_box` under the transfer key,
//! the associated data bound to the subscription id AND the migration direction
//! ([`transfer_owner`]) — no new cryptography. The plaintext exists only in memory
//! (zeroized on drop), is never logged, stored or serialized. Without a transfer key the
//! subscription export is refused (503 `CAPABILITY_UNAVAILABLE`). A secret Calling cannot
//! open (key id not configured, or the seal does not open) is exported as `UNOPENABLE`
//! with no secret material at all: the importer must mark the subscription
//! `ROTATION_REQUIRED` — a substitute secret is never invented. A keyed fingerprint
//! ([`secret_fingerprint`]: the `CF-Webhook-Signature` of a fixed probe, i.e. exactly what
//! every delivery already discloses) lets the importer prove continuity without plaintext.
//!
//! **Flags** (defaults keep today's behaviour exactly): `CALLING_WEBHOOK_MUTATIONS_FROZEN`,
//! `CALLING_WEBHOOK_DELIVERY_ENABLED`, `CALLING_WEBHOOK_AUTHORITY` ([`MigrationFlags`]).

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use cf_service_auth::InternalPermission;
use cf_webhooks::secret_box::{SealingKey, SecretBox, SecretBoxError};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{stmt, DbResultExt, WebhookResult, WebhookService};
use crate::{
    config::ConfigError, enterprise::http::QueryParams, error::CallingError,
    internal_auth::middleware::InternalCaller, state::AppState,
};

/// Associated-data purpose of a Calling → Developer Platform transfer seal.
pub const EXPORT_PURPOSE: &str = "webhook-migration|calling-to-developer-platform";
/// Associated-data purpose of a Developer Platform → Calling (rollback) transfer seal.
pub const ROLLBACK_PURPOSE: &str = "webhook-migration|developer-platform-to-calling";
/// Version tag of the canonical digest lines (documented in the runbook).
pub const DIGEST_ALGORITHM: &str = "sha256-canonical-lines-v1";
/// Failure category of a delivery settled because the Developer Platform delivered it.
pub const SETTLED_CATEGORY: &str = "DELIVERED_BY_DEVELOPER_PLATFORM";
/// Header naming why a customer webhook route is refused during the migration.
pub const MIGRATION_HEADER: &str = "cf-webhook-migration";

/// Largest page and the byte budget of one page (the internal client reads 1 MiB at most).
const MAX_PAGE: u64 = 500;
const DEFAULT_PAGE: u64 = 100;
const PAGE_BYTE_BUDGET: usize = 768 * 1024;
/// Largest rollback batch.
const MAX_ROLLBACK_BATCH: usize = 500;

/// `CALLING_WEBHOOK_AUTHORITY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebhookAuthority {
    /// Calling owns customer webhooks (default).
    #[default]
    Active,
    /// The Developer Platform owns them: Calling's customer routes answer "moved", its
    /// delivery worker is off, every record is retained.
    Retired,
}

impl WebhookAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            WebhookAuthority::Active => "active",
            WebhookAuthority::Retired => "retired",
        }
    }
}

/// The migration flags of [`super::WebhookSettings`].
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `CALLING_WEBHOOK_MUTATIONS_FROZEN` | `false` | `true`: customer create / update / delete / rotate-secret / replay answer 503 `SERVICE_UNAVAILABLE` (`CF-Webhook-Migration: frozen`, `Retry-After`); reads and deliveries continue |
/// | `CALLING_WEBHOOK_DELIVERY_ENABLED` | `true` | `false`: the delivery worker sends nothing; deliveries stay recorded |
/// | `CALLING_WEBHOOK_AUTHORITY` | `active` | `retired`: every customer webhook route answers 503 `SERVICE_UNAVAILABLE` (`CF-Webhook-Migration: moved`), delivery off |
/// | `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY` (`_FILE`) | unset | the migration transfer key (base64, 32 bytes, not weak); unset = subscription export refused |
/// | `CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID` | with the key | its key id (`[A-Za-z0-9_-]`, 1–32) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationFlags {
    pub mutations_frozen: bool,
    pub delivery_enabled: bool,
    pub authority: WebhookAuthority,
}

fn bool_flag(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<bool>, ConfigError> {
    match get(var)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .as_deref()
    {
        None => Ok(None),
        Some("true") | Some("1") => Ok(Some(true)),
        Some("false") | Some("0") => Ok(Some(false)),
        Some(_) => Err(ConfigError::invalid(
            var,
            "must be true or false".to_string(),
        )),
    }
}

impl MigrationFlags {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mutations_frozen = bool_flag(get, "CALLING_WEBHOOK_MUTATIONS_FROZEN")?.unwrap_or(false);
        let delivery = bool_flag(get, "CALLING_WEBHOOK_DELIVERY_ENABLED")?;
        let authority = match get("CALLING_WEBHOOK_AUTHORITY")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .as_deref()
        {
            None | Some("active") => WebhookAuthority::Active,
            Some("retired") => WebhookAuthority::Retired,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "CALLING_WEBHOOK_AUTHORITY",
                    "must be active or retired".to_string(),
                ))
            }
        };
        if authority == WebhookAuthority::Retired && delivery == Some(true) {
            return Err(ConfigError::invalid(
                "CALLING_WEBHOOK_DELIVERY_ENABLED",
                "cannot be true while CALLING_WEBHOOK_AUTHORITY=retired".to_string(),
            ));
        }
        Ok(Self {
            mutations_frozen,
            delivery_enabled: delivery.unwrap_or(authority == WebhookAuthority::Active),
            authority,
        })
    }
}

/// The migration transfer key (a one-key `SecretBox`), or `None` when unset.
pub fn transfer_key_from_lookup(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<SecretBox>, ConfigError> {
    use secrecy::ExposeSecret;
    const KEY: &str = "CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY";
    const KEY_FILE: &str = "CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_FILE";
    const KEY_ID: &str = "CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID";
    let Some(raw) = crate::config::secret::read_secret(get, KEY, KEY_FILE)? else {
        return Ok(None);
    };
    let kid = get(KEY_ID)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::Missing { var: KEY_ID })?;
    let key = SealingKey::from_base64(&kid, raw.expose_secret()).map_err(|e| {
        ConfigError::invalid(
            match e {
                SecretBoxError::KeyId => KEY_ID,
                _ => KEY,
            },
            match e {
                SecretBoxError::WeakKey => {
                    "is a weak key (repeated or low-entropy bytes); generate 32 random bytes"
                }
                SecretBoxError::KeyId => "has an invalid key id ([A-Za-z0-9_-], 1-32)",
                _ => "must be 32 bytes, base64",
            }
            .to_string(),
        )
    })?;
    SecretBox::new(key, None)
        .map(Some)
        .map_err(|_| ConfigError::invalid(KEY, "is invalid".to_string()))
}

/// The associated-data owner of a transfer seal: the subscription id AND the direction.
pub fn transfer_owner(purpose: &str, subscription: Uuid) -> String {
    format!("{purpose}|{subscription}")
}

/// A keyed fingerprint of a signing secret: the `CF-Webhook-Signature` of a fixed probe
/// (timestamp 0, delivery id `webhook-migration-fingerprint|<id>`, body `cfmeet`). It is
/// what any delivery already discloses and lets the importer prove the secret it holds is
/// the original one, without the plaintext ever leaving either service.
pub fn secret_fingerprint(secret: &str, subscription: Uuid) -> String {
    cf_webhooks::signing::sign(
        secret.as_bytes(),
        0,
        &format!("webhook-migration-fingerprint|{subscription}"),
        b"cfmeet",
    )
}

/// RFC 3339 with microseconds (PostgreSQL precision: an export round-trips exactly).
pub fn exact(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn exact_opt(at: Option<DateTime<Utc>>) -> Value {
    at.map(|t| Value::String(exact(t))).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// Cursors (opaque base64url of the keyset position).
// ---------------------------------------------------------------------------

fn encode_cursor(text: String) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text)
}

fn decode_cursor(cursor: &str) -> WebhookResult<String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or(CallingError::Validation("cursor is invalid."))
}

fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

fn from_micros(v: &str) -> Option<DateTime<Utc>> {
    v.parse::<i64>()
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_micros)
}

/// One export page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPage {
    pub data: Vec<Value>,
    pub next_cursor: Option<String>,
}

/// Keeps at most `limit` rows within the byte budget (at least one); the next cursor
/// names the last kept row when rows remain.
fn paged<R>(
    rows: Vec<R>,
    limit: u64,
    view: impl Fn(&R) -> Value,
    key: impl Fn(&R) -> String,
) -> ExportPage {
    let total = rows.len();
    let mut data = Vec::new();
    let mut bytes = 0usize;
    let mut last_key = None;
    for r in rows.iter().take(limit as usize) {
        let v = view(r);
        let size = v.to_string().len();
        if !data.is_empty() && bytes + size > PAGE_BYTE_BUDGET {
            break;
        }
        bytes += size;
        data.push(v);
        last_key = Some(key(r));
    }
    let next_cursor = (data.len() < total)
        .then_some(last_key)
        .flatten()
        .map(encode_cursor);
    ExportPage { data, next_cursor }
}

// ---------------------------------------------------------------------------
// Rows.
// ---------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct SubscriptionExportRow {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    environment_kind: Option<String>,
    endpoint_url: String,
    description: Option<String>,
    status: String,
    event_types: Vec<String>,
    secret_sealed: String,
    secret_version: i32,
    created_by_credential_id: Option<Uuid>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct EventExportRow {
    id: Uuid,
    event_id: String,
    source_event_id: String,
    producer: String,
    event_type: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    occurred_at: DateTime<Utc>,
    body: Value,
    body_sha256: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct DeliveryExportRow {
    id: Uuid,
    subscription_id: Uuid,
    event_id: String,
    producer: String,
    source_event_id: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    replay_of: Option<Uuid>,
    status: String,
    attempt_count: i32,
    next_attempt_at: DateTime<Utc>,
    lease_until: Option<DateTime<Utc>>,
    last_http_status: Option<i32>,
    failure_category: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct AttemptExportRow {
    delivery_id: Uuid,
    attempt_number: i32,
    outcome: String,
    http_status: Option<i32>,
    failure_category: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}

const SUBSCRIPTION_EXPORT_SQL: &str =
    "SELECT s.id, s.tenant_id, s.application_id, s.environment_id, \
       pe.kind AS environment_kind, s.endpoint_url, s.description, s.status, s.event_types, \
       s.secret_sealed, s.secret_version, s.created_by_credential_id, s.version, s.created_at, \
       s.updated_at, s.deleted_at \
  FROM webhook_subscriptions s LEFT JOIN platform_environments pe ON pe.id = s.environment_id";

const EVENT_EXPORT_SQL: &str =
    "SELECT e.id, e.event_id, e.source_event_id, e.producer, e.event_type, \
       e.tenant_id, e.application_id, e.environment_id, e.occurred_at, e.body, \
       encode(sha256(convert_to(e.body::text, 'UTF8')), 'hex') AS body_sha256, e.created_at \
  FROM webhook_events e";

const DELIVERY_EXPORT_SQL: &str = "SELECT d.id, d.subscription_id, e.event_id, e.producer, \
       e.source_event_id, d.tenant_id, d.application_id, d.environment_id, d.replay_of, d.status, \
       d.attempt_count, d.next_attempt_at, d.lease_until, d.last_http_status, d.failure_category, \
       d.created_at, d.updated_at, d.completed_at \
  FROM webhook_deliveries d JOIN webhook_events e ON e.id = d.webhook_event_id";

const ATTEMPT_EXPORT_SQL: &str = "SELECT delivery_id, attempt_number, outcome, http_status, \
       failure_category, started_at, finished_at FROM webhook_delivery_attempts";

/// Canonical digest line of a subscription (the Developer Platform computes the same).
fn subscription_line(r: &SubscriptionExportRow) -> String {
    let mut types = r.event_types.clone();
    types.sort();
    json!([
        r.id,
        r.tenant_id,
        r.application_id,
        r.environment_id,
        r.endpoint_url,
        r.description,
        types,
        r.status,
        r.secret_version,
        r.version,
        exact(r.created_at),
        exact(r.updated_at),
        exact_opt(r.deleted_at),
    ])
    .to_string()
}

fn event_line(r: &EventExportRow) -> String {
    json!([
        r.event_id,
        r.producer,
        r.source_event_id,
        r.event_type,
        r.tenant_id,
        r.application_id,
        r.environment_id,
        exact(r.occurred_at),
        exact(r.created_at),
        r.body_sha256,
    ])
    .to_string()
}

fn delivery_line(r: &DeliveryExportRow) -> String {
    json!([
        r.id,
        r.subscription_id,
        r.event_id,
        r.replay_of,
        r.status,
        r.attempt_count,
        exact(r.next_attempt_at),
        r.last_http_status,
        r.failure_category,
        exact(r.created_at),
        exact_opt(r.completed_at),
    ])
    .to_string()
}

fn attempt_line(r: &AttemptExportRow) -> String {
    json!([
        r.delivery_id,
        r.attempt_number,
        r.outcome,
        r.http_status,
        r.failure_category,
        exact(r.started_at),
        exact(r.finished_at),
    ])
    .to_string()
}

/// A running digest: count and sha256 over `line + "\n"`.
struct Running {
    count: i64,
    hasher: Sha256,
}

impl Running {
    fn new() -> Self {
        Self {
            count: 0,
            hasher: Sha256::new(),
        }
    }

    fn add(&mut self, line: &str) {
        self.count += 1;
        self.hasher.update(line.as_bytes());
        self.hasher.update(b"\n");
    }

    fn finish(self) -> Value {
        json!({ "count": self.count, "digest": hex::encode(self.hasher.finalize()) })
    }
}

/// The plaintext of a secret opened for re-sealing: zeroized on drop, never printed.
struct Plain(Zeroizing<String>);

impl Plain {
    fn open(ring: &SecretBox, sealed: &str, owner: &str) -> Result<Self, SecretBoxError> {
        ring.open(sealed, owner).map(|s| Plain(Zeroizing::new(s)))
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Why a stored secret cannot be exported.
fn unopenable_reason(e: SecretBoxError) -> &'static str {
    match e {
        SecretBoxError::UnknownKey => "SIGNING_KEY_UNKNOWN",
        _ => "SEAL_UNOPENABLE",
    }
}

impl WebhookService {
    /// The migration refusal of a customer webhook route, if any: `retired` refuses every
    /// route ("moved"), the freeze refuses mutations. 503 `SERVICE_UNAVAILABLE` with
    /// `CF-Webhook-Migration` (and `Retry-After` while frozen).
    pub fn migration_refusal(&self, mutation: bool) -> Option<Response> {
        let (marker, retry) = if self.settings.authority == WebhookAuthority::Retired {
            ("moved", false)
        } else if mutation && self.settings.mutations_frozen {
            ("frozen", true)
        } else {
            return None;
        };
        let mut response = CallingError::ServiceUnavailable.into_response();
        let headers = response.headers_mut();
        headers.insert(MIGRATION_HEADER, HeaderValue::from_static(marker));
        if retry {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("300"));
        }
        Some(response)
    }

    fn transfer_key(&self) -> WebhookResult<&SecretBox> {
        self.settings
            .transfer_key
            .as_deref()
            .ok_or(CallingError::Call {
                code: crate::error::ErrorCode::CapabilityUnavailable,
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            })
    }

    async fn count_by<C: ConnectionTrait>(
        db: &C,
        sql: &str,
        values: Vec<sea_orm::Value>,
    ) -> WebhookResult<Map<String, Value>> {
        let rows = db.query_all(stmt(sql, values)).await.db()?;
        let mut out = Map::new();
        for r in rows {
            let k: String = r.try_get_by_index(0).db()?;
            let n: i64 = r.try_get_by_index(1).db()?;
            out.insert(k, Value::from(n));
        }
        Ok(out)
    }

    async fn scalar(&self, sql: &str, values: Vec<sea_orm::Value>) -> WebhookResult<i64> {
        self.db()
            .query_one(stmt(sql, values))
            .await
            .db()?
            .ok_or(CallingError::Internal)?
            .try_get_by_index::<i64>(0)
            .db()
    }

    /// Counts per entity and status, the migration flags and the number of secrets the
    /// key ring cannot open. Never a secret, a seal or an endpoint URL.
    pub async fn migration_inventory(&self) -> WebhookResult<Value> {
        let now = self.now();
        let db = self.db();
        let subscriptions = Self::count_by(
            db,
            "SELECT status, count(*) FROM webhook_subscriptions GROUP BY status ORDER BY status",
            vec![],
        )
        .await?;
        let (mut unopenable, mut unopenable_deleted) = (0i64, 0i64);
        let ring = self.key_ring();
        if let Some(ring) = ring {
            let mut after: Option<Uuid> = None;
            loop {
                let rows = db
                    .query_all(stmt(
                        "SELECT id, secret_sealed, status FROM webhook_subscriptions \
                          WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 500",
                        vec![after.into()],
                    ))
                    .await
                    .db()?;
                if rows.is_empty() {
                    break;
                }
                for r in &rows {
                    let id: Uuid = r.try_get_by_index(0).db()?;
                    let sealed: String = r.try_get_by_index(1).db()?;
                    let status: String = r.try_get_by_index(2).db()?;
                    if Plain::open(ring, &sealed, &id.to_string()).is_err() {
                        if status == "DELETED" {
                            unopenable_deleted += 1;
                        } else {
                            unopenable += 1;
                        }
                    }
                    after = Some(id);
                }
            }
        }
        let events = Self::count_by(
            db,
            "SELECT producer, count(*) FROM webhook_events GROUP BY producer ORDER BY producer",
            vec![],
        )
        .await?;
        let deliveries = Self::count_by(
            db,
            "SELECT status, count(*) FROM webhook_deliveries GROUP BY status ORDER BY status",
            vec![],
        )
        .await?;
        let in_flight = self
            .scalar(
                "SELECT count(*) FROM webhook_deliveries WHERE status = 'DELIVERING' AND lease_until > $1",
                vec![now.into()],
            )
            .await?;
        let attempts = self
            .scalar("SELECT count(*) FROM webhook_delivery_attempts", vec![])
            .await?;
        let sum = |m: &Map<String, Value>| m.values().filter_map(Value::as_i64).sum::<i64>();
        let open: i64 = ["PENDING", "RETRY_SCHEDULED", "DELIVERING"]
            .iter()
            .filter_map(|s| deliveries.get(*s).and_then(Value::as_i64))
            .sum();
        Ok(json!({
            "generatedAt": exact(now),
            "flags": {
                "mutationsFrozen": self.settings.mutations_frozen,
                "deliveryEnabled": self.settings.delivers(),
                "authority": self.settings.authority.as_str(),
                "signingKeyRingConfigured": ring.is_some(),
                "transferKeyId": self.settings.transfer_key.as_deref().map(|k| k.current_kid()),
            },
            "subscriptions": {
                "total": sum(&subscriptions),
                "byStatus": subscriptions,
                "unopenableSecrets": ring.map(|_| unopenable),
                "unopenableDeletedSecrets": ring.map(|_| unopenable_deleted),
            },
            "events": { "total": sum(&events), "byProducer": events },
            "deliveries": {
                "total": sum(&deliveries),
                "byStatus": deliveries,
                "open": open,
                "inFlight": in_flight,
            },
            "attempts": { "total": attempts },
        }))
    }

    /// Per-entity count and sha256 over the canonical lines ([`DIGEST_ALGORITHM`]), in id
    /// order. The Developer Platform computes the same over its migrated rows.
    pub async fn migration_digest(&self) -> WebhookResult<Value> {
        let db = self.db();
        let mut subs = Running::new();
        let mut after: Option<Uuid> = None;
        loop {
            let rows = SubscriptionExportRow::find_by_statement(stmt(
                &format!(
                    "{SUBSCRIPTION_EXPORT_SQL} WHERE ($1::uuid IS NULL OR s.id > $1) ORDER BY s.id LIMIT 1000"
                ),
                vec![after.into()],
            ))
            .all(db)
            .await
            .db()?;
            let Some(last) = rows.last() else { break };
            after = Some(last.id);
            rows.iter().for_each(|r| subs.add(&subscription_line(r)));
        }
        let mut events = Running::new();
        let mut after: Option<String> = None;
        loop {
            let rows = EventExportRow::find_by_statement(stmt(
                &format!(
                    "{EVENT_EXPORT_SQL} WHERE ($1::text IS NULL OR e.event_id COLLATE \"C\" > $1) \
                     ORDER BY e.event_id COLLATE \"C\" LIMIT 1000"
                ),
                vec![after.clone().into()],
            ))
            .all(db)
            .await
            .db()?;
            let Some(last) = rows.last() else { break };
            after = Some(last.event_id.clone());
            rows.iter().for_each(|r| events.add(&event_line(r)));
        }
        let mut deliveries = Running::new();
        let mut after: Option<Uuid> = None;
        loop {
            let rows = DeliveryExportRow::find_by_statement(stmt(
                &format!(
                    "{DELIVERY_EXPORT_SQL} WHERE ($1::uuid IS NULL OR d.id > $1) ORDER BY d.id LIMIT 1000"
                ),
                vec![after.into()],
            ))
            .all(db)
            .await
            .db()?;
            let Some(last) = rows.last() else { break };
            after = Some(last.id);
            rows.iter().for_each(|r| deliveries.add(&delivery_line(r)));
        }
        let mut attempts = Running::new();
        let mut after: Option<(Uuid, i32)> = None;
        loop {
            let (d, n) = after.map(|(d, n)| (Some(d), n)).unwrap_or((None, 0));
            let rows = AttemptExportRow::find_by_statement(stmt(
                &format!(
                    "{ATTEMPT_EXPORT_SQL} WHERE ($1::uuid IS NULL OR (delivery_id, attempt_number) > ($1, $2)) \
                     ORDER BY delivery_id, attempt_number LIMIT 1000"
                ),
                vec![d.into(), n.into()],
            ))
            .all(db)
            .await
            .db()?;
            let Some(last) = rows.last() else { break };
            after = Some((last.delivery_id, last.attempt_number));
            rows.iter().for_each(|r| attempts.add(&attempt_line(r)));
        }
        Ok(json!({
            "algorithm": DIGEST_ALGORITHM,
            "subscriptions": subs.finish(),
            "events": events.finish(),
            "deliveries": deliveries.finish(),
            "attempts": attempts.finish(),
        }))
    }

    /// One page of subscriptions (id order) with each secret ONLY re-sealed under the
    /// transfer key — or `UNOPENABLE` without any secret material. Refused (503) without
    /// a transfer key or a signing key ring.
    pub async fn export_subscriptions(
        &self,
        cursor: Option<&str>,
        limit: u64,
    ) -> WebhookResult<ExportPage> {
        let transfer = self.transfer_key()?;
        let ring = self.secret_box()?;
        let after = cursor
            .map(|c| {
                decode_cursor(c)?
                    .parse::<Uuid>()
                    .map_err(|_| CallingError::Validation("cursor is invalid."))
            })
            .transpose()?;
        let rows = SubscriptionExportRow::find_by_statement(stmt(
            &format!(
                "{SUBSCRIPTION_EXPORT_SQL} WHERE ($1::uuid IS NULL OR s.id > $1) ORDER BY s.id LIMIT $2"
            ),
            vec![after.into(), (limit as i64 + 1).into()],
        ))
        .all(self.db())
        .await
        .db()?;
        let page = paged(
            rows,
            limit,
            |r| {
                let secret = match Plain::open(ring, &r.secret_sealed, &r.id.to_string()) {
                    Ok(plain) => {
                        let sealed =
                            transfer.seal(plain.as_str(), &transfer_owner(EXPORT_PURPOSE, r.id));
                        json!({
                            "state": "TRANSFER_SEALED",
                            "transferKeyId": transfer.current_kid(),
                            "transferSealed": sealed,
                            "fingerprint": secret_fingerprint(plain.as_str(), r.id),
                            "unopenableReason": null,
                        })
                    }
                    Err(e) => json!({
                        "state": "UNOPENABLE",
                        "transferKeyId": null,
                        "transferSealed": null,
                        "fingerprint": null,
                        "unopenableReason": unopenable_reason(e),
                    }),
                };
                json!({
                    "webhookId": r.id,
                    "tenantId": r.tenant_id,
                    "applicationId": r.application_id,
                    "environmentId": r.environment_id,
                    "environmentKind": r.environment_kind,
                    "url": r.endpoint_url,
                    "description": r.description,
                    "eventTypes": r.event_types,
                    "status": r.status,
                    "secretVersion": r.secret_version,
                    "version": r.version,
                    "createdByCredentialId": r.created_by_credential_id,
                    "createdAt": exact(r.created_at),
                    "updatedAt": exact(r.updated_at),
                    "deletedAt": exact_opt(r.deleted_at),
                    "secret": secret,
                })
            },
            |r| r.id.to_string(),
        );
        tracing::info!(
            exported = page.data.len(),
            more = page.next_cursor.is_some(),
            "webhook migration: subscriptions page exported (secrets transfer-sealed only)"
        );
        Ok(page)
    }

    /// One page of public events in (creation time, id) order, ids preserved.
    pub async fn export_events(
        &self,
        cursor: Option<&str>,
        limit: u64,
    ) -> WebhookResult<ExportPage> {
        let (after_at, after_id) = match cursor {
            None => (None, None),
            Some(c) => {
                let text = decode_cursor(c)?;
                let (at, id) = text
                    .split_once(':')
                    .and_then(|(a, i)| Some((from_micros(a)?, i.parse::<Uuid>().ok()?)))
                    .ok_or(CallingError::Validation("cursor is invalid."))?;
                (Some(at), Some(id))
            }
        };
        let rows = EventExportRow::find_by_statement(stmt(
            &format!(
                "{EVENT_EXPORT_SQL} WHERE ($1::timestamptz IS NULL OR (e.created_at, e.id) > ($1, $2)) \
                 ORDER BY e.created_at, e.id LIMIT $3"
            ),
            vec![after_at.into(), after_id.into(), (limit as i64 + 1).into()],
        ))
        .all(self.db())
        .await
        .db()?;
        Ok(paged(
            rows,
            limit,
            |r| {
                json!({
                    "eventId": r.event_id,
                    "producer": r.producer,
                    "sourceEventId": r.source_event_id,
                    "eventType": r.event_type,
                    "tenantId": r.tenant_id,
                    "applicationId": r.application_id,
                    "environmentId": r.environment_id,
                    "occurredAt": exact(r.occurred_at),
                    "createdAt": exact(r.created_at),
                    "body": r.body,
                    "bodySha256": r.body_sha256,
                })
            },
            |r| format!("{}:{}", micros(r.created_at), r.id),
        ))
    }

    /// One page of deliveries in (creation time, replay last, id) order (an original
    /// precedes its replays), ids, `replay_of` and status preserved. `open_only`: the
    /// deliveries still to be sent (rollback settlement).
    pub async fn export_deliveries(
        &self,
        cursor: Option<&str>,
        limit: u64,
        open_only: bool,
    ) -> WebhookResult<ExportPage> {
        let (after_at, after_flag, after_id) = match cursor {
            None => (None, 0i32, None),
            Some(c) => {
                let text = decode_cursor(c)?;
                let mut parts = text.splitn(3, ':');
                let at = parts.next().and_then(from_micros);
                let flag = parts.next().and_then(|f| f.parse::<i32>().ok());
                let id = parts.next().and_then(|i| i.parse::<Uuid>().ok());
                match (at, flag, id) {
                    (Some(at), Some(flag), Some(id)) => (Some(at), flag, Some(id)),
                    _ => return Err(CallingError::Validation("cursor is invalid.")),
                }
            }
        };
        let rows = DeliveryExportRow::find_by_statement(stmt(
            &format!(
                "{DELIVERY_EXPORT_SQL} WHERE ($1::timestamptz IS NULL \
                     OR (d.created_at, (d.replay_of IS NOT NULL)::int, d.id) > ($1, $2, $3)) \
                   AND (NOT $4 OR d.status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING')) \
                 ORDER BY d.created_at, (d.replay_of IS NOT NULL)::int, d.id LIMIT $5"
            ),
            vec![
                after_at.into(),
                after_flag.into(),
                after_id.into(),
                open_only.into(),
                (limit as i64 + 1).into(),
            ],
        ))
        .all(self.db())
        .await
        .db()?;
        Ok(paged(
            rows,
            limit,
            |r| {
                json!({
                    "deliveryId": r.id,
                    "webhookId": r.subscription_id,
                    "eventId": r.event_id,
                    "producer": r.producer,
                    "sourceEventId": r.source_event_id,
                    "tenantId": r.tenant_id,
                    "applicationId": r.application_id,
                    "environmentId": r.environment_id,
                    "replayOf": r.replay_of,
                    "status": r.status,
                    "attemptCount": r.attempt_count,
                    "nextAttemptAt": exact(r.next_attempt_at),
                    "leaseUntil": exact_opt(r.lease_until),
                    "lastHttpStatus": r.last_http_status,
                    "failureCategory": r.failure_category,
                    "createdAt": exact(r.created_at),
                    "updatedAt": exact(r.updated_at),
                    "completedAt": exact_opt(r.completed_at),
                })
            },
            |r| {
                format!(
                    "{}:{}:{}",
                    micros(r.created_at),
                    r.replay_of.is_some() as i32,
                    r.id
                )
            },
        ))
    }

    /// One page of delivery attempts in (delivery, attempt number) order.
    pub async fn export_attempts(
        &self,
        cursor: Option<&str>,
        limit: u64,
    ) -> WebhookResult<ExportPage> {
        let (after_d, after_n) = match cursor {
            None => (None, 0),
            Some(c) => {
                let text = decode_cursor(c)?;
                let (d, n) = text
                    .split_once(':')
                    .and_then(|(d, n)| Some((d.parse::<Uuid>().ok()?, n.parse::<i32>().ok()?)))
                    .ok_or(CallingError::Validation("cursor is invalid."))?;
                (Some(d), n)
            }
        };
        let rows = AttemptExportRow::find_by_statement(stmt(
            &format!(
                "{ATTEMPT_EXPORT_SQL} WHERE ($1::uuid IS NULL OR (delivery_id, attempt_number) > ($1, $2)) \
                 ORDER BY delivery_id, attempt_number LIMIT $3"
            ),
            vec![after_d.into(), after_n.into(), (limit as i64 + 1).into()],
        ))
        .all(self.db())
        .await
        .db()?;
        Ok(paged(
            rows,
            limit,
            |r| {
                json!({
                    "deliveryId": r.delivery_id,
                    "attemptNumber": r.attempt_number,
                    "outcome": r.outcome,
                    "httpStatus": r.http_status,
                    "failureCategory": r.failure_category,
                    "startedAt": exact(r.started_at),
                    "finishedAt": exact(r.finished_at),
                })
            },
            |r| format!("{}:{}", r.delivery_id, r.attempt_number),
        ))
    }

    /// Applies the Developer Platform's rollback export: each subscription is inserted
    /// (a subscription created there after the cutover) or updated when its version is
    /// newer than Calling's; a newer secret arrives only transfer-sealed and is re-sealed
    /// under Calling's key ring. Every item is its own transaction; refusals are reported
    /// per item with a bounded reason.
    pub async fn apply_rollback_subscriptions(
        &self,
        items: Vec<RollbackSubscription>,
    ) -> WebhookResult<Value> {
        let ring = self.secret_box()?;
        let transfer = self.transfer_key()?;
        if items.len() > MAX_ROLLBACK_BATCH {
            return Err(CallingError::Validation(
                "too many subscriptions in one batch.",
            ));
        }
        let (mut applied, mut unchanged) = (0u64, 0u64);
        let mut conflicts = Vec::new();
        for item in items {
            match self.apply_one(ring, transfer, &item).await {
                Ok(true) => applied += 1,
                Ok(false) => unchanged += 1,
                Err(reason) => {
                    conflicts.push(json!({ "webhookId": item.webhook_id, "reason": reason }))
                }
            }
        }
        tracing::info!(
            applied,
            unchanged,
            conflicts = conflicts.len(),
            "webhook migration: rollback subscriptions applied"
        );
        Ok(json!({ "applied": applied, "unchanged": unchanged, "conflicts": conflicts }))
    }

    async fn apply_one(
        &self,
        ring: &SecretBox,
        transfer: &SecretBox,
        item: &RollbackSubscription,
    ) -> Result<bool, &'static str> {
        if !matches!(item.status.as_str(), "ACTIVE" | "DISABLED" | "DELETED")
            || (item.status == "DELETED") != item.deleted_at.is_some()
            || item.event_types.is_empty()
            || item.event_types.len() > 64
            || item.version < 1
            || item.secret_version < 1
        {
            return Err("INVALID");
        }
        if item
            .event_types
            .iter()
            .any(|t| super::catalog::definition(t).is_none())
        {
            return Err("EVENT_TYPE_UNSUPPORTED");
        }
        let mut types = item.event_types.clone();
        types.sort();
        types.dedup();
        let resealed = match &item.secret {
            None => None,
            Some(s) => {
                let plain = Plain::open(
                    transfer,
                    &s.transfer_sealed,
                    &transfer_owner(ROLLBACK_PURPOSE, item.webhook_id),
                )
                .map_err(|_| "TRANSFER_SEAL_UNOPENABLE")?;
                Some(ring.seal(plain.as_str(), &item.webhook_id.to_string()))
            }
        };
        let db_err = |_| "STORE_REFUSED";
        let txn = self.db().begin().await.map_err(db_err)?;
        let existing = txn
            .query_one(stmt(
                "SELECT tenant_id, application_id, environment_id, status, version, secret_version \
                   FROM webhook_subscriptions WHERE id = $1 FOR UPDATE",
                vec![item.webhook_id.into()],
            ))
            .await
            .map_err(db_err)?;
        let outcome = match existing {
            None => {
                let Some(sealed) = resealed else {
                    return Err("SECRET_REQUIRED");
                };
                txn.execute(stmt(
                    "INSERT INTO webhook_subscriptions (id, tenant_id, application_id, environment_id, \
                       endpoint_url, description, status, event_types, secret_sealed, secret_version, \
                       created_by_credential_id, version, created_at, updated_at, deleted_at, secret_key_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, $11, $12, $13, $14, $15)",
                    vec![
                        item.webhook_id.into(),
                        item.tenant_id.into(),
                        item.application_id.into(),
                        item.environment_id.into(),
                        item.url.clone().into(),
                        item.description.clone().into(),
                        item.status.clone().into(),
                        types.into(),
                        sealed.into(),
                        item.secret_version.into(),
                        item.version.into(),
                        item.created_at.into(),
                        item.updated_at.into(),
                        item.deleted_at.into(),
                        ring.current_kid().into(),
                    ],
                ))
                .await
                .map_err(db_err)?;
                true
            }
            Some(row) => {
                let get_uuid = |i| row.try_get_by_index::<Uuid>(i).map_err(db_err);
                if (get_uuid(0)?, get_uuid(1)?, get_uuid(2)?)
                    != (item.tenant_id, item.application_id, item.environment_id)
                {
                    return Err("SCOPE_MISMATCH");
                }
                let status: String = row.try_get_by_index(3).map_err(db_err)?;
                let version: i64 = row.try_get_by_index(4).map_err(db_err)?;
                let secret_version: i32 = row.try_get_by_index(5).map_err(db_err)?;
                if item.version <= version {
                    false
                } else if status == "DELETED" {
                    return Err("DELETED_IN_CALLING");
                } else {
                    let new_secret = resealed.filter(|_| item.secret_version > secret_version);
                    txn.execute(stmt(
                        "UPDATE webhook_subscriptions SET endpoint_url = $2, description = $3, \
                           event_types = $4, status = $5, version = $6, updated_at = $7, deleted_at = $8, \
                           secret_sealed = COALESCE($9, secret_sealed), \
                           secret_key_id = CASE WHEN $9::text IS NULL THEN secret_key_id ELSE $10 END, \
                           secret_version = CASE WHEN $9::text IS NULL THEN secret_version ELSE $11 END \
                         WHERE id = $1",
                        vec![
                            item.webhook_id.into(),
                            item.url.clone().into(),
                            item.description.clone().into(),
                            types.into(),
                            item.status.clone().into(),
                            item.version.into(),
                            item.updated_at.into(),
                            item.deleted_at.into(),
                            new_secret.into(),
                            ring.current_kid().into(),
                            item.secret_version.into(),
                        ],
                    ))
                    .await
                    .map_err(db_err)?;
                    if item.status == "DELETED" {
                        txn.execute(stmt(
                            "UPDATE webhook_deliveries SET status = 'CANCELLED', completed_at = $2, \
                               lease_until = NULL, updated_at = $2 \
                             WHERE subscription_id = $1 AND status IN ('PENDING', 'RETRY_SCHEDULED')",
                            vec![item.webhook_id.into(), self.now().into()],
                        ))
                        .await
                        .map_err(db_err)?;
                    }
                    true
                }
            }
        };
        txn.commit().await.map_err(db_err)?;
        Ok(outcome)
    }

    /// Settles deliveries the Developer Platform already delivered (rollback): each named
    /// delivery still open here (pending, scheduled for retry, or claimed by a worker whose
    /// lease expired) is CANCELLED with [`SETTLED_CATEGORY`] — never sent twice.
    pub async fn settle_rollback_deliveries(&self, ids: Vec<Uuid>) -> WebhookResult<Value> {
        if ids.len() > MAX_ROLLBACK_BATCH {
            return Err(CallingError::Validation(
                "too many deliveries in one batch.",
            ));
        }
        let now = self.now();
        let settled = self
            .db()
            .execute(stmt(
                "UPDATE webhook_deliveries SET status = 'CANCELLED', completed_at = $2, \
                   lease_until = NULL, failure_category = $3, updated_at = $2 \
                 WHERE id = ANY($1) AND (status IN ('PENDING', 'RETRY_SCHEDULED') \
                       OR (status = 'DELIVERING' AND lease_until <= $2))",
                vec![ids.clone().into(), now.into(), SETTLED_CATEGORY.into()],
            ))
            .await
            .db()?
            .rows_affected();
        tracing::info!(
            requested = ids.len(),
            settled,
            "webhook migration: deliveries settled for rollback"
        );
        Ok(json!({ "requested": ids.len(), "settled": settled }))
    }
}

/// One subscription of the Developer Platform's rollback export.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackSubscription {
    pub webhook_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
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
    /// The signing secret, transfer-sealed for [`ROLLBACK_PURPOSE`] (absent: unchanged
    /// or not transferable).
    #[serde(default)]
    pub secret: Option<RollbackSecret>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RollbackSecret {
    pub transfer_key_id: String,
    pub transfer_sealed: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollbackSubscriptionsBody {
    subscriptions: Vec<RollbackSubscription>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollbackDeliveriesBody {
    delivery_ids: Vec<Uuid>,
}

// ---------------------------------------------------------------------------
// Internal routes (nested under /internal/v1 by `routes::app`).
// ---------------------------------------------------------------------------

/// The webhook migration routes.
pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/webhook-migration/inventory", get(inventory))
        .route("/webhook-migration/digest", get(digest))
        .route("/webhook-migration/subscriptions", get(subscriptions))
        .route("/webhook-migration/events", get(events))
        .route("/webhook-migration/deliveries", get(deliveries))
        .route("/webhook-migration/attempts", get(attempts))
        .route(
            "/webhook-migration/rollback/subscriptions",
            post(rollback_subscriptions),
        )
        .route(
            "/webhook-migration/rollback/deliveries",
            post(rollback_deliveries),
        )
}

type Q = Result<Query<Vec<(String, String)>>, QueryRejection>;

fn export_page(q: Q, extra: &[&str]) -> Result<(QueryParams, Option<String>, u64), CallingError> {
    let mut allowed = vec!["cursor", "limit"];
    allowed.extend_from_slice(extra);
    let q = QueryParams::parse(q, &allowed)?;
    let limit = match q.get("limit") {
        None => DEFAULT_PAGE,
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|l| (1..=MAX_PAGE).contains(l))
            .ok_or(CallingError::Validation(
                "limit must be an integer between 1 and 500.",
            ))?,
    };
    let cursor = q.get("cursor").map(str::to_string);
    Ok((q, cursor, limit))
}

fn no_store(value: impl Serialize) -> Response {
    let mut r = Json(value).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn exporter(caller: &InternalCaller) -> Result<(), CallingError> {
    caller.require_permission(InternalPermission::WebhookMigrationExport)
}

async fn inventory(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    Ok(no_store(state.webhooks.migration_inventory().await?))
}

async fn digest(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    Ok(no_store(state.webhooks.migration_digest().await?))
}

async fn subscriptions(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Q,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    let (_, cursor, limit) = export_page(query, &[])?;
    Ok(no_store(
        state
            .webhooks
            .export_subscriptions(cursor.as_deref(), limit)
            .await?,
    ))
}

async fn events(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Q,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    let (_, cursor, limit) = export_page(query, &[])?;
    Ok(no_store(
        state
            .webhooks
            .export_events(cursor.as_deref(), limit)
            .await?,
    ))
}

async fn deliveries(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Q,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    let (q, cursor, limit) = export_page(query, &["status"])?;
    let open_only = match q.get("status") {
        None => false,
        Some("open") => true,
        Some(_) => return Err(CallingError::Validation("status must be open.")),
    };
    Ok(no_store(
        state
            .webhooks
            .export_deliveries(cursor.as_deref(), limit, open_only)
            .await?,
    ))
}

async fn attempts(
    State(state): State<AppState>,
    caller: InternalCaller,
    query: Q,
) -> Result<Response, CallingError> {
    exporter(&caller)?;
    let (_, cursor, limit) = export_page(query, &[])?;
    Ok(no_store(
        state
            .webhooks
            .export_attempts(cursor.as_deref(), limit)
            .await?,
    ))
}

async fn rollback_subscriptions(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<RollbackSubscriptionsBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    caller.require_permission(InternalPermission::WebhookMigrationImport)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The request is invalid."))?;
    Ok(no_store(
        state
            .webhooks
            .apply_rollback_subscriptions(body.subscriptions)
            .await?,
    ))
}

async fn rollback_deliveries(
    State(state): State<AppState>,
    caller: InternalCaller,
    body: Result<Json<RollbackDeliveriesBody>, JsonRejection>,
) -> Result<Response, CallingError> {
    caller.require_permission(InternalPermission::WebhookMigrationImport)?;
    let Json(body) = body.map_err(|_| CallingError::Validation("The request is invalid."))?;
    Ok(no_store(
        state
            .webhooks
            .settle_rollback_deliveries(body.delivery_ids)
            .await?,
    ))
}

/// Shared plumbing kept for the composition root (no secret material).
pub fn transfer_key_id(settings: &super::WebhookSettings) -> Option<String> {
    settings
        .transfer_key
        .as_deref()
        .map(|k| k.current_kid().to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn b64(seed: u8) -> String {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = seed.wrapping_add((i as u8).wrapping_mul(7));
        }
        base64::engine::general_purpose::STANDARD.encode(k)
    }

    #[test]
    fn defaults_keep_todays_behaviour() {
        let f = MigrationFlags::from_lookup(&lookup(&[])).unwrap();
        assert_eq!(
            f,
            MigrationFlags {
                mutations_frozen: false,
                delivery_enabled: true,
                authority: WebhookAuthority::Active
            }
        );
        assert!(transfer_key_from_lookup(&lookup(&[])).unwrap().is_none());
        let s = super::super::WebhookSettings::default();
        assert!(s.delivers() && !s.mutations_frozen && s.transfer_key.is_none());
    }

    #[test]
    fn flags_parse_and_refuse_contradictions() {
        let f = MigrationFlags::from_lookup(&lookup(&[
            ("CALLING_WEBHOOK_MUTATIONS_FROZEN", "true"),
            ("CALLING_WEBHOOK_DELIVERY_ENABLED", "false"),
        ]))
        .unwrap();
        assert!(f.mutations_frozen && !f.delivery_enabled);
        let retired =
            MigrationFlags::from_lookup(&lookup(&[("CALLING_WEBHOOK_AUTHORITY", "retired")]))
                .unwrap();
        assert!(!retired.delivery_enabled && retired.authority == WebhookAuthority::Retired);
        for bad in [
            vec![("CALLING_WEBHOOK_AUTHORITY", "gone")],
            vec![("CALLING_WEBHOOK_MUTATIONS_FROZEN", "yes")],
            vec![
                ("CALLING_WEBHOOK_AUTHORITY", "retired"),
                ("CALLING_WEBHOOK_DELIVERY_ENABLED", "true"),
            ],
        ] {
            assert!(
                MigrationFlags::from_lookup(&lookup(&bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_transfer_key_needs_an_id_and_is_never_echoed() {
        let key = b64(3);
        let ring = transfer_key_from_lookup(&lookup(&[
            ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY", &key),
            ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID", "transfer-1"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(ring.current_kid(), "transfer-1");
        let weak = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        for pairs in [
            vec![("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY", key.as_str())],
            vec![
                ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY", weak.as_str()),
                ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID", "transfer-x9"),
            ],
            vec![
                ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY", key.as_str()),
                ("CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID", "a.b"),
            ],
        ] {
            let err = transfer_key_from_lookup(&lookup(&pairs)).expect_err("refused");
            for (_, v) in &pairs {
                assert!(!err.to_string().contains(v), "{err}");
            }
        }
    }

    #[test]
    fn transfer_seals_are_bound_to_the_subscription_and_the_direction() {
        let t = SecretBox::new(SealingKey::from_base64("t1", &b64(9)).unwrap(), None).unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = t.seal("whsec_original", &transfer_owner(EXPORT_PURPOSE, a));
        assert!(!sealed.contains("whsec_original"));
        assert_eq!(
            t.open(&sealed, &transfer_owner(EXPORT_PURPOSE, a)).unwrap(),
            "whsec_original"
        );
        assert!(t.open(&sealed, &transfer_owner(EXPORT_PURPOSE, b)).is_err());
        assert!(t
            .open(&sealed, &transfer_owner(ROLLBACK_PURPOSE, a))
            .is_err());
        assert_eq!(
            secret_fingerprint("whsec_original", a),
            secret_fingerprint("whsec_original", a)
        );
        assert_ne!(
            secret_fingerprint("whsec_original", a),
            secret_fingerprint("whsec_other", a)
        );
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let c = encode_cursor("1:2".into());
        assert_eq!(decode_cursor(&c).unwrap(), "1:2");
        assert!(decode_cursor("***").is_err());
        let rows: Vec<u8> = (0..5).collect();
        let p = paged(rows, 3, |r| json!(r), |r| r.to_string());
        assert_eq!(p.data.len(), 3);
        assert_eq!(decode_cursor(&p.next_cursor.unwrap()).unwrap(), "2");
        let p = paged(vec![1u8], 3, |r| json!(r), |r| r.to_string());
        assert!(p.next_cursor.is_none());
    }
}
