//! Calling's publications to the Notification Service (roadmap N7 / D-10, Document 4 §5.1,
//! §9.5): the producer outbox `calling_notification_publications` (its own migration
//! history, `calling_notification_publications_seaql_migrations`) and its relay to I1
//! (`POST /internal/v1/notification-events`, `ingestNotificationEvents`).
//!
//! Catalogue entries #15–#17 (`cfmeet.calling`, subject `CALL`):
//!
//! | Event | When (same transaction) | Recipients | data |
//! |---|---|---|---|
//! | `calling.call.participant-missed` (MISSED_CALLS, HIGH) | the Call reaches a terminal state ([`crate::calls::CallService`] transition) | every RECIPIENT participant whose offers all ended unanswered: the ADR 0025 `MISSED` derivation of the call history (never answered, declined or answered elsewhere, no connected session) | `callId`, `callKind` |
//! | `calling.voicemail.ready` (VOICEMAIL) | a voicemail message becomes READY | the principals who own the mailbox (its principal, an extension's member, a queue's agents, a ring group's principal members) | `callId`, `voiceMessageId` |
//! | `calling.recording.ready` (VOICEMAIL) | a call recording becomes READY | the principals who took part in the Call (the initiator, a recipient who answered or connected): Calling authorizes every participant to retrieve it | `callId`, `recordingId` |
//!
//! Before N7 Calling notified no user of any of the three: missed calls are derived at read
//! time for the call history (and counted by `calling_missed_calls_total`); voicemail and
//! recording facts reach only the customer's own webhooks (`voicemail.*`,
//! `recording.ready`) and the Developer Platform — event deliveries to an application's
//! backend, never a notification to the person. Publishing them here never double-sends.
//!
//! * **Recipients** are CF Meet accounts only: a principal of a FIRST_PARTY tenant whose
//!   kind is `NATIVE_CF_USER` and whose reference is the account reference
//!   `cfmeet-user:<uuid>` (how the Meeting Service and the Communications Hub register
//!   CF Meet users with Platform Access, which Calling projects). An EXTERNAL tenant's
//!   principals are customer identities, never CF Meet accounts — even one registered with
//!   a `cfmeet-user:` reference — so nothing is written for an EXTERNAL tenant (its
//!   application is notified by its own webhooks). Inside a FIRST_PARTY tenant a principal
//!   without an account (a guest, a bot, a telephone endpoint) or an inactive one is
//!   recorded `SKIPPED` with the reason (`RECIPIENT_NOT_CF_MEET_ACCOUNT`,
//!   `RECIPIENT_INACTIVE`) and nothing is sent.
//! * **Enqueue** runs inside the state change's transaction, only while
//!   `CF_NOTIFICATIONS_BASE_URL` is configured ([`crate::calls::CallServiceConfig`]
//!   `notification_publications`); unset, nothing is written or sent (Calling behaves
//!   exactly as before). The envelope is built once with `packages/notification-client` and
//!   stored, ids only: the call id, the call kind, the message / recording id, the
//!   recipient's account reference and the Call's scope (tenant / application /
//!   environment); never a display name, number, label, object key, URL or media. Event
//!   id, route decision id, dedupe and idempotency keys are derived from the fact, its type
//!   and the recipient, so a re-post is byte-identical and the service de-duplicates on
//!   (producer, eventId). Missed calls carry a coalesce key per caller and 10-minute window
//!   (one grouped inbox item for repeated calls).
//! * **Relay** ([`relay_due`], every second): only while `CF_NOTIFICATIONS_BASE_URL` is
//!   configured, signed with Calling's service identity (`cfmeet.calling`) for audience
//!   `cfmeet.notifications` (`notifications-event.publish`). Batches ≤ 100. Before posting,
//!   a row whose recipient is no longer active (suspended, deactivated, tombstoned) or
//!   whose voicemail message / recording is no longer READY (deleted) is `SKIPPED`.
//!   `ACCEPTED`, `DUPLICATE` and `HELD` acknowledge the row (`ACKED`); a per-event
//!   `REJECTED` or a contract refusal of the batch (4xx other than 401 / 403 / 408 / 429)
//!   dead-letters it (`DEAD`); 5xx, 401 / 403 / 408 / 429 and transport failures retry with
//!   exponential backoff (2 s doubling, at most 5 min) until the row expires (`EXPIRED`).
//! * **Maintenance** ([`maintain`], every minute): expires overdue rows and purges settled
//!   rows after [`RETENTION_DAYS`].

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{
    envelope::Scope as EnvelopeScope, EnvelopeBuilder, EnvelopeError, Producer, RouteRef,
    ROUTING_PROTOCOL,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, Statement,
    TransactionTrait, Value as DbValue,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::calls;

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";
/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;
/// Settled rows are kept this long (diagnostics) before [`maintain`] purges them.
pub const RETENTION_DAYS: i32 = 30;
/// Calling has no legacy user-notification path, so it has no routing fence: every
/// decision is its own (one per event) at routing version 1.
pub const ROUTING_VERSION: i32 = 1;
/// Most recipients one fact is published to (a queue mailbox's agents, a group call).
pub const MAX_RECIPIENTS_PER_FACT: usize = 200;
/// Missed calls from the same caller inside one window share a coalesce key.
pub const MISSED_COALESCE_WINDOW_SECONDS: i64 = 600;
/// The account reference of a CF Meet user principal.
pub const ACCOUNT_REFERENCE_PREFIX: &str = "cfmeet-user:";

/// The three Calling notifications of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallNotice {
    /// #15: a recipient missed the Call (source: the participant).
    ParticipantMissed,
    /// #16: a voicemail message is ready (source: the message).
    VoicemailReady,
    /// #17: a call recording is ready (source: the recording).
    RecordingReady,
}

