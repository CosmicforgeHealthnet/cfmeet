//! Owner events to the HQ admin bridge (ADR 0068 phase A7): Calling's transactional outbox
//! `calling_admin_bridge_events` (admin-operations migration history) and its relay to the
//! bridge's private event listener (`POST /internal/v1/hq-bridge/events`).
//!
//! | Type | Subject kind | When (same transaction) | state | version |
//! |---|---|---|---|---|
//! | `abuse.report.changed.v1` | `abuse_report` | a report is filed (OPEN), or an operator / the bridge reviews it (`set_status`) | `OPEN` / `REVIEWED` / `DISMISSED` | the report row's `version` |
//! | `incident.changed.v1` | `incident` | a tenant's incident is opened (not a repeat occurrence of an open one), or resolved | `OPEN` / `RESOLVED` | 1 while OPEN, 2 once RESOLVED (the row has no version column; an incident is resolved at most once and never reopens — a later occurrence opens a NEW incident) |
//!
//! * **Off by default.** Rows are written only while `CF_ADMIN_BRIDGE_BASE_URL` is
//!   configured ([`set_enabled`], at startup of the server and of `platform_admin`);
//!   unset, nothing is stored or sent.
//! * **Ids only.** The subject id is the report's / incident's opaque UUID inside its
//!   tenant. Never a telephone number, SIP URI, principal, category, comment code, evidence
//!   or free text. A platform-wide incident (no tenant) is never published.
//! * **Relay** ([`relay_due`], every second, every instance: `FOR UPDATE SKIP LOCKED`),
//!   signed with Calling's service identity (`cfmeet.calling`) for audience
//!   `cfmeet.admin-bridge` (`hq-bridge-events.publish`), one event per request (each
//!   token is single use at the bridge). `202` (`APPENDED`, `DUPLICATE`, `STALE`,
//!   `QUARANTINED`) acknowledges the row (`ACKED`); a contract refusal (`400`, or any 4xx
//!   other than 401 / 403 / 408 / 429) dead-letters it (`DEAD`); 5xx, 401 / 403 / 408 / 429
//!   and transport failures retry with exponential backoff (2 s doubling, at most 5 min)
//!   until the row expires (`EXPIRED`, after [`LIFETIME_DAYS`]). Delivery is at least
//!   once; the bridge dedupes on (producer, event_id).
//! * **Maintenance** ([`maintain`], every minute): expires overdue rows and purges settled
//!   rows after [`RETENTION_DAYS`].

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, Statement,
    TransactionTrait, Value as DbValue,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// The bridge's owner-event route (its private events listener).
pub const EVENTS_PATH: &str = "/internal/v1/hq-bridge/events";
/// The configuration that switches owner events on.
pub const ENV_BASE_URL: &str = "CF_ADMIN_BRIDGE_BASE_URL";
/// How long an event may wait for the bridge before it is dropped (`EXPIRED`).
pub const LIFETIME_DAYS: i64 = 7;
/// Settled rows are kept this long (diagnostics) before [`maintain`] purges them.
pub const RETENTION_DAYS: i32 = 30;
/// Most rows one relay transaction locks and posts.
pub const BATCH: usize = 50;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Switches the outbox on (`CF_ADMIN_BRIDGE_BASE_URL` configured) or off. Process-wide:
/// the writers are the state changes themselves, wherever they run (the server, the
/// operator console route, `platform_admin`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

/// Whether owner events are written.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Whether `CF_ADMIN_BRIDGE_BASE_URL` is set (non-blank) in `lookup`.
pub fn configured_in(lookup: impl Fn(&str) -> Option<String>) -> bool {
    lookup(ENV_BASE_URL).is_some_and(|v| !v.trim().is_empty())
}

/// The subject of an owner event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    AbuseReport,
    Incident,
}

impl Subject {
    pub fn event_type(self) -> &'static str {
        match self {
            Subject::AbuseReport => "abuse.report.changed.v1",
            Subject::Incident => "incident.changed.v1",
        }
    }

    /// The bounded metric label of the subject (a code, never the event type string).
    pub fn as_str(self) -> &'static str {
        match self {
            Subject::AbuseReport => "ABUSE_REPORT",
            Subject::Incident => "INCIDENT",
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            Subject::AbuseReport => "abuse_report",
            Subject::Incident => "incident",
        }
    }

    /// The states the bridge may see for this subject.
    pub fn states(self) -> &'static [&'static str] {
        match self {
            Subject::AbuseReport => &["OPEN", "REVIEWED", "DISMISSED"],
            Subject::Incident => &["OPEN", "RESOLVED"],
        }
    }
}

