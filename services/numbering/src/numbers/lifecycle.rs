//! The centralized Number Resource transition policy (Part IV §D2).
//!
//! [`allowed`] is the edge set (mirrored by the database function
//! `number_lifecycle_allowed`; a test compares them). [`transition_in`] is the ONLY way
//! a resource's state changes: it checks the expected revision (optimistic concurrency),
//! the edge, the transition's prerequisites, records actor / reason / evidence /
//! timestamps, appends the lifecycle event and the outbox event — all in the caller's
//! transaction. The database refuses state changes made any other way.
//!
//! Uncertainty: entering UNKNOWN_PROVIDER_STATE records which external operation is
//! uncertain and from which state; leaving it requires a RECONCILIATION actor with
//! evidence. Nothing irreversible is repeated while a resource is uncertain.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};
use serde_json::json;
use uuid::Uuid;

use super::{
    resources::ResourceRow, stmt, Actor, ActorKind, DomainError, DomainResult, FailureCategory,
    InventorySource, LifecycleState, UncertainOperation,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

use LifecycleState as S;

/// The canonical edges.
pub fn allowed(from: LifecycleState, to: LifecycleState) -> bool {
    matches!(
        (from, to),
        (
            S::Discovered,
            S::Available | S::Failed | S::Released | S::UnknownProviderState
        ) | (
            S::Available,
            S::Reserving | S::Quarantined | S::Released | S::UnknownProviderState
        ) | (
            S::Reserving,
            S::Reserved | S::Available | S::Reassignable | S::Failed | S::UnknownProviderState
        ) | (
            S::Reserved,
            S::Provisioning | S::Available | S::Reassignable | S::UnknownProviderState
        ) | (
            S::Provisioning,
            S::Provisioned | S::Reserved | S::Failed | S::UnknownProviderState
        ) | (S::Provisioned, S::Active | S::Releasing)
            | (S::Active, S::Suspended | S::Releasing)
            | (S::Suspended, S::Active | S::Releasing)
            | (
                S::Releasing,
                S::Quarantined | S::Released | S::Failed | S::UnknownProviderState
            )
            | (S::Quarantined, S::Reassignable | S::Releasing)
            | (
                S::Reassignable,
                S::Reserving | S::Releasing | S::Quarantined
            )
            | (S::Released, S::Discovered)
            | (S::Failed, S::Discovered)
            | (
                S::UnknownProviderState,
                S::Available
                    | S::Reserved
                    | S::Provisioned
                    | S::Active
                    | S::Quarantined
                    | S::Reassignable
                    | S::Released
                    | S::Failed
            )
    )
}

/// States in which a resource can be allocated (reserved) normally.
pub fn allocatable(state: LifecycleState) -> bool {
    matches!(state, S::Available | S::Reassignable)
}

/// A requested lifecycle transition.
#[derive(Debug, Clone)]
pub struct TransitionRequest {
    pub to: LifecycleState,
    pub expected_revision: i64,
    pub actor: Actor,
    pub reason_code: String,
    pub evidence_ref: Option<Uuid>,
    pub failure_category: Option<FailureCategory>,
    pub suspension_reason: Option<String>,
    pub quarantine_until: Option<DateTime<Utc>>,
    pub uncertain_operation: Option<UncertainOperation>,
}

impl TransitionRequest {
    pub fn new(
        to: LifecycleState,
        expected_revision: i64,
        actor: Actor,
        reason_code: &str,
    ) -> Self {
        Self {
            to,
            expected_revision,
            actor,
            reason_code: reason_code.to_string(),
            evidence_ref: None,
            failure_category: None,
            suspension_reason: None,
            quarantine_until: None,
            uncertain_operation: None,
        }
    }

    pub fn with_evidence(mut self, evidence: Uuid) -> Self {
        self.evidence_ref = Some(evidence);
        self
    }

    pub fn with_failure(mut self, category: FailureCategory) -> Self {
        self.failure_category = Some(category);
        self
    }

    pub fn with_suspension(mut self, reason: &str) -> Self {
        self.suspension_reason = Some(reason.to_string());
        self
    }

    pub fn with_quarantine_until(mut self, until: DateTime<Utc>) -> Self {
        self.quarantine_until = Some(until);
        self
    }

    pub fn uncertain(mut self, operation: UncertainOperation) -> Self {
        self.uncertain_operation = Some(operation);
        self
    }
}

#[derive(Debug, FromQueryResult)]
struct Count {
    n: i64,
}

async fn count(
    txn: &DatabaseTransaction,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> DomainResult<i64> {
    Ok(Count::find_by_statement(stmt(sql, values))
        .one(txn)
        .await?
        .map(|c| c.n)
        .unwrap_or(0))
}

fn precondition(code: &'static str, message: &str) -> DomainError {
    DomainError::precondition(code, message)
}

/// The transition's prerequisites, given the locked row.
async fn check_prerequisites(
    txn: &DatabaseTransaction,
    row: &ResourceRow,
    req: &TransitionRequest,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    let from = row.lifecycle_state();
    let synthetic = row.inventory() == InventorySource::SyntheticTest;
    let id = row.id;
    super::types::reason_code(&req.reason_code)?;

    if from == S::UnknownProviderState
        && (req.actor.kind != ActorKind::Reconciliation || req.evidence_ref.is_none())
    {
        return Err(precondition(
            "RECONCILIATION_REQUIRED",
            "an uncertain resource is resolved only by reconciliation with evidence",
        ));
    }
    match req.to {
        S::UnknownProviderState if req.uncertain_operation.is_none() => {
            return Err(DomainError::Validation(
                "the uncertain operation is required".into(),
            ))
        }
        S::Failed if req.failure_category.is_none() => {
            return Err(DomainError::Validation(
                "a failure category is required".into(),
            ))
        }
        S::Suspended => {
            let reason = req.suspension_reason.as_deref().unwrap_or("");
            super::types::reason_code(reason).map_err(|_| {
                DomainError::Validation("a suspension reason code is required".into())
            })?;
        }
        S::Quarantined if req.quarantine_until.is_none_or(|u| u <= now) => {
            return Err(DomainError::Validation(
                "quarantine needs a future end".into(),
            ))
        }
        _ => {}
    }

    let fresh_observation = |availability: Option<&'static str>| async move {
        let sql = match availability {
            Some(_) => {
                "SELECT count(*) AS n FROM (SELECT availability, fresh_until FROM number_inventory_snapshots \
                   WHERE number_resource_id = $1 ORDER BY observed_at DESC, sequence DESC LIMIT 1) s \
                 WHERE s.fresh_until > $2 AND s.availability = $3"
            }
            None => {
                "SELECT count(*) AS n FROM (SELECT fresh_until FROM number_inventory_snapshots \
                   WHERE number_resource_id = $1 ORDER BY observed_at DESC, sequence DESC LIMIT 1) s \
                 WHERE s.fresh_until > $2 AND $3::text IS NULL"
            }
        };
        count(
            txn,
            sql,
            vec![
                id.into(),
                now.into(),
                availability.map(str::to_string).into(),
            ],
        )
        .await
    };

    match (from, req.to) {
        (S::Discovered, S::Available) => {
            if fresh_observation(Some("AVAILABLE")).await? == 0 {
                return Err(precondition(
                    "INVENTORY_NOT_FRESH",
                    "a fresh observation offering the number is required",
                ));
            }
        }
        (S::Released | S::Failed, S::Discovered) => {
            if fresh_observation(None).await? == 0 {
                return Err(precondition(
                    "INVENTORY_NOT_FRESH",
                    "rediscovery needs a fresh observation",
                ));
            }
        }
        (S::Reserved, S::Available | S::Reassignable)
            if !synthetic && req.evidence_ref.is_none() =>
        {
            return Err(precondition(
                "RELEASE_EVIDENCE_REQUIRED",
                "a held number returns to inventory only with provider release evidence",
            ));
        }
        (_, S::Reserving) => {
            let current = count(
                txn,
                "SELECT count(*) AS n FROM number_reservations WHERE number_resource_id = $1 AND status = 'PENDING'",
                vec![id.into()],
            )
            .await?;
            if current == 0 {
                return Err(precondition(
                    "RESERVATION_REQUIRED",
                    "reserving needs a pending reservation",
                ));
            }
        }
        (S::Reserving, S::Reserved) => {
            let held = count(
                txn,
                "SELECT count(*) AS n FROM number_reservations WHERE number_resource_id = $1 AND status = 'HELD'",
                vec![id.into()],
            )
            .await?;
            if held == 0 {
                return Err(precondition(
                    "HOLD_REQUIRED",
                    "RESERVED needs a confirmed provider hold",
                ));
            }
        }
        (_, S::Provisioning) => {
            let assignments = count(
                txn,
                "SELECT count(*) AS n FROM dcn_assignments WHERE number_resource_id = $1 \
                   AND status IN ('RESERVED', 'ACTIVATING')",
                vec![id.into()],
            )
            .await?;
            if assignments == 0 {
                return Err(precondition(
                    "ASSIGNMENT_REQUIRED",
                    "provisioning needs an assignment holding the reservation",
                ));
            }
        }
        (_, S::Provisioned) if from != S::UnknownProviderState => {
            let bindings = count(
                txn,
                "SELECT count(*) AS n FROM number_provider_bindings WHERE number_resource_id = $1 AND status = 'ACTIVE'",
                vec![id.into()],
            )
            .await?;
            if bindings == 0 {
                return Err(precondition(
                    "BINDING_REQUIRED",
                    "PROVISIONED needs an active provider binding",
                ));
            }
        }
        (S::Provisioned, S::Active) => {
            let uncovered = count(
                txn,
                "SELECT count(*) AS n FROM dcn_assignments a, unnest(a.capability_snapshot) AS cap \
                  WHERE a.number_resource_id = $1 AND a.status = 'ACTIVATING' \
                    AND cap IN ('VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH') \
                    AND NOT EXISTS (SELECT 1 FROM number_provider_bindings b WHERE b.number_resource_id = $1 \
                                     AND b.status = 'ACTIVE' AND cap = ANY (b.capabilities))",
                vec![id.into()],
            )
            .await?;
            let activating = count(
                txn,
                "SELECT count(*) AS n FROM dcn_assignments WHERE number_resource_id = $1 AND status = 'ACTIVATING'",
                vec![id.into()],
            )
            .await?;
            if activating == 0 {
                return Err(precondition(
                    "ASSIGNMENT_REQUIRED",
                    "a number becomes ACTIVE only for an ACTIVATING assignment",
                ));
            }
            if uncovered > 0 {
                return Err(precondition(
                    "BINDING_REQUIRED",
                    "every routable capability of the assignment needs an active provider binding",
                ));
            }
        }
        (_, S::Releasing) => {
            let live = count(
                txn,
                "SELECT count(*) AS n FROM dcn_assignments WHERE number_resource_id = $1 \
                   AND status NOT IN ('RELEASE_REQUESTED', 'ROUTING_REMOVED', 'QUARANTINED', 'CLOSED')",
                vec![id.into()],
            )
            .await?;
            if live > 0 {
                return Err(precondition(
                    "ASSIGNMENT_ACTIVE",
                    "release the assignment (RELEASE_REQUESTED) before releasing the number",
                ));
            }
        }
        (S::Quarantined, S::Reassignable) => {
            if row.quarantine_until.is_some_and(|u| u > now) {
                return Err(precondition(
                    "QUARANTINE_ACTIVE",
                    "the quarantine period has not ended",
                ));
            }
            let open = count(
                txn,
                "SELECT count(*) AS n FROM dcn_assignments WHERE number_resource_id = $1 \
                   AND status NOT IN ('QUARANTINED', 'CLOSED')",
                vec![id.into()],
            )
            .await?;
            if open > 0 {
                return Err(precondition(
                    "ASSIGNMENT_ACTIVE",
                    "the previous assignment is still open",
                ));
            }
        }
        _ => {}
    }
    if req.to == S::Released {
        let active = count(
            txn,
            "SELECT count(*) AS n FROM number_provider_bindings WHERE number_resource_id = $1 AND status = 'ACTIVE'",
            vec![id.into()],
        )
        .await?;
        if active > 0 {
            return Err(precondition(
                "BINDINGS_ACTIVE",
                "end the provider bindings before the number is released",
            ));
        }
        if from == S::Releasing && !synthetic && req.evidence_ref.is_none() {
            return Err(precondition(
                "RELEASE_EVIDENCE_REQUIRED",
                "RELEASED needs provider release evidence",
            ));
        }
    }
    Ok(())
}

/// Applies a lifecycle transition inside `txn` (the only state-changing path).
pub async fn transition_in(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    resource_id: Uuid,
    req: TransitionRequest,
    now: DateTime<Utc>,
) -> DomainResult<ResourceRow> {
    let row = super::resources::lock(txn, resource_id).await?;
    if row.revision != req.expected_revision {
        return Err(DomainError::VersionConflict("number resource"));
    }
    let from = row.lifecycle_state();
    if !allowed(from, req.to) {
        return Err(DomainError::InvalidTransition {
            entity: "number resource",
            from: from.as_str().into(),
            to: req.to.as_str().into(),
        });
    }
    check_prerequisites(txn, &row, &req, now).await?;

    let to = req.to;
    let mode = if from == S::UnknownProviderState || req.actor.kind == ActorKind::Reconciliation {
        "reconciliation"
    } else {
        "transition"
    };
    // Timestamps and state-specific fields (the CHECK constraints keep them consistent).
    let stamp = |state: LifecycleState| (to == state).then_some(now);
    txn.execute(stmt(
        "SELECT set_config('numbering.lifecycle', $1, true)",
        vec![mode.into()],
    ))
    .await?;
    let updated = ResourceRow::find_by_statement(stmt(
        "UPDATE number_resources SET \
            state = $3, revision = revision + 1, state_changed_at = $4, updated_at = $4, \
            available_at = COALESCE($5, available_at), reserved_at = COALESCE($6, reserved_at), \
            provisioned_at = COALESCE($7, provisioned_at), activated_at = COALESCE($8, activated_at), \
            suspended_at = COALESCE($9, suspended_at), released_at = COALESCE($10, released_at), \
            failed_at = COALESCE($11, failed_at), \
            suspension_reason = $12, failure_category = $13, \
            quarantine_until = CASE WHEN $3 = 'QUARANTINED' THEN $14 ELSE quarantine_until END, \
            uncertain_operation = $15, uncertain_from_state = $16, uncertain_since = $17 \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            resource_id.into(),
            req.expected_revision.into(),
            to.as_str().into(),
            now.into(),
            stamp(S::Available).into(),
            stamp(S::Reserved).into(),
            stamp(S::Provisioned).into(),
            stamp(S::Active).into(),
            stamp(S::Suspended).into(),
            stamp(S::Released).into(),
            stamp(S::Failed).into(),
            (to == S::Suspended).then(|| req.suspension_reason.clone()).flatten().into(),
            (to == S::Failed).then(|| req.failure_category.map(|c| c.as_str().to_string())).flatten().into(),
            req.quarantine_until.into(),
            (to == S::UnknownProviderState)
                .then(|| req.uncertain_operation.map(|o| o.as_str().to_string()))
                .flatten()
                .into(),
            (to == S::UnknownProviderState).then(|| from.as_str().to_string()).into(),
            (to == S::UnknownProviderState).then_some(now).into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("number resource"))?;
    txn.execute(stmt(
        "SELECT set_config('numbering.lifecycle', 'off', true)",
        vec![],
    ))
    .await?;

    record_event(txn, ctx, &updated, Some(from), &req, now).await?;
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.resource.lifecycle-changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: resource_id,
            scope: None,
            payload: json!({
                "number_resource_id": resource_id,
                "from_state": from.as_str(),
                "to_state": to.as_str(),
                "revision": updated.revision,
                "reason_code": req.reason_code,
                "actor_kind": req.actor.kind.as_str(),
                "failure_category": req.failure_category.map(|c| c.as_str()),
                "uncertain_operation": req.uncertain_operation.map(|o| o.as_str()),
                "inventory_source": updated.inventory_source,
            }),
        },
        ctx,
        now,
    )
    .await?;
    crate::metrics::inc(
        "numbering_lifecycle_transitions_total",
        &[("to", to.as_str())],
    );
    Ok(updated)
}