impl CallNotice {
    pub fn event_type(self) -> &'static str {
        match self {
            Self::ParticipantMissed => "calling.call.participant-missed",
            Self::VoicemailReady => "calling.voicemail.ready",
            Self::RecordingReady => "calling.recording.ready",
        }
    }

    /// Bounded metric label.
    fn label(self) -> &'static str {
        match self {
            Self::ParticipantMissed => "PARTICIPANT_MISSED",
            Self::VoicemailReady => "VOICEMAIL_READY",
            Self::RecordingReady => "RECORDING_READY",
        }
    }

    /// How long a publication may wait for the relay (and the envelope's `expiresAt`):
    /// a missed-call notice is time-sensitive; a ready artifact stays useful longer.
    pub fn lifetime(self) -> Duration {
        match self {
            Self::ParticipantMissed => Duration::hours(24),
            Self::VoicemailReady | Self::RecordingReady => Duration::hours(72),
        }
    }
}

fn derive(purpose: &str, notice: CallNotice, source: Uuid, recipient: Uuid) -> [u8; 16] {
    let digest = Sha256::digest(format!(
        "cfmeet.calling:notification:{purpose}:{}:{source}:{recipient}",
        notice.event_type()
    ));
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// The deterministic envelope event id (`evt_<32 hex>`) of one fact for one recipient.
pub fn event_id(notice: CallNotice, source: Uuid, recipient_principal: Uuid) -> String {
    format!(
        "evt_{}",
        hex::encode(derive("event", notice, source, recipient_principal))
    )
}

/// The deterministic route decision id (unique per producer at I1).
pub fn decision_id(notice: CallNotice, source: Uuid, recipient_principal: Uuid) -> Uuid {
    uuid::Builder::from_random_bytes(derive("route", notice, source, recipient_principal))
        .into_uuid()
}

/// Whether `reference` is a CF Meet account reference (`cfmeet-user:` + a lower-case,
/// hyphenated UUID).
pub fn account_reference(reference: &str) -> bool {
    reference
        .strip_prefix(ACCOUNT_REFERENCE_PREFIX)
        .and_then(|id| Uuid::parse_str(id).ok().map(|u| (id, u)))
        .is_some_and(|(id, u)| u.hyphenated().to_string() == id)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// One publication to write: a fact for one CF Meet account.
#[derive(Debug, Clone)]
pub struct Publication<'a> {
    pub notice: CallNotice,
    pub call: &'a calls::Model,
    /// The missed participant, the voicemail message or the recording.
    pub source_id: Uuid,
    pub recipient_principal_id: Uuid,
    /// `cfmeet-user:<uuid>`.
    pub recipient: &'a str,
}

/// The ids-only envelope of a publication (validated by the client builder).
pub fn envelope(
    p: &Publication<'_>,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<Value, EnvelopeError> {
    let event_type = p.notice.event_type();
    let event_id = event_id(p.notice, p.source_id, p.recipient_principal_id);
    let route = RouteRef {
        decision_id: decision_id(p.notice, p.source_id, p.recipient_principal_id),
        routing_version: ROUTING_VERSION,
        protocol: ROUTING_PROTOCOL,
    };
    let mut builder =
        EnvelopeBuilder::new(Producer::Calling, event_type, &rfc3339(occurred_at), route)?
            .event_id(event_id.clone())
            .correlation_id(format!("corr_{}", p.call.id.simple()))
            .scope(EnvelopeScope {
                tenant_id: p.call.tenant_id,
                application_id: p.call.application_id,
                environment_id: p.call.environment_id,
            })
            .subject(p.call.id, None)
            .recipient(p.recipient.to_owned())
            .dedupe_key(format!(
                "{event_type}:{}:{}",
                p.source_id, p.recipient_principal_id
            ))
            .idempotency_key(event_id)
            .expires_at(rfc3339(expires_at))
            .data("callId", p.call.id.to_string());
    builder = match p.notice {
        CallNotice::ParticipantMissed => builder
            .data("callKind", p.call.call_kind.as_str())
            // Repeated calls from one caller group into one inbox item (per recipient).
            .coalesce_key(format!(
                "calling-missed:{}:{}",
                p.call.initiator_principal_id.simple(),
                occurred_at
                    .timestamp()
                    .div_euclid(MISSED_COALESCE_WINDOW_SECONDS)
            )),
        CallNotice::VoicemailReady => builder.data("voiceMessageId", p.source_id.to_string()),
        CallNotice::RecordingReady => builder.data("recordingId", p.source_id.to_string()),
    };
    let built = builder.build()?;
    serde_json::to_value(built).map_err(|_| EnvelopeError::Invalid("envelope"))
}

#[derive(Debug)]
struct Recipient {
    kind: String,
    reference: String,
    status: String,
}

async fn first_party<C: ConnectionTrait>(db: &C, tenant: Uuid) -> Result<bool, DbErr> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM platform_tenants WHERE id = $1 AND classification = 'FIRST_PARTY'",
            vec![tenant.into()],
        ))
        .await?
        .is_some())
}

