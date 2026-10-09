//! Call-queue execution on ONE canonical Call (Part F §56, §47–§55).
//!
//! ```text
//! router ─▶ EnterQueue ─▶ capacity? ─ full ─▶ overflow destination | BUSY
//!                │
//!                ▼ entry WAITING (Call RINGING, context QUEUED)
//!   dispatch (FIFO, per-queue advisory lock, Call row SKIP LOCKED)
//!                │ available agent: ACTIVE member, ACTIVE principal, no pending offer,
//!                │ not on an answered call, has an eligible endpoint
//!                ▼ offer PENDING + ring offers (answer group = entry's) ─ entry OFFERING
//!   accept ─▶ existing per-participant + group claims ─▶ entry ANSWERED, offer ACCEPTED
//!   offer expires / declined ─▶ offer EXPIRED / DECLINED, entry WAITING (next agent)
//!   max wait ─▶ entry OVERFLOWED (overflow destination takes the SAME Call) | TIMED_OUT
//!   caller cancels ─▶ entry ABANDONED; any other end ─▶ entry CANCELLED
//! ```
//!
//! Multi-instance safety: dispatch of one queue runs under a transaction-scoped advisory
//! lock taken with `pg_try_advisory_xact_lock` (an instance that cannot take it skips the
//! queue — another instance is dispatching it), every entry is changed only under its
//! Call's row lock, and the database refuses a second pending offer for an entry or an
//! agent and a second accepted offer for an entry. A second answering agent is
//! impossible: the entry's offers share one answer group (`call_answer_group_claims`).

use chrono::Duration;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, QuerySelect,
    TransactionTrait,
};
use serde_json::{json, Map};
use uuid::Uuid;

use super::{
    context_of, exec, record_step, router, stmt, update_context, ContextRow, Continuation,
    EnterprisePhase,
};
use crate::{
    calls::{
        app::{scope_of, system_cmd, AppCallService, PendingSignals},
        command::CallCommandContext,
        errors::CallResult,
        kinds::{InvitationPurpose, InvitationStatus, InvitationTerminalReason},
        state::CallState,
        transitions::CallAction,
    },
    domain::PrincipalId,
    enterprise::{
        destinations::{Destination, DestinationType},
        policy::{effective_policy, EffectivePolicy},
        queues::{self, QueueRow},
    },
    models::{call_invitations, call_legs, call_participants, calls},
    platform::principals::PrincipalReference,
    signaling::signals::SignalType,
};

type Rung = (calls::Model, Vec<call_invitations::Model>);

/// Waiting entries considered per dispatch pass of one queue.
const DISPATCH_BATCH: u64 = 25;
/// Waiting callers that receive a position update per change.
const POSITION_SIGNAL_LIMIT: i64 = 50;

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct EntryRow {
    pub id: Uuid,
    pub queue_id: Uuid,
    pub call_id: Uuid,
    pub state: String,
    pub entered_at: chrono::DateTime<chrono::Utc>,
    pub deadline_at: chrono::DateTime<chrono::Utc>,
    pub answer_group_id: Uuid,
    pub offer_count: i32,
}

impl EntryRow {
    fn active(&self) -> bool {
        matches!(self.state.as_str(), "WAITING" | "OFFERING")
    }
}

const ENTRY_COLUMNS: &str =
    "id, queue_id, call_id, state, entered_at, deadline_at, answer_group_id, offer_count";

