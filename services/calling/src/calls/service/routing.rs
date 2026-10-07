//! Routes, legs and attempts: provider-independent persistence primitives (Part C §16,
//! §21). No routing algorithm, carrier selection, failover or provider execution happens
//! here; the application-call orchestration composes these primitives inside its own
//! transactions (`*_in` variants), the public methods run each in a transaction of its own.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    TransactionTrait,
};
use uuid::Uuid;

use super::{data, scalar_i64, text, CallService, EventDraft};
use crate::{
    calls::{
        attempts::{CallAttempt, NewAttempt},
        attributes::SafeValue,
        command::CallCommandContext,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallAttemptId, CallId, CallLegId, CallRouteId},
        kinds::{AttemptOutcome, FailureCategory, LegState, RouteState},
        legs::{CallLeg, NewLeg},
        routes::{CallRoute, NewRoute},
    },
    models::{call_attempts, call_legs, call_participants, call_routes, calls},
};

impl CallService {
    /// Records a routing decision for a non-terminal Call (state SELECTED).
    pub async fn record_route(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewRoute,
    ) -> CallResult<CallRoute> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let route = self.record_route_in(&txn, cmd, &call, new).await?;
        txn.commit().await?;
        Ok(route.into())
    }

    pub(crate) async fn record_route_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewRoute,
    ) -> CallResult<call_routes::Model> {
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        let at = self.now().max(call.updated_at);
        let route = call_routes::ActiveModel {
            id: Set(CallRouteId::generate().as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            route_class: Set(new.route_class),
            state: Set(RouteState::Selected),
            policy_version: Set(new.policy_version.as_str().to_string()),
            decision_snapshot: Set(new.decision_snapshot.to_json()),
            selected_at: Set(at),
            ended_at: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallRouteRecorded,
                data(vec![
                    ("route_id", text(route.id)),
                    ("route_class", text(route.route_class)),
                    ("policy_version", text(&route.policy_version)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(route)
    }

    /// Moves a route along its state graph. After the Call is terminal only terminal
    /// route states may still be recorded (closing evidence).
    pub async fn update_route_state(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        route_id: CallRouteId,
        target: RouteState,
    ) -> CallResult<CallRoute> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let route = route_of(&txn, &call, route_id).await?;
        let route = self.move_route_in(&txn, cmd, &call, route, target).await?;
        txn.commit().await?;
        Ok(route.into())
    }

    pub(crate) async fn move_route_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        route: call_routes::Model,
        target: RouteState,
    ) -> CallResult<call_routes::Model> {
        if call.current_state.is_terminal() && !target.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !route.state.can_transition_to(target) {
            return Err(CallError::InvalidState("route transition not allowed"));
        }
        let from = route.state;
        let at = self.now().max(route.updated_at);
        let mut active: call_routes::ActiveModel = route.into();
        active.state = Set(target);
        if target.is_terminal() {
            active.ended_at = Set(Some(at));
        }
        active.updated_at = Set(at);
        let route = active.update(txn).await?;
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(
                CallEventType::CallRouteStateChanged,
                data(vec![
                    ("route_id", text(route.id)),
                    ("from_route_state", text(from)),
                    ("to_route_state", text(target)),
                ])?,
            ),
            at,
        )
        .await?;
        Ok(route)
    }

    /// Records a canonical leg (state CREATED) for a non-terminal Call. Its route and
    /// participant, when given, must belong to the same Call.
    pub async fn record_leg(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewLeg,
    ) -> CallResult<CallLeg> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let leg = self.record_leg_in(&txn, cmd, &call, new).await?;
        txn.commit().await?;
        Ok(leg.into())
    }

    pub(crate) async fn record_leg_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewLeg,
    ) -> CallResult<call_legs::Model> {
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if let Some(route_id) = new.route_id {
            let route = route_of(txn, call, route_id).await?;
            if route.state.is_terminal() {
                return Err(CallError::InvalidState("the route has ended"));
            }
        }
        if let Some(participant_id) = new.participant_id {
            call_participants::Entity::find_by_id(participant_id.as_uuid())
                .filter(call_participants::Column::CallId.eq(call.id))
                .one(txn)
                .await?
                .ok_or(CallError::NotFound)?;
        }
        let at = self.now().max(call.updated_at);
        let leg = call_legs::ActiveModel {
            id: Set(CallLegId::generate().as_uuid()),
            call_id: Set(call.id),
            route_id: Set(new.route_id.map(|r| r.as_uuid())),
            participant_id: Set(new.participant_id.map(|p| p.as_uuid())),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            direction: Set(new.direction),
            leg_kind: Set(new.kind),
            state: Set(LegState::Created),
            failure_category: Set(None),
            execution_binding_ref: Set(None),
            created_at: Set(at),
            dialing_at: Set(None),
            ringing_at: Set(None),
            answered_at: Set(None),
            connected_at: Set(None),
            ended_at: Set(None),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        let mut details = vec![
            ("leg_id", text(leg.id)),
            ("direction", text(leg.direction)),
            ("leg_kind", text(leg.leg_kind)),
        ];
        if let Some(route_id) = leg.route_id {
            details.push(("route_id", text(route_id)));
        }
        if let Some(participant_id) = leg.participant_id {
            details.push(("participant_id", text(participant_id)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallLegRecorded, data(details)?),
            at,
        )
        .await?;
        Ok(leg)
    }

    /// Records a normalized leg state change (first-entry timestamps). FAILED requires a
    /// failure category, no other state accepts one. After the Call is terminal only
    /// terminal leg states may still be recorded, and a leg update never changes the
    /// Call's state.
    pub async fn update_leg_state(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        leg_id: CallLegId,
        target: LegState,
        failure_category: Option<FailureCategory>,
    ) -> CallResult<CallLeg> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let leg = leg_of(&txn, &call, leg_id).await?;
        let leg = self
            .move_leg_in(&txn, cmd, &call, leg, target, failure_category)
            .await?;
        txn.commit().await?;
        Ok(leg.into())
    }

    pub(crate) async fn move_leg_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: call_legs::Model,
        target: LegState,
        failure_category: Option<FailureCategory>,
    ) -> CallResult<call_legs::Model> {
        if (target == LegState::Failed) != failure_category.is_some() {
            return Err(CallError::Validation(
                "a failure category is required for FAILED and only for FAILED",
            ));
        }
        if call.current_state.is_terminal() && !target.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !leg.state.can_transition_to(target) {
            return Err(CallError::InvalidState("leg transition not allowed"));
        }
        let from = leg.state;
        let at = self.now().max(leg.updated_at);
        let mut active: call_legs::ActiveModel = leg.clone().into();
        active.state = Set(target);
        active.failure_category = Set(failure_category);
        active.updated_at = Set(at);
        match target {
            LegState::Dialing if leg.dialing_at.is_none() => active.dialing_at = Set(Some(at)),
            LegState::Ringing if leg.ringing_at.is_none() => active.ringing_at = Set(Some(at)),
            LegState::Answered if leg.answered_at.is_none() => active.answered_at = Set(Some(at)),
            LegState::Connected if leg.connected_at.is_none() => {
                active.connected_at = Set(Some(at))
            }
            t if t.is_terminal() => active.ended_at = Set(Some(at)),
            _ => {}
        }
        let leg = active.update(txn).await?;
        let mut details = vec![
            ("leg_id", text(leg.id)),
            ("from_leg_state", text(from)),
            ("to_leg_state", text(target)),
        ];
        if let Some(category) = failure_category {
            details.push(("failure_category", text(category)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallLegStateChanged, data(details)?),
            at,
        )
        .await?;
        Ok(leg)
    }

    /// Assigns the opaque execution binding reference of a leg (set once; repeating the
    /// same reference is idempotent). Calling never learns what the reference points to.
    pub async fn assign_leg_execution_binding(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        leg_id: CallLegId,
        binding_ref: Uuid,
    ) -> CallResult<CallLeg> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let leg = leg_of(&txn, &call, leg_id).await?;
        match leg.execution_binding_ref {
            Some(existing) if existing == binding_ref => {
                txn.commit().await?;
                return Ok(leg.into());
            }
            Some(_) => {
                return Err(CallError::Conflict(
                    "the leg already has an execution binding",
                ))
            }
            None => {}
        }
        let at = self.now().max(leg.updated_at);
        let mut active: call_legs::ActiveModel = leg.into();
        active.execution_binding_ref = Set(Some(binding_ref));
        active.updated_at = Set(at);
        let leg = active.update(&txn).await?;
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(
                CallEventType::CallLegBindingAssigned,
                data(vec![("leg_id", text(leg.id))])?,
            ),
            at,
        )
        .await?;
        txn.commit().await?;
        Ok(leg.into())
    }

    /// Records the next attempt of a non-terminal Call (attempt number = previous + 1,
    /// gap-free per Call). A previous attempt must belong to the Call and be finished.
    pub async fn record_attempt(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewAttempt,
    ) -> CallResult<CallAttempt> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let attempt = self.record_attempt_in(&txn, cmd, &call, new).await?;
        txn.commit().await?;
        Ok(attempt.into())
    }

    pub(crate) async fn record_attempt_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        new: NewAttempt,
    ) -> CallResult<call_attempts::Model> {
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if let Some(route_id) = new.route_id {
            route_of(txn, call, route_id).await?;
        }
        if let Some(leg_id) = new.leg_id {
            leg_of(txn, call, leg_id).await?;
        }
        if let Some(previous) = new.previous_attempt_id {
            let previous = attempt_of(txn, call, previous).await?;
            if previous.outcome == AttemptOutcome::Pending {
                return Err(CallError::InvalidState(
                    "the previous attempt is still pending",
                ));
            }
        }
        let number = scalar_i64(
            txn,
            "SELECT COALESCE(MAX(attempt_number), 0)::bigint FROM call_attempts WHERE call_id = $1",
            vec![call.id.into()],
        )
        .await?
            + 1;
        let number =
            i32::try_from(number).map_err(|_| CallError::InvalidState("too many attempts"))?;
        let at = self.now().max(call.updated_at);
        let attempt = call_attempts::ActiveModel {
            id: Set(CallAttemptId::generate().as_uuid()),
            call_id: Set(call.id),
            route_id: Set(new.route_id.map(|r| r.as_uuid())),
            leg_id: Set(new.leg_id.map(|l| l.as_uuid())),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            attempt_number: Set(number),
            previous_attempt_id: Set(new.previous_attempt_id.map(|a| a.as_uuid())),
            outcome: Set(AttemptOutcome::Pending),
            unknown_outcome: Set(false),
            failure_category: Set(None),
            started_at: Set(at),
            finished_at: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        let mut details = vec![
            ("attempt_id", text(attempt.id)),
            ("attempt_number", SafeValue::Integer(number.into())),
        ];
        if let Some(previous) = attempt.previous_attempt_id {
            details.push(("previous_attempt_id", text(previous)));
        }
        if let Some(route_id) = attempt.route_id {
            details.push(("route_id", text(route_id)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallAttemptRecorded, data(details)?),
            at,
        )
        .await?;
        Ok(attempt)
    }

    /// Records an attempt outcome. UNKNOWN is explicit (not a failure) and can later be
    /// resolved to SUCCEEDED or FAILED; `unknown_outcome` then keeps the history.
    pub async fn finish_attempt(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        attempt_id: CallAttemptId,
        outcome: AttemptOutcome,
        failure_category: Option<FailureCategory>,
    ) -> CallResult<CallAttempt> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let attempt = attempt_of(&txn, &call, attempt_id).await?;
        let attempt = self
            .finish_attempt_in(&txn, cmd, &call, attempt, outcome, failure_category)
            .await?;
        txn.commit().await?;
        Ok(attempt.into())
    }

    pub(crate) async fn finish_attempt_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        attempt: call_attempts::Model,
        outcome: AttemptOutcome,
        failure_category: Option<FailureCategory>,
    ) -> CallResult<call_attempts::Model> {
        if (outcome == AttemptOutcome::Failed) != failure_category.is_some() {
            return Err(CallError::Validation(
                "a failure category is required for FAILED and only for FAILED",
            ));
        }
        if !attempt.outcome.can_transition_to(outcome) {
            return Err(CallError::InvalidState(
                "attempt outcome transition not allowed",
            ));
        }
        let at = self.now().max(attempt.updated_at);
        let first_finish = attempt.finished_at.is_none();
        let unknown = attempt.unknown_outcome || outcome == AttemptOutcome::Unknown;
        let mut active: call_attempts::ActiveModel = attempt.into();
        active.outcome = Set(outcome);
        active.unknown_outcome = Set(unknown);
        active.failure_category = Set(failure_category);
        if first_finish {
            active.finished_at = Set(Some(at));
        }
        active.updated_at = Set(at);
        let attempt = active.update(txn).await?;
        let mut details = vec![
            ("attempt_id", text(attempt.id)),
            ("outcome", text(outcome)),
            ("unknown_outcome", SafeValue::Bool(unknown)),
        ];
        if let Some(category) = failure_category {
            details.push(("failure_category", text(category)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallAttemptFinished, data(details)?),
            at,
        )
        .await?;
        Ok(attempt)
    }
}

pub(crate) async fn route_of(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    id: CallRouteId,
) -> CallResult<call_routes::Model> {
    call_routes::Entity::find_by_id(id.as_uuid())
        .filter(call_routes::Column::CallId.eq(call.id))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)
}

pub(crate) async fn leg_of(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    id: CallLegId,
) -> CallResult<call_legs::Model> {
    call_legs::Entity::find_by_id(id.as_uuid())
        .filter(call_legs::Column::CallId.eq(call.id))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)
}

pub(crate) async fn attempt_of(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    id: CallAttemptId,
) -> CallResult<call_attempts::Model> {
    call_attempts::Entity::find_by_id(id.as_uuid())
        .filter(call_attempts::Column::CallId.eq(call.id))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)
}