/// Appends the lifecycle audit row of a change to `row` (a transition, or the creation
/// when `from` is `None`).
pub(crate) async fn record_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &ResourceRow,
    from: Option<LifecycleState>,
    req: &TransitionRequest,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    txn.execute(stmt(
        "INSERT INTO number_lifecycle_events (id, number_resource_id, from_state, to_state, revision, actor_kind, \
           actor_ref, reason_code, failure_category, evidence_ref, correlation_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            from.map(|f| f.as_str().to_string()).into(),
            row.state.clone().into(),
            row.revision.into(),
            req.actor.kind.as_str().into(),
            req.actor.reference.clone().into(),
            req.reason_code.clone().into(),
            req.failure_category.map(|c| c.as_str().to_string()).into(),
            req.evidence_ref.into(),
            ctx.correlation_id.as_str().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_have_only_the_documented_reentries() {
        for to in LifecycleState::ALL {
            assert_eq!(allowed(S::Released, *to), *to == S::Discovered, "{to}");
            assert_eq!(allowed(S::Failed, *to), *to == S::Discovered, "{to}");
        }
    }

    #[test]
    fn no_self_loops_and_unknown_is_reachable_only_from_provider_affecting_states() {
        for s in LifecycleState::ALL {
            assert!(!allowed(*s, *s), "{s}");
        }
        let into_unknown: Vec<_> = LifecycleState::ALL
            .iter()
            .filter(|s| allowed(**s, S::UnknownProviderState))
            .collect();
        assert_eq!(
            into_unknown,
            vec![
                &S::Discovered,
                &S::Available,
                &S::Reserving,
                &S::Reserved,
                &S::Provisioning,
                &S::Releasing
            ]
        );
        // Quarantined / uncertain resources are never allocatable.
        assert!(!allocatable(S::Quarantined) && !allocatable(S::UnknownProviderState));
        assert!(
            !allowed(S::Quarantined, S::Reserving)
                && !allowed(S::UnknownProviderState, S::Reserving)
        );
    }
}
