//! The Hub's publications to the Notification Service (roadmap N7, Document 4 §5.1, §9.5):
//! the producer outbox `communication_notification_publications` (fifth migration
//! history) and its relay to I1 (`POST /internal/v1/notification-events`,
//! `ingestNotificationEvents`).
//!
//! Catalogue entries #11 and #12 (`cfmeet.communications`, category `CONTACT_REQUESTS`,
//! subject `CONNECTION`, data `actorRef` + `connectionId`):
//!
//! | Event | When | Recipient | `actorRef` |
//! |---|---|---|---|
//! | `communications.connection.requested` | a new contact request is created ([`State::send_request`]) | the addressee | the requester |
//! | `communications.connection.connected` | the addressee accepts it ([`State::decide_request`]) | the requester | the addressee (who accepted) |
//!
//! Before N7 the Hub delivered no user notification for either event (its own outbox
//! only fans relationship events out to Calling and Messaging, which apply them and notify
//! nobody), so publishing them here never double-sends.
//!
//! * **Enqueue** ([`enqueue`]) runs inside the relationship change's transaction, after the
//!   Hub's own checks (blocks either way, the addressee's contact-request privacy, the
//!   accept-time account and domain-block checks) refused anything they refuse: a
//!   suppressed request writes nothing. Only while `CF_NOTIFICATIONS_BASE_URL` is
//!   configured; unset, nothing is written or sent (the Hub behaves exactly as before).
//!   Recipients and the actor are CF Meet accounts: a Hub profile's `account_reference`
//!   (`cfmeet-user:<uuid>`, the mapping Account owns and the Hub stores with every profile).
//!   A person without one is recorded `SKIPPED` with the reason
//!   (`ACTOR_NOT_CF_MEET_ACCOUNT`, `RECIPIENT_NOT_CF_MEET_ACCOUNT`) and nothing is sent.
//!   The envelope is built once with `packages/notification-client` and stored, ids only:
//!   the connection id and version, the actor's account reference, the recipient's account
//!   reference and the row's scope (tenant / application / environment); never a name,
//!   public id, privacy value or message. Event id, route decision id, dedupe and
//!   idempotency keys are derived from the contact request id and the event type, so a
//!   re-post is byte-identical and the service de-duplicates on (producer, eventId).
//! * **Relay** ([`relay_due`], every second in the Hub's worker): runs only when
//!   `CF_NOTIFICATIONS_BASE_URL` is configured, signed with the Hub's service identity (`cfmeet.communications`) for
//!   audience `cfmeet.notifications` (`notifications-event.publish`). Batches ≤ 100. Before
//!   posting, a row whose pair is now blocked (either way), whose request is no longer
//!   pending (`requested`) or whose connection is no longer connected (`connected`) is
//!   `SKIPPED` (the service re-checks blocks and privacy through X4 anyway). `ACCEPTED`,
//!   `DUPLICATE` and `HELD` acknowledge the row (`ACKED`); a per-event `REJECTED` or a
//!   contract refusal of the batch (4xx other than 401 / 403 / 408 / 429) dead-letters it
//!   (`DEAD`); 5xx, 401 / 403 / 408 / 429 and transport failures retry with exponential
//!   backoff (2 s doubling, at most 5 min) until the row expires ([`LIFETIME`], `EXPIRED`).
//! * **Maintenance** ([`maintain`], with the Hub's retention sweep every minute): expires overdue
//!   rows and purges settled rows after [`RETENTION_DAYS`].

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
use sea_orm::DatabaseTransaction;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::Result,
    model::Profile,
    preferences::valid_reference,
    store::{execute, rows},
    State,
};

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";
/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;
/// How long a publication may wait for the relay (and the envelope's `expiresAt`).
pub const LIFETIME: Duration = Duration::hours(24);
/// Settled rows are kept this long (diagnostics) before [`maintain`] purges them.
pub const RETENTION_DAYS: i32 = 30;
/// The route of a producer without a routing fence: the Hub has no legacy notification
/// path, so every decision is its own (one per event) at routing version 1.
pub const ROUTING_VERSION: i32 = 1;

/// The two contact-request notifications of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionNotice {
    /// #11: a new contact request, to the addressee.
    Requested,
    /// #12: the request was accepted, to the requester.
    Connected,
}

impl ConnectionNotice {
    pub fn event_type(self) -> &'static str {
        match self {
            Self::Requested => "communications.connection.requested",
            Self::Connected => "communications.connection.connected",
        }
    }
}

