//! Organizations' publications to the Notification Service (roadmap N7, Document 4 §5.1,
//! §5.2 #1–10): the catalogue events `organizations.invitation.{created, accepted,
//! declined, revoked, expired}`, `organizations.membership.{role_changed, suspended,
//! removed}`, `organizations.ownership.transferred` and
//! `organizations.organization.suspended`, posted to I1
//! (`POST /internal/v1/notification-events`, `ingestNotificationEvents`).
//!
//! Organizations has no old notification path (it never notified users itself), so it
//! needs no routing-fence decision table (Document 4 §9.2): everything it publishes is
//! new-only. The publications are fed from the SAME committed outbox rows as every other
//! consumer: the `cfmeet.reconciliation` record row that [`crate::store::emit`] writes in
//! the transaction of each change. Nothing extra is written in the business transaction,
//! and nothing at all happens while `CF_NOTIFICATIONS_BASE_URL` is unset.
//!
//! * **Discovery** ([`discover`]): record rows of the ten source types younger than
//!   [`LIFETIME`] that have no publication yet become `organization_notification_publications`
//!   rows (one discoverer at a time, advisory lock). The recipients are resolved then, in
//!   Organizations' own database and only within the event's organization, and the ids-only
//!   envelope is built once with `packages/notification-client` and stored, so every re-post
//!   is byte-identical. `eventId` (`evt_<hex>`) and `route.decisionId` derive from the source
//!   event id (and the recipient chunk): a redelivery is a `DUPLICATE`, never a second
//!   notification. An event without a recipient, or whose value the contract cannot carry,
//!   is recorded `SKIPPED`.
//! * **Relay** ([`relay_due`]): batches of ≤ [`MAX_BATCH`] due rows are posted with a
//!   service token for `cfmeet.notifications` (`notifications-event.publish`). `ACCEPTED`,
//!   `DUPLICATE` and `HELD` acknowledge (`ACKED`); a per-event `REJECTED` or a contract
//!   refusal of the batch (4xx other than 401 / 403 / 408 / 429) dead-letters (`DEAD`);
//!   5xx, 401 / 403 / 408 / 429 and transport failures retry with exponential backoff, at
//!   most [`MAX_ATTEMPTS`] times and never past the row's expiry (`EXPIRED`).
//! * **Maintenance** ([`maintain`]): expires overdue rows and purges settled rows older than
//!   [`RETENTION`].
//!
//! Recipients (`cfmeet-user:<id>`; the acting user is never notified of their own change):
//!
//! | Source event | Recipients |
//! |---|---|
//! | `invitation.created` | the invitee's account, when Organizations knows it (it does not for a pending, email-addressed invitation: then `SKIPPED`) |
//! | `invitation.accepted` / `invitation.declined` | the inviter |
//! | `invitation.revoked` / `invitation.expired` | the inviter and the invitee's account when known |
//! | `membership.role_changed` / `membership.suspended` / `membership.removed` | the member |
//! | `ownership.transferred` | the new and the previous owner |
//! | `organization.suspended` | the organization's ACTIVE members at the time of the event, in chunks of [`RECIPIENTS_PER_ENVELOPE`] |

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{
    envelope::MAX_RECIPIENTS, EnvelopeBuilder, EnvelopeError, Producer, RouteRef, ROUTING_PROTOCOL,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, FromQueryResult,
    TransactionTrait,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{db, store::rfc3339};

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";
/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;
/// How long a source event may wait to be published (also the envelope's
/// `delivery.expiresAt`, and the discovery window).
pub const LIFETIME: Duration = Duration::hours(24);
/// How long a settled row is kept (diagnostics) before [`maintain`] removes it. Longer
/// than [`LIFETIME`], so a purged source is never discovered again.
pub const RETENTION: Duration = Duration::days(30);
/// Counted attempts before a row is dead-lettered (`ATTEMPTS_EXHAUSTED`).
pub const MAX_ATTEMPTS: i32 = 16;
/// Recipients per envelope: the worst case (every account ref, the largest data set)
/// stays well inside the 4 KiB envelope limit.
pub const RECIPIENTS_PER_ENVELOPE: usize = 40;
const _: () = assert!(RECIPIENTS_PER_ENVELOPE <= MAX_RECIPIENTS);
/// Organizations has no routing fence (new-only producer, Document 4 §9.2).
pub const ROUTING_VERSION: i32 = 0;
/// Upper bound of the members one `organization.suspended` is addressed to.
pub const MAX_ORGANIZATION_RECIPIENTS: i64 = 20_000;
/// `pg_try_advisory_xact_lock` key of the discoverer ("orgnotif").
const DISCOVERY_LOCK: i64 = 0x6f72_676e_6f74_6966;

/// Source outbox event type → catalogue event type.
pub const SOURCE_TYPES: &[(&str, &str)] = &[
    ("invitation.created", "organizations.invitation.created"),
    ("invitation.accepted", "organizations.invitation.accepted"),
    ("invitation.declined", "organizations.invitation.declined"),
    ("invitation.revoked", "organizations.invitation.revoked"),
    ("invitation.expired", "organizations.invitation.expired"),
    (
        "membership.role_changed",
        "organizations.membership.role_changed",
    ),
    ("membership.suspended", "organizations.membership.suspended"),
    ("membership.removed", "organizations.membership.removed"),
    (
        "ownership.transferred",
        "organizations.ownership.transferred",
    ),
    (
        "organization.suspended",
        "organizations.organization.suspended",
    ),
];

/// Roles the notification contract can carry (`data.role` enum). Organizations' MANAGER
/// is not among them.
pub const CONTRACT_ROLES: &[&str] = &["OWNER", "ADMIN", "MEMBER", "GUEST"];

/// The catalogue type published for an Organizations outbox event type.
pub fn catalogue_type(source_type: &str) -> Option<&'static str> {
    SOURCE_TYPES
        .iter()
        .find(|(s, _)| *s == source_type)
        .map(|(_, c)| *c)
}

