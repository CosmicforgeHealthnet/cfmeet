//! IVR execution on ONE canonical Call (Part F §57, §65–§75).
//!
//! ```text
//! router ─▶ EnterIvr ─▶ session ACTIVE at the start node of the flow's ACTIVE version
//!   (the Call is RINGING, context phase IVR; a SIP caller's leg is answered so prompts
//!    can play) ─▶ run nodes:
//!     PROMPT ─▶ play, next            MENU ─▶ play, await ONE key until the deadline
//!     ROUTE_* ─▶ router (same Call)   HANGUP ─▶ play, the Call ends (COMPLETED_IN_IVR)
//!   key (RTC keypad API / Telephony DTMF) ─▶ valid: branch │ invalid: retry or onInvalid
//!   deadline (worker) ─▶ retry or onTimeout
//! ```
//!
//! Calling owns the IVR business state; the media plane only plays BUILT-IN prompts and
//! reports keys: Telephony (Asterisk) for a SIP caller, the application client for an RTC
//! caller (it reads the prompts and options from the Call view / `call.ivr_waiting`).
//! The digits pressed are never logged, stored or used as metric labels: the session keeps
//! only the selected branch (a node id). Every session is bounded by the step budget
//! (`MAX_IVR_STEPS`), so a graph cycle through a menu can never loop forever.

use chrono::Duration;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, QuerySelect,
    TransactionTrait,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    context_of, exec, record_step, router, stmt, update_context, ContextRow, EnterprisePhase,
};
use crate::{
    calls::{
        app::{scope_of, system_cmd, AppCallService, PendingSignals},
        command::CallCommandContext,
        errors::{CallError, CallResult},
        kinds::FailureCategory,
        state::CallState,
        transitions::CallAction,
    },
    enterprise::{
        destinations::{Destination, DestinationType},
        ivr::{self, BuiltinPrompt, FlowRow, IvrGraph, IvrNode, MAX_IVR_STEPS},
        policy::{effective_policy, EffectivePolicy},
    },
    models::{call_invitations, call_legs, calls},
    signaling::signals::SignalType,
};

type Rung = (calls::Model, Vec<call_invitations::Model>);

/// Who supplies the caller's keys / hears the prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IvrInputSource {
    /// The RTC caller's own endpoint (keypad API).
    Application,
    /// Telephony's normalized DTMF of the caller's SIP leg.
    Sip,
}

