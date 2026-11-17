//! The Developer Platform's publications to the Notification Service (roadmap N7 / N11,
//! Document 4 §5.1, §9.5): the producer outbox `devplat_notification_publications` and
//! its relay to I1 (`POST /internal/v1/notification-events`, `ingestNotificationEvents`).
//!
//! Three catalogue types (#28–#30, category `DEVELOPER_UPDATES`, email + in-app at the
//! service; no sealed part):
//!
//! | Type | Enqueued by | Recipients | Keys |
//! |---|---|---|---|
//! | `developer-platform.credential.expiring-soon` | the worker's credential sweep, once per crossed lead time ([`crate::control::credential_lifecycle`]) | the organization's ACTIVE `OWNER` and `ADMIN` members | `devplat-credential:<credential>:<days>:<expiry epoch>` |
//! | `developer-platform.credential.expired` | the same sweep, once per expiry | the same | `devplat-credential:<credential>:0:<expiry epoch>` |
//! | `developer-platform.production-access.decided` | the operator decision transaction (approve / reject; the automatic policy) | the applicant (`requestedBy`, while an ACTIVE member), else the organization's ACTIVE `OWNER`s | `devplat-access:<request>:<APPROVED / REJECTED>` (today's Meeting ledger format) |
//!
//! Recipients are CF Meet account references (`cfmeet-user:<user id>`: workspace members
//! are CF Meet accounts signed in through the Meeting Service session bridge), resolved
//! when the envelope is first built, at most [`MAX_RECIPIENTS`] per event (owners first),
//! so every envelope fits the 4 KiB I1 limit. An event without any recipient is recorded
//! `SKIPPED` (`NO_RECIPIENTS`).
//!
//! * **Enqueue** ([`enqueue`]) is idempotent: the event id is derived from the fact
//!   ([`event_id`]) and the routing decision id from the event id, so a fact is published
//!   at most once whatever the number of workers, retries or restarts. Nothing is enqueued
//!   unless publication is configured (`CF_NOTIFICATIONS_BASE_URL` and the event's route,
//!   [`crate::config::NotificationRoutes`]); the production-access decision is published
//!   only on the explicit `notification-service` route, so it never doubles the
//!   operator-console email.
//! * **Relay** ([`relay_due`]): run by the worker only when `CF_NOTIFICATIONS_BASE_URL`
//!   is configured. It builds each row's envelope once with `packages/notification-client`
//!   and stores it, so a re-post is byte-identical (the service de-duplicates on
//!   (producer, eventId) plus digest). Batches of ≤ 100 rows are posted with a service
//!   token for `cfmeet.notifications` (`notifications-event.publish`). `ACCEPTED`,
//!   `DUPLICATE` and `HELD` acknowledge the row (`ACKED`); a per-event `REJECTED` or a
//!   contract refusal of the batch (4xx other than 401 / 403 / 408 / 429) dead-letters it
//!   (`DEAD`); 5xx, 401 / 403 / 408 / 429 and transport failures retry with exponential
//!   backoff (≤ 5 min) until the row expires (`EXPIRED`, never posted).
//! * **Maintenance** ([`maintain`]): expires overdue rows and purges settled rows after
//!   [`RETENTION`].

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{
    envelope::Scope, EnvelopeBuilder, Producer, RouteRef, ROUTING_PROTOCOL,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, FromQueryResult,
    TransactionTrait,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::control::{
    credential_lifecycle::{Notice, EXPIRED},
    stmt,
};

pub const CREDENTIAL_EXPIRING_SOON: &str = "developer-platform.credential.expiring-soon";
pub const CREDENTIAL_EXPIRED: &str = "developer-platform.credential.expired";
pub const PRODUCTION_ACCESS_DECIDED: &str = "developer-platform.production-access.decided";

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";

/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;

/// Most recipients of one event: every envelope with this many account references fits
/// the 4 KiB I1 envelope limit (owners first, then admins, oldest membership first).
pub const MAX_RECIPIENTS: u64 = 40;

/// How long a publication may wait for the relay (today's Meeting ledger expiry of the
/// production-access email). A credential notice never outlives the credential's expiry.
pub const LIFETIME: Duration = Duration::days(3);

/// How long a settled publication is kept after `completed_at` (diagnostics).
pub const RETENTION: Duration = Duration::days(30);

/// `route.routingVersion` of every publication: the Developer Platform has no routing
/// fence; the per-event route switch (configuration) is version 1.
pub const ROUTING_VERSION: i32 = 1;

/// The worker schedule key of [`maintain`] (`devplat_worker_schedules.job`).
pub const MAINTENANCE_JOB: &str = "notification-publications";

/// What the running process may enqueue (effective: requested route AND
/// `CF_NOTIFICATIONS_BASE_URL`). Default: nothing (today's behaviour).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Publishing {
    /// Production-access decisions (`notification-service` route only).
    pub production_access: bool,
}

