//! Durable background work (Part C §85, §86): invitation expiry and media
//! reconciliation. Multi-instance safe without coordination services:
//!
//! * a sweep picks candidates from partial indexes (live invitations by expiry,
//!   non-terminated media bindings by due time) — never scanning call history;
//! * work on one Call runs under that Call's row lock, taken with `SKIP LOCKED` by the
//!   expiry sweep, so concurrent instances share the work instead of duplicating it;
//! * a reconciliation pass leases its binding by moving `next_reconcile_at` forward in
//!   the claiming statement (`FOR UPDATE SKIP LOCKED`), so a binding is reconciled by one
//!   instance at a time;
//! * every step is idempotent: a duplicate run finds nothing left to do, never overrides
//!   an accepted winner and never modifies a terminal Call.

use std::time::Duration as StdDuration;

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

use super::AppCallService;
use crate::calls::errors::CallResult;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerReport {
    pub invitations_expired: usize,
    pub transfers_expired: usize,
    pub calls_reconciled: usize,
    pub repairs: usize,
}

impl AppCallService {
    /// Finalizes every due ring offer (server time) and derives NO_ANSWER / DECLINED /
    /// transfer failure where no live offer remains. Also fails accepted transfers whose
    /// target never joined.
    pub async fn expire_due(&self, limit: u64) -> CallResult<WorkerReport> {
        let now = self.calls.now();
        let rows = self
            .db()
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT DISTINCT call_id FROM ( \
                     SELECT call_id FROM call_invitations \
                      WHERE status IN ('PENDING', 'RINGING') AND expires_at <= $1 \
                      ORDER BY expires_at LIMIT $2) due",
                vec![now.into(), (limit as i64).into()],
            ))
            .await?;
        let mut report = WorkerReport::default();
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            if let Some(n) = self.expire_call_invitations(call_id).await? {
                report.invitations_expired += n;
            }
        }
        let stalled = self
            .db()
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT call_id FROM call_transfers WHERE state = 'ACCEPTED' AND accepted_at <= $1 LIMIT $2",
                vec![(now - self.config.transfer_join_deadline).into(), (limit as i64).into()],
            ))
            .await?;
        for row in stalled {
            let call_id: Uuid = row.try_get_by_index(0)?;
            if self.expire_stalled_transfer(call_id).await? {
                report.transfers_expired += 1;
            }
        }
        Ok(report)
    }

    /// Reconciles media bindings that are due (active, ensuring, terminating or with
    /// pending enforcement), each leased to this instance for one interval.
    pub async fn reconcile_due(&self, limit: u64) -> CallResult<WorkerReport> {
        if self.media.is_none() {
            return Ok(WorkerReport::default());
        }
        let now = self.calls.now();
        let lease = now + self.config.reconcile_interval;
        let rows = self
            .db()
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE call_media_sessions SET next_reconcile_at = $2, updated_at = GREATEST($1, updated_at) \
                  WHERE id IN ( \
                      SELECT id FROM call_media_sessions \
                       WHERE status <> 'TERMINATED' AND next_reconcile_at <= $1 \
                       ORDER BY next_reconcile_at \
                       LIMIT $3 \
                       FOR UPDATE SKIP LOCKED) \
                  RETURNING call_id",
                vec![now.into(), lease.into(), (limit as i64).into()],
            ))
            .await?;
        let mut report = WorkerReport::default();
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            match self.reconcile_call(call_id).await {
                Ok(repairs) => {
                    report.calls_reconciled += 1;
                    report.repairs += repairs;
                }
                Err(error) => {
                    tracing::warn!(%call_id, %error, "media reconciliation deferred");
                }
            }
        }
        Ok(report)
    }

    /// Runs both sweeps forever on this instance (spawned by the composition root).
    pub async fn run_workers(self, expiry_every: StdDuration, reconcile_every: StdDuration) {
        let expiry = {
            let app = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(expiry_every);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    if let Err(error) = app.expire_due(200).await {
                        tracing::warn!(%error, "invitation expiry sweep failed");
                    }
                }
            })
        };
        let reconcile = tokio::spawn(async move {
            let mut tick = tokio::time::interval(reconcile_every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.reconcile_due(50).await {
                    tracing::warn!(%error, "media reconciliation sweep failed");
                }
                // Durable Telephony commands (retries, deferred originates, releases).
                if let Err(error) = self.dispatch_telephony_commands(50).await {
                    tracing::warn!(%error, "telephony command dispatch sweep failed");
                }
            }
        });
        let _ = tokio::join!(expiry, reconcile);
    }
}