/// Writes the publications of one fact (`(source, recipient principal)` pairs) inside the
/// caller's transaction. Idempotent (one row per fact, type and recipient). The CALLER
/// checks that publications are configured. Returns how many rows were written.
pub async fn enqueue(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    notice: CallNotice,
    facts: Vec<(Uuid, Uuid)>,
) -> Result<usize, DbErr> {
    let mut facts = facts;
    facts.sort_unstable();
    facts.dedup();
    if facts.is_empty() {
        return Ok(0);
    }
    let kind = notice.label();
    if !first_party(txn, call.tenant_id).await? {
        crate::metrics::add(
            "calling_notification_publications_total",
            &[("kind", kind), ("result", "NOT_FIRST_PARTY")],
            facts.len() as u64,
        );
        return Ok(0);
    }
    if facts.len() > MAX_RECIPIENTS_PER_FACT {
        crate::metrics::add(
            "calling_notification_publications_total",
            &[("kind", kind), ("result", "OVER_RECIPIENT_LIMIT")],
            (facts.len() - MAX_RECIPIENTS_PER_FACT) as u64,
        );
        facts.truncate(MAX_RECIPIENTS_PER_FACT);
    }
    let ids: Vec<Uuid> = facts.iter().map(|(_, p)| *p).collect();
    let recipients: HashMap<Uuid, Recipient> = txn
        .query_all(stmt(
            "SELECT id, kind, external_reference, status FROM platform_principals \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = ANY($4)",
            vec![
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                ids.into(),
            ],
        ))
        .await?
        .into_iter()
        .map(|r| {
            Ok((
                r.try_get::<Uuid>("", "id")?,
                Recipient {
                    kind: r.try_get("", "kind")?,
                    reference: r.try_get("", "external_reference")?,
                    status: r.try_get("", "status")?,
                },
            ))
        })
        .collect::<Result<_, DbErr>>()?;
    let now = Utc::now();
    let expires = now + notice.lifetime();
    let mut written = 0;
    for (source_id, principal) in facts {
        // Out of the Call's scope (never expected): nothing to write, the FK would refuse.
        let Some(r) = recipients.get(&principal) else {
            continue;
        };
        let publication = Publication {
            notice,
            call,
            source_id,
            recipient_principal_id: principal,
            recipient: &r.reference,
        };
        let (envelope, state, error) =
            if r.kind != "NATIVE_CF_USER" || !account_reference(&r.reference) {
                (None, "SKIPPED", Some("RECIPIENT_NOT_CF_MEET_ACCOUNT"))
            } else if r.status != "ACTIVE" {
                (None, "SKIPPED", Some("RECIPIENT_INACTIVE"))
            } else {
                match envelope(&publication, now, expires) {
                    Ok(e) => (Some(e), "PENDING", None),
                    // A contract violation is a bug: dead-lettered, never sent.
                    Err(_) => (None, "DEAD", Some("ENVELOPE_INVALID")),
                }
            };
        let n = txn
            .execute(stmt(
                "INSERT INTO calling_notification_publications (id, event_id, event_type, tenant_id, \
                   application_id, environment_id, call_id, source_id, recipient_principal_id, envelope, \
                   state, last_error, next_attempt_at, occurred_at, created_at, completed_at, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13, $13, \
                   CASE WHEN $11 = 'PENDING' THEN NULL ELSE $13 END, $14) \
                 ON CONFLICT DO NOTHING",
                vec![
                    Uuid::new_v4().into(),
                    event_id(notice, source_id, principal).into(),
                    notice.event_type().into(),
                    call.tenant_id.into(),
                    call.application_id.into(),
                    call.environment_id.into(),
                    call.id.into(),
                    source_id.into(),
                    principal.into(),
                    envelope.into(),
                    state.into(),
                    error.map(str::to_owned).into(),
                    now.into(),
                    expires.into(),
                ],
            ))
            .await?
            .rows_affected();
        if n > 0 {
            written += 1;
            let result = if state == "PENDING" {
                "ENQUEUED"
            } else {
                state
            };
            crate::metrics::inc(
                "calling_notification_publications_total",
                &[("kind", kind), ("result", result)],
            );
            if let Some(reason) = error {
                tracing::info!(
                    event_type = notice.event_type(),
                    reason,
                    "notification publication not sent"
                );
            }
        }
    }
    Ok(written)
}

