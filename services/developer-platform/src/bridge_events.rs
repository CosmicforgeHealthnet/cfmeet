//! ADR 0068 A7 — owner events to the HQ admin bridge (`POST /internal/v1/hq-bridge/events`
//! on the bridge's private events listener; contract `cfmeet.admin-events.v1`).
//!
//! The Developer Platform publishes five types (`event-adapters.v1.json`):
//!
//! | Type | Subject kind | Subject id | State | Version |
//! |---|---|---|---|---|
//! | `verification.case.changed.v1` | `verification_case` | `cfmeet:<KIND>:<uuid>` (production-access request, app review) | HQ's case state (OPEN, CLAIMED, APPROVED, REJECTED, PUBLISHED, SUSPENDED, CANCELLED) | the request / app row version |
//! | `partner.workspace.changed.v1` | `partner_workspace` | the workspace (tenant) id | the workspace state | the workspace row version |
//! | `app.review.changed.v1` | `app` | the app id | the app status | the app row version |
//! | `pricing.version.changed.v1` | `price_version` | the price version id | the version state | the version row version |
//! | `incident.changed.v1` | `incident` | the incident record id | `RECORDED` | 1 (records are immutable) |
//!
//! Body (closed): `{event_id, type, subject: {kind, id}, state, version, occurred_at}` — ids,
//! states, versions and times only, never personal data.
//!
//! * **Outbox.** [`record`] inserts the event in the SAME transaction as the state change
//!   (reading the subject's resulting state and version from its row), only while the
//!   feature is on ([`BridgeEvents`]: `CF_ADMIN_BRIDGE_BASE_URL` set). Off (the default):
//!   nothing is stored and nothing is sent. One row per (type, subject, version), so a
//!   repeated write never doubles an event.
//! * **Publisher.** The worker ([`publish_due`]) posts each due event with the Developer
//!   Platform's own service token (audience `cfmeet.admin-bridge`, permission
//!   `hq-bridge-events.publish`), one fresh token per post. `202` (APPENDED, DUPLICATE,
//!   STALE or QUARANTINED) settles the row `SENT`; `400` / `413` / `422` (the bridge refused
//!   the body) `DEAD`; anything else retries with exponential backoff (≤ 5 min) until the
//!   event is [`LIFETIME`] old (`EXPIRED`). Delivery is at least once: the bridge dedupes
//!   on (producer, event_id). Settled rows are purged after [`RETENTION`].

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::control::{stmt, verification};

pub const VERIFICATION_CASE_CHANGED: &str = "verification.case.changed.v1";
pub const PARTNER_WORKSPACE_CHANGED: &str = "partner.workspace.changed.v1";
pub const APP_REVIEW_CHANGED: &str = "app.review.changed.v1";
pub const PRICING_VERSION_CHANGED: &str = "pricing.version.changed.v1";
pub const INCIDENT_CHANGED: &str = "incident.changed.v1";

/// The bridge's owner event route (on its private events listener).
pub const EVENTS_PATH: &str = "/internal/v1/hq-bridge/events";
/// How long an event may wait for the bridge before it is given up (`EXPIRED`).
pub const LIFETIME: Duration = Duration::days(7);
/// How long a settled event is kept (diagnostics).
pub const RETENTION: Duration = Duration::days(7);
/// Events posted per worker pass.
pub const BATCH: i64 = 200;
/// The worker schedule key of the purge.
pub const MAINTENANCE_JOB: &str = "bridge-events";

/// Whether this process records owner events (`CF_ADMIN_BRIDGE_BASE_URL` set). Off by
/// default: nothing is stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BridgeEvents {
    enabled: bool,
}

impl BridgeEvents {
    pub const OFF: BridgeEvents = BridgeEvents { enabled: false };
    pub const ON: BridgeEvents = BridgeEvents { enabled: true };

    pub fn enabled(self) -> bool {
        self.enabled
    }
}

/// The subject whose state just changed (its row is read in the same transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A partner workspace (`devplat_organizations`).
    Workspace(Uuid),
    /// A production-access request (a `PARTNER_GO_LIVE` verification case).
    ProductionAccess(Uuid),
    /// An app (its review status, and its `APP_REVIEW` verification case once submitted).
    App(Uuid),
    /// A price version.
    PriceVersion(Uuid),
    /// A recorded workspace incident (immutable: version 1).
    Incident(Uuid),
}