fn derive(purpose: &str, event_type: &str, request_id: Uuid) -> [u8; 16] {
    let digest = Sha256::digest(format!(
        "cfmeet.communications:notification:{purpose}:{event_type}:{request_id}"
    ));
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// The deterministic envelope event id (`evt_<32 hex>`) of a request's notification.
pub fn event_id(notice: ConnectionNotice, request_id: Uuid) -> String {
    format!(
        "evt_{}",
        hex::encode(derive("event", notice.event_type(), request_id))
    )
}

/// The deterministic route decision id (unique per producer at I1).
pub fn decision_id(notice: ConnectionNotice, request_id: Uuid) -> Uuid {
    uuid::Builder::from_random_bytes(derive("route", notice.event_type(), request_id)).into_uuid()
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// One publication to write with a relationship change.
#[derive(Debug, Clone, Copy)]
pub struct Notice<'a> {
    pub notice: ConnectionNotice,
    pub request_id: Uuid,
    pub connection_id: Uuid,
    /// The connection's version after the change.
    pub connection_version: i64,
    pub actor: &'a Profile,
    pub recipient: &'a Profile,
}

/// The ids-only envelope of a publication (validated by the client builder).
pub fn envelope(
    n: &Notice<'_>,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> std::result::Result<Value, EnvelopeError> {
    let event_type = n.notice.event_type();
    let event_id = event_id(n.notice, n.request_id);
    let route = RouteRef {
        decision_id: decision_id(n.notice, n.request_id),
        routing_version: ROUTING_VERSION,
        protocol: ROUTING_PROTOCOL,
    };
    let built = EnvelopeBuilder::new(
        Producer::Communications,
        event_type,
        &rfc3339(occurred_at),
        route,
    )?
    .event_id(event_id.clone())
    .correlation_id(format!("corr_{}", n.request_id.simple()))
    .scope(EnvelopeScope {
        tenant_id: n.actor.tenant_id,
        application_id: n.actor.application_id,
        environment_id: n.actor.environment_id,
    })
    .subject(n.connection_id, Some(n.connection_version))
    .recipient(n.recipient.account_reference.clone())
    .dedupe_key(format!("{event_type}:{}", n.request_id))
    .idempotency_key(event_id)
    .expires_at(rfc3339(expires_at))
    .data("actorRef", n.actor.account_reference.clone())
    .data("connectionId", n.connection_id.to_string())
    .build()?;
    serde_json::to_value(built).map_err(|_| EnvelopeError::Invalid("envelope"))
}

/// Writes the publication of `n` inside the caller's relationship transaction, only while
/// the Notification Service is configured. Idempotent (one row per request and type).
/// Returns whether a row was written.
pub async fn enqueue(state: &State, txn: &DatabaseTransaction, n: Notice<'_>) -> Result<bool> {
    if !state.config.notifications_configured {
        return Ok(false);
    }
    let now = Utc::now();
    let expires = now + LIFETIME;
    let (envelope, row_state, error) = if !valid_reference(&n.actor.account_reference) {
        (None, "SKIPPED", Some("ACTOR_NOT_CF_MEET_ACCOUNT"))
    } else if !valid_reference(&n.recipient.account_reference) {
        (None, "SKIPPED", Some("RECIPIENT_NOT_CF_MEET_ACCOUNT"))
    } else {
        match envelope(&n, now, expires) {
            Ok(e) => (Some(e), "PENDING", None),
            // A contract violation is a bug: dead-lettered, never sent.
            Err(_) => (None, "DEAD", Some("ENVELOPE_INVALID")),
        }
    };
    let s = n.actor.scope();
    let written = execute(
        txn,
        "INSERT INTO communication_notification_publications(id,event_id,event_type,tenant_id,application_id,environment_id,\
           connection_id,request_id,actor_profile_id,recipient_profile_id,envelope,state,last_error,next_attempt_at,occurred_at,\
           created_at,completed_at,expires_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$14,$14,CASE WHEN $12='PENDING' THEN NULL ELSE $14 END,$15) \
         ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            event_id(n.notice, n.request_id).into(),
            n.notice.event_type().into(),
            s.tenant_id.into(),
            s.application_id.into(),
            s.environment_id.into(),
            n.connection_id.into(),
            n.request_id.into(),
            n.actor.id.into(),
            n.recipient.id.into(),
            envelope.into(),
            row_state.into(),
            error.map(str::to_owned).into(),
            now.into(),
            expires.into(),
        ],
    )
    .await?;
    if let Some(reason) = error {
        tracing::info!(
            event_type = n.notice.event_type(),
            reason,
            "notification publication not sent"
        );
    }
    Ok(written > 0)
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
    async fn ingest(
        &self,
        events: &[Value],
    ) -> std::result::Result<Vec<IngestResult>, PublishFailure>;
}

