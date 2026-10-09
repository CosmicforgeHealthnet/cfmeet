//! Enterprise transfer targets (Part F §59, §90–§97).
//!
//! Hold, resume and transfer stay the canonical Call actions of Part C (no parallel
//! enterprise versions). Part F only extends WHO a transfer may reach and WHETHER the
//! enterprise policy permits it: an enterprise destination is resolved, in the trusted
//! scope, to ONE principal and the existing transfer runs on the SAME Call (TRANSFERRING,
//! the stable path kept until the outcome is known, recovery to CONNECTED / HELD).
//!
//! * EXTENSION: followed (bounded, loop-safe) to its final destination.
//! * ENTERPRISE_MEMBER / PRINCIPAL: that principal.
//! * RING_GROUP: the group's first member (in order) with an eligible endpoint.
//! * QUEUE: the queue's first available agent (same availability as queue offers).
//! * SCHEDULE: its current OPEN / CLOSED route.
//! * PUBLIC_CONTACT: refused — 403 when the enterprise policy forbids public forwarding,
//!   otherwise 422 (public transfer of an established Call is not offered).
//! * IVR / VOICEMAIL: not transfer targets.

use sea_orm::{DatabaseTransaction, TransactionTrait};
use uuid::Uuid;

use super::{context_of, record_step};
use crate::{
    calls::{
        app::{scope_of, AppCallService},
        command::CallScope,
        errors::{CallError, CallResult},
        ids::CallId,
    },
    domain::{PrincipalId, PrincipalKind},
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions, hours,
        policy::{effective_policy, EffectivePolicy},
        queues, ring_groups,
    },
    models::calls,
    platform::{context::PlatformAuthContext, principals::PrincipalReference},
};

/// Bound of a destination chain followed for a transfer.
const MAX_CHAIN: usize = 16;

/// Who a transfer should reach.
#[derive(Debug, Clone)]
pub enum TransferTarget {
    Principal(PrincipalId),
    Extension(String),
    Destination(Destination),
}

/// A transfer target resolved to one principal (and the enterprise destination asked).
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTransferTarget {
    pub principal: PrincipalId,
    pub destination: Option<Destination>,
}

impl AppCallService {
    /// Resolves `target` for a transfer of `call_id` (§93) and applies the enterprise
    /// policy of an enterprise Call (`transferAllowed`). Another scope's Call is 404.
    /// The HTTP layer requires `enterprise.calls.use` for an enterprise target.
    pub async fn resolve_transfer_target(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
        target: TransferTarget,
    ) -> CallResult<ResolvedTransferTarget> {
        let scope = CallScope::of_platform(ctx);
        let txn = self.db().begin().await?;
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        let context = context_of(&txn, call.id).await?;
        let enterprise_target = !matches!(target, TransferTarget::Principal(_));
        if enterprise_target && !self.enterprise_flags.enterprise_calling {
            return Err(CallError::CapabilityUnavailable(
                "enterprise calling is not enabled",
            ));
        }
        let policy = effective_policy(&txn, scope, context.as_ref().and_then(|c| c.policy_id))
            .await
            .map_err(super::enterprise_error)?;
        if (context.is_some() || enterprise_target) && !policy.rules.transfer_allowed {
            return Err(CallError::AuthorizationDenied);
        }
        let resolved = match target {
            TransferTarget::Principal(p) => ResolvedTransferTarget {
                principal: p,
                destination: None,
            },
            TransferTarget::Extension(number) => {
                let ext = extensions::resolve_number(&txn, scope, &number)
                    .await
                    .map_err(super::enterprise_error)?
                    .ok_or(CallError::NotFound)?;
                let dest = Destination::new(DestinationType::Extension, ext.id);
                ResolvedTransferTarget {
                    principal: self.transfer_principal(&txn, &call, &policy, dest).await?,
                    destination: Some(dest),
                }
            }
            TransferTarget::Destination(dest) => ResolvedTransferTarget {
                principal: self.transfer_principal(&txn, &call, &policy, dest).await?,
                destination: Some(dest),
            },
        };
        txn.commit().await?;
        Ok(resolved)
    }