/// `calling.call.participant-missed` for every recipient of the terminal `call` whose
/// outcome is MISSED (the history derivation of ADR 0025, the same set
/// `calling_missed_calls_total` counts).
pub async fn enqueue_missed(
    txn: &DatabaseTransaction,
    call: &calls::Model,
) -> Result<usize, DbErr> {
    let facts = txn
        .query_all(stmt(
            "SELECT p.id, p.principal_id FROM call_participants p WHERE p.call_id = $1 \
               AND p.role = 'RECIPIENT' \
               AND EXISTS (SELECT 1 FROM call_invitations i WHERE i.participant_id = p.id) \
               AND NOT EXISTS (SELECT 1 FROM call_invitations i WHERE i.participant_id = p.id \
                     AND i.status IN ('ACCEPTED', 'DECLINED', 'ANSWERED_ELSEWHERE')) \
               AND NOT EXISTS (SELECT 1 FROM call_sessions s \
                     JOIN call_session_intervals v ON v.session_id = s.id \
                    WHERE s.participant_id = p.id) \
             ORDER BY p.created_at, p.id",
            vec![call.id.into()],
        ))
        .await?
        .into_iter()
        .map(|r| Ok((r.try_get_by_index(0)?, r.try_get_by_index(1)?)))
        .collect::<Result<Vec<(Uuid, Uuid)>, DbErr>>()?;
    enqueue(txn, call, CallNotice::ParticipantMissed, facts).await
}

