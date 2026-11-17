//! Messaging's publications to the Notification Service (roadmap N7, Document 4 §5.1,
//! §9.3, §9.5): the producer outbox `messaging_notification_publications` (routing fence
//! history, second migration) and its relay to I1
//! (`POST /internal/v1/notification-events`, `ingestNotificationEvents`).
//!
//! * **Enqueue** ([`enqueue`]) happens inside the push fan-out transaction that records
//!   the message's routing decision ([`crate::realtime::push::fanout_jobs`]): in `SHADOW` an
//!   `OLD` decision writes the legacy push jobs AND one shadow publication (posted with
//!   `shadow: true`; the service records it and never delivers it, whatever its own
//!   mode); a `NEW` decision (`FENCED_NEW`) writes one publication and no push jobs. One
//!   row per outbox event (the routing key), never per device; nothing in `LEGACY_ONLY`
//!   or `FENCED_OLD`.
//! * **Relay** ([`relay_due`], [`run`]): started only when `CF_NOTIFICATIONS_BASE_URL` is
//!   configured (otherwise rows wait PENDING until they expire and nothing is sent). It
//!   builds each row's `cfmeet.notifications.event` envelope once with
//!   `packages/notification-client` and stores it, so a re-post is byte-identical (the
//!   service de-duplicates on (producer, eventId) plus digest). The envelope is ids only:
//!   the conversation, its event cursor, kind and security profile, the routing decision,
//!   and the recipients' CF Meet account references (the X3 entitlement and account
//!   mapping, [`crate::notifications`]); never content, ciphertext or keys (E2EE). The
//!   sender is not part of the contract's data: the service learns it from X3.
//!   Batches of ≤ 100 rows are posted with a service token for `cfmeet.notifications`
//!   (`notifications-event.publish`). `ACCEPTED`, `DUPLICATE` (ACCEPTED semantics) and
//!   `HELD` (stored by the service) acknowledge the row (`ACKED`; for a `NEW` decision
//!   `ns_acked_at` records the responsibility boundary, §9.5); a per-event `REJECTED`
//!   or a contract refusal of the batch (4xx other than 401 / 403 / 408 / 429)
//!   dead-letters it (`DEAD`, counted); 5xx, 401 / 403 / 408 / 429 and transport failures
//!   retry with backoff until the row expires.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{
    envelope::{Scope, MAX_RECIPIENTS},
    EnvelopeBuilder, EnvelopeError, Producer, RouteRef,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db,
    error::Result,
    events::outbox::OutboxEvent,
    notification_fence::{self as fence, Decision},
    realtime::push,
    state::AppState,
};

/// The largest audience (push candidates of one event) the fence may route `NEW`: every
/// envelope for this many worst-case account refs fits the I1 limits (4096 bytes,
/// [`MAX_RECIPIENTS`]); a larger audience is decided `OLD` (reason `RECIPIENT_LIMIT`) and
/// keeps the legacy push path, so no member of a large group loses a wake-up when the
/// category moves to `FENCED_NEW`. Entitled recipients are a subset of the candidates
/// and account refs are deduplicated, so the envelope never holds more refs than this.
pub const NEW_ROUTE_MAX_CANDIDATES: usize = 40;
const _: () = assert!(NEW_ROUTE_MAX_CANDIDATES <= MAX_RECIPIENTS);

/// The catalogue type of a message wake-up (#13).
pub const EVENT_TYPE: &str = "messaging.message.created";

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";

/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;

/// How long a publication may wait for the relay (as a push job).
pub const LIFETIME: Duration = Duration::hours(24);

/// Counter of relay outcomes (`outcome`: acked / retried / dead / skipped / expired;
/// `kind`: shadow / publication).
pub const METRIC: &str = "messaging_notification_publications_total";

