//! Enterprise destination resolution (Part F §31, §58–§59, §83–§88): a typed destination
//! becomes ONE routing action. Every followed configuration node (extension, schedule)
//! and every principal reached is added to the context's visited set; following a node or
//! a principal's forwarding costs one hop of the policy's budget. So `A -> B -> A` (and
//! deeper loops, through extensions, schedules or forwarding) can never route forever:
//! they end in `NO_ROUTE / LOOP_BLOCKED` or `NO_ROUTE / HOP_BUDGET_EXHAUSTED`.
//!
//! * SCHEDULE: business hours evaluated at the routing instant (IANA zone) select the
//!   OPEN or the CLOSED route.
//! * A principal with an ACTIVE `ALWAYS` forwarding rule is forwarded before ringing (if
//!   the policy permits forwarding); BUSY / NO_ANSWER / UNAVAILABLE rules are followed by
//!   [`resolve_forward`] once ringing the principal failed that way.
//! * PUBLIC_CONTACT becomes a public forward that still has to pass every Part E gate
//!   (policy, flags, Privacy Relay authority, country, spend, Telephony route) before any
//!   public leg exists — nothing here bypasses them.

use chrono::{DateTime, Utc};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

use super::{exec, record_step_of, ContextRow};
use crate::calls::app::incidents::{
    Incident, IncidentCategory, IncidentResource, IncidentSeverity,
};
use crate::{
    calls::{command::CallScope, errors::CallResult, service::CallService},
    domain::PrincipalId,
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions,
        forwarding::{self, ForwardingCondition},
        hours::{self, HoursStatus},
        ivr,
        policy::EffectivePolicy,
        queues, ring_groups, voicemail,
    },
    platform::principals::{PrincipalReference, PrincipalResolver, ResolvedPrincipal},
};

/// The database bound of the visited set.
const MAX_VISITED: usize = 32;

/// What the router decided.
#[derive(Debug, Clone)]
pub(crate) enum RouteAction {
    /// Ring one principal's eligible endpoints.
    RingPrincipal {
        principal: ResolvedPrincipal,
        destination: Destination,
    },
    /// Ring a ring group (simultaneous / ordered).
    RingGroup {
        group: ring_groups::RingGroupRow,
        destination: Destination,
    },
    /// Enter an IVR flow (its ACTIVE version).
    EnterIvr {
        flow: ivr::FlowRow,
        destination: Destination,
    },
    /// Wait in a call queue (FIFO, offered to available agents).
    EnterQueue {
        queue: queues::QueueRow,
        destination: Destination,
    },
    /// Forward to a registered, ACTIVE protected public contact (every Part E gate still
    /// applies before a public leg exists).
    ForwardPublic { destination: Destination },
    /// Take a voicemail message into an ACTIVE mailbox (same Call).
    EnterVoicemail {
        mailbox: voicemail::MailboxRow,
        destination: Destination,
    },
    /// Nothing can take the Call.
    NoRoute {
        reason: &'static str,
        destination: Option<Destination>,
    },
}

/// The visited set / hop count being built during one resolution.
struct Walk<'a> {
    calls: &'a CallService,
    txn: &'a DatabaseTransaction,
    scope: CallScope,
    context: &'a ContextRow,
    visited: Vec<Uuid>,
    hops: i16,
    max_hops: i16,
    at: DateTime<Utc>,
}

impl<'a> Walk<'a> {
    fn new(
        calls: &'a CallService,
        txn: &'a DatabaseTransaction,
        scope: CallScope,
        policy: &EffectivePolicy,
        context: &'a ContextRow,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            calls,
            txn,
            scope,
            context,
            visited: context.visited.clone(),
            hops: context.hop_count,
            max_hops: (context.max_hops as u8).min(policy.rules.max_routing_hops) as i16,
            at,
        }
    }

    /// Counts one hop into `node`; `Some(blocked)` when that is a loop or over budget.
    async fn enter(&mut self, node: Destination) -> CallResult<Option<RouteAction>> {
        if self.visited.contains(&node.id) {
            return self.blocked("LOOP_BLOCKED", node).await.map(Some);
        }
        if self.hops + 1 > self.max_hops || self.visited.len() >= MAX_VISITED {
            return self.blocked("HOP_BUDGET_EXHAUSTED", node).await.map(Some);
        }
        self.hops += 1;
        self.visited.push(node.id);
        Ok(None)
    }

    async fn persist(&self) -> CallResult<()> {
        if self.hops == self.context.hop_count && self.visited == self.context.visited {
            return Ok(());
        }
        exec(
            self.txn,
            "UPDATE enterprise_call_contexts SET hop_count = $2, visited = $3, version = version + 1, \
             updated_at = GREATEST(updated_at, $4) WHERE call_id = $1",
            vec![
                self.context.call_id.into(),
                self.hops.into(),
                self.visited.clone().into(),
                self.at.into(),
            ],
        )
        .await?;
        Ok(())
    }

    async fn done(&self, action: RouteAction) -> CallResult<RouteAction> {
        self.persist().await?;
        Ok(action)
    }

    async fn disabled(&self, destination: Destination) -> CallResult<RouteAction> {
        self.done(RouteAction::NoRoute {
            reason: "DESTINATION_DISABLED",
            destination: Some(destination),
        })
        .await
    }

    async fn blocked(&self, reason: &'static str, at: Destination) -> CallResult<RouteAction> {
        self.persist().await?;
        crate::metrics::inc(
            "calling_enterprise_loop_blocks_total",
            &[("reason", reason)],
        );
        tracing::warn!(call_id = %self.context.call_id, reason, "enterprise routing loop blocked");
        // A loop that keeps being blocked is configuration an operator must fix: one open
        // incident per destination, its occurrences counting the repeats.
        crate::calls::app::incidents::raise(
            self.txn,
            &Incident::new(
                IncidentCategory::ForwardingLoopRepeated,
                IncidentSeverity::Medium,
                self.scope,
                IncidentResource::Destination,
                at.id,
            )
            .call(self.context.call_id)
            .evidence(serde_json::json!({ "reason": reason, "destinationType": at.kind.as_str() })),
            self.at,
        )
        .await;
        Ok(RouteAction::NoRoute {
            reason,
            destination: Some(at),
        })
    }

    async fn step(
        &self,
        decision: &'static str,
        destination: Destination,
        reason: &'static str,
    ) -> CallResult<()> {
        record_step_of(
            self.calls,
            self.txn,
            self.context.call_id,
            self.scope,
            decision,
            Some(destination),
            reason,
            None,
            self.at,
        )
        .await
    }
}

