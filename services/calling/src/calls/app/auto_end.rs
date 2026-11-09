//! Automatic end of abandoned Calls (Blueprint v1.0 §12, CAL-001).
//!
//! A Call that was answered must not outlive its parties. Three server-side rules end it
//! through the SAME path as `POST /v1/calls/{id}/end` (ENDING, live offers / transfer /
//! attempts stopped, provider cleanup, ENDED on cleanup evidence, usage finalized), issued
//! as a system command whose CALL_STATE_CHANGED event carries `end_trigger`:
//!
//! * `PARTICIPATION_ENDED` — a CONNECTED / HELD Call with fewer than two parties with
//!   live participation. A party is an active participant with a CONNECTED session, a
//!   session DISCONNECTED (or created but not yet connected) within the participation
//!   grace, a live network leg (SIP / public) or a live ring offer; or a live network leg
//!   that belongs to no participant (a public-network party). The APPLICATION leg (the
//!   shared media binding) is infrastructure, never a party; a recording capture is never
//!   a party either (recording one remaining person is not a conversation). Evaluated
//!   when a session is removed, when the provider reports a connection gone, after a
//!   transfer completes or fails, and by the reaper (for grace expiry).
//! * `IDLE_TIMEOUT` — a CONNECTED / HELD Call with no connected session and no connected
//!   network leg since `idle_call_timeout`, or a Call still ANSWERED (the media path
//!   never established) `idle_call_timeout` after the answer.
//! * `MAX_DURATION` — an ANSWERED / CONNECTED / HELD / TRANSFERRING Call older than
//!   `max_call_duration` (counted from its first connection, else its answer).
//!
//! Pre-connect Calls (REQUESTED … RINGING) are never touched here: enterprise IVR menus,
//! queue waits and voicemail recordings — one human plus server legs — all run while the
//! Call is still ringing and are bounded by their own deadlines and the setup deadline.
//! A TRANSFERRING Call is bounded by the transfer join deadline and re-evaluated once it
//! completes or fails.
//!
//! Concurrency: every decision is re-taken under the Call row lock; a Call already ENDING
//! / ENDED is left alone, so a client END racing an automatic end yields one ENDING → ENDED
//! transition and one finalized usage set (usage keys are idempotent).

use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, QuerySelect, Statement, TransactionTrait};
use tracing::Instrument;
use uuid::Uuid;

use super::{system_cmd, AppCallService};
use crate::{
    calls::{
        attributes::SafeValue, errors::CallResult, service::TERMINAL_STATES_SQL, state::CallState,
        transitions::CallAction,
    },
    models::calls,
};

/// Why Calling ended a Call by itself (the `end_trigger` event detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoEndReason {
    ParticipationEnded,
    IdleTimeout,
    MaxDuration,
}

impl AutoEndReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            AutoEndReason::ParticipationEnded => "PARTICIPATION_ENDED",
            AutoEndReason::IdleTimeout => "IDLE_TIMEOUT",
            AutoEndReason::MaxDuration => "MAX_DURATION",
        }
    }
}

const LIVE_LEG: &str = "l.leg_kind <> 'APPLICATION' \
     AND l.state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')";

/// Number of parties with live participation in the Call `call` (an SQL expression), with
/// `grace` the parameter holding `now − participation_grace` and `now` the current time.
fn live_parties_sql(call: &str, grace: &str, now: &str) -> String {
    format!(
        "(SELECT count(*) FROM ( \
            SELECT p.id FROM call_participants p \
             WHERE p.call_id = {call} AND p.ended_at IS NULL AND ( \
                EXISTS (SELECT 1 FROM call_sessions s \
                         WHERE s.call_id = p.call_id AND s.participant_id = p.id AND ( \
                               s.state = 'CONNECTED' \
                            OR (s.state = 'DISCONNECTED' AND s.last_disconnected_at > {grace}) \
                            OR (s.state = 'CREATED' AND s.created_at > {grace}))) \
             OR EXISTS (SELECT 1 FROM call_legs l \
                         WHERE l.call_id = p.call_id AND l.participant_id = p.id AND {LIVE_LEG}) \
             OR EXISTS (SELECT 1 FROM call_invitations i \
                         WHERE i.call_id = p.call_id AND i.participant_id = p.id \
                           AND i.status IN ('PENDING', 'RINGING') AND i.expires_at > {now})) \
            UNION ALL \
            SELECT l.id FROM call_legs l \
             WHERE l.call_id = {call} AND l.participant_id IS NULL AND {LIVE_LEG} \
         ) parties)"
    )
}