/// The lifecycle version of a calling incident (`calling_incidents` has no version
/// column): 1 while OPEN, 2 once RESOLVED. Resolution happens at most once (`WHERE status
/// = 'OPEN'`) and an incident never reopens, so this is monotonic per incident.
pub fn incident_version(state: &str) -> i64 {
    if state == "RESOLVED" {
        2
    } else {
        1
    }
}

fn stmt(sql: &str, values: Vec<DbValue>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Writes one owner event inside the caller's transaction, when [`enabled`]. `tenant`
/// `None` (a platform-wide subject) writes nothing. Returns whether a row was written.
pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    subject: Subject,
    subject_id: Uuid,
    tenant: Option<Uuid>,
    state: &str,
    version: i64,
    occurred_at: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let Some(tenant) = tenant else {
        return Ok(false);
    };
    if !enabled() || !subject.states().contains(&state) || version < 1 {
        return Ok(false);
    }
    db.execute(stmt(
        "INSERT INTO calling_admin_bridge_events (event_id, event_type, subject_kind, subject_id, \
           tenant_id, subject_state, subject_version, occurred_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(days => $9))",
        vec![
            Uuid::new_v4().into(),
            subject.event_type().into(),
            subject.kind().into(),
            subject_id.into(),
            tenant.into(),
            state.into(),
            version.into(),
            occurred_at.into(),
            (LIFETIME_DAYS as i32).into(),
        ],
    ))
    .await?;
    crate::metrics::inc(
        "calling_admin_bridge_events_total",
        &[("type", subject.as_str()), ("result", "ENQUEUED")],
    );
    Ok(true)
}

/// The closed body the bridge accepts: ids, a state, a version and a time.
pub fn body(
    event_id: Uuid,
    event_type: &str,
    subject_kind: &str,
    subject_id: Uuid,
    state: &str,
    version: i64,
    occurred_at: DateTime<Utc>,
) -> Value {
    json!({
        "event_id": event_id,
        "type": event_type,
        "subject": { "kind": subject_kind, "id": subject_id },
        "state": state,
        "version": version,
        "occurred_at": occurred_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    })
}

