//! Workstream Q — release, quarantine and the reassignment gate.
//!
//! A release is an auditable `number_release_requests` record, never a delete. Its steps
//! run in the mandatory order, each idempotent and resumable by the worker:
//!
//! 1. new grants and SMS submissions stop: the assignment leaves ACTIVE / SUSPENDED
//!    (RELEASE_REQUESTED) and every outstanding grant is revoked;
//! 2. + 3. the previous owner's voice and SMS routing is removed: the assignment is
//!    ROUTING_REMOVED, the number RELEASING, its CONFIGURED routes retired (no consumer
//!    readiness survives for a later owner);
//! 4. the previous owner's organizational resources are detached (pool memberships);
//! 5. the routing-removal evidence is confirmed (no live grant, no ready route, the
//!    resolver refuses) — otherwise the release fails closed with an incident;
//! 6. `PROVIDER_RELINQUISH`: the provider RELEASE through the durable executor
//!    (`POOL_RETURN` keeps the number CF Meet-controlled; `PORTED_OUT` is completed by the
//!    authoritative port completion);
//! 7. an unknown provider outcome waits for reconciliation (never blindly repeated);
//! 8. quarantine with its policy and permanent evidence.
//!
//! Quarantine completion is a worker (`FOR UPDATE SKIP LOCKED`, so two workers never
//! complete the same quarantine): it waits for the cooldown, a required review, every
//! provider command to settle and — for a POOL_RETURN number — the provider's own
//! read-back that CF Meet still controls it. A relinquished number is finally RELEASED,
//! never reassignable, whatever the provider's inventory later says. Contradicting
//! provider state is an incident and the quarantine is held.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{command_outcome, count, CommandOutcome, IdRow, Stewardship};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
    numbers::{
        assignments::AssignmentRow,
        lifecycle::{transition_in, TransitionRequest},
        resources::{lock, ResourceRow},
        stmt, Actor, ActorKind, AssignmentStatus, DomainError, DomainResult, InventorySource,
        LifecycleState, NumberType, OwnerScope, RiskTier,
    },
    providers::{incidents, ProviderNumberStatus, ProviderOutcome},
};

/// What happens to the number after the owner lets it go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReleasePolicy {
    /// Returned to the platform / organization pool: CF Meet keeps it at the provider.
    PoolReturn,
    /// Given back to the provider: no longer CF Meet's after the quarantine.
    ProviderRelinquish,
    /// Ported to another carrier (completed by the port-out, Workstream U).
    PortedOut,
}

impl ReleasePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleasePolicy::PoolReturn => "POOL_RETURN",
            ReleasePolicy::ProviderRelinquish => "PROVIDER_RELINQUISH",
            ReleasePolicy::PortedOut => "PORTED_OUT",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "POOL_RETURN" => Some(ReleasePolicy::PoolReturn),
            "PROVIDER_RELINQUISH" => Some(ReleasePolicy::ProviderRelinquish),
            "PORTED_OUT" => Some(ReleasePolicy::PortedOut),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub owner_kind: String,
    pub owner_ref: Uuid,
    pub ownership_class: String,
    pub reason_code: String,
    pub policy: String,
    pub requester_kind: String,
    pub requester_ref: String,
    pub requested_at: DateTime<Utc>,
    pub effective_at: DateTime<Utc>,
    pub idempotency_key: String,
    pub status: String,
    pub revision: i64,
    pub grants_stopped_at: Option<DateTime<Utc>>,
    pub routing_removed_at: Option<DateTime<Utc>>,
    pub routing_evidence: Option<Value>,
    pub detached_at: Option<DateTime<Utc>>,
    pub provider_command_id: Option<Uuid>,
    pub provider_released_at: Option<DateTime<Utc>>,
    pub quarantine_id: Option<Uuid>,
    pub failure_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub correlation_id: String,
}

impl ReleaseRow {
    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    pub fn policy(&self) -> ReleasePolicy {
        ReleasePolicy::parse(&self.policy).expect("CHECK-constrained policy")
    }

    fn event_scope(&self) -> EventScope {
        EventScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub release_request_id: Option<Uuid>,
    pub previous_assignment_id: Option<Uuid>,
    pub disposition: String,
    pub policy_ref: String,
    pub cooldown_days: i32,
    pub started_at: DateTime<Utc>,
    pub quarantine_until: DateTime<Utc>,
    pub status: String,
    pub revision: i64,
    pub review_required: bool,
    pub review_decision: Option<String>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub extensions: i32,
    pub provider_confirmed_at: Option<DateTime<Utc>>,
    pub provider_evidence: Option<Value>,
    pub completed_at: Option<DateTime<Utc>>,
    pub completion_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A release request.
#[derive(Debug, Clone)]
pub struct NewRelease {
    pub assignment_id: Uuid,
    pub reason_code: String,
    pub policy: ReleasePolicy,
    /// Default: now. A future date schedules the release (the worker runs it).
    pub effective_at: Option<DateTime<Utc>>,
}

/// An operator's quarantine review decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReview {
    Approve,
    Extend { days: i64 },
    Hold,
}

fn release_columns() -> &'static str {
    "SELECT * FROM number_release_requests"
}