/// Resolves `destination` for the Call of `context`, inside the caller's transaction
/// (restart- and concurrency-safe: the Call row is locked).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve(
    calls: &CallService,
    txn: &DatabaseTransaction,
    principals: &PrincipalResolver,
    scope: CallScope,
    policy: &EffectivePolicy,
    context: &ContextRow,
    destination: Destination,
    at: DateTime<Utc>,
) -> CallResult<RouteAction> {
    let mut walk = Walk::new(calls, txn, scope, policy, context, at);
    walk_to(&mut walk, principals, policy, destination).await
}

/// After ringing `principal` failed with `condition` (BUSY / NO_ANSWER / UNAVAILABLE): the
/// principal's forwarding rule for it, if the policy permits forwarding. `None` = no rule
/// (the ordinary outcome applies).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_forward(
    calls: &CallService,
    txn: &DatabaseTransaction,
    principals: &PrincipalResolver,
    scope: CallScope,
    policy: &EffectivePolicy,
    context: &ContextRow,
    principal: Uuid,
    condition: ForwardingCondition,
    at: DateTime<Utc>,
) -> CallResult<Option<RouteAction>> {
    if !policy.rules.forwarding_allowed {
        return Ok(None);
    }
    let Some(rule) = forwarding::active_rule(txn, scope, principal, condition)
        .await
        .map_err(super::enterprise_error)?
    else {
        return Ok(None);
    };
    let Some(target) = rule.destination() else {
        return Ok(None);
    };
    let mut walk = Walk::new(calls, txn, scope, policy, context, at);
    // The principal was rung (it is in the visited set); forwarding from it is one hop.
    if walk.hops + 1 > walk.max_hops {
        let from = Destination::new(DestinationType::Principal, principal);
        return walk.blocked("HOP_BUDGET_EXHAUSTED", from).await.map(Some);
    }
    walk.hops += 1;
    walk.step("FORWARD", target, condition_reason(condition))
        .await?;
    crate::metrics::inc(
        "calling_enterprise_forwards_total",
        &[
            ("condition", condition.as_str()),
            ("target_type", target.kind.as_str()),
        ],
    );
    walk_to(&mut walk, principals, policy, target)
        .await
        .map(Some)
}

fn condition_reason(condition: ForwardingCondition) -> &'static str {
    match condition {
        ForwardingCondition::Always => "FORWARD_ALWAYS",
        ForwardingCondition::Busy => "FORWARD_BUSY",
        ForwardingCondition::NoAnswer => "FORWARD_NO_ANSWER",
        ForwardingCondition::Unavailable => "FORWARD_UNAVAILABLE",
    }
}

