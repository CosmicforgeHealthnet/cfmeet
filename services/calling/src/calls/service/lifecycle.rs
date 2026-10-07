//! Call creation, state transitions, authorization decisions and the paid public-network
//! authorization guard.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use super::{data, execute, scalar_i64, text, CallService, EventDraft, TERMINAL_STATES_SQL};
use crate::{
    calls::{
        aggregate::Call,
        attributes::SafeValue,
        authorizations::{
            CallAuthorization, NewAuthorization, PublicNetworkAuthorization, PAID_ACTION_STATES,
        },
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        events::{CallEvent, CallEventType},
        ids::{CallAuthorizationId, CallId, CallParticipantId},
        kinds::{AuthorizationDecision, CallKind, LegState, ParticipantRole, SessionState},
        participants::{CallParticipant, DisplayName},
        state::CallState,
        transitions::{
            plan_transition, CallAction, ConnectedEvidence, EndedEvidence, TransitionPlan,
        },
    },
    domain::{EnvironmentKind, EnvironmentStatus},
    models::{
        call_authorizations, call_legs, call_participants, call_sessions, calls, environments,
    },
    platform::{
        context::PlatformAuthContext, limits::concurrent_communication_limits,
        principals::ResolvedPrincipal,
    },
};

/// Input of [`CallService::create_requested_call`] (everything else comes from the
/// authenticated context and the resolved initiator).
#[derive(Debug, Clone)]
pub struct NewCall {
    pub kind: CallKind,
    /// Opaque reference to a communication context owned by another service.
    pub communication_context_ref: Option<Uuid>,
    /// Opaque reference to a relay record owned by another service.
    pub privacy_relay_ref: Option<Uuid>,
    pub initiator_display_name: Option<DisplayName>,
}