async fn entry<C: ConnectionTrait>(db: &C, id: Uuid, lock: bool) -> CallResult<Option<EntryRow>> {
    Ok(EntryRow::find_by_statement(stmt(
        &format!(
            "SELECT {ENTRY_COLUMNS} FROM enterprise_queue_entries WHERE id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

/// The queue's advisory key (per queue; the namespace keeps it apart from other users).
fn queue_key(queue_id: Uuid) -> sea_orm::Value {
    format!("enterprise-queue:{queue_id}").into()
}

async fn lock_queue(txn: &DatabaseTransaction, queue_id: Uuid) -> CallResult<()> {
    exec(
        txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![queue_key(queue_id)],
    )
    .await?;
    Ok(())
}

async fn try_lock_queue(txn: &DatabaseTransaction, queue_id: Uuid) -> CallResult<bool> {
    let row = txn
        .query_one(stmt(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
            vec![queue_key(queue_id)],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

/// Ends an active entry (final state). Its pending offer becomes ACCEPTED when it is the
/// answering agent's, CANCELLED otherwise.
async fn exit_entry<C: ConnectionTrait>(
    db: &C,
    entry_id: Uuid,
    state: &'static str,
    assigned: Option<Uuid>,
    at: chrono::DateTime<chrono::Utc>,
) -> CallResult<bool> {
    let changed = exec(
        db,
        "UPDATE enterprise_queue_entries SET state = $2, assigned_principal_id = $3, \
         exited_at = GREATEST(entered_at, $4), version = version + 1 \
         WHERE id = $1 AND state IN ('WAITING', 'OFFERING')",
        vec![entry_id.into(), state.into(), assigned.into(), at.into()],
    )
    .await?;
    if changed > 0 {
        exec(
            db,
            "UPDATE enterprise_queue_offers SET outcome = CASE WHEN principal_id = $2 \
             THEN 'ACCEPTED' ELSE 'CANCELLED' END, resolved_at = GREATEST(offered_at, $3) \
             WHERE entry_id = $1 AND outcome = 'PENDING'",
            vec![
                entry_id.into(),
                assigned.unwrap_or(Uuid::nil()).into(),
                at.into(),
            ],
        )
        .await?;
    }
    Ok(changed > 0)
}

fn wait_seconds(entry: &EntryRow, at: chrono::DateTime<chrono::Utc>) -> f64 {
    ((at - entry.entered_at).num_milliseconds().max(0) as f64) / 1000.0
}

fn exit_metrics(entry: &EntryRow, state: &'static str, at: chrono::DateTime<chrono::Utc>) {
    crate::metrics::inc("calling_enterprise_queue_exits_total", &[("result", state)]);
    crate::metrics::observe(
        "calling_enterprise_queue_wait_seconds",
        &[("result", state)],
        wait_seconds(entry, at),
    );
}

impl AppCallService {
    /// RouteAction::EnterQueue: the Call joins `queue` (FIFO) or, when the queue is full,
    /// goes to its overflow destination (or ends BUSY).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn enter_queue(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        context: &ContextRow,
        policy: &EffectivePolicy,
        queue: QueueRow,
        destination: Destination,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        // Capacity is checked and the entry inserted under the queue's lock: a burst of
        // callers can never over-fill it.
        lock_queue(txn, queue.id).await?;
        let waiting = queues::waiting_count(txn, queue.id)
            .await
            .map_err(super::enterprise_error)?;
        if waiting >= queue.capacity as i64 {
            record_step(
                txn,
                call,
                "QUEUE_FULL",
                Some(destination),
                "QUEUE_FULL",
                None,
                now,
            )
            .await?;
            return self
                .queue_overflow(txn, cmd, call, leg, policy, &queue, "QUEUE_FULL")
                .await;
        }
        let entry_id = Uuid::new_v4();
        let answer_group = Uuid::new_v4();
        let deadline = now + Duration::seconds(queue.max_wait_seconds as i64);
        exec(
            txn,
            "INSERT INTO enterprise_queue_entries (id, tenant_id, application_id, environment_id, \
             queue_id, call_id, state, entered_at, deadline_at, answer_group_id, offer_count, version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'WAITING', $7, $8, $9, 0, 1)",
            vec![
                entry_id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                queue.id.into(),
                call.id.into(),
                now.into(),
                deadline.into(),
                answer_group.into(),
            ],
        )
        .await?;
        exec(
            txn,
            "UPDATE enterprise_call_contexts SET queue_entry_id = $2, answer_group_id = $3, \
             ring_group_id = NULL, ring_cursor = NULL, ring_deadline = NULL, \
             version = version + 1, updated_at = GREATEST(updated_at, $4) WHERE call_id = $1",
            vec![
                call.id.into(),
                entry_id.into(),
                answer_group.into(),
                now.into(),
            ],
        )
        .await?;
        let call = self.mark_ringing(txn, cmd, call, leg).await?;
        record_step(
            txn,
            &call,
            "QUEUE_ENTER",
            Some(destination),
            "DESTINATION_RESOLVED",
            Some(answer_group),
            now,
        )
        .await?;
        update_context(
            txn,
            context,
            EnterprisePhase::Queued,
            Some(destination),
            None,
            now,
        )
        .await?;
        crate::metrics::inc("calling_enterprise_queue_entries_total", &[]);
        Ok((call, Vec::new()))
    }

    /// The queue cannot keep the Call (full, or the maximum wait elapsed): the overflow
    /// destination takes the SAME Call within the hop budget, or the Call ends (BUSY when
    /// full, NO_ANSWER after the wait).
    #[allow(clippy::too_many_arguments)]
    async fn queue_overflow(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        policy: &EffectivePolicy,
        queue: &QueueRow,
        reason: &'static str,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        crate::metrics::inc(
            "calling_enterprise_queue_overflows_total",
            &[
                ("reason", reason),
                (
                    "destination",
                    if queue.overflow().is_some() {
                        "CONFIGURED"
                    } else {
                        "NONE"
                    },
                ),
            ],
        );
        if let Some(overflow) = queue.overflow() {
            record_step(txn, call, "OVERFLOW", Some(overflow), reason, None, now).await?;
            let context = context_of(txn, call.id)
                .await?
                .ok_or(crate::calls::errors::CallError::NotFound)?;
            let action = router::resolve(
                txn,
                &self.principals,
                scope_of(call),
                policy,
                &context,
                overflow,
                now,
            )
            .await?;
            return Box::pin(
                self.apply_route_action(txn, cmd, call, leg, &context, policy, action),
            )
            .await;
        }
        let (action, outcome) = if reason == "QUEUE_FULL" {
            (CallAction::MarkBusy, "BUSY")
        } else {
            (CallAction::MarkNoAnswer, "NO_ANSWER")
        };
        let call = self.locked(txn, call).await?;
        let call = self
            .calls
            .transition_in(txn, cmd, &call, call.version, action)
            .await?
            .call;
        if let Some(context) = context_of(txn, call.id).await? {
            update_context(
                txn,
                &context,
                EnterprisePhase::Ended,
                None,
                Some(outcome),
                now,
            )
            .await?;
        }
        record_step(txn, &call, "ENDED", None, outcome, None, now).await?;
        Ok((call, Vec::new()))
    }

    /// The ring round of a queue offer ended without an answer (expired / declined): the
    /// offer is resolved and the entry waits again for the next available agent.
    pub(crate) async fn queue_offer_ended_in(
        &self,
        txn: &DatabaseTransaction,
        _cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
    ) -> CallResult<Continuation> {
        let call = self.locked(txn, call).await?;
        let Some(entry_id) = context.queue_entry_id else {
            return Ok(Continuation::Waiting);
        };
        let Some(entry) = entry(txn, entry_id, true).await? else {
            return Ok(Continuation::Waiting);
        };
        if entry.state != "OFFERING" {
            return Ok(Continuation::Waiting);
        }
        let now = self.calls.now();
        // DECLINED only when every offer of the agent's round was declined.
        let row = txn
            .query_one(stmt(
                "SELECT o.id, bool_and(i.status = 'DECLINED') AS declined, o.principal_id \
                   FROM enterprise_queue_offers o \
                   JOIN call_invitations i ON i.call_id = o.call_id \
                    AND i.answer_group_id = $2 AND i.principal_id = o.principal_id \
                    AND i.issued_at >= o.offered_at \
                  WHERE o.entry_id = $1 AND o.outcome = 'PENDING' \
                  GROUP BY o.id, o.principal_id",
                vec![entry.id.into(), entry.answer_group_id.into()],
            ))
            .await?;
        if let Some(row) = row {
            let offer_id: Uuid = row.try_get("", "id")?;
            let declined: Option<bool> = row.try_get("", "declined")?;
            let outcome = if declined.unwrap_or(false) {
                "DECLINED"
            } else {
                "EXPIRED"
            };
            exec(
                txn,
                "UPDATE enterprise_queue_offers SET outcome = $2, \
                 resolved_at = GREATEST(offered_at, $3) WHERE id = $1 AND outcome = 'PENDING'",
                vec![offer_id.into(), outcome.into(), now.into()],
            )
            .await?;
            crate::metrics::inc(
                "calling_enterprise_queue_offers_total",
                &[("outcome", outcome)],
            );
        }
        exec(
            txn,
            "UPDATE enterprise_queue_entries SET state = 'WAITING', version = version + 1 \
             WHERE id = $1 AND state = 'OFFERING'",
            vec![entry.id.into()],
        )
        .await?;
        record_step(
            txn,
            &call,
            "QUEUE_WAIT",
            Some(Destination::new(DestinationType::Queue, entry.queue_id)),
            "OFFER_NOT_ANSWERED",
            Some(entry.answer_group_id),
            now,
        )
        .await?;
        Ok(Continuation::Waiting)
    }

    /// Queue bookkeeping of an accepted offer (inside the accepting transaction, under the
    /// Call row lock): the entry is ANSWERED by `principal_id`, its offer ACCEPTED.
    pub(crate) async fn queue_answered_in(
        &self,
        txn: &DatabaseTransaction,
        context: &ContextRow,
        principal_id: Uuid,
    ) -> CallResult<()> {
        let Some(entry_id) = context.queue_entry_id else {
            return Ok(());
        };
        let Some(entry) = entry(txn, entry_id, true).await? else {
            return Ok(());
        };
        let now = self.calls.now();
        if exit_entry(txn, entry.id, "ANSWERED", Some(principal_id), now).await? {
            crate::metrics::inc(
                "calling_enterprise_queue_offers_total",
                &[("outcome", "ACCEPTED")],
            );
            exit_metrics(&entry, "ANSWERED", now);
        }
        Ok(())
    }

    /// The Call ended while queued (the caller hung up → ABANDONED; any other end →
    /// CANCELLED). Idempotent; runs under the Call row lock.
    pub(crate) async fn queue_call_ended_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        context: &ContextRow,
    ) -> CallResult<bool> {
        let Some(entry_id) = context.queue_entry_id else {
            return Ok(false);
        };
        let Some(entry) = entry(txn, entry_id, true).await? else {
            return Ok(false);
        };
        if !entry.active() {
            return Ok(false);
        }
        let now = self.calls.now();
        let state = if call.current_state == CallState::Cancelled {
            "ABANDONED"
        } else {
            "CANCELLED"
        };
        if exit_entry(txn, entry.id, state, None, now).await? {
            exit_metrics(&entry, state, now);
            return Ok(true);
        }
        Ok(false)
    }

    /// One worker pass over every queue: entries past their maximum wait, entries of
    /// Calls that ended elsewhere, then FIFO dispatch of queues with waiting Calls.
    /// Returns (entries finished, offers made).
    pub(crate) async fn queue_tick(&self, limit: u64) -> CallResult<(usize, usize)> {
        let now = self.calls.now();
        let mut finished = 0usize;
        let due = self
            .db()
            .query_all(stmt(
                "SELECT id FROM enterprise_queue_entries \
                  WHERE state IN ('WAITING', 'OFFERING') AND deadline_at <= $1 \
                  ORDER BY deadline_at LIMIT $2",
                vec![now.into(), (limit as i64).into()],
            ))
            .await?;
        for row in due {
            let id: Uuid = row.try_get_by_index(0)?;
            if self.expire_queue_entry(id).await? {
                finished += 1;
            }
        }
        let orphaned = self
            .db()
            .query_all(stmt(
                "SELECT e.id FROM enterprise_queue_entries e JOIN calls c ON c.id = e.call_id \
                  WHERE e.state IN ('WAITING', 'OFFERING') \
                    AND c.current_state IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', \
                                            'CANCELLED', 'ANSWERED', 'CONNECTED', 'HELD', \
                                            'TRANSFERRING', 'ENDING') \
                  LIMIT $1",
                vec![(limit as i64).into()],
            ))
            .await?;
        for row in orphaned {
            let id: Uuid = row.try_get_by_index(0)?;
            if self.settle_orphaned_entry(id).await? {
                finished += 1;
            }
        }
        let queues = self
            .db()
            .query_all(stmt(
                "SELECT DISTINCT queue_id FROM enterprise_queue_entries WHERE state = 'WAITING' \
                  LIMIT $1",
                vec![(limit as i64).into()],
            ))
            .await?;
        let mut offers = 0usize;
        for row in queues {
            let id: Uuid = row.try_get_by_index(0)?;
            offers += self.dispatch_queue_inner(id).await?;
        }
        Ok((finished, offers))
    }

    /// The maximum wait of an entry elapsed: its pending offer is withdrawn, the entry
    /// OVERFLOWS (the overflow destination takes the Call) or TIMES OUT (NO_ANSWER).
    async fn expire_queue_entry(&self, entry_id: Uuid) -> CallResult<bool> {
        let Some(found) = entry(self.db(), entry_id, false).await? else {
            return Ok(false);
        };
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(found.call_id)
            .lock_with_behavior(
                sea_orm::sea_query::LockType::Update,
                sea_orm::sea_query::LockBehavior::SkipLocked,
            )
            .one(&txn)
            .await?
        else {
            txn.rollback().await?;
            return Ok(false);
        };
        let Some(entry) = entry(&txn, entry_id, true).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        let now = self.calls.now();
        if !entry.active() || entry.deadline_at > now {
            txn.commit().await?;
            return Ok(false);
        }
        let cmd = system_cmd(&call);
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        let Some(context) = context_of(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        if call.current_state.is_terminal() || call.current_state != CallState::Ringing {
            self.queue_call_ended_in(&txn, &call, &context).await?;
            txn.commit().await?;
            return Ok(true);
        }
        let scope = scope_of(&call);
        let queue = queues::queue_row(&txn, scope, entry.queue_id, false)
            .await
            .map_err(super::enterprise_error)?;
        let policy = effective_policy(&txn, scope, context.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        // Withdraw the pending offer.
        let withdrawn = self.withdraw_offer_in(&txn, &cmd, &call, &entry).await?;
        for invitation in &withdrawn {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::RingCancelled,
                &call,
                invitation,
                &correlation,
            )
            .await?;
        }
        let state = if queue.overflow().is_some() {
            "OVERFLOWED"
        } else {
            "TIMED_OUT"
        };
        exit_entry(&txn, entry.id, state, None, now).await?;
        exit_metrics(&entry, state, now);
        record_step(
            &txn,
            &call,
            "QUEUE_TIMEOUT",
            Some(Destination::new(DestinationType::Queue, queue.id)),
            "MAX_WAIT_ELAPSED",
            Some(entry.answer_group_id),
            now,
        )
        .await?;
        let leg = self.rtc_leg(&txn, &call).await?;
        let (call, offers) = self
            .queue_overflow(&txn, &cmd, &call, &leg, &policy, &queue, "MAX_WAIT")
            .await?;
        self.enterprise_ring_signals(&txn, &mut pending, &call, &offers, &correlation)
            .await?;
        txn.commit().await?;
        self.publish(pending).await;
        if call.current_state.is_terminal() {
            self.teardown_media(call.id).await;
        }
        self.enterprise_after_commit(call.id).await;
        self.queue_position_signals(queue.id).await;
        Ok(true)
    }

    /// Withdraws the entry's pending offer: its live ring offers end. Returns them.
    async fn withdraw_offer_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        entry: &EntryRow,
    ) -> CallResult<Vec<call_invitations::Model>> {
        use sea_orm::{ColumnTrait, QueryFilter};
        let live = call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call.id))
            .filter(call_invitations::Column::AnswerGroupId.eq(entry.answer_group_id))
            .filter(
                call_invitations::Column::Status
                    .is_in([InvitationStatus::Pending, InvitationStatus::Ringing]),
            )
            .all(txn)
            .await?;
        let mut out = Vec::new();
        for invitation in live {
            out.push(
                self.calls
                    .finish_invitation_in(
                        txn,
                        cmd,
                        call,
                        invitation,
                        InvitationStatus::Cancelled,
                        InvitationTerminalReason::ParticipantRemoved,
                        None,
                    )
                    .await?,
            );
        }
        Ok(out)
    }

    /// An active entry whose Call already left RINGING by another path (cancelled,
    /// failed, ended): the entry gets its final state.
    async fn settle_orphaned_entry(&self, entry_id: Uuid) -> CallResult<bool> {
        let Some(found) = entry(self.db(), entry_id, false).await? else {
            return Ok(false);
        };
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(found.call_id)
            .lock_with_behavior(
                sea_orm::sea_query::LockType::Update,
                sea_orm::sea_query::LockBehavior::SkipLocked,
            )
            .one(&txn)
            .await?
        else {
            txn.rollback().await?;
            return Ok(false);
        };
        let Some(context) = context_of(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        let done = if call.current_state.is_terminal() {
            self.queue_call_ended_in(&txn, &call, &context).await?
        } else if let (Some(principal), true) = (
            context.answered_principal_id,
            matches!(
                call.current_state,
                CallState::Answered
                    | CallState::Connected
                    | CallState::Held
                    | CallState::Transferring
                    | CallState::Ending
            ),
        ) {
            let before = entry(&txn, entry_id, false).await?;
            self.queue_answered_in(&txn, &context, principal).await?;
            before.is_some_and(|e| e.active())
        } else {
            false
        };
        txn.commit().await?;
        if done {
            self.queue_position_signals(found.queue_id).await;
        }
        Ok(done)
    }

    /// Post-commit nudge of an enterprise Call: a queued Call's queue is dispatched now
    /// (the worker would otherwise pick it up on its next pass).
    pub(crate) async fn enterprise_after_commit(&self, call_id: Uuid) {
        let context = context_of(self.db(), call_id).await;
        if let Ok(Some(c)) = &context {
            if c.phase == EnterprisePhase::Forwarded.as_str() {
                // A public forward waits for its Part E gates (the worker retries).
                if let Err(error) = self.execute_public_forward(call_id).await {
                    tracing::warn!(%call_id, %error, "public forward pending; the worker retries");
                }
                return;
            }
        }
        let queue = match context {
            Ok(Some(context)) if context.phase == EnterprisePhase::Queued.as_str() => {
                match context.queue_entry_id {
                    Some(id) => entry(self.db(), id, false)
                        .await
                        .ok()
                        .flatten()
                        .map(|e| e.queue_id),
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(queue) = queue {
            self.dispatch_queue(queue).await;
        }
    }

    /// Offers waiting Calls of `queue_id` to available agents (FIFO). Errors are logged;
    /// the worker retries on its next pass.
    pub async fn dispatch_queue(&self, queue_id: Uuid) {
        if let Err(error) = self.dispatch_queue_inner(queue_id).await {
            tracing::warn!(%queue_id, %error, "queue dispatch deferred");
        }
    }

    async fn dispatch_queue_inner(&self, queue_id: Uuid) -> CallResult<usize> {
        let txn = self.db().begin().await?;
        if !try_lock_queue(&txn, queue_id).await? {
            // Another instance is dispatching this queue right now.
            txn.rollback().await?;
            return Ok(0);
        }
        let waiting = EntryRow::find_by_statement(stmt(
            &format!(
                "SELECT {ENTRY_COLUMNS} FROM enterprise_queue_entries \
                  WHERE queue_id = $1 AND state = 'WAITING' ORDER BY entered_at, seq LIMIT $2"
            ),
            vec![queue_id.into(), (DISPATCH_BATCH as i64).into()],
        ))
        .all(&txn)
        .await?;
        if waiting.is_empty() {
            txn.commit().await?;
            return Ok(0);
        }
        let mut agents = self.available_agents(&txn, queue_id).await?;
        let mut pending = PendingSignals::default();
        let mut offered = 0usize;
        for candidate in waiting {
            if agents.is_empty() {
                break;
            }
            // The Call row first (never wait: a Call being changed elsewhere is skipped
            // this pass), then the entry.
            let Some(call) = calls::Entity::find_by_id(candidate.call_id)
                .lock_with_behavior(
                    sea_orm::sea_query::LockType::Update,
                    sea_orm::sea_query::LockBehavior::SkipLocked,
                )
                .one(&txn)
                .await?
            else {
                continue;
            };
            let Some(entry) = entry(&txn, candidate.id, true).await? else {
                continue;
            };
            if entry.state != "WAITING" || call.current_state != CallState::Ringing {
                continue;
            }
            let now = self.calls.now();
            let remaining = entry.deadline_at - now;
            if remaining < Duration::seconds(2) {
                continue; // the maximum wait is about to end it
            }
            let scope = scope_of(&call);
            let queue = queues::queue_row(&txn, scope, queue_id, false)
                .await
                .map_err(super::enterprise_error)?;
            let ttl = Duration::seconds(queue.offer_timeout_seconds as i64).min(remaining);
            let cmd = system_cmd(&call);
            // The agent whose offer of this entry just ended unanswered is not offered it
            // again before one offer timeout passed (others may take it meanwhile).
            let cooling = txn
                .query_one(stmt(
                    "SELECT principal_id FROM enterprise_queue_offers \
                      WHERE entry_id = $1 AND outcome IN ('EXPIRED', 'DECLINED') AND resolved_at > $2 \
                      ORDER BY generation DESC LIMIT 1",
                    vec![
                        entry.id.into(),
                        (now - Duration::seconds(queue.offer_timeout_seconds as i64)).into(),
                    ],
                ))
                .await?
                .map(|r| r.try_get_by_index::<Uuid>(0))
                .transpose()?;
            let mut offer = None;
            let mut i = 0;
            while i < agents.len() {
                let agent = agents[i];
                if agent == call.initiator_principal_id || Some(agent) == cooling {
                    i += 1;
                    continue;
                }
                // Tried once per pass: offered, or unable to take a Call right now.
                agents.remove(i);
                if let Some(invitations) = self
                    .offer_entry_in(&txn, &cmd, &call, &entry, agent, ttl)
                    .await?
                {
                    offer = Some(invitations);
                    break;
                }
            }
            let Some(invitations) = offer else {
                continue;
            };
            offered += 1;
            let correlation = cmd.correlation_id().as_str().to_string();
            for invitation in &invitations {
                self.invitation_signal(
                    &txn,
                    &mut pending,
                    SignalType::Incoming,
                    &call,
                    invitation,
                    &correlation,
                )
                .await?;
            }
        }
        txn.commit().await?;
        self.publish(pending).await;
        if offered > 0 {
            self.queue_position_signals(queue_id).await;
        }
        Ok(offered)
    }

    /// Available agents of the queue, least recently offered first: ACTIVE members whose
    /// principal is ACTIVE, without a pending offer (of any queue) and not on an answered
    /// Call.
    pub(crate) async fn available_agents(
        &self,
        txn: &DatabaseTransaction,
        queue_id: Uuid,
    ) -> CallResult<Vec<Uuid>> {
        let rows = txn
            .query_all(stmt(
                "SELECT m.principal_id FROM enterprise_queue_members m \
                   JOIN platform_principals p ON p.tenant_id = m.tenant_id \
                    AND p.application_id = m.application_id AND p.environment_id = m.environment_id \
                    AND p.id = m.principal_id \
                  WHERE m.queue_id = $1 AND m.status = 'ACTIVE' AND p.status = 'ACTIVE' \
                    AND NOT EXISTS (SELECT 1 FROM enterprise_queue_offers o \
                         WHERE o.tenant_id = m.tenant_id AND o.application_id = m.application_id \
                           AND o.environment_id = m.environment_id \
                           AND o.principal_id = m.principal_id AND o.outcome = 'PENDING') \
                    AND NOT EXISTS (SELECT 1 FROM call_participants cp \
                         JOIN calls c ON c.id = cp.call_id \
                         WHERE cp.principal_id = m.principal_id AND cp.ended_at IS NULL \
                           AND c.current_state IN ('ANSWERED', 'CONNECTED', 'HELD', \
                                                   'TRANSFERRING', 'ENDING') \
                           AND (cp.role = 'INITIATOR' OR EXISTS (SELECT 1 FROM call_answer_claims a \
                                WHERE a.call_id = cp.call_id AND a.participant_id = cp.id))) \
                  ORDER BY (SELECT max(o.offered_at) FROM enterprise_queue_offers o \
                             WHERE o.queue_id = m.queue_id AND o.principal_id = m.principal_id) \
                           ASC NULLS FIRST, m.position",
                vec![queue_id.into()],
            ))
            .await?;
        rows.into_iter()
            .map(|r| r.try_get_by_index::<Uuid>(0).map_err(Into::into))
            .collect()
    }

    /// Offers `entry` to `agent`: one PENDING offer (the database refuses a second one
    /// for the entry or the agent), the agent joins the Call and their eligible endpoints
    /// ring with the entry's answer group. `None` = the agent cannot take it now.
    async fn offer_entry_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        entry: &EntryRow,
        agent: Uuid,
        ttl: Duration,
    ) -> CallResult<Option<Vec<call_invitations::Model>>> {
        let scope = scope_of(call);
        let Ok(principal) = self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(agent)),
            )
            .await
        else {
            return Ok(None);
        };
        let endpoints = self
            .endpoints
            .eligible_in(txn, scope, agent, self.config.max_endpoints_per_participant)
            .await?;
        if endpoints.is_empty() {
            return Ok(None);
        }
        // An agent who already left this Call (e.g. a ring-group loser) is not offered it.
        {
            use sea_orm::{ColumnTrait, QueryFilter};
            if call_participants::Entity::find()
                .filter(call_participants::Column::CallId.eq(call.id))
                .filter(call_participants::Column::PrincipalId.eq(agent))
                .one(txn)
                .await?
                .is_some_and(|p| p.ended_at.is_some())
            {
                return Ok(None);
            }
        }
        let now = self.calls.now();
        let inserted = exec(
            txn,
            "INSERT INTO enterprise_queue_offers (id, tenant_id, application_id, environment_id, \
             entry_id, queue_id, call_id, principal_id, generation, outcome, offered_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'PENDING', $10, $11) ON CONFLICT DO NOTHING",
            vec![
                Uuid::new_v4().into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                entry.id.into(),
                entry.queue_id.into(),
                call.id.into(),
                agent.into(),
                (entry.offer_count + 1).into(),
                now.into(),
                (now + ttl).into(),
            ],
        )
        .await?;
        if inserted == 0 {
            return Ok(None);
        }
        let (participant, _) = self
            .calls
            .add_participant_in(txn, cmd, call, &principal, None)
            .await?;
        self.calls
            .ensure_media_permissions_in(txn, call, &participant)
            .await?;
        let invitations = self
            .calls
            .issue_invitations_in(
                txn,
                cmd,
                call,
                &participant,
                &endpoints,
                InvitationPurpose::Call,
                None,
                ttl,
                Some(entry.answer_group_id),
            )
            .await?;
        exec(
            txn,
            "UPDATE enterprise_queue_entries SET state = 'OFFERING', offer_count = offer_count + 1, \
             version = version + 1 WHERE id = $1 AND state = 'WAITING'",
            vec![entry.id.into()],
        )
        .await?;
        record_step(
            txn,
            call,
            "QUEUE_OFFER",
            Some(Destination::new(DestinationType::Queue, entry.queue_id)),
            "AGENT_AVAILABLE",
            Some(entry.answer_group_id),
            now,
        )
        .await?;
        crate::metrics::inc(
            "calling_enterprise_queue_offers_total",
            &[("outcome", "OFFERED")],
        );
        Ok(Some(invitations))
    }

    /// Sends each waiting caller of the queue (bounded) its authoritative position.
    pub(crate) async fn queue_position_signals(&self, queue_id: Uuid) {
        if let Err(error) = self.queue_position_signals_inner(queue_id).await {
            tracing::debug!(%queue_id, %error, "queue position signals skipped");
        }
    }

    async fn queue_position_signals_inner(&self, queue_id: Uuid) -> CallResult<()> {
        let rows = self
            .db()
            .query_all(stmt(
                "SELECT call_id, row_number() OVER (ORDER BY entered_at, seq) AS position \
                   FROM enterprise_queue_entries \
                  WHERE queue_id = $1 AND state IN ('WAITING', 'OFFERING') \
                  ORDER BY entered_at, seq LIMIT $2",
                vec![queue_id.into(), POSITION_SIGNAL_LIMIT.into()],
            ))
            .await?;
        let mut pending = PendingSignals::default();
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            let position: i64 = row.try_get_by_index(1)?;
            let Some(call) = calls::Entity::find_by_id(call_id).one(self.db()).await? else {
                continue;
            };
            let mut data = Map::new();
            data.insert("queueId".into(), json!(queue_id));
            data.insert("position".into(), json!(position));
            self.initiator_signal(
                self.db(),
                &mut pending,
                SignalType::QueuePosition,
                &call,
                "queue-position",
                data,
            )
            .await?;
        }
        self.publish(pending).await;
        Ok(())
    }

    /// A Call-level signal for the initiator's endpoints only (queue position is the
    /// caller's business, not the agents').
    pub(crate) async fn initiator_signal<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        kind: SignalType,
        call: &calls::Model,
        correlation_id: &str,
        data: Map<String, serde_json::Value>,
    ) -> CallResult<()> {
        let rows = db
            .query_all(stmt(
                "SELECT DISTINCT s.endpoint_id FROM call_sessions s \
                   JOIN call_participants p ON p.id = s.participant_id \
                  WHERE s.call_id = $1 AND p.role = 'INITIATOR' AND s.endpoint_id IS NOT NULL",
                vec![call.id.into()],
            ))
            .await?;
        let endpoints = rows
            .into_iter()
            .filter_map(|r| r.try_get_by_index::<Uuid>(0).ok())
            .collect::<Vec<_>>();
        if endpoints.is_empty() {
            return Ok(());
        }
        let sequence = self.sequence(db, call.id).await?;
        pending.push(
            endpoints,
            crate::signaling::signals::build(
                kind,
                call,
                sequence,
                correlation_id,
                None,
                data,
                self.calls.now(),
            ),
        );
        Ok(())
    }
}
