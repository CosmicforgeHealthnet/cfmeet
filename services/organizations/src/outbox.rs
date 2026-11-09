//! Transactional outbox delivery (`docs/events/organization-event-contracts.md`).
//!
//! Every mutation records its events in `organization_outbox` in its own transaction:
//! one `cfmeet.reconciliation` record row (never sent) plus one row per consumer that
//! subscribes to the event type ([`subscribers`]). Rows are durable per consumer: a
//! consumer that is not configured yet keeps its rows PENDING and receives the backlog in
//! order once it is configured.
//!
//! [`HttpEventDispatcher`] delivers to each consumer's signed internal ingress
//! (`POST /internal/v1/organization-events`, one receiver-authoritative permission per
//! consumer). [`claim_due`] leases due rows of the configured destinations with
//! `FOR UPDATE SKIP LOCKED` (multi-instance safe) and never overtakes an undelivered
//! earlier row of the same aggregate; [`settle`] marks the outcome with the lease it holds
//! (exponential backoff, dead letter after [`MAX_ATTEMPTS`] or a permanent refusal), and
//! an expired lease is reclaimed. Delivery is at least once; consumers dedupe on `eventId`
//! and apply monotonically on `aggregateVersion`. [`replay_dead`] returns dead-lettered
//! rows to PENDING (operator replay).

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    services, CorrelationId, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use crate::db;

/// The record destination (read by reconciliation; never dispatched).
pub const RECONCILIATION: &str = "cfmeet.reconciliation";
/// The consumer ingress every subscribed service exposes.
pub const CONSUMER_PATH: &str = "/internal/v1/organization-events";

/// Consumers of each event type (contract §2 "Consumer obligation"). The Meeting Service
/// is the first-party integration owner: it keeps the organization projection and the
/// organization-scope Messaging / Calling identities, and runs their offboarding.
pub fn subscribers(event_type: &str) -> &'static [&'static str] {
    const MEETING: &[&str] = &[services::MEETING_SERVICE];
    match event_type {
        "organization.created"
        | "organization.profile_updated"
        | "organization.provisioned"
        | "organization.suspended"
        | "organization.reactivated"
        | "organization.archived"
        | "membership.activated"
        | "membership.role_changed"
        | "membership.suspended"
        | "membership.removed"
        | "ownership.transferred"
        | "policy.updated"
        | "scope.mapping_changed" => MEETING,
        _ => &[],
    }
}

/// Attempts before a row is dead-lettered.
pub const MAX_ATTEMPTS: i32 = 12;
/// How long a claimed row stays leased.
pub const LEASE_SECONDS: i64 = 90;

/// A delivery failure class (never a payload or a response body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryError {
    pub class: String,
    pub permanent: bool,
}

#[async_trait]
pub trait EventDispatcher: Send + Sync {
    /// Whether this dispatcher sends anything (disabled: rows stay PENDING).
    fn enabled(&self) -> bool {
        !self.destinations().is_empty()
    }
    /// The destinations this dispatcher delivers to; rows of other destinations stay
    /// PENDING (the reconciliation record is never among them).
    fn destinations(&self) -> Vec<String>;
    async fn deliver(&self, destination: &str, envelope: &Value) -> Result<(), DeliveryError>;
}

/// Signed service-to-service delivery to each configured consumer.
pub struct HttpEventDispatcher {
    client: Arc<InternalServiceClient>,
    destinations: Vec<String>,
}

impl HttpEventDispatcher {
    /// `destinations` must be consumer identities the client has a base URL for.
    pub fn new(client: Arc<InternalServiceClient>, destinations: Vec<String>) -> Self {
        Self {
            client,
            destinations,
        }
    }
}

/// Maps a delivery failure to a stable class. A 4xx refusal other than timeout or rate
/// limiting is permanent (the consumer will never accept this envelope); every other
/// failure is retried.
pub fn classify(error: &InternalCallError) -> DeliveryError {
    match error {
        InternalCallError::Status { status, code, .. }
            if (400..500).contains(status)
                && *status != 408
                && *status != 429
                && code.as_deref() != Some("RATE_LIMITED") =>
        {
            DeliveryError {
                class: format!("REFUSED_{status}"),
                permanent: true,
            }
        }
        InternalCallError::Status { status, .. } => DeliveryError {
            class: format!("UNAVAILABLE_{status}"),
            permanent: false,
        },
        InternalCallError::UnknownTarget(_) => DeliveryError {
            class: "NOT_CONFIGURED".into(),
            permanent: false,
        },
        InternalCallError::Timeout { .. } => DeliveryError {
            class: "TIMEOUT".into(),
            permanent: false,
        },
        _ => DeliveryError {
            class: "UNAVAILABLE".into(),
            permanent: false,
        },
    }
}

#[async_trait]
impl EventDispatcher for HttpEventDispatcher {
    fn destinations(&self) -> Vec<String> {
        self.destinations.clone()
    }

