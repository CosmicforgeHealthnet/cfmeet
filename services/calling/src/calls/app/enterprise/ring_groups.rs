//! Ring-group execution on ONE Call (Part F §40–§44).
//!
//! * SIMULTANEOUS: every eligible member (bounded by the policy's fan-out and the Call's
//!   participant limit) rings at once for the group timeout.
//! * ORDERED: one member at a time for `memberRingSeconds` (never past the group
//!   deadline); when its offers end without an answer the NEXT eligible member rings —
//!   driven by the durable invitation expiry worker, so stepping survives restarts.
//! * Every offer of a group carries the group's `answer_group_id`: the first answering
//!   member claims the group (`call_answer_group_claims`), every other live offer of the
//!   group is ANSWERED_ELSEWHERE and those members leave the Call. A second member can
//!   never become a winner (application check + database invariant).
//! * No answer: the group's fallback (extension, queue, voicemail) takes the SAME Call,
//!   within the hop budget; otherwise the ordinary DECLINED / NO_ANSWER outcome applies.

use chrono::Duration;
use sea_orm::{ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter};
use uuid::Uuid;

use super::{
    context_of, exec, record_step, router, update_context, ContextRow, Continuation,
    EnterprisePhase, RouteAction,
};
use crate::{
    calls::{
        app::{scope_of, AppCallService},
        command::CallCommandContext,
        errors::{CallError, CallResult},
        kinds::{InvitationPurpose, InvitationStatus, InvitationTerminalReason},
    },
    domain::PrincipalId,
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions,
        forwarding::ForwardingCondition,
        policy::{effective_policy, EffectivePolicy},
        ring_groups::{self, RingGroupRow, RingStrategy},
    },
    models::{call_invitations, call_legs, call_participants, calls},
    platform::principals::{PrincipalReference, ResolvedPrincipal},
};

type Rung = (calls::Model, Vec<call_invitations::Model>);

impl AppCallService {
    /// The members of `group` in order, resolved to ACTIVE principals (an extension
    /// member to its principal destination); unusable members are skipped.
    pub(crate) async fn group_members(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        group: &RingGroupRow,
    ) -> CallResult<Vec<(i16, ResolvedPrincipal)>> {
        let scope = scope_of(call);
        let mut out = Vec::new();
        for member in ring_groups::members(txn, group.id)
            .await
            .map_err(super::enterprise_error)?
        {
            let Some(dest) = member.destination() else {
                continue;
            };
            let principal_id = match dest.kind {
                DestinationType::Extension => {
                    match extensions::extension_row(txn, scope, dest.id, false).await {
                        Ok(ext) if ext.status == "ACTIVE" => match ext.destination() {
                            Ok(d) if d.kind.is_principal() => d.id,
                            _ => continue,
                        },
                        _ => continue,
                    }
                }
                k if k.is_principal() => dest.id,
                _ => continue,
            };
            if let Ok(principal) = self
                .principals
                .resolve_in(
                    scope.tenant_id(),
                    scope.application_id(),
                    scope.environment_id(),
                    &PrincipalReference::Id(PrincipalId::from_uuid(principal_id)),
                )
                .await
            {
                if dest.kind == DestinationType::EnterpriseMember
                    && principal.kind() != crate::domain::PrincipalKind::EnterpriseMember
                {
                    continue;
                }
                if principal.id().as_uuid() == call.initiator_principal_id {
                    continue;
                }
                out.push((member.position, principal));
            }
        }
        // Part H §70: members blocked from (or blocking) the caller are skipped silently.
        let candidates: Vec<Uuid> = out.iter().map(|(_, p)| p.id().as_uuid()).collect();
        let blocked = crate::calls::app::blocks::blocked_targets(
            txn,
            scope.environment_id().as_uuid(),
            call.initiator_principal_id,
            &candidates,
        )
        .await?;
        out.retain(|(_, p)| !blocked.contains(&p.id().as_uuid()));
        Ok(out)
    }