// ------------------------------------------------------------------------------- sink

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailure {
    /// Retry with backoff (5xx, 401, 403, 408, 429, timeout, unreachable, malformed answer).
    Unavailable,
    /// The bridge refused the event for good (a contract error): dead letter.
    Rejected,
    /// No admin bridge target configured: rows wait.
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

/// Where owner events go.
#[async_trait]
pub trait AdminBridgeEventSink: Send + Sync {
    /// Posts one event; `Ok` carries the bridge's status (`APPENDED`, `DUPLICATE`, `STALE`,
    /// `QUARANTINED`).
    async fn publish(&self, event: &Value) -> Result<String, PublishFailure>;
}

/// The bridge's event listener (`cfmeet.admin-bridge`, `hq-bridge-events.publish`), signed
/// with Calling's own service identity.
pub struct AdminBridgeEvents {
    client: Arc<InternalServiceClient>,
}

impl AdminBridgeEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct Answer {
    status: String,
}

const ACCEPTED: &[&str] = &["APPENDED", "DUPLICATE", "STALE", "QUARANTINED"];

/// Classifies one bridge answer.
pub fn classify(status: u16, answer: Option<&str>) -> Result<String, PublishFailure> {
    match status {
        200..=299 => answer
            .filter(|a| ACCEPTED.contains(a))
            .map(str::to_owned)
            .ok_or(PublishFailure::Unavailable),
        401 | 403 | 408 | 429 => Err(PublishFailure::Unavailable),
        400..=499 => Err(PublishFailure::Rejected),
        _ => Err(PublishFailure::Unavailable),
    }
}

#[async_trait]
impl AdminBridgeEventSink for AdminBridgeEvents {
    async fn publish(&self, event: &Value) -> Result<String, PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::admin_bridge());
        match self
            .client
            .exchange_json(&request, Method::POST, EVENTS_PATH, Some(event))
            .await
        {
            Ok(r) => {
                let answer = r.json::<Answer>().ok().map(|a| a.status);
                classify(r.status(), answer.as_deref())
            }
            Err(InternalCallError::UnknownTarget(_)) => Err(PublishFailure::NotConfigured),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

// -------------------------------------------------------------------------------- relay

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub expired: u64,
    /// Rows left waiting because no admin bridge target is configured.
    pub not_configured: usize,
}

/// Exponential backoff after `attempts` failed attempts: 2 s, 4 s, … at most 5 minutes.
pub fn backoff_seconds(attempts: i32) -> i64 {
    (1i64 << attempts.clamp(0, 9)).min(300)
}

#[derive(Debug)]
struct Due {
    event_id: Uuid,
    body: Value,
    attempts: i32,
}

const DUE: &str = "SELECT event_id, event_type, subject_kind, subject_id, subject_state, \
      subject_version, occurred_at, attempts \
    FROM calling_admin_bridge_events \
    WHERE delivery_state = 'PENDING' AND next_attempt_at <= now() AND expires_at > now() \
    ORDER BY next_attempt_at, created_at LIMIT $1 FOR UPDATE SKIP LOCKED";

async fn settle(
    txn: &DatabaseTransaction,
    id: Uuid,
    state: &str,
    outcome: &str,
    error: Option<&str>,
) -> Result<(), DbErr> {
    txn.execute(stmt(
        "UPDATE calling_admin_bridge_events SET delivery_state = $2, outcome = $3, \
           last_error = $4, attempts = attempts + 1, completed_at = now() \
         WHERE event_id = $1 AND delivery_state = 'PENDING'",
        vec![
            id.into(),
            state.into(),
            outcome.into(),
            error.map(str::to_owned).into(),
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
        "UPDATE calling_admin_bridge_events SET attempts = $2, last_error = $3, \
           next_attempt_at = now() + make_interval(secs => $4) WHERE event_id = $1",
        vec![
            row.event_id.into(),
            attempts.into(),
            error.into(),
            (delay as f64).into(),
        ],
    ))
    .await?;
    Ok(())
}

/// One relay pass over at most `limit` due rows (oldest first), in transactions of at
/// most [`BATCH`]. Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP
/// LOCKED`) while posted and settled in the same transaction; a crash mid-attempt leaves
/// them PENDING and the re-post (same `event_id`) is answered `DUPLICATE`. A failure that
/// is not the event's own stops the pass (the bridge is down; the next pass retries).
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn AdminBridgeEventSink,
    limit: usize,
) -> Result<RelayReport, DbErr> {
    let mut report = RelayReport {
        expired: expire_overdue(db).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(BATCH);
        let txn = db.begin().await?;
        let due = txn
            .query_all(stmt(DUE, vec![(batch as i64).into()]))
            .await?
            .into_iter()
            .map(|r| {
                let event_id: Uuid = r.try_get("", "event_id")?;
                Ok(Due {
                    event_id,
                    body: body(
                        event_id,
                        &r.try_get::<String>("", "event_type")?,
                        &r.try_get::<String>("", "subject_kind")?,
                        r.try_get("", "subject_id")?,
                        &r.try_get::<String>("", "subject_state")?,
                        r.try_get("", "subject_version")?,
                        r.try_get("", "occurred_at")?,
                    ),
                    attempts: r.try_get("", "attempts")?,
                })
            })
            .collect::<Result<Vec<Due>, DbErr>>()?;
        if due.is_empty() {
            txn.commit().await?;
            break;
        }
        remaining = remaining.saturating_sub(due.len());
        let full = due.len() == batch;
        let mut stop = false;
        for row in &due {
            if stop {
                // The bridge is unavailable: the rest of the batch waits for the next pass
                // without counting an attempt.
                break;
            }
            match sink.publish(&row.body).await {
                Ok(outcome) => {
                    settle(&txn, row.event_id, "ACKED", &outcome, None).await?;
                    report.acked += 1;
                }
                Err(f @ PublishFailure::Rejected) => {
                    settle(&txn, row.event_id, "DEAD", "REJECTED", Some(f.code())).await?;
                    report.dead += 1;
                }
                Err(f @ PublishFailure::Unavailable) => {
                    retry(&txn, row, f.code(), true).await?;
                    report.retried += 1;
                    stop = true;
                }
                Err(f @ PublishFailure::NotConfigured) => {
                    retry(&txn, row, f.code(), false).await?;
                    report.not_configured += 1;
                    stop = true;
                }
            }
        }
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    if report != RelayReport::default() {
        for (result, n) in [
            ("ACKED", report.acked),
            ("RETRIED", report.retried),
            ("DEAD", report.dead),
            ("EXPIRED", report.expired as usize),
        ] {
            crate::metrics::add(
                "calling_admin_bridge_relay_total",
                &[("result", result)],
                n as u64,
            );
        }
        tracing::info!(
            acked = report.acked,
            retried = report.retried,
            dead = report.dead,
            expired = report.expired,
            "admin bridge owner events relayed"
        );
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted). Returns how many.
async fn expire_overdue(db: &DatabaseConnection) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "UPDATE calling_admin_bridge_events SET delivery_state = 'EXPIRED', \
               last_error = 'EXPIRED', completed_at = now() \
             WHERE delivery_state = 'PENDING' AND expires_at <= now()",
            vec![],
        ))
        .await?
        .rows_affected())
}