/// `calling.voicemail.ready` for each owner of the message's mailbox.
pub async fn enqueue_voicemail_ready(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    message_id: Uuid,
    owners: Vec<Uuid>,
) -> Result<usize, DbErr> {
    let facts = owners.into_iter().map(|o| (message_id, o)).collect();
    enqueue(txn, call, CallNotice::VoicemailReady, facts).await
}

/// `calling.recording.ready` for each principal who took part in the Call: its initiator
/// and every recipient who answered or connected (each may retrieve the recording).
pub async fn enqueue_recording_ready(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    recording_id: Uuid,
) -> Result<usize, DbErr> {
    let facts = txn
        .query_all(stmt(
            "SELECT DISTINCT p.principal_id FROM call_participants p WHERE p.call_id = $1 \
               AND (p.role = 'INITIATOR' \
                 OR EXISTS (SELECT 1 FROM call_invitations i WHERE i.participant_id = p.id \
                      AND i.status = 'ACCEPTED') \
                 OR EXISTS (SELECT 1 FROM call_sessions s \
                      JOIN call_session_intervals v ON v.session_id = s.id \
                     WHERE s.participant_id = p.id))",
            vec![call.id.into()],
        ))
        .await?
        .into_iter()
        .map(|r| Ok((recording_id, r.try_get_by_index(0)?)))
        .collect::<Result<Vec<(Uuid, Uuid)>, DbErr>>()?;
    enqueue(txn, call, CallNotice::RecordingReady, facts).await
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
/// `notifications-event.publish`), signed with Calling's own service identity.
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
            Ok(r) if r.status() == 200 => r
                .json::<IngestAnswer>()
                .map(|a| a.results)
                .map_err(|_| PublishFailure::Unavailable),
            Ok(r) if r.status() >= 500 || matches!(r.status(), 401 | 403 | 408 | 429) => {
                Err(PublishFailure::Unavailable)
            }
            Ok(_) => Err(PublishFailure::Rejected),
            Err(InternalCallError::UnknownTarget(_)) => Err(PublishFailure::NotConfigured),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

// -------------------------------------------------------------------------------- relay

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

/// Exponential backoff after `attempts` failed attempts: 2 s, 4 s, … at most 5 minutes.
pub fn backoff_seconds(attempts: i32) -> i64 {
    (1i64 << attempts.clamp(0, 9)).min(300)
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

#[derive(Debug)]
struct Due {
    id: Uuid,
    event_id: String,
    envelope: Value,
    attempts: i32,
    /// Why Calling no longer publishes this row, if it does not.
    suppressed: Option<String>,
}

async fn settle(
    txn: &DatabaseTransaction,
    id: Uuid,
    state: &str,
    outcome: Option<&str>,
    error: Option<String>,
) -> Result<(), DbErr> {
    txn.execute(stmt(
        "UPDATE calling_notification_publications SET state = $2, outcome = $3, last_error = $4, \
           attempts = attempts + CASE WHEN $2 IN ('ACKED', 'DEAD') THEN 1 ELSE 0 END, \
           completed_at = now() \
         WHERE id = $1 AND state = 'PENDING'",
        vec![
            id.into(),
            state.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn retry(
    txn: &DatabaseTransaction,
    row: &Due,
    error: &str,
    counted: bool,
) -> Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let delay = if counted {
        backoff_seconds(attempts)
    } else {
        60
    };
    txn.execute(stmt(
        "UPDATE calling_notification_publications SET attempts = $2, last_error = $3, \
           next_attempt_at = now() + make_interval(secs => $4) WHERE id = $1",
        vec![
            row.id.into(),
            attempts.into(),
            error.into(),
            (delay as f64).into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The due rows of one batch, locked, with Calling's current suppression of each: the
/// recipient is no longer the active account it was (suspended, deactivated, tombstoned),
/// or the voicemail message / recording is no longer READY (deleted meanwhile).
const DUE: &str = "SELECT p.id, p.event_id, p.envelope, p.attempts, CASE \
      WHEN NOT EXISTS (SELECT 1 FROM platform_principals r WHERE r.id = p.recipient_principal_id \
        AND r.status = 'ACTIVE' AND r.external_reference = p.envelope -> 'recipients' -> 0 ->> 'ref') \
        THEN 'RECIPIENT_INACTIVE' \
      WHEN p.event_type = 'calling.voicemail.ready' AND NOT EXISTS (SELECT 1 FROM voicemail_messages m \
        WHERE m.id = p.source_id AND m.state = 'READY') THEN 'VOICEMAIL_NOT_READY' \
      WHEN p.event_type = 'calling.recording.ready' AND NOT EXISTS (SELECT 1 FROM call_recordings r \
        WHERE r.id = p.source_id AND r.state = 'READY') THEN 'RECORDING_NOT_READY' \
    END AS suppressed \
  FROM calling_notification_publications p \
  WHERE p.state = 'PENDING' AND p.next_attempt_at <= now() AND p.expires_at > now() \
  ORDER BY p.next_attempt_at, p.created_at LIMIT $1 FOR UPDATE OF p SKIP LOCKED";

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the byte-identical re-post is answered `DUPLICATE` (acknowledged). Expired rows end
/// `EXPIRED` first and are never posted.
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<PublicationReport, DbErr> {
    let mut report = PublicationReport {
        expired: expire_overdue(db).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = db.begin().await?;
        let due = txn
            .query_all(stmt(DUE, vec![(batch as i64).into()]))
            .await?
            .into_iter()
            .map(|r| {
                Ok(Due {
                    id: r.try_get("", "id")?,
                    event_id: r.try_get("", "event_id")?,
                    envelope: r.try_get("", "envelope")?,
                    attempts: r.try_get("", "attempts")?,
                    suppressed: r.try_get("", "suppressed")?,
                })
            })
            .collect::<Result<Vec<Due>, DbErr>>()?;
        if due.is_empty() {
            txn.commit().await?;
            break;
        }
        remaining = remaining.saturating_sub(due.len());
        let full = due.len() == batch;
        let mut ready = Vec::with_capacity(due.len());
        for row in due {
            match row.suppressed.clone() {
                Some(reason) => {
                    settle(&txn, row.id, "SKIPPED", None, Some(reason)).await?;
                    report.skipped += 1;
                }
                None => ready.push(row),
            }
        }
        let mut stop = false;
        if !ready.is_empty() {
            let events: Vec<Value> = ready.iter().map(|r| r.envelope.clone()).collect();
            match sink.ingest(&events).await {
                Ok(results) => {
                    let by_id: HashMap<&str, &IngestResult> =
                        results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                    for row in &ready {
                        match by_id.get(row.event_id.as_str()) {
                            Some(r)
                                if matches!(
                                    r.outcome.as_str(),
                                    "ACCEPTED" | "DUPLICATE" | "HELD"
                                ) =>
                            {
                                settle(&txn, row.id, "ACKED", Some(&r.outcome), None).await?;
                                report.acked += 1;
                            }
                            Some(r) if r.outcome == "REJECTED" => {
                                let reason = reason_code(r.reason.as_deref(), "REJECTED");
                                settle(&txn, row.id, "DEAD", Some("REJECTED"), Some(reason))
                                    .await?;
                                report.dead += 1;
                            }
                            // No (or an unexpected) answer for this event: try again.
                            _ => {
                                retry(&txn, row, "NO_RESULT", true).await?;
                                report.retried += 1;
                            }
                        }
                    }
                }
                Err(f @ PublishFailure::Rejected) => {
                    for row in &ready {
                        settle(&txn, row.id, "DEAD", None, Some(f.code().into())).await?;
                        report.dead += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::Unavailable) => {
                    for row in &ready {
                        retry(&txn, row, f.code(), true).await?;
                        report.retried += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::NotConfigured) => {
                    for row in &ready {
                        retry(&txn, row, f.code(), false).await?;
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
    if report != PublicationReport::default() {
        for (result, n) in [
            ("ACKED", report.acked),
            ("RETRIED", report.retried),
            ("DEAD", report.dead),
            ("SKIPPED", report.skipped),
            ("EXPIRED", report.expired as usize),
        ] {
            crate::metrics::add(
                "calling_notification_relay_total",
                &[("result", result)],
                n as u64,
            );
        }
        tracing::info!(
            acked = report.acked,
            retried = report.retried,
            dead = report.dead,
            skipped = report.skipped,
            expired = report.expired,
            "notification publications relayed"
        );
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted). Returns how many.
async fn expire_overdue(db: &DatabaseConnection) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE calling_notification_publications SET state = 'EXPIRED', last_error = 'EXPIRED', \
               completed_at = now() WHERE state = 'PENDING' AND expires_at <= now()",
            vec![],
        ))
        .await?
        .rows_affected())
}

/// Retention, whether or not the relay runs: expires overdue PENDING rows and deletes at
/// most `batch` settled rows older than [`RETENTION_DAYS`]. Returns (expired, purged).
pub async fn maintain(db: &DatabaseConnection, batch: i64) -> Result<(u64, u64), DbErr> {
    let expired = expire_overdue(db).await?;
    let purged = db
        .execute(stmt(
            "DELETE FROM calling_notification_publications WHERE id IN (SELECT id \
               FROM calling_notification_publications WHERE state <> 'PENDING' \
               AND completed_at < now() - make_interval(days => $1) ORDER BY completed_at LIMIT $2)",
            vec![RETENTION_DAYS.into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((expired, purged))
}

/// The relay loop of a Calling instance (every second; maintenance every minute). Started
/// only while `CF_NOTIFICATIONS_BASE_URL` is configured.
pub async fn run(db: DatabaseConnection, sink: Arc<dyn NotificationEventSink>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut ticks: u64 = 0;
    loop {
        interval.tick().await;
        if let Err(error) = relay_due(&db, sink.as_ref(), 500).await {
            tracing::warn!(%error, "notification publication relay pass failed; retried");
        }
        if ticks.is_multiple_of(60) {
            if let Err(error) = maintain(&db, 1000).await {
                tracing::warn!(%error, "notification publication maintenance failed; retried");
            }
        }
        ticks = ticks.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic_per_fact_type_and_recipient() {
        let (s, r) = (Uuid::new_v4(), Uuid::new_v4());
        let a = event_id(CallNotice::VoicemailReady, s, r);
        assert_eq!(a, event_id(CallNotice::VoicemailReady, s, r));
        assert_ne!(a, event_id(CallNotice::RecordingReady, s, r));
        assert_ne!(a, event_id(CallNotice::VoicemailReady, s, Uuid::new_v4()));
        assert_ne!(a, event_id(CallNotice::VoicemailReady, Uuid::new_v4(), r));
        assert!(a.len() == 36 && a.starts_with("evt_"));
        assert!(a[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(
            decision_id(CallNotice::ParticipantMissed, s, r),
            decision_id(CallNotice::ParticipantMissed, s, r)
        );
        assert_ne!(
            decision_id(CallNotice::ParticipantMissed, s, r),
            decision_id(CallNotice::VoicemailReady, s, r)
        );
    }

    #[test]
    fn only_cf_meet_account_references_are_recipients() {
        let u = Uuid::new_v4();
        assert!(account_reference(&format!("cfmeet-user:{u}")));
        for bad in [
            format!("cfmeet-user:{}", u.simple()),
            format!("cfmeet-user:{}", u.hyphenated().to_string().to_uppercase()),
            format!("user:{u}"),
            u.to_string(),
            "cfmeet-user:".into(),
            format!("cfmeet-user:{u}x"),
        ] {
            assert!(!account_reference(&bad), "{bad}");
        }
    }

    #[test]
    fn service_reasons_are_recorded_only_in_their_machine_shape() {
        assert_eq!(
            reason_code(Some("DIGEST_MISMATCH"), "REJECTED"),
            "DIGEST_MISMATCH"
        );
        for bad in ["", "x", "digest mismatch", "A\u{7}B", &"A".repeat(65)] {
            assert_eq!(reason_code(Some(bad), "REJECTED"), "REJECTED", "{bad:?}");
        }
        assert_eq!(reason_code(None, "REJECTED"), "REJECTED");
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff_seconds(1), 2);
        assert!(backoff_seconds(5) > backoff_seconds(4));
        assert_eq!(backoff_seconds(40), 300);
    }
}