impl NewCall {
    /// A call of `kind` without context references or initiator display name.
    pub fn of_kind(kind: CallKind) -> Self {
        Self {
            kind,
            communication_context_ref: None,
            privacy_relay_ref: None,
            initiator_display_name: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreatedCall {
    pub call: Call,
    pub initiator: CallParticipant,
    pub event: CallEvent,
}

#[derive(Debug, Clone)]
pub struct TransitionOutcome {
    pub call: Call,
    pub event: CallEvent,
}

/// Serializes Call creation per application (`pg_advisory_xact_lock`), so concurrent
/// creations cannot both pass the concurrent-call check.
const CREATION_LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))";

impl CallService {
    /// Creates a Call in REQUESTED (version 1) with its initiating participant, the
    /// immutable CALL_REQUESTED event and its outbox row — atomically — after enforcing
    /// the concurrent-call limits of the credential and the application.
    pub async fn create_requested_call(
        &self,
        ctx: &PlatformAuthContext,
        initiator: &ResolvedPrincipal,
        request: NewCall,
    ) -> CallResult<CreatedCall> {
        let cmd = CallCommandContext::customer(ctx);
        let scope = cmd.scope();
        // The initiator must have been resolved in the caller's own scope.
        if CallScope::of_principal(initiator) != scope {
            return Err(CallError::NotFound);
        }
        let txn = self.db.begin().await?;
        self.enforce_concurrent_call_limit(&txn, ctx).await?;

        let at = self.now();
        let call_id = CallId::generate();
        let call = calls::ActiveModel {
            id: Set(call_id.as_uuid()),
            tenant_id: Set(scope.tenant_id().as_uuid()),
            application_id: Set(scope.application_id().as_uuid()),
            environment_id: Set(scope.environment_id().as_uuid()),
            created_by_service_account_id: Set(ctx.service_account_id().as_uuid()),
            created_by_credential_id: Set(ctx.credential_id().as_uuid()),
            initiator_principal_id: Set(initiator.id().as_uuid()),
            call_kind: Set(request.kind),
            current_state: Set(CallState::Requested),
            version: Set(1),
            communication_context_ref: Set(request.communication_context_ref),
            privacy_relay_ref: Set(request.privacy_relay_ref),
            terminal_reason: Set(None),
            failure_category: Set(None),
            requested_at: Set(at),
            authorized_at: Set(None),
            resolving_at: Set(None),
            routing_at: Set(None),
            dialing_at: Set(None),
            ringing_at: Set(None),
            answered_at: Set(None),
            connected_at: Set(None),
            ending_at: Set(None),
            ended_at: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
        }
        .insert(&txn)
        .await?;
        let participant = call_participants::ActiveModel {
            id: Set(CallParticipantId::generate().as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            principal_id: Set(initiator.id().as_uuid()),
            principal_kind: Set(initiator.kind()),
            role: Set(ParticipantRole::Initiator),
            display_name: Set(request
                .initiator_display_name
                .as_ref()
                .map(|d| d.as_str().to_string())),
            created_at: Set(at),
            ended_at: Set(None),
            updated_at: Set(at),
        }
        .insert(&txn)
        .await?;
        let event = self
            .append_event(
                &txn,
                &cmd,
                &call,
                EventDraft {
                    event_type: CallEventType::CallRequested,
                    from_state: None,
                    to_state: Some(CallState::Requested),
                    data: data(vec![
                        ("call_kind", text(call.call_kind)),
                        ("initiator_participant_id", text(participant.id)),
                        ("initiator_principal_kind", text(participant.principal_kind)),
                        (
                            "has_communication_context",
                            SafeValue::Bool(call.communication_context_ref.is_some()),
                        ),
                        (
                            "has_privacy_relay_ref",
                            SafeValue::Bool(call.privacy_relay_ref.is_some()),
                        ),
                    ])?,
                },
                at,
            )
            .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            call_state = %call.current_state,
            call_version = call.version,
            tenant_id = %call.tenant_id,
            application_id = %call.application_id,
            environment_id = %call.environment_id,
            participant_id = %participant.id,
            request_id = %cmd.request_id(),
            correlation_id = %cmd.correlation_id(),
            "call requested"
        );
        Ok(CreatedCall {
            call: call.into(),
            initiator: participant.into(),
            event,
        })
    }

    /// Enforces the effective concurrent-call limits inside the creating transaction:
    /// per credential (effective limit) and per application (aggregate, when configured).
    /// Creations of one application are serialized by a transaction-scoped advisory lock;
    /// only non-terminal Calls count.
    async fn enforce_concurrent_call_limit(
        &self,
        txn: &DatabaseTransaction,
        ctx: &PlatformAuthContext,
    ) -> CallResult<()> {
        execute(
            txn,
            CREATION_LOCK_SQL,
            vec![format!("cfmeet.calling.active-calls:{}", ctx.application_id()).into()],
        )
        .await?;
        let (per_credential, application) = concurrent_communication_limits(
            txn,
            ctx.application_id(),
            ctx.credential_id(),
            self.limit_defaults,
        )
        .await?;
        let by_credential = scalar_i64(
            txn,
            &format!(
                "SELECT count(*) FROM calls WHERE created_by_credential_id = $1 \
                 AND current_state NOT IN {TERMINAL_STATES_SQL}"
            ),
            vec![ctx.credential_id().as_uuid().into()],
        )
        .await?;
        if by_credential >= per_credential {
            tracing::info!(
                credential_id = %ctx.credential_id(),
                application_id = %ctx.application_id(),
                limit = per_credential,
                "concurrent call limit reached (credential)"
            );
            return Err(CallError::ConcurrentCallLimitExceeded {
                level: "credential",
                limit: per_credential,
            });
        }
        if let Some(limit) = application {
            let by_application = scalar_i64(
                txn,
                &format!(
                    "SELECT count(*) FROM calls WHERE application_id = $1 \
                     AND current_state NOT IN {TERMINAL_STATES_SQL}"
                ),
                vec![ctx.application_id().as_uuid().into()],
            )
            .await?;
            if by_application >= limit {
                tracing::info!(
                    application_id = %ctx.application_id(),
                    limit,
                    "concurrent call limit reached (application)"
                );
                return Err(CallError::ConcurrentCallLimitExceeded {
                    level: "application",
                    limit,
                });
            }
        }
        Ok(())
    }