/// Retention: expires overdue PENDING rows and deletes at most `batch` settled rows older
/// than [`RETENTION_DAYS`]. Returns (expired, purged).
pub async fn maintain(db: &DatabaseConnection, batch: i64) -> Result<(u64, u64), DbErr> {
    let expired = expire_overdue(db).await?;
    let purged = db
        .execute(stmt(
            "DELETE FROM calling_admin_bridge_events WHERE event_id IN (SELECT event_id \
               FROM calling_admin_bridge_events WHERE delivery_state <> 'PENDING' \
               AND completed_at < now() - make_interval(days => $1) \
               ORDER BY completed_at LIMIT $2)",
            vec![RETENTION_DAYS.into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((expired, purged))
}

/// The relay loop of a Calling instance (every second; maintenance every minute). Started
/// only while `CF_ADMIN_BRIDGE_BASE_URL` is configured.
pub async fn run(db: DatabaseConnection, sink: Arc<dyn AdminBridgeEventSink>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut ticks: u64 = 0;
    loop {
        interval.tick().await;
        if let Err(error) = relay_due(&db, sink.as_ref(), 200).await {
            tracing::warn!(%error, "admin bridge event relay pass failed; retried");
        }
        if ticks.is_multiple_of(60) {
            if let Err(error) = maintain(&db, 1000).await {
                tracing::warn!(%error, "admin bridge event maintenance failed; retried");
            }
        }
        ticks = ticks.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_the_closed_bridge_shape_with_ids_only() {
        let (e, s) = (Uuid::new_v4(), Uuid::new_v4());
        let at = DateTime::parse_from_rfc3339("2026-11-21T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let b = body(e, "incident.changed.v1", "incident", s, "RESOLVED", 2, at);
        let keys: Vec<&String> = b.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "event_id",
                "occurred_at",
                "state",
                "subject",
                "type",
                "version"
            ]
        );
        assert_eq!(b["event_id"], json!(e.to_string()));
        assert_eq!(
            b["subject"],
            json!({ "kind": "incident", "id": s.to_string() })
        );
        assert_eq!(b["state"], "RESOLVED");
        assert_eq!(b["version"], 2);
        assert_eq!(b["occurred_at"], "2026-11-21T10:00:00.000Z");
    }

    #[test]
    fn subjects_name_the_adapter_types_and_states() {
        assert_eq!(Subject::AbuseReport.event_type(), "abuse.report.changed.v1");
        assert_eq!(Subject::AbuseReport.kind(), "abuse_report");
        assert_eq!(Subject::Incident.event_type(), "incident.changed.v1");
        assert_eq!(Subject::Incident.kind(), "incident");
        assert!(Subject::Incident.states().contains(&"RESOLVED"));
        assert!(!Subject::AbuseReport.states().contains(&"RESOLVED"));
        assert_eq!(incident_version("OPEN"), 1);
        assert_eq!(incident_version("RESOLVED"), 2);
    }

    #[test]
    fn answers_are_acked_dead_lettered_or_retried() {
        for s in ACCEPTED {
            assert_eq!(classify(202, Some(s)), Ok((*s).to_string()));
        }
        assert_eq!(classify(202, None), Err(PublishFailure::Unavailable));
        assert_eq!(
            classify(202, Some("OTHER")),
            Err(PublishFailure::Unavailable)
        );
        assert_eq!(classify(400, None), Err(PublishFailure::Rejected));
        assert_eq!(classify(413, None), Err(PublishFailure::Rejected));
        for s in [401, 403, 408, 429, 500, 503] {
            assert_eq!(classify(s, None), Err(PublishFailure::Unavailable), "{s}");
        }
    }

    #[test]
    fn the_switch_reads_the_base_url_only() {
        assert!(configured_in(
            |k| (k == ENV_BASE_URL).then(|| "http://bridge-events:8081".to_string())
        ));
        assert!(!configured_in(
            |k| (k == ENV_BASE_URL).then(|| "  ".to_string())
        ));
        assert!(!configured_in(|_| None));
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff_seconds(1), 2);
        assert!(backoff_seconds(5) > backoff_seconds(4));
        assert_eq!(backoff_seconds(40), 300);
    }
}