/// The `cfmeet-user:<uuid>` account reference of the Notification Service contracts.
pub fn account_ref(user: Uuid) -> String {
    format!("cfmeet-user:{user}")
}

fn derived(source: Uuid, chunk: i16, purpose: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"cfmeet.organizations/notification-publication/");
    h.update(purpose.as_bytes());
    h.update(b"/");
    h.update(source.as_bytes());
    h.update(chunk.to_be_bytes());
    let digest = h.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// The envelope `eventId` of chunk `chunk` of source event `source` (deterministic).
pub fn publication_event_id(source: Uuid, chunk: i16) -> String {
    format!("evt_{}", hex::encode(derived(source, chunk, "event")))
}

/// The envelope `route.decisionId` of chunk `chunk` of `source` (deterministic, unique
/// per publication).
pub fn decision_id(source: Uuid, chunk: i16) -> Uuid {
    uuid::Builder::from_random_bytes(derived(source, chunk, "decision")).into_uuid()
}

// ------------------------------------------------------------------------------- source

/// A committed record row of the Organizations outbox.
#[derive(Debug, Clone, FromQueryResult)]
pub struct Source {
    pub event_id: Uuid,
    pub organization_id: Uuid,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    pub event_type: String,
    /// The organization event envelope (`docs/events/organization-event-contracts.md`).
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

fn uuid_at(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::try_parse(s).ok())
}

#[derive(FromQueryResult)]
struct IdRow {
    id: Uuid,
}