impl IvrInputSource {
    fn as_str(&self) -> &'static str {
        match self {
            IvrInputSource::Application => "APPLICATION",
            IvrInputSource::Sip => "SIP",
        }
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct SessionRow {
    pub id: Uuid,
    pub flow_id: Uuid,
    pub flow_version_id: Uuid,
    pub input_source: String,
    pub state: String,
    pub current_node: String,
    pub retries: i16,
    pub steps: i16,
    pub awaiting_input: bool,
    pub input_deadline: Option<chrono::DateTime<chrono::Utc>>,
    pub selected_branch: Option<String>,
    pub last_prompts: Vec<String>,
}

const SESSION_COLUMNS: &str = "id, flow_id, flow_version_id, input_source, state, \
    current_node, retries, steps, awaiting_input, input_deadline, selected_branch, last_prompts";

pub(crate) async fn session<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    lock: bool,
) -> CallResult<Option<SessionRow>> {
    Ok(SessionRow::find_by_statement(stmt(
        &format!(
            "SELECT {SESSION_COLUMNS} FROM enterprise_ivr_sessions WHERE id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

/// The mutable part of a session while nodes run inside one transaction.
struct Cursor {
    node: String,
    retries: i16,
    steps: i16,
    prompts: Vec<String>,
    branch: Option<String>,
    /// Nominal seconds of the prompts queued in this step (input timers start after).
    playback: i64,
    /// The HANGUP node's prompt has been played: end the Call now.
    farewell_played: bool,
}

impl Cursor {
    fn at(session: &SessionRow, retries: i16) -> Self {
        Cursor {
            node: session.current_node.clone(),
            retries,
            steps: session.steps,
            prompts: Vec::new(),
            branch: None,
            playback: 0,
            farewell_played: false,
        }
    }
}

/// What running the graph led to.
enum Outcome {
    /// Waiting for one key until the deadline.
    Awaiting(chrono::DateTime<chrono::Utc>),
    /// A ROUTE_* node: the router takes the Call to this destination.
    Route(Destination),
    /// A HANGUP node whose prompt is playing: the Call ends at the deadline.
    Farewell(chrono::DateTime<chrono::Utc>),
    /// A HANGUP node.
    Hangup,
    /// The step budget ran out (a misconfigured cycle): fail safe.
    Exhausted,
}

/// The caller's SIP leg of an enterprise device Call (bound to its Telephony leg).
pub(crate) async fn sip_leg<C: ConnectionTrait>(
    db: &C,
    call: &calls::Model,
) -> CallResult<Option<call_legs::Model>> {
    if call.origin != "ENTERPRISE_DEVICE" {
        return Ok(None);
    }
    use sea_orm::{ColumnTrait, QueryFilter};
    Ok(call_legs::Entity::find()
        .filter(call_legs::Column::CallId.eq(call.id))
        .filter(call_legs::Column::LegKind.eq(crate::calls::kinds::LegKind::PrivateNetwork))
        .filter(call_legs::Column::Direction.eq(crate::calls::kinds::LegDirection::Inbound))
        .one(db)
        .await?)
}

impl AppCallService {
    /// RouteAction::EnterIvr: the Call enters the flow's ACTIVE version (pinned for the
    /// whole session) and the graph runs until it waits for a key or routes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn enter_ivr(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        context: &ContextRow,
        policy: &EffectivePolicy,
        flow: FlowRow,
        destination: Destination,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        let Some(version_id) = flow.active_version_id else {
            record_step(
                txn,
                call,
                "NO_ROUTE",
                Some(destination),
                "IVR_NOT_PUBLISHED",
                None,
                now,
            )
            .await?;
            return self
                .enterprise_no_route(txn, cmd, call, context, FailureCategory::Routing)
                .await;
        };
        let version = ivr::version_row(txn, version_id)
            .await
            .map_err(super::enterprise_error)?;
        let graph = version.graph().map_err(super::enterprise_error)?;
        let channel = if sip_leg(txn, call).await?.is_some() {
            IvrInputSource::Sip
        } else {
            IvrInputSource::Application
        };
        let session_id = Uuid::new_v4();
        exec(
            txn,
            "INSERT INTO enterprise_ivr_sessions (id, tenant_id, application_id, environment_id, \
             call_id, flow_id, flow_version_id, input_source, state, current_node, retries, steps, \
             awaiting_input, last_prompts, started_at, version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'ACTIVE', $9, 0, 0, FALSE, '{}', $10, 1)",
            vec![
                session_id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                call.id.into(),
                flow.id.into(),
                version_id.into(),
                channel.as_str().into(),
                graph.start_node.clone().into(),
                now.into(),
            ],
        )
        .await?;
        exec(
            txn,
            "UPDATE enterprise_call_contexts SET ivr_session_id = $2, version = version + 1, \
             updated_at = GREATEST(updated_at, $3) WHERE call_id = $1",
            vec![call.id.into(), session_id.into(), now.into()],
        )
        .await?;
        let call = self.mark_ringing(txn, cmd, call, leg).await?;
        update_context(
            txn,
            context,
            EnterprisePhase::Ivr,
            Some(destination),
            None,
            now,
        )
        .await?;
        record_step(
            txn,
            &call,
            "IVR_ENTER",
            Some(destination),
            "DESTINATION_RESOLVED",
            None,
            now,
        )
        .await?;
        crate::metrics::inc(
            "calling_enterprise_ivr_entries_total",
            &[("channel", channel.as_str())],
        );
        // A SIP caller's dialog is answered by the edge so prompts can play (internal,
        // no carrier, no charge). The canonical Call stays pre-answer (RINGING).
        if let Some(sip) = sip_leg(txn, &call).await? {
            self.enqueue_sip_answer_in(txn, &call, &sip).await?;
        }
        let cursor = Cursor {
            node: graph.start_node.clone(),
            retries: 0,
            steps: 0,
            prompts: Vec::new(),
            branch: None,
            playback: 0,
            farewell_played: false,
        };
        let session = session(txn, session_id, true)
            .await?
            .ok_or(CallError::NotFound)?;
        self.run_ivr(txn, cmd, &call, leg, policy, &session, &graph, cursor)
            .await
    }

    /// Runs nodes from the cursor until the graph waits, routes or ends; persists the
    /// session and executes the outcome on the SAME Call.
    #[allow(clippy::too_many_arguments)]
    async fn run_ivr(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        policy: &EffectivePolicy,
        session: &SessionRow,
        graph: &IvrGraph,
        mut cursor: Cursor,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        let sip = sip_leg(txn, call).await?;
        let outcome = loop {
            if cursor.steps >= MAX_IVR_STEPS {
                break Outcome::Exhausted;
            }
            cursor.steps += 1;
            let Some(node) = graph.node(&cursor.node) else {
                break Outcome::Exhausted;
            };
            match node {
                IvrNode::Prompt { prompt, next, .. } => {
                    self.ivr_play(txn, call, sip.as_ref(), &mut cursor, prompt.builtin)
                        .await?;
                    cursor.node = next.clone();
                }
                IvrNode::Menu {
                    prompt,
                    input_timeout_seconds,
                    ..
                } => {
                    self.ivr_play(txn, call, sip.as_ref(), &mut cursor, prompt.builtin)
                        .await?;
                    break Outcome::Awaiting(
                        now + Duration::seconds(cursor.playback + *input_timeout_seconds as i64),
                    );
                }
                IvrNode::Hangup { prompt, .. } => match prompt {
                    Some(p) if !cursor.farewell_played => {
                        self.ivr_play(txn, call, sip.as_ref(), &mut cursor, p.builtin)
                            .await?;
                        break Outcome::Farewell(now + Duration::seconds(cursor.playback));
                    }
                    _ => break Outcome::Hangup,
                },
                route => match route.destination() {
                    Some(dest) => {
                        cursor.branch = Some(route.id().to_string());
                        break Outcome::Route(dest);
                    }
                    None => break Outcome::Exhausted,
                },
            }
        };
        let (state, awaiting, deadline, end_reason) = match &outcome {
            Outcome::Awaiting(at) => ("ACTIVE", true, Some(*at), None),
            Outcome::Farewell(at) => ("ACTIVE", false, Some(*at), None),
            Outcome::Route(_) => ("ROUTED", false, None, Some("ROUTED")),
            Outcome::Hangup => ("COMPLETED", false, None, Some("HANGUP_NODE")),
            Outcome::Exhausted => ("FAILED", false, None, Some("STEP_BUDGET_EXHAUSTED")),
        };
        exec(
            txn,
            "UPDATE enterprise_ivr_sessions SET state = $2, current_node = $3, retries = $4, \
             steps = $5, awaiting_input = $6, input_deadline = $7, selected_branch = COALESCE($8, selected_branch), \
             last_prompts = $9, end_reason = $10, \
             ended_at = CASE WHEN $2 = 'ACTIVE' THEN NULL ELSE GREATEST(started_at, $11) END, \
             version = version + 1 WHERE id = $1",
            vec![
                session.id.into(),
                state.into(),
                cursor.node.clone().into(),
                cursor.retries.into(),
                cursor.steps.into(),
                awaiting.into(),
                deadline.into(),
                cursor.branch.clone().into(),
                cursor
                    .prompts
                    .iter()
                    .rev()
                    .take(8)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .into(),
                end_reason.into(),
                now.into(),
            ],
        )
        .await?;
        let context = context_of(txn, call.id).await?.ok_or(CallError::NotFound)?;
        let flow_dest = Destination::new(DestinationType::Ivr, session.flow_id);
        match outcome {
            Outcome::Awaiting(_) | Outcome::Farewell(_) => {
                let call = self.locked(txn, call).await?;
                Ok((call, Vec::new()))
            }
            Outcome::Route(dest) => {
                record_step(
                    txn,
                    call,
                    "IVR_ROUTE",
                    Some(dest),
                    "BRANCH_SELECTED",
                    None,
                    now,
                )
                .await?;
                crate::metrics::inc(
                    "calling_enterprise_ivr_results_total",
                    &[("result", "ROUTED"), ("route_type", dest.kind.as_str())],
                );
                let action = router::resolve(
                    txn,
                    &self.principals,
                    scope_of(call),
                    policy,
                    &context,
                    dest,
                    now,
                )
                .await?;
                Box::pin(self.apply_route_action(txn, cmd, call, leg, &context, policy, action))
                    .await
            }
            Outcome::Hangup => {
                crate::metrics::inc(
                    "calling_enterprise_ivr_results_total",
                    &[("result", "HANGUP"), ("route_type", "NONE")],
                );
                record_step(
                    txn,
                    call,
                    "IVR_HANGUP",
                    Some(flow_dest),
                    "HANGUP_NODE",
                    None,
                    now,
                )
                .await?;
                self.ivr_end_call(txn, cmd, call, &context, "COMPLETED_IN_IVR")
                    .await
            }
            Outcome::Exhausted => {
                crate::metrics::inc(
                    "calling_enterprise_ivr_results_total",
                    &[("result", "STEP_BUDGET_EXHAUSTED"), ("route_type", "NONE")],
                );
                tracing::warn!(call_id = %call.id, "IVR step budget exhausted; failing safe");
                record_step(
                    txn,
                    call,
                    "NO_ROUTE",
                    Some(flow_dest),
                    "IVR_STEP_BUDGET_EXHAUSTED",
                    None,
                    now,
                )
                .await?;
                self.enterprise_no_route(txn, cmd, call, &context, FailureCategory::Routing)
                    .await
            }
        }
    }

    /// The IVR ended the Call itself (HANGUP node): DECLINED with the business outcome
    /// COMPLETED_IN_IVR (the caller was served by the IVR; nobody was rung).
    async fn ivr_end_call(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
        outcome: &'static str,
    ) -> CallResult<Rung> {
        let call = self.locked(txn, call).await?;
        let call = self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::Decline)
            .await?
            .call;
        update_context(
            txn,
            context,
            EnterprisePhase::Ended,
            None,
            Some(outcome),
            self.calls.now(),
        )
        .await?;
        Ok((call, Vec::new()))
    }

    /// Plays a built-in prompt: a durable PROMPT command on a SIP caller's leg; for an RTC
    /// caller the prompt is part of the IVR view (the client plays its own copy).
    async fn ivr_play(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        sip: Option<&call_legs::Model>,
        cursor: &mut Cursor,
        prompt: BuiltinPrompt,
    ) -> CallResult<()> {
        cursor.prompts.push(prompt.as_str().to_string());
        cursor.playback += prompt.nominal_seconds();
        if let Some(leg) = sip {
            self.enqueue_sip_prompt_in(txn, call, leg, prompt, cursor.steps)
                .await?;
        }
        Ok(())
    }

    /// One key from the caller (RTC keypad or SIP DTMF). The digit is used once, here,
    /// and never logged or stored. Returns `Ok(false)` when the Call does not wait for a
    /// key (stale input is ignored).
    pub(crate) async fn ivr_input(
        &self,
        call_id: Uuid,
        digit: &str,
        source: IvrInputSource,
    ) -> CallResult<bool> {
        if !matches!(digit.as_bytes(), [b'0'..=b'9' | b'*' | b'#']) {
            return Err(CallError::Validation("digit must be one key: 0-9, * or #."));
        }
        let txn = self.db().begin().await?;
        let call = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        let Some(context) = context_of(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        let Some(session_id) = context
            .ivr_session_id
            .filter(|_| context.phase == EnterprisePhase::Ivr.as_str())
        else {
            txn.rollback().await?;
            return Ok(false);
        };
        let Some(session) = session(&txn, session_id, true).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        let now = self.calls.now();
        if session.state != "ACTIVE"
            || !session.awaiting_input
            || session.input_source != source.as_str()
            || call.current_state != CallState::Ringing
            || session.input_deadline.is_some_and(|d| d <= now)
        {
            txn.rollback().await?;
            return Ok(false);
        }
        let version = ivr::version_row(&txn, session.flow_version_id)
            .await
            .map_err(super::enterprise_error)?;
        let graph = version.graph().map_err(super::enterprise_error)?;
        let Some(IvrNode::Menu {
            options,
            max_retries,
            on_invalid,
            ..
        }) = graph.node(&session.current_node).cloned()
        else {
            txn.rollback().await?;
            return Ok(false);
        };
        let chosen = options
            .iter()
            .find(|o| o.digit == digit)
            .map(|o| o.next.clone());
        let mut cursor = Cursor::at(&session, session.retries);
        let valid = chosen.is_some();
        match chosen {
            Some(next) => {
                cursor.branch = Some(next.clone());
                cursor.node = next;
                cursor.retries = 0;
            }
            None => {
                cursor.retries += 1;
                if cursor.retries > max_retries as i16 {
                    cursor.node = on_invalid;
                    cursor.retries = 0;
                } else {
                    let sip = sip_leg(&txn, &call).await?;
                    self.ivr_play(
                        &txn,
                        &call,
                        sip.as_ref(),
                        &mut cursor,
                        BuiltinPrompt::InvalidOption,
                    )
                    .await?;
                }
            }
        }
        crate::metrics::inc(
            "calling_enterprise_ivr_inputs_total",
            &[
                ("result", if valid { "VALID" } else { "INVALID" }),
                ("channel", source.as_str()),
            ],
        );
        let cmd = system_cmd(&call);
        let outcome = self
            .ivr_continue_in(&txn, &cmd, &call, &context, &session, &graph, cursor)
            .await?;
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        let mut data = Map::new();
        data.insert("valid".into(), json!(valid));
        self.initiator_signal(
            &txn,
            &mut pending,
            SignalType::IvrInput,
            &outcome.0,
            &correlation,
            data,
        )
        .await?;
        self.ivr_outcome_signals(&txn, &mut pending, &outcome, &correlation)
            .await?;
        txn.commit().await?;
        self.publish(pending).await;
        self.ivr_after_commit(&outcome.0).await;
        Ok(true)
    }

    /// The RTC keypad: `endpoint_id` must be the Call's initiator endpoint in `scope`
    /// (another scope's Call is 404). Returns whether the IVR took the key.
    pub async fn ivr_keypad(
        &self,
        scope: crate::calls::command::CallScope,
        call_id: Uuid,
        endpoint_id: Uuid,
        digit: &str,
    ) -> CallResult<bool> {
        let owner = self
            .db()
            .query_one(stmt(
                "SELECT 1 FROM calls c JOIN call_participants p ON p.call_id = c.id AND p.role = 'INITIATOR' \
                   JOIN call_sessions s ON s.participant_id = p.id AND s.endpoint_id = $5 \
                  WHERE c.id = $1 AND c.tenant_id = $2 AND c.application_id = $3 AND c.environment_id = $4",
                vec![
                    call_id.into(),
                    scope.tenant_id().as_uuid().into(),
                    scope.application_id().as_uuid().into(),
                    scope.environment_id().as_uuid().into(),
                    endpoint_id.into(),
                ],
            ))
            .await?;
        if owner.is_none() {
            return Err(CallError::NotFound);
        }
        self.ivr_input(call_id, digit, IvrInputSource::Application)
            .await
    }

    /// Continues a waiting session from `cursor` (inside the Call's transaction).
    #[allow(clippy::too_many_arguments)]
    async fn ivr_continue_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
        session: &SessionRow,
        graph: &IvrGraph,
        cursor: Cursor,
    ) -> CallResult<Rung> {
        let policy = effective_policy(txn, scope_of(call), context.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        let leg = self.rtc_leg(txn, call).await?;
        self.run_ivr(txn, cmd, call, &leg, &policy, session, graph, cursor)
            .await
    }

    /// Signals after an IVR step: offers / state for a routed Call, `call.ivr_waiting`
    /// to the caller while a menu waits.
    async fn ivr_outcome_signals(
        &self,
        txn: &DatabaseTransaction,
        pending: &mut PendingSignals,
        (call, offers): &Rung,
        correlation: &str,
    ) -> CallResult<()> {
        if !offers.is_empty() || call.current_state.is_terminal() {
            return self
                .enterprise_ring_signals(txn, pending, call, offers, correlation)
                .await;
        }
        if self.waiting_in(txn, call).await? {
            return self.queued_signal(txn, pending, call, correlation).await;
        }
        self.ivr_waiting_signal(txn, pending, call, correlation)
            .await
    }

    /// `call.ivr_waiting` to the caller of a Call whose IVR waits for a key.
    pub(crate) async fn ivr_waiting_signal<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        call: &calls::Model,
        correlation: &str,
    ) -> CallResult<()> {
        let Some(context) = context_of(db, call.id).await? else {
            return Ok(());
        };
        let Some(session_id) = context
            .ivr_session_id
            .filter(|_| context.phase == EnterprisePhase::Ivr.as_str())
        else {
            return Ok(());
        };
        let Some(session) = session(db, session_id, false).await? else {
            return Ok(());
        };
        if !session.awaiting_input {
            return Ok(());
        }
        let mut data = Map::new();
        data.insert("ivrSessionId".into(), json!(session.id));
        data.insert("awaitingInput".into(), json!(true));
        data.insert("retries".into(), json!(session.retries));
        self.initiator_signal(db, pending, SignalType::IvrWaiting, call, correlation, data)
            .await
    }

    /// Post-commit work of an IVR step: a terminal Call's media goes; a queued Call's
    /// queue is dispatched; SIP commands are dispatched.
    pub(crate) async fn ivr_after_commit(&self, call: &calls::Model) {
        if call.current_state.is_terminal() {
            self.teardown_media(call.id).await;
            if let Err(error) = self.release_public_legs(call.id).await {
                tracing::warn!(call_id = %call.id, %error, "leg release pending; reconciliation retries");
            }
        }
        self.enterprise_after_commit(call.id).await;
        self.dispatch_call(call.id).await;
    }

    /// Worker pass: sessions whose input deadline passed retry the menu (NO_INPUT) or
    /// continue to `onTimeout`; a HANGUP whose prompt has played ends the Call. Each under
    /// its Call's row lock (SKIP LOCKED).
    pub(crate) async fn ivr_tick(&self, limit: u64) -> CallResult<usize> {
        let now = self.calls.now();
        let due = self
            .db()
            .query_all(stmt(
                "SELECT id, call_id FROM enterprise_ivr_sessions \
                  WHERE state = 'ACTIVE' AND input_deadline <= $1 \
                  ORDER BY input_deadline LIMIT $2",
                vec![now.into(), (limit as i64).into()],
            ))
            .await?;
        let mut done = 0;
        for row in due {
            let session_id: Uuid = row.try_get_by_index(0)?;
            let call_id: Uuid = row.try_get_by_index(1)?;
            if self.ivr_timeout(session_id, call_id).await? {
                done += 1;
            }
        }
        Ok(done)
    }

    async fn ivr_timeout(&self, session_id: Uuid, call_id: Uuid) -> CallResult<bool> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
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
        let Some(session) = session(&txn, session_id, true).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        let now = self.calls.now();
        if session.state != "ACTIVE" || session.input_deadline.is_none_or(|d| d > now) {
            txn.commit().await?;
            return Ok(false);
        }
        let Some(context) = context_of(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(false);
        };
        if call.current_state.is_terminal() || call.current_state != CallState::Ringing {
            self.ivr_session_ended_in(&txn, &call, &context).await?;
            txn.commit().await?;
            return Ok(true);
        }
        let version = ivr::version_row(&txn, session.flow_version_id)
            .await
            .map_err(super::enterprise_error)?;
        let graph = version.graph().map_err(super::enterprise_error)?;
        let cursor = match graph.node(&session.current_node).cloned() {
            Some(IvrNode::Menu {
                max_retries,
                on_timeout,
                ..
            }) if session.awaiting_input => {
                let mut cursor = Cursor::at(&session, session.retries + 1);
                if cursor.retries > max_retries as i16 {
                    cursor.node = on_timeout;
                    cursor.retries = 0;
                } else {
                    let sip = sip_leg(&txn, &call).await?;
                    self.ivr_play(
                        &txn,
                        &call,
                        sip.as_ref(),
                        &mut cursor,
                        BuiltinPrompt::NoInput,
                    )
                    .await?;
                }
                crate::metrics::inc("calling_enterprise_ivr_timeouts_total", &[]);
                cursor
            }
            Some(IvrNode::Hangup { .. }) if !session.awaiting_input => Cursor {
                farewell_played: true,
                ..Cursor::at(&session, session.retries)
            },
            _ => {
                txn.rollback().await?;
                return Ok(false);
            }
        };
        let cmd = system_cmd(&call);
        let outcome = self
            .ivr_continue_in(&txn, &cmd, &call, &context, &session, &graph, cursor)
            .await?;
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        self.ivr_outcome_signals(&txn, &mut pending, &outcome, &correlation)
            .await?;
        txn.commit().await?;
        self.publish(pending).await;
        self.ivr_after_commit(&outcome.0).await;
        Ok(true)
    }

    /// The Call left the IVR by another path (caller hung up, failure): the session ends
    /// (ABANDONED when the caller cancelled). Idempotent; under the Call row lock.
    /// Returns whether the IVR had already finished with the caller (its HANGUP prompt was
    /// playing): the session then completes as the HANGUP node.
    pub(crate) async fn ivr_session_ended_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        context: &ContextRow,
    ) -> CallResult<bool> {
        let Some(session_id) = context.ivr_session_id else {
            return Ok(false);
        };
        let Some(current) = session(txn, session_id, true).await? else {
            return Ok(false);
        };
        let farewell = current.state == "ACTIVE"
            && !current.awaiting_input
            && current.input_deadline.is_some();
        let (state, reason) = if farewell {
            ("COMPLETED", "HANGUP_NODE")
        } else if call.current_state == CallState::Cancelled {
            ("ABANDONED", "CALLER_HUNG_UP")
        } else {
            ("FAILED", "CALL_ENDED")
        };
        let changed = exec(
            txn,
            "UPDATE enterprise_ivr_sessions SET state = $2, end_reason = $3, awaiting_input = FALSE, \
             ended_at = GREATEST(started_at, $4), version = version + 1 \
             WHERE id = $1 AND state = 'ACTIVE'",
            vec![
                session_id.into(),
                state.into(),
                reason.into(),
                self.calls.now().into(),
            ],
        )
        .await?;
        if changed > 0 {
            crate::metrics::inc(
                "calling_enterprise_ivr_results_total",
                &[
                    ("result", if farewell { "HANGUP" } else { state }),
                    ("route_type", "NONE"),
                ],
            );
        }
        Ok(farewell && changed > 0)
    }

    /// The safe IVR block of the Call view: what the caller hears and may press (never the
    /// keys pressed).
    pub(crate) async fn ivr_view<C: ConnectionTrait>(
        &self,
        db: &C,
        session_id: Uuid,
    ) -> CallResult<Option<Value>> {
        let Some(s) = session(db, session_id, false).await? else {
            return Ok(None);
        };
        let mut options: Vec<String> = Vec::new();
        let mut timeout = None;
        if s.awaiting_input {
            if let Ok(version) = ivr::version_row(db, s.flow_version_id).await {
                if let Ok(graph) = version.graph() {
                    if let Some(IvrNode::Menu {
                        options: o,
                        input_timeout_seconds,
                        ..
                    }) = graph.node(&s.current_node)
                    {
                        options = o.iter().map(|o| o.digit.clone()).collect();
                        timeout = Some(*input_timeout_seconds);
                    }
                }
            }
        }
        Ok(Some(json!({
            "ivrFlowId": s.flow_id,
            "state": s.state,
            "awaitingInput": s.awaiting_input,
            "prompts": s.last_prompts,
            "options": options,
            "inputTimeoutSeconds": timeout,
            "inputDeadline": s.input_deadline.map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            "retries": s.retries,
            "selectedBranch": s.selected_branch,
        })))
    }
}