pub(crate) async fn lock_release(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<ReleaseRow> {
    ReleaseRow::find_by_statement(stmt(
        &format!("{} WHERE id = $1 FOR UPDATE", release_columns()),
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("release request"))
}

#[derive(Default)]
struct Step {
    grants_stopped_at: Option<DateTime<Utc>>,
    routing_removed_at: Option<DateTime<Utc>>,
    routing_evidence: Option<Value>,
    detached_at: Option<DateTime<Utc>>,
    provider_command_id: Option<Uuid>,
    provider_released_at: Option<DateTime<Utc>>,
    quarantine_id: Option<Uuid>,
    failure_reason: Option<String>,
}

async fn set_release(
    txn: &DatabaseTransaction,
    row: &ReleaseRow,
    status: &str,
    s: Step,
    now: DateTime<Utc>,
) -> DomainResult<ReleaseRow> {
    ReleaseRow::find_by_statement(stmt(
        "UPDATE number_release_requests SET status = $3, revision = revision + 1, updated_at = $4, \
           grants_stopped_at = COALESCE(grants_stopped_at, $5), routing_removed_at = COALESCE(routing_removed_at, $6), \
           routing_evidence = COALESCE(routing_evidence, $7), detached_at = COALESCE(detached_at, $8), \
           provider_command_id = COALESCE(provider_command_id, $9), \
           provider_released_at = COALESCE(provider_released_at, $10), quarantine_id = COALESCE(quarantine_id, $11), \
           failure_reason = COALESCE($12, failure_reason) \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            row.id.into(),
            row.revision.into(),
            status.into(),
            now.into(),
            s.grants_stopped_at.into(),
            s.routing_removed_at.into(),
            s.routing_evidence.into(),
            s.detached_at.into(),
            s.provider_command_id.into(),
            s.provider_released_at.into(),
            s.quarantine_id.into(),
            s.failure_reason.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("release request"))
}

async fn release_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &ReleaseRow,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.release.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: row.number_resource_id,
            scope: Some(row.event_scope()),
            payload: json!({
                "number_resource_id": row.number_resource_id,
                "assignment_id": row.assignment_id,
                "release_request_id": row.id,
                "policy": row.policy,
                "status": row.status,
                "reason_code": row.reason_code,
                "revision": row.revision,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

async fn quarantine_event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    q: &QuarantineRow,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.quarantine.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: q.number_resource_id,
            scope: None,
            payload: json!({
                "number_resource_id": q.number_resource_id,
                "quarantine_id": q.id,
                "disposition": q.disposition,
                "status": q.status,
                "policy_ref": q.policy_ref,
                "cooldown_days": q.cooldown_days,
                "quarantine_until": q.quarantine_until,
                "revision": q.revision,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

impl Stewardship {
    /// Requests a release (idempotent per scope and key). `scope`: the customer's trusted
    /// scope (None for operators). Runs the steps at once when effective now.
    pub async fn request_release(
        &self,
        ctx: &RequestContext,
        scope: Option<&OwnerScope>,
        actor: Actor,
        req: NewRelease,
        idempotency_key: &str,
    ) -> DomainResult<(ReleaseRow, bool)> {
        crate::numbers::types::reason_code(&req.reason_code)?;
        if !crate::infra::idempotency::valid_key(idempotency_key) {
            return Err(DomainError::Validation(
                "an idempotency key is 8–128 of [A-Za-z0-9._:-]".into(),
            ));
        }
        let now = self.now();
        let effective = req.effective_at.unwrap_or(now).max(now);
        let txn = self.numbers.db().begin().await?;
        let assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
            vec![req.assignment_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|a| scope.is_none_or(|s| a.in_scope(s)))
        .ok_or(DomainError::NotFound("assignment"))?;
        if let Some(existing) = ReleaseRow::find_by_statement(stmt(
            &format!(
                "{} WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND idempotency_key = $4",
                release_columns()
            ),
            vec![
                assignment.tenant_id.into(),
                assignment.application_id.into(),
                assignment.environment_id.into(),
                idempotency_key.into(),
            ],
        ))
        .one(&txn)
        .await?
        {
            txn.rollback().await?;
            if existing.assignment_id != assignment.id || existing.policy != req.policy.as_str() {
                return Err(DomainError::conflict(
                    "IDEMPOTENCY_KEY_REUSED",
                    "the idempotency key was used for a different release",
                ));
            }
            return Ok((existing, true));
        }
        if req.policy == ReleasePolicy::PortedOut && actor.kind == ActorKind::Customer {
            return Err(DomainError::precondition(
                "PORT_OUT_REQUIRED",
                "a ported-out release is completed by the port-out itself",
            ));
        }
        if !matches!(
            assignment.status(),
            AssignmentStatus::Active
                | AssignmentStatus::Suspended
                | AssignmentStatus::RoutingRemoved
                | AssignmentStatus::ReleaseRequested
        ) {
            return Err(DomainError::precondition(
                "ASSIGNMENT_NOT_RELEASABLE",
                "only an active, suspended or partially released assignment is released",
            ));
        }
        if req.policy != ReleasePolicy::PortedOut
            && count(
                &txn,
                "SELECT count(*) AS n FROM number_port_requests WHERE assignment_id = $1 AND direction = 'OUT' \
                   AND status NOT IN ('COMPLETED', 'REJECTED', 'CANCELLED')",
                vec![assignment.id.into()],
            )
            .await?
                > 0
        {
            return Err(DomainError::conflict(
                "PORT_IN_PROGRESS",
                "a port-out of this number is in progress",
            ));
        }
        let row = ReleaseRow::find_by_statement(stmt(
            "INSERT INTO number_release_requests (id, number_resource_id, assignment_id, tenant_id, application_id, \
               environment_id, owner_kind, owner_ref, ownership_class, reason_code, policy, requester_kind, \
               requester_ref, requested_at, effective_at, idempotency_key, status, updated_at, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, 'REQUESTED', $14, $17) \
             RETURNING *",
            vec![
                Uuid::new_v4().into(),
                assignment.number_resource_id.into(),
                assignment.id.into(),
                assignment.tenant_id.into(),
                assignment.application_id.into(),
                assignment.environment_id.into(),
                assignment.owner_kind.clone().into(),
                assignment.owner_ref.into(),
                assignment.ownership_class.clone().into(),
                req.reason_code.clone().into(),
                req.policy.as_str().into(),
                actor.kind.as_str().into(),
                actor.reference.clone().into(),
                now.into(),
                effective.into(),
                idempotency_key.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        release_event(&txn, ctx, &row, now).await?;
        // Rapid release / reassignment is a risk signal of the scope (Workstream T).
        super::risk::note_release_in(&txn, &self.settings.risk, &row, now).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_release_requests_total",
            &[("policy", req.policy.as_str())],
        );
        let row = if effective <= now {
            self.advance_release(ctx, row.id).await?
        } else {
            row
        };
        Ok((row, false))
    }

    /// A release request of the caller's scope.
    pub async fn release_request(
        &self,
        scope: Option<&OwnerScope>,
        id: Uuid,
    ) -> DomainResult<ReleaseRow> {
        ReleaseRow::find_by_statement(stmt(
            &format!("{} WHERE id = $1", release_columns()),
            vec![id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .filter(|r| scope.is_none_or(|s| r.in_scope(s)))
        .ok_or(DomainError::NotFound("release request"))
    }

    /// Advances one release through its next steps (idempotent; safe to call again).
    pub async fn advance_release(
        &self,
        ctx: &RequestContext,
        id: Uuid,
    ) -> DomainResult<ReleaseRow> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let row = lock_release(&txn, id).await?;
        let actor = Actor::new(ActorKind::System, "numbering.release");
        let out = match row.status.as_str() {
            "REQUESTED" if row.effective_at <= now => {
                self.release_routing_in(&txn, ctx, &row, &actor, now)
                    .await?
            }
            "ROUTING_REMOVED" => {
                self.release_provider_in(&txn, ctx, &row, &actor, now)
                    .await?
            }
            "PROVIDER_RELEASE_PENDING" | "PROVIDER_OUTCOME_UNKNOWN" => {
                self.release_settle_in(&txn, ctx, &row, &actor, now).await?
            }
            _ => row,
        };
        txn.commit().await?;
        Ok(out)
    }

    /// Steps 1–5: grants stopped, voice / SMS routing removed, resources detached and
    /// the removal evidence confirmed.
    async fn release_routing_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        row: &ReleaseRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<ReleaseRow> {
        let resource = lock(txn, row.number_resource_id).await?;
        let mut assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
            vec![row.assignment_id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::NotFound("assignment"))?;
        // 1. Stop new grants and SMS submissions.
        super::suspension::end_open_suspension_in(txn, resource.id, "RELEASED", now).await?;
        if matches!(
            assignment.status(),
            AssignmentStatus::Active | AssignmentStatus::Suspended
        ) {
            assignment = self
                .numbers
                .step_assignment_in(
                    txn,
                    ctx,
                    &assignment,
                    AssignmentStatus::ReleaseRequested,
                    None,
                    actor,
                    &row.reason_code,
                    now,
                )
                .await?;
        }
        crate::numbers::grants::revoke_grants_in(
            txn,
            resource.id,
            crate::numbers::grants::revoke_reason::ASSIGNMENT_NOT_ACTIVE,
            now,
        )
        .await?;
        // 2–3. Remove the previous owner's voice and SMS routing.
        if assignment.status() == AssignmentStatus::ReleaseRequested {
            assignment = self
                .numbers
                .step_assignment_in(
                    txn,
                    ctx,
                    &assignment,
                    AssignmentStatus::RoutingRemoved,
                    None,
                    actor,
                    "ROUTING_REMOVED",
                    now,
                )
                .await?;
        }
        let resource = lock(txn, resource.id).await?;
        if matches!(
            resource.lifecycle_state(),
            LifecycleState::Active | LifecycleState::Suspended
        ) {
            transition_in(
                txn,
                ctx,
                resource.id,
                TransitionRequest::new(
                    LifecycleState::Releasing,
                    resource.revision,
                    actor.clone(),
                    "ASSIGNMENT_RELEASED",
                ),
                now,
            )
            .await?;
        }
        let retired = self
            .numbers
            .retire_routes_in(txn, ctx, resource.id, "ROUTE_REMOVED_ON_RELEASE", now)
            .await?;
        // 4. Detach the previous owner's organizational resources.
        let detached = self
            .numbers
            .detach_pool_memberships_in(txn, ctx, actor, assignment.id, now)
            .await?;
        // 5. Confirm the removal evidence: nothing may still authorize or route.
        let live_grants = count(
            txn,
            "SELECT count(*) AS n FROM number_routing_grants WHERE number_resource_id = $1 \
               AND revoked_at IS NULL AND expires_at > $2",
            vec![resource.id.into(), now.into()],
        )
        .await?;
        let ready_routes = count(
            txn,
            "SELECT count(*) AS n FROM number_route_configurations WHERE number_resource_id = $1 AND status = 'CONFIGURED'",
            vec![resource.id.into()],
        )
        .await?;
        let state = lock(txn, resource.id).await?.state;
        if live_grants > 0 || ready_routes > 0 || state == "ACTIVE" {
            incidents::raise(
                txn,
                incidents::Category::LifecycleSafety,
                &format!("release-routing:{}", row.id),
                incidents::Subject {
                    provider_id: None,
                    number_resource_id: Some(resource.id),
                    command_id: None,
                },
                json!({ "reason": "ROUTING_REMOVAL_UNCONFIRMED" }),
                now,
            )
            .await?;
            let failed = set_release(
                txn,
                row,
                "FAILED",
                Step {
                    failure_reason: Some("ROUTING_REMOVAL_UNCONFIRMED".into()),
                    ..Default::default()
                },
                now,
            )
            .await?;
            release_event(txn, ctx, &failed, now).await?;
            return Ok(failed);
        }
        let evidence = json!({
            "assignmentStatus": assignment.status,
            "lifecycleState": state,
            "retiredRoutes": retired.iter().map(|(f, r, v)| json!({
                "family": f, "routeRef": r, "routeRevision": v,
            })).collect::<Vec<_>>(),
            "routingRevision": resource.routing_revision,
            "liveGrants": live_grants,
            "readyRoutes": ready_routes,
            "poolMembershipsEnded": detached,
            "confirmedAt": now,
        });
        let next = set_release(
            txn,
            row,
            "ROUTING_REMOVED",
            Step {
                grants_stopped_at: Some(now),
                routing_removed_at: Some(now),
                routing_evidence: Some(evidence),
                detached_at: Some(now),
                ..Default::default()
            },
            now,
        )
        .await?;
        release_event(txn, ctx, &next, now).await?;
        self.release_provider_in(txn, ctx, &next, actor, now).await
    }

    /// Step 6: the provider release (relinquish) or, for a pool return, quarantine now.
    async fn release_provider_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        row: &ReleaseRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<ReleaseRow> {
        match row.policy() {
            ReleasePolicy::PoolReturn => {
                self.enter_quarantine_in(txn, ctx, row, actor, "POOL_RETURN", None, now)
                    .await
            }
            ReleasePolicy::PortedOut => Ok(row.clone()),
            ReleasePolicy::ProviderRelinquish => {
                let p = self.providers()?;
                let submitted = p
                    .request_release_in(
                        txn,
                        ctx,
                        actor.clone(),
                        row.number_resource_id,
                        &format!("release:{}", row.id.simple()),
                    )
                    .await?;
                let next = set_release(
                    txn,
                    row,
                    "PROVIDER_RELEASE_PENDING",
                    Step {
                        provider_command_id: Some(submitted.command.id),
                        ..Default::default()
                    },
                    now,
                )
                .await?;
                release_event(txn, ctx, &next, now).await?;
                Ok(next)
            }
        }
    }

    /// Step 7: the provider outcome decides; an unknown one waits for reconciliation.
    async fn release_settle_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        row: &ReleaseRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<ReleaseRow> {
        let p = self.providers()?;
        let cmd = p
            .command(
                row.provider_command_id
                    .expect("a pending release has its command"),
            )
            .await?;
        match command_outcome(&cmd.status) {
            CommandOutcome::Succeeded => {
                let evidence = json!({ "providerCommandId": cmd.id, "commandStatus": cmd.status });
                self.enter_quarantine_in(txn, ctx, row, actor, "RELINQUISHED", Some(evidence), now)
                    .await
            }
            CommandOutcome::Unknown if row.status != "PROVIDER_OUTCOME_UNKNOWN" => {
                let next =
                    set_release(txn, row, "PROVIDER_OUTCOME_UNKNOWN", Step::default(), now).await?;
                release_event(txn, ctx, &next, now).await?;
                Ok(next)
            }
            CommandOutcome::Failed | CommandOutcome::Unsupported => {
                incidents::raise(
                    txn,
                    incidents::Category::LifecycleSafety,
                    &format!("release-refused:{}", row.id),
                    incidents::Subject {
                        provider_id: Some(cmd.provider_id.clone()),
                        number_resource_id: Some(row.number_resource_id),
                        command_id: Some(cmd.id),
                    },
                    json!({ "reason": "PROVIDER_RELEASE_REFUSED" }),
                    now,
                )
                .await?;
                let next = set_release(
                    txn,
                    row,
                    "FAILED",
                    Step {
                        failure_reason: Some("PROVIDER_RELEASE_REFUSED".into()),
                        ..Default::default()
                    },
                    now,
                )
                .await?;
                release_event(txn, ctx, &next, now).await?;
                Ok(next)
            }
            _ => Ok(row.clone()),
        }
    }

    /// Step 8: quarantine with its policy (the assignment QUARANTINED, the number
    /// QUARANTINED when still releasing).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn enter_quarantine_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        row: &ReleaseRow,
        actor: &Actor,
        disposition: &str,
        provider_evidence: Option<Value>,
        now: DateTime<Utc>,
    ) -> DomainResult<ReleaseRow> {
        let resource = lock(txn, row.number_resource_id).await?;
        let (days, policy_ref) = self.cooldown_for(&resource);
        let until = now + Duration::days(days);
        let assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
            vec![row.assignment_id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::NotFound("assignment"))?;
        if assignment.status() == AssignmentStatus::RoutingRemoved {
            self.numbers
                .step_assignment_in(
                    txn,
                    ctx,
                    &assignment,
                    AssignmentStatus::Quarantined,
                    None,
                    actor,
                    "QUARANTINED",
                    now,
                )
                .await?;
        }
        if resource.lifecycle_state() == LifecycleState::Releasing {
            transition_in(
                txn,
                ctx,
                resource.id,
                TransitionRequest::new(
                    LifecycleState::Quarantined,
                    resource.revision,
                    actor.clone(),
                    "RELEASE_QUARANTINE",
                )
                .with_quarantine_until(until),
                now,
            )
            .await?;
        }
        let state = lock(txn, resource.id).await?.lifecycle_state();
        if state != LifecycleState::Quarantined {
            return Err(DomainError::InvalidTransition {
                entity: "number resource",
                from: state.as_str().into(),
                to: "QUARANTINED".into(),
            });
        }
        let relinquished = disposition == "RELINQUISHED";
        let q = QuarantineRow::find_by_statement(stmt(
            "INSERT INTO number_quarantines (id, number_resource_id, release_request_id, previous_assignment_id, \
               disposition, policy_ref, cooldown_days, started_at, quarantine_until, status, \
               provider_confirmed_at, provider_evidence, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE', $10, $11, $8) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                resource.id.into(),
                row.id.into(),
                row.assignment_id.into(),
                disposition.into(),
                policy_ref.into(),
                (days as i32).into(),
                now.into(),
                until.into(),
                relinquished.then_some(now).into(),
                provider_evidence.into(),
            ],
        ))
        .one(txn)
        .await?
        .expect("insert returns the row");
        quarantine_event(txn, ctx, &q, now).await?;
        let next = set_release(
            txn,
            row,
            "QUARANTINED",
            Step {
                quarantine_id: Some(q.id),
                provider_released_at: (row.policy() != ReleasePolicy::PoolReturn).then_some(now),
                ..Default::default()
            },
            now,
        )
        .await?;
        release_event(txn, ctx, &next, now).await?;
        crate::metrics::inc(
            "numbering_quarantines_total",
            &[(
                "disposition",
                if relinquished {
                    "RELINQUISHED"
                } else {
                    "POOL_RETURN"
                },
            )],
        );
        Ok(next)
    }

    fn cooldown_for(&self, r: &ResourceRow) -> (i64, String) {
        self.settings.quarantine.cooldown(
            &r.country_code,
            NumberType::parse(&r.number_type).unwrap_or(NumberType::Unclassified),
            r.inventory() == InventorySource::SyntheticTest,
            RiskTier::parse(&r.risk_tier).unwrap_or(RiskTier::Unassessed),
        )
    }

    /// Worker pass: every release with a step due.
    pub async fn advance_releases(&self, ctx: &RequestContext, limit: u64) -> DomainResult<usize> {
        let ids = IdRow::find_by_statement(stmt(
            "SELECT id FROM number_release_requests WHERE \
               (status = 'REQUESTED' AND effective_at <= $1) \
               OR status IN ('ROUTING_REMOVED', 'PROVIDER_RELEASE_PENDING', 'PROVIDER_OUTCOME_UNKNOWN') \
             ORDER BY updated_at LIMIT $2",
            vec![self.now().into(), (limit.clamp(1, 500) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for r in ids {
            match self.advance_release(ctx, r.id).await {
                Ok(_) => n += 1,
                Err(e) => tracing::warn!(error = %e.code(), "release step refused"),
            }
        }
        Ok(n)
    }

    /// The current quarantine of a number (privileged).
    pub async fn quarantine_of(&self, resource_id: Uuid) -> DomainResult<QuarantineRow> {
        QuarantineRow::find_by_statement(stmt(
            "SELECT * FROM number_quarantines WHERE number_resource_id = $1 ORDER BY started_at DESC, id LIMIT 1",
            vec![resource_id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::NotFound("quarantine"))
    }

    /// Quarantines of the caller's scope (by previous assignment), newest first.
    pub async fn quarantine_for_assignment(
        &self,
        scope: &OwnerScope,
        assignment_id: Uuid,
    ) -> DomainResult<QuarantineRow> {
        QuarantineRow::find_by_statement(stmt(
            "SELECT q.* FROM number_quarantines q JOIN dcn_assignments a ON a.id = q.previous_assignment_id \
             WHERE q.previous_assignment_id = $1 AND a.tenant_id = $2 AND a.application_id = $3 AND a.environment_id = $4 \
             ORDER BY q.started_at DESC LIMIT 1",
            vec![
                assignment_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or(DomainError::NotFound("quarantine"))
    }

    /// An operator's review decision on a quarantine (approve completion, extend, hold).
    pub async fn review_quarantine(
        &self,
        ctx: &RequestContext,
        operator: &Actor,
        quarantine_id: Uuid,
        decision: QuarantineReview,
    ) -> DomainResult<QuarantineRow> {
        if operator.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "quarantine reviews are operator decisions",
            ));
        }
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let q = QuarantineRow::find_by_statement(stmt(
            "SELECT * FROM number_quarantines WHERE id = $1 FOR UPDATE",
            vec![quarantine_id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("quarantine"))?;
        if q.status == "COMPLETED" {
            return Err(DomainError::precondition(
                "QUARANTINE_COMPLETED",
                "a completed quarantine is not reviewed",
            ));
        }
        let (status, until, label, extensions) = match decision {
            QuarantineReview::Approve => ("ACTIVE", q.quarantine_until, "APPROVED", q.extensions),
            QuarantineReview::Hold => ("HELD_FOR_REVIEW", q.quarantine_until, "HELD", q.extensions),
            QuarantineReview::Extend { days } => {
                if !(1..=self.settings.quarantine.max_extension_days).contains(&days) {
                    return Err(DomainError::Validation(
                        "an extension is 1 to the policy maximum of days".into(),
                    ));
                }
                (
                    "ACTIVE",
                    q.quarantine_until.max(now) + Duration::days(days),
                    "EXTENDED",
                    q.extensions + 1,
                )
            }
        };
        let row = QuarantineRow::find_by_statement(stmt(
            "UPDATE number_quarantines SET status = $3, quarantine_until = $4, review_decision = $5, \
               reviewed_by = $6, reviewed_at = $7, extensions = $8, review_required = $9, \
               revision = revision + 1, updated_at = $7 \
             WHERE id = $1 AND revision = $2 RETURNING *",
            vec![
                q.id.into(),
                q.revision.into(),
                status.into(),
                until.into(),
                label.into(),
                operator.reference.clone().into(),
                now.into(),
                extensions.into(),
                (label == "HELD").into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::VersionConflict("quarantine"))?;
        quarantine_event(&txn, ctx, &row, now).await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Records late voice / SMS traffic for a releasing / quarantined number (refused,
    /// never delivered). Repeated late traffic requires an operator review before reuse.
    pub(crate) async fn record_late_activity(
        numbers: &crate::numbers::NumberDomain,
        review_after: i64,
        ctx: &RequestContext,
        resource: &ResourceRow,
        family: &'static str,
        consumer: &str,
    ) -> DomainResult<()> {
        let now = numbers.clock().now();
        let txn = numbers.db().begin().await?;
        let q = QuarantineRow::find_by_statement(stmt(
            "SELECT * FROM number_quarantines WHERE number_resource_id = $1 AND status <> 'COMPLETED' FOR UPDATE",
            vec![resource.id.into()],
        ))
        .one(&txn)
        .await?;
        txn.execute(stmt(
            "INSERT INTO number_late_activity (id, number_resource_id, quarantine_id, family, consumer, \
               lifecycle_state, treatment, observed_at, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, 'REFUSED', $7, $8)",
            vec![
                Uuid::new_v4().into(),
                resource.id.into(),
                q.as_ref().map(|q| q.id).into(),
                family.into(),
                consumer.into(),
                resource.state.clone().into(),
                now.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .await?;
        if let Some(q) = q.filter(|q| !q.review_required) {
            let late = count(
                &txn,
                "SELECT count(*) AS n FROM number_late_activity WHERE quarantine_id = $1",
                vec![q.id.into()],
            )
            .await?;
            if late >= review_after {
                txn.execute(stmt(
                    "UPDATE number_quarantines SET review_required = TRUE, revision = revision + 1, updated_at = $2 \
                     WHERE id = $1",
                    vec![q.id.into(), now.into()],
                ))
                .await?;
            }
        }
        txn.commit().await?;
        crate::metrics::inc("numbering_late_activity_total", &[("family", family)]);
        Ok(())
    }

    /// Worker pass: completes every quarantine whose conditions hold. Each completion
    /// locks its row with SKIP LOCKED: concurrent workers never complete one twice.
    pub async fn complete_quarantines(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<usize> {
        let mut done = 0;
        for _ in 0..limit.clamp(1, 500) {
            match self.complete_next_quarantine(ctx).await? {
                Some(true) => done += 1,
                Some(false) => {}
                None => break,
            }
        }
        Ok(done)
    }

    /// One completion attempt. `None`: nothing due; `Some(completed)`.
    async fn complete_next_quarantine(&self, ctx: &RequestContext) -> DomainResult<Option<bool>> {
        let now = self.now();
        // Read-back first (outside the transaction: no provider call holds a lock).
        let due = QuarantineRow::find_by_statement(stmt(
            "SELECT * FROM number_quarantines WHERE status = 'ACTIVE' AND quarantine_until <= $1 \
               AND NOT (review_required AND review_decision IS DISTINCT FROM 'APPROVED') \
             ORDER BY quarantine_until, id LIMIT 50",
            vec![now.into()],
        ))
        .all(self.numbers.db())
        .await?;
        for candidate in due {
            let txn = self.numbers.db().begin().await?;
            let Some(q) = QuarantineRow::find_by_statement(stmt(
                "SELECT * FROM number_quarantines WHERE id = $1 AND status = 'ACTIVE' FOR UPDATE SKIP LOCKED",
                vec![candidate.id.into()],
            ))
            .one(&txn)
            .await?
            else {
                txn.rollback().await?;
                continue;
            };
            let completed = self.complete_quarantine_in(&txn, ctx, &q, now).await?;
            txn.commit().await?;
            return Ok(Some(completed));
        }
        Ok(None)
    }

    async fn complete_quarantine_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        q: &QuarantineRow,
        now: DateTime<Utc>,
    ) -> DomainResult<bool> {
        let actor = Actor::new(ActorKind::System, "numbering.quarantine");
        let hold = |reason: &'static str| async move {
            let held = QuarantineRow::find_by_statement(stmt(
                "UPDATE number_quarantines SET status = 'HELD_FOR_REVIEW', review_required = TRUE, \
                   completion_reason = $3, revision = revision + 1, updated_at = $4 \
                 WHERE id = $1 AND revision = $2 RETURNING *",
                vec![q.id.into(), q.revision.into(), reason.into(), now.into()],
            ))
            .one(txn)
            .await?
            .ok_or(DomainError::VersionConflict("quarantine"))?;
            incidents::raise(
                txn,
                incidents::Category::QuarantineConflict,
                &format!("quarantine:{}", q.id),
                incidents::Subject {
                    provider_id: None,
                    number_resource_id: Some(q.number_resource_id),
                    command_id: None,
                },
                json!({ "reason": reason }),
                now,
            )
            .await?;
            quarantine_event(txn, ctx, &held, now).await?;
            Ok::<bool, DomainError>(false)
        };
        // No unresolved provider command and no open release.
        let unsettled = count(
            txn,
            "SELECT count(*) AS n FROM number_provider_commands WHERE number_resource_id = $1 \
               AND status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')",
            vec![q.number_resource_id.into()],
        )
        .await?;
        let open_release = count(
            txn,
            "SELECT count(*) AS n FROM number_release_requests WHERE number_resource_id = $1 \
               AND status NOT IN ('QUARANTINED', 'FAILED', 'CANCELLED')",
            vec![q.number_resource_id.into()],
        )
        .await?;
        if unsettled > 0 || open_release > 0 {
            return Ok(false);
        }
        let resource = lock(txn, q.number_resource_id).await?;
        if resource.lifecycle_state() != LifecycleState::Quarantined {
            return hold("LIFECYCLE_NOT_QUARANTINED").await;
        }
        let previous = match q.previous_assignment_id {
            Some(id) => {
                AssignmentRow::find_by_statement(stmt(
                    "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
                    vec![id.into()],
                ))
                .one(txn)
                .await?
            }
            None => None,
        };
        let (to, reason, confirmed, evidence) = if q.disposition == "RELINQUISHED" {
            // No longer CF Meet's: finally RELEASED, never reassignable.
            if count(
                txn,
                "SELECT count(*) AS n FROM number_provider_bindings WHERE number_resource_id = $1 AND status = 'ACTIVE'",
                vec![resource.id.into()],
            )
            .await?
                > 0
            {
                return hold("BINDING_STILL_ACTIVE").await;
            }
            (
                LifecycleState::Released,
                "RELINQUISHED_FINAL",
                q.provider_confirmed_at,
                q.provider_evidence.clone(),
            )
        } else {
            // A pool return: the provider must confirm CF Meet still controls the number.
            let read = match &self.providers {
                Some(p) => p.inspect_resource(resource.id).await?,
                None => None,
            };
            let state = match read {
                Some(ProviderOutcome::Succeeded(s)) => s,
                // Outage / no adapter: try again later, never assume.
                _ => return Ok(false),
            };
            let ours = state.owned_by_us
                && matches!(
                    state.status,
                    ProviderNumberStatus::Provisioned | ProviderNumberStatus::Suspended
                );
            if !ours {
                return hold("PROVIDER_STATE_CONFLICT").await;
            }
            (
                LifecycleState::Reassignable,
                "QUARANTINE_COMPLETED",
                Some(now),
                Some(json!({
                    "providerStatus": state.status,
                    "ownedByUs": state.owned_by_us,
                    "providerSequence": state.sequence,
                    "observedAt": state.observed_at,
                })),
            )
        };
        let completed = QuarantineRow::find_by_statement(stmt(
            "UPDATE number_quarantines SET status = 'COMPLETED', completed_at = $3, completion_reason = $4, \
               provider_confirmed_at = COALESCE(provider_confirmed_at, $5), \
               provider_evidence = COALESCE($6, provider_evidence), revision = revision + 1, updated_at = $3 \
             WHERE id = $1 AND revision = $2 RETURNING *",
            vec![
                q.id.into(),
                q.revision.into(),
                now.into(),
                reason.into(),
                confirmed.into(),
                evidence.into(),
            ],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("quarantine"))?;
        if let Some(a) = previous.filter(|a| a.status() == AssignmentStatus::Quarantined) {
            self.numbers
                .step_assignment_in(
                    txn,
                    ctx,
                    &a,
                    AssignmentStatus::Closed,
                    Some("RELEASED"),
                    &actor,
                    "RELEASED",
                    now,
                )
                .await?;
        }
        if to == LifecycleState::Released {
            let r = lock(txn, resource.id).await?;
            transition_in(
                txn,
                ctx,
                r.id,
                TransitionRequest::new(
                    LifecycleState::Releasing,
                    r.revision,
                    actor.clone(),
                    "RELINQUISHED",
                ),
                now,
            )
            .await?;
            let r = lock(txn, resource.id).await?;
            transition_in(
                txn,
                ctx,
                r.id,
                TransitionRequest::new(
                    LifecycleState::Released,
                    r.revision,
                    actor.clone(),
                    "RELINQUISHED_FINAL",
                )
                .with_evidence(Uuid::new_v4()),
                now,
            )
            .await?;
        } else {
            let r = lock(txn, resource.id).await?;
            transition_in(
                txn,
                ctx,
                r.id,
                TransitionRequest::new(
                    LifecycleState::Reassignable,
                    r.revision,
                    actor.clone(),
                    "QUARANTINE_COMPLETED",
                ),
                now,
            )
            .await?;
        }
        quarantine_event(txn, ctx, &completed, now).await?;
        crate::metrics::inc(
            "numbering_quarantine_completions_total",
            &[(
                "disposition",
                if completed.disposition == "RELINQUISHED" {
                    "RELINQUISHED"
                } else {
                    "POOL_RETURN"
                },
            )],
        );
        Ok(true)
    }
}
