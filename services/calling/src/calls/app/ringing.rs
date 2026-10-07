//! Ringing, single-winner arbitration, decline and no-answer (Part C §11–§18, §58, §59).

use chrono::Duration;
use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{summary, AppCallService, CommandOutcome, PendingSignals};
use crate::{
    calls::{
        command::CallCommandContext,
        errors::{CallError, CallResult},
        ids::{CallId, CallInvitationId, CallLegId, CallParticipantId},
        kinds::{
            AttemptOutcome, FailureCategory, InvitationPurpose, InvitationStatus,
            InvitationTerminalReason, LegState, TransferFailureReason, TransferState,
        },
        service::{participant_of, CommandKey, PriorCommand},
        state::CallState,
        transitions::CallAction,
    },
    domain::PrincipalId,
    endpoints::{CallEndpointId, EndpointStatus},
    models::{call_endpoints, call_invitations, calls},
    platform::principals::{PrincipalReference, PrincipalResolutionError},
    signaling::{signals::SignalType, SignalingIdentity},
};

/// Who answers / declines a ring offer. Either way the server derives participant and
/// session from the invitation; an invitation id alone never grants anything.
#[derive(Debug, Clone)]
pub enum InvitationActor {
    /// The application backend (API credential) acting for an explicit endpoint.
    Backend {
        cmd: CallCommandContext,
        endpoint_id: CallEndpointId,
    },
    /// The endpoint itself, through its own signaling credential.
    Endpoint(SignalingIdentity),
}

impl InvitationActor {
    fn cmd(&self) -> CallCommandContext {
        match self {
            InvitationActor::Backend { cmd, .. } => cmd.clone(),
            InvitationActor::Endpoint(identity) => CallCommandContext::endpoint(identity),
        }
    }

    fn endpoint_id(&self) -> CallEndpointId {
        match self {
            InvitationActor::Backend { endpoint_id, .. } => *endpoint_id,
            InvitationActor::Endpoint(identity) => identity.endpoint_id(),
        }
    }

    fn principal(&self) -> Option<PrincipalId> {
        match self {
            InvitationActor::Backend { .. } => None,
            InvitationActor::Endpoint(identity) => Some(identity.principal_id()),
        }
    }
}

/// An invitation as a reconnecting endpoint recovers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredInvitation {
    pub invitation_id: Uuid,
    pub call_id: Uuid,
    pub call_kind: &'static str,
    pub call_state: &'static str,
    pub call_version: i64,
    pub purpose: &'static str,
    pub status: &'static str,
    pub actionable: bool,
    pub issued_at: String,
    pub expires_at: String,
    pub terminal_reason: Option<&'static str>,
    pub session_id: Option<Uuid>,
}