    /// Follows `dest` to ONE principal.
    async fn transfer_principal(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        policy: &EffectivePolicy,
        dest: Destination,
    ) -> CallResult<PrincipalId> {
        let scope = scope_of(call);
        let mut current = dest;
        let mut visited: Vec<Uuid> = Vec::new();
        for _ in 0..MAX_CHAIN {
            if visited.contains(&current.id) {
                return Err(CallError::TargetUnavailable);
            }
            visited.push(current.id);
            match current.kind {
                DestinationType::Principal | DestinationType::EnterpriseMember => {
                    let principal = self
                        .principals
                        .resolve_in(
                            scope.tenant_id(),
                            scope.application_id(),
                            scope.environment_id(),
                            &PrincipalReference::Id(PrincipalId::from_uuid(current.id)),
                        )
                        .await
                        .map_err(|_| CallError::NotFound)?;
                    if current.kind == DestinationType::EnterpriseMember
                        && principal.kind() != PrincipalKind::EnterpriseMember
                    {
                        return Err(CallError::NotFound);
                    }
                    return Ok(principal.id());
                }
                DestinationType::Extension => {
                    let ext = extensions::extension_row(txn, scope, current.id, false)
                        .await
                        .map_err(super::enterprise_error)?;
                    if ext.status != "ACTIVE" {
                        return Err(CallError::TargetUnavailable);
                    }
                    current = ext.destination().map_err(super::enterprise_error)?;
                }
                DestinationType::Schedule => {
                    let row = hours::hours_row(txn, scope, current.id, false)
                        .await
                        .map_err(super::enterprise_error)?;
                    if row.status != "ACTIVE" {
                        return Err(CallError::TargetUnavailable);
                    }
                    let schedule = hours::schedule_of(txn, &row)
                        .await
                        .map_err(super::enterprise_error)?;
                    current = match schedule.evaluate(self.calls.now()).status {
                        hours::HoursStatus::Open => row.open(),
                        hours::HoursStatus::Closed => row.closed(),
                    }
                    .ok_or(CallError::TargetUnavailable)?;
                }
                DestinationType::RingGroup => {
                    let group = ring_groups::ring_group_row(txn, scope, current.id, false)
                        .await
                        .map_err(super::enterprise_error)?;
                    if group.status != "ACTIVE" {
                        return Err(CallError::TargetUnavailable);
                    }
                    for (_, member) in self.group_members(txn, call, &group).await? {
                        if self.reachable(txn, scope, member.id().as_uuid()).await? {
                            return Ok(member.id());
                        }
                    }
                    return Err(CallError::TargetUnavailable);
                }
                DestinationType::Queue => {
                    let queue = queues::queue_row(txn, scope, current.id, false)
                        .await
                        .map_err(super::enterprise_error)?;
                    if queue.status != "ACTIVE" {
                        return Err(CallError::TargetUnavailable);
                    }
                    for agent in self.available_agents(txn, queue.id).await? {
                        if agent != call.initiator_principal_id
                            && self.reachable(txn, scope, agent).await?
                        {
                            return Ok(PrincipalId::from_uuid(agent));
                        }
                    }
                    return Err(CallError::TargetUnavailable);
                }
                DestinationType::PublicContact => {
                    return Err(
                        if policy.rules.forwarding_allowed && policy.rules.public_forwarding_allowed
                        {
                            CallError::CapabilityUnavailable(
                                "public transfer of an established call is not offered",
                            )
                        } else {
                            CallError::AuthorizationDenied
                        },
                    );
                }
                DestinationType::Ivr | DestinationType::Voicemail => {
                    return Err(CallError::Validation(
                        "the destination cannot take a transfer.",
                    ));
                }
            }
        }
        Err(CallError::TargetUnavailable)
    }

    async fn reachable(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        principal: Uuid,
    ) -> CallResult<bool> {
        Ok(!self
            .endpoints
            .eligible_in(txn, scope, principal, 1)
            .await?
            .is_empty())
    }

    /// Enterprise evidence of a transfer step (no-op for a non-enterprise Call).
    pub(crate) async fn enterprise_transfer_step_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        destination: Option<Destination>,
        reason: &'static str,
    ) -> CallResult<()> {
        if context_of(txn, call.id).await?.is_none() {
            return Ok(());
        }
        record_step(
            txn,
            call,
            "TRANSFER",
            destination,
            reason,
            None,
            self.calls.now(),
        )
        .await
    }
}
