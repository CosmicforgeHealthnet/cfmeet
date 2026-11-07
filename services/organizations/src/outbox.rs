//! Transactional outbox delivery (contract §6, `docs/events/organization-event-contracts.md`).
//!
//! Every mutation records its events in `organization_outbox` in its own transaction.
//! Delivery targets and transport are added per consumer in Sections 7–14; until then the
//! [`DisabledDispatcher`] keeps every row PENDING (destination `cfmeet.reconciliation` is a
//! record, read through the reconciliation and authority reads).
//!
//! When a dispatcher is enabled, [`claim_due`] leases due rows with
//! `FOR UPDATE SKIP LOCKED` (multi-instance safe), [`settle`] marks the outcome with the
//! lease it holds, and an expired lease is reclaimed. Delivery is at least once.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use crate::db;

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
    fn enabled(&self) -> bool;
    async fn deliver(&self, destination: &str, envelope: &Value) -> Result<(), DeliveryError>;
}

/// The default: no delivery target is configured (Sections 7–14 add them).
pub struct DisabledDispatcher;

#[async_trait]
impl EventDispatcher for DisabledDispatcher {
    fn enabled(&self) -> bool {
        false
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

/// Leases up to `limit` due rows (PENDING and due, or INFLIGHT with an expired lease).
pub async fn claim_due<C: TransactionTrait>(
    db: &C,
    lease: Uuid,
    limit: i64,
) -> Result<Vec<Claimed>, DbErr> {
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
        "WITH due AS (SELECT seq FROM organization_outbox \
             WHERE (state = 'PENDING' AND next_attempt_at <= now()) OR (state = 'INFLIGHT' AND lease_until <= now()) \
             ORDER BY seq LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE organization_outbox o SET state = 'INFLIGHT', attempts = o.attempts + 1, lease_id = $1, \
             lease_until = now() + make_interval(secs => $3) \
         FROM due WHERE o.seq = due.seq \
         RETURNING o.seq, o.event_id, o.destination, o.attempts, o.payload",
        vec![lease.into(), limit.into(), (LEASE_SECONDS as f64).into()],
    )
    .await?;
    txn.commit().await?;
    Ok(rows)
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
        "SELECT count(*)::bigint AS pending, min(created_at) AS oldest FROM organization_outbox WHERE state IN ('PENDING', 'INFLIGHT')",
        vec![],
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