    /// Rings `principal` as a member of answer group `group_id` for `ttl`. Returns
    /// `None` when the member has no eligible endpoint.
    async fn ring_member(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        principal: &ResolvedPrincipal,
        group_id: Uuid,
        ttl: Duration,
    ) -> CallResult<Option<Vec<call_invitations::Model>>> {
        let endpoints = self
            .endpoints
            .eligible_in(
                txn,
                scope_of(call),
                principal.id().as_uuid(),
                self.config.max_endpoints_per_participant,
            )
            .await?;
        if endpoints.is_empty() {
            return Ok(None);
        }
        let (participant, _) = self
            .calls
            .add_participant_in(txn, cmd, call, principal, None)
            .await?;
        if participant.ended_at.is_some() {
            return Ok(None);
        }
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
                Some(group_id),
            )
            .await?;
        Ok(Some(invitations))
    }

    /// Starts ringing `group` for the Call (first ring step).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_ring_group(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        context: &ContextRow,
        policy: &EffectivePolicy,
        group: RingGroupRow,
        destination: Destination,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        let answer_group = Uuid::new_v4();
        let deadline = now + Duration::seconds(group.group_timeout_seconds as i64);
        let members = self.group_members(txn, call, &group).await?;
        let mut invitations = Vec::new();
        let mut cursor: Option<i16> = None;
        match group.strategy() {
            RingStrategy::Simultaneous => {
                let fanout = (policy.rules.max_ring_group_fanout as usize)
                    .min(self.config.max_group_participants.saturating_sub(1))
                    .max(1);
                let mut rung = 0usize;
                for (_, principal) in &members {
                    if rung >= fanout {
                        break;
                    }
                    if let Some(offers) = self
                        .ring_member(
                            txn,
                            cmd,
                            call,
                            principal,
                            answer_group,
                            Duration::seconds(group.group_timeout_seconds as i64),
                        )
                        .await?
                    {
                        rung += 1;
                        invitations.extend(offers);
                    }
                }
                crate::metrics::inc(
                    "calling_enterprise_ring_group_attempts_total",
                    &[("strategy", "SIMULTANEOUS")],
                );
            }
            RingStrategy::Ordered => {
                let ttl = Duration::seconds(
                    (group.member_ring_seconds as i64).min(group.group_timeout_seconds as i64),
                );
                for (position, principal) in &members {
                    if let Some(offers) = self
                        .ring_member(txn, cmd, call, principal, answer_group, ttl)
                        .await?
                    {
                        cursor = Some(*position);
                        invitations.extend(offers);
                        break;
                    }
                }
                crate::metrics::inc(
                    "calling_enterprise_ring_group_attempts_total",
                    &[("strategy", "ORDERED")],
                );
            }
        }
        exec(
            txn,
            "UPDATE enterprise_call_contexts SET ring_group_id = $2, ring_cursor = $3, \
             answer_group_id = $4, ring_deadline = $5, version = version + 1, \
             updated_at = GREATEST(updated_at, $6) WHERE call_id = $1",
            vec![
                call.id.into(),
                group.id.into(),
                cursor.into(),
                answer_group.into(),
                deadline.into(),
                now.into(),
            ],
        )
        .await?;
        let context = context_of(txn, call.id).await?.unwrap_or(context.clone());
        if invitations.is_empty() {
            record_step(
                &self.calls,
                txn,
                call,
                "RING_GROUP_EMPTY",
                Some(destination),
                "NO_ELIGIBLE_MEMBER",
                Some(answer_group),
                now,
            )
            .await?;
            return self
                .group_fallback(txn, cmd, call, leg, &context, policy, &group)
                .await;
        }
        let call = self.mark_ringing(txn, cmd, call, leg).await?;
        record_step(
            &self.calls,
            txn,
            &call,
            match group.strategy() {
                RingStrategy::Simultaneous => "RING_GROUP_SIMULTANEOUS",
                RingStrategy::Ordered => "RING_GROUP_MEMBER",
            },
            Some(destination),
            "DESTINATION_RESOLVED",
            Some(answer_group),
            now,
        )
        .await?;
        update_context(
            txn,
            &context,
            EnterprisePhase::RingGroup,
            Some(destination),
            None,
            now,
        )
        .await?;
        Ok((call, invitations))
    }

    /// No member answered: the group's fallback takes the Call (hop-budgeted), or the
    /// Call ends with the ordinary outcome (returns no offers; the caller derives it).
    #[allow(clippy::too_many_arguments)]
    async fn group_fallback(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        context: &ContextRow,
        policy: &EffectivePolicy,
        group: &RingGroupRow,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        let Some(fallback) = group.fallback() else {
            crate::metrics::inc("calling_enterprise_ring_group_no_answer_total", &[]);
            let call = self.locked(txn, call).await?;
            return Ok((call, Vec::new()));
        };
        record_step(
            &self.calls,
            txn,
            call,
            "FALLBACK",
            Some(fallback),
            "RING_GROUP_NO_ANSWER",
            None,
            now,
        )
        .await?;
        let context = context_of(txn, call.id).await?.unwrap_or(context.clone());
        let action = router::resolve(
            &self.calls,
            txn,
            &self.principals,
            scope_of(call),
            policy,
            &context,
            fallback,
            now,
        )
        .await?;
        // A fallback may lead to another group (sequential, never nested membership);
        // the visited set and hop budget make a chain back to this group impossible.
        self.apply_route_action(txn, cmd, call, leg, &context, policy, action)
            .await
    }

    /// Ring-round exhaustion hook (called by `evaluate_ring_outcome_in` BEFORE the
    /// ordinary DECLINED / NO_ANSWER derivation of a DIALING / RINGING Call): the next
    /// ordered member, or the group's fallback. `None` = no enterprise continuation.
    pub(crate) async fn enterprise_continue_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<Option<Continuation>> {
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(None);
        };
        if context.phase == EnterprisePhase::Queued.as_str() {
            // A queue offer ended unanswered: the Call waits for the next agent.
            return self
                .queue_offer_ended_in(txn, cmd, call, &context)
                .await
                .map(Some);
        }
        if context.phase == EnterprisePhase::Ringing.as_str() {
            // One principal rang: its BUSY / NO_ANSWER forwarding, if any.
            return self.principal_forward_in(txn, cmd, call, &context).await;
        }
        let Some(group_id) = context.ring_group_id else {
            return Ok(None);
        };
        if context.phase != EnterprisePhase::RingGroup.as_str() {
            return Ok(None);
        }
        let scope = scope_of(call);
        let policy = effective_policy(txn, scope, context.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        let group = ring_groups::ring_group_row(txn, scope, group_id, false)
            .await
            .map_err(super::enterprise_error)?;
        let leg = self.rtc_leg(txn, call).await?;
        let now = self.calls.now();
        if group.strategy() == RingStrategy::Ordered && group.status == "ACTIVE" {
            let deadline = context.ring_deadline.unwrap_or(now);
            let cursor = context.ring_cursor.unwrap_or(0);
            if now < deadline {
                let remaining = deadline - now;
                let ttl = Duration::seconds(group.member_ring_seconds as i64).min(remaining);
                let answer_group = context.answer_group_id.unwrap_or_else(Uuid::new_v4);
                for (position, principal) in self.group_members(txn, call, &group).await? {
                    if position <= cursor {
                        continue;
                    }
                    if let Some(offers) = self
                        .ring_member(txn, cmd, call, &principal, answer_group, ttl)
                        .await?
                    {
                        exec(
                            txn,
                            "UPDATE enterprise_call_contexts SET ring_cursor = $2, \
                             version = version + 1, updated_at = GREATEST(updated_at, $3) \
                             WHERE call_id = $1",
                            vec![call.id.into(), position.into(), now.into()],
                        )
                        .await?;
                        record_step(
                            &self.calls,
                            txn,
                            call,
                            "RING_GROUP_MEMBER",
                            Some(Destination::new(DestinationType::RingGroup, group.id)),
                            "PREVIOUS_MEMBER_NO_ANSWER",
                            Some(answer_group),
                            now,
                        )
                        .await?;
                        let call = self.locked(txn, call).await?;
                        return Ok(Some(Continuation::Rung(call, offers)));
                    }
                }
            }
        }
        let rung = self
            .group_fallback(txn, cmd, call, &leg, &context, &policy, &group)
            .await?;
        self.classify(txn, rung).await
    }

    /// After a principal's ring offers all ended unanswered: BUSY (every offer declined) or
    /// NO_ANSWER forwarding of that principal takes the SAME Call (hop-budgeted, loop-safe).
    async fn principal_forward_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
    ) -> CallResult<Option<Continuation>> {
        let Some(destination) = context.destination().filter(|d| d.kind.is_principal()) else {
            return Ok(None);
        };
        let statuses = super::strings_in(
            txn,
            "SELECT status FROM call_invitations WHERE call_id = $1 AND principal_id = $2 \
               AND purpose = 'CALL'",
            vec![call.id.into(), destination.id.into()],
        )
        .await?;
        if statuses.is_empty() {
            return Ok(None);
        }
        let condition = if statuses.iter().all(|s| s == "DECLINED") {
            ForwardingCondition::Busy
        } else {
            ForwardingCondition::NoAnswer
        };
        let scope = scope_of(call);
        let policy = effective_policy(txn, scope, context.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        let Some(action) = router::resolve_forward(
            &self.calls,
            txn,
            &self.principals,
            scope,
            &policy,
            context,
            destination.id,
            condition,
            self.calls.now(),
        )
        .await?
        else {
            return Ok(None);
        };
        if let RouteAction::NoRoute {
            reason,
            destination,
        } = action
        {
            // A forward that would loop / exceed the budget: the ordinary outcome
            // (NO_ANSWER / DECLINED) applies, with the evidence.
            record_step(
                &self.calls,
                txn,
                call,
                "NO_ROUTE",
                destination,
                reason,
                None,
                self.calls.now(),
            )
            .await?;
            return Ok(None);
        }
        let context = context_of(txn, call.id).await?.unwrap_or(context.clone());
        let leg = self.rtc_leg(txn, call).await?;
        let rung =
            Box::pin(self.apply_route_action(txn, cmd, call, &leg, &context, &policy, action))
                .await?;
        self.classify(txn, rung).await
    }

    /// Classifies what a continuation step led to. `None` = no enterprise continuation
    /// (the ordinary DECLINED / NO_ANSWER outcome applies).
    pub(crate) async fn classify(
        &self,
        txn: &DatabaseTransaction,
        (call, offers): Rung,
    ) -> CallResult<Option<Continuation>> {
        if call.current_state.is_terminal() || !offers.is_empty() {
            return Ok(Some(Continuation::Rung(call, offers)));
        }
        if self.waiting_in(txn, &call).await? {
            return Ok(Some(Continuation::Queued(call)));
        }
        Ok(None)
    }

    /// The Call's application leg (the RTC media path).
    pub(crate) async fn rtc_leg(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<call_legs::Model> {
        let binding = self
            .calls
            .media_binding_in(txn, call.id)
            .await?
            .ok_or(CallError::InvalidState("the call has no media binding"))?;
        crate::calls::service::leg_of(
            txn,
            call,
            crate::calls::ids::CallLegId::from_uuid(binding.leg_id),
        )
        .await
    }

    /// Group-level single winner (§41): claims `invitation`'s answer group for its
    /// participant. A group already claimed by another participant is ANSWERED_ELSEWHERE
    /// for this answer. On success every other live offer of the group ends
    /// ANSWERED_ELSEWHERE and those members leave the Call (returned for signals).
    pub(crate) async fn claim_answer_group_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        invitation: &call_invitations::Model,
        participant_id: Uuid,
    ) -> CallResult<Vec<call_invitations::Model>> {
        let Some(group) = invitation.answer_group_id else {
            return Ok(Vec::new());
        };
        let claimed = exec(
            txn,
            "INSERT INTO call_answer_group_claims (call_id, answer_group_id, tenant_id, \
             application_id, environment_id, participant_id, invitation_id, claimed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
            vec![
                call.id.into(),
                group.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                participant_id.into(),
                invitation.id.into(),
                self.calls.now().into(),
            ],
        )
        .await?;
        if claimed == 0 {
            crate::metrics::inc(
                "calling_enterprise_group_answer_conflicts_total",
                &[("result", "ANSWERED_ELSEWHERE")],
            );
            return Err(CallError::AnsweredElsewhere);
        }
        let mut losers = Vec::new();
        let others = call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call.id))
            .filter(call_invitations::Column::AnswerGroupId.eq(group))
            .filter(call_invitations::Column::ParticipantId.ne(participant_id))
            .filter(
                call_invitations::Column::Status
                    .is_in([InvitationStatus::Pending, InvitationStatus::Ringing]),
            )
            .all(txn)
            .await?;
        let mut losing_participants: Vec<Uuid> = Vec::new();
        for other in others {
            losing_participants.push(other.participant_id);
            losers.push(
                self.calls
                    .finish_invitation_in(
                        txn,
                        cmd,
                        call,
                        other,
                        InvitationStatus::AnsweredElsewhere,
                        InvitationTerminalReason::AnsweredElsewhere,
                        None,
                    )
                    .await?,
            );
        }
        losing_participants.sort();
        losing_participants.dedup();
        for id in losing_participants {
            if let Some(member) = call_participants::Entity::find_by_id(id).one(txn).await? {
                self.calls
                    .retire_participant_in(txn, cmd, call, member, "ANSWERED_ELSEWHERE")
                    .await?;
            }
        }
        crate::metrics::inc(
            "calling_enterprise_ring_group_winners_total",
            &[("result", "ANSWERED")],
        );
        Ok(losers)
    }
}