/// One owner event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEvent {
    pub event_type: &'static str,
    pub subject_kind: &'static str,
    pub subject_id: String,
    pub state: String,
    pub version: i64,
    pub occurred_at: DateTime<Utc>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The closed body of one event.
pub fn body(event_id: Uuid, e: &OwnerEvent) -> Value {
    json!({
        "event_id": event_id,
        "type": e.event_type,
        "subject": { "kind": e.subject_kind, "id": e.subject_id },
        "state": e.state,
        "version": e.version,
        "occurred_at": rfc3339(e.occurred_at),
    })
}

/// An upper-snake state the bridge accepts (`^[A-Z][A-Z0-9_]{0,63}$`).
fn state_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

async fn row<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    id: Uuid,
) -> Result<Option<(String, i64, DateTime<Utc>)>, DbErr> {
    db.query_one(stmt(sql, vec![id.into()]))
        .await?
        .map(|r| {
            Ok((
                r.try_get::<String>("", "state")?,
                r.try_get::<i64>("", "version")?,
                r.try_get::<DateTime<Utc>>("", "updated_at")?,
            ))
        })
        .transpose()
}

/// The events a change of `source` produces, from its current row (empty: unknown row,
/// or a state that is not an event, e.g. a DRAFT app is not a verification case).
pub async fn events_of<C: ConnectionTrait>(
    db: &C,
    source: Source,
    now: DateTime<Utc>,
) -> Result<Vec<OwnerEvent>, DbErr> {
    let mut out = Vec::new();
    match source {
        Source::Workspace(id) => {
            if let Some((state, version, at)) = row(
                db,
                "SELECT state, version, updated_at FROM devplat_organizations WHERE tenant_id = $1",
                id,
            )
            .await?
            {
                out.push(OwnerEvent {
                    event_type: PARTNER_WORKSPACE_CHANGED,
                    subject_kind: "partner_workspace",
                    subject_id: id.to_string(),
                    state,
                    version,
                    occurred_at: at,
                });
            }
        }
        Source::ProductionAccess(id) => {
            if let Some((state, version, at)) = row(
                db,
                "SELECT state, version, updated_at FROM devplat_production_access_requests WHERE id = $1",
                id,
            )
            .await?
            {
                if let Some(hq) = verification::hq_state(verification::PARTNER_GO_LIVE, &state) {
                    out.push(OwnerEvent {
                        event_type: VERIFICATION_CASE_CHANGED,
                        subject_kind: "verification_case",
                        subject_id: verification::case_id(verification::PARTNER_GO_LIVE, id),
                        state: hq.to_string(),
                        version,
                        occurred_at: at,
                    });
                }
            }
        }
        Source::App(id) => {
            if let Some((state, version, at)) = row(
                db,
                "SELECT status AS state, version, updated_at FROM devplat_apps WHERE app_id = $1",
                id,
            )
            .await?
            {
                if let Some(hq) = verification::hq_state(verification::APP_REVIEW, &state) {
                    out.push(OwnerEvent {
                        event_type: VERIFICATION_CASE_CHANGED,
                        subject_kind: "verification_case",
                        subject_id: verification::case_id(verification::APP_REVIEW, id),
                        state: hq.to_string(),
                        version,
                        occurred_at: at,
                    });
                }
                out.push(OwnerEvent {
                    event_type: APP_REVIEW_CHANGED,
                    subject_kind: "app",
                    subject_id: id.to_string(),
                    state,
                    version,
                    occurred_at: at,
                });
            }
        }
        Source::PriceVersion(id) => {
            if let Some((state, version, at)) = row(
                db,
                "SELECT state, version, updated_at FROM devplat_price_versions WHERE id = $1",
                id,
            )
            .await?
            {
                out.push(OwnerEvent {
                    event_type: PRICING_VERSION_CHANGED,
                    subject_kind: "price_version",
                    subject_id: id.to_string(),
                    state,
                    version,
                    occurred_at: at,
                });
            }
        }
        Source::Incident(id) => out.push(OwnerEvent {
            event_type: INCIDENT_CHANGED,
            subject_kind: "incident",
            subject_id: id.to_string(),
            state: "RECORDED".into(),
            version: 1,
            occurred_at: now,
        }),
    }
    // Never a future time beyond the bridge's skew allowance, never a malformed state.
    out.retain(|e| state_ok(&e.state) && e.version >= 1);
    for e in &mut out {
        e.occurred_at = e.occurred_at.min(now);
    }
    Ok(out)
}

