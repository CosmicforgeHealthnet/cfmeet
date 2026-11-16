//! C7 — inviting participants into an ongoing call (`POST /v1/calls/{id}/participants`).
//!
//! * Authority: the application backend (`calls.write`) names the inviting participant —
//!   an ACTIVE participant of the Call (the CF Meet web adapter injects the signed-in
//!   user's own). Targets are resolved in the credential's scope exactly like call
//!   creation: principal categories, block rules between every active party and every
//!   target (Part H §70, never disclosing which side), and — for a governed personal scope
//!   — the Communications policy (`DirectCall`) from the inviter and from the initiator.
//! * Rules: the Call must be ANSWERED, CONNECTED or HELD with an active media resource
//!   (pre-answer, TRANSFERRING, ENDING and terminal Calls are refused); `expectedVersion`
//!   is checked under the Call row lock (the version itself is unchanged: adding a
//!   participant is not a state transition). The participant limit
//!   (`CALLING_MAX_GROUP_PARTICIPANTS`, initiator included) counts every active participant
//!   — concurrent invitations serialize on the row lock, so they can never exceed it. A
//!   principal that already participates (or participated) is refused (409 CONFLICT);
//!   a target without an eligible endpoint makes the whole request fail (409
//!   TARGET_UNAVAILABLE) — nothing is half-applied. Protected (Privacy Relay),
//!   public-network and enterprise Calls are not supported (422 CAPABILITY_UNAVAILABLE):
//!   they introduce parties only through their own authorities.
//! * Effect: each target becomes a RECIPIENT participant with server-derived media
//!   permissions (the Call's current kind) and PENDING ring offers (purpose CALL, the usual
//!   TTL) on its eligible endpoints; `call.incoming` (+ push) reaches the targets' devices
//!   and `call.participants_changed` every endpoint of the Call. The target answers through
//!   the normal accept path (valid for an answered Call) and joins the shared media
//!   resource.
//! * Multiparty: the automatic end (CAL-001) already counts the parties with live
//!   participation NOW, whatever the Call started as — a one-to-one Call that gained a third
//!   party continues when one of three leaves. An added participant's offers expiring never
//!   end the ongoing Call (`evaluate_ring_outcome_in` derives no outcome once answered);
//!   history reports it as a MISSED inbound Call for that participant.

use std::collections::HashSet;

use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement, TransactionTrait,
};
use serde_json::{json, Map};
use uuid::Uuid;

use super::{
    create::resolution, ringing::replay, summary, AppCallService, CommandOutcome, PendingSignals,
};
use crate::{
    calls::{
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallId, CallParticipantId},
        kinds::{CommandOperation, InvitationPurpose, MediaSessionStatus},
        service::{participant_of, CommandKey},
        state::CallState,
    },
    domain::PrincipalId,
    models::call_participants,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, ResolvedPrincipal},
    },
    signaling::signals::SignalType,
};

/// `POST /v1/calls/{call_id}/participants` input.
#[derive(Debug, Clone)]
pub struct InviteParticipants {
    pub inviting_participant: CallParticipantId,
    pub targets: Vec<PrincipalId>,
    pub expected_version: i64,
}

/// States in which participants may be invited into a Call.
pub(crate) fn invitable_state(state: CallState) -> bool {
    matches!(
        state,
        CallState::Answered | CallState::Connected | CallState::Held
    )
}