/// The users a source event is addressed to (sorted, distinct, never the actor), read
/// within the event's organization only.
pub async fn recipients<C: ConnectionTrait>(c: &C, src: &Source) -> Result<Vec<Uuid>, DbErr> {
    let inner = &src.payload["payload"];
    let inviter = || async {
        db::one::<IdRow, _>(
            c,
            "SELECT invited_by AS id FROM organization_invitations WHERE id = $1 AND organization_id = $2",
            vec![src.aggregate_id.into(), src.organization_id.into()],
        )
        .await
        .map(|r| r.map(|r| r.id))
    };
    let invitee = uuid_at(inner, "inviteeUserId");
    let mut users: Vec<Uuid> = match src.event_type.as_str() {
        "invitation.created" => invitee.into_iter().collect(),
        "invitation.accepted" | "invitation.declined" => inviter().await?.into_iter().collect(),
        "invitation.revoked" | "invitation.expired" => {
            inviter().await?.into_iter().chain(invitee).collect()
        }
        "membership.role_changed" | "membership.suspended" | "membership.removed" => {
            uuid_at(inner, "userId").into_iter().collect()
        }
        "ownership.transferred" => uuid_at(inner, "toUserId")
            .into_iter()
            .chain(uuid_at(inner, "fromUserId"))
            .collect(),
        "organization.suspended" => db::all::<IdRow, _>(
            c,
            "SELECT user_id AS id FROM organization_memberships \
              WHERE organization_id = $1 AND status = 'ACTIVE' AND created_at <= $2 \
              ORDER BY user_id LIMIT $3",
            vec![
                src.organization_id.into(),
                src.created_at.into(),
                MAX_ORGANIZATION_RECIPIENTS.into(),
            ],
        )
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect(),
        _ => Vec::new(),
    };
    if let Some(actor) = uuid_at(&src.payload["actor"], "userId") {
        users.retain(|u| *u != actor);
    }
    users.sort();
    users.dedup();
    Ok(users)
}

/// Why a source event yields no envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unbuilt {
    /// Nothing to publish (recorded `SKIPPED` with this reason).
    Skip(&'static str),
    /// The envelope violates the contract (a bug): recorded `DEAD`.
    Invalid,
}

/// The catalogue `data` of a source event (ids and the contract role only).
fn data(src: &Source) -> Result<Vec<(&'static str, Value)>, Unbuilt> {
    let org = json!(src.organization_id.to_string());
    let subject = json!(src.aggregate_id.to_string());
    let inner = &src.payload["payload"];
    let contract_role = |key: &str| {
        inner
            .get(key)
            .and_then(Value::as_str)
            .filter(|r| CONTRACT_ROLES.contains(r))
            .map(|r| json!(r))
    };
    Ok(match src.event_type.as_str() {
        t if t.starts_with("invitation.") => {
            let mut d = vec![("invitationId", subject), ("organizationId", org)];
            // Optional in the catalogue: omitted when the contract cannot carry it.
            if let Some(role) = contract_role("role") {
                d.push(("role", role));
            }
            d
        }
        "membership.role_changed" => vec![
            ("membershipId", subject),
            ("organizationId", org),
            (
                "role",
                contract_role("toRole").ok_or(Unbuilt::Skip("ROLE_OUTSIDE_CONTRACT"))?,
            ),
        ],
        "membership.suspended" | "membership.removed" => {
            vec![("membershipId", subject), ("organizationId", org)]
        }
        "ownership.transferred" | "organization.suspended" => vec![("organizationId", org)],
        _ => return Err(Unbuilt::Skip("NOT_PUBLISHED")),
    })
}

/// The correlation id of the change that caused the event (`corr_<32 hex>`).
fn correlation(src: &Source) -> String {
    let id = uuid_at(&src.payload, "correlationId").unwrap_or(src.event_id);
    format!("corr_{}", id.simple())
}