/// Whether the Call `call` has been idle (nothing connected) since `cutoff`.
fn idle_sql(call: &str, cutoff: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM call_sessions s WHERE s.call_id = {call} AND s.state = 'CONNECTED') \
          AND NOT EXISTS (SELECT 1 FROM call_legs l WHERE l.call_id = {call} \
                           AND l.leg_kind <> 'APPLICATION' AND l.state = 'CONNECTED') \
          AND GREATEST(c.updated_at, \
                (SELECT max(s.last_disconnected_at) FROM call_sessions s WHERE s.call_id = {call}), \
                (SELECT max(s.created_at) FROM call_sessions s WHERE s.call_id = {call}), \
                (SELECT max(l.ended_at) FROM call_legs l WHERE l.call_id = {call} \
                  AND l.leg_kind <> 'APPLICATION')) <= {cutoff})"
    )
}

/// The decision for one Call (`$1` call id, `$2` now − grace, `$3` now, `$4` now − idle,
/// `$5` now − maximum duration): the first rule that applies, if any.
fn decision_sql() -> String {
    format!(
        "SELECT CASE \
            WHEN COALESCE(c.connected_at, c.answered_at) <= $5 THEN 'MAX_DURATION' \
            WHEN c.current_state IN ('CONNECTED', 'HELD') AND {parties} < 2 THEN 'PARTICIPATION_ENDED' \
            WHEN c.current_state = 'ANSWERED' AND c.answered_at <= $4 THEN 'IDLE_TIMEOUT' \
            WHEN c.current_state IN ('CONNECTED', 'HELD') AND {idle} THEN 'IDLE_TIMEOUT' \
          END FROM calls c \
          WHERE c.id = $1 AND c.current_state IN ('ANSWERED', 'CONNECTED', 'HELD', 'TRANSFERRING')",
        parties = live_parties_sql("c.id", "$2", "$3"),
        idle = idle_sql("c.id", "$4"),
    )
}

impl AppCallService {
    /// The time windows of the rules at `now`: (now − grace, now, now − idle, now − max).
    fn windows(&self) -> [sea_orm::Value; 4] {
        let now = self.calls.now();
        [
            (now - self.config.participation_grace).into(),
            now.into(),
            (now - self.config.idle_call_timeout).into(),
            (now - self.config.max_call_duration).into(),
        ]
    }

