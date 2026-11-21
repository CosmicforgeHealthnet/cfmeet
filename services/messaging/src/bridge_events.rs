//! Owner events to the HQ admin bridge (ADR 0068 A7, brief §2): Messaging publishes
//! `abuse.report.changed.v1` (its chat reports) and `incident.changed.v1` (its operational
//! incidents) to the bridge's private event listener
//! (`POST /internal/v1/hq-bridge/events`, `cfmeet.admin-bridge`, permission
//! `hq-bridge-events.publish`).
//!
//! * **Off by default.** Only when `CF_ADMIN_BRIDGE_BASE_URL` is configured
//!   ([`install`] at startup) are events stored and relayed; otherwise [`record`] stores
//!   nothing and nothing is sent.
//! * **Transactional outbox.** [`record`] inserts the event in the SAME transaction as the
//!   report / incident change (`messaging_admin_bridge_events`); the relay ([`relay_due`],
//!   [`run`]) leases due rows and posts each closed body
//!   `{event_id, type, subject: {kind, id}, state, version, occurred_at}` with Messaging's
//!   own service token. At least once: the bridge dedupes on (producer, `event_id`) and
//!   ignores a version not newer than the subject's last (STALE).
//! * A `2xx` (APPENDED, DUPLICATE, STALE or QUARANTINED) acknowledges the row; `400`,
//!   `404`, `405`, `413` or `422` dead-letter it (a contract violation, counted); anything
//!   else (`401`, `403`, `5xx`, transport) retries with exponential backoff, up to
//!   [`MAX_ATTEMPTS`]. Settled rows are purged after [`RETENTION`].
//! * Ids, UPPER_SNAKE states, row versions and times only: never content, a principal, a
//!   category, a summary or any personal data.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{db, error::Result, state::AppState};

/// A Messaging chat report changed (subject kind `abuse_report`, the report id).
pub const ABUSE_REPORT_CHANGED: &str = "abuse.report.changed.v1";
/// A Messaging operational incident changed (subject kind `incident`, the incident id).
pub const INCIDENT_CHANGED: &str = "incident.changed.v1";
/// The bridge's event ingress, under its events base URL.
pub const EVENTS_PATH: &str = "/internal/v1/hq-bridge/events";
/// Delivery attempts before a row is dead-lettered.
pub const MAX_ATTEMPTS: i32 = 20;
/// How long a settled row is kept.
pub const RETENTION: Duration = Duration::days(7);
/// Counter of relay outcomes (`outcome`: acked / retried / dead).
pub const METRIC: &str = "messaging_admin_bridge_events_total";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns event storage on (startup, with `CF_ADMIN_BRIDGE_BASE_URL`; tests).
pub fn install(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}

/// Whether events are stored and relayed.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// The subject kind of an event type.
fn subject_kind(event_type: &str) -> &'static str {
    if event_type == INCIDENT_CHANGED {
        "incident"
    } else {
        "abuse_report"
    }
}