impl AppCallService {
    /// `POST /v1/calls/{id}/participants` (C7).
    pub async fn invite_participants(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        request: InviteParticipants,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let scope = CallScope::of_platform(ctx);
        if let Some(key) = key {
            if let Some(prior) = self.calls.prior_command(self.db(), scope, key).await? {
                return replay(prior);
            }
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        if request.targets.is_empty() {
            return Err(CallError::Validation(
                "targetPrincipalIds must not be empty.",
            ));
        }
        let distinct: HashSet<_> = request.targets.iter().collect();
        if distinct.len() != request.targets.len() {
            return Err(CallError::Validation(
                "targetPrincipalIds must be distinct.",
            ));
        }
        if request.targets.len() + 1 > self.config.max_group_participants {
            return Err(CallError::ParticipantLimitExceeded {
                limit: self.config.max_group_participants as i64,
            });
        }
        // Resolution in the credential's own scope, before any lock (as creation does).
        let mut targets: Vec<ResolvedPrincipal> = Vec::with_capacity(request.targets.len());
        for target in &request.targets {
            targets.push(
                self.principals
                    .resolve(ctx, &PrincipalReference::Id(*target))
                    .await
                    .map_err(|e| resolution(e, false))?,
            );
        }
        if targets
            .iter()
            .any(|t| !t.kind().may_be_application_call_target())
        {
            return Err(CallError::TargetUnavailable);
        }
        let target_ids: Vec<Uuid> = targets.iter().map(|t| t.id().as_uuid()).collect();
        let cmd = CallCommandContext::customer(ctx);
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        if call.version != request.expected_version {
            return Err(CallError::StaleVersion {
                expected: request.expected_version,
                current: call.version,
            });
        }
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !invitable_state(call.current_state) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: CommandOperation::AddParticipants.as_str(),
            });
        }
        if call.privacy_relay_ref.is_some() || foreign_authority(&txn, call.id).await? {
            return Err(CallError::CapabilityUnavailable(
                "protected, public-network and enterprise calls cannot add participants",
            ));
        }
        self.calls
            .media_binding_in(&txn, call.id)
            .await?
            .filter(|b| b.status == MediaSessionStatus::Active)
            .ok_or(CallError::InvalidState(
                "the call has no active media resource",
            ))?;
        let inviter = participant_of(&txn, &call, request.inviting_participant).await?;
        if inviter.ended_at.is_some() {
            return Err(CallError::InvalidState(
                "the inviting participant has left the call",
            ));
        }
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .all(&txn)
            .await?;
        if participants
            .iter()
            .any(|p| target_ids.contains(&p.principal_id))
        {
            return Err(CallError::Conflict(
                "a target already participates in this call",
            ));
        }
        let active: Vec<Uuid> = participants
            .iter()
            .filter(|p| p.ended_at.is_none())
            .map(|p| p.principal_id)
            .collect();
        if active.len() + targets.len() > self.config.max_group_participants {
            return Err(CallError::ParticipantLimitExceeded {
                limit: self.config.max_group_participants as i64,
            });
        }
        // Part H §70: no target may be blocked by (or block) any active party.
        for party in &active {
            if !super::blocks::blocked_targets(&txn, call.environment_id, *party, &target_ids)
                .await?
                .is_empty()
            {
                txn.rollback().await?;
                self.note_refusal(
                    scope,
                    inviter.principal_id,
                    super::blocks::RefusalKind::BlockedAttempt,
                    self.calls.now(),
                )
                .await;
                return Err(CallError::TargetUnavailable);
            }
        }
        if self.personal_call_governed(&call).await? {
            self.require_personal_call(&call, None).await?;
            self.require_personal_create(ctx, inviter.principal_id, &target_ids)
                .await?;
            if call.initiator_principal_id != inviter.principal_id {
                self.require_personal_create(ctx, call.initiator_principal_id, &target_ids)
                    .await?;
            }
        }
        let mut added = Vec::with_capacity(targets.len());
        let mut invitations = Vec::new();
        for target in &targets {
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
                // Every target is required: nothing of this request is applied.
                return Err(CallError::TargetUnavailable);
            }
            let (participant, _) = self
                .calls
                .add_participant_in(&txn, &cmd, &call, target, None)
                .await?;
            self.calls
                .ensure_media_permissions_in(&txn, &call, &participant)
                .await?;
            let offers = self
                .calls
                .issue_invitations_in(
                    &txn,
                    &cmd,
                    &call,
                    &participant,
                    &endpoints,
                    InvitationPurpose::Call,
                    None,
                    self.config.invitation_ttl,
                    None,
                )
                .await?;
            added.push(json!({
                "participantId": participant.id,
                "principalId": participant.principal_id,
                "role": participant.role.as_str(),
                "liveInvitationCount": offers.len(),
            }));
            invitations.extend(offers);
        }
        let call = self.locked(&txn, &call).await?;
        let body = json!({
            "call": summary(&call),
            "operation": CommandOperation::AddParticipants.as_str(),
            "invitingParticipantId": inviter.id,
            "participants": added,
        });
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    &cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    None,
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
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
        let mut data = Map::new();
        data.insert("participantsAdded".into(), json!(targets.len()));
        data.insert(
            "activeParticipants".into(),
            json!(active.len() + targets.len()),
        );
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::ParticipantsChanged,
            &call,
            &correlation,
            data,
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::add(
            "calling_participants_invited_total",
            &[("call_kind", call.call_kind.as_str())],
            targets.len() as u64,
        );
        tracing::info!(call_id = %call.id, added = targets.len(), offers = invitations.len(), "participants invited into the call");
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }
}

/// Whether another authority introduces the Call's parties: a public / private network leg
/// or an enterprise routing context.
async fn foreign_authority<C: ConnectionTrait>(db: &C, call_id: Uuid) -> CallResult<bool> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 WHERE EXISTS (SELECT 1 FROM call_legs WHERE call_id = $1 \
                                     AND leg_kind <> 'APPLICATION') \
                        OR EXISTS (SELECT 1 FROM enterprise_call_contexts WHERE call_id = $1)",
            vec![call_id.into()],
        ))
        .await?
        .is_some())
}