// ----------------------------------------------------------------------------- enqueue

/// The deterministic envelope event id of a fact (`evt_<32 hex>`).
pub fn event_id(event_type: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    h.update(b"cfmeet.developer-platform/notification-publications/v1\n");
    h.update(event_type.as_bytes());
    for p in parts {
        h.update(b"\n");
        h.update(p.as_bytes());
    }
    format!("evt_{}", &hex::encode(h.finalize())[..32])
}

/// The routing decision id of an event (deterministic: unique per event, so the service's
/// per-producer decision uniqueness holds and a re-post carries the same decision).
pub fn decision_id(event_id: &str) -> Uuid {
    let digest = Sha256::digest(format!("cfmeet.developer-platform/route/v1\n{event_id}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn correlation(value: &str) -> String {
    CorrelationId::parse(value)
        .map(|c| c.as_str().to_owned())
        .unwrap_or_else(|_| CorrelationId::generate().as_str().to_owned())
}

/// One publication to enqueue.
#[derive(Debug, Clone, PartialEq)]
pub struct Publication {
    pub event_id: String,
    pub event_type: &'static str,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub subject_id: Uuid,
    /// The envelope's `data` (ids only, the catalogue's keys).
    pub data: Map<String, Value>,
    pub dedupe_key: String,
    pub idempotency_key: Option<String>,
    pub correlation_id: String,
    pub occurred_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// `developer-platform.production-access.decided` for `decision` (`APPROVED` /
/// `REJECTED`) of request `request_id`.
pub fn production_access_decided(
    request_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    decision: &'static str,
    correlation_id: &str,
    now: DateTime<Utc>,
) -> Publication {
    let request = request_id.to_string();
    let mut data = Map::new();
    data.insert("decision".into(), json!(decision));
    data.insert("requestId".into(), json!(request));
    Publication {
        event_id: event_id(PRODUCTION_ACCESS_DECIDED, &[&request, decision]),
        event_type: PRODUCTION_ACCESS_DECIDED,
        tenant_id,
        application_id,
        environment_id: None,
        subject_id: request_id,
        data,
        dedupe_key: format!("{PRODUCTION_ACCESS_DECIDED}:{request}:{decision}"),
        // Today's Meeting ledger key of the operator-console email (same dedupe rule).
        idempotency_key: Some(format!("devplat-access:{request}:{decision}")),
        correlation_id: correlation(correlation_id),
        occurred_at: now,
        expires_at: now + LIFETIME,
    }
}

/// The credential notice `notice` of `credential_id` (expiring at `expires_at`) in
/// `scope` (tenant, application, environment).
pub fn credential_notice(
    scope: (Uuid, Uuid, Uuid),
    credential_id: Uuid,
    notice: &Notice,
    expires_at: DateTime<Utc>,
    correlation_id: &str,
    now: DateTime<Utc>,
) -> Publication {
    let event_type = if notice.event_type == EXPIRED {
        CREDENTIAL_EXPIRED
    } else {
        CREDENTIAL_EXPIRING_SOON
    };
    let credential = credential_id.to_string();
    let threshold = notice.threshold_days.to_string();
    let epoch = expires_at.timestamp().to_string();
    let mut data = Map::new();
    data.insert("applicationId".into(), json!(scope.1.to_string()));
    data.insert("credentialId".into(), json!(credential));
    data.insert("expiresAt".into(), json!(rfc3339(expires_at)));
    // A warning is useless once the credential expired (`expired` follows).
    let lifetime_end = if event_type == CREDENTIAL_EXPIRING_SOON {
        (now + LIFETIME).min(expires_at)
    } else {
        now + LIFETIME
    };
    Publication {
        event_id: event_id(event_type, &[&credential, &threshold, &epoch]),
        event_type,
        tenant_id: scope.0,
        application_id: scope.1,
        environment_id: Some(scope.2),
        subject_id: credential_id,
        data,
        dedupe_key: format!("{event_type}:{credential}:{threshold}:{epoch}"),
        idempotency_key: Some(format!(
            "devplat-credential:{credential}:{threshold}:{epoch}"
        )),
        correlation_id: correlation(correlation_id),
        occurred_at: notice.occurred_at,
        expires_at: lifetime_end,
    }
}

/// Writes `p` (inside the caller's transaction when there is one). Idempotent: `true`
/// when a row was written, `false` when the same event was already enqueued.
pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    p: &Publication,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let written = db
        .execute(stmt(
            "INSERT INTO devplat_notification_publications (id, event_id, decision_id, event_type, tenant_id, \
               application_id, environment_id, subject_id, data, dedupe_key, idempotency_key, correlation_id, \
               next_attempt_at, occurred_at, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $13, $15) \
             ON CONFLICT (event_id) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                p.event_id.clone().into(),
                decision_id(&p.event_id).into(),
                p.event_type.into(),
                p.tenant_id.into(),
                p.application_id.into(),
                p.environment_id.into(),
                p.subject_id.into(),
                Value::Object(p.data.clone()).into(),
                p.dedupe_key.clone().into(),
                p.idempotency_key.clone().into(),
                p.correlation_id.clone().into(),
                now.into(),
                p.occurred_at.into(),
                p.expires_at.into(),
            ],
        ))
        .await?
        .rows_affected();
    Ok(written > 0)
}

// -------------------------------------------------------------------------------- sink

/// One per-event answer of I1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResult {
    pub event_id: String,
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailure {
    /// Retry with backoff (5xx, 401, 403, 408, 429, timeout, unreachable, malformed answer).
    Unavailable,
    /// The service refused the batch for good (a contract error): dead letter.
    Rejected,
    /// No Notification Service target configured: rows wait.
    NotConfigured,
}

impl PublishFailure {
    fn code(self) -> &'static str {
        match self {
            PublishFailure::Unavailable => "UNAVAILABLE",
            PublishFailure::Rejected => "CONTRACT_REJECTED",
            PublishFailure::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

/// Where publications go (I1).
#[async_trait]
pub trait NotificationEventSink: Send + Sync {
    /// Posts one batch (≤ [`MAX_BATCH`]).
    async fn ingest(&self, events: &[Value]) -> Result<Vec<IngestResult>, PublishFailure>;
}

/// The service-authenticated I1 of the Notification Service (`cfmeet.notifications`,
/// `notifications-event.publish`), signed with the Developer Platform's own service key.
pub struct NotificationServiceEvents {
    client: Arc<InternalServiceClient>,
}

impl NotificationServiceEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct IngestAnswer {
    results: Vec<IngestResult>,
}

/// The I1 status classification: `Ok(())` for 200, else the failure.
pub fn classify_status(status: u16) -> Result<(), PublishFailure> {
    match status {
        200 => Ok(()),
        s if s >= 500 || matches!(s, 401 | 403 | 408 | 429) => Err(PublishFailure::Unavailable),
        _ => Err(PublishFailure::Rejected),
    }
}

#[async_trait]
impl NotificationEventSink for NotificationServiceEvents {
    async fn ingest(&self, events: &[Value]) -> Result<Vec<IngestResult>, PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                INGEST_PATH,
                Some(&json!({ "events": events })),
            )
            .await
        {
            Ok(r) => {
                classify_status(r.status())?;
                r.json::<IngestAnswer>()
                    .map(|a| a.results)
                    .map_err(|_| PublishFailure::Unavailable)
            }
            Err(InternalCallError::UnknownTarget(_)) => Err(PublishFailure::NotConfigured),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

// ---------------------------------------------------------------------------- envelope

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    event_id: String,
    decision_id: Uuid,
    event_type: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Option<Uuid>,
    subject_id: Uuid,
    data: Value,
    dedupe_key: String,
    idempotency_key: Option<String>,
    correlation_id: String,
    envelope: Option<Value>,
    attempts: i32,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// Why a row's envelope was not built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unbuilt {
    /// Nothing to publish (recorded as SKIPPED with this reason).
    Skip(&'static str),
    /// The database could not answer: retry.
    Unavailable,
    /// The envelope violates the contract (a bug): dead letter.
    Invalid,
}

async fn user_ids<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Vec<Uuid>, DbErr> {
    db.query_all(stmt(sql, values))
        .await?
        .iter()
        .map(|r| r.try_get::<Uuid>("", "user_id"))
        .collect()
}

/// The CF Meet account references the event of `row` is for.
async fn recipients<C: ConnectionTrait>(db: &C, row: &Row) -> Result<Vec<String>, Unbuilt> {
    let unavailable = |_| Unbuilt::Unavailable;
    let owners_and_admins = |roles: &'static [&'static str]| {
        let roles: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        user_ids(
            db,
            "SELECT user_id FROM devplat_workspace_members \
              WHERE tenant_id = $1 AND status = 'ACTIVE' AND role = ANY($2) \
              ORDER BY (role = 'OWNER') DESC, created_at, user_id LIMIT $3",
            vec![
                row.tenant_id.into(),
                roles.into(),
                (MAX_RECIPIENTS as i64).into(),
            ],
        )
    };
    let users = if row.event_type == PRODUCTION_ACCESS_DECIDED {
        let request = db
            .query_one(stmt(
                "SELECT requested_by FROM devplat_production_access_requests WHERE id = $1 AND tenant_id = $2",
                vec![row.subject_id.into(), row.tenant_id.into()],
            ))
            .await
            .map_err(unavailable)?
            .ok_or(Unbuilt::Skip("SUBJECT_NOT_FOUND"))?;
        let requested_by: String = request.try_get("", "requested_by").map_err(unavailable)?;
        // The applicant, while still a member of the organization (the operator-console
        // email went to the applicant); a machine applicant: the organization's owners.
        let applicant = match requested_by
            .strip_prefix("user:")
            .and_then(|u| Uuid::parse_str(u).ok())
        {
            Some(user) => user_ids(
                db,
                "SELECT user_id FROM devplat_workspace_members \
                  WHERE tenant_id = $1 AND user_id = $2 AND status = 'ACTIVE'",
                vec![row.tenant_id.into(), user.into()],
            )
            .await
            .map_err(unavailable)?,
            None => Vec::new(),
        };
        if applicant.is_empty() {
            owners_and_admins(&["OWNER"]).await.map_err(unavailable)?
        } else {
            applicant
        }
    } else {
        owners_and_admins(&["OWNER", "ADMIN"])
            .await
            .map_err(unavailable)?
    };
    let mut refs: Vec<String> = users.iter().map(|u| format!("cfmeet-user:{u}")).collect();
    refs.sort();
    refs.dedup();
    Ok(refs)
}

/// The ids-only envelope of `row` for `recipients`.
fn envelope(row: &Row, recipients: &[String]) -> Result<Value, Unbuilt> {
    let route = RouteRef {
        decision_id: row.decision_id,
        routing_version: ROUTING_VERSION,
        protocol: ROUTING_PROTOCOL,
    };
    let mut builder = EnvelopeBuilder::new(
        Producer::DeveloperPlatform,
        &row.event_type,
        &rfc3339(row.occurred_at),
        route,
    )
    .map_err(|_| Unbuilt::Invalid)?
    .event_id(row.event_id.clone())
    .correlation_id(row.correlation_id.clone())
    .subject(row.subject_id, None)
    .dedupe_key(row.dedupe_key.clone())
    .expires_at(rfc3339(row.expires_at));
    if let Some(environment_id) = row.environment_id {
        builder = builder.scope(Scope {
            tenant_id: row.tenant_id,
            application_id: row.application_id,
            environment_id,
        });
    }
    if let Some(k) = &row.idempotency_key {
        builder = builder.idempotency_key(k.clone());
    }
    let Value::Object(data) = &row.data else {
        return Err(Unbuilt::Invalid);
    };
    for (k, v) in data {
        builder = builder.data(k, v.clone());
    }
    for r in recipients {
        builder = builder.recipient(r.clone());
    }
    builder
        .build()
        .map_err(|_| Unbuilt::Invalid)
        .and_then(|e| serde_json::to_value(e).map_err(|_| Unbuilt::Invalid))
}

async fn build<C: ConnectionTrait>(db: &C, row: &Row) -> Result<(Value, usize), Unbuilt> {
    let refs = recipients(db, row).await?;
    if refs.is_empty() {
        return Err(Unbuilt::Skip("NO_RECIPIENTS"));
    }
    Ok((envelope(row, &refs)?, refs.len()))
}

// ------------------------------------------------------------------------------- relay

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublicationReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub skipped: usize,
    pub expired: u64,
    /// Rows left waiting because no Notification Service target is configured.
    pub not_configured: usize,
}

/// Exponential backoff of attempt `attempts` (2 s … 5 min).
pub fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

/// A machine reason from the service, kept only when it has the recorded shape.
fn reason_code(reason: Option<&str>, fallback: &'static str) -> String {
    reason
        .filter(|r| {
            (2..=64).contains(&r.len())
                && r.as_bytes()[0].is_ascii_uppercase()
                && r.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or(fallback)
        .to_owned()
}

async fn settle(
    txn: &DatabaseTransaction,
    row: &Row,
    state: &str,
    outcome: Option<&str>,
    error: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    txn.execute(stmt(
        "UPDATE devplat_notification_publications SET state = $2, outcome = $3, last_error = $4, \
           attempts = attempts + CASE WHEN $2 IN ('ACKED', 'DEAD') THEN 1 ELSE 0 END, completed_at = $5 \
         WHERE id = $1",
        vec![
            row.id.into(),
            state.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn retry(
    txn: &DatabaseTransaction,
    row: &Row,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    txn.execute(stmt(
        "UPDATE devplat_notification_publications SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    ))
    .await?;
    Ok(())
}

/// One relay pass at `now` over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the re-post is answered `DUPLICATE` (acknowledged). Expired PENDING rows end
/// `EXPIRED` first and are never posted.
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<PublicationReport, DbErr> {
    let mut report = PublicationReport {
        expired: expire_overdue(db, now).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = db.begin().await?;
        let rows = Row::find_by_statement(stmt(
            "SELECT id, event_id, decision_id, event_type, tenant_id, application_id, environment_id, subject_id, \
                    data, dedupe_key, idempotency_key, correlation_id, envelope, attempts, occurred_at, expires_at \
               FROM devplat_notification_publications \
              WHERE state = 'PENDING' AND next_attempt_at <= $1 AND expires_at > $1 \
              ORDER BY next_attempt_at, created_at LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), (batch as i64).into()],
        ))
        .all(&txn)
        .await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        let mut ready: Vec<(Row, Value)> = Vec::with_capacity(rows.len());
        let mut stop = false;
        // Build every missing envelope once; it is stored with the attempt. Recipients are
        // read outside the batch transaction (a failed read must not abort it).
        let mut unavailable = false;
        for mut row in rows {
            let built = match row.envelope.take() {
                Some(e) => Ok((e, None)),
                // The database just failed for this batch: do not ask again per row.
                None if unavailable => Err(Unbuilt::Unavailable),
                None => build(db, &row).await.map(|(e, n)| (e, Some(n))),
            };
            unavailable |= matches!(built, Err(Unbuilt::Unavailable));
            match built {
                Ok((e, None)) => ready.push((row, e)),
                Ok((e, Some(n))) => {
                    txn.execute(stmt(
                        "UPDATE devplat_notification_publications SET envelope = $2, recipients = $3 WHERE id = $1",
                        vec![row.id.into(), e.clone().into(), (n as i32).into()],
                    ))
                    .await?;
                    ready.push((row, e));
                }
                Err(Unbuilt::Skip(reason)) => {
                    settle(&txn, &row, "SKIPPED", None, Some(reason.into()), now).await?;
                    report.skipped += 1;
                }
                Err(Unbuilt::Unavailable) => {
                    retry(&txn, &row, "RECIPIENTS_UNAVAILABLE", true, now).await?;
                    report.retried += 1;
                }
                Err(Unbuilt::Invalid) => {
                    settle(
                        &txn,
                        &row,
                        "DEAD",
                        None,
                        Some("ENVELOPE_INVALID".into()),
                        now,
                    )
                    .await?;
                    report.dead += 1;
                }
            }
        }
        if !ready.is_empty() {
            let events: Vec<Value> = ready.iter().map(|(_, e)| e.clone()).collect();
            match sink.ingest(&events).await {
                Ok(results) => {
                    let by_id: HashMap<&str, &IngestResult> =
                        results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                    for (row, _) in &ready {
                        match by_id.get(row.event_id.as_str()) {
                            Some(r)
                                if matches!(
                                    r.outcome.as_str(),
                                    "ACCEPTED" | "DUPLICATE" | "HELD"
                                ) =>
                            {
                                settle(&txn, row, "ACKED", Some(&r.outcome), None, now).await?;
                                report.acked += 1;
                            }
                            Some(r) if r.outcome == "REJECTED" => {
                                let reason = reason_code(r.reason.as_deref(), "REJECTED");
                                settle(&txn, row, "DEAD", Some("REJECTED"), Some(reason), now)
                                    .await?;
                                report.dead += 1;
                            }
                            // No (or an unexpected) answer for this event: try again.
                            _ => {
                                retry(&txn, row, "NO_RESULT", true, now).await?;
                                report.retried += 1;
                            }
                        }
                    }
                }
                Err(f @ PublishFailure::Rejected) => {
                    for (row, _) in &ready {
                        settle(&txn, row, "DEAD", None, Some(f.code().into()), now).await?;
                        report.dead += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::Unavailable) => {
                    for (row, _) in &ready {
                        retry(&txn, row, f.code(), true, now).await?;
                        report.retried += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::NotConfigured) => {
                    for (row, _) in &ready {
                        retry(&txn, row, f.code(), false, now).await?;
                        report.not_configured += 1;
                    }
                    stop = true;
                }
            }
        }
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted). Returns how many.
async fn expire_overdue(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE devplat_notification_publications SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
              WHERE state = 'PENDING' AND expires_at <= $1",
            vec![now.into()],
        ))
        .await?
        .rows_affected())
}

/// Maintenance at `now`: expires overdue PENDING rows and deletes at most `batch` settled
/// rows older than [`RETENTION`]. Returns (expired, purged).
pub async fn maintain(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    batch: i64,
) -> Result<(u64, u64), DbErr> {
    let expired = expire_overdue(db, now).await?;
    let purged = db
        .execute(stmt(
            "DELETE FROM devplat_notification_publications WHERE id IN (SELECT id FROM devplat_notification_publications \
              WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((expired, purged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(event_type: &str, data: Value, environment: bool) -> Row {
        let now = Utc::now();
        let event_id = event_id(event_type, &["x"]);
        Row {
            id: Uuid::new_v4(),
            decision_id: decision_id(&event_id),
            event_id,
            event_type: event_type.into(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: environment.then(Uuid::new_v4),
            subject_id: Uuid::new_v4(),
            data,
            dedupe_key: format!("{event_type}:x"),
            idempotency_key: Some("devplat-access:x:APPROVED".into()),
            correlation_id: CorrelationId::generate().as_str().into(),
            envelope: None,
            attempts: 0,
            occurred_at: now,
            expires_at: now + LIFETIME,
        }
    }

    #[test]
    fn ids_are_deterministic_and_distinct_per_fact() {
        let a = event_id(PRODUCTION_ACCESS_DECIDED, &["r", "APPROVED"]);
        assert!(a.starts_with("evt_") && a.len() == 36);
        assert_eq!(a, event_id(PRODUCTION_ACCESS_DECIDED, &["r", "APPROVED"]));
        assert_ne!(a, event_id(PRODUCTION_ACCESS_DECIDED, &["r", "REJECTED"]));
        assert_ne!(a, event_id(CREDENTIAL_EXPIRED, &["r", "APPROVED"]));
        assert_eq!(decision_id(&a), decision_id(&a));
        assert_ne!(
            decision_id(&a),
            decision_id(&event_id(CREDENTIAL_EXPIRED, &["r"]))
        );
        assert_eq!(decision_id(&a).get_version_num(), 4);
    }

    #[test]
    fn the_largest_audience_fits_one_envelope() {
        let refs: Vec<String> = (0..MAX_RECIPIENTS)
            .map(|_| format!("cfmeet-user:{}", Uuid::new_v4()))
            .collect();
        let credential = row(
            CREDENTIAL_EXPIRING_SOON,
            json!({
                "applicationId": Uuid::new_v4().to_string(),
                "credentialId": Uuid::new_v4().to_string(),
                "expiresAt": "2026-12-01T00:00:00Z",
            }),
            true,
        );
        assert!(envelope(&credential, &refs).is_ok());
        let decided = row(
            PRODUCTION_ACCESS_DECIDED,
            json!({ "decision": "REJECTED", "requestId": Uuid::new_v4().to_string() }),
            false,
        );
        assert!(envelope(&decided, &refs).is_ok());
        // A data key outside the catalogue is a contract violation, never sent.
        let bad = row(
            PRODUCTION_ACCESS_DECIDED,
            json!({ "decision": "APPROVED", "requestId": Uuid::new_v4().to_string(), "email": "x" }),
            false,
        );
        assert_eq!(envelope(&bad, &refs), Err(Unbuilt::Invalid));
    }

    #[test]
    fn statuses_classify_into_retry_and_dead_letter() {
        assert_eq!(classify_status(200), Ok(()));
        for s in [500, 502, 503, 401, 403, 408, 429] {
            assert_eq!(classify_status(s), Err(PublishFailure::Unavailable), "{s}");
        }
        for s in [400, 404, 409, 413, 422] {
            assert_eq!(classify_status(s), Err(PublishFailure::Rejected), "{s}");
        }
    }

    #[test]
    fn backoff_grows_and_is_bounded_and_reasons_keep_their_shape() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert!(backoff(5) > backoff(4));
        assert_eq!(backoff(40), Duration::seconds(300));
        assert_eq!(
            reason_code(Some("DIGEST_MISMATCH"), "REJECTED"),
            "DIGEST_MISMATCH"
        );
        assert_eq!(reason_code(Some("bad reason"), "REJECTED"), "REJECTED");
        assert_eq!(reason_code(None, "REJECTED"), "REJECTED");
    }
}
