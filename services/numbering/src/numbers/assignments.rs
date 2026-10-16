//! DCN assignments (Part IV §D6–§D8): who owns a number and where it routes — a record
//! separate from the Number Resource.
//!
//! * The owner is in the TRUSTED scope (tenant / application / environment) and is a
//!   principal (PERSONAL / PROFESSIONAL) or an organization (every organizational class,
//!   including EMPLOYEE_DIRECT: a number routed to an employee stays organizational
//!   property). Ownership never changes on a record: a new owner is a new assignment, and
//!   the previous one stays in history.
//! * One open (not CLOSED) assignment per number.
//! * An assignment becomes ACTIVE only through REQUESTED → PENDING_ELIGIBILITY → ELIGIBLE
//!   (recorded eligibility decision) → RESERVED (a held reservation of its scope) →
//!   ACTIVATING (routing profile pinned, number provisioned) → ACTIVE (number ACTIVE with
//!   provider bindings covering every routable capability) — never because a number
//!   exists. Eligibility, KYC, provisioning and routing are satisfied by later workstreams;
//!   their prerequisites are enforced now.
//! * Every change appends `number_assignment_history` and an outbox event.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    lifecycle::{transition_in, TransitionRequest},
    reservations::ReservationRow,
    resources::lock,
    routing::RoutingProfileRow,
    stmt, Actor, ActorKind, AssignmentStatus, DomainError, DomainResult, EnvironmentKind,
    LifecycleState, NumberDomain, OwnerKind, OwnerScope, OwnershipClass, ReservationStatus,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
};

use AssignmentStatus as A;

/// The assignment edges (mirrored by `dcn_assignment_transition_allowed`).
pub fn allowed(from: AssignmentStatus, to: AssignmentStatus) -> bool {
    matches!(
        (from, to),
        (A::Requested, A::PendingEligibility | A::Closed)
            | (A::PendingEligibility, A::Eligible | A::Closed)
            | (A::Eligible, A::Reserved | A::Closed)
            | (A::Reserved, A::Activating | A::Closed)
            | (A::Activating, A::Active | A::Reserved | A::Closed)
            | (A::Active, A::Suspended | A::ReleaseRequested)
            | (A::Suspended, A::Active | A::ReleaseRequested)
            | (A::ReleaseRequested, A::RoutingRemoved)
            | (A::RoutingRemoved, A::Quarantined | A::Closed)
            | (A::Quarantined, A::Closed)
    )
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub reservation_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub owner_kind: String,
    pub owner_ref: Uuid,
    pub ownership_class: String,
    pub routing_profile_id: Option<Uuid>,
    pub routing_profile_revision: Option<i64>,
    pub capability_snapshot: Vec<String>,
    pub policy_snapshot_ref: Option<Uuid>,
    pub eligibility_decision_ref: Option<Uuid>,
    pub kyc_profile_ref: Option<Uuid>,
    pub risk_tier_snapshot: Option<String>,
    pub status: String,
    pub revision: i64,
    pub requested_at: DateTime<Utc>,
    pub eligible_at: Option<DateTime<Utc>>,
    pub activated_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub release_requested_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub close_reason: Option<String>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
}

