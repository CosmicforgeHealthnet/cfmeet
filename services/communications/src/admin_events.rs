//! Owner events to the HQ admin bridge (ADR 0068 A7, design §11): the Hub's transactional
//! outbox `communication_admin_bridge_events` (eleventh migration history) and its relay to
//! the bridge's private event listener (`POST /internal/v1/hq-bridge/events`).
//!
//! The Hub publishes one type, `abuse.report.changed.v1`, for profile reports (subject
//! kind `abuse_report`, subject id the report id):
//!
//! | When | State | Version |
//! |---|---|---|
//! | a person reports a profile (`POST /v1/profiles/{cfMeetId}/report`) | `OPEN` | 1 |
//! | the operator console reviews it (`admin/reports/review`) | `DISMISSED` / `ACTIONED` | 2 |
//! | the admin bridge reviews it (`operator/reports/{reportId}/review`) | `DISMISSED` / `ACTIONED` | 2 |
//!
//! (The report version is derived, as the A2 read and the A4 review answer it: 1 while open,
//! 2 once reviewed.)
//!
//! * **Enqueue** ([`enqueue_report`]) runs inside the report change's transaction, only while
//!   `CF_ADMIN_BRIDGE_BASE_URL` is configured; unset, nothing is written or sent (the Hub
//!   behaves exactly as before). The event id is derived from (type, report, version), so
//!   a replayed write adds nothing and every re-post is byte-identical.
//! * **Body** (closed, the bridge's `events.rs`): `{event_id, type, subject {kind, id},
//!   state, version, occurred_at}`: ids, states, versions and times only, never the
//!   reporter, the target, the reason or a CF Meet ID.
//! * **Relay** ([`relay_due`], every second in the Hub's worker, only while configured):
//!   signed with the Hub's service identity (`cfmeet.communications`) for audience
//!   `cfmeet.admin-bridge` (`hq-bridge-events.publish`), one event per request. Due rows
//!   are LEASED in a short transaction (`FOR UPDATE SKIP LOCKED`, [`LEASE_SECONDS`]) and
//!   posted outside it, so several replicas never post the same row at once and no
//!   transaction stays open across HTTP; a row is settled only by the lease holder. A
//!   crash leaves the row PENDING; once its lease lapses it is posted again and the bridge
//!   answers DUPLICATE (delivery is at least once; the bridge dedupes on producer +
//!   event id). `202` acknowledges the row (`ACKED`, with the bridge's outcome APPENDED /
//!   DUPLICATE / STALE / QUARANTINED); `400` / `413` (a contract refusal) dead-letters it
//!   (`DEAD`); anything else (5xx, 401, 403, 404, 408, 429, transport) retries with
//!   exponential backoff (2 s doubling, at most 5 min) until the row expires ([`LIFETIME`],
//!   `EXPIRED`).
//! * **Maintenance** ([`maintain`], with the Hub's retention sweep): expires overdue rows and
//!   purges settled rows after [`RETENTION_DAYS`].

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
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
    store::{execute, rows},
    State,
};

/// The bridge's owner event route, under `CF_ADMIN_BRIDGE_BASE_URL`.
pub const EVENTS_PATH: &str = "/internal/v1/hq-bridge/events";
/// The one event type the Hub publishes (`event-adapters.v1.json`).
pub const REPORT_EVENT: &str = "abuse.report.changed.v1";
/// Its subject kind.
pub const REPORT_SUBJECT: &str = "abuse_report";
/// How long an event may wait for the bridge before it ends EXPIRED.
pub const LIFETIME: Duration = Duration::days(7);
/// Settled rows are kept this long (diagnostics) before [`maintain`] purges them.
pub const RETENTION_DAYS: i32 = 30;
/// How long a relay pass holds the rows it took. Larger than one pass can take
/// ([`MAX_PASS`] requests of at most the client's 5-second timeout).
pub const LEASE_SECONDS: i32 = 120;
/// Most rows one relay pass takes.
pub const MAX_PASS: usize = 20;