/// Writes the publication of one routed message wake-up inside the caller's fan-out
/// transaction (idempotent: one row per event and per decision). `shadow`: the SHADOW
/// copy of an `OLD` decision. `true` when a row was written.
pub async fn enqueue(
    txn: &DatabaseTransaction,
    event: &OutboxEvent,
    conversation_id: Uuid,
    event_sequence: i64,
    decision: &Decision,
    shadow: bool,
    now: DateTime<Utc>,
) -> std::result::Result<bool, DbErr> {
    let correlation = cf_service_auth::CorrelationId::parse(&event.correlation_id)
        .map(|c| c.as_str().to_owned())
        .unwrap_or_else(|_| format!("corr_{}", event.id.simple()));
    let written = db::exec(
        txn,
        "INSERT INTO messaging_notification_publications (id, event_id, outbox_event_id, decision_id, routing_version, \
           category, event_type, shadow, tenant_id, application_id, environment_id, conversation_id, event_sequence, \
           correlation_id, next_attempt_at, occurred_at, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $15, $17) \
         ON CONFLICT DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            fence::push_key(event.id).into(),
            event.id.into(),
            decision.decision_id.into(),
            decision.routing_version.into(),
            fence::MESSAGE_WAKEUPS.into(),
            EVENT_TYPE.into(),
            shadow.into(),
            event.tenant_id.into(),
            event.application_id.into(),
            event.environment_id.into(),
            conversation_id.into(),
            event_sequence.into(),
            correlation.into(),
            now.into(),
            event.occurred_at.into(),
            (now + LIFETIME).into(),
        ],
    )
    .await?;
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
    /// Posts one batch (≤ [`MAX_BATCH`]); `shadow` marks a batch of SHADOW copies.
    async fn ingest(
        &self,
        events: &[Value],
        shadow: bool,
    ) -> std::result::Result<Vec<IngestResult>, PublishFailure>;
}

/// The service-authenticated I1 of the Notification Service (`cfmeet.notifications`,
/// `notifications-event.publish`).
pub struct NotificationServiceEvents {
    client: Arc<InternalServiceClient>,
}

impl NotificationServiceEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

/// The I1 request body. `shadow` is sent only when set (1.1.0 field; default `false`).
pub fn batch_body(events: &[Value], shadow: bool) -> Value {
    let mut body = json!({ "events": events });
    if shadow {
        body["shadow"] = json!(true);
    }
    body
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
        shadow: bool,
    ) -> std::result::Result<Vec<IngestResult>, PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                INGEST_PATH,
                Some(&batch_body(events, shadow)),
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

// ----------------------------------------------------------------------------- envelope

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    event_id: String,
    outbox_event_id: Uuid,
    decision_id: Uuid,
    routing_version: i32,
    shadow: bool,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    event_sequence: i64,
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
    /// The principal directory (account mapping) is unavailable: retry.
    Unavailable,
    /// The envelope violates the contract (a bug): dead letter.
    Invalid,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Builds the envelope of a row: the X3 entitlement of the event now, mapped to CF Meet
/// account references (the same mapping X3 answers with); ids only.
async fn build(state: &AppState, row: &Row) -> std::result::Result<Value, Unbuilt> {
    let authorization =
        push::authorize_wakeups(&state.db, row.conversation_id, row.event_sequence, None)
            .await
            .map_err(|_| Unbuilt::Unavailable)?
            .ok_or(Unbuilt::Skip("EVENT_NOT_FOUND"))?;
    if !authorization.conversation_active || authorization.entitled.is_empty() {
        return Err(Unbuilt::Skip("NO_ENTITLED_RECIPIENTS"));
    }
    let accounts = crate::notifications::account_refs(state, &authorization)
        .await
        .map_err(|_| Unbuilt::Unavailable)?;
    let mut recipients: Vec<String> = authorization
        .entitled
        .iter()
        .filter_map(|e| accounts.get(&e.principal_id).cloned())
        .collect();
    recipients.sort();
    recipients.dedup();
    if recipients.is_empty() {
        return Err(Unbuilt::Skip("NO_ACCOUNT_RECIPIENTS"));
    }
    envelope(
        row,
        crate::notifications::conversation_kind(&authorization.context_class),
        &authorization.security_profile,
        recipients,
    )
}