/// Records the events of a change of `source` in `db` (the change's transaction). No-op
/// while the feature is off.
pub async fn record<C: ConnectionTrait>(
    db: &C,
    ev: BridgeEvents,
    source: Source,
) -> Result<(), DbErr> {
    if !ev.enabled {
        return Ok(());
    }
    let now = Utc::now();
    for e in events_of(db, source, now).await? {
        db.execute(stmt(
            "INSERT INTO devplat_bridge_events (id, event_type, subject_kind, subject_id, state, subject_version, \
               occurred_at, created_at, next_attempt_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
             ON CONFLICT (event_type, subject_id, subject_version) DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                e.event_type.into(),
                e.subject_kind.into(),
                e.subject_id.into(),
                e.state.into(),
                e.version.into(),
                e.occurred_at.into(),
                now.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

/// [`record`] for every price version of `book_id` written at `at` (a supersession or a
/// rollback changes several versions in one statement).
pub async fn record_price_versions_at<C: ConnectionTrait>(
    db: &C,
    ev: BridgeEvents,
    book_id: Uuid,
    at: DateTime<Utc>,
) -> Result<(), DbErr> {
    if !ev.enabled {
        return Ok(());
    }
    let ids = db
        .query_all(stmt(
            "SELECT id FROM devplat_price_versions WHERE price_book_id = $1 AND updated_at = $2",
            vec![book_id.into(), at.into()],
        ))
        .await?;
    for r in ids {
        record(db, ev, Source::PriceVersion(r.try_get("", "id")?)).await?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------ publisher

/// Why a post did not settle the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostFailure {
    /// Retry with backoff (5xx, 401, 403, 404, 405, 408, 429, transport, unknown target).
    Retry(&'static str),
    /// The bridge refused the body for good (400, 413, 422): dead letter.
    Dead(&'static str),
}

/// The bridge's answer classification: `Ok(())` for 202.
pub fn classify_status(status: u16) -> Result<(), PostFailure> {
    match status {
        200..=299 => Ok(()),
        400 | 413 | 422 => Err(PostFailure::Dead("EVENT_REFUSED")),
        _ => Err(PostFailure::Retry("UNAVAILABLE")),
    }
}

/// Where owner events go.
#[async_trait]
pub trait BridgeEventSink: Send + Sync {
    async fn post(&self, body: &Value) -> Result<(), PostFailure>;
}

/// The admin bridge's events listener, signed with the Developer Platform's own key.
pub struct HttpBridgeEvents {
    client: Arc<InternalServiceClient>,
}

impl HttpBridgeEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BridgeEventSink for HttpBridgeEvents {
    async fn post(&self, body: &Value) -> Result<(), PostFailure> {
        // A fresh single-use token per post (the bridge claims each token id once).
        let request = OutboundServiceRequest::new_operation(ServiceId::admin_bridge());
        match self
            .client
            .exchange_json(&request, Method::POST, EVENTS_PATH, Some(body))
            .await
        {
            Ok(r) => classify_status(r.status()),
            Err(InternalCallError::UnknownTarget(_)) => Err(PostFailure::Retry("NOT_CONFIGURED")),
            Err(_) => Err(PostFailure::Retry("UNAVAILABLE")),
        }
    }
}

/// What one publisher pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {
    pub sent: usize,
    pub retried: usize,
    pub dead: usize,
    pub expired: u64,
}

/// Exponential backoff of attempt `attempts` (2 s … 5 min).
pub fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

/// One publisher pass at `now`: the due events (oldest first, ≤ `limit`, locked
/// `FOR UPDATE SKIP LOCKED` for their attempt) are posted one by one and settled in the
/// same transaction. A failure to reach the bridge stops the pass (the rest wait).
pub async fn publish_due(
    db: &DatabaseConnection,
    sink: &dyn BridgeEventSink,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<PublishReport, DbErr> {
    let mut report = PublishReport {
        expired: db
            .execute(stmt(
                "UPDATE devplat_bridge_events SET delivery_state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1 \
                 WHERE delivery_state = 'PENDING' AND created_at <= $2",
                vec![now.into(), (now - LIFETIME).into()],
            ))
            .await?
            .rows_affected(),
        ..Default::default()
    };
    let txn = db.begin().await?;
    let rows = txn
        .query_all(stmt(
            "SELECT id, event_type, subject_kind, subject_id, state, subject_version, occurred_at, attempts \
             FROM devplat_bridge_events WHERE delivery_state = 'PENDING' AND next_attempt_at <= $1 \
             ORDER BY created_at, subject_version LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), limit.into()],
        ))
        .await?;
    for r in &rows {
        let id: Uuid = r.try_get("", "id")?;
        let attempts: i32 = r.try_get("", "attempts")?;
        let event_type: String = r.try_get("", "event_type")?;
        let subject_kind: String = r.try_get("", "subject_kind")?;
        let payload = json!({
            "event_id": id,
            "type": event_type,
            "subject": { "kind": subject_kind, "id": r.try_get::<String>("", "subject_id")? },
            "state": r.try_get::<String>("", "state")?,
            "version": r.try_get::<i64>("", "subject_version")?,
            "occurred_at": rfc3339(r.try_get("", "occurred_at")?),
        });
        match sink.post(&payload).await {
            Ok(()) => {
                txn.execute(stmt(
                    "UPDATE devplat_bridge_events SET delivery_state = 'SENT', attempts = attempts + 1, \
                       last_error = NULL, completed_at = $2 WHERE id = $1",
                    vec![id.into(), now.into()],
                ))
                .await?;
                report.sent += 1;
            }
            Err(PostFailure::Dead(code)) => {
                txn.execute(stmt(
                    "UPDATE devplat_bridge_events SET delivery_state = 'DEAD', attempts = attempts + 1, \
                       last_error = $2, completed_at = $3 WHERE id = $1",
                    vec![id.into(), code.into(), now.into()],
                ))
                .await?;
                report.dead += 1;
            }
            Err(PostFailure::Retry(code)) => {
                txn.execute(stmt(
                    "UPDATE devplat_bridge_events SET attempts = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
                    vec![
                        id.into(),
                        (attempts + 1).into(),
                        code.into(),
                        (now + backoff(attempts + 1)).into(),
                    ],
                ))
                .await?;
                report.retried += 1;
                break;
            }
        }
    }
    txn.commit().await?;
    Ok(report)
}

/// Deletes at most `batch` settled events older than [`RETENTION`]. Returns how many.
pub async fn purge(db: &DatabaseConnection, now: DateTime<Utc>, batch: i64) -> Result<u64, DbErr> {
    Ok(db
        .execute(stmt(
            "DELETE FROM devplat_bridge_events WHERE id IN (SELECT id FROM devplat_bridge_events \
              WHERE delivery_state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_the_bridge_closed_shape() {
        let id = Uuid::new_v4();
        let e = OwnerEvent {
            event_type: VERIFICATION_CASE_CHANGED,
            subject_kind: "verification_case",
            subject_id: format!("cfmeet:PARTNER_GO_LIVE:{id}"),
            state: "APPROVED".into(),
            version: 3,
            occurred_at: Utc::now(),
        };
        let b = body(id, &e);
        let keys: Vec<&String> = b.as_object().unwrap().keys().collect();
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
        assert_eq!(b["subject"].as_object().unwrap().len(), 2);
        assert_eq!(b["version"], 3);
        assert!(DateTime::parse_from_rfc3339(b["occurred_at"].as_str().unwrap()).is_ok());
    }

    #[test]
    fn answers_classify_into_settled_dead_and_retry() {
        assert_eq!(classify_status(202), Ok(()));
        for s in [400, 413, 422] {
            assert!(
                matches!(classify_status(s), Err(PostFailure::Dead(_))),
                "{s}"
            );
        }
        for s in [401, 403, 404, 405, 429, 500, 503] {
            assert!(
                matches!(classify_status(s), Err(PostFailure::Retry(_))),
                "{s}"
            );
        }
        assert_eq!(backoff(1), Duration::seconds(2));
        assert_eq!(backoff(30), Duration::seconds(300));
    }

    #[test]
    fn states_are_upper_snake_only() {
        assert!(state_ok("UNDER_REVIEW") && state_ok("V2"));
        assert!(!state_ok("") && !state_ok("open") && !state_ok("A-B"));
        assert!(BridgeEvents::default() == BridgeEvents::OFF && !BridgeEvents::OFF.enabled());
    }
}
