//! Applying Telephony's normalized events (Part E §77–§81, §128–§130, §139).
//!
//! Telephony never sets Calling state: each event is validated against Calling's own
//! state machines. Events are deduplicated by id (`call_telephony_events`), applied in
//! per-leg sequence order (an older sequence is STALE and never regresses a leg), and an
//! event that does not match a public leg of the Call is UNMATCHED (never attached to an
//! arbitrary Call). Events for a terminal Call never resurrect it: they may only record
//! a terminal leg state and trigger cleanup of the execution.
//!
//! UNKNOWN (LEG_STATUS_UNKNOWN) records the attempt outcome as UNKNOWN — not a failure —
//! and changes nothing else: no failover, no new leg, until evidence resolves it.

use sea_orm::{
    ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QuerySelect, TransactionTrait,
};
use serde_json::Map;
use uuid::Uuid;

use super::{bridge, exec, stmt};
use crate::{
    calls::{
        app::{system_cmd, AppCallService, PendingSignals},
        errors::{CallError, CallResult},
        ids::{CallAttemptId, CallLegId, CallSessionId},
        kinds::{AttemptOutcome, FailureCategory, LegKind, LegState, SessionState},
        service::{attempt_of, leg_of, LegCost, LegUsageFact},
        state::CallState,
        transitions::{CallAction, ConnectedEvidence},
    },
    models::{call_legs, call_participants, call_sessions, calls},
    signaling::signals::SignalType,
    telephony::{
        events::{TelephonyEvent, TelephonyEventType},
        spend,
    },
};

/// What happened to a delivered event (Calling answers 200 for every one of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventDisposition {
    Applied,
    Duplicate,
    Stale,
    IgnoredTerminal,
    Unmatched,
}

impl EventDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventDisposition::Applied => "APPLIED",
            EventDisposition::Duplicate => "DUPLICATE",
            EventDisposition::Stale => "STALE",
            EventDisposition::IgnoredTerminal => "IGNORED_TERMINAL",
            EventDisposition::Unmatched => "UNMATCHED",
        }
    }
}

/// Work done after the event's transaction committed (idempotent; retried by workers).
#[derive(Debug, Default)]
struct After {
    dispatch: bool,
    finish_ending: bool,
    teardown: bool,
    release_legs: bool,
    confirm: bool,
}

/// A terminal public-leg outcome in Calling's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegOutcome {
    Completed,
    Busy,
    NoAnswer,
    Declined,
    Failed(FailureCategory),
}

impl LegOutcome {
    fn from_event(event: &TelephonyEvent) -> Self {
        match event.kind() {
            TelephonyEventType::LegBusy => LegOutcome::Busy,
            TelephonyEventType::LegNoAnswer => LegOutcome::NoAnswer,
            TelephonyEventType::LegDeclined => LegOutcome::Declined,
            TelephonyEventType::LegFailed => LegOutcome::Failed(category(
                event.failure_category.as_deref().unwrap_or("EXECUTION"),
            )),
            _ => match event.end_reason.as_deref() {
                Some("BUSY") => LegOutcome::Busy,
                Some("NO_ANSWER") => LegOutcome::NoAnswer,
                Some("DECLINED") => LegOutcome::Declined,
                Some("FAILED")
                | Some("TIMEOUT")
                | Some("ABSENT_AT_PROVIDER")
                | Some("NOT_SENT") => LegOutcome::Failed(category(
                    event.failure_category.as_deref().unwrap_or("EXECUTION"),
                )),
                _ => LegOutcome::Completed,
            },
        }
    }

    /// The canonical leg state of this outcome given whether the leg was answered.
    fn leg_state(&self, answered: bool) -> LegState {
        match (self, answered) {
            (LegOutcome::Failed(_), _) => LegState::Failed,
            (_, true) => LegState::Ended,
            (LegOutcome::Completed, false) => LegState::Cancelled,
            (LegOutcome::Busy, false) => LegState::Busy,
            (LegOutcome::NoAnswer, false) => LegState::NoAnswer,
            (LegOutcome::Declined, false) => LegState::Declined,
        }
    }
}