impl AssignmentRow {
    pub fn status(&self) -> AssignmentStatus {
        AssignmentStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
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
pub struct HistoryRow {
    pub sequence: i64,
    pub assignment_id: Uuid,
    pub number_resource_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub event_kind: String,
    pub from_status: Option<String>,
    pub to_status: String,
    pub assignment_revision: i64,
    pub owner_kind: String,
    pub owner_ref: Uuid,
    pub ownership_class: String,
    pub routing_profile_id: Option<Uuid>,
    pub routing_profile_revision: Option<i64>,
    pub capability_snapshot: Vec<String>,
    pub eligibility_decision_ref: Option<Uuid>,
    pub kyc_profile_ref: Option<Uuid>,
    pub risk_tier: Option<String>,
    pub effective_from: DateTime<Utc>,
    pub actor_kind: String,
    pub actor_ref: String,
    pub reason_code: String,
    pub correlation_id: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewAssignment {
    pub number_resource_id: Uuid,
    pub ownership_class: OwnershipClass,
    /// A principal id (PERSONAL / PROFESSIONAL) or an opaque organization reference.
    pub owner_ref: Uuid,
    pub policy_snapshot_ref: Option<Uuid>,
    pub kyc_profile_ref: Option<Uuid>,
}

async fn lock_assignment(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<AssignmentRow> {
    AssignmentRow::find_by_statement(stmt(
        "SELECT * FROM dcn_assignments WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("assignment"))
}

#[allow(clippy::too_many_arguments)]
async fn append_history(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    row: &AssignmentRow,
    event_kind: &str,
    from: Option<AssignmentStatus>,
    actor: &Actor,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    super::types::reason_code(reason)?;
    txn.execute(stmt(
        "INSERT INTO number_assignment_history (id, assignment_id, number_resource_id, tenant_id, application_id, \
           environment_id, event_kind, from_status, to_status, assignment_revision, owner_kind, owner_ref, \
           ownership_class, routing_profile_id, routing_profile_revision, capability_snapshot, \
           eligibility_decision_ref, kyc_profile_ref, risk_tier, effective_from, actor_kind, actor_ref, reason_code, \
           correlation_id, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, \
           $22, $23, $24, $20)",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            row.number_resource_id.into(),
            row.tenant_id.into(),
            row.application_id.into(),
            row.environment_id.into(),
            event_kind.into(),
            from.map(|f| f.as_str().to_string()).into(),
            row.status.clone().into(),
            row.revision.into(),
            row.owner_kind.clone().into(),
            row.owner_ref.into(),
            row.ownership_class.clone().into(),
            row.routing_profile_id.into(),
            row.routing_profile_revision.into(),
            row.capability_snapshot.clone().into(),
            row.eligibility_decision_ref.into(),
            row.kyc_profile_ref.into(),
            row.risk_tier_snapshot.clone().into(),
            now.into(),
            actor.kind.as_str().into(),
            actor.reference.clone().into(),
            reason.into(),
            ctx.correlation_id.as_str().into(),
        ],
    ))
    .await?;
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.assignment.changed",
            aggregate_type: "ASSIGNMENT",
            aggregate_id: row.id,
            scope: Some(row.event_scope()),
            payload: json!({
                "assignment_id": row.id,
                "number_resource_id": row.number_resource_id,
                "event_kind": event_kind,
                "from_status": from.map(|f| f.as_str()),
                "to_status": row.status,
                "revision": row.revision,
                "ownership_class": row.ownership_class,
                "owner_kind": row.owner_kind,
                "routing_profile_id": row.routing_profile_id,
                "routing_profile_revision": row.routing_profile_revision,
                "reason_code": reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

/// Field changes that accompany a status change.
#[derive(Default)]
struct Fields {
    reservation_id: Option<Uuid>,
    eligibility_decision_ref: Option<Uuid>,
    close_reason: Option<String>,
}

/// The only status-changing path of an assignment (inside `txn`).
#[allow(clippy::too_many_arguments)]
async fn transition_assignment(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    current: &AssignmentRow,
    expected_revision: i64,
    to: AssignmentStatus,
    fields: Fields,
    actor: &Actor,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<AssignmentRow> {
    if current.revision != expected_revision {
        return Err(DomainError::VersionConflict("assignment"));
    }
    let from = current.status();
    if !allowed(from, to) {
        return Err(DomainError::InvalidTransition {
            entity: "assignment",
            from: from.as_str().into(),
            to: to.as_str().into(),
        });
    }
    let stamp = |s: AssignmentStatus| (to == s).then_some(now);
    txn.execute(stmt(
        "SELECT set_config('numbering.assignment', 'transition', true)",
        vec![],
    ))
    .await?;
    let row = AssignmentRow::find_by_statement(stmt(
        "UPDATE dcn_assignments SET status = $3, revision = revision + 1, updated_at = $4, \
           reservation_id = COALESCE($5, reservation_id), \
           eligibility_decision_ref = COALESCE($6, eligibility_decision_ref), \
           eligible_at = COALESCE(eligible_at, $7), activated_at = COALESCE($8, activated_at), \
           suspended_at = COALESCE($9, suspended_at), release_requested_at = COALESCE($10, release_requested_at), \
           closed_at = COALESCE($11, closed_at), close_reason = COALESCE($12, close_reason) \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            current.id.into(),
            expected_revision.into(),
            to.as_str().into(),
            now.into(),
            fields.reservation_id.into(),
            fields.eligibility_decision_ref.into(),
            stamp(A::Eligible).into(),
            stamp(A::Active).into(),
            stamp(A::Suspended).into(),
            stamp(A::ReleaseRequested).into(),
            stamp(A::Closed).into(),
            fields.close_reason.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("assignment"))?;
    txn.execute(stmt(
        "SELECT set_config('numbering.assignment', 'off', true)",
        vec![],
    ))
    .await?;
    append_history(
        txn,
        ctx,
        &row,
        "STATUS_CHANGED",
        Some(from),
        actor,
        reason,
        now,
    )
    .await?;
    crate::metrics::inc(
        "numbering_assignment_transitions_total",
        &[("to", to.as_str())],
    );
    Ok(row)
}

impl NumberDomain {
    /// Opens an assignment (REQUESTED) for the trusted scope.
    pub async fn request_assignment(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        new: NewAssignment,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let resource = lock(&txn, new.number_resource_id).await?;
        if !scope.may_use(resource.inventory()) {
            return Err(DomainError::precondition(
                "ENVIRONMENT_INVENTORY_MISMATCH",
                "SANDBOX / TEST environments use synthetic test inventory only; PRODUCTION never does",
            ));
        }
        if matches!(
            resource.lifecycle_state(),
            LifecycleState::Quarantined
                | LifecycleState::UnknownProviderState
                | LifecycleState::Released
                | LifecycleState::Failed
                | LifecycleState::Releasing
        ) {
            return Err(DomainError::precondition(
                "NOT_ALLOCATABLE",
                "the number cannot be assigned in its current state",
            ));
        }
        #[derive(FromQueryResult)]
        struct Cap {
            capability: String,
        }
        let capabilities: Vec<String> = Cap::find_by_statement(stmt(
            "SELECT capability FROM number_capabilities WHERE number_resource_id = $1 AND effective_to IS NULL \
               AND status IN ('VERIFIED', 'DECLARED') ORDER BY capability",
            vec![resource.id.into()],
        ))
        .all(&txn)
        .await?
        .into_iter()
        .map(|c| c.capability)
        .collect();
        let owner_kind: OwnerKind = new.ownership_class.owner_kind();
        let row = AssignmentRow::find_by_statement(stmt(
            "INSERT INTO dcn_assignments (id, number_resource_id, tenant_id, application_id, environment_id, \
               environment_kind, owner_kind, owner_ref, ownership_class, capability_snapshot, policy_snapshot_ref, \
               kyc_profile_ref, risk_tier_snapshot, status, revision, requested_at, created_by, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'REQUESTED', 1, $14, $15, $14) \
             RETURNING *",
            vec![
                Uuid::new_v4().into(),
                resource.id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                scope.environment_kind.as_str().into(),
                owner_kind.as_str().into(),
                new.owner_ref.into(),
                new.ownership_class.as_str().into(),
                capabilities.into(),
                new.policy_snapshot_ref.into(),
                new.kyc_profile_ref.into(),
                Some(resource.risk_tier.clone()).into(),
                now.into(),
                actor.reference.clone().into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        append_history(
            &txn,
            ctx,
            &row,
            "CREATED",
            None,
            &actor,
            "ASSIGNMENT_REQUESTED",
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    async fn simple_transition(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        to: AssignmentStatus,
        fields: Fields,
        reason: &str,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            to,
            fields,
            &actor,
            reason,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// REQUESTED → PENDING_ELIGIBILITY.
    pub async fn submit_for_eligibility(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
    ) -> DomainResult<AssignmentRow> {
        self.simple_transition(
            ctx,
            actor,
            id,
            expected_revision,
            A::PendingEligibility,
            Fields::default(),
            "ELIGIBILITY_REQUESTED",
        )
        .await
    }

    /// PENDING_ELIGIBILITY → ELIGIBLE with the eligibility decision that allowed it.
    pub async fn record_eligibility(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        decision_ref: Uuid,
    ) -> DomainResult<AssignmentRow> {
        // Only an ELIGIBLE decision of the assignment's own scope, still in force, for its
        // number (when the decision names one) admits it (Workstream F).
        let assignment = AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .ok_or(DomainError::NotFound("assignment"))?;
        let decision = super::eligibility::EligibilityDecisionRow::find_by_statement(stmt(
            "SELECT * FROM number_eligibility_decisions WHERE id = $1",
            vec![decision_ref.into()],
        ))
        .one(&self.db)
        .await?;
        let scope = OwnerScope {
            tenant_id: assignment.tenant_id,
            application_id: assignment.application_id,
            environment_id: assignment.environment_id,
            environment_kind: EnvironmentKind::parse(&assignment.environment_kind)
                .expect("CHECK-constrained kind"),
            service_account_id: Uuid::nil(),
        };
        let admitted = decision.as_ref().is_some_and(|d| {
            d.valid_for(&scope, self.clock.now())
                && d.number_resource_id
                    .is_none_or(|n| n == assignment.number_resource_id)
        });
        if !admitted {
            return Err(DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                "an ELIGIBLE decision of this scope and number, still in force, is required",
            ));
        }
        self.simple_transition(
            ctx,
            actor,
            id,
            expected_revision,
            A::Eligible,
            Fields {
                eligibility_decision_ref: Some(decision_ref),
                ..Default::default()
            },
            "ELIGIBILITY_GRANTED",
        )
        .await
    }

    /// ELIGIBLE → RESERVED: the assignment takes over a HELD reservation of its own scope
    /// and number (the reservation becomes CONVERTED).
    pub async fn attach_reservation(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        reservation_id: Uuid,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        let reservation = ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE id = $1 FOR UPDATE",
            vec![reservation_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|r| {
            r.tenant_id == current.tenant_id
                && r.application_id == current.application_id
                && r.environment_id == current.environment_id
                && r.number_resource_id == current.number_resource_id
        })
        .ok_or(DomainError::NotFound("reservation"))?;
        if reservation.status() != ReservationStatus::Held {
            return Err(DomainError::precondition(
                "HOLD_REQUIRED",
                "an assignment takes over a reservation only once its provider hold is confirmed",
            ));
        }
        if reservation.is_expired_at(now) {
            return Err(DomainError::precondition(
                "RESERVATION_EXPIRED",
                "the reservation has expired",
            ));
        }
        txn.execute(stmt(
            "UPDATE number_reservations SET status = 'CONVERTED', revision = revision + 1, updated_at = $2, \
               ended_at = $2 WHERE id = $1",
            vec![reservation.id.into(), now.into()],
        ))
        .await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            A::Reserved,
            Fields {
                reservation_id: Some(reservation.id),
                ..Default::default()
            },
            &actor,
            "RESERVATION_ATTACHED",
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Pins a CURRENT routing profile revision of the assignment's scope.
    pub async fn set_assignment_routing(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        profile_id: Uuid,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        if current.revision != expected_revision {
            return Err(DomainError::VersionConflict("assignment"));
        }
        if matches!(
            current.status(),
            A::Requested
                | A::PendingEligibility
                | A::RoutingRemoved
                | A::Quarantined
                | A::Closed
                | A::ReleaseRequested
        ) {
            return Err(DomainError::precondition(
                "ROUTING_NOT_ALLOWED",
                "routing is set between eligibility and release",
            ));
        }
        let profile = RoutingProfileRow::find_by_statement(stmt(
            "SELECT * FROM number_routing_profiles WHERE profile_id = $1 AND status = 'CURRENT'",
            vec![profile_id.into()],
        ))
        .one(&txn)
        .await?
        .filter(|p| {
            p.tenant_id == current.tenant_id
                && p.application_id == current.application_id
                && p.environment_id == current.environment_id
        })
        .ok_or(DomainError::NotFound("routing profile"))?;
        let row = AssignmentRow::find_by_statement(stmt(
            "UPDATE dcn_assignments SET routing_profile_id = $3, routing_profile_revision = $4, revision = revision + 1, \
               updated_at = $5 WHERE id = $1 AND revision = $2 RETURNING *",
            vec![
                id.into(),
                expected_revision.into(),
                profile.profile_id.into(),
                profile.revision.into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::VersionConflict("assignment"))?;
        txn.execute(stmt(
            "UPDATE number_resources SET routing_revision = routing_revision + 1, revision = revision + 1, \
               updated_at = $2 WHERE id = $1",
            vec![row.number_resource_id.into(), now.into()],
        ))
        .await?;
        let status = row.status();
        append_history(
            &txn,
            ctx,
            &row,
            "ROUTING_CHANGED",
            Some(status),
            &actor,
            "ROUTING_PROFILE_PINNED",
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// RESERVED → ACTIVATING: needs a pinned routing profile and a number being (or
    /// already) provisioned.
    pub async fn begin_activation(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        if current.routing_profile_id.is_none() {
            return Err(DomainError::precondition(
                "ROUTING_REQUIRED",
                "activation needs a routing profile",
            ));
        }
        let resource = lock(&txn, current.number_resource_id).await?;
        if !matches!(
            resource.lifecycle_state(),
            LifecycleState::Reserved | LifecycleState::Provisioning | LifecycleState::Provisioned
        ) {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "activation needs a reserved or provisioned number",
            ));
        }
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            A::Activating,
            Fields::default(),
            &actor,
            "ACTIVATION_STARTED",
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// ACTIVATING → ACTIVE together with the number PROVISIONED → ACTIVE (one transaction).
    pub async fn activate_assignment(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        if current.status() != A::Activating {
            return Err(DomainError::InvalidTransition {
                entity: "assignment",
                from: current.status.clone(),
                to: "ACTIVE".into(),
            });
        }
        if current.eligibility_decision_ref.is_none() {
            return Err(DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                "no eligibility decision is recorded",
            ));
        }
        let resource = lock(&txn, current.number_resource_id).await?;
        if resource.lifecycle_state() != LifecycleState::Provisioned {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "the number must be provisioned before it is activated",
            ));
        }
        transition_in(
            &txn,
            ctx,
            resource.id,
            TransitionRequest::new(
                LifecycleState::Active,
                resource.revision,
                actor.clone(),
                "ASSIGNMENT_ACTIVATED",
            ),
            now,
        )
        .await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            A::Active,
            Fields::default(),
            &actor,
            "ASSIGNMENT_ACTIVATED",
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// ACTIVE ↔ SUSPENDED for assignment and number together.
    pub async fn set_assignment_suspended(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        suspended: bool,
        reason: &str,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        let resource = lock(&txn, current.number_resource_id).await?;
        let (to, resource_to) = if suspended {
            (A::Suspended, LifecycleState::Suspended)
        } else {
            (A::Active, LifecycleState::Active)
        };
        if current.revision != expected_revision {
            return Err(DomainError::VersionConflict("assignment"));
        }
        // The number first: an assignment becomes ACTIVE only on an ACTIVE number.
        let mut request =
            TransitionRequest::new(resource_to, resource.revision, actor.clone(), reason);
        if suspended {
            request = request.with_suspension(reason);
        }
        transition_in(&txn, ctx, resource.id, request, now).await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            to,
            Fields::default(),
            &actor,
            reason,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// ACTIVE / SUSPENDED → RELEASE_REQUESTED.
    pub async fn request_release(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
    ) -> DomainResult<AssignmentRow> {
        self.simple_transition(
            ctx,
            actor,
            id,
            expected_revision,
            A::ReleaseRequested,
            Fields::default(),
            "RELEASE_REQUESTED",
        )
        .await
    }

    /// RELEASE_REQUESTED → ROUTING_REMOVED; the number starts RELEASING.
    pub async fn remove_routing(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            A::RoutingRemoved,
            Fields::default(),
            &actor,
            "ROUTING_REMOVED",
            now,
        )
        .await?;
        let resource = lock(&txn, row.number_resource_id).await?;
        if matches!(
            resource.lifecycle_state(),
            LifecycleState::Active | LifecycleState::Suspended
        ) {
            transition_in(
                &txn,
                ctx,
                resource.id,
                TransitionRequest::new(
                    LifecycleState::Releasing,
                    resource.revision,
                    actor,
                    "ASSIGNMENT_RELEASED",
                ),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(row)
    }

    /// ROUTING_REMOVED → QUARANTINED; the number is quarantined until `until` (cool-down
    /// before reuse).
    pub async fn quarantine_assignment(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        until: DateTime<Utc>,
    ) -> DomainResult<AssignmentRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_assignment(&txn, id).await?;
        let row = transition_assignment(
            &txn,
            ctx,
            &current,
            expected_revision,
            A::Quarantined,
            Fields::default(),
            &actor,
            "QUARANTINED",
            now,
        )
        .await?;
        let resource = lock(&txn, row.number_resource_id).await?;
        if resource.lifecycle_state() == LifecycleState::Releasing {
            transition_in(
                &txn,
                ctx,
                resource.id,
                TransitionRequest::new(
                    LifecycleState::Quarantined,
                    resource.revision,
                    actor,
                    "ASSIGNMENT_QUARANTINE",
                )
                .with_quarantine_until(until),
                now,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(row)
    }

    /// Closes an assignment (history is kept; the number can be reassigned later by a NEW
    /// assignment).
    pub async fn close_assignment(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        id: Uuid,
        expected_revision: i64,
        reason: &str,
    ) -> DomainResult<AssignmentRow> {
        super::types::reason_code(reason)?;
        self.simple_transition(
            ctx,
            actor,
            id,
            expected_revision,
            A::Closed,
            Fields {
                close_reason: Some(reason.to_string()),
                ..Default::default()
            },
            reason,
        )
        .await
    }

    /// An assignment of the caller's scope (other scopes: not found).
    pub async fn assignment(&self, scope: &OwnerScope, id: Uuid) -> DomainResult<AssignmentRow> {
        AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .filter(|a| a.in_scope(scope))
        .ok_or(DomainError::NotFound("assignment"))
    }

    /// The caller scope's assignments, newest first (bounded).
    pub async fn assignments(
        &self,
        scope: &OwnerScope,
        limit: u64,
    ) -> DomainResult<Vec<AssignmentRow>> {
        Ok(AssignmentRow::find_by_statement(stmt(
            "SELECT * FROM dcn_assignments WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             ORDER BY requested_at DESC, id LIMIT $4",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                (limit.clamp(1, 200) as i64).into(),
            ],
        ))
        .all(&self.db)
        .await?)
    }

    /// The history of one assignment of the caller's scope.
    pub async fn assignment_history(
        &self,
        scope: &OwnerScope,
        id: Uuid,
    ) -> DomainResult<Vec<HistoryRow>> {
        self.assignment(scope, id).await?;
        Ok(HistoryRow::find_by_statement(stmt(
            "SELECT * FROM number_assignment_history WHERE assignment_id = $1 ORDER BY sequence",
            vec![id.into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// Every owner's history of a number — privileged operational access only (an operator
    /// or reconciliation actor); customers see their own scope's assignments only.
    pub async fn resource_assignment_history(
        &self,
        actor: &Actor,
        resource_id: Uuid,
    ) -> DomainResult<Vec<HistoryRow>> {
        if !matches!(actor.kind, ActorKind::Operator | ActorKind::Reconciliation) {
            return Err(DomainError::precondition(
                "PRIVILEGED_ACCESS_REQUIRED",
                "cross-owner history is restricted to operators",
            ));
        }
        Ok(HistoryRow::find_by_statement(stmt(
            "SELECT * FROM number_assignment_history WHERE number_resource_id = $1 ORDER BY sequence",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_is_terminal_and_activation_has_a_single_entry() {
        for to in AssignmentStatus::ALL {
            assert!(!allowed(A::Closed, *to), "{to}");
        }
        let into_active: Vec<_> = AssignmentStatus::ALL
            .iter()
            .filter(|s| allowed(**s, A::Active))
            .collect();
        assert_eq!(into_active, vec![&A::Activating, &A::Suspended]);
        assert!(!allowed(A::Requested, A::Active) && !allowed(A::Eligible, A::Active));
    }

    #[test]
    fn organizational_classes_stay_organizational() {
        for class in OwnershipClass::ALL {
            let expected = matches!(
                class,
                OwnershipClass::Personal | OwnershipClass::Professional
            );
            assert_eq!(
                class.owner_kind() == OwnerKind::Principal,
                expected,
                "{class}"
            );
        }
        assert_eq!(
            OwnershipClass::EmployeeDirect.owner_kind(),
            OwnerKind::Organization
        );
    }
}
