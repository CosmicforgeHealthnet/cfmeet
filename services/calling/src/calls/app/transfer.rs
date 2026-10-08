//! Application-to-application transfer (Part C §64–§68). The Call id never changes: the
//! transfer adds the target as a participant of the SAME Call, rings the target's
//! endpoints (single-winner per participant), and on the target's verified join retires
//! the transferring participant and returns the Call to CONNECTED. A failed transfer
//! retires the target path and returns the Call to its pre-transfer state over the
//! original, still valid media path (or ends it if that path is gone).

use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{ringing::replay, summary, system_cmd, AppCallService, CommandOutcome, PendingSignals};
use crate::{
    calls::{
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallAttemptId, CallId, CallLegId, CallParticipantId, CallRouteId},
        invitations::CallTransfer,
        kinds::{
            AttemptOutcome, FailureCategory, InvitationPurpose, InvitationStatus,
            InvitationTerminalReason, LegDirection, LegKind, LegState, MediaAccess, RouteClass,
            RouteState, SessionState, TransferFailureReason, TransferState,
        },
        legs::NewLeg,
        read_model::TransferView,
        routes::{NewRoute, PolicyVersion},
        service::{attempt_of, leg_of, participant_of, route_of, CommandKey, NewTransfer},
        state::CallState,
        transitions::{CallAction, ConnectedEvidence},
    },
    domain::PrincipalId,
    models::{call_attempts, call_participants, call_sessions, call_transfers, calls},
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError},
    },
    privacy_relay::AuthorizationPurpose,
    signaling::signals::SignalType,
};

/// `POST /v1/calls/{call_id}/transfer` input (Idempotency-Key required).
#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub transferring_participant: CallParticipantId,
    pub target: PrincipalId,
    pub expected_version: i64,
    pub key: CommandKey,
}

fn transfer_body(call: &calls::Model, transfer: &call_transfers::Model) -> Value {
    json!({
        "call": summary(call),
        "transfer": TransferView::from(&CallTransfer::from(transfer.clone())),
    })
}