/// The ids-only envelopes of a source event for `recipients` (one per chunk of
/// [`RECIPIENTS_PER_ENVELOPE`]), in chunk order.
pub fn envelopes(src: &Source, recipients: &[Uuid]) -> Result<Vec<Value>, Unbuilt> {
    let event_type = catalogue_type(&src.event_type).ok_or(Unbuilt::Skip("NOT_PUBLISHED"))?;
    let data = data(src)?;
    if recipients.is_empty() {
        return Err(Unbuilt::Skip("NO_RECIPIENTS"));
    }
    let occurred_at = rfc3339(src.created_at);
    let expires_at = rfc3339(src.created_at + LIFETIME);
    let mut out = Vec::new();
    for (chunk, refs) in recipients.chunks(RECIPIENTS_PER_ENVELOPE).enumerate() {
        let chunk = i16::try_from(chunk)
            .ok()
            .filter(|c| *c < 1000)
            .ok_or(Unbuilt::Skip("RECIPIENT_LIMIT"))?;
        let event_id = publication_event_id(src.event_id, chunk);
        let route = RouteRef {
            decision_id: decision_id(src.event_id, chunk),
            routing_version: ROUTING_VERSION,
            protocol: ROUTING_PROTOCOL,
        };
        let mut builder =
            EnvelopeBuilder::new(Producer::Organizations, event_type, &occurred_at, route)
                .map_err(|_| Unbuilt::Invalid)?
                .event_id(event_id.clone())
                .correlation_id(correlation(src))
                .subject(src.aggregate_id, Some(src.aggregate_version))
                .dedupe_key(format!("{event_type}:{}", src.event_id))
                .idempotency_key(event_id)
                .expires_at(expires_at.clone());
        for (k, v) in &data {
            builder = builder.data(k, v.clone());
        }
        for user in refs {
            builder = builder.recipient(account_ref(*user));
        }
        match builder.build() {
            Ok(envelope) => out.push(serde_json::to_value(envelope).map_err(|_| Unbuilt::Invalid)?),
            Err(EnvelopeError::TooLarge(_)) => return Err(Unbuilt::Skip("RECIPIENT_LIMIT")),
            Err(_) => return Err(Unbuilt::Invalid),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------- discovery

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublicationReport {
    /// Source events turned into publication rows (any state).
    pub discovered: usize,
    pub skipped: usize,
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub expired: u64,
    pub purged: u64,
    /// Rows left waiting because no Notification Service target is configured.
    pub not_configured: usize,
}

#[allow(clippy::too_many_arguments)]
async fn insert(
    txn: &DatabaseTransaction,
    src: &Source,
    event_type: &str,
    chunk: i16,
    envelope: Option<Value>,
    state: &str,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> Result<u64, DbErr> {
    db::exec(
        txn,
        "INSERT INTO organization_notification_publications (id, event_id, source_event_id, chunk, organization_id, \
           event_type, envelope, state, last_error, next_attempt_at, occurred_at, created_at, expires_at, completed_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $10, $12, CASE WHEN $8 = 'PENDING' THEN NULL ELSE $10 END) \
         ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            publication_event_id(src.event_id, chunk).into(),
            src.event_id.into(),
            chunk.into(),
            src.organization_id.into(),
            event_type.into(),
            envelope.into(),
            state.into(),
            error.map(str::to_owned).into(),
            now.into(),
            src.created_at.into(),
            (src.created_at + LIFETIME).into(),
        ],
    )
    .await
}

/// Turns at most `limit` recent, not yet published source events into publication rows
/// (built envelopes, or a `SKIPPED` / `DEAD` record). One discoverer at a time; returns
/// (discovered, skipped).
pub async fn discover(db: &DatabaseConnection, limit: i64) -> Result<(usize, usize), DbErr> {
    let txn = db.begin().await?;
    let locked = txn
        .query_one(db::stmt(
            "SELECT pg_try_advisory_xact_lock($1) AS locked",
            vec![DISCOVERY_LOCK.into()],
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "locked"))
        .transpose()?
        .unwrap_or(false);
    if !locked {
        txn.rollback().await?;
        return Ok((0, 0));
    }
    let now = db::now(&txn).await?;
    let types: Vec<String> = SOURCE_TYPES.iter().map(|(s, _)| s.to_string()).collect();
    let sources: Vec<Source> = db::all(
        &txn,
        "SELECT o.event_id, o.organization_id, o.aggregate_id, o.aggregate_version, o.event_type, o.payload, o.created_at \
           FROM organization_outbox o \
          WHERE o.destination = 'cfmeet.reconciliation' AND o.created_at > $1 AND o.event_type = ANY($2) \
            AND NOT EXISTS (SELECT 1 FROM organization_notification_publications p WHERE p.source_event_id = o.event_id) \
          ORDER BY o.created_at, o.seq LIMIT $3",
        vec![(now - LIFETIME).into(), types.into(), limit.into()],
    )
    .await?;
    let (mut discovered, mut skipped) = (0, 0);
    for src in &sources {
        let event_type =
            catalogue_type(&src.event_type).unwrap_or("organizations.invitation.created");
        let users = recipients(&txn, src).await?;
        match envelopes(src, &users) {
            Ok(built) => {
                for (chunk, envelope) in built.into_iter().enumerate() {
                    insert(
                        &txn,
                        src,
                        event_type,
                        chunk as i16,
                        Some(envelope),
                        "PENDING",
                        None,
                        now,
                    )
                    .await?;
                }
            }
            Err(Unbuilt::Skip(reason)) => {
                insert(&txn, src, event_type, 0, None, "SKIPPED", Some(reason), now).await?;
                skipped += 1;
            }
            Err(Unbuilt::Invalid) => {
                tracing::error!(source_event_id = %src.event_id, event_type, "notification envelope violates the contract");
                insert(
                    &txn,
                    src,
                    event_type,
                    0,
                    None,
                    "DEAD",
                    Some("ENVELOPE_INVALID"),
                    now,
                )
                .await?;
            }
        }
        discovered += 1;
    }
    txn.commit().await?;
    Ok((discovered, skipped))
}

// ------------------------------------------------------------------------------- sink

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
/// `notifications-event.publish`), signed with Organizations' outbound key.
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

/// Maps an I1 HTTP status to the relay's failure class (`None`: 200).
pub fn classify_status(status: u16) -> Option<PublishFailure> {
    match status {
        200 => None,
        s if s >= 500 || matches!(s, 401 | 403 | 408 | 429) => Some(PublishFailure::Unavailable),
        _ => Some(PublishFailure::Rejected),
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
            Ok(r) => match classify_status(r.status()) {
                None => r
                    .json::<IngestAnswer>()
                    .map(|a| a.results)
                    .map_err(|_| PublishFailure::Unavailable),
                Some(f) => Err(f),
            },
            Err(InternalCallError::UnknownTarget(_)) => Err(PublishFailure::NotConfigured),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

// -------------------------------------------------------------------------------- relay

#[derive(Debug, Clone, FromQueryResult)]
struct Due {
    id: Uuid,
    event_id: String,
    envelope: Value,
    attempts: i32,
}

/// Exponential backoff after `attempts` counted attempts (2^n seconds, at most 5 min).
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
    row: &Due,
    state: &str,
    outcome: Option<&str>,
    error: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db::exec(
        txn,
        "UPDATE organization_notification_publications SET state = $2, outcome = $3, last_error = $4, \
           attempts = attempts + 1, completed_at = $5 WHERE id = $1",
        vec![
            row.id.into(),
            state.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Schedules another attempt (`counted`: the attempt reached the service or failed in
/// transit). Returns whether the row was dead-lettered instead (attempts exhausted).
async fn retry(
    txn: &DatabaseTransaction,
    row: &Due,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let attempts = row.attempts + i32::from(counted);
    if attempts >= MAX_ATTEMPTS {
        db::exec(
            txn,
            "UPDATE organization_notification_publications SET state = 'DEAD', attempts = $2, last_error = 'ATTEMPTS_EXHAUSTED', completed_at = $3 WHERE id = $1",
            vec![row.id.into(), attempts.into(), now.into()],
        )
        .await?;
        return Ok(true);
    }
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    db::exec(
        txn,
        "UPDATE organization_notification_publications SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    )
    .await?;
    Ok(false)
}

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the re-post is answered `DUPLICATE` (acknowledged).
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<PublicationReport, DbErr> {
    let mut report = PublicationReport::default();
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = db.begin().await?;
        let now = db::now(&txn).await?;
        let rows: Vec<Due> = db::all(
            &txn,
            "SELECT id, event_id, envelope, attempts FROM organization_notification_publications \
              WHERE state = 'PENDING' AND next_attempt_at <= $1 AND expires_at > $1 \
              ORDER BY next_attempt_at, created_at, chunk LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), (batch as i64).into()],
        )
        .await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        let events: Vec<Value> = rows.iter().map(|r| r.envelope.clone()).collect();
        let mut stop = false;
        match sink.ingest(&events).await {
            Ok(results) => {
                let by_id: HashMap<&str, &IngestResult> =
                    results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                for row in &rows {
                    match by_id.get(row.event_id.as_str()) {
                        Some(r)
                            if matches!(r.outcome.as_str(), "ACCEPTED" | "DUPLICATE" | "HELD") =>
                        {
                            settle(&txn, row, "ACKED", Some(&r.outcome), None, now).await?;
                            report.acked += 1;
                        }
                        Some(r) if r.outcome == "REJECTED" => {
                            let reason = reason_code(r.reason.as_deref(), "REJECTED");
                            tracing::warn!(event_id = %row.event_id, reason = %reason, "notification publication rejected");
                            settle(&txn, row, "DEAD", Some("REJECTED"), Some(reason), now).await?;
                            report.dead += 1;
                        }
                        // No (or an unexpected) answer for this event: try again.
                        _ => {
                            if retry(&txn, row, "NO_RESULT", true, now).await? {
                                report.dead += 1;
                            } else {
                                report.retried += 1;
                            }
                        }
                    }
                }
            }
            Err(f @ PublishFailure::Rejected) => {
                tracing::warn!(
                    events = rows.len(),
                    "notification publication batch refused by the contract"
                );
                for row in &rows {
                    settle(&txn, row, "DEAD", None, Some(f.code().into()), now).await?;
                    report.dead += 1;
                }
                stop = true;
            }
            Err(f @ PublishFailure::Unavailable) => {
                for row in &rows {
                    if retry(&txn, row, f.code(), true, now).await? {
                        report.dead += 1;
                    } else {
                        report.retried += 1;
                    }
                }
                stop = true;
            }
            Err(f @ PublishFailure::NotConfigured) => {
                for row in &rows {
                    retry(&txn, row, f.code(), false, now).await?;
                    report.not_configured += 1;
                }
                stop = true;
            }
        }
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted) and deletes at most
/// `batch` settled rows older than [`RETENTION`]. Returns (expired, purged).
pub async fn maintain(db: &DatabaseConnection, batch: i64) -> Result<(u64, u64), DbErr> {
    let now = db::now(db).await?;
    let expired = db::exec(
        db,
        "UPDATE organization_notification_publications SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    let purged = db::exec(
        db,
        "DELETE FROM organization_notification_publications WHERE id IN (SELECT id FROM organization_notification_publications \
          WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(now - RETENTION).into(), batch.into()],
    )
    .await?;
    Ok((expired, purged))
}

/// One full pass (run by the maintenance loop only when `CF_NOTIFICATIONS_BASE_URL` is
/// set): maintenance, discovery, relay.
pub async fn run_once(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
) -> Result<PublicationReport, DbErr> {
    let (expired, purged) = maintain(db, 1000).await?;
    let (discovered, skipped) = discover(db, 200).await?;
    let relayed = relay_due(db, sink, 500).await?;
    Ok(PublicationReport {
        discovered,
        skipped,
        expired,
        purged,
        ..relayed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cf_notification_client::{validate, Envelope};

    fn source(event_type: &str, inner: Value) -> Source {
        let now = Utc::now();
        Source {
            event_id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            aggregate_id: Uuid::new_v4(),
            aggregate_version: i64::MAX,
            event_type: event_type.into(),
            payload: json!({
                "correlationId": Uuid::new_v4(),
                "actor": { "kind": "USER", "serviceId": "cfmeet.meeting-service", "userId": Uuid::new_v4() },
                "payload": inner,
            }),
            created_at: now,
        }
    }

    fn users(n: usize) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
        v.sort();
        v
    }

    #[test]
    fn every_source_type_maps_to_its_organizations_catalogue_type() {
        for (s, c) in SOURCE_TYPES {
            let spec = cf_notification_client::catalogue::spec(c).expect(c);
            assert_eq!(spec.producer, Some(Producer::Organizations), "{c}");
            assert_eq!(catalogue_type(s), Some(*c));
        }
        assert_eq!(catalogue_type("membership.activated"), None);
        assert_eq!(catalogue_type("organization.reactivated"), None);
    }

    #[test]
    fn envelopes_validate_and_are_deterministic_per_source_event() {
        for (s, _) in SOURCE_TYPES {
            let src = source(
                s,
                json!({ "role": "ADMIN", "toRole": "MEMBER", "userId": Uuid::new_v4() }),
            );
            let refs = users(3);
            let a = envelopes(&src, &refs).unwrap();
            let b = envelopes(&src, &refs).unwrap();
            assert_eq!(a, b, "{s}");
            assert_eq!(a.len(), 1);
            let env: Envelope = serde_json::from_value(a[0].clone()).unwrap();
            validate(&env, Some("cfmeet.organizations")).unwrap();
            assert_eq!(env.event_id, publication_event_id(src.event_id, 0));
            assert_eq!(env.route.decision_id, decision_id(src.event_id, 0));
            assert_eq!(env.recipients.len(), 3);
        }
    }

    #[test]
    fn the_worst_case_chunk_fits_one_envelope_and_large_audiences_are_chunked() {
        let src = source("invitation.revoked", json!({ "role": "GUEST" }));
        let built = envelopes(&src, &users(RECIPIENTS_PER_ENVELOPE)).unwrap();
        assert_eq!(built.len(), 1);
        let src = source("organization.suspended", json!({}));
        let built = envelopes(&src, &users(RECIPIENTS_PER_ENVELOPE * 2 + 1)).unwrap();
        assert_eq!(built.len(), 3);
        let ids: std::collections::BTreeSet<&str> = built
            .iter()
            .map(|e| e["eventId"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        let dedupe: std::collections::BTreeSet<&str> = built
            .iter()
            .map(|e| e["delivery"]["dedupeKey"].as_str().unwrap())
            .collect();
        assert_eq!(dedupe.len(), 1, "one logical notification per recipient");
    }

    #[test]
    fn values_outside_the_contract_are_never_sent() {
        // MANAGER is not a contract role: optional for invitations (omitted), required
        // for a role change (skipped).
        let src = source("invitation.created", json!({ "role": "MANAGER" }));
        let e = &envelopes(&src, &users(1)).unwrap()[0];
        assert!(e["data"].get("role").is_none());
        let src = source("membership.role_changed", json!({ "toRole": "MANAGER" }));
        assert_eq!(
            envelopes(&src, &users(1)),
            Err(Unbuilt::Skip("ROLE_OUTSIDE_CONTRACT"))
        );
        let src = source("membership.removed", json!({}));
        assert_eq!(envelopes(&src, &[]), Err(Unbuilt::Skip("NO_RECIPIENTS")));
    }

    #[test]
    fn statuses_are_classified_like_the_other_producers() {
        assert_eq!(classify_status(200), None);
        for s in [500, 502, 503, 401, 403, 408, 429] {
            assert_eq!(classify_status(s), Some(PublishFailure::Unavailable), "{s}");
        }
        for s in [400, 404, 409, 413, 422] {
            assert_eq!(classify_status(s), Some(PublishFailure::Rejected), "{s}");
        }
    }

    #[test]
    fn service_reasons_are_recorded_only_in_their_machine_shape() {
        assert_eq!(
            reason_code(Some("DIGEST_MISMATCH"), "REJECTED"),
            "DIGEST_MISMATCH"
        );
        for bad in ["", "x", "route not new", "A\u{7}B", &"A".repeat(65)] {
            assert_eq!(reason_code(Some(bad), "REJECTED"), "REJECTED", "{bad:?}");
        }
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert!(backoff(5) > backoff(4));
        assert_eq!(backoff(40), Duration::seconds(300));
        assert!(RETENTION > LIFETIME);
    }
}