/// The ids-only envelope of a row for `recipients` (sorted, deduplicated account refs).
fn envelope(
    row: &Row,
    conversation_kind: &str,
    security_profile: &str,
    recipients: Vec<String>,
) -> std::result::Result<Value, Unbuilt> {
    if recipients.len() > MAX_RECIPIENTS {
        return Err(Unbuilt::Skip("RECIPIENT_LIMIT"));
    }
    let route = RouteRef {
        decision_id: row.decision_id,
        routing_version: row.routing_version,
        protocol: fence::ROUTING_PROTOCOL,
    };
    let mut builder = EnvelopeBuilder::new(
        Producer::Messaging,
        EVENT_TYPE,
        &rfc3339(row.occurred_at),
        route,
    )
    .map_err(|_| Unbuilt::Invalid)?
    .event_id(row.event_id.clone())
    .correlation_id(row.correlation_id.clone())
    .scope(Scope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
    })
    .subject(row.conversation_id, None)
    .dedupe_key(format!("{EVENT_TYPE}:{}", row.outbox_event_id))
    .idempotency_key(row.event_id.clone())
    .coalesce_key(format!("conversation:{}", row.conversation_id))
    .expires_at(rfc3339(row.expires_at))
    .data("conversationId", row.conversation_id.to_string())
    .data("conversationKind", conversation_kind.to_owned())
    .data("eventSequence", row.event_sequence)
    .data("securityProfile", security_profile.to_owned());
    for r in recipients {
        builder = builder.recipient(r);
    }
    match builder.build() {
        Ok(envelope) => serde_json::to_value(envelope).map_err(|_| Unbuilt::Invalid),
        Err(EnvelopeError::TooLarge(_)) => Err(Unbuilt::Skip("RECIPIENT_LIMIT")),
        Err(_) => Err(Unbuilt::Invalid),
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

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

fn kind(shadow: bool) -> &'static str {
    if shadow {
        "shadow"
    } else {
        "publication"
    }
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
    state_: &str,
    outcome: Option<&str>,
    error: Option<String>,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    db::exec(
        txn,
        "UPDATE messaging_notification_publications SET state = $2, outcome = $3, last_error = $4, \
           attempts = attempts + CASE WHEN $2 IN ('ACKED', 'DEAD') THEN 1 ELSE 0 END, completed_at = $5 \
         WHERE id = $1",
        vec![
            row.id.into(),
            state_.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
            now.into(),
        ],
    )
    .await?;
    if state_ == "ACKED" && !row.shadow {
        // §9.5 / F5: the service committed a NEW decision's event.
        db::exec(
            txn,
            "UPDATE messaging_notification_route_decisions SET ns_acked_at = $2 \
              WHERE decision_id = $1 AND route = 'NEW' AND ns_acked_at IS NULL",
            vec![row.decision_id.into(), now.into()],
        )
        .await?;
    }
    Ok(())
}