/// The deterministic event id of a report at one version.
pub fn event_id(event_type: &str, subject_id: Uuid, version: i64) -> Uuid {
    let digest = Sha256::digest(format!(
        "cfmeet.communications:admin-bridge-event:{event_type}:{subject_id}:{version}"
    ));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// The report's state code as the bridge sees it (`OPEN` before a review).
fn report_state(decision: Option<&str>) -> Option<&'static str> {
    match decision {
        None => Some("OPEN"),
        Some("DISMISSED") => Some("DISMISSED"),
        Some("ACTIONED") => Some("ACTIONED"),
        Some(_) => None,
    }
}

/// Writes the `abuse.report.changed.v1` event of `report` at `version` (1 open, 2 reviewed)
/// inside the caller's transaction, only while the admin bridge is configured. `decision`
/// is the review's decision (`None` for a new report). Idempotent. Returns whether a row
/// was written.
pub async fn enqueue_report(
    state: &State,
    txn: &DatabaseTransaction,
    report: Uuid,
    decision: Option<&str>,
    version: i64,
) -> Result<bool> {
    if !state.config.admin_bridge_events {
        return Ok(false);
    }
    let Some(subject_state) = report_state(decision) else {
        return Ok(false);
    };
    let written = execute(
        txn,
        "INSERT INTO communication_admin_bridge_events(event_id,event_type,subject_kind,subject_id,subject_state,\
           subject_version,expires_at) VALUES($1,$2,$3,$4,$5,$6,now()+make_interval(days=>$7)) ON CONFLICT DO NOTHING",
        vec![
            event_id(REPORT_EVENT, report, version).into(),
            REPORT_EVENT.into(),
            REPORT_SUBJECT.into(),
            report.into(),
            subject_state.into(),
            version.into(),
            (LIFETIME.num_days() as i32).into(),
        ],
    )
    .await?;
    Ok(written > 0)
}

/// One stored event, as the relay reads it.
#[derive(Debug, Clone, Deserialize)]
pub struct StoredEvent {
    pub event_id: Uuid,
    pub event_type: String,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub subject_state: String,
    pub subject_version: i64,
    pub occurred_at: DateTime<Utc>,
    pub attempts: i32,
}

/// The closed body the bridge accepts.
pub fn body(e: &StoredEvent) -> Value {
    json!({
        "event_id": e.event_id,
        "type": e.event_type,
        "subject": {"kind": e.subject_kind, "id": e.subject_id},
        "state": e.subject_state,
        "version": e.subject_version,
        "occurred_at": e.occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
    })
}

