//! Workstream P — suspension and restoration.
//!
//! Suspension is LOCAL FIRST, in one transaction: the number and its assignment become
//! SUSPENDED (the assignment leaving ACTIVE revokes every outstanding grant; new caller-
//! identity / SMS-sender grants and inbound resolutions are refused from that commit on —
//! a grant issued concurrently fails verification against current state), the
//! suspension record keeps source, reason, actor, timing and evidence, the provider
//! SUSPEND (route disable) is submitted to the durable executor where the adapter supports
//! it, and `number.suspended` is published. Lawful ownership is retained: the assignment
//! stays open and the owner unchanged.
//!
//! Restoration re-checks, at that moment: the same open assignment and owner, the KYC /
//! eligibility basis in force, the tenant standing, no risk restriction in force, an
//! active provider binding serving every routable capability, the capabilities usable and
//! every routed family READY. A suspension by an operator, the risk policy or the provider
//! is lifted by an operator only. When the provider was suspended, RESTORE is submitted
//! and the assignment returns to ACTIVE only once the provider confirms it; an uncertain
//! provider outcome waits for reconciliation. Only then is `number.restored` published.

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{command_outcome, count, CommandOutcome, Stewardship};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
    numbers::{
        assignments::AssignmentRow,
        eligibility::Standing,
        lifecycle::{transition_in, TransitionRequest},
        resources::lock,
        stmt, Actor, ActorKind, AssignmentStatus, Capability, DomainError, DomainResult,
        LifecycleState, OwnerScope,
    },
    providers::{incidents, ProviderOperation},
};

/// Who asked for the suspension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuspensionSource {
    Customer,
    Organization,
    Operator,
    Risk,
    Provider,
}

impl SuspensionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SuspensionSource::Customer => "CUSTOMER",
            SuspensionSource::Organization => "ORGANIZATION",
            SuspensionSource::Operator => "OPERATOR",
            SuspensionSource::Risk => "RISK",
            SuspensionSource::Provider => "PROVIDER",
        }
    }

    /// Lifted only by an operator.
    pub fn operator_lifted(&self) -> bool {
        matches!(
            self,
            SuspensionSource::Operator | SuspensionSource::Risk | SuspensionSource::Provider
        )
    }

    fn parse(s: &str) -> Self {
        match s {
            "CUSTOMER" => SuspensionSource::Customer,
            "ORGANIZATION" => SuspensionSource::Organization,
            "RISK" => SuspensionSource::Risk,
            "PROVIDER" => SuspensionSource::Provider,
            _ => SuspensionSource::Operator,
        }
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuspensionRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub source: String,
    pub reason_code: String,
    pub evidence_ref: Option<Uuid>,
    pub note_ref: Option<Uuid>,
    pub actor_kind: String,
    pub actor_ref: String,
    pub suspended_at: DateTime<Utc>,
    pub provider_command_id: Option<Uuid>,
    pub provider_state: String,
    pub status: String,
    pub revision: i64,
    pub restore_requested_at: Option<DateTime<Utc>>,
    pub restore_actor_kind: Option<String>,
    pub restore_actor_ref: Option<String>,
    pub restore_command_id: Option<Uuid>,
    pub restore_checks: Option<Value>,
    pub restore_blocked_reason: Option<String>,
    pub restored_at: Option<DateTime<Utc>>,
    pub ended_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub correlation_id: String,
}

impl SuspensionRow {
    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    pub fn source(&self) -> SuspensionSource {
        SuspensionSource::parse(&self.source)
    }

