//! Durable background work (Part C §85, §86): invitation expiry and media
//! reconciliation, and the retention loop (Part H §80, [`crate::retention::worker`]).
//! Multi-instance safe without coordination services:
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
use tracing::Instrument;
use uuid::Uuid;

use super::AppCallService;
use crate::calls::errors::CallResult;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerReport {
    pub invitations_expired: usize,
    pub transfers_expired: usize,
    pub calls_reconciled: usize,
    pub repairs: usize,
    pub queue_entries_finished: usize,
    pub queue_offers: usize,
    pub ivr_timeouts: usize,
    pub public_forwards: usize,
    pub voicemail_failed: usize,
    pub voicemail_calls_ended: usize,
    pub voicemail_retention: usize,
    pub voicemail_deleted: usize,
    pub recordings_stopped: usize,
    pub recordings_failed: usize,
    pub recordings_auto_started: usize,
    pub recordings_deleted: usize,
    pub captures_stored: usize,
    pub captures_failed: usize,
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
            if let Some(n) = self
                .expire_call_invitations(call_id)
                .instrument(tracing::info_span!("invitation_expiry", call_id = %call_id))
                .await?
            {
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
        // Enterprise queues: maximum waits, entries of Calls that ended elsewhere, FIFO
        // offers to available agents. Runs even with enterprise calling switched off so
        // waiting Calls always drain (a kill switch never strands a Call).
        let (finished, offers) = self.queue_tick(limit).await?;
        report.queue_entries_finished = finished;
        report.queue_offers = offers;
        // IVR menus whose input deadline passed (retry / onTimeout).
        report.ivr_timeouts = self.ivr_tick(limit).await?;
        // Public forwards waiting for their Part E gates (placed, or refused safely).
        report.public_forwards = self.public_forward_tick(limit).await?;
        // Voicemail: lost captures, overdue recordings, retention and deletion (never
        // blocked by a kill switch).
        let (failed, ended, retention, deleted) = self.voicemail_tick(limit).await?;
        report.voicemail_failed = failed;
        report.voicemail_calls_ended = ended;
        report.voicemail_retention = retention;
        report.voicemail_deleted = deleted;
        // Call recording: ended Calls / deadlines stop, lost captures fail, automatic
        // starts (only while recording is enabled), retention and deletion.
        let (stopped, failed, auto, deleted) = self.recording_tick(limit).await?;
        report.recordings_stopped = stopped;
        report.recordings_failed = failed;
        report.recordings_auto_started = auto;
        report.recordings_deleted = deleted;
        // Application (RTC) captures: stop when due, collect, store, release.
        let (stored, failed) = self.capture_tick(limit).await?;
        report.captures_stored = stored;
        report.captures_failed = failed;
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
            match self
                .reconcile_call(call_id)
                .instrument(tracing::info_span!("media_reconciliation", call_id = %call_id))
                .await
            {
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

    /// Runs the retention worker (Part H §80) forever on this instance, one bounded tick
    /// per `settings.interval` (spawned by the composition root; multi-instance safe:
    /// `FOR UPDATE SKIP LOCKED` claims, one transaction per class batch).
    pub async fn run_retention(self, settings: crate::retention::RetentionSettings) {
        crate::retention::worker::RetentionWorker::new(
            self.db().clone(),
            self.calls.clock(),
            settings,
        )
        .run()
        .await
    }

    /// Setup deadline (§82): Calls still pre-connect (REQUESTED … RINGING) with no Call
    /// event for [`AppCallingConfig::setup_deadline`](super::AppCallingConfig) — typically a
    /// public / SIP Call whose Telephony events never arrive — end FAILED (TIMEOUT)
    /// through the canonical transition. The transition's own cleanup cancels live ring
    /// offers, closes sessions and marks the media binding for teardown; its commit writes
    /// the durable TERMINATE of every live public / SIP leg (same transaction). Multi-
    /// instance safe: each Call is taken with `SKIP LOCKED` and re-checked under its lock.
    /// Returns how many Calls were ended.
    pub async fn expire_stalled_setups(&self, limit: u64) -> CallResult<usize> {
        let now = self.calls.now();
        let cutoff = now - self.config.setup_deadline;
        let rows = self
            .db()
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT c.id FROM calls c \
                  WHERE c.current_state IN ('REQUESTED', 'AUTHORIZED', 'RESOLVING', 'ROUTING', 'DIALING', 'RINGING') \
                    AND c.requested_at <= $1 \
                    AND NOT EXISTS (SELECT 1 FROM call_events e WHERE e.call_id = c.id AND e.occurred_at > $1) \
                  ORDER BY c.requested_at LIMIT $2",
                vec![cutoff.into(), (limit.clamp(1, 500) as i64).into()],
            ))
            .await?;
        let mut ended = 0;
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            match self
                .expire_stalled_setup(call_id, cutoff)
                .instrument(tracing::info_span!("setup_deadline", call_id = %call_id))
                .await
            {
                Ok(true) => ended += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%call_id, %error, "setup deadline not applied; retried next sweep");
                }
            }
        }
        Ok(ended)
    }

    async fn expire_stalled_setup(
        &self,
        call_id: Uuid,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> CallResult<bool> {
        use sea_orm::{EntityTrait, QuerySelect, TransactionTrait};

        use crate::{
            calls::{kinds::FailureCategory, state::CallState, transitions::CallAction},
            models::calls,
            signaling::signals::SignalType,
        };
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_with_behavior(
                sea_orm::sea_query::LockType::Update,
                sea_orm::sea_query::LockBehavior::SkipLocked,
            )
            .one(&txn)
            .await?
        else {
            // Another instance holds the Call (it is sweeping or progressing it).
            txn.rollback().await?;
            return Ok(false);
        };
        let pre_connect = matches!(
            call.current_state,
            CallState::Requested
                | CallState::Authorized
                | CallState::Resolving
                | CallState::Routing
                | CallState::Dialing
                | CallState::Ringing
        );
        // Re-checked under the lock: progress since the candidate scan keeps the Call.
        let progressed = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM call_events WHERE call_id = $1 AND occurred_at > $2)",
                vec![call.id.into(), cutoff.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<bool>(0))
            .transpose()?
            .unwrap_or(true);
        if !pre_connect || progressed || call.requested_at > cutoff {
            txn.rollback().await?;
            return Ok(false);
        }
        let cmd = super::system_cmd(&call);
        let applied = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::Fail {
                    category: FailureCategory::Timeout,
                },
            )
            .await?;
        let ended = applied.call;
        let queue = self.enterprise_call_ended_in(&txn, &ended).await?;
        let mut pending = super::PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        for invitation in &applied.cancelled_invitations {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::RingCancelled,
                &ended,
                invitation,
                &correlation,
            )
            .await?;
        }
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Failed,
            &ended,
            &correlation,
            serde_json::Map::new(),
        )
        .await?;
        txn.commit().await?;
        tracing::warn!(
            call_id = %ended.id,
            correlation_id = %correlation,
            from_state = %call.current_state,
            "call setup deadline passed without progress; call failed (TIMEOUT)"
        );
        self.publish(pending).await;
        if let Some(queue) = queue {
            self.queue_position_signals(queue).await;
        }
        // Leg TERMINATEs were committed with the transition; send them now (the worker
        // retries whatever fails), then tear the media resource down.
        self.dispatch_call(ended.id).await;
        self.teardown_media(ended.id).await;
        self.enterprise_after_commit(ended.id).await;
        Ok(true)
    }

    /// Samples the durable-work backlog into gauges (§84) and raises one WORKER_BACKLOG
    /// incident per queue while its oldest pending item is older than the configured age
    /// (§86). Bounded aggregate queries over partial indexes; never per-tenant labels.
    pub async fn sample_worker_backlog(&self) -> CallResult<()> {
        use crate::calls::app::incidents::{self, Incident, IncidentCategory, IncidentSeverity};
        let now = self.calls.now();
        let row = self
            .db()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT \
                   (SELECT count(*) FROM calling_outbox_events WHERE status = 'PENDING'), \
                   (SELECT min(created_at) FROM calling_outbox_events WHERE status = 'PENDING'), \
                   (SELECT count(*) FROM call_telephony_commands WHERE status = 'PENDING'), \
                   (SELECT min(created_at) FROM call_telephony_commands WHERE status = 'PENDING'), \
                   (SELECT count(*) FROM call_telephony_commands WHERE status = 'DEAD'), \
                   (SELECT count(*) FROM webhook_deliveries \
                     WHERE status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING'))",
            ))
            .await?;
        let Some(row) = row else { return Ok(()) };
        let outbox_pending: i64 = row.try_get_by_index(0)?;
        let outbox_oldest: Option<chrono::DateTime<chrono::Utc>> = row.try_get_by_index(1)?;
        let commands_pending: i64 = row.try_get_by_index(2)?;
        let commands_oldest: Option<chrono::DateTime<chrono::Utc>> = row.try_get_by_index(3)?;
        let commands_dead: i64 = row.try_get_by_index(4)?;
        let deliveries_pending: i64 = row.try_get_by_index(5)?;
        let age = |oldest: Option<chrono::DateTime<chrono::Utc>>| {
            oldest.map(|t| (now - t).num_seconds().max(0)).unwrap_or(0)
        };
        let outbox_age = age(outbox_oldest);
        let commands_age = age(commands_oldest);
        crate::metrics::set_gauge("calling_outbox_pending", &[], outbox_pending);
        crate::metrics::set_gauge("calling_outbox_oldest_pending_age_seconds", &[], outbox_age);
        crate::metrics::set_gauge("calling_telephony_commands_pending", &[], commands_pending);
        crate::metrics::set_gauge(
            "calling_telephony_commands_oldest_pending_age_seconds",
            &[],
            commands_age,
        );
        crate::metrics::set_gauge("calling_telephony_commands_dead", &[], commands_dead);
        crate::metrics::set_gauge(
            "calling_webhook_deliveries_pending",
            &[],
            deliveries_pending,
        );
        let limit = self.config.worker_backlog_incident_age.num_seconds();
        for (queue, age, pending) in [
            ("outbox", outbox_age, outbox_pending),
            ("telephony_command", commands_age, commands_pending),
        ] {
            if age > limit {
                let incident = Incident::platform(
                    IncidentCategory::WorkerBacklog,
                    IncidentSeverity::High,
                    queue,
                )
                .evidence(serde_json::json!({
                    "queue": queue,
                    "oldestPendingAgeSeconds": age,
                    "pending": pending,
                }));
                incidents::raise(self.db(), &incident, now).await;
            }
        }
        Ok(())
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
                    if let Err(error) = app.expire_stalled_setups(100).await {
                        tracing::warn!(%error, "setup deadline sweep failed");
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
                // Backlog gauges and the WORKER_BACKLOG incident (§84 / §86).
                if let Err(error) = self.sample_worker_backlog().await {
                    tracing::warn!(%error, "worker backlog sampling failed");
                }
            }
        });
        let _ = tokio::join!(expiry, reconcile);
    }
}