async fn retry(
    txn: &DatabaseTransaction,
    row: &Row,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> std::result::Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    db::exec(
        txn,
        "UPDATE messaging_notification_publications SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    )
    .await?;
    Ok(())
}

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the re-post is answered `DUPLICATE` (acknowledged). Expired PENDING rows end
/// `EXPIRED` first and are never posted.
pub async fn relay_due(
    state: &AppState,
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
        let txn = state.db.begin().await?;
        let now = state.now();
        let rows: Vec<Row> = db::all(
            &txn,
            "SELECT id, event_id, outbox_event_id, decision_id, routing_version, shadow, tenant_id, application_id, \
                    environment_id, conversation_id, event_sequence, correlation_id, envelope, attempts, occurred_at, expires_at \
               FROM messaging_notification_publications \
              WHERE state = 'PENDING' AND next_attempt_at <= $1 AND expires_at > $1 \
              ORDER BY next_attempt_at, created_at LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), (batch as i64).into()],
        )
        .await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        // Build every missing envelope once; it is stored with the attempt.
        let mut ready: Vec<(Row, Value)> = Vec::with_capacity(rows.len());
        let mut stop = false;
        for mut row in rows {
            let (built, stored) = match row.envelope.take() {
                Some(e) => (Ok(e), true),
                // The directory just failed for this batch: do not ask again per row.
                None if stop => (Err(Unbuilt::Unavailable), false),
                None => (build(state, &row).await, false),
            };
            let envelope = match built {
                Ok(e) if stored => e,
                Ok(e) => {
                    db::exec(
                        &txn,
                        "UPDATE messaging_notification_publications SET envelope = $2 WHERE id = $1",
                        vec![row.id.into(), e.clone().into()],
                    )
                    .await?;
                    e
                }
                Err(Unbuilt::Skip(reason)) => {
                    settle(&txn, &row, "SKIPPED", None, Some(reason.into()), now).await?;
                    report.skipped += 1;
                    crate::metrics::inc(
                        METRIC,
                        &[("outcome", "skipped"), ("kind", kind(row.shadow))],
                    );
                    continue;
                }
                Err(Unbuilt::Unavailable) => {
                    retry(&txn, &row, "PRINCIPALS_UNAVAILABLE", true, now).await?;
                    report.retried += 1;
                    crate::metrics::inc(
                        METRIC,
                        &[("outcome", "retried"), ("kind", kind(row.shadow))],
                    );
                    stop = true;
                    continue;
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
                    crate::metrics::inc(METRIC, &[("outcome", "dead"), ("kind", kind(row.shadow))]);
                    continue;
                }
            };
            ready.push((row, envelope));
        }
        // `shadow` is a batch flag: shadow copies and publications are posted apart.
        for shadow in [true, false] {
            let group: Vec<&(Row, Value)> =
                ready.iter().filter(|(r, _)| r.shadow == shadow).collect();
            if group.is_empty() {
                continue;
            }
            let events: Vec<Value> = group.iter().map(|(_, e)| e.clone()).collect();
            match sink.ingest(&events, shadow).await {
                Ok(results) => {
                    let by_id: HashMap<&str, &IngestResult> =
                        results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                    for (row, _) in group {
                        match by_id.get(row.event_id.as_str()) {
                            Some(r)
                                if matches!(
                                    r.outcome.as_str(),
                                    "ACCEPTED" | "DUPLICATE" | "HELD"
                                ) =>
                            {
                                settle(&txn, row, "ACKED", Some(&r.outcome), None, now).await?;
                                report.acked += 1;
                                crate::metrics::inc(
                                    METRIC,
                                    &[("outcome", "acked"), ("kind", kind(shadow))],
                                );
                            }
                            Some(r) if r.outcome == "REJECTED" => {
                                let reason = reason_code(r.reason.as_deref(), "REJECTED");
                                settle(&txn, row, "DEAD", Some("REJECTED"), Some(reason), now)
                                    .await?;
                                report.dead += 1;
                                crate::metrics::inc(
                                    METRIC,
                                    &[("outcome", "dead"), ("kind", kind(shadow))],
                                );
                            }
                            // No (or an unexpected) answer for this event: try again.
                            _ => {
                                retry(&txn, row, "NO_RESULT", true, now).await?;
                                report.retried += 1;
                                crate::metrics::inc(
                                    METRIC,
                                    &[("outcome", "retried"), ("kind", kind(shadow))],
                                );
                            }
                        }
                    }
                }
                Err(PublishFailure::Rejected) => {
                    for (row, _) in group {
                        settle(
                            &txn,
                            row,
                            "DEAD",
                            None,
                            Some(PublishFailure::Rejected.code().into()),
                            now,
                        )
                        .await?;
                        report.dead += 1;
                        crate::metrics::inc(METRIC, &[("outcome", "dead"), ("kind", kind(shadow))]);
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::Unavailable) => {
                    for (row, _) in group {
                        retry(&txn, row, f.code(), true, now).await?;
                        report.retried += 1;
                        crate::metrics::inc(
                            METRIC,
                            &[("outcome", "retried"), ("kind", kind(shadow))],
                        );
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::NotConfigured) => {
                    for (row, _) in group {
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
async fn expire_overdue(state: &AppState) -> std::result::Result<u64, DbErr> {
    let expired = db::exec(
        &state.db,
        "UPDATE messaging_notification_publications SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![state.now().into()],
    )
    .await?;
    for _ in 0..expired {
        crate::metrics::inc(METRIC, &[("outcome", "expired")]);
    }
    Ok(expired)
}

/// How long a settled publication (ACKED, DEAD, SKIPPED, EXPIRED) is kept after
/// `completed_at` (shadow comparison, diagnostics) before [`maintain`] removes it.
pub const RETENTION: Duration = Duration::days(30);

/// Hourly maintenance, whether or not the relay runs: expires overdue PENDING rows and
/// deletes at most `batch` settled rows older than [`RETENTION`]. A database without the
/// routing fence history (no table) is a no-op. Returns (expired, purged).
pub async fn maintain(state: &AppState, batch: i64) -> Result<(u64, u64)> {
    let undefined = |e: &DbErr| db::sqlstate(e).as_deref() == Some("42P01");
    let expired = match expire_overdue(state).await {
        Err(e) if undefined(&e) => return Ok((0, 0)),
        other => other?,
    };
    let purged = db::exec(
        &state.db,
        "DELETE FROM messaging_notification_publications WHERE id IN (SELECT id FROM messaging_notification_publications \
          WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(state.now() - RETENTION).into(), batch.into()],
    )
    .await?;
    Ok((expired, purged))
}

/// The background relay loop (spawned only with `CF_NOTIFICATIONS_BASE_URL`).
pub async fn run(state: AppState, sink: Arc<dyn NotificationEventSink>, interval: StdDuration) {
    loop {
        if let Err(e) = relay_due(&state, sink.as_ref(), 500).await {
            tracing::warn!(error = %e, "notification publication relay pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_flag_is_sent_only_for_shadow_copies() {
        let e = vec![json!({"eventId": "evt_x"})];
        assert_eq!(batch_body(&e, false), json!({ "events": e }));
        assert_eq!(batch_body(&e, true), json!({ "events": e, "shadow": true }));
    }

    #[test]
    fn service_reasons_are_recorded_only_in_their_machine_shape() {
        assert_eq!(
            reason_code(Some("ROUTE_NOT_NEW"), "REJECTED"),
            "ROUTE_NOT_NEW"
        );
        for bad in ["", "x", "route not new", "A\u{7}B", &"A".repeat(65)] {
            assert_eq!(reason_code(Some(bad), "REJECTED"), "REJECTED", "{bad:?}");
        }
        assert_eq!(reason_code(None, "REJECTED"), "REJECTED");
    }

    #[test]
    fn the_new_route_capacity_always_fits_one_envelope() {
        let now = Utc::now();
        let row = Row {
            id: Uuid::new_v4(),
            event_id: format!("evt_{}", "f".repeat(32)),
            outbox_event_id: Uuid::new_v4(),
            decision_id: Uuid::new_v4(),
            routing_version: i32::MAX,
            shadow: true,
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            event_sequence: i64::MAX,
            correlation_id: format!("corr_{}", "f".repeat(32)),
            envelope: None,
            attempts: 0,
            occurred_at: now,
            expires_at: now + Duration::hours(24),
        };
        let refs = |n: usize| -> Vec<String> {
            (0..n)
                .map(|_| format!("cfmeet-user:{}", Uuid::new_v4()))
                .collect()
        };
        for kind in ["DIRECT", "GROUP"] {
            for profile in ["MANAGED", "END_TO_END"] {
                let built = envelope(&row, kind, profile, refs(NEW_ROUTE_MAX_CANDIDATES));
                assert!(built.is_ok(), "{kind} {profile}: {built:?}");
            }
        }
        // The size limit, not the count, is what bounds an audience: well under 200.
        assert_eq!(
            envelope(&row, "GROUP", "END_TO_END", refs(MAX_RECIPIENTS)),
            Err(Unbuilt::Skip("RECIPIENT_LIMIT"))
        );
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert!(backoff(5) > backoff(4));
        assert_eq!(backoff(40), Duration::seconds(300));
    }
}