    /// The automatic-end rule that applies to `call_id` now (read on `db`; under the Call
    /// row lock when `db` holds it).
    async fn auto_end_reason<C: ConnectionTrait>(
        &self,
        db: &C,
        call_id: Uuid,
    ) -> CallResult<Option<AutoEndReason>> {
        let mut values = vec![call_id.into()];
        values.extend(self.windows());
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                decision_sql(),
                values,
            ))
            .await?;
        let reason: Option<String> = match row {
            Some(row) => row.try_get_by_index(0)?,
            None => None,
        };
        Ok(match reason.as_deref() {
            Some("MAX_DURATION") => Some(AutoEndReason::MaxDuration),
            Some("PARTICIPATION_ENDED") => Some(AutoEndReason::ParticipationEnded),
            Some("IDLE_TIMEOUT") => Some(AutoEndReason::IdleTimeout),
            _ => None,
        })
    }

    /// Whether any session of the Call still has live participation (CONNECTED, or
    /// DISCONNECTED / CREATED within the grace). `None`: the Call has no session at all.
    pub(crate) async fn sessions_live(&self, call_id: Uuid) -> CallResult<Option<bool>> {
        let now = self.calls.now();
        let row = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*)::bigint, count(*) FILTER (WHERE state = 'CONNECTED' \
                    OR (state = 'DISCONNECTED' AND last_disconnected_at > $2) \
                    OR (state = 'CREATED' AND created_at > $2))::bigint \
                   FROM call_sessions WHERE call_id = $1",
                vec![
                    call_id.into(),
                    (now - self.config.participation_grace).into(),
                ],
            ))
            .await?;
        let Some(row) = row else { return Ok(None) };
        let total: i64 = row.try_get_by_index(0)?;
        let live: i64 = row.try_get_by_index(1)?;
        Ok((total > 0).then_some(live > 0))
    }

    /// Ends `call_id` when its participation is exhausted (`PARTICIPATION_ENDED`) — the
    /// event-driven evaluation after a session / connection / transferred party went away.
    /// Idle and maximum-duration rules are the reaper's. Returns whether it ended the Call.
    pub(crate) async fn end_if_participation_ended(&self, call_id: Uuid) -> CallResult<bool> {
        self.auto_end(call_id, false, |reason| {
            reason == AutoEndReason::ParticipationEnded
        })
        .await
    }

    /// Runs the after-a-departure evaluation and only logs a failure: the caller's own
    /// command already committed, and the reaper retries.
    pub(crate) async fn evaluate_participation(&self, call_id: Uuid) {
        if let Err(error) = self.end_if_participation_ended(call_id).await {
            tracing::warn!(%call_id, %error, "participation evaluation deferred to the reaper");
        }
    }

    /// Ends `call_id` through the END path if a rule accepted by `accept` applies under
    /// the Call row lock. `skip_locked`: a Call another instance holds is left for the
    /// next sweep (the reaper), instead of waiting for it.
    async fn auto_end(
        &self,
        call_id: Uuid,
        skip_locked: bool,
        accept: impl Fn(AutoEndReason) -> bool,
    ) -> CallResult<bool> {
        let txn = self.db().begin().await?;
        let query = calls::Entity::find_by_id(call_id);
        let call = if skip_locked {
            query
                .lock_with_behavior(
                    sea_orm::sea_query::LockType::Update,
                    sea_orm::sea_query::LockBehavior::SkipLocked,
                )
                .one(&txn)
                .await?
        } else {
            query.lock_exclusive().one(&txn).await?
        };
        let Some(call) = call else {
            txn.rollback().await?;
            return Ok(false);
        };
        if !matches!(
            call.current_state,
            CallState::Answered | CallState::Connected | CallState::Held | CallState::Transferring
        ) {
            txn.rollback().await?;
            return Ok(false);
        }
        let Some(reason) = self
            .auto_end_reason(&txn, call.id)
            .await?
            .filter(|r| accept(*r))
        else {
            txn.rollback().await?;
            return Ok(false);
        };
        let cmd = system_cmd(&call);
        let ending = self
            .calls
            .transition_with_details_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::RequestEnd,
                vec![("end_trigger", SafeValue::Text(reason.as_str().into()))],
            )
            .await?
            .call;
        let cancelled = self.end_effects_in(&txn, &cmd, &ending).await?;
        let pending = self.end_signals_in(&txn, &cmd, &ending, &cancelled).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_calls_auto_ended_total",
            &[("reason", reason.as_str())],
        );
        tracing::info!(
            call_id = %ending.id,
            from_state = %call.current_state,
            end_trigger = reason.as_str(),
            "call ended automatically"
        );
        self.publish(pending).await;
        if let Err(error) = self.finish_ending(ending.id).await {
            tracing::warn!(call_id = %ending.id, %error, "call cleanup pending; reconciliation will retry");
        }
        Ok(true)
    }

    /// The reaper (worker): ends answered / active Calls whose participation is exhausted
    /// past the grace, that have been idle past `idle_call_timeout`, or that exceeded
    /// `max_call_duration`. Candidates come from the partial index of non-terminal Calls;
    /// each is re-decided under its row lock (`SKIP LOCKED`: multi-instance safe).
    /// Returns how many Calls were ended.
    pub async fn reap_abandoned_calls(&self, limit: u64) -> CallResult<usize> {
        let mut values: Vec<sea_orm::Value> = self.windows().into();
        values.push((limit.clamp(1, 500) as i64).into());
        let sql = format!(
            "SELECT c.id FROM calls c \
              WHERE c.current_state NOT IN {TERMINAL_STATES_SQL} \
                AND c.current_state IN ('ANSWERED', 'CONNECTED', 'HELD', 'TRANSFERRING') \
                AND (COALESCE(c.connected_at, c.answered_at) <= $4 \
                  OR (c.current_state IN ('CONNECTED', 'HELD') AND {parties} < 2) \
                  OR (c.current_state = 'ANSWERED' AND c.answered_at <= $3) \
                  OR (c.current_state IN ('CONNECTED', 'HELD') AND {idle})) \
              ORDER BY c.updated_at LIMIT $5",
            parties = live_parties_sql("c.id", "$1", "$2"),
            idle = idle_sql("c.id", "$3"),
        );
        let rows = self
            .db()
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?;
        let mut ended = 0;
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            match self
                .auto_end(call_id, true, |_| true)
                .instrument(tracing::info_span!("call_reaper", call_id = %call_id))
                .await
            {
                Ok(true) => ended += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%call_id, %error, "automatic end not applied; retried next sweep");
                }
            }
        }
        Ok(ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_bounded_codes() {
        for (reason, code) in [
            (AutoEndReason::ParticipationEnded, "PARTICIPATION_ENDED"),
            (AutoEndReason::IdleTimeout, "IDLE_TIMEOUT"),
            (AutoEndReason::MaxDuration, "MAX_DURATION"),
        ] {
            assert_eq!(reason.as_str(), code);
        }
    }

    #[test]
    fn the_party_count_never_counts_the_shared_application_leg() {
        let sql = live_parties_sql("c.id", "$2", "$3");
        assert!(sql.contains("leg_kind <> 'APPLICATION'"));
        assert!(sql.contains("ended_at IS NULL"));
        assert!(decision_sql().contains("'TRANSFERRING'"));
    }
}