// ------------------------------------------------------------------------------- sink

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailure {
    /// Retry with backoff (5xx, 401, 403, 404, 408, 429, timeout, unreachable, malformed
    /// answer).
    Unavailable,
    /// The bridge refused the event for good (400 / 413): dead letter.
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
pub trait AdminBridgeSink: Send + Sync {
    /// Posts one event; `Ok` carries the bridge's outcome (APPENDED, DUPLICATE, STALE,
    /// QUARANTINED).
    async fn publish(&self, body: &Value) -> std::result::Result<String, PublishFailure>;
}

/// The admin bridge's event listener (`cfmeet.admin-bridge`, `hq-bridge-events.publish`),
/// signed with the Hub's own service identity.
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

/// The bridge's 202 outcomes.
pub const OUTCOMES: [&str; 4] = ["APPENDED", "DUPLICATE", "STALE", "QUARANTINED"];

#[async_trait]
impl AdminBridgeSink for AdminBridgeEvents {
    async fn publish(&self, body: &Value) -> std::result::Result<String, PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::admin_bridge());
        match self
            .client
            .exchange_json(&request, Method::POST, EVENTS_PATH, Some(body))
            .await
        {
            Ok(r) if r.status() == 202 => match r.json::<Answer>() {
                Ok(a) if OUTCOMES.contains(&a.status.as_str()) => Ok(a.status),
                _ => Err(PublishFailure::Unavailable),
            },
            Ok(r) if matches!(r.status(), 400 | 413) => Err(PublishFailure::Rejected),
            Ok(_) => Err(PublishFailure::Unavailable),
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

/// Takes at most `limit` due rows for `lease` (oldest first, a subject's versions in
/// order). Rows another replica holds are skipped; a lapsed lease is taken over.
const TAKE: &str = "WITH due AS (SELECT event_id FROM communication_admin_bridge_events \
     WHERE delivery='PENDING' AND next_attempt_at<=now() AND expires_at>now() \
       AND (lease_until IS NULL OR lease_until<=now()) \
     ORDER BY occurred_at, subject_version LIMIT $2 FOR UPDATE SKIP LOCKED) \
   UPDATE communication_admin_bridge_events e SET lease_id=$1,lease_until=now()+make_interval(secs=>$3) \
     FROM due WHERE e.event_id=due.event_id \
   RETURNING jsonb_build_object('event_id',e.event_id,'event_type',e.event_type,'subject_kind',e.subject_kind, \
     'subject_id',e.subject_id,'subject_state',e.subject_state,'subject_version',e.subject_version, \
     'occurred_at',e.occurred_at,'attempts',e.attempts) doc";

async fn take(state: &State, lease: Uuid, limit: usize) -> Result<Vec<StoredEvent>> {
    let mut taken: Vec<StoredEvent> = rows(
        &state.store.db,
        TAKE,
        vec![
            lease.into(),
            (limit as i64).into(),
            f64::from(LEASE_SECONDS).into(),
        ],
    )
    .await?;
    taken.sort_by(|a, b| {
        (a.occurred_at, a.subject_version).cmp(&(b.occurred_at, b.subject_version))
    });
    Ok(taken)
}

/// Settles a row the lease still holds: ACKED with the bridge's outcome, or DEAD.
async fn settle(
    state: &State,
    e: &StoredEvent,
    lease: Uuid,
    delivery: &str,
    outcome: Option<&str>,
    error: Option<&str>,
) -> Result<bool> {
    let n = execute(
        &state.store.db,
        "UPDATE communication_admin_bridge_events SET delivery=$3,outcome=$4,last_error=$5,attempts=attempts+1,\
           completed_at=now(),lease_id=NULL,lease_until=NULL \
         WHERE event_id=$1 AND lease_id=$2 AND delivery='PENDING'",
        vec![
            e.event_id.into(),
            lease.into(),
            delivery.into(),
            outcome.map(str::to_owned).into(),
            error.map(str::to_owned).into(),
        ],
    )
    .await?;
    Ok(n > 0)
}

/// Gives a held row back: due again after `delay` seconds, counting an attempt when the
/// post was tried.
async fn release(
    state: &State,
    e: &StoredEvent,
    lease: Uuid,
    error: Option<&str>,
    counted: bool,
    delay: i64,
) -> Result<()> {
    execute(
        &state.store.db,
        "UPDATE communication_admin_bridge_events SET attempts=attempts+$3,last_error=COALESCE($4,last_error),\
           next_attempt_at=now()+make_interval(secs=>$5),lease_id=NULL,lease_until=NULL \
         WHERE event_id=$1 AND lease_id=$2 AND delivery='PENDING'",
        vec![
            e.event_id.into(),
            lease.into(),
            i32::from(counted).into(),
            error.map(str::to_owned).into(),
            (delay as f64).into(),
        ],
    )
    .await?;
    Ok(())
}

/// One relay pass over at most `limit` (≤ [`MAX_PASS`]) due rows, posted one by one in
/// order. Safe across replicas: rows are leased before posting and settled only by the
/// lease holder. A failure that means the bridge is unreachable ends the pass; the rows
/// not yet tried are given back uncounted, due when the failed one is.
pub async fn relay_due(
    state: &State,
    sink: &dyn AdminBridgeSink,
    limit: usize,
) -> Result<RelayReport> {
    let mut report = RelayReport {
        expired: expire_overdue(state).await?,
        ..Default::default()
    };
    let lease = Uuid::new_v4();
    let taken = take(state, lease, limit.clamp(1, MAX_PASS)).await?;
    // Once the bridge is unreachable, the rows not yet tried wait as long as the failed one.
    let mut stop: Option<i64> = None;
    for e in &taken {
        if let Some(delay) = stop {
            release(state, e, lease, None, false, delay).await?;
            continue;
        }
        match sink.publish(&body(e)).await {
            Ok(outcome) => {
                if outcome == "QUARANTINED" {
                    tracing::warn!(
                        event_type = %e.event_type,
                        "admin bridge quarantined an owner event"
                    );
                }
                if settle(state, e, lease, "ACKED", Some(&outcome), None).await? {
                    report.acked += 1;
                }
            }
            Err(f @ PublishFailure::Rejected) => {
                if settle(state, e, lease, "DEAD", None, Some(f.code())).await? {
                    report.dead += 1;
                }
            }
            Err(f @ PublishFailure::Unavailable) => {
                let delay = backoff_seconds(e.attempts + 1);
                release(state, e, lease, Some(f.code()), true, delay).await?;
                report.retried += 1;
                stop = Some(delay);
            }
            Err(f @ PublishFailure::NotConfigured) => {
                release(state, e, lease, Some(f.code()), false, 60).await?;
                report.not_configured += 1;
                stop = Some(60);
            }
        }
    }
    if report != RelayReport::default() {
        tracing::info!(
            acked = report.acked,
            retried = report.retried,
            dead = report.dead,
            expired = report.expired,
            "admin bridge events relayed"
        );
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted again). Returns how
/// many.
async fn expire_overdue(state: &State) -> Result<u64> {
    execute(
        &state.store.db,
        "UPDATE communication_admin_bridge_events SET delivery='EXPIRED',last_error='EXPIRED',completed_at=now(),\
           lease_id=NULL,lease_until=NULL WHERE delivery='PENDING' AND expires_at<=now()",
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
        "DELETE FROM communication_admin_bridge_events WHERE event_id IN (SELECT event_id FROM communication_admin_bridge_events \
          WHERE delivery<>'PENDING' AND completed_at<now()-make_interval(days=>$1) ORDER BY completed_at LIMIT $2)",
        vec![RETENTION_DAYS.into(), batch.into()],
    )
    .await?;
    Ok((expired, purged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_ids_are_deterministic_per_report_and_version() {
        let r = Uuid::new_v4();
        let a = event_id(REPORT_EVENT, r, 1);
        assert_eq!(a, event_id(REPORT_EVENT, r, 1));
        assert_ne!(a, event_id(REPORT_EVENT, r, 2));
        assert_ne!(a, event_id(REPORT_EVENT, Uuid::new_v4(), 1));
        // The bridge accepts the lower-case hyphenated form only.
        let text = a.to_string();
        assert_eq!(
            Uuid::parse_str(&text).unwrap().hyphenated().to_string(),
            text
        );
    }

    #[test]
    fn the_body_is_the_bridges_closed_shape() {
        let e = StoredEvent {
            event_id: Uuid::new_v4(),
            event_type: REPORT_EVENT.into(),
            subject_kind: REPORT_SUBJECT.into(),
            subject_id: Uuid::new_v4(),
            subject_state: "ACTIONED".into(),
            subject_version: 2,
            occurred_at: DateTime::parse_from_rfc3339("2026-11-21T10:00:00.5Z")
                .unwrap()
                .with_timezone(&Utc),
            attempts: 0,
        };
        let b = body(&e);
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
        assert_eq!(
            b["subject"],
            json!({"kind": "abuse_report", "id": e.subject_id})
        );
        assert_eq!(b["version"], 2);
        assert_eq!(b["state"], "ACTIONED");
        assert_eq!(b["occurred_at"], "2026-11-21T10:00:00.500000Z");
        assert!(DateTime::parse_from_rfc3339(b["occurred_at"].as_str().unwrap()).is_ok());
    }

    #[test]
    fn only_report_states_are_published() {
        assert_eq!(report_state(None), Some("OPEN"));
        assert_eq!(report_state(Some("DISMISSED")), Some("DISMISSED"));
        assert_eq!(report_state(Some("ACTIONED")), Some("ACTIONED"));
        assert_eq!(report_state(Some("other")), None);
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff_seconds(1), 2);
        assert!(backoff_seconds(5) > backoff_seconds(4));
        assert_eq!(backoff_seconds(40), 300);
    }
}