    fn event_scope(&self) -> EventScope {
        EventScope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}

/// A suspension request.
#[derive(Debug, Clone)]
pub struct SuspendRequest {
    pub assignment_id: Uuid,
    pub reason_code: String,
    pub source: SuspensionSource,
    pub evidence_ref: Option<Uuid>,
}

pub(crate) async fn open_suspension_in<C: sea_orm::ConnectionTrait>(
    db: &C,
    resource_id: Uuid,
    lock_row: bool,
) -> DomainResult<Option<SuspensionRow>> {
    Ok(SuspensionRow::find_by_statement(stmt(
        &format!(
            "SELECT * FROM number_suspensions WHERE number_resource_id = $1 \
               AND status IN ('SUSPENDED', 'RESTORING'){}",
            if lock_row { " FOR UPDATE" } else { "" }
        ),
        vec![resource_id.into()],
    ))
    .one(db)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn update(
    txn: &DatabaseTransaction,
    row: &SuspensionRow,
    status: &str,
    provider_state: &str,
    restore: Option<(&Actor, DateTime<Utc>)>,
    restore_command: Option<Uuid>,
    checks: Option<Value>,
    blocked: Option<&str>,
    restored_at: Option<DateTime<Utc>>,
    ended_reason: Option<&str>,
    now: DateTime<Utc>,
) -> DomainResult<SuspensionRow> {
    SuspensionRow::find_by_statement(stmt(
        "UPDATE number_suspensions SET status = $3, provider_state = $4, \
           restore_requested_at = COALESCE($5, restore_requested_at), \
           restore_actor_kind = COALESCE($6, restore_actor_kind), restore_actor_ref = COALESCE($7, restore_actor_ref), \
           restore_command_id = COALESCE($8, restore_command_id), restore_checks = COALESCE($9, restore_checks), \
           restore_blocked_reason = $10, restored_at = COALESCE($11, restored_at), \
           ended_reason = COALESCE($12, ended_reason), revision = revision + 1, updated_at = $13 \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            row.id.into(),
            row.revision.into(),
            status.into(),
            provider_state.into(),
            restore.map(|r| r.1).into(),
            restore.map(|r| r.0.kind.as_str().to_string()).into(),
            restore.map(|r| r.0.reference.clone()).into(),
            restore_command.into(),
            checks.into(),
            blocked.map(str::to_string).into(),
            restored_at.into(),
            ended_reason.map(str::to_string).into(),
            now.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("suspension"))
}

async fn event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &SuspensionRow,
    restored: bool,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    let payload = json!({
        "number_resource_id": row.number_resource_id,
        "assignment_id": row.assignment_id,
        "suspension_id": row.id,
        "source": row.source,
        "reason_code": row.reason_code,
        "provider_state": row.provider_state,
        "status": row.status,
    });
    let event = if restored {
        NewEvent {
            event_type: "number.restored",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: row.number_resource_id,
            scope: Some(row.event_scope()),
            payload,
        }
    } else {
        NewEvent {
            event_type: "number.suspended",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: row.number_resource_id,
            scope: Some(row.event_scope()),
            payload,
        }
    };
    outbox::enqueue(txn, event, ctx, now).await?;
    Ok(())
}

/// Ends the open suspension of a number (release / port-out supersedes it).
pub(crate) async fn end_open_suspension_in(
    txn: &DatabaseTransaction,
    resource_id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    if let Some(open) = open_suspension_in(txn, resource_id, true).await? {
        update(
            txn,
            &open,
            "ENDED",
            &open.provider_state,
            None,
            None,
            None,
            None,
            None,
            Some(reason),
            now,
        )
        .await?;
    }
    Ok(())
}

impl Stewardship {
    /// Suspends an ACTIVE assignment and its number. `scope`: the customer's trusted scope
    /// (None for operator / risk / provider actions). Idempotent: an already suspended
    /// number answers its open suspension.
    pub async fn suspend(
        &self,
        ctx: &RequestContext,
        scope: Option<&OwnerScope>,
        actor: Actor,
        req: SuspendRequest,
    ) -> DomainResult<SuspensionRow> {
        crate::numbers::types::reason_code(&req.reason_code)?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let assignment = match scope {
            Some(sc) => {
                self.numbers
                    .lock_assignment_in(&txn, sc, req.assignment_id)
                    .await?
            }
            None => self.lock_any(&txn, req.assignment_id).await?,
        };
        let row = self
            .suspend_in(&txn, ctx, &assignment, actor, &req, now)
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_suspensions_total",
            &[("source", req.source.as_str())],
        );
        Ok(row)
    }

    async fn lock_any(&self, txn: &DatabaseTransaction, id: Uuid) -> DomainResult<AssignmentRow> {
        AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
            vec![id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::NotFound("assignment"))
    }

    /// [`Self::suspend`] in the caller's transaction (risk-triggered suspension composes it).
    pub(crate) async fn suspend_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        assignment: &AssignmentRow,
        actor: Actor,
        req: &SuspendRequest,
        now: DateTime<Utc>,
    ) -> DomainResult<SuspensionRow> {
        let resource = lock(txn, assignment.number_resource_id).await?;
        if let Some(open) = open_suspension_in(txn, resource.id, true).await? {
            if open.assignment_id == assignment.id {
                return Ok(open);
            }
        }
        if assignment.status() != AssignmentStatus::Active
            || resource.lifecycle_state() != LifecycleState::Active
        {
            return Err(DomainError::precondition(
                "ASSIGNMENT_NOT_ACTIVE",
                "only an ACTIVE number is suspended",
            ));
        }
        // 1. The number, then the assignment (leaving ACTIVE revokes outstanding grants).
        let mut t = TransitionRequest::new(
            LifecycleState::Suspended,
            resource.revision,
            actor.clone(),
            "NUMBER_SUSPENDED",
        )
        .with_suspension(&req.reason_code);
        if let Some(e) = req.evidence_ref {
            t = t.with_evidence(e);
        }
        transition_in(txn, ctx, resource.id, t, now).await?;
        self.numbers
            .step_assignment_in(
                txn,
                ctx,
                assignment,
                AssignmentStatus::Suspended,
                None,
                &actor,
                &req.reason_code,
                now,
            )
            .await?;
        crate::numbers::grants::revoke_grants_in(
            txn,
            resource.id,
            crate::numbers::grants::revoke_reason::ASSIGNMENT_NOT_ACTIVE,
            now,
        )
        .await?;
        // 2. The provider route disable, where the adapter supports it.
        let id = Uuid::new_v4();
        let (command, provider_state) = match &self.providers {
            Some(p)
                if p.provider_supports(resource.id, ProviderOperation::Suspend)
                    .await? =>
            {
                let submitted = p
                    .request_suspend_in(
                        txn,
                        ctx,
                        resource.id,
                        &req.reason_code,
                        &format!("suspend:{}", id.simple()),
                    )
                    .await?;
                (Some(submitted.command.id), "PENDING")
            }
            Some(_) => (None, "UNSUPPORTED"),
            None => (None, "NOT_REQUIRED"),
        };
        let row = SuspensionRow::find_by_statement(stmt(
            "INSERT INTO number_suspensions (id, number_resource_id, assignment_id, tenant_id, application_id, \
               environment_id, source, reason_code, evidence_ref, actor_kind, actor_ref, suspended_at, \
               provider_command_id, provider_state, status, updated_at, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'SUSPENDED', $12, $15) RETURNING *",
            vec![
                id.into(),
                resource.id.into(),
                assignment.id.into(),
                assignment.tenant_id.into(),
                assignment.application_id.into(),
                assignment.environment_id.into(),
                req.source.as_str().into(),
                req.reason_code.clone().into(),
                req.evidence_ref.into(),
                actor.kind.as_str().into(),
                actor.reference.clone().into(),
                now.into(),
                command.into(),
                provider_state.into(),
                ctx.correlation_id.as_str().into(),
            ],
        ))
        .one(txn)
        .await?
        .expect("insert returns the row");
        event(txn, ctx, &row, false, now).await?;
        Ok(row)
    }

    /// The open (or latest) suspension of an assignment in the caller's scope.
    pub async fn suspension_of(
        &self,
        scope: Option<&OwnerScope>,
        assignment_id: Uuid,
    ) -> DomainResult<SuspensionRow> {
        SuspensionRow::find_by_statement(stmt(
            "SELECT * FROM number_suspensions WHERE assignment_id = $1 \
               ORDER BY (status IN ('SUSPENDED', 'RESTORING')) DESC, suspended_at DESC, updated_at DESC LIMIT 1",
            vec![assignment_id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .filter(|s| scope.is_none_or(|sc| s.in_scope(sc)))
        .ok_or(DomainError::NotFound("suspension"))
    }

    /// Requests restoration of a suspended assignment; returns the suspension (RESTORED
    /// when no provider step is owed, RESTORING while the provider RESTORE runs). A failed
    /// check is recorded on the suspension and refused with its rule code.
    pub async fn restore(
        &self,
        ctx: &RequestContext,
        scope: Option<&OwnerScope>,
        actor: Actor,
        assignment_id: Uuid,
        standing: Standing,
    ) -> DomainResult<SuspensionRow> {
        let open = self.suspension_of(scope, assignment_id).await?;
        if !matches!(open.status.as_str(), "SUSPENDED" | "RESTORING") {
            return Err(DomainError::precondition(
                "NOT_SUSPENDED",
                "the assignment has no open suspension",
            ));
        }
        if open.status == "RESTORING" {
            return self.advance_suspension(ctx, open.id).await;
        }
        if open.source().operator_lifted() && actor.kind != ActorKind::Operator {
            return Err(DomainError::precondition(
                "OPERATOR_RESTORE_REQUIRED",
                "an operator, risk or provider suspension is lifted by an operator",
            ));
        }
        let checks = self.restore_checks(&open, standing).await?;
        let failed = checks
            .as_object()
            .and_then(|o| o.iter().find(|(_, v)| v.as_bool() == Some(false)))
            .map(|(k, _)| k.clone());
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let locked = open_suspension_in(&txn, open.number_resource_id, true)
            .await?
            .filter(|s| s.id == open.id && s.status == "SUSPENDED")
            .ok_or(DomainError::VersionConflict("suspension"))?;
        if let Some(code) = failed {
            let code = blocked_code(&code);
            update(
                &txn,
                &locked,
                "SUSPENDED",
                &locked.provider_state,
                None,
                None,
                Some(checks),
                Some(code),
                None,
                None,
                now,
            )
            .await?;
            txn.commit().await?;
            return Err(DomainError::precondition(
                code,
                "a restoration prerequisite does not hold",
            ));
        }
        let row = match locked.provider_state.as_str() {
            "PENDING" | "UNKNOWN" => {
                return Err(DomainError::conflict(
                    "COMMAND_IN_FLIGHT",
                    "the provider suspension has not settled; restoration waits for it",
                ))
            }
            "CONFIRMED" => {
                let p = self.providers()?;
                let submitted = p
                    .request_restore_in(
                        &txn,
                        ctx,
                        locked.number_resource_id,
                        &format!("restore:{}", locked.id.simple()),
                    )
                    .await?;
                update(
                    &txn,
                    &locked,
                    "RESTORING",
                    "CONFIRMED",
                    Some((&actor, now)),
                    Some(submitted.command.id),
                    Some(checks),
                    None,
                    None,
                    None,
                    now,
                )
                .await?
            }
            // NOT_REQUIRED / UNSUPPORTED / FAILED: nothing was disabled at the provider.
            _ => {
                let restoring = update(
                    &txn,
                    &locked,
                    "RESTORING",
                    &locked.provider_state,
                    Some((&actor, now)),
                    None,
                    Some(checks),
                    None,
                    None,
                    None,
                    now,
                )
                .await?;
                self.finalize_restore_in(&txn, ctx, &restoring, &actor, now)
                    .await?
            }
        };
        txn.commit().await?;
        Ok(row)
    }

    /// The restoration prerequisites, each `true` / `false` (stored with the suspension).
    async fn restore_checks(&self, s: &SuspensionRow, standing: Standing) -> DomainResult<Value> {
        let db = self.numbers.db();
        let assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1",
            vec![s.assignment_id.into()],
        ))
        .one(db)
        .await?
        .ok_or(DomainError::NotFound("assignment"))?;
        let owner_current = assignment.status() == AssignmentStatus::Suspended
            && self
                .numbers
                .open_assignment_of(s.number_resource_id)
                .await?
                .is_some_and(|a| a.id == assignment.id);
        // KYC / eligibility basis: the admitting decision recorded, its KYC case in force.
        let kyc = count(
            db,
            "SELECT count(*) AS n FROM dcn_assignments a \
               LEFT JOIN number_eligibility_decisions d ON d.id = a.eligibility_decision_ref \
               LEFT JOIN number_kyc_cases k ON k.id = d.kyc_case_id \
             WHERE a.id = $1 AND a.eligibility_decision_ref IS NOT NULL \
               AND (d.kyc_case_id IS NULL OR (k.status = 'APPROVED' AND k.expires_at > $2))",
            vec![assignment.id.into(), self.now().into()],
        )
        .await?
            > 0;
        let risk_clear = !super::risk::restriction_in_force(db, s.number_resource_id).await?;
        let caps: Vec<Capability> = assignment
            .capability_snapshot
            .iter()
            .filter_map(|c| Capability::parse(c))
            .collect();
        let bindings = self.numbers.bindings(s.number_resource_id).await?;
        let bound = caps.iter().filter(|c| c.is_routable()).all(|c| {
            bindings
                .iter()
                .any(|b| b.status == "ACTIVE" && b.capabilities.iter().any(|x| x == c.as_str()))
        });
        let resolution = self
            .numbers
            .resolve_capabilities(s.number_resource_id)
            .await?;
        // The capability truth itself (a suspended number is "not active", which is the
        // very thing being restored; evidence, catalog and class must still hold).
        let usable = caps.iter().all(|c| resolution.get(*c).supported);
        let routes = self
            .numbers
            .activation_readiness(s.number_resource_id, &caps)
            .await?;
        let routes_ready = !routes.is_empty() && routes.iter().all(|r| r.ready);
        Ok(json!({
            "ownerCurrent": owner_current,
            "kycInForce": kyc,
            "standingActive": standing.active,
            "riskClear": risk_clear,
            "bindingActive": bound,
            "capabilitiesUsable": usable,
            "routesReady": routes_ready,
        }))
    }

    /// The assignment returns to ACTIVE (the number too when still SUSPENDED); the
    /// suspension is RESTORED; `number.restored` is published.
    async fn finalize_restore_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        s: &SuspensionRow,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> DomainResult<SuspensionRow> {
        let resource = lock(txn, s.number_resource_id).await?;
        let assignment = self.lock_any(txn, s.assignment_id).await?;
        if assignment.status() != AssignmentStatus::Suspended {
            return Err(DomainError::precondition(
                "ASSIGNMENT_NOT_SUSPENDED",
                "the suspended assignment is no longer current",
            ));
        }
        match resource.lifecycle_state() {
            LifecycleState::Suspended => {
                transition_in(
                    txn,
                    ctx,
                    resource.id,
                    TransitionRequest::new(
                        LifecycleState::Active,
                        resource.revision,
                        actor.clone(),
                        "NUMBER_RESTORED",
                    ),
                    now,
                )
                .await?;
            }
            LifecycleState::Active => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    entity: "number resource",
                    from: other.as_str().into(),
                    to: "ACTIVE".into(),
                })
            }
        }
        self.numbers
            .step_assignment_in(
                txn,
                ctx,
                &assignment,
                AssignmentStatus::Active,
                None,
                actor,
                "NUMBER_RESTORED",
                now,
            )
            .await?;
        let row = update(
            txn,
            s,
            "RESTORED",
            &s.provider_state,
            None,
            None,
            None,
            None,
            Some(now),
            None,
            now,
        )
        .await?;
        event(txn, ctx, &row, true, now).await?;
        crate::metrics::inc("numbering_restorations_total", &[]);
        Ok(row)
    }

    /// Advances one suspension from its provider command outcomes.
    pub async fn advance_suspension(
        &self,
        ctx: &RequestContext,
        id: Uuid,
    ) -> DomainResult<SuspensionRow> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let s = SuspensionRow::find_by_statement(stmt(
            "SELECT * FROM number_suspensions WHERE id = $1 FOR UPDATE",
            vec![id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("suspension"))?;
        let status_of = |cid: Option<Uuid>| {
            let p = self.providers.clone();
            async move {
                match (p, cid) {
                    (Some(p), Some(c)) => {
                        Ok::<_, DomainError>(Some(command_outcome(&p.command(c).await?.status)))
                    }
                    _ => Ok(None),
                }
            }
        };
        let out = match s.status.as_str() {
            "SUSPENDED" if matches!(s.provider_state.as_str(), "PENDING" | "UNKNOWN") => {
                let state = match status_of(s.provider_command_id).await? {
                    Some(CommandOutcome::Succeeded) => "CONFIRMED",
                    Some(CommandOutcome::Unsupported) => "UNSUPPORTED",
                    Some(CommandOutcome::Unknown) => "UNKNOWN",
                    Some(CommandOutcome::Failed) => {
                        // The provider did not disable its routes: CF Meet keeps the
                        // number suspended (fail closed) and an operator is told.
                        incidents::raise(
                            &txn,
                            incidents::Category::LifecycleSafety,
                            &format!("suspend-failed:{}", s.id),
                            incidents::Subject {
                                provider_id: None,
                                number_resource_id: Some(s.number_resource_id),
                                command_id: s.provider_command_id,
                            },
                            json!({ "reason": "PROVIDER_SUSPEND_FAILED" }),
                            now,
                        )
                        .await?;
                        "FAILED"
                    }
                    _ => &*s.provider_state,
                };
                if state == s.provider_state {
                    s
                } else {
                    update(
                        &txn,
                        &s,
                        "SUSPENDED",
                        state,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        now,
                    )
                    .await?
                }
            }
            "RESTORING" => match status_of(s.restore_command_id).await? {
                Some(CommandOutcome::Succeeded) | None => {
                    let actor = Actor::new(
                        s.restore_actor_kind
                            .as_deref()
                            .and_then(ActorKind::parse)
                            .unwrap_or(ActorKind::System),
                        s.restore_actor_ref
                            .clone()
                            .unwrap_or_else(|| "restore".into()),
                    );
                    // Routes must still be ready at the moment of restoration.
                    let a = self.lock_any(&txn, s.assignment_id).await?;
                    let caps: Vec<Capability> = a
                        .capability_snapshot
                        .iter()
                        .filter_map(|c| Capability::parse(c))
                        .collect();
                    let ready = self
                        .numbers
                        .activation_readiness(s.number_resource_id, &caps)
                        .await?
                        .iter()
                        .all(|r| r.ready);
                    if !ready {
                        update(
                            &txn,
                            &s,
                            "RESTORING",
                            &s.provider_state,
                            None,
                            None,
                            None,
                            Some("ROUTE_NOT_READY"),
                            None,
                            None,
                            now,
                        )
                        .await?
                    } else {
                        self.finalize_restore_in(&txn, ctx, &s, &actor, now).await?
                    }
                }
                Some(CommandOutcome::Failed) | Some(CommandOutcome::Unsupported) => {
                    update(
                        &txn,
                        &s,
                        "SUSPENDED",
                        &s.provider_state,
                        None,
                        None,
                        None,
                        Some("PROVIDER_RESTORE_FAILED"),
                        None,
                        None,
                        now,
                    )
                    .await?
                }
                _ => s,
            },
            _ => s,
        };
        txn.commit().await?;
        Ok(out)
    }

    /// Worker pass: advances every suspension waiting on a provider outcome.
    pub async fn advance_suspensions(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<usize> {
        let ids = super::IdRow::find_by_statement(stmt(
            "SELECT id FROM number_suspensions WHERE status = 'RESTORING' \
                OR (status = 'SUSPENDED' AND provider_state IN ('PENDING', 'UNKNOWN')) \
             ORDER BY updated_at LIMIT $1",
            vec![(limit.clamp(1, 500) as i64).into()],
        ))
        .all(self.numbers.db())
        .await?;
        let mut n = 0;
        for r in ids {
            if self.advance_suspension(ctx, r.id).await.is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

fn blocked_code(check: &str) -> &'static str {
    match check {
        "ownerCurrent" => "ASSIGNMENT_NOT_SUSPENDED",
        "kycInForce" => "KYC_NOT_IN_FORCE",
        "standingActive" => "STANDING_NOT_ACTIVE",
        "riskClear" => "RISK_RESTRICTED",
        "bindingActive" => "BINDING_REQUIRED",
        "capabilitiesUsable" => "CAPABILITY_NOT_USABLE",
        "routesReady" => "ROUTE_NOT_READY",
        _ => "RESTORE_CHECK_FAILED",
    }
}