    /// Applies `action` to the Call if the command's `expected_version` is current and the
    /// action, the canonical graph and the action's evidence allow it. One transaction:
    /// Call update (version + 1, first-entry timestamp, terminal fields), terminal
    /// cleanup, one CALL_STATE_CHANGED event, one outbox row.
    pub async fn transition(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        action: CallAction,
    ) -> CallResult<TransitionOutcome> {
        let txn = self.db.begin().await?;
        let row = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let current: Call = row.clone().into();
        let plan = plan_transition(&current, expected_version, &action, self.now())?;
        let mut details = self.verify_evidence(&txn, &row, &action, &plan).await?;

        let mut update = <calls::ActiveModel as ActiveModelTrait>::default();
        update.current_state = Set(plan.to);
        update.version = Set(plan.new_version);
        update.updated_at = Set(plan.at);
        set_first_entry(&mut update, &row, plan.to, plan.at);
        if plan.to.is_terminal() {
            update.ended_at = Set(Some(plan.at));
            update.terminal_reason = Set(plan.terminal_reason);
            update.failure_category = Set(plan.failure_category);
        }
        let updated = calls::Entity::update_many()
            .set(update)
            .filter(calls::Column::Id.eq(row.id))
            .filter(calls::Column::Version.eq(expected_version))
            .exec(&txn)
            .await?;
        if updated.rows_affected != 1 {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: row.version,
            });
        }
        let call = calls::Entity::find_by_id(row.id)
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;