    async fn deliver(&self, destination: &str, envelope: &Value) -> Result<(), DeliveryError> {
        let target = ServiceId::parse(destination).map_err(|_| DeliveryError {
            class: "INVALID_DESTINATION".into(),
            permanent: true,
        })?;
        // The trace continues the operation that caused the change.
        let request = envelope
            .get("correlationId")
            .and_then(Value::as_str)
            .and_then(|c| Uuid::try_parse(c).ok())
            .and_then(|c| CorrelationId::parse(&format!("corr_{}", c.simple())).ok())
            .map(|c| OutboundServiceRequest::continuing(target.clone(), &c))
            .unwrap_or_else(|| OutboundServiceRequest::new_operation(target));
        self.client
            .post_json(&request, CONSUMER_PATH, envelope)
            .await
            .map(|_| ())
            .map_err(|e| classify(&e))
    }
}

/// No delivery target configured: every row stays PENDING.
pub struct DisabledDispatcher;

#[async_trait]
impl EventDispatcher for DisabledDispatcher {
    fn destinations(&self) -> Vec<String> {
        Vec::new()
    }

    async fn deliver(&self, _destination: &str, _envelope: &Value) -> Result<(), DeliveryError> {
        Err(DeliveryError {
            class: "DISPATCH_DISABLED".into(),
            permanent: false,
        })
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct Claimed {
    pub seq: i64,
    pub event_id: Uuid,
    pub destination: String,
    pub attempts: i32,
    pub payload: Value,
}

/// Exponential backoff after `attempts` failed attempts (2^n seconds, capped at ~17 min).
pub fn backoff_seconds(attempts: i32) -> i64 {
    2_i64.pow(attempts.clamp(0, 10) as u32)
}

/// Leases up to `limit` due rows of `destinations` (PENDING and due, or INFLIGHT with an
/// expired lease). A row is claimed only when no earlier row of the same aggregate for the
/// same destination is undelivered, so a consumer receives each aggregate in version order
/// even across retries (a dead-lettered predecessor holds its successors until an operator
/// replays it).
pub async fn claim_due<C: TransactionTrait>(
    db: &C,
    lease: Uuid,
    limit: i64,
    destinations: &[String],
) -> Result<Vec<Claimed>, DbErr> {
    if destinations.is_empty() {
        return Ok(Vec::new());
    }
    let txn = db.begin().await?;
    db::exec(
        &txn,
        "UPDATE organization_outbox SET state = 'DEAD', lease_id = NULL, lease_until = NULL, last_error_class = 'ATTEMPTS_EXHAUSTED' \
         WHERE state = 'INFLIGHT' AND lease_until <= now() AND attempts >= $1",
        vec![MAX_ATTEMPTS.into()],
    )
    .await?;
    let rows: Vec<Claimed> = db::all(
        &txn,
        "WITH due AS (SELECT o.seq FROM organization_outbox o \
             WHERE o.destination = ANY($4) AND o.destination <> 'cfmeet.reconciliation' \
               AND ((o.state = 'PENDING' AND o.next_attempt_at <= now()) OR (o.state = 'INFLIGHT' AND o.lease_until <= now())) \
               AND NOT EXISTS (SELECT 1 FROM organization_outbox p \
                   WHERE p.destination = o.destination AND p.aggregate_id = o.aggregate_id \
                     AND p.seq < o.seq AND p.state <> 'DELIVERED') \
             ORDER BY o.seq LIMIT $2 FOR UPDATE OF o SKIP LOCKED) \
         UPDATE organization_outbox o SET state = 'INFLIGHT', attempts = o.attempts + 1, lease_id = $1, \
             lease_until = now() + make_interval(secs => $3) \
         FROM due WHERE o.seq = due.seq \
         RETURNING o.seq, o.event_id, o.destination, o.attempts, o.payload",
        vec![
            lease.into(),
            limit.into(),
            (LEASE_SECONDS as f64).into(),
            destinations.to_vec().into(),
        ],
    )
    .await?;
    txn.commit().await?;
    Ok(rows)
}

/// Operator replay: dead-lettered rows (optionally of one destination and/or listed
/// events) return to PENDING with a fresh attempt budget. Returns the count.
pub async fn replay_dead<C: ConnectionTrait>(
    c: &C,
    destination: Option<&str>,
    event_ids: &[Uuid],
) -> Result<u64, DbErr> {
    db::exec(
        c,
        "UPDATE organization_outbox SET state = 'PENDING', attempts = 0, next_attempt_at = now(), last_error_class = NULL \
         WHERE state = 'DEAD' AND destination <> $1 \
           AND ($2::text IS NULL OR destination = $2) \
           AND (cardinality($3::uuid[]) = 0 OR event_id = ANY($3))",
        vec![
            RECONCILIATION.into(),
            destination.map(str::to_string).into(),
            event_ids.to_vec().into(),
        ],
    )
    .await
}

/// Delivery state of one consumer destination (monitoring and diagnostics).
#[derive(Debug, Clone, FromQueryResult, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationStatus {
    pub destination: String,
    pub pending: i64,
    pub inflight: i64,
    pub dead: i64,
    pub delivered: i64,
    pub oldest_undelivered_at: Option<DateTime<Utc>>,
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub last_error_class: Option<String>,
}

/// Per-destination counts (the reconciliation record is excluded).
pub async fn destination_status<C: ConnectionTrait>(
    c: &C,
) -> Result<Vec<DestinationStatus>, DbErr> {
    db::all(
        c,
        "SELECT destination, \
            count(*) FILTER (WHERE state = 'PENDING')::bigint AS pending, \
            count(*) FILTER (WHERE state = 'INFLIGHT')::bigint AS inflight, \
            count(*) FILTER (WHERE state = 'DEAD')::bigint AS dead, \
            count(*) FILTER (WHERE state = 'DELIVERED')::bigint AS delivered, \
            min(created_at) FILTER (WHERE state <> 'DELIVERED') AS oldest_undelivered_at, \
            max(delivered_at) AS last_delivered_at, \
            (array_agg(last_error_class ORDER BY seq DESC) FILTER (WHERE last_error_class IS NOT NULL))[1] AS last_error_class \
         FROM organization_outbox WHERE destination <> $1 GROUP BY destination ORDER BY destination",
        vec![RECONCILIATION.into()],
    )
    .await
}

/// Records the outcome of a claimed row (only while `lease` still holds it).
pub async fn settle<C: ConnectionTrait>(
    c: &C,
    row: &Claimed,
    lease: Uuid,
    outcome: Result<(), DeliveryError>,
) -> Result<(), DbErr> {
    match outcome {
        Ok(()) => db::exec(
            c,
            "UPDATE organization_outbox SET state = 'DELIVERED', delivered_at = now(), lease_id = NULL, lease_until = NULL, last_error_class = NULL WHERE seq = $1 AND lease_id = $2",
            vec![row.seq.into(), lease.into()],
        )
        .await
        .map(|_| ()),
        Err(e) => {
            let dead = e.permanent || row.attempts >= MAX_ATTEMPTS;
            db::exec(
                c,
                "UPDATE organization_outbox SET state = $3, next_attempt_at = now() + make_interval(secs => $4), lease_id = NULL, lease_until = NULL, last_error_class = $5 WHERE seq = $1 AND lease_id = $2",
                vec![
                    row.seq.into(),
                    lease.into(),
                    (if dead { "DEAD" } else { "PENDING" }).into(),
                    (backoff_seconds(row.attempts) as f64).into(),
                    e.class.chars().take(64).collect::<String>().into(),
                ],
            )
            .await
            .map(|_| ())
        }
    }
}

/// Outbox lag: the oldest undelivered event's creation time and the pending count.
#[derive(Debug, Clone, FromQueryResult)]
pub struct Backlog {
    pub pending: i64,
    pub oldest: Option<DateTime<Utc>>,
}

pub async fn backlog<C: ConnectionTrait>(c: &C) -> Result<Backlog, DbErr> {
    db::one(
        c,
        "SELECT count(*)::bigint AS pending, min(created_at) AS oldest FROM organization_outbox WHERE state IN ('PENDING', 'INFLIGHT') AND destination <> $1",
        vec![RECONCILIATION.into()],
    )
    .await
    .map(|b| b.unwrap_or(Backlog { pending: 0, oldest: None }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_seconds(0), 1);
        assert_eq!(backoff_seconds(3), 8);
        assert_eq!(backoff_seconds(10), 1024);
        assert_eq!(backoff_seconds(50), 1024);
        assert_eq!(backoff_seconds(-1), 1);
    }

    #[test]
    fn lifecycle_and_membership_events_reach_the_meeting_service() {
        for t in [
            "membership.removed",
            "membership.suspended",
            "organization.suspended",
            "organization.reactivated",
            "policy.updated",
        ] {
            assert_eq!(subscribers(t), &[services::MEETING_SERVICE], "{t}");
        }
        for t in [
            "invitation.created",
            "team.created",
            "organization.provisioning_changed",
            "unknown",
        ] {
            assert!(subscribers(t).is_empty(), "{t}");
        }
    }

    #[test]
    fn refusals_are_permanent_and_outages_are_retried() {
        let target = ServiceId::parse(services::MEETING_SERVICE).unwrap();
        let status = |status: u16, code: Option<&str>| InternalCallError::Status {
            target: target.clone(),
            status,
            code: code.map(str::to_string),
        };
        assert!(classify(&status(400, Some("VALIDATION_ERROR"))).permanent);
        assert!(classify(&status(403, None)).permanent);
        assert!(!classify(&status(429, Some("RATE_LIMITED"))).permanent);
        assert!(!classify(&status(503, None)).permanent);
        assert!(!classify(&status(408, None)).permanent);
        assert!(!classify(&InternalCallError::UnknownTarget(target.clone())).permanent);
        assert!(!classify(&InternalCallError::Timeout { target }).permanent);
    }

    #[tokio::test]
    async fn the_default_dispatcher_sends_nothing() {
        let d = DisabledDispatcher;
        assert!(!d.enabled());
        assert!(d
            .deliver("cfmeet.reconciliation", &serde_json::json!({}))
            .await
            .is_err());
    }
}