/// What a finished ring round led to.
pub(crate) enum RingOutcome {
    Unchanged,
    /// The Call reached a terminal state (DECLINED / NO_ANSWER).
    CallEnded(calls::Model),
    /// A transfer failed and the Call was recovered (or is ending).
    TransferFailed(calls::Model),
}

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl AppCallService {
    /// Locates invitation `id` within the actor's scope and endpoint (anything else is
    /// not found: a foreign or other-endpoint offer is indistinguishable from none).
    async fn invitation_for(
        &self,
        txn: &DatabaseTransaction,
        actor: &InvitationActor,
        call_id: Option<CallId>,
        id: CallInvitationId,
    ) -> CallResult<call_invitations::Model> {
        let scope = actor.cmd().scope();
        let invitation = call_invitations::Entity::find_by_id(id.as_uuid())
            .filter(call_invitations::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(call_invitations::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(call_invitations::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .one(txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if call_id.is_some_and(|c| c.as_uuid() != invitation.call_id)
            || invitation.endpoint_id != actor.endpoint_id().as_uuid()
            || actor
                .principal()
                .is_some_and(|p| p.as_uuid() != invitation.principal_id)
        {
            return Err(CallError::NotFound);
        }
        Ok(invitation)
    }

    /// Accepts a ring offer (§58). Transactional single-winner arbitration per logical
    /// participant: the Call row is locked, the invitation re-read under lock, and the
    /// participant's claim is inserted under `uq_call_answer_claims_participant`. The loser
    /// receives ANSWERED_ELSEWHERE and never a session or media authority.
    pub async fn accept_invitation(
        &self,
        actor: InvitationActor,
        call_id: Option<CallId>,
        invitation_id: CallInvitationId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let cmd = actor.cmd();
        let scope = cmd.scope();
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let located = self
            .invitation_for(&txn, &actor, call_id, invitation_id)
            .await?;
        let call = self
            .calls
            .lock_call(&txn, scope, CallId::from_uuid(located.call_id))
            .await?;
        let invitation = self
            .calls
            .lock_invitation(&txn, &call, invitation_id)
            .await?;
        let now = self.calls.now();
        match invitation.status {
            InvitationStatus::AnsweredElsewhere => return Err(CallError::AnsweredElsewhere),
            InvitationStatus::Expired => return Err(CallError::InvitationExpired),
            InvitationStatus::Accepted => {
                // The same endpoint repeating its own successful answer: same result.
                let body = accept_body(&call, &invitation, false);
                txn.commit().await?;
                return Ok(CommandOutcome {
                    body,
                    replayed: true,
                });
            }
            InvitationStatus::Declined | InvitationStatus::Cancelled => {
                return Err(CallError::InvitationNotActionable)
            }
            InvitationStatus::Pending | InvitationStatus::Ringing => {}
        }
        // Server time decides; an old push opened after expiry can never answer.
        if invitation.expires_at <= now {
            return Err(CallError::InvitationExpired);
        }
        if call.current_state.is_terminal() || call.current_state == CallState::Ending {
            return Err(CallError::InvitationNotActionable);
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        if invitation.status != InvitationStatus::Ringing {
            return Err(CallError::InvitationNotActionable);
        }
        let transfer = match invitation.purpose {
            InvitationPurpose::Call => {
                if !matches!(
                    call.current_state,
                    CallState::Dialing
                        | CallState::Ringing
                        | CallState::Answered
                        | CallState::Connected
                        | CallState::Held
                        | CallState::Transferring
                ) {
                    return Err(CallError::InvalidTransition {
                        state: call.current_state,
                        action: "ACCEPT",
                    });
                }
                None
            }
            InvitationPurpose::Transfer => {
                let transfer = self
                    .calls
                    .active_transfer(&txn, &call)
                    .await?
                    .filter(|t| {
                        Some(t.id) == invitation.transfer_id
                            && t.state == TransferState::RingingTarget
                    })
                    .ok_or(CallError::InvitationNotActionable)?;
                Some(transfer)
            }
        };
        let participant = participant_of(
            &txn,
            &call,
            CallParticipantId::from_uuid(invitation.participant_id),
        )
        .await?;
        if participant.ended_at.is_some() {
            return Err(CallError::InvitationNotActionable);
        }
        // The participant must still be authorized (an active principal of the scope).
        match self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(participant.principal_id)),
            )
            .await
        {
            Ok(_) => {}
            Err(PrincipalResolutionError::Unavailable { summary }) => {
                return Err(CallError::Database {
                    summary,
                    connectivity: true,
                })
            }
            Err(_) => return Err(CallError::AuthorizationDenied),
        }
        let endpoint = call_endpoints::Entity::find_by_id(invitation.endpoint_id)
            .one(&txn)
            .await?
            .filter(|e| e.status == EndpointStatus::Active)
            .ok_or(CallError::InvitationNotActionable)?;

        // Arbitration: session + claim; a second winner cannot commit.
        let session = self
            .calls
            .insert_session(
                &txn,
                &cmd,
                &call,
                &participant,
                endpoint.endpoint_kind.session_category(),
                Some(endpoint.id),
                None,
            )
            .await?;
        if !self
            .calls
            .claim_answer_in(&txn, &call, participant.id, invitation.id, session.id, now)
            .await?
        {
            return Err(CallError::AnsweredElsewhere);
        }
        let invitation = self
            .calls
            .finish_invitation_in(
                &txn,
                &cmd,
                &call,
                invitation,
                InvitationStatus::Accepted,
                InvitationTerminalReason::Accepted,
                Some(session.id),
            )
            .await?;
        let losers = self
            .calls
            .finish_live_invitations_in(
                &txn,
                &cmd,
                &call,
                Some(participant.id),
                Some(invitation.id),
                InvitationStatus::AnsweredElsewhere,
                InvitationTerminalReason::AnsweredElsewhere,
            )
            .await?;
        // Media credentials become possible only now, for this session only.
        self.calls.authorize_session_media_in(&txn, session).await?;
        self.calls
            .ensure_media_permissions_in(&txn, &call, &participant)
            .await?;

        let mut answered = false;
        let mut call = call;
        if let Some(transfer) = transfer {
            self.calls
                .transfer_accepted_in(&txn, transfer.clone())
                .await?;
            if let Some(leg_id) = transfer.new_leg_id {
                let leg = crate::calls::service::leg_of(&txn, &call, CallLegId::from_uuid(leg_id))
                    .await?;
                if leg.state.can_transition_to(LegState::Answered) {
                    self.calls
                        .move_leg_in(&txn, &cmd, &call, leg, LegState::Answered, None)
                        .await?;
                }
            }
        } else if matches!(call.current_state, CallState::Dialing | CallState::Ringing) {
            // The first target answer moves the Call; later group answers do not.
            call = self
                .calls
                .transition_in(&txn, &cmd, &call, call.version, CallAction::Accept)
                .await?
                .call;
            answered = true;
            if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                let leg = crate::calls::service::leg_of(
                    &txn,
                    &call,
                    CallLegId::from_uuid(binding.leg_id),
                )
                .await?;
                if leg.state.can_transition_to(LegState::Answered) {
                    self.calls
                        .move_leg_in(&txn, &cmd, &call, leg, LegState::Answered, None)
                        .await?;
                }
                if let Some(attempt) = self.pending_attempt(&txn, &call, binding.route_id).await? {
                    self.calls
                        .finish_attempt_in(
                            &txn,
                            &cmd,
                            &call,
                            attempt,
                            AttemptOutcome::Succeeded,
                            None,
                        )
                        .await?;
                }
            }
        }
        let body = accept_body(&call, &invitation, answered);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    &cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    Some(session_id(&invitation)),
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        let mut data = Map::new();
        data.insert("participantId".into(), json!(participant.id));
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Accepted,
            &call,
            &correlation,
            data,
        )
        .await?;
        if answered {
            self.call_signal(
                &txn,
                &mut pending,
                SignalType::Answered,
                &call,
                &correlation,
                Map::new(),
            )
            .await?;
        }
        for loser in &losers {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::AnsweredElsewhere,
                &call,
                loser,
                &correlation,
            )
            .await?;
        }
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(
            call_id = %call.id,
            participant_id = %participant.id,
            invitation_id = %invitation.id,
            answered_elsewhere = losers.len(),
            "invitation accepted"
        );
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Declines ONE ring offer (§59). The Call is declined only when every ring offer of
    /// the round was explicitly declined and no route remains; a stale decline never
    /// overrides an accepted winner.
    pub async fn decline_invitation(
        &self,
        actor: InvitationActor,
        call_id: Option<CallId>,
        invitation_id: CallInvitationId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let cmd = actor.cmd();
        let scope = cmd.scope();
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let located = self
            .invitation_for(&txn, &actor, call_id, invitation_id)
            .await?;
        let call = self
            .calls
            .lock_call(&txn, scope, CallId::from_uuid(located.call_id))
            .await?;
        let invitation = self
            .calls
            .lock_invitation(&txn, &call, invitation_id)
            .await?;
        match invitation.status {
            InvitationStatus::AnsweredElsewhere => return Err(CallError::AnsweredElsewhere),
            InvitationStatus::Expired => return Err(CallError::InvitationExpired),
            InvitationStatus::Declined => {
                let body = decline_body(&call, &invitation);
                txn.commit().await?;
                return Ok(CommandOutcome {
                    body,
                    replayed: true,
                });
            }
            InvitationStatus::Accepted | InvitationStatus::Cancelled => {
                return Err(CallError::InvitationNotActionable)
            }
            InvitationStatus::Pending | InvitationStatus::Ringing => {}
        }
        if invitation.expires_at <= self.calls.now() {
            return Err(CallError::InvitationExpired);
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        let invitation = self
            .calls
            .finish_invitation_in(
                &txn,
                &cmd,
                &call,
                invitation,
                InvitationStatus::Declined,
                InvitationTerminalReason::Declined,
                None,
            )
            .await?;
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        let outcome = self.evaluate_ring_outcome_in(&txn, &cmd, &call).await?;
        let call = self
            .ring_outcome_signals(&txn, &mut pending, outcome, &call, &correlation)
            .await?;
        if !call.current_state.is_terminal() {
            // One device declined; the Call still rings elsewhere (state unchanged).
            let mut data = Map::new();
            data.insert("participantId".into(), json!(invitation.participant_id));
            data.insert("invitationId".into(), json!(invitation.id));
            self.call_signal(
                &txn,
                &mut pending,
                SignalType::Declined,
                &call,
                &correlation,
                data,
            )
            .await?;
        }
        let body = decline_body(&call, &invitation);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    &cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    Some(invitation.id),
                )
                .await?;
        }
        txn.commit().await?;
        self.publish(pending).await;
        self.settle_media(call.id).await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Derives the Call outcome once a ring round has no live offer left (§16, §17):
    /// every offer explicitly DECLINED → DECLINED; otherwise (expired / cancelled / mixed)
    /// → NO_ANSWER. App calls have no fallback route today. A finished transfer ring round
    /// fails the transfer and recovers the Call (§67). Group calls already answered are
    /// unaffected by later targets' outcomes.
    pub(crate) async fn evaluate_ring_outcome_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<RingOutcome> {
        let invitations = call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call.id))
            .all(txn)
            .await?;
        match call.current_state {
            CallState::Dialing | CallState::Ringing => {
                let round: Vec<_> = invitations
                    .iter()
                    .filter(|i| i.purpose == InvitationPurpose::Call)
                    .collect();
                if round.is_empty() || round.iter().any(|i| i.status.is_live()) {
                    return Ok(RingOutcome::Unchanged);
                }
                let action = if round.iter().all(|i| i.status == InvitationStatus::Declined) {
                    CallAction::Decline
                } else {
                    CallAction::MarkNoAnswer
                };
                let call = self
                    .calls
                    .transition_in(txn, cmd, call, call.version, action)
                    .await?
                    .call;
                Ok(RingOutcome::CallEnded(call))
            }
            CallState::Transferring => {
                let Some(transfer) = self.calls.active_transfer(txn, call).await? else {
                    return Ok(RingOutcome::Unchanged);
                };
                if transfer.state != TransferState::RingingTarget {
                    return Ok(RingOutcome::Unchanged);
                }
                let round: Vec<_> = invitations
                    .iter()
                    .filter(|i| i.transfer_id == Some(transfer.id))
                    .collect();
                if round.iter().any(|i| i.status.is_live()) {
                    return Ok(RingOutcome::Unchanged);
                }
                let (reason, category) = if !round.is_empty()
                    && round.iter().all(|i| i.status == InvitationStatus::Declined)
                {
                    (
                        TransferFailureReason::TargetDeclined,
                        FailureCategory::TargetUnavailable,
                    )
                } else {
                    (
                        TransferFailureReason::TargetNoAnswer,
                        FailureCategory::Timeout,
                    )
                };
                let call = self
                    .fail_transfer_and_recover_in(txn, cmd, call, transfer, reason, category)
                    .await?;
                Ok(RingOutcome::TransferFailed(call))
            }
            _ => Ok(RingOutcome::Unchanged),
        }
    }

    /// Signals for a ring outcome; returns the Call as it now is.
    pub(crate) async fn ring_outcome_signals(
        &self,
        txn: &DatabaseTransaction,
        pending: &mut PendingSignals,
        outcome: RingOutcome,
        call: &calls::Model,
        correlation: &str,
    ) -> CallResult<calls::Model> {
        match outcome {
            RingOutcome::Unchanged => self.locked(txn, call).await,
            RingOutcome::CallEnded(call) => {
                if let Some(kind) = super::state_signal(call.current_state) {
                    self.call_signal(txn, pending, kind, &call, correlation, Map::new())
                        .await?;
                }
                Ok(call)
            }
            RingOutcome::TransferFailed(call) => {
                self.call_signal(
                    txn,
                    pending,
                    SignalType::TransferFailed,
                    &call,
                    correlation,
                    Map::new(),
                )
                .await?;
                if let Some(kind) = super::state_signal(call.current_state) {
                    self.call_signal(txn, pending, kind, &call, correlation, Map::new())
                        .await?;
                }
                Ok(call)
            }
        }
    }

    /// Expires every due live invitation of one Call (server time) and derives the
    /// outcome. Returns `None` if another instance holds the Call (skipped this round).
    pub(crate) async fn expire_call_invitations(&self, call_id: Uuid) -> CallResult<Option<usize>> {
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
            return Ok(None);
        };
        let cmd = super::system_cmd(&call);
        let now = self.calls.now();
        let due: Vec<_> = self
            .calls
            .live_invitations(&txn, &call, None)
            .await?
            .into_iter()
            .filter(|i| i.expires_at <= now)
            .collect();
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        let mut expired = Vec::new();
        for invitation in due {
            expired.push(
                self.calls
                    .finish_invitation_in(
                        &txn,
                        &cmd,
                        &call,
                        invitation,
                        InvitationStatus::Expired,
                        InvitationTerminalReason::Expired,
                        None,
                    )
                    .await?,
            );
        }
        for invitation in &expired {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::InvitationExpired,
                &call,
                invitation,
                &correlation,
            )
            .await?;
        }
        if !expired.is_empty() && !call.current_state.is_terminal() {
            let outcome = self.evaluate_ring_outcome_in(&txn, &cmd, &call).await?;
            self.ring_outcome_signals(&txn, &mut pending, outcome, &call, &correlation)
                .await?;
        }
        txn.commit().await?;
        self.publish(pending).await;
        if !expired.is_empty() {
            self.settle_media(call.id).await;
        }
        Ok(Some(expired.len()))
    }

    /// After a committed ring outcome: a terminal Call's media resource is torn down and
    /// an ENDING Call is finished (reconciliation retries whatever fails here).
    pub(crate) async fn settle_media(&self, call_id: Uuid) {
        let state = match calls::Entity::find_by_id(call_id).one(self.db()).await {
            Ok(Some(call)) => call.current_state,
            _ => return,
        };
        if state.is_terminal() {
            self.teardown_media(call_id).await;
        } else if state == CallState::Ending {
            if let Err(error) = self.finish_ending(call_id).await {
                tracing::warn!(%call_id, %error, "ending not finished; reconciliation retries");
            }
        }
    }

    /// Cancels the live ring offers of a revoked endpoint and derives each Call's outcome.
    pub async fn cancel_endpoint_invitations(
        &self,
        endpoint_id: CallEndpointId,
    ) -> CallResult<usize> {
        let calls_of: Vec<Uuid> = call_invitations::Entity::find()
            .filter(call_invitations::Column::EndpointId.eq(endpoint_id.as_uuid()))
            .filter(
                call_invitations::Column::Status
                    .is_in([InvitationStatus::Pending, InvitationStatus::Ringing]),
            )
            .all(self.db())
            .await?
            .into_iter()
            .map(|i| i.call_id)
            .collect();
        let mut cancelled = 0;
        for call_id in calls_of {
            let txn = self.db().begin().await?;
            let Some(call) = calls::Entity::find_by_id(call_id)
                .lock_exclusive()
                .one(&txn)
                .await?
            else {
                continue;
            };
            let cmd = super::system_cmd(&call);
            let correlation = cmd.correlation_id().as_str().to_string();
            let mut pending = PendingSignals::default();
            for invitation in self.calls.live_invitations(&txn, &call, None).await? {
                if invitation.endpoint_id != endpoint_id.as_uuid() {
                    continue;
                }
                let invitation = self
                    .calls
                    .finish_invitation_in(
                        &txn,
                        &cmd,
                        &call,
                        invitation,
                        InvitationStatus::Cancelled,
                        InvitationTerminalReason::EndpointRevoked,
                        None,
                    )
                    .await?;
                self.invitation_signal(
                    &txn,
                    &mut pending,
                    SignalType::RingCancelled,
                    &call,
                    &invitation,
                    &correlation,
                )
                .await?;
                cancelled += 1;
            }
            let outcome = self.evaluate_ring_outcome_in(&txn, &cmd, &call).await?;
            self.ring_outcome_signals(&txn, &mut pending, outcome, &call, &correlation)
                .await?;
            txn.commit().await?;
            self.publish(pending).await;
            self.settle_media(call_id).await;
        }
        Ok(cancelled)
    }

    /// Realtime recovery (§87): the endpoint's live offers and recently finished ones,
    /// with the authoritative Call state and whether each is still actionable.
    pub async fn invitations_for_endpoint(
        &self,
        identity: &SignalingIdentity,
    ) -> CallResult<Vec<RecoveredInvitation>> {
        let scope = identity.scope();
        let now = self.calls.now();
        let since = now - Duration::minutes(15);
        let invitations = call_invitations::Entity::find()
            .filter(call_invitations::Column::EndpointId.eq(identity.endpoint_id().as_uuid()))
            .filter(call_invitations::Column::PrincipalId.eq(identity.principal_id().as_uuid()))
            .filter(call_invitations::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(call_invitations::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(call_invitations::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .filter(call_invitations::Column::IssuedAt.gte(since))
            .order_by_desc(call_invitations::Column::IssuedAt)
            .limit(50)
            .all(self.db())
            .await?;
        let mut out = Vec::with_capacity(invitations.len());
        for invitation in invitations {
            let Some(call) = calls::Entity::find_by_id(invitation.call_id)
                .one(self.db())
                .await?
            else {
                continue;
            };
            let actionable = invitation.status == InvitationStatus::Ringing
                && invitation.expires_at > now
                && !call.current_state.is_terminal()
                && call.current_state != CallState::Ending;
            // A due offer the worker has not finalized yet is reported as expired.
            let status = if invitation.status.is_live() && invitation.expires_at <= now {
                InvitationStatus::Expired
            } else {
                invitation.status
            };
            out.push(RecoveredInvitation {
                invitation_id: invitation.id,
                call_id: call.id,
                call_kind: call.call_kind.as_str(),
                call_state: call.current_state.as_str(),
                call_version: call.version,
                purpose: invitation.purpose.as_str(),
                status: status.as_str(),
                actionable,
                issued_at: time(invitation.issued_at),
                expires_at: time(invitation.expires_at),
                terminal_reason: invitation.terminal_reason.map(|r| r.as_str()),
                session_id: invitation.session_id,
            });
        }
        Ok(out)
    }
}

fn session_id(invitation: &call_invitations::Model) -> Uuid {
    invitation.session_id.unwrap_or(invitation.id)
}

fn accept_body(call: &calls::Model, invitation: &call_invitations::Model, answered: bool) -> Value {
    json!({
        "call": summary(call),
        "invitationId": invitation.id,
        "participantId": invitation.participant_id,
        "sessionId": invitation.session_id,
        "outcome": "ACCEPTED",
        "callAnswered": answered,
    })
}

fn decline_body(call: &calls::Model, invitation: &call_invitations::Model) -> Value {
    json!({
        "call": summary(call),
        "invitationId": invitation.id,
        "participantId": invitation.participant_id,
        "outcome": "DECLINED",
    })
}

pub(crate) fn replay(prior: PriorCommand) -> CallResult<CommandOutcome> {
    match prior {
        PriorCommand::Completed { body, .. } => Ok(CommandOutcome {
            body,
            replayed: true,
        }),
        PriorCommand::InProgress { .. } => Err(CallError::Conflict(
            "the command with this Idempotency-Key is still in progress",
        )),
    }
}