/// The service-authenticated I1 of the Notification Service (`cfmeet.notifications`,
/// `notifications-event.publish`), signed with the Hub's own service identity.
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
    async fn ingest(
        &self,
        events: &[Value],
    ) -> std::result::Result<Vec<IngestResult>, PublishFailure> {
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

#[derive(Debug, Deserialize)]
struct Due {
    id: Uuid,
    event_id: String,
    envelope: Value,
    attempts: i32,
    /// Why the Hub no longer publishes this row, if it does not.
    suppressed: Option<String>,
}

async fn settle(
    txn: &DatabaseTransaction,
    id: Uuid,
    state: &str,
    outcome: Option<&str>,
    error: Option<String>,
) -> Result<()> {
    execute(
        txn,
        "UPDATE communication_notification_publications SET state=$2,outcome=$3,last_error=$4,\
           attempts=attempts+CASE WHEN $2 IN ('ACKED','DEAD') THEN 1 ELSE 0 END,completed_at=now() \
         WHERE id=$1 AND state='PENDING'",
        vec![
            id.into(),
            state.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn retry(txn: &DatabaseTransaction, row: &Due, error: &str, counted: bool) -> Result<()> {
    let attempts = row.attempts + i32::from(counted);
    let delay = if counted {
        backoff_seconds(attempts)
    } else {
        60
    };
    execute(
        txn,
        "UPDATE communication_notification_publications SET attempts=$2,last_error=$3,\
           next_attempt_at=now()+make_interval(secs=>$4) WHERE id=$1",
        vec![
            row.id.into(),
            attempts.into(),
            error.into(),
            (delay as f64).into(),
        ],
    )
    .await?;
    Ok(())
}

/// The due rows of one batch, locked, with the Hub's current suppression of each: an
/// active block between the two people (either way), a request no longer pending, or a
/// connection no longer connected.
const DUE: &str = "SELECT jsonb_build_object('id',p.id,'event_id',p.event_id,'envelope',p.envelope,'attempts',p.attempts,\
    'suppressed',CASE \
      WHEN EXISTS(SELECT 1 FROM communication_block_bridge b WHERE b.active AND \
        ((b.blocker_profile_id=p.actor_profile_id AND b.blocked_profile_id=p.recipient_profile_id) \
         OR (b.blocker_profile_id=p.recipient_profile_id AND b.blocked_profile_id=p.actor_profile_id))) THEN 'BLOCKED' \
      WHEN p.event_type='communications.connection.requested' AND NOT EXISTS(SELECT 1 FROM communication_contact_requests r \
        WHERE r.id=p.request_id AND r.state='PENDING' AND r.expires_at>now()) THEN 'REQUEST_NOT_PENDING' \
      WHEN p.event_type='communications.connection.connected' AND NOT EXISTS(SELECT 1 FROM communication_connections c \
        WHERE c.id=p.connection_id AND c.state='CONNECTED') THEN 'NOT_CONNECTED' \
    END) doc \
  FROM communication_notification_publications p \
  WHERE p.state='PENDING' AND p.next_attempt_at<=now() AND p.expires_at>now() \
  ORDER BY p.next_attempt_at, p.created_at LIMIT $1 FOR UPDATE OF p SKIP LOCKED";

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the byte-identical re-post is answered `DUPLICATE` (acknowledged). Expired rows end
/// `EXPIRED` first and are never posted.
pub async fn relay_due(
    state: &State,
    sink: &dyn NotificationEventSink,
    limit: usize,
) -> Result<PublicationReport> {
    let mut report = PublicationReport {
        expired: expire_overdue(state).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = state.store.begin().await?;
        let due: Vec<Due> = rows(&txn, DUE, vec![(batch as i64).into()]).await?;
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
async fn expire_overdue(state: &State) -> Result<u64> {
    execute(
        &state.store.db,
        "UPDATE communication_notification_publications SET state='EXPIRED',last_error='EXPIRED',completed_at=now() \
          WHERE state='PENDING' AND expires_at<=now()",
        vec![],
    )
    .await
}

/// Retention, whether or not the relay runs: expires overdue PENDING rows and deletes at
/// most `batch` settled rows older than [`RETENTION_DAYS`]. Returns (expired, purged).
pub async fn maintain(state: &State, batch: i64) -> Result<(u64, u64)> {
    let expired = expire_overdue(state).await?;
    let purged = execute(
        &state.store.db,
        "DELETE FROM communication_notification_publications WHERE id IN (SELECT id FROM communication_notification_publications \
          WHERE state<>'PENDING' AND completed_at<now()-make_interval(days=>$1) ORDER BY completed_at LIMIT $2)",
        vec![RETENTION_DAYS.into(), batch.into()],
    )
    .await?;
    Ok((expired, purged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic_per_request_and_type() {
        let r = Uuid::new_v4();
        let a = event_id(ConnectionNotice::Requested, r);
        assert_eq!(a, event_id(ConnectionNotice::Requested, r));
        assert_ne!(a, event_id(ConnectionNotice::Connected, r));
        assert_ne!(a, event_id(ConnectionNotice::Requested, Uuid::new_v4()));
        assert!(a.len() == 36 && a.starts_with("evt_"));
        assert!(a[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(
            decision_id(ConnectionNotice::Connected, r),
            decision_id(ConnectionNotice::Connected, r)
        );
        assert_ne!(
            decision_id(ConnectionNotice::Requested, r),
            decision_id(ConnectionNotice::Connected, r)
        );
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