async fn walk_to(
    walk: &mut Walk<'_>,
    principals: &PrincipalResolver,
    policy: &EffectivePolicy,
    destination: Destination,
) -> CallResult<RouteAction> {
    let scope = walk.scope;
    let mut current = destination;
    loop {
        if current.kind.is_principal() {
            // A principal reached twice is a loop (A forwards to B, B back to A).
            if walk.visited.contains(&current.id) {
                return walk.blocked("LOOP_BLOCKED", current).await;
            }
            if walk.visited.len() >= MAX_VISITED {
                return walk.blocked("HOP_BUDGET_EXHAUSTED", current).await;
            }
            walk.visited.push(current.id);
            if policy.rules.forwarding_allowed {
                if let Some(rule) = forwarding::active_rule(
                    walk.txn,
                    scope,
                    current.id,
                    ForwardingCondition::Always,
                )
                .await
                .map_err(super::enterprise_error)?
                {
                    if let Some(target) = rule.destination() {
                        if walk.hops + 1 > walk.max_hops {
                            return walk.blocked("HOP_BUDGET_EXHAUSTED", current).await;
                        }
                        walk.hops += 1;
                        walk.step("FORWARD", target, "FORWARD_ALWAYS").await?;
                        crate::metrics::inc(
                            "calling_enterprise_forwards_total",
                            &[
                                ("condition", "ALWAYS"),
                                ("target_type", target.kind.as_str()),
                            ],
                        );
                        current = target;
                        continue;
                    }
                }
            }
            let action = match principals
                .resolve_in(
                    scope.tenant_id(),
                    scope.application_id(),
                    scope.environment_id(),
                    &PrincipalReference::Id(PrincipalId::from_uuid(current.id)),
                )
                .await
            {
                Ok(principal)
                    if current.kind == DestinationType::Principal
                        || principal.kind() == crate::domain::PrincipalKind::EnterpriseMember =>
                {
                    RouteAction::RingPrincipal {
                        principal,
                        destination: current,
                    }
                }
                _ => RouteAction::NoRoute {
                    reason: "TARGET_UNAVAILABLE",
                    destination: Some(current),
                },
            };
            return walk.done(action).await;
        }
        if let Some(blocked) = walk.enter(current).await? {
            return Ok(blocked);
        }
        match current.kind {
            DestinationType::Extension => {
                match extensions::extension_row(walk.txn, scope, current.id, false).await {
                    Ok(row) if row.status == "ACTIVE" => {
                        current = row.destination().map_err(super::enterprise_error)?;
                    }
                    _ => return walk.disabled(current).await,
                }
            }
            DestinationType::Schedule => {
                let row = match hours::hours_row(walk.txn, scope, current.id, false).await {
                    Ok(row) if row.status == "ACTIVE" => row,
                    _ => return walk.disabled(current).await,
                };
                let schedule = match hours::schedule_of(walk.txn, &row).await {
                    Ok(s) => s,
                    Err(_) => return walk.disabled(current).await,
                };
                let evaluation = schedule.evaluate(walk.at);
                let (next, reason) = match evaluation.status {
                    HoursStatus::Open => (row.open(), "OPEN"),
                    HoursStatus::Closed => (row.closed(), "CLOSED"),
                };
                walk.step("SCHEDULE", current, reason).await?;
                crate::metrics::inc(
                    "calling_enterprise_schedule_evaluations_total",
                    &[("status", evaluation.status.as_str())],
                );
                match next {
                    Some(next) => current = next,
                    None => return walk.disabled(current).await,
                }
            }
            DestinationType::RingGroup => {
                return match ring_groups::ring_group_row(walk.txn, scope, current.id, false).await {
                    Ok(group) if group.status == "ACTIVE" => {
                        walk.done(RouteAction::RingGroup {
                            group,
                            destination: current,
                        })
                        .await
                    }
                    _ => walk.disabled(current).await,
                };
            }
            DestinationType::Ivr => {
                return match ivr::flow_row(walk.txn, scope, current.id, false).await {
                    Ok(flow) if flow.status == "ACTIVE" && flow.active_version_id.is_some() => {
                        walk.done(RouteAction::EnterIvr {
                            flow,
                            destination: current,
                        })
                        .await
                    }
                    _ => walk.disabled(current).await,
                };
            }
            DestinationType::Queue => {
                return match queues::queue_row(walk.txn, scope, current.id, false).await {
                    Ok(queue) if queue.status == "ACTIVE" => {
                        walk.done(RouteAction::EnterQueue {
                            queue,
                            destination: current,
                        })
                        .await
                    }
                    _ => walk.disabled(current).await,
                };
            }
            DestinationType::Voicemail => {
                if !policy.rules.voicemail_allowed {
                    return walk
                        .done(RouteAction::NoRoute {
                            reason: "VOICEMAIL_NOT_PERMITTED",
                            destination: Some(current),
                        })
                        .await;
                }
                return match voicemail::mailbox_row(walk.txn, scope, current.id, false).await {
                    Ok(mailbox) if mailbox.status == "ACTIVE" => {
                        walk.done(RouteAction::EnterVoicemail {
                            mailbox,
                            destination: current,
                        })
                        .await
                    }
                    _ => walk.disabled(current).await,
                };
            }
            DestinationType::PublicContact => {
                if !(policy.rules.forwarding_allowed && policy.rules.public_forwarding_allowed) {
                    return walk
                        .done(RouteAction::NoRoute {
                            reason: "PUBLIC_FORWARDING_NOT_PERMITTED",
                            destination: Some(current),
                        })
                        .await;
                }
                return match forwarding::public_contact_row(walk.txn, scope, current.id, false)
                    .await
                {
                    Ok(contact) if contact.status == "ACTIVE" => {
                        walk.done(RouteAction::ForwardPublic {
                            destination: current,
                        })
                        .await
                    }
                    _ => walk.disabled(current).await,
                };
            }
            _ => {
                return walk
                    .done(RouteAction::NoRoute {
                        reason: "DESTINATION_UNSUPPORTED",
                        destination: Some(current),
                    })
                    .await;
            }
        }
    }
}