impl AppCallService {
    /// Starts an application transfer of an active Call.
    pub async fn transfer(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        request: TransferRequest,
    ) -> CallResult<CommandOutcome> {
        let scope = CallScope::of_platform(ctx);
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            return replay(prior);
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application transfers",
            ));
        }
        let target = self
            .principals
            .resolve(ctx, &PrincipalReference::Id(request.target))
            .await
            .map_err(|e| match e {
                PrincipalResolutionError::NotFound => CallError::NotFound,
                PrincipalResolutionError::Inactive => CallError::TargetUnavailable,
                PrincipalResolutionError::InvalidReference => {
                    CallError::Validation("targetPrincipalId is invalid.")
                }
                PrincipalResolutionError::Unavailable { summary } => CallError::Database {
                    summary,
                    connectivity: true,
                },
            })?;
        // A protected call never introduces a participant on its original authorization:
        // the transferring participant -> target introduction needs CURRENT Privacy Relay
        // authority under the call's relay session (fail closed).
        let privacy = match self.protected_session(scope, call_id).await? {
            None => None,
            Some(session) => {
                let transferring = call_participants::Entity::find_by_id(
                    request.transferring_participant.as_uuid(),
                )
                .filter(call_participants::Column::CallId.eq(call_id.as_uuid()))
                .one(self.db())
                .await?
                .ok_or(CallError::NotFound)?;
                let source = self
                    .principals
                    .resolve(
                        ctx,
                        &PrincipalReference::Id(crate::domain::PrincipalId::from_uuid(
                            transferring.principal_id,
                        )),
                    )
                    .await
                    .map_err(|_| CallError::AuthorizationDenied)?;
                Some((
                    self.privacy_authority(
                        ctx,
                        &source,
                        session,
                        vec![target.id().as_uuid()],
                        AuthorizationPurpose::CallTransfer,
                    )
                    .await?,
                    transferring.principal_id,
                ))
            }
        };
        let cmd = CallCommandContext::customer(ctx);
        let txn = self.db().begin().await?;
        if let Some(prior) = self
            .calls
            .begin_command_in(&txn, scope, &request.key)
            .await?
        {
            return replay(prior);
        }
        if let Some((decision, source)) = &privacy {
            self.guard_privacy_decision(&txn, scope, decision, &[target.id().as_uuid(), *source])
                .await?;
        }
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        if let Some((decision, _)) = &privacy {
            if call.privacy_relay_ref != Some(decision.relay_session_id) {
                return Err(CallError::AuthorizationDenied);
            }
        }
        if call.version != request.expected_version {
            return Err(CallError::StaleVersion {
                expected: request.expected_version,
                current: call.version,
            });
        }
        if !matches!(call.current_state, CallState::Connected | CallState::Held) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: "TRANSFER",
            });
        }
        if self.calls.active_transfer(&txn, &call).await?.is_some() {
            return Err(CallError::InvalidState("a transfer is already in flight"));
        }
        let transferring = participant_of(&txn, &call, request.transferring_participant).await?;
        if transferring.ended_at.is_some() {
            return Err(CallError::InvalidState(
                "the transferring participant has left the call",
            ));
        }
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .all(&txn)
            .await?;
        if participants
            .iter()
            .any(|p| p.principal_id == target.id().as_uuid())
        {
            return Err(CallError::Conflict(
                "the target already participates in this call",
            ));
        }
        let active = participants.iter().filter(|p| p.ended_at.is_none()).count();
        if active + 1 > self.config.max_group_participants {
            return Err(CallError::ParticipantLimitExceeded {
                limit: self.config.max_group_participants as i64,
            });
        }
        let endpoints = self
            .endpoints
            .eligible_in(
                &txn,
                scope,
                target.id().as_uuid(),
                self.config.max_endpoints_per_participant,
            )
            .await?;
        if endpoints.is_empty() {
            return Err(CallError::TargetUnavailable);
        }
        let binding = self
            .calls
            .media_binding_in(&txn, call.id)
            .await?
            .filter(|b| b.status == crate::calls::kinds::MediaSessionStatus::Active)
            .ok_or(CallError::InvalidState(
                "the call has no active media resource",
            ))?;

        let command = self
            .calls
            .record_command_in(&txn, &cmd, &request.key, Some(call.id), None, None, None)
            .await?;
        let transfer = self
            .calls
            .record_transfer_in(
                &txn,
                &cmd,
                &call,
                NewTransfer {
                    command_id: command.id,
                    transferring_participant_id: transferring.id,
                    target_principal_id: target.id().as_uuid(),
                    pre_transfer_state: call.current_state,
                    expected_call_version: request.expected_version,
                },
            )
            .await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginTransfer)
            .await?
            .call;
        let (target_participant, _) = self
            .calls
            .add_participant_in(&txn, &cmd, &call, &target, None)
            .await?;
        if let Some((decision, _)) = &privacy {
            self.record_privacy_decision(
                &txn,
                &call,
                decision,
                AuthorizationPurpose::CallTransfer,
                Some(target.id().as_uuid()),
            )
            .await?;
        }
        self.calls
            .ensure_media_permissions_in(&txn, &call, &target_participant)
            .await?;
        // Transfer route / leg / attempt evidence on the Call's existing media resource.
        let mut decision = SafeAttributes::new();
        decision.insert("reason", SafeValue::Text("APPLICATION_TRANSFER".into()))?;
        decision.insert("transfer_id", SafeValue::Text(transfer.id.to_string()))?;
        decision.insert("media_session_id", SafeValue::Text(binding.id.to_string()))?;
        let route = self
            .calls
            .record_route_in(
                &txn,
                &cmd,
                &call,
                NewRoute {
                    route_class: RouteClass::Application,
                    policy_version: PolicyVersion::parse(super::create::APP_ROUTE_POLICY)?,
                    decision_snapshot: decision,
                },
            )
            .await?;
        let route = self
            .calls
            .move_route_in(&txn, &cmd, &call, route, RouteState::Active)
            .await?;
        let leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(CallRouteId::from_uuid(route.id)),
                    participant_id: Some(CallParticipantId::from_uuid(target_participant.id)),
                    direction: LegDirection::Outbound,
                    kind: LegKind::Application,
                },
            )
            .await?;
        let leg = self
            .calls
            .move_leg_in(&txn, &cmd, &call, leg, LegState::Dialing, None)
            .await?;
        let leg = self
            .calls
            .move_leg_in(&txn, &cmd, &call, leg, LegState::Ringing, None)
            .await?;
        let previous = call_attempts::Entity::find()
            .filter(call_attempts::Column::CallId.eq(call.id))
            .order_by_desc(call_attempts::Column::AttemptNumber)
            .one(&txn)
            .await?
            .filter(|a| a.outcome != AttemptOutcome::Pending)
            .map(|a| CallAttemptId::from_uuid(a.id));
        let attempt = self
            .calls
            .record_attempt_in(
                &txn,
                &cmd,
                &call,
                NewAttempt {
                    route_id: Some(CallRouteId::from_uuid(route.id)),
                    leg_id: Some(CallLegId::from_uuid(leg.id)),
                    previous_attempt_id: previous,
                },
            )
            .await?;
        let transfer = self
            .calls
            .transfer_ringing_in(
                &txn,
                transfer,
                target_participant.id,
                route.id,
                leg.id,
                attempt.id,
            )
            .await?;
        let invitations = self
            .calls
            .issue_invitations_in(
                &txn,
                &cmd,
                &call,
                &target_participant,
                &endpoints,
                InvitationPurpose::Transfer,
                Some(transfer.id),
                self.config.invitation_ttl,
            )
            .await?;
        let call = self.locked(&txn, &call).await?;
        let body = transfer_body(&call, &transfer);
        self.calls
            .complete_command_in(&txn, scope, &request.key, 200, &body, Some(call.version))
            .await?;
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::TransferStarted,
            &call,
            &correlation,
            Map::new(),
        )
        .await?;
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
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(call_id = %call.id, transfer_id = %transfer.id, "application transfer ringing target");
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Fails an in-flight transfer and recovers the Call (§67): the target path is
    /// retired (offers cancelled, participant retired, leg / route / attempt closed) and
    /// the Call returns to HELD (if it was held) or CONNECTED over the original media path
    /// — or, if that path is gone, starts ending. Never stuck in TRANSFERRING.
    pub(crate) async fn fail_transfer_and_recover_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        transfer: call_transfers::Model,
        reason: TransferFailureReason,
        category: FailureCategory,
    ) -> CallResult<calls::Model> {
        let transfer = self
            .calls
            .fail_transfer_in(txn, cmd, call, transfer, reason, category)
            .await?;
        for invitation in self.calls.live_invitations(txn, call, None).await? {
            if invitation.transfer_id == Some(transfer.id) {
                self.calls
                    .finish_invitation_in(
                        txn,
                        cmd,
                        call,
                        invitation,
                        InvitationStatus::Cancelled,
                        InvitationTerminalReason::TransferEnded,
                        None,
                    )
                    .await?;
            }
        }
        if let Some(target_id) = transfer.target_participant_id {
            let target = participant_of(txn, call, CallParticipantId::from_uuid(target_id)).await?;
            self.calls
                .retire_participant_in(txn, cmd, call, target, "TRANSFER_FAILED")
                .await?;
        }
        let leg_target = match reason {
            TransferFailureReason::TargetDeclined => LegState::Declined,
            TransferFailureReason::TargetNoAnswer => LegState::NoAnswer,
            _ => LegState::Failed,
        };
        if let Some(leg_id) = transfer.new_leg_id {
            let leg = leg_of(txn, call, CallLegId::from_uuid(leg_id)).await?;
            if !leg.state.is_terminal() {
                let target = if leg.state.can_transition_to(leg_target) {
                    leg_target
                } else {
                    LegState::Failed
                };
                let leg_category = (target == LegState::Failed).then_some(category);
                self.calls
                    .move_leg_in(txn, cmd, call, leg, target, leg_category)
                    .await?;
            }
        }
        if let Some(route_id) = transfer.new_route_id {
            let route = route_of(txn, call, CallRouteId::from_uuid(route_id)).await?;
            if !route.state.is_terminal() {
                let target = if matches!(
                    reason,
                    TransferFailureReason::TargetDeclined | TransferFailureReason::TargetNoAnswer
                ) {
                    RouteState::Exhausted
                } else {
                    RouteState::Failed
                };
                self.calls
                    .move_route_in(txn, cmd, call, route, target)
                    .await?;
            }
        }
        if let Some(attempt_id) = transfer.attempt_id {
            let attempt = attempt_of(txn, call, CallAttemptId::from_uuid(attempt_id)).await?;
            if matches!(
                attempt.outcome,
                AttemptOutcome::Pending | AttemptOutcome::Unknown
            ) {
                self.calls
                    .finish_attempt_in(
                        txn,
                        cmd,
                        call,
                        attempt,
                        AttemptOutcome::Failed,
                        Some(category),
                    )
                    .await?;
            }
        }
        let call = self.locked(txn, call).await?;
        if transfer.pre_transfer_state == CallState::Held {
            return Ok(self
                .calls
                .transition_in(txn, cmd, &call, call.version, CallAction::Hold)
                .await?
                .call);
        }
        match self.media_path_evidence(txn, &call, None).await? {
            Some(evidence) => Ok(self
                .calls
                .transition_in(
                    txn,
                    cmd,
                    &call,
                    call.version,
                    CallAction::RevertTransfer { evidence },
                )
                .await?
                .call),
            None => Ok(self
                .calls
                .transition_in(txn, cmd, &call, call.version, CallAction::RequestEnd)
                .await?
                .call),
        }
    }

    /// Positive media-path evidence from Calling's own records: connected, authorized,
    /// admitted sessions of at least two different active participants (optionally
    /// requiring `including` among them).
    pub(crate) async fn media_path_evidence(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        including: Option<Uuid>,
    ) -> CallResult<Option<ConnectedEvidence>> {
        let active_participants: Vec<Uuid> = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .filter(call_participants::Column::EndedAt.is_null())
            .all(txn)
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .filter(call_sessions::Column::State.eq(SessionState::Connected))
            .filter(call_sessions::Column::MediaAccess.eq(MediaAccess::Authorized))
            .filter(call_sessions::Column::MediaAdmittedAt.is_not_null())
            .order_by_asc(call_sessions::Column::CreatedAt)
            .all(txn)
            .await?;
        let mut chosen: Vec<call_sessions::Model> = Vec::new();
        if let Some(required) = including {
            if let Some(s) = sessions.iter().find(|s| s.id == required) {
                chosen.push(s.clone());
            } else {
                return Ok(None);
            }
        }
        for session in &sessions {
            if !active_participants.contains(&session.participant_id) {
                continue;
            }
            if chosen
                .iter()
                .any(|c| c.participant_id == session.participant_id)
            {
                continue;
            }
            chosen.push(session.clone());
            if chosen.len() >= ConnectedEvidence::MAX_REFERENCES {
                break;
            }
        }
        if chosen.len() < 2 {
            return Ok(None);
        }
        Ok(Some(ConnectedEvidence::RtcMediaPathEstablished {
            session_ids: chosen
                .iter()
                .map(|s| crate::calls::ids::CallSessionId::from_uuid(s.id))
                .collect(),
        }))
    }

    /// Completes an accepted transfer once the target's session is verified in the media
    /// resource next to a remaining party: the transferring participant is retired (its
    /// sessions removed — provider removal follows commit), the target leg is CONNECTED,
    /// the transfer COMPLETED and the Call CONNECTED again under the same Call id.
    pub(crate) async fn complete_transfer_if_joined(&self, call_id: Uuid) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(());
        };
        if call.current_state != CallState::Transferring {
            txn.commit().await?;
            return Ok(());
        }
        let Some(transfer) = self.calls.active_transfer(&txn, &call).await? else {
            txn.commit().await?;
            return Ok(());
        };
        if transfer.state != TransferState::Accepted {
            txn.commit().await?;
            return Ok(());
        }
        let Some(target_id) = transfer.target_participant_id else {
            txn.commit().await?;
            return Ok(());
        };
        let target_session = call_sessions::Entity::find()
            .filter(call_sessions::Column::ParticipantId.eq(target_id))
            .filter(call_sessions::Column::State.eq(SessionState::Connected))
            .filter(call_sessions::Column::MediaAdmittedAt.is_not_null())
            .one(&txn)
            .await?;
        let Some(target_session) = target_session else {
            txn.commit().await?;
            return Ok(());
        };
        let cmd = system_cmd(&call);
        let transferring = participant_of(
            &txn,
            &call,
            CallParticipantId::from_uuid(transfer.transferring_participant_id),
        )
        .await?;
        let (_, retired) = self
            .calls
            .retire_participant_in(&txn, &cmd, &call, transferring, "TRANSFERRED")
            .await?;
        let call = self.locked(&txn, &call).await?;
        let Some(evidence) = self
            .media_path_evidence(&txn, &call, Some(target_session.id))
            .await?
        else {
            // No remaining party is connected: the transfer cannot complete; it times out
            // through the join deadline and recovers then.
            txn.rollback().await?;
            return Ok(());
        };
        if let Some(leg_id) = transfer.new_leg_id {
            let leg = leg_of(&txn, &call, CallLegId::from_uuid(leg_id)).await?;
            if leg.state.can_transition_to(LegState::Connected) {
                self.calls
                    .move_leg_in(&txn, &cmd, &call, leg, LegState::Connected, None)
                    .await?;
            }
        }
        if let Some(attempt_id) = transfer.attempt_id {
            let attempt = attempt_of(&txn, &call, CallAttemptId::from_uuid(attempt_id)).await?;
            if matches!(
                attempt.outcome,
                AttemptOutcome::Pending | AttemptOutcome::Unknown
            ) {
                self.calls
                    .finish_attempt_in(&txn, &cmd, &call, attempt, AttemptOutcome::Succeeded, None)
                    .await?;
            }
        }
        self.calls
            .transfer_completed_in(&txn, &cmd, &call, transfer)
            .await?;
        let call = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::CompleteTransfer { evidence },
            )
            .await?
            .call;
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Transferred,
            &call,
            &correlation,
            Map::new(),
        )
        .await?;
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Connected,
            &call,
            &correlation,
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        // Provider enforcement of the retired party's removal.
        self.enforce_removals(&call, retired).await;
        tracing::info!(call_id = %call.id, "application transfer completed");
        Ok(())
    }

    /// Fails accepted transfers whose target never joined within the deadline.
    pub(crate) async fn expire_stalled_transfer(&self, call_id: Uuid) -> CallResult<bool> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(false);
        };
        let Some(transfer) = self.calls.active_transfer(&txn, &call).await? else {
            txn.commit().await?;
            return Ok(false);
        };
        let stalled = transfer.state == TransferState::Accepted
            && transfer
                .accepted_at
                .is_some_and(|at| self.calls.now() - at >= self.config.transfer_join_deadline);
        if !stalled || call.current_state != CallState::Transferring {
            txn.commit().await?;
            return Ok(false);
        }
        let cmd = system_cmd(&call);
        let correlation = cmd.correlation_id().as_str().to_string();
        let recovered = self
            .fail_transfer_and_recover_in(
                &txn,
                &cmd,
                &call,
                transfer,
                TransferFailureReason::MediaPathLost,
                FailureCategory::Media,
            )
            .await?;
        let mut pending = PendingSignals::default();
        self.ring_outcome_signals(
            &txn,
            &mut pending,
            super::ringing::RingOutcome::TransferFailed(recovered),
            &call,
            &correlation,
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        Ok(true)
    }
}
