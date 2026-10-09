//! Enterprise destination resolution (Part F §31, §59, §86): a typed destination becomes
//! ONE routing action. Extension links are followed with a visited set and the policy's
//! hop budget, so `A -> B -> A` (and deeper loops) can never route forever: they end in
//! `NO_ROUTE / LOOP_BLOCKED` or `NO_ROUTE / HOP_BUDGET_EXHAUSTED`.

use chrono::{DateTime, Utc};
use sea_orm::DatabaseTransaction;

use super::{exec, ContextRow};
use crate::{
    calls::{command::CallScope, errors::CallResult},
    domain::PrincipalId,
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions,
        policy::EffectivePolicy,
        ring_groups,
    },
    platform::principals::{PrincipalReference, PrincipalResolver, ResolvedPrincipal},
};

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
    /// Nothing can take the Call.
    NoRoute {
        reason: &'static str,
        destination: Option<Destination>,
    },
}

/// Resolves `destination` for the Call of `context`. Every followed extension link is
/// added to the context's visited set and counted against the hop budget, inside the
/// caller's transaction (restart- and concurrency-safe: the Call row is locked).
pub(crate) async fn resolve(
    txn: &DatabaseTransaction,
    principals: &PrincipalResolver,
    scope: CallScope,
    policy: &EffectivePolicy,
    context: &ContextRow,
    destination: Destination,
    at: DateTime<Utc>,
) -> CallResult<RouteAction> {
    let max_hops = (context.max_hops as u8).min(policy.rules.max_routing_hops) as i16;
    let mut visited = context.visited.clone();
    let mut hops = context.hop_count;
    let mut current = destination;
    loop {
        if current.kind.is_principal() {
            persist(txn, context, &visited, hops, at).await?;
            return Ok(
                match principals
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
                            || principal.kind()
                                == crate::domain::PrincipalKind::EnterpriseMember =>
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
                },
            );
        }
        // Every followed configuration node counts against the hop budget and is
        // remembered: reaching one twice is a loop.
        if visited.contains(&current.id) {
            return blocked(txn, context, &visited, hops, "LOOP_BLOCKED", current, at).await;
        }
        if hops + 1 > max_hops {
            return blocked(
                txn,
                context,
                &visited,
                hops,
                "HOP_BUDGET_EXHAUSTED",
                current,
                at,
            )
            .await;
        }
        hops += 1;
        visited.push(current.id);
        match current.kind {
            DestinationType::Extension => {
                match extensions::extension_row(txn, scope, current.id, false).await {
                    Ok(row) if row.status == "ACTIVE" => {
                        current = row.destination().map_err(super::enterprise_error)?;
                    }
                    _ => return disabled(txn, context, &visited, hops, current, at).await,
                }
            }
            DestinationType::RingGroup => {
                return match ring_groups::ring_group_row(txn, scope, current.id, false).await {
                    Ok(group) if group.status == "ACTIVE" => {
                        persist(txn, context, &visited, hops, at).await?;
                        Ok(RouteAction::RingGroup {
                            group,
                            destination: current,
                        })
                    }
                    _ => disabled(txn, context, &visited, hops, current, at).await,
                };
            }
            _ => {
                persist(txn, context, &visited, hops, at).await?;
                return Ok(RouteAction::NoRoute {
                    reason: "DESTINATION_UNSUPPORTED",
                    destination: Some(current),
                });
            }
        }
    }
}

async fn disabled(
    txn: &DatabaseTransaction,
    context: &ContextRow,
    visited: &[uuid::Uuid],
    hops: i16,
    destination: Destination,
    at: DateTime<Utc>,
) -> CallResult<RouteAction> {
    persist(txn, context, visited, hops, at).await?;
    Ok(RouteAction::NoRoute {
        reason: "DESTINATION_DISABLED",
        destination: Some(destination),
    })
}

async fn persist(
    txn: &DatabaseTransaction,
    context: &ContextRow,
    visited: &[uuid::Uuid],
    hops: i16,
    at: DateTime<Utc>,
) -> CallResult<()> {
    if hops == context.hop_count && visited == context.visited.as_slice() {
        return Ok(());
    }
    exec(
        txn,
        "UPDATE enterprise_call_contexts SET hop_count = $2, visited = $3, version = version + 1, \
         updated_at = GREATEST(updated_at, $4) WHERE call_id = $1",
        vec![
            context.call_id.into(),
            hops.into(),
            visited.to_vec().into(),
            at.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn blocked(
    txn: &DatabaseTransaction,
    context: &ContextRow,
    visited: &[uuid::Uuid],
    hops: i16,
    reason: &'static str,
    at_destination: Destination,
    at: DateTime<Utc>,
) -> CallResult<RouteAction> {
    persist(txn, context, visited, hops, at).await?;
    crate::metrics::inc(
        "calling_enterprise_loop_blocks_total",
        &[("reason", reason)],
    );
    tracing::warn!(call_id = %context.call_id, reason, "enterprise routing loop blocked");
    Ok(RouteAction::NoRoute {
        reason,
        destination: Some(at_destination),
    })
}