/// Telephony's failure vocabulary is Calling's (same names); anything else is EXECUTION.
fn category(s: &str) -> FailureCategory {
    match s {
        "TARGET_UNAVAILABLE" => FailureCategory::TargetUnavailable,
        "ROUTING" => FailureCategory::Routing,
        "TIMEOUT" => FailureCategory::Timeout,
        "MEDIA" => FailureCategory::Media,
        "NETWORK" => FailureCategory::Network,
        "INTERNAL" => FailureCategory::Internal,
        "UNKNOWN" => FailureCategory::Unknown,
        _ => FailureCategory::Execution,
    }
}

/// The path of leg states from `from` to `to` through the leg graph (empty: none).
fn leg_path(from: LegState, to: LegState) -> Vec<LegState> {
    use LegState::*;
    if from == to {
        return Vec::new();
    }
    if from.can_transition_to(to) {
        return vec![to];
    }
    let via: &[LegState] = match to {
        Answered => &[Dialing],
        Connected => &[Dialing, Answered],
        Busy | NoAnswer | Declined => &[Dialing],
        _ => &[],
    };
    let mut path = Vec::new();
    let mut at = from;
    for step in via {
        if at.can_transition_to(*step) && *step != at {
            path.push(*step);
            at = *step;
        }
    }
    if at.can_transition_to(to) {
        path.push(to);
        path
    } else {
        Vec::new()
    }
}

/// The public party role (CALLER / CALLEE) served by a public leg, if any.
async fn party_role<C: ConnectionTrait>(db: &C, leg_id: Uuid) -> CallResult<Option<String>> {
    Ok(db
        .query_one(stmt(
            "SELECT role FROM call_public_parties WHERE call_leg_id = $1",
            vec![leg_id.into()],
        ))
        .await?
        .map(|r| r.try_get_by_index::<String>(0))
        .transpose()?)
}

impl AppCallService {
    /// Applies one Telephony event (idempotent), inside a span naming its Call.
    pub async fn apply_telephony_event(
        &self,
        event: &TelephonyEvent,
    ) -> CallResult<EventDisposition> {
        use tracing::Instrument;
        let span = tracing::info_span!(
            "telephony_event",
            call_id = %event.call_id,
            event_type = %event.event_type,
        );
        self.apply_telephony_event_inner(event)
            .instrument(span)
            .await
    }

    async fn apply_telephony_event_inner(
        &self,
        event: &TelephonyEvent,
    ) -> CallResult<EventDisposition> {
        if !event.well_formed() {
            return Err(CallError::Validation("malformed telephony event"));
        }
        let txn = self.db().begin().await?;
        // The Call row lock serializes every event of one Call; the dedupe check below
        // is then exact.
        let call = calls::Entity::find_by_id(event.call_id)
            .lock_exclusive()
            .one(&txn)
            .await?;
        let seen = txn
            .query_one(stmt(
                "SELECT 1 FROM call_telephony_events WHERE event_id = $1",
                vec![event.event_id.into()],
            ))
            .await?
            .is_some();
        if seen {
            txn.commit().await?;
            return Ok(EventDisposition::Duplicate);
        }
        let mut after = After::default();
        let mut pending = PendingSignals::default();
        let disposition = match &call {
            None => EventDisposition::Unmatched,
            Some(call) => {
                self.apply_in(&txn, call, event, &mut after, &mut pending)
                    .await?
            }
        };
        exec(
            &txn,
            "INSERT INTO call_telephony_events (event_id, call_id, call_leg_id, telephony_leg_id, bridge_id, \
               event_type, leg_sequence, disposition, occurred_at, received_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            vec![
                event.event_id.into(),
                event.call_id.into(),
                event.call_leg_id.into(),
                event.telephony_leg_id.into(),
                event.bridge_id.into(),
                event.event_type.clone().into(),
                event.sequence.into(),
                match disposition {
                    EventDisposition::Duplicate => "APPLIED",
                    d => d.as_str(),
                }
                .into(),
                event.occurred_at.into(),
                self.calls.now().into(),
            ],
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        if let Some(call) = &call {
            tracing::info!(
                call_id = %call.id,
                correlation_id = call.correlation_id.as_deref().unwrap_or(""),
                event_type = %event.event_type,
                disposition = disposition.as_str(),
                "telephony event processed"
            );
        }
        if disposition != EventDisposition::Applied
            && disposition != EventDisposition::IgnoredTerminal
        {
            tracing::info!(event_id = %event.event_id, call_id = %event.call_id, event_type = %event.event_type, disposition = disposition.as_str(), "telephony event not applied");
        }
        self.run_after(event.call_id, after).await;
        Ok(disposition)
    }