/// Records one event in `db` (the transaction of the change), or nothing while the bridge
/// is not configured. `state` is the subject's UPPER_SNAKE state, `version` its row version.
pub async fn record<C: ConnectionTrait>(
    db: &C,
    event_type: &'static str,
    subject_id: Uuid,
    state: &str,
    version: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    db::exec(
        db,
        "INSERT INTO messaging_admin_bridge_events (event_id, event_type, subject_kind, subject_id, subject_state, \
           subject_version, occurred_at, next_attempt_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7, $7)",
        vec![
            Uuid::new_v4().into(),
            event_type.into(),
            subject_kind(event_type).into(),
            subject_id.into(),
            state.into(),
            version.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Why a post failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailure {
    /// A contract refusal (the event will never be accepted): dead letter.
    Rejected,
    /// Anything else: retry later.
    Unavailable,
}

/// Where events go.
#[async_trait]
pub trait BridgeEventSink: Send + Sync {
    async fn publish(&self, body: &Value) -> std::result::Result<(), PublishFailure>;
}

/// The admin bridge's event ingress (`cfmeet.admin-bridge`, `hq-bridge-events.publish`).
pub struct AdminBridgeEvents {
    client: Arc<InternalServiceClient>,
}

impl AdminBridgeEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

/// How an answer of the bridge settles a row.
pub fn classify(status: u16) -> std::result::Result<(), PublishFailure> {
    match status {
        200..=299 => Ok(()),
        400 | 404 | 405 | 413 | 422 => Err(PublishFailure::Rejected),
        _ => Err(PublishFailure::Unavailable),
    }
}

#[async_trait]
impl BridgeEventSink for AdminBridgeEvents {
    async fn publish(&self, body: &Value) -> std::result::Result<(), PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::admin_bridge());
        match self
            .client
            .exchange_json(&request, Method::POST, EVENTS_PATH, Some(body))
            .await
        {
            Ok(r) => classify(r.status()),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    event_id: Uuid,
    event_type: String,
    subject_kind: String,
    subject_id: Uuid,
    subject_state: String,
    subject_version: i64,
    occurred_at: DateTime<Utc>,
    attempts: i32,
}

/// The closed body of one event (the bridge's `parse_event`).
fn body(r: &Row) -> Value {
    json!({
        "event_id": r.event_id,
        "type": r.event_type,
        "subject": { "kind": r.subject_kind, "id": r.subject_id },
        "state": r.subject_state,
        "version": r.subject_version,
        "occurred_at": r.occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
    })
}

/// Exponential backoff after `attempts` failures: 2^n seconds, at most one hour.
pub fn backoff(attempts: i32) -> Duration {
    Duration::seconds(1i64 << attempts.clamp(0, 12)).min(Duration::hours(1))
}

/// What one relay pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
}

/// Leases at most `batch` due rows (multi-instance safe) and posts them in order.
pub async fn relay_due(
    state: &AppState,
    sink: &dyn BridgeEventSink,
    batch: i64,
) -> Result<RelayReport> {
    let now = state.now();
    let token = Uuid::new_v4();
    let mut rows: Vec<Row> = db::all(
        &state.db,
        "WITH due AS (SELECT event_id FROM messaging_admin_bridge_events WHERE delivery = 'PENDING' \
             AND next_attempt_at <= $1 AND (lease_expires_at IS NULL OR lease_expires_at <= $1) \
             ORDER BY occurred_at, event_id LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE messaging_admin_bridge_events e SET lease_token = $3, lease_expires_at = $4 FROM due \
          WHERE e.event_id = due.event_id \
         RETURNING e.event_id, e.event_type, e.subject_kind, e.subject_id, e.subject_state, e.subject_version, \
                   e.occurred_at, e.attempts",
        vec![
            now.into(),
            batch.into(),
            token.into(),
            (now + Duration::minutes(2)).into(),
        ],
    )
    .await?;
    rows.sort_by_key(|r| (r.occurred_at, r.event_id));
    let mut report = RelayReport::default();
    for r in rows {
        let outcome = sink.publish(&body(&r)).await;
        let attempts = r.attempts + 1;
        let (sql, values, label): (&str, Vec<sea_orm::Value>, &str) = match outcome {
            Ok(()) => (
                "UPDATE messaging_admin_bridge_events SET delivery = 'ACKED', attempts = $3, completed_at = $4, \
                   lease_token = NULL, lease_expires_at = NULL WHERE event_id = $1 AND lease_token = $2",
                vec![r.event_id.into(), token.into(), attempts.into(), now.into()],
                "acked",
            ),
            Err(PublishFailure::Unavailable) if attempts < MAX_ATTEMPTS => (
                "UPDATE messaging_admin_bridge_events SET attempts = $3, next_attempt_at = $4, \
                   lease_token = NULL, lease_expires_at = NULL WHERE event_id = $1 AND lease_token = $2",
                vec![
                    r.event_id.into(),
                    token.into(),
                    attempts.into(),
                    (now + backoff(attempts)).into(),
                ],
                "retried",
            ),
            Err(_) => (
                "UPDATE messaging_admin_bridge_events SET delivery = 'DEAD', attempts = $3, completed_at = $4, \
                   lease_token = NULL, lease_expires_at = NULL WHERE event_id = $1 AND lease_token = $2",
                vec![r.event_id.into(), token.into(), attempts.into(), now.into()],
                "dead",
            ),
        };
        db::exec(&state.db, sql, values).await?;
        crate::metrics::inc(METRIC, &[("outcome", label)]);
        match label {
            "acked" => report.acked += 1,
            "retried" => report.retried += 1,
            _ => {
                report.dead += 1;
                tracing::warn!(event = %r.event_id, event_type = %r.event_type, "admin bridge event dead-lettered");
            }
        }
    }
    Ok(report)
}

/// Removes at most `batch` settled rows older than [`RETENTION`].
pub async fn purge(state: &AppState, batch: i64) -> Result<u64> {
    Ok(db::exec(
        &state.db,
        "DELETE FROM messaging_admin_bridge_events WHERE event_id IN (SELECT event_id FROM messaging_admin_bridge_events \
           WHERE delivery <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![(state.now() - RETENTION).into(), batch.into()],
    )
    .await?)
}

/// The relay loop (spawned only with `CF_ADMIN_BRIDGE_BASE_URL`).
pub async fn run(state: AppState, sink: Arc<dyn BridgeEventSink>, interval: StdDuration) {
    let mut passes: u64 = 0;
    loop {
        if let Err(e) = relay_due(&state, sink.as_ref(), 200).await {
            tracing::warn!(error = %e, "admin bridge event relay pass failed");
        }
        if passes.is_multiple_of(600) {
            if let Err(e) = purge(&state, 1000).await {
                tracing::warn!(error = %e, "admin bridge event purge failed");
            }
        }
        passes = passes.wrapping_add(1);
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_settle_rows_by_contract() {
        assert_eq!(classify(202), Ok(()));
        for s in [400, 404, 405, 413, 422] {
            assert_eq!(classify(s), Err(PublishFailure::Rejected), "{s}");
        }
        for s in [401, 403, 408, 429, 500, 503] {
            assert_eq!(classify(s), Err(PublishFailure::Unavailable), "{s}");
        }
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(5), Duration::seconds(32));
        assert_eq!(backoff(40), Duration::hours(1));
    }

    #[test]
    fn the_body_is_the_bridges_closed_shape() {
        let r = Row {
            event_id: Uuid::new_v4(),
            event_type: INCIDENT_CHANGED.into(),
            subject_kind: subject_kind(INCIDENT_CHANGED).into(),
            subject_id: Uuid::new_v4(),
            subject_state: "OPEN".into(),
            subject_version: 1,
            occurred_at: Utc::now(),
            attempts: 0,
        };
        let b = body(&r);
        let mut keys: Vec<&String> = b.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "event_id",
                "occurred_at",
                "state",
                "subject",
                "type",
                "version"
            ]
        );
        assert_eq!(b["subject"]["kind"], "incident");
        assert_eq!(subject_kind(ABUSE_REPORT_CHANGED), "abuse_report");
    }
}