        if plan.to.is_terminal() {
            let (intervals, sessions, participants) =
                close_participation(&txn, row.id, plan.at).await?;
            details.push(("sessions_closed", SafeValue::Integer(sessions as i64)));
            details.push(("intervals_closed", SafeValue::Integer(intervals as i64)));
            details.push((
                "participants_ended",
                SafeValue::Integer(participants as i64),
            ));
        }
        details.push(("action", text(action.name())));
        if let Some(reason) = plan.terminal_reason {
            details.push(("terminal_reason", text(reason)));
        }
        if let Some(category) = plan.failure_category {
            details.push(("failure_category", text(category)));
        }
        let event = self
            .append_event(
                &txn,
                cmd,
                &call,
                EventDraft {
                    event_type: CallEventType::CallStateChanged,
                    from_state: Some(plan.from),
                    to_state: Some(plan.to),
                    data: data(details)?,
                },
                plan.at,
            )
            .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            call_state = %call.current_state,
            call_version = call.version,
            from_state = %plan.from,
            action = action.name(),
            tenant_id = %call.tenant_id,
            application_id = %call.application_id,
            environment_id = %call.environment_id,
            request_id = %cmd.request_id(),
            correlation_id = %cmd.correlation_id(),
            "call state changed"
        );
        Ok(TransitionOutcome {
            call: call.into(),
            event,
        })
    }

    /// Verifies the facts behind an action's evidence under the call lock, returning safe
    /// event details.
    async fn verify_evidence(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        action: &CallAction,
        plan: &TransitionPlan,
    ) -> CallResult<Vec<(&'static str, SafeValue)>> {
        match action {
            CallAction::Authorize { authorization_id } => {
                let auth = self.effective_authorization(txn, call).await?;
                match auth {
                    Some(a) if a.id == authorization_id.as_uuid() => match a.decision {
                        AuthorizationDecision::Approved => {
                            Ok(vec![("authorization_id", text(authorization_id))])
                        }
                        AuthorizationDecision::Denied => Err(CallError::AuthorizationDenied),
                    },
                    Some(a) if a.decision == AuthorizationDecision::Denied => {
                        Err(CallError::AuthorizationDenied)
                    }
                    _ => Err(CallError::AuthorizationRequired),
                }
            }
            CallAction::DenyByPolicy { authorization_id } => {
                match self.effective_authorization(txn, call).await? {
                    Some(a)
                        if a.id == authorization_id.as_uuid()
                            && a.decision == AuthorizationDecision::Denied =>
                    {
                        Ok(vec![
                            ("authorization_id", text(authorization_id)),
                            ("authorization_reason", text(a.reason_code)),
                        ])
                    }
                    _ => Err(CallError::InsufficientEvidence(
                        "an effective denied authorization is required",
                    )),
                }
            }
            CallAction::ConfirmConnected { evidence }
            | CallAction::Resume { evidence }
            | CallAction::CompleteTransfer { evidence } => {
                self.verify_connected(txn, call, evidence).await
            }
            CallAction::ConfirmEnded { evidence } => {
                self.verify_ended(txn, call, *evidence, plan.at).await?;
                Ok(vec![("evidence", text(evidence.name()))])
            }
            _ => Ok(Vec::new()),
        }
    }

    async fn verify_connected(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        evidence: &ConnectedEvidence,
    ) -> CallResult<Vec<(&'static str, SafeValue)>> {
        let session_ids: Vec<Uuid> = evidence.session_ids().iter().map(|s| s.as_uuid()).collect();
        let leg_ids: Vec<Uuid> = evidence.leg_ids().iter().map(|l| l.as_uuid()).collect();
        if !session_ids.is_empty() {
            let sessions = call_sessions::Entity::find()
                .filter(call_sessions::Column::CallId.eq(call.id))
                .filter(call_sessions::Column::Id.is_in(session_ids.clone()))
                .all(txn)
                .await?;
            if sessions.len() != session_ids.len()
                || sessions.iter().any(|s| s.state != SessionState::Connected)
            {
                return Err(CallError::InsufficientEvidence(
                    "every referenced session must be connected in this call",
                ));
            }
            let participants: BTreeSet<Uuid> = sessions.iter().map(|s| s.participant_id).collect();
            if matches!(evidence, ConnectedEvidence::RtcMediaPathEstablished { .. })
                && participants.len() < 2
            {
                return Err(CallError::InsufficientEvidence(
                    "a media path needs connected sessions of two participants",
                ));
            }
        }
        if !leg_ids.is_empty() {
            let legs = call_legs::Entity::find()
                .filter(call_legs::Column::CallId.eq(call.id))
                .filter(call_legs::Column::Id.is_in(leg_ids.clone()))
                .all(txn)
                .await?;
            if legs.len() != leg_ids.len() || legs.iter().any(|l| l.state != LegState::Connected) {
                return Err(CallError::InsufficientEvidence(
                    "every referenced leg must be connected in this call",
                ));
            }
        }
        let ids = |v: &[Uuid]| SafeValue::TextList(v.iter().map(Uuid::to_string).collect());
        let mut details = vec![("evidence", text(evidence.name()))];
        if !session_ids.is_empty() {
            details.push(("evidence_session_ids", ids(&session_ids)));
        }
        if !leg_ids.is_empty() {
            details.push(("evidence_leg_ids", ids(&leg_ids)));
        }
        Ok(details)
    }

    async fn verify_ended(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        evidence: EndedEvidence,
        at: DateTime<Utc>,
    ) -> CallResult<()> {
        match evidence {
            EndedEvidence::CleanupConfirmed => Ok(()),
            EndedEvidence::AllParticipationEnded => {
                let open = scalar_i64(
                    txn,
                    "SELECT count(*) FROM call_session_intervals WHERE call_id = $1 AND disconnected_at IS NULL",
                    vec![call.id.into()],
                )
                .await?;
                let active_legs = scalar_i64(
                    txn,
                    "SELECT count(*) FROM call_legs WHERE call_id = $1 \
                     AND state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')",
                    vec![call.id.into()],
                )
                .await?;
                if open > 0 || active_legs > 0 {
                    return Err(CallError::InsufficientEvidence(
                        "sessions or legs of the call are still active",
                    ));
                }
                Ok(())
            }
            EndedEvidence::ReconciliationTimeout => {
                let ending_since = call.ending_at.ok_or(CallError::InsufficientEvidence(
                    "the call has not been ending",
                ))?;
                if at - ending_since < self.config.ending_reconciliation_timeout {
                    return Err(CallError::InsufficientEvidence(
                        "the reconciliation timeout has not elapsed",
                    ));
                }
                Ok(())
            }
        }
    }

    pub(crate) async fn effective_authorization(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<Option<call_authorizations::Model>> {
        Ok(call_authorizations::Entity::find()
            .filter(call_authorizations::Column::CallId.eq(call.id))
            .filter(call_authorizations::Column::SupersededAt.is_null())
            .one(txn)
            .await?)
    }

    /// Records an authorization decision for a non-terminal Call; it supersedes the
    /// previous effective decision. Evidence: the command's actor (service account,
    /// credential or internal service), the Call's initiating principal and the request /
    /// correlation ids. Does not change the Call's state.
    pub async fn record_authorization(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        new: NewAuthorization,
    ) -> CallResult<CallAuthorization> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let at = self.now().max(call.updated_at);
        let id = CallAuthorizationId::generate();
        let previous = self.effective_authorization(&txn, &call).await?;
        let decided_at = previous.as_ref().map_or(at, |p| at.max(p.decided_at));
        if let Some(previous) = &previous {
            let mut superseded: call_authorizations::ActiveModel = previous.clone().into();
            superseded.superseded_at = Set(Some(decided_at));
            superseded.superseded_by_authorization_id = Set(Some(id.as_uuid()));
            superseded.update(&txn).await?;
        }
        let decision = new.reason.decision();
        let model = call_authorizations::ActiveModel {
            id: Set(id.as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            decision: Set(decision),
            reason_code: Set(new.reason),
            policy_version: Set(new.policy_version.as_str().to_string()),
            policy_snapshot: Set(new.policy_snapshot.to_json()),
            limits_snapshot: Set(new.limits_snapshot.to_json()),
            actor_kind: Set(cmd.actor().kind()),
            evidence_service_account_id: Set(cmd.actor().service_account_id().as_uuid()),
            evidence_credential_id: Set(cmd.actor().credential_id().map(|c| c.as_uuid())),
            evidence_service: Set(cmd.actor().service().map(|s| s.as_str().to_string())),
            evidence_principal_id: Set(call.initiator_principal_id),
            request_id: Set(cmd.request_id().as_str().to_string()),
            correlation_id: Set(cmd.correlation_id().as_str().to_string()),
            decided_at: Set(decided_at),
            superseded_at: Set(None),
            superseded_by_authorization_id: Set(None),
            created_at: Set(at),
        }
        .insert(&txn)
        .await?;
        let mut details = vec![
            ("authorization_id", text(id)),
            ("decision", text(decision)),
            ("reason_code", text(new.reason)),
            ("policy_version", text(new.policy_version.as_str())),
        ];
        if let Some(previous) = &previous {
            details.push(("supersedes_authorization_id", text(previous.id)));
        }
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(CallEventType::CallAuthorizationRecorded, data(details)?),
            decided_at,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            authorization_id = %id,
            decision = %decision,
            request_id = %cmd.request_id(),
            correlation_id = %cmd.correlation_id(),
            "call authorization recorded"
        );
        Ok(model.into())
    }

    /// The guard every paid public-network action must pass (Part C §18). Succeeds only if
    /// the Call exists in `scope`, is in an executable state (authorized, not ending, not
    /// terminal), its effective authorization is APPROVED and its environment is allowed
    /// live telecom. Executes nothing.
    pub async fn require_public_network_authorization(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<PublicNetworkAuthorization> {
        let txn = self.db.begin().await?;
        let call = calls::Entity::find_by_id(call_id.as_uuid())
            .filter(calls::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(calls::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(calls::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .lock_shared()
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        let authorization = match self.effective_authorization(&txn, &call).await? {
            None => return Err(CallError::AuthorizationRequired),
            Some(a) if a.decision == AuthorizationDecision::Denied => {
                return Err(CallError::AuthorizationDenied)
            }
            Some(a) => a,
        };
        if !PAID_ACTION_STATES.contains(&call.current_state) {
            return Err(match call.current_state {
                CallState::Requested => CallError::AuthorizationRequired,
                state => CallError::InvalidTransition {
                    state,
                    action: "PUBLIC_NETWORK_ACTION",
                },
            });
        }
        let environment = environments::Entity::find_by_id(call.environment_id)
            .filter(environments::Column::ApplicationId.eq(call.application_id))
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if !(environment.live_telecom_allowed
            && environment.kind == EnvironmentKind::Production
            && environment.status == EnvironmentStatus::Active)
        {
            return Err(CallError::LiveTelecomNotAllowed);
        }
        txn.commit().await?;
        Ok(PublicNetworkAuthorization::new(
            CallId::from_uuid(call.id),
            CallAuthorizationId::from_uuid(authorization.id),
            call.version,
            self.now(),
        ))
    }
}

/// Sets the first-entry timestamp of `state` if the Call has never been in it.
fn set_first_entry(
    update: &mut calls::ActiveModel,
    row: &calls::Model,
    state: CallState,
    at: DateTime<Utc>,
) {
    let (current, field) = match state {
        CallState::Authorized => (row.authorized_at, &mut update.authorized_at),
        CallState::Resolving => (row.resolving_at, &mut update.resolving_at),
        CallState::Routing => (row.routing_at, &mut update.routing_at),
        CallState::Dialing => (row.dialing_at, &mut update.dialing_at),
        CallState::Ringing => (row.ringing_at, &mut update.ringing_at),
        CallState::Answered => (row.answered_at, &mut update.answered_at),
        CallState::Connected => (row.connected_at, &mut update.connected_at),
        CallState::Ending => (row.ending_at, &mut update.ending_at),
        _ => return,
    };
    if current.is_none() {
        *field = Set(Some(at));
    }
}

/// Terminal Call: closes every open connection interval, ends every session and every
/// participant. Returns (intervals, sessions, participants) affected.
async fn close_participation(
    txn: &DatabaseTransaction,
    call_id: Uuid,
    at: DateTime<Utc>,
) -> CallResult<(u64, u64, u64)> {
    let intervals = execute(
        txn,
        "UPDATE call_session_intervals \
            SET disconnected_at = GREATEST($2, connected_at), disconnect_reason = 'CALL_TERMINATED' \
          WHERE call_id = $1 AND disconnected_at IS NULL",
        vec![call_id.into(), at.into()],
    )
    .await?;
    let sessions = execute(
        txn,
        "UPDATE call_sessions \
            SET last_disconnected_at = CASE WHEN state = 'CONNECTED' \
                    THEN GREATEST($2, updated_at) ELSE last_disconnected_at END, \
                state = 'ENDED', \
                ended_at = GREATEST($2, updated_at), \
                updated_at = GREATEST($2, updated_at) \
          WHERE call_id = $1 AND state <> 'ENDED'",
        vec![call_id.into(), at.into()],
    )
    .await?;
    let participants = execute(
        txn,
        "UPDATE call_participants \
            SET ended_at = GREATEST($2, updated_at), updated_at = GREATEST($2, updated_at) \
          WHERE call_id = $1 AND ended_at IS NULL",
        vec![call_id.into(), at.into()],
    )
    .await?;
    Ok((intervals, sessions, participants))
}