    async fn run_after(&self, call_id: Uuid, after: After) {
        if after.release_legs {
            if let Err(error) = self.release_public_legs(call_id).await {
                tracing::warn!(%call_id, %error, "public leg release pending; reconciliation retries");
            }
        }
        if after.dispatch {
            self.dispatch_call(call_id).await;
        }
        if after.confirm {
            if let Err(error) = self.try_confirm_public_connected(call_id).await {
                tracing::warn!(%call_id, %error, "connected confirmation pending");
            }
        }
        if after.teardown {
            self.teardown_media(call_id).await;
        }
        if after.finish_ending {
            if let Err(error) = self.finish_ending(call_id).await {
                tracing::warn!(%call_id, %error, "call cleanup pending; reconciliation will retry");
            }
        }
    }

    async fn apply_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        event: &TelephonyEvent,
        after: &mut After,
        pending: &mut PendingSignals,
    ) -> CallResult<EventDisposition> {
        let kind = event.kind();
        // Bridge events.
        if matches!(
            kind,
            TelephonyEventType::BridgeEstablished | TelephonyEventType::BridgeEnded
        ) {
            return self.apply_bridge_event(txn, call, event, after).await;
        }
        let Some(leg_id) = event.call_leg_id else {
            return Ok(EventDisposition::Unmatched);
        };
        let leg = call_legs::Entity::find_by_id(leg_id)
            .filter(<call_legs::Column as sea_orm::ColumnTrait>::eq(
                &call_legs::Column::CallId,
                call.id,
            ))
            .one(txn)
            .await?;
        // Public legs, and (Part F) the bound PRIVATE_NETWORK leg of an enterprise SIP
        // device. An unbound private leg is never matched.
        let Some(leg) = leg.filter(|l| {
            l.leg_kind == LegKind::PublicNetwork
                || (l.leg_kind == LegKind::PrivateNetwork && l.execution_binding_ref.is_some())
        }) else {
            return Ok(EventDisposition::Unmatched);
        };
        let enterprise_device_leg = leg.leg_kind == LegKind::PrivateNetwork;
        // The execution must be THIS leg's (set once; an unbound leg binds now only if
        // Calling commanded its origination or it is an inbound leg Telephony offered).
        match (leg.execution_binding_ref, event.telephony_leg_id) {
            (Some(bound), Some(t)) if bound != t => return Ok(EventDisposition::Unmatched),
            (_, None) => return Ok(EventDisposition::Unmatched),
            (None, Some(t)) => {
                let commanded = txn
                    .query_one(stmt(
                        "SELECT 1 FROM call_telephony_commands WHERE call_leg_id = $1 AND kind = 'ORIGINATE' \
                           AND (telephony_ref IS NULL OR telephony_ref = $2)",
                        vec![leg.id.into(), t.into()],
                    ))
                    .await?
                    .is_some();
                if !commanded {
                    return Ok(EventDisposition::Unmatched);
                }
                exec(
                    txn,
                    "UPDATE call_legs SET execution_binding_ref = $2, updated_at = GREATEST(updated_at, $3) \
                     WHERE id = $1 AND execution_binding_ref IS NULL",
                    vec![leg.id.into(), t.into(), self.calls.now().into()],
                )
                .await?;
            }
            _ => {}
        }
        // Per-leg order: never apply an older sequence.
        if let Some(seq) = event.sequence {
            let last = txn
                .query_one(stmt(
                    "SELECT COALESCE(MAX(leg_sequence), 0) FROM call_telephony_events \
                     WHERE call_leg_id = $1 AND disposition = 'APPLIED'",
                    vec![leg.id.into()],
                ))
                .await?
                .map(|r| r.try_get_by_index::<i64>(0))
                .transpose()?
                .unwrap_or(0);
            if seq <= last {
                return Ok(EventDisposition::Stale);
            }
        }
        let leg = leg_of(txn, call, CallLegId::from_uuid(leg.id)).await?;
        let cmd = system_cmd(call);
        let correlation = cmd.correlation_id().as_str().to_string();
        let call_terminal = call.current_state.is_terminal();
        let attempt = match event.attempt_id {
            Some(a) => attempt_of(txn, call, CallAttemptId::from_uuid(a))
                .await
                .ok(),
            None => None,
        };
        match kind {
            TelephonyEventType::UsageFinalized => {
                if let Some(usage) = &event.usage {
                    let reserved = spend::reservation_of(txn, leg.id).await?;
                    let cost = usage
                        .cost_minor
                        .or(reserved.as_ref().map(|r| r.amount_minor))
                        .unwrap_or(0);
                    spend::settle_in(
                        txn,
                        leg.id,
                        cost,
                        usage.cost_known && usage.cost_minor.is_some(),
                        usage.billable_seconds.clamp(0, i64::from(i32::MAX)) as i32,
                        self.calls.now(),
                    )
                    .await?;
                    // Part G §69: the leg's usage fact (once per leg; a repeated or late
                    // provider usage event changes nothing). Cost evidence keeps its
                    // status: an unknown cost is UNKNOWN, never zero.
                    self.calls
                        .record_leg_usage_in(
                            txn,
                            &cmd,
                            call,
                            &LegUsageFact {
                                leg_id: leg.id,
                                connected_ms: usage.connected_duration_ms,
                                billable_seconds: usage.billable_seconds,
                                destination_country: usage.destination_country.clone(),
                                cost: LegCost::from_evidence(
                                    usage.cost_minor,
                                    usage.cost_known,
                                    usage.currency.as_deref(),
                                ),
                            },
                        )
                        .await?;
                }
                Ok(EventDisposition::Applied)
            }
            TelephonyEventType::LegStatusUnknown => {
                // Not a failure, not a success: the attempt's outcome is UNKNOWN until
                // Telephony's evidence (late event / reconciliation) resolves it.
                if let Some(a) = attempt {
                    if a.outcome.can_transition_to(AttemptOutcome::Unknown) {
                        self.calls
                            .finish_attempt_in(txn, &cmd, call, a, AttemptOutcome::Unknown, None)
                            .await?;
                    }
                }
                Ok(EventDisposition::Applied)
            }
            TelephonyEventType::LegAccepted
            | TelephonyEventType::LegRinging
            | TelephonyEventType::LegAnswered
            | TelephonyEventType::LegConnected => {
                if call_terminal {
                    // A late success must not resurrect the Call: clean the execution up.
                    after.release_legs = true;
                    return Ok(EventDisposition::IgnoredTerminal);
                }
                if leg.state.is_terminal() {
                    after.release_legs = true;
                    return Ok(EventDisposition::Stale);
                }
                let target = match kind {
                    TelephonyEventType::LegAccepted => LegState::Dialing,
                    TelephonyEventType::LegRinging => LegState::Ringing,
                    TelephonyEventType::LegAnswered => LegState::Answered,
                    _ => LegState::Connected,
                };
                let mut leg = leg;
                for step in leg_path(leg.state, target) {
                    leg = self
                        .calls
                        .move_leg_in(txn, &cmd, call, leg, step, None)
                        .await?;
                }
                let mut call = self.locked(txn, call).await?;
                if call.current_state == CallState::Ending {
                    after.release_legs = true;
                    return Ok(EventDisposition::Applied);
                }
                // Only the dialed CALLEE's leg rings / answers the Call. An inbound caller's
                // leg is answered by the edge when its bridge is built; a callback relay's
                // first leg (the requester) answering dials the callee.
                let role = party_role(txn, leg.id).await?;
                let drives_call = leg.direction == crate::calls::kinds::LegDirection::Outbound
                    && role.as_deref() == Some("CALLEE");
                if leg.direction == crate::calls::kinds::LegDirection::Outbound
                    && role.as_deref() == Some("CALLER")
                    && matches!(target, LegState::Answered | LegState::Connected)
                    && self.originate_callee_in(txn, &call).await?
                {
                    after.dispatch = true;
                }
                match target {
                    LegState::Ringing
                        if drives_call && call.current_state == CallState::Dialing =>
                    {
                        call = self
                            .calls
                            .transition_in(txn, &cmd, &call, call.version, CallAction::MarkRinging)
                            .await?
                            .call;
                        self.call_signal(
                            txn,
                            pending,
                            SignalType::Ringing,
                            &call,
                            &correlation,
                            Map::new(),
                        )
                        .await?;
                    }
                    LegState::Answered | LegState::Connected
                        if drives_call
                            && matches!(
                                call.current_state,
                                CallState::Dialing | CallState::Ringing
                            ) =>
                    {
                        call = self
                            .calls
                            .transition_in(txn, &cmd, &call, call.version, CallAction::Accept)
                            .await?
                            .call;
                        // An enterprise Call forwarded to a public contact: answered there.
                        self.enterprise_public_answered_in(txn, &call).await?;
                        self.call_signal(
                            txn,
                            pending,
                            SignalType::Answered,
                            &call,
                            &correlation,
                            Map::new(),
                        )
                        .await?;
                    }
                    _ => {}
                }
                if matches!(target, LegState::Answered | LegState::Connected)
                    && call.current_state == CallState::Answered
                {
                    // Media path: the answered public leg is bridged to the Call's other
                    // side (app RTC media, or the other public leg once it answered).
                    if let Some(t) = event.telephony_leg_id {
                        self.bridge_answered_in(txn, &call, t, after).await?;
                    }
                }
                if target == LegState::Connected {
                    if let Some(a) = attempt {
                        if a.outcome.can_transition_to(AttemptOutcome::Succeeded) {
                            self.calls
                                .finish_attempt_in(
                                    txn,
                                    &cmd,
                                    &call,
                                    a,
                                    AttemptOutcome::Succeeded,
                                    None,
                                )
                                .await?;
                        }
                    }
                    after.confirm = true;
                }
                Ok(EventDisposition::Applied)
            }
            TelephonyEventType::LegHeld | TelephonyEventType::LegResumed => {
                Ok(EventDisposition::Applied)
            }
            k if k.is_terminal_leg() => {
                let outcome = LegOutcome::from_event(event);
                let answered = leg.answered_at.is_some();
                let mut leg = leg;
                if !leg.state.is_terminal() {
                    let state = outcome.leg_state(answered);
                    let state = if leg_path(leg.state, state).is_empty() {
                        if answered {
                            LegState::Ended
                        } else {
                            LegState::Failed
                        }
                    } else {
                        state
                    };
                    for step in leg_path(leg.state, state) {
                        let cat = (step == LegState::Failed).then_some(match outcome {
                            LegOutcome::Failed(c) => c,
                            _ => FailureCategory::Execution,
                        });
                        leg = self
                            .calls
                            .move_leg_in(txn, &cmd, call, leg, step, cat)
                            .await?;
                    }
                }
                if let Some(a) = attempt {
                    let (o, c) = match (outcome, answered) {
                        (LegOutcome::Failed(c), _) => (AttemptOutcome::Failed, Some(c)),
                        (_, true) => (AttemptOutcome::Succeeded, None),
                        (LegOutcome::Completed, false) => (AttemptOutcome::Abandoned, None),
                        _ => (
                            AttemptOutcome::Failed,
                            Some(FailureCategory::TargetUnavailable),
                        ),
                    };
                    if a.outcome.can_transition_to(o) {
                        self.calls
                            .finish_attempt_in(txn, &cmd, call, a, o, c)
                            .await?;
                    }
                }
                if call_terminal {
                    return Ok(EventDisposition::IgnoredTerminal);
                }
                // A DEFINITE provider-side failure before connection may fail over to
                // another interconnect (new leg + attempt) when the plan permits.
                if !enterprise_device_leg
                    && super::failover::failover_candidate(event, &leg)
                    && leg.state == LegState::Failed
                {
                    let attempt = match event.attempt_id {
                        Some(a) => attempt_of(txn, call, CallAttemptId::from_uuid(a))
                            .await
                            .ok(),
                        None => None,
                    };
                    let call_now = self.locked(txn, call).await?;
                    if self
                        .failover_in(txn, &call_now, &leg, attempt.as_ref())
                        .await?
                    {
                        // Part G §66: participants learn the route changed (same Call).
                        let mut data = serde_json::Map::new();
                        data.insert("failover".into(), serde_json::json!(true));
                        let correlation =
                            system_cmd(&call_now).correlation_id().as_str().to_string();
                        self.call_signal(
                            txn,
                            pending,
                            SignalType::RouteChanged,
                            &call_now,
                            &correlation,
                            data,
                        )
                        .await?;
                        after.dispatch = true;
                        return Ok(EventDisposition::Applied);
                    }
                }
                // An enterprise device's dialog is the caller of its Call.
                let caller_leg = enterprise_device_leg
                    || party_role(txn, leg.id).await?.as_deref() == Some("CALLER");
                self.public_leg_terminal_in(txn, call, outcome, caller_leg, after, pending)
                    .await?;
                Ok(EventDisposition::Applied)
            }
            _ => Ok(EventDisposition::Applied),
        }
    }

    /// The Call-level effect of a public leg reaching a terminal outcome (Call not
    /// terminal). Before answer: the Call ends with the leg's outcome. After answer: the
    /// Call ends (hangup propagates; other legs are released).
    async fn public_leg_terminal_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        outcome: LegOutcome,
        caller_leg: bool,
        after: &mut After,
        pending: &mut PendingSignals,
    ) -> CallResult<()> {
        let call = self.locked(txn, call).await?;
        let cmd = system_cmd(&call);
        let correlation = cmd.correlation_id().as_str().to_string();
        after.release_legs = true;
        match call.current_state {
            // The CALLER (an inbound caller, or a callback requester) hung up before the
            // callee answered: the Call is cancelled.
            CallState::Dialing | CallState::Ringing | CallState::Routing
                if caller_leg && outcome == LegOutcome::Completed =>
            {
                let call = self
                    .calls
                    .transition_in(txn, &cmd, &call, call.version, CallAction::Cancel)
                    .await?;
                // Part F: an enterprise caller hung up while routed (queue, IVR, ring):
                // abandoned.
                self.enterprise_call_ended_in(txn, &call.call).await?;
                for invitation in &call.cancelled_invitations {
                    self.invitation_signal(
                        txn,
                        pending,
                        SignalType::RingCancelled,
                        &call.call,
                        invitation,
                        &correlation,
                    )
                    .await?;
                }
                self.call_signal(
                    txn,
                    pending,
                    SignalType::Cancelled,
                    &call.call,
                    &correlation,
                    Map::new(),
                )
                .await?;
                after.teardown = true;
            }
            CallState::Dialing | CallState::Ringing => {
                let action = match outcome {
                    LegOutcome::Busy => CallAction::MarkBusy,
                    LegOutcome::NoAnswer => CallAction::MarkNoAnswer,
                    LegOutcome::Declined => CallAction::Decline,
                    LegOutcome::Failed(c) if !c.is_policy() => CallAction::Fail { category: c },
                    _ => CallAction::Fail {
                        category: FailureCategory::TargetUnavailable,
                    },
                };
                let call = self
                    .calls
                    .transition_in(txn, &cmd, &call, call.version, action)
                    .await?
                    .call;
                if let Some(kind) = super::super::state_signal(call.current_state) {
                    self.call_signal(txn, pending, kind, &call, &correlation, Map::new())
                        .await?;
                }
                after.teardown = true;
            }
            // Answered but never connected (the media path was never established): the
            // Call fails with the leg's outcome; nothing was ever bridged.
            CallState::Answered if call.connected_at.is_none() => {
                let category = match outcome {
                    LegOutcome::Failed(c) if !c.is_policy() => c,
                    _ => FailureCategory::TargetUnavailable,
                };
                let call = self
                    .calls
                    .transition_in(
                        txn,
                        &cmd,
                        &call,
                        call.version,
                        CallAction::Fail { category },
                    )
                    .await?
                    .call;
                self.call_signal(
                    txn,
                    pending,
                    SignalType::Failed,
                    &call,
                    &correlation,
                    Map::new(),
                )
                .await?;
                after.teardown = true;
            }
            CallState::Answered
            | CallState::Connected
            | CallState::Held
            | CallState::Transferring => {
                let call = self
                    .calls
                    .transition_in(txn, &cmd, &call, call.version, CallAction::RequestEnd)
                    .await?
                    .call;
                self.call_signal(
                    txn,
                    pending,
                    SignalType::Ended,
                    &call,
                    &correlation,
                    Map::new(),
                )
                .await?;
                after.finish_ending = true;
            }
            CallState::Ending => after.finish_ending = true,
            _ => {}
        }
        Ok(())
    }

    async fn apply_bridge_event(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        event: &TelephonyEvent,
        after: &mut After,
    ) -> CallResult<EventDisposition> {
        let Some(telephony_bridge) = event.bridge_id else {
            return Ok(EventDisposition::Unmatched);
        };
        let row = bridge::BridgeRow::find_by_statement(stmt(
            &format!(
                "SELECT {} FROM call_public_bridges WHERE call_id = $1 AND telephony_bridge_id = $2",
                bridge::BRIDGE_COLUMNS
            ),
            vec![call.id.into(), telephony_bridge.into()],
        ))
        .one(txn)
        .await?;
        let Some(row) = row else {
            return Ok(EventDisposition::Unmatched);
        };
        let now = self.calls.now();
        if event.kind() == TelephonyEventType::BridgeEstablished {
            if call.current_state.is_terminal() {
                after.release_legs = true;
                return Ok(EventDisposition::IgnoredTerminal);
            }
            exec(
                txn,
                "UPDATE call_public_bridges SET status = 'ACTIVE', established_at = GREATEST($2, created_at), \
                   updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND status IN ('REQUESTED', 'ATTACHING')",
                vec![row.id.into(), now.into()],
            )
            .await?;
            after.confirm = true;
        } else {
            exec(
                txn,
                "UPDATE call_public_bridges SET status = 'ENDED', ended_at = GREATEST($2, created_at), \
                   updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND status IN ('REQUESTED', 'ATTACHING', 'ACTIVE')",
                vec![row.id.into(), now.into()],
            )
            .await?;
            if !call.current_state.is_terminal() {
                // The media path is gone: the Call ends (Calling orchestrates the legs).
                after.release_legs = true;
                let call = self.locked(txn, call).await?;
                if matches!(
                    call.current_state,
                    CallState::Answered | CallState::Connected | CallState::Held
                ) {
                    let cmd = system_cmd(&call);
                    self.calls
                        .transition_in(txn, &cmd, &call, call.version, CallAction::RequestEnd)
                        .await?;
                    after.finish_ending = true;
                }
            }
        }
        Ok(EventDisposition::Applied)
    }

    /// Requests the media bridge once the public leg answered (§79): APPLICATION_MEDIA
    /// for one public leg; PUBLIC_LEGS once two public legs are answered.
    async fn bridge_answered_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        _telephony_leg: Uuid,
        after: &mut After,
    ) -> CallResult<()> {
        let legs = call_legs::Entity::find()
            .filter(<call_legs::Column as sea_orm::ColumnTrait>::eq(
                &call_legs::Column::CallId,
                call.id,
            ))
            .all(txn)
            .await?;
        let public: Vec<&call_legs::Model> = legs
            .iter()
            .filter(|l| l.leg_kind == LegKind::PublicNetwork && !l.state.is_terminal())
            .collect();
        let answered =
            |l: &&call_legs::Model| matches!(l.state, LegState::Answered | LegState::Connected);
        let has_app_side = legs.iter().any(|l| l.leg_kind == LegKind::Application);
        let (kind, ready) = if has_app_side {
            (
                "APPLICATION_MEDIA",
                public.len() == 1 && public.iter().all(answered),
            )
        } else {
            // Two public legs: every OUTBOUND leg answered; an inbound caller's leg is
            // answered by the edge when the bridge is built.
            (
                "PUBLIC_LEGS",
                public.len() >= 2
                    && public.iter().all(|l| {
                        answered(l) || l.direction == crate::calls::kinds::LegDirection::Inbound
                    }),
            )
        };
        let members: Vec<Uuid> = public
            .iter()
            .filter_map(|l| l.execution_binding_ref)
            .collect();
        if !ready || members.len() != public.len() {
            return Ok(());
        }
        bridge::request_bridge_in(txn, call, kind, &members, self.calls.now()).await?;
        after.dispatch = true;
        Ok(())
    }

    /// ANSWERED → CONNECTED for a public Call on positive evidence only (§78): every
    /// live public leg CONNECTED (Telephony evidence), the bridge ACTIVE and — with an
    /// application side — the initiator's session connected.
    pub(crate) async fn try_confirm_public_connected(&self, call_id: Uuid) -> CallResult<bool> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(false);
        };
        if call.current_state != CallState::Answered {
            txn.commit().await?;
            return Ok(false);
        }
        let legs = call_legs::Entity::find()
            .filter(<call_legs::Column as sea_orm::ColumnTrait>::eq(
                &call_legs::Column::CallId,
                call.id,
            ))
            .all(&txn)
            .await?;
        // The edge-executed network legs: public legs, and (Part F) the bound
        // PRIVATE_NETWORK leg of an enterprise SIP device bridged to application media.
        let public: Vec<&call_legs::Model> = legs
            .iter()
            .filter(|l| {
                (l.leg_kind == LegKind::PublicNetwork
                    || (l.leg_kind == LegKind::PrivateNetwork && l.execution_binding_ref.is_some()))
                    && !l.state.is_terminal()
            })
            .collect();
        let bridge_active = bridge::live_bridge(&txn, call.id)
            .await?
            .is_some_and(|b| b.status == "ACTIVE");
        if public.is_empty()
            || public.iter().any(|l| l.state != LegState::Connected)
            || !bridge_active
        {
            txn.commit().await?;
            return Ok(false);
        }
        let leg_ids: Vec<CallLegId> = public.iter().map(|l| CallLegId::from_uuid(l.id)).collect();
        let app_leg = legs.iter().find(|l| l.leg_kind == LegKind::Application);
        let evidence = match app_leg {
            None => ConnectedEvidence::NetworkLegEstablished { leg_ids },
            Some(_) => {
                let participants: Vec<Uuid> = call_participants::Entity::find()
                    .filter(<call_participants::Column as sea_orm::ColumnTrait>::eq(
                        &call_participants::Column::CallId,
                        call.id,
                    ))
                    .all(&txn)
                    .await?
                    .into_iter()
                    .filter(|p| p.ended_at.is_none())
                    .map(|p| p.id)
                    .collect();
                let sessions: Vec<CallSessionId> = call_sessions::Entity::find()
                    .filter(<call_sessions::Column as sea_orm::ColumnTrait>::eq(
                        &call_sessions::Column::CallId,
                        call.id,
                    ))
                    .all(&txn)
                    .await?
                    .into_iter()
                    .filter(|s| {
                        s.state == SessionState::Connected
                            && participants.contains(&s.participant_id)
                    })
                    .map(|s| CallSessionId::from_uuid(s.id))
                    .collect();
                if sessions.is_empty() {
                    txn.commit().await?;
                    return Ok(false);
                }
                ConnectedEvidence::MixedBridgeEstablished {
                    session_ids: sessions,
                    leg_ids,
                }
            }
        };
        let cmd = system_cmd(&call);
        let call = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::ConfirmConnected { evidence },
            )
            .await?
            .call;
        if let Some(app) = app_leg {
            let app = leg_of(&txn, &call, CallLegId::from_uuid(app.id)).await?;
            for step in leg_path(app.state, LegState::Connected) {
                let current = leg_of(&txn, &call, CallLegId::from_uuid(app.id)).await?;
                self.calls
                    .move_leg_in(&txn, &cmd, &call, current, step, None)
                    .await?;
            }
        }
        let mut pending = PendingSignals::default();
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Connected,
            &call,
            cmd.correlation_id().as_str(),
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(call_id = %call.id, "public call connected on bridge + leg evidence");
        Ok(true)
    }

    /// A public leg failed definitively before Telephony executed it.
    pub(crate) async fn public_leg_failed(
        &self,
        call_id: Uuid,
        _leg_id: Uuid,
        category: FailureCategory,
    ) -> CallResult<()> {
        self.fail_public_call(call_id, category).await
    }

    /// Ends a public Call that cannot continue: before answer it FAILS with `category`;
    /// after answer it ENDS (ENDING → cleanup). Terminal Calls are untouched.
    pub(crate) async fn fail_public_call(
        &self,
        call_id: Uuid,
        category: FailureCategory,
    ) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(());
        };
        let mut after = After {
            release_legs: true,
            ..Default::default()
        };
        let mut pending = PendingSignals::default();
        if !call.current_state.is_terminal() {
            self.public_leg_terminal_in(
                &txn,
                &call,
                LegOutcome::Failed(category),
                false,
                &mut after,
                &mut pending,
            )
            .await?;
            let call = self.locked(&txn, &call).await?;
            if !call.current_state.is_terminal()
                && call.current_state != CallState::Ending
                && call.current_state.can_transition_to(CallState::Failed)
            {
                let cmd = system_cmd(&call);
                self.calls
                    .transition_in(
                        &txn,
                        &cmd,
                        &call,
                        call.version,
                        CallAction::Fail { category },
                    )
                    .await?;
                after.teardown = true;
            }
        }
        txn.commit().await?;
        self.publish(pending).await;
        self.run_after(call_id, after).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leg_paths_follow_the_canonical_graph() {
        use LegState::*;
        assert_eq!(
            leg_path(Created, Connected),
            vec![Dialing, Answered, Connected]
        );
        assert_eq!(leg_path(Ringing, Connected), vec![Answered, Connected]);
        assert_eq!(leg_path(Created, Busy), vec![Dialing, Busy]);
        assert_eq!(leg_path(Connected, Busy), Vec::<LegState>::new());
        assert_eq!(leg_path(Answered, Ended), vec![Ended]);
    }
}
