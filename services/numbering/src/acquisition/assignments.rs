//! Ownership and assignment (Part IV Workstream J §144).
//!
//! An assignment binds an acquired (or held) number to its LEGAL OWNER and to where its
//! traffic routes, under the ownership policy of [`crate::numbers::ownership`]:
//!
//! * **Owner from trusted context.** Organizational classes belong to the authenticated
//!   tenant; a personal / professional number to a principal Platform Access resolves as
//!   ACTIVE in the caller's environment. An employee's direct line routes to an employee
//!   resolved the same way. A request never names a tenant.
//! * **Eligibility bound to the owner.** The decision must be the caller's, of the
//!   requested ownership class and — for a personal / professional number — about the
//!   owner; it is RE-EVALUATED (KYC, risk, limits, capabilities, provider) at creation, at
//!   activation and at a class change. A stale or revoked basis never assigns.
//! * **Lifecycle.** Creation runs REQUESTED → PENDING_ELIGIBILITY → ELIGIBLE → RESERVED
//!   in one transaction (the hold is taken over, the routing profile pinned). ACTIVATING →
//!   ACTIVE happens only when the number is provisioned and EVERY routed family is READY
//!   — provider route configured on the current binding and acknowledged by its consumer
//!   (Telephony / SMS Gateway) — never because a number exists.
//! * **Routing changes are revisions.** A target change supersedes the profile revision
//!   and advances the number's routing revision (routing grants bound to the old revision
//!   stop verifying — Workstream K). Employee offboarding removes or replaces the
//!   employee's route; the number stays the organization's.
//! * **Transfers keep the legal owner.** Same class: a routing revision. Another class of
//!   the same owner: the active assignment is CLOSED (TRANSFERRED_INTERNALLY) and a
//!   successor opens, admitted and activated in the same transaction (database-linked
//!   predecessor / successor, pool membership carried). A different legal owner is
//!   refused (`CROSS_OWNER_TRANSFER_UNSUPPORTED`) before anything changes.

use sea_orm::{FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
    reservations::{begin_idempotent, customer_actor},
    Acquisition,
};
use crate::{
    domain::RequestContext,
    identity::PrincipalStatus,
    infra::idempotency,
    numbers::{
        acquisitions::{AcquisitionStatus, FamilyReadiness},
        assignments::{AssignmentRow, OpenAssignment},
        eligibility::Standing,
        ownership::{self, TransferKind},
        routing::RoutingTargets,
        stmt, AssignmentStatus, Capability, DomainError, DomainResult, LifecycleState, OwnerKind,
        OwnerScope, OwnershipClass, ReservationStatus, SmsTargetKind, VoiceTargetKind,
    },
};

/// Where a new assignment's number comes from.
#[derive(Debug, Clone, Copy)]
pub enum AssignmentBasis {
    /// A PROVISIONED acquisition of the caller (its reservation is linked).
    Acquisition(Uuid),
    /// A HELD reservation of the caller (provisioned afterwards on the assignment basis).
    Reservation(Uuid),
}

#[derive(Debug, Clone)]
pub struct AssignmentRequest {
    pub basis: AssignmentBasis,
    pub ownership_class: OwnershipClass,
    /// The owning principal (PERSONAL / PROFESSIONAL only).
    pub owner_principal_id: Option<Uuid>,
    pub targets: RoutingTargets,
    /// The routable capabilities the assignment relies on.
    pub capabilities: Vec<Capability>,
    pub eligibility_decision_id: Option<Uuid>,
    /// An organizational pool to join (organization-owned only).
    pub pool_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct Assigned {
    pub assignment: AssignmentRow,
    pub replayed: bool,
}

#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub ownership_class: OwnershipClass,
    /// The owning principal of a personal / professional target class (defaults to the
    /// current owner; anyone else is refused).
    pub owner_principal_id: Option<Uuid>,
    pub targets: RoutingTargets,
    /// Required for a class change (eligibility of the new class).
    pub eligibility_decision_id: Option<Uuid>,
    /// The routing profile revision the caller saw.
    pub expected_routing_revision: i64,
}

#[derive(Debug, Clone)]
pub struct Transferred {
    pub kind: TransferKind,
    /// The assignment now carrying the number (the successor for a class change).
    pub assignment: AssignmentRow,
    pub predecessor: Option<AssignmentRow>,
    pub replayed: bool,
}

fn capability_names(caps: &[Capability]) -> Vec<String> {
    caps.iter().map(|c| c.as_str().to_string()).collect()
}

fn snapshot_capabilities(a: &AssignmentRow) -> Vec<Capability> {
    a.capability_snapshot
        .iter()
        .filter_map(|c| Capability::parse(c))
        .collect()
}

fn normalize(caps: &[Capability]) -> DomainResult<Vec<Capability>> {
    if caps.is_empty()
        || caps.len() > Capability::ALL.len()
        || caps.iter().any(|c| !c.is_routable())
    {
        return Err(DomainError::Validation(
            "an assignment names the routable capabilities it relies on".into(),
        ));
    }
    let mut caps = caps.to_vec();
    caps.sort();
    caps.dedup();
    Ok(caps)
}

fn not_ready(routes: &[FamilyReadiness]) -> DomainError {
    let detail: Vec<String> = routes
        .iter()
        .filter(|r| !r.ready)
        .map(|r| format!("{}: {}", r.family.as_str(), r.reason.unwrap_or("NOT_READY")))
        .collect();
    DomainError::precondition(
        "ROUTING_NOT_READY",
        format!(
            "every routed family must be configured and acknowledged by its consumer ({})",
            if detail.is_empty() {
                "no routed family".to_string()
            } else {
                detail.join(", ")
            }
        ),
    )
}

impl Acquisition {
    /// The principal is ACTIVE in the caller's environment (Platform Access). Anything
    /// else — including an unreachable directory — refuses.
    async fn require_active_principal(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> DomainResult<()> {
        match self.principals.resolve(scope, principal_id).await {
            Ok(PrincipalStatus::Active) => Ok(()),
            Ok(PrincipalStatus::Inactive) => Err(DomainError::precondition(
                "PRINCIPAL_NOT_ACTIVE",
                "the principal is not active",
            )),
            Ok(PrincipalStatus::NotFound) => Err(DomainError::precondition(
                "PRINCIPAL_NOT_FOUND",
                "no such principal in this environment",
            )),
            Err(e) => {
                tracing::warn!(error = %e.0, "principal resolution unavailable");
                Err(DomainError::precondition(
                    "PRINCIPAL_UNVERIFIED",
                    "the principal could not be verified",
                ))
            }
        }
    }

    /// The legal owner of `class` for the caller: the tenant for organizational classes,
    /// the (resolved, active) principal for personal / professional ones.
    async fn resolve_owner(
        &self,
        scope: &OwnerScope,
        class: OwnershipClass,
        principal: Option<Uuid>,
    ) -> DomainResult<Uuid> {
        match (class.owner_kind(), principal) {
            (OwnerKind::Organization, None) => Ok(scope.tenant_id),
            (OwnerKind::Organization, Some(_)) => Err(DomainError::Validation(
                "an organizational number belongs to the authenticated organization; it names no owner"
                    .into(),
            )),
            (OwnerKind::Principal, None) => Err(DomainError::Validation(
                "a personal or professional number names its owning principal".into(),
            )),
            (OwnerKind::Principal, Some(p)) => {
                self.require_active_principal(scope, p).await?;
                Ok(p)
            }
        }
    }

    /// Validates targets for the class and owner, resolving any routed principal.
    async fn check_targets(
        &self,
        scope: &OwnerScope,
        class: OwnershipClass,
        owner_ref: Uuid,
        targets: &RoutingTargets,
    ) -> DomainResult<()> {
        ownership::validate_targets(class, owner_ref, targets)?;
        if class == OwnershipClass::EmployeeDirect {
            if let Some(employee) = ownership::routed_principal(class, owner_ref, targets) {
                self.require_active_principal(scope, employee).await?;
            }
        }
        Ok(())
    }

    /// The decision basis: the caller's, of `class`, about `owner_ref` for an individual
    /// class — then re-evaluated for the number and `caps`.
    #[allow(clippy::too_many_arguments)]
    async fn eligibility_for(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        decision_id: Uuid,
        class: OwnershipClass,
        owner_ref: Uuid,
        resource_id: Uuid,
        caps: &[Capability],
    ) -> DomainResult<Uuid> {
        let base = self
            .numbers
            .eligibility_decision(scope, decision_id)
            .await
            .map_err(|_| {
                DomainError::precondition("ELIGIBILITY_REQUIRED", "no such decision in this scope")
            })?;
        if base.ownership_class != class.as_str()
            || (class.owner_kind() == OwnerKind::Principal && base.subject_ref != owner_ref)
            || base.number_resource_id.is_some_and(|r| r != resource_id)
        {
            return Err(DomainError::precondition(
                "ELIGIBILITY_SUBJECT_MISMATCH",
                "the eligibility decision is for another owner, class or number",
            ));
        }
        Ok(self
            .reevaluate_eligibility(ctx, scope, standing, decision_id, resource_id, caps)
            .await?
            .id)
    }

    /// Creates an assignment (see the module rules): RESERVED on success, with its
    /// routing profile pinned. `idempotency_key` is required.
    pub async fn create_assignment(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        request: AssignmentRequest,
        idempotency_key: &str,
    ) -> DomainResult<Assigned> {
        let caps = normalize(&request.capabilities)?;
        let (basis_kind, basis_id) = match request.basis {
            AssignmentBasis::Acquisition(id) => ("acquisition", id),
            AssignmentBasis::Reservation(id) => ("reservation", id),
        };
        let fingerprint = idempotency::fingerprint(&[
            b"assignment.create",
            basis_kind.as_bytes(),
            basis_id.as_bytes(),
            request.ownership_class.as_str().as_bytes(),
            request
                .owner_principal_id
                .map(|p| p.to_string())
                .unwrap_or_default()
                .as_bytes(),
            format!("{:?}", request.targets).as_bytes(),
            capability_names(&caps).join(",").as_bytes(),
            request
                .eligibility_decision_id
                .map(|d| d.to_string())
                .unwrap_or_default()
                .as_bytes(),
            request
                .pool_id
                .map(|p| p.to_string())
                .unwrap_or_default()
                .as_bytes(),
        ]);
        if let Some(done) = self
            .replay(
                scope,
                "numbers.assignment.create",
                idempotency_key,
                &fingerprint,
            )
            .await?
        {
            return Ok(Assigned {
                assignment: done,
                replayed: true,
            });
        }

        // The basis: the caller's reservation that (still) holds the number.
        let (reservation, served, basis_decision) = match request.basis {
            AssignmentBasis::Acquisition(id) => {
                let acquisition = self.numbers.acquisition(scope, id).await?;
                if acquisition.status() != AcquisitionStatus::Provisioned {
                    return Err(DomainError::precondition(
                        "PROVISIONING_REQUIRED",
                        "an acquisition is assigned once it is provisioned",
                    ));
                }
                let reservation = self
                    .numbers
                    .reservation(scope, acquisition.reservation_id)
                    .await?;
                let served = acquisition.capabilities();
                (
                    reservation,
                    Some(served),
                    Some(acquisition.eligibility_decision_ref),
                )
            }
            AssignmentBasis::Reservation(id) => {
                let reservation = self.numbers.reservation(scope, id).await?;
                if reservation.status() != ReservationStatus::Held {
                    return Err(DomainError::precondition(
                        "HOLD_REQUIRED",
                        "only a held reservation (or a provisioned acquisition) is assigned",
                    ));
                }
                #[derive(FromQueryResult)]
                struct InFlight {
                    #[allow(dead_code)]
                    id: Uuid,
                }
                if InFlight::find_by_statement(stmt(
                    "SELECT id FROM number_acquisitions WHERE reservation_id = $1 \
                       AND status IN ('PROVISIONING', 'UNKNOWN')",
                    vec![reservation.id.into()],
                ))
                .one(self.numbers.db())
                .await?
                .is_some()
                {
                    return Err(DomainError::conflict(
                        "ACQUISITION_IN_PROGRESS",
                        "the reservation is being provisioned; assign the acquisition once it is provisioned",
                    ));
                }
                let decision = reservation.eligibility_decision_ref;
                (reservation, None, decision)
            }
        };
        let resource_id = reservation.number_resource_id;
        if let Some(served) = &served {
            if let Some(c) = caps.iter().find(|c| !served.contains(c)) {
                return Err(DomainError::precondition(
                    "CAPABILITY_NOT_SUPPORTED",
                    format!("{c} was not provisioned for this number"),
                ));
            }
        } else {
            let resolution = self.numbers.resolve_capabilities(resource_id).await?;
            if let Some(c) = caps.iter().find(|c| !resolution.get(**c).supported) {
                return Err(DomainError::precondition(
                    "CAPABILITY_NOT_SUPPORTED",
                    format!("{c} is not supported for this number"),
                ));
            }
        }
        let class = request.ownership_class;
        let owner_ref = self
            .resolve_owner(scope, class, request.owner_principal_id)
            .await?;
        self.check_targets(scope, class, owner_ref, &request.targets)
            .await?;
        if request.pool_id.is_some() && class.owner_kind() != OwnerKind::Organization {
            return Err(DomainError::precondition(
                "POOL_MEMBER_NOT_ORGANIZATIONAL",
                "only organization-owned numbers join a pool",
            ));
        }
        let decision_id = request
            .eligibility_decision_id
            .or(basis_decision)
            .ok_or_else(|| {
                DomainError::precondition(
                    "ELIGIBILITY_REQUIRED",
                    "an assignment requires an eligibility decision",
                )
            })?;
        let decision = self
            .eligibility_for(
                ctx,
                scope,
                standing,
                decision_id,
                class,
                owner_ref,
                resource_id,
                &caps,
            )
            .await?;

        let now = self.now();
        let actor = customer_actor(scope);
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.assignment.create",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(existing) => {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("assignment"))?;
                return Ok(Assigned {
                    assignment: self.numbers.assignment(scope, id).await?,
                    replayed: true,
                });
            }
        };
        let opened = self
            .numbers
            .open_assignment_in(
                &txn,
                ctx,
                scope,
                &actor,
                OpenAssignment {
                    id: Uuid::new_v4(),
                    number_resource_id: resource_id,
                    ownership_class: class,
                    owner_ref,
                    capability_snapshot: capability_names(&caps),
                    policy_snapshot_ref: None,
                    kyc_profile_ref: None,
                    predecessor_assignment_id: None,
                },
                "ASSIGNMENT_REQUESTED",
                now,
            )
            .await
            .map_err(|e| match e {
                DomainError::Conflict { .. } => DomainError::conflict(
                    "ASSIGNMENT_EXISTS",
                    "the number already has an open assignment",
                ),
                other => other,
            })?;
        let admitted = self
            .numbers
            .admit_in(&txn, ctx, &actor, &opened, decision, now)
            .await?;
        let reserved = self
            .numbers
            .hold_for_assignment_in(&txn, ctx, &actor, &admitted, reservation.id, now)
            .await?;
        let profile = self
            .numbers
            .create_routing_profile_in(&txn, ctx, scope, &actor, request.targets, now)
            .await?;
        let pinned = self
            .numbers
            .pin_routing_in(
                &txn,
                ctx,
                &actor,
                &reserved,
                profile.profile_id,
                "ROUTING_ASSIGNED",
                now,
            )
            .await?;
        if let Some(pool) = request.pool_id {
            self.numbers
                .add_pool_member_in(&txn, ctx, scope, &actor, pool, &pinned, now)
                .await?;
        }
        idempotency::complete(&txn, record, 201, None, Some(pinned.id), now).await?;
        txn.commit().await?;
        Ok(Assigned {
            assignment: pinned,
            replayed: false,
        })
    }

    /// A completed earlier request with the same key returns its assignment.
    async fn replay(
        &self,
        scope: &OwnerScope,
        operation: &str,
        key: &str,
        fingerprint: &str,
    ) -> DomainResult<Option<AssignmentRow>> {
        let txn = self.numbers.db().begin().await?;
        let outcome =
            begin_idempotent(&txn, scope, operation, key, fingerprint, self.now()).await?;
        match outcome {
            Ok(_) => {
                txn.rollback().await?;
                Ok(None)
            }
            Err(existing) => {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("assignment"))?;
                Ok(Some(self.numbers.assignment(scope, id).await?))
            }
        }
    }

    /// Per routed family of the assignment's capabilities: is it READY.
    pub async fn assignment_readiness(
        &self,
        assignment: &AssignmentRow,
    ) -> DomainResult<Vec<FamilyReadiness>> {
        self.numbers
            .activation_readiness(
                assignment.number_resource_id,
                &snapshot_capabilities(assignment),
            )
            .await
    }

    /// Activates a RESERVED (or ACTIVATING) assignment: eligibility re-evaluated, the
    /// number provisioned and every routed family READY. An ACTIVE assignment is returned
    /// as is (`replayed`).
    pub async fn activate_assignment(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        assignment_id: Uuid,
    ) -> DomainResult<Assigned> {
        let assignment = self.numbers.assignment(scope, assignment_id).await?;
        match assignment.status() {
            AssignmentStatus::Active => {
                return Ok(Assigned {
                    assignment,
                    replayed: true,
                })
            }
            AssignmentStatus::Reserved | AssignmentStatus::Activating => {}
            other => {
                return Err(DomainError::InvalidTransition {
                    entity: "assignment",
                    from: other.as_str().into(),
                    to: "ACTIVE".into(),
                })
            }
        }
        let resource = self.numbers.resource(assignment.number_resource_id).await?;
        if resource.lifecycle_state() != LifecycleState::Provisioned {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "the number must be provisioned before it is activated",
            ));
        }
        let caps = snapshot_capabilities(&assignment);
        let decision = assignment.eligibility_decision_ref.ok_or_else(|| {
            DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                "no eligibility decision is recorded",
            )
        })?;
        let class = OwnershipClass::parse(&assignment.ownership_class)
            .ok_or_else(|| DomainError::Validation("corrupt ownership class".into()))?;
        self.eligibility_for(
            ctx,
            scope,
            standing,
            decision,
            class,
            assignment.owner_ref,
            assignment.number_resource_id,
            &caps,
        )
        .await?;
        if class.owner_kind() == OwnerKind::Principal {
            self.require_active_principal(scope, assignment.owner_ref)
                .await?;
        }
        let routes = self.assignment_readiness(&assignment).await?;
        if routes.is_empty() || routes.iter().any(|r| !r.ready) {
            return Err(not_ready(&routes));
        }
        let now = self.now();
        let actor = customer_actor(scope);
        let txn = self.numbers.db().begin().await?;
        let locked = self
            .numbers
            .lock_assignment_in(&txn, scope, assignment_id)
            .await?;
        if locked.revision != assignment.revision {
            return Err(DomainError::VersionConflict("assignment"));
        }
        let active = self
            .numbers
            .activate_in(&txn, ctx, &actor, &locked, "ASSIGNMENT_ACTIVATED", now)
            .await?;
        txn.commit().await?;
        Ok(Assigned {
            assignment: active,
            replayed: false,
        })
    }

    /// Changes where an open assignment routes (same class, same owner): a new routing
    /// profile revision pinned on the assignment.
    pub async fn update_routing(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        assignment_id: Uuid,
        expected_routing_revision: i64,
        targets: RoutingTargets,
        reason: &'static str,
    ) -> DomainResult<AssignmentRow> {
        let assignment = self.numbers.assignment(scope, assignment_id).await?;
        if !matches!(
            assignment.status(),
            AssignmentStatus::Reserved
                | AssignmentStatus::Activating
                | AssignmentStatus::Active
                | AssignmentStatus::Suspended
        ) {
            return Err(DomainError::precondition(
                "ASSIGNMENT_NOT_ROUTABLE",
                "routing is changed on a reserved, activating, active or suspended assignment",
            ));
        }
        let class = OwnershipClass::parse(&assignment.ownership_class)
            .ok_or_else(|| DomainError::Validation("corrupt ownership class".into()))?;
        self.check_targets(scope, class, assignment.owner_ref, &targets)
            .await?;
        let profile_id = assignment.routing_profile_id.ok_or_else(|| {
            DomainError::precondition("ROUTING_REQUIRED", "the assignment has no routing profile")
        })?;
        let now = self.now();
        let actor = customer_actor(scope);
        let txn = self.numbers.db().begin().await?;
        let locked = self
            .numbers
            .lock_assignment_in(&txn, scope, assignment_id)
            .await?;
        if locked.revision != assignment.revision {
            return Err(DomainError::VersionConflict("assignment"));
        }
        self.numbers
            .revise_routing_profile_in(
                &txn,
                ctx,
                scope,
                &actor,
                profile_id,
                expected_routing_revision,
                targets,
                now,
            )
            .await?;
        let row = self
            .numbers
            .pin_routing_in(&txn, ctx, &actor, &locked, profile_id, reason, now)
            .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Employee offboarding: an EMPLOYEE_DIRECT number stops routing to the departing
    /// employee — unrouted (or to voicemail), or to a replacement employee. The number
    /// stays the organization's; grants bound to the old routing revision stop verifying.
    pub async fn offboard_employee(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        assignment_id: Uuid,
        expected_routing_revision: i64,
        replacement: Option<RoutingTargets>,
    ) -> DomainResult<AssignmentRow> {
        let assignment = self.numbers.assignment(scope, assignment_id).await?;
        if assignment.ownership_class != OwnershipClass::EmployeeDirect.as_str() {
            return Err(DomainError::precondition(
                "NOT_AN_EMPLOYEE_NUMBER",
                "offboarding applies to an employee's direct number",
            ));
        }
        let departing = match (
            assignment.routing_profile_id,
            assignment.routing_profile_revision,
        ) {
            (Some(p), Some(r)) => {
                let current = self.numbers.routing_profile_revision(p, r).await?;
                [current.voice_target_ref, current.sms_target_ref]
                    .into_iter()
                    .flatten()
                    .next()
            }
            _ => None,
        };
        let targets = replacement.unwrap_or(RoutingTargets {
            voice: (VoiceTargetKind::None, None),
            sms: (SmsTargetKind::None, None),
        });
        if let Some(next) = ownership::routed_principal(
            OwnershipClass::EmployeeDirect,
            assignment.owner_ref,
            &targets,
        ) {
            if Some(next) == departing {
                return Err(DomainError::precondition(
                    "TARGET_NOT_PERMITTED",
                    "offboarding moves the number away from the departing employee",
                ));
            }
        }
        self.update_routing(
            ctx,
            scope,
            assignment_id,
            expected_routing_revision,
            targets,
            "EMPLOYEE_OFFBOARDED",
        )
        .await
    }

    /// Transfers an ACTIVE assignment within its legal owner (see the module rules).
    /// `idempotency_key` is required.
    pub async fn transfer_assignment(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        assignment_id: Uuid,
        request: TransferRequest,
        idempotency_key: &str,
    ) -> DomainResult<Transferred> {
        let current = self.numbers.assignment(scope, assignment_id).await?;
        let from = OwnershipClass::parse(&current.ownership_class)
            .ok_or_else(|| DomainError::Validation("corrupt ownership class".into()))?;
        let to = request.ownership_class;
        // The target owner: a request can only restate the current owner.
        let to_owner = match to.owner_kind() {
            OwnerKind::Organization => {
                if request.owner_principal_id.is_some() {
                    return Err(DomainError::Validation(
                        "an organizational number names no owner".into(),
                    ));
                }
                scope.tenant_id
            }
            OwnerKind::Principal => request.owner_principal_id.unwrap_or(current.owner_ref),
        };
        let kind = ownership::classify_transfer(from, current.owner_ref, to, to_owner)?;
        let fingerprint = idempotency::fingerprint(&[
            b"assignment.transfer",
            assignment_id.as_bytes(),
            to.as_str().as_bytes(),
            format!("{:?}", request.targets).as_bytes(),
            request
                .eligibility_decision_id
                .map(|d| d.to_string())
                .unwrap_or_default()
                .as_bytes(),
            request.expected_routing_revision.to_string().as_bytes(),
        ]);
        if let Some(done) = self
            .replay(
                scope,
                "numbers.assignment.transfer",
                idempotency_key,
                &fingerprint,
            )
            .await?
        {
            let predecessor = match done.predecessor_assignment_id {
                Some(p) if done.id != assignment_id => {
                    Some(self.numbers.assignment(scope, p).await?)
                }
                _ => None,
            };
            return Ok(Transferred {
                kind,
                assignment: done,
                predecessor,
                replayed: true,
            });
        }
        if current.status() != AssignmentStatus::Active {
            return Err(DomainError::precondition(
                "ASSIGNMENT_NOT_ACTIVE",
                "only an active assignment is transferred",
            ));
        }
        if kind == TransferKind::Retarget {
            let row = self
                .update_routing(
                    ctx,
                    scope,
                    assignment_id,
                    request.expected_routing_revision,
                    request.targets,
                    "TRANSFERRED_INTERNALLY",
                )
                .await?;
            self.record_transfer(scope, idempotency_key, &fingerprint, row.id)
                .await?;
            return Ok(Transferred {
                kind,
                assignment: row,
                predecessor: None,
                replayed: false,
            });
        }

        // Reclassification: a successor assignment of the same legal owner.
        if to.owner_kind() == OwnerKind::Principal {
            self.require_active_principal(scope, to_owner).await?;
        }
        self.check_targets(scope, to, to_owner, &request.targets)
            .await?;
        if current.routing_profile_revision != Some(request.expected_routing_revision) {
            return Err(DomainError::VersionConflict("routing profile"));
        }
        let caps = snapshot_capabilities(&current);
        let decision_id = request.eligibility_decision_id.ok_or_else(|| {
            DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                "a class change requires an eligibility decision for the new class",
            )
        })?;
        let decision = self
            .eligibility_for(
                ctx,
                scope,
                standing,
                decision_id,
                to,
                to_owner,
                current.number_resource_id,
                &caps,
            )
            .await?;
        let routes = self.assignment_readiness(&current).await?;
        if routes.is_empty() || routes.iter().any(|r| !r.ready) {
            return Err(not_ready(&routes));
        }
        let reservation_id = current.reservation_id.ok_or_else(|| {
            DomainError::precondition("HOLD_REQUIRED", "the assignment holds no reservation")
        })?;

        let now = self.now();
        let actor = customer_actor(scope);
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.assignment.transfer",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(_) => {
                txn.rollback().await?;
                return Err(DomainError::conflict(
                    "REQUEST_IN_PROGRESS",
                    "a request with this idempotency key completed concurrently; retry to read it",
                ));
            }
        };
        let locked = self
            .numbers
            .lock_assignment_in(&txn, scope, assignment_id)
            .await?;
        if locked.revision != current.revision {
            return Err(DomainError::VersionConflict("assignment"));
        }
        let successor_id = Uuid::new_v4();
        let closed = self
            .numbers
            .close_for_transfer_in(&txn, ctx, &actor, &locked, successor_id, now)
            .await?;
        let opened = self
            .numbers
            .open_assignment_in(
                &txn,
                ctx,
                scope,
                &actor,
                OpenAssignment {
                    id: successor_id,
                    number_resource_id: current.number_resource_id,
                    ownership_class: to,
                    owner_ref: to_owner,
                    capability_snapshot: current.capability_snapshot.clone(),
                    policy_snapshot_ref: current.policy_snapshot_ref,
                    kyc_profile_ref: current.kyc_profile_ref,
                    predecessor_assignment_id: Some(current.id),
                },
                "TRANSFERRED_INTERNALLY",
                now,
            )
            .await?;
        let admitted = self
            .numbers
            .admit_in(&txn, ctx, &actor, &opened, decision, now)
            .await?;
        let reserved = self
            .numbers
            .hold_for_assignment_in(&txn, ctx, &actor, &admitted, reservation_id, now)
            .await?;
        let profile = self
            .numbers
            .create_routing_profile_in(&txn, ctx, scope, &actor, request.targets, now)
            .await?;
        let pinned = self
            .numbers
            .pin_routing_in(
                &txn,
                ctx,
                &actor,
                &reserved,
                profile.profile_id,
                "TRANSFERRED_INTERNALLY",
                now,
            )
            .await?;
        let active = self
            .numbers
            .activate_in(&txn, ctx, &actor, &pinned, "TRANSFERRED_INTERNALLY", now)
            .await?;
        self.numbers
            .transfer_pool_membership_in(&txn, ctx, scope, &actor, &closed, &active, now)
            .await?;
        idempotency::complete(&txn, record, 200, None, Some(active.id), now).await?;
        txn.commit().await?;
        Ok(Transferred {
            kind,
            assignment: active,
            predecessor: Some(closed),
            replayed: false,
        })
    }

    async fn record_transfer(
        &self,
        scope: &OwnerScope,
        key: &str,
        fingerprint: &str,
        assignment_id: Uuid,
    ) -> DomainResult<()> {
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        if let Ok(record) = begin_idempotent(
            &txn,
            scope,
            "numbers.assignment.transfer",
            key,
            fingerprint,
            now,
        )
        .await?
        {
            idempotency::complete(&txn, record, 200, None, Some(assignment_id), now).await?;
        }
        txn.commit().await?;
        Ok(())
    }
}

/// A new owner for a number whose quarantine completed (Workstream Q reassignment).
#[derive(Debug, Clone)]
pub struct ReassignmentRequest {
    pub number_resource_id: Uuid,
    pub ownership_class: OwnershipClass,
    pub owner_principal_id: Option<Uuid>,
    pub targets: RoutingTargets,
    pub capabilities: Vec<Capability>,
    pub eligibility_decision_id: Uuid,
    pub pool_id: Option<Uuid>,
}

impl Acquisition {
    /// Reassigns a REASSIGNABLE number (a POOL_RETURN quarantine that COMPLETED with the
    /// provider's confirmation) to a new owner: a fresh reservation confirmed by a fresh
    /// provider read-back (CF Meet still holds the number — no provider hold is taken), a
    /// NEW assignment with the new owner's eligibility, a new routing profile (the number's
    /// routing revision advances) and NEW provider route configurations that the consumers
    /// must acknowledge again before activation. Nothing of the previous owner (routes,
    /// grants, voicemail / inbox / webhook targets, external references) carries over.
    pub async fn reassign(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        request: ReassignmentRequest,
        idempotency_key: &str,
    ) -> DomainResult<Assigned> {
        let commands = self.commands()?;
        let caps = normalize(&request.capabilities)?;
        let fingerprint = idempotency::fingerprint(&[
            b"assignment.reassign",
            request.number_resource_id.as_bytes(),
            request.ownership_class.as_str().as_bytes(),
            request
                .owner_principal_id
                .map(|p| p.to_string())
                .unwrap_or_default()
                .as_bytes(),
            format!("{:?}", request.targets).as_bytes(),
            capability_names(&caps).join(",").as_bytes(),
            request.eligibility_decision_id.as_bytes(),
        ]);
        if let Some(done) = self
            .replay(
                scope,
                "numbers.assignment.reassign",
                idempotency_key,
                &fingerprint,
            )
            .await?
        {
            return Ok(Assigned {
                assignment: done,
                replayed: true,
            });
        }
        let resource = self.numbers.resource(request.number_resource_id).await?;
        if !scope.may_use(resource.inventory()) || resource.service_class != "DCN" {
            return Err(DomainError::NotFound("number resource"));
        }
        let refuse = || {
            DomainError::precondition(
                "NOT_ALLOCATABLE",
                "the number is not available for reassignment",
            )
        };
        if resource.lifecycle_state() != LifecycleState::Reassignable {
            return Err(refuse());
        }
        // The gate: a completed, provider-confirmed POOL_RETURN quarantine; no open
        // release; no unsettled provider command.
        #[derive(FromQueryResult)]
        struct Gate {
            quarantine_ok: i64,
            open_release: i64,
            unsettled: i64,
        }
        let gate = Gate::find_by_statement(stmt(
            "SELECT \
               (SELECT count(*) FROM (SELECT * FROM number_quarantines WHERE number_resource_id = $1 \
                   ORDER BY started_at DESC LIMIT 1) q \
                 WHERE q.status = 'COMPLETED' AND q.disposition = 'POOL_RETURN' AND q.provider_confirmed_at IS NOT NULL) AS quarantine_ok, \
               (SELECT count(*) FROM number_release_requests WHERE number_resource_id = $1 \
                 AND status NOT IN ('QUARANTINED', 'FAILED', 'CANCELLED')) AS open_release, \
               (SELECT count(*) FROM number_provider_commands WHERE number_resource_id = $1 \
                 AND status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')) AS unsettled",
            vec![resource.id.into()],
        ))
        .one(self.numbers.db())
        .await?
        .ok_or_else(refuse)?;
        if gate.quarantine_ok != 1 || gate.open_release > 0 || gate.unsettled > 0 {
            return Err(refuse());
        }
        // The provider's own, fresh word that CF Meet still controls the number.
        let state = match commands.inspect_resource(resource.id).await? {
            Some(crate::providers::ProviderOutcome::Succeeded(s)) => s,
            _ => {
                return Err(DomainError::precondition(
                    "PROVIDER_UNAVAILABLE",
                    "the provider state could not be confirmed",
                ))
            }
        };
        if !state.owned_by_us || state.status != crate::providers::ProviderNumberStatus::Provisioned
        {
            return Err(refuse());
        }
        let risk = crate::stewardship::risk::evaluate_allocation_in(
            self.numbers.db(),
            ctx,
            &self.settings.risk,
            scope,
            &crate::stewardship::risk::AllocationRequest {
                country: resource.country_code.clone(),
                ownership_class: Some(request.ownership_class),
            },
            self.now(),
        )
        .await?;
        if risk.outcome != "ALLOW" {
            return Err(DomainError::precondition(
                if risk.outcome == "DENY" {
                    "RISK_DENIED"
                } else {
                    "RISK_REVIEW_REQUIRED"
                },
                format!(
                    "reassignment refused by the risk policy: {}",
                    risk.reasons.join(", ")
                ),
            ));
        }
        let class = request.ownership_class;
        let owner_ref = self
            .resolve_owner(scope, class, request.owner_principal_id)
            .await?;
        self.check_targets(scope, class, owner_ref, &request.targets)
            .await?;
        let decision = self
            .eligibility_for(
                ctx,
                scope,
                standing,
                request.eligibility_decision_id,
                class,
                owner_ref,
                resource.id,
                &caps,
            )
            .await?;
        let bindings = self.numbers.bindings(resource.id).await?;
        if let Some(c) = caps.iter().find(|c| {
            !bindings
                .iter()
                .any(|b| b.status == "ACTIVE" && b.capabilities.iter().any(|x| x == c.as_str()))
        }) {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_SUPPORTED",
                format!("{c} is not served by the number's provider binding"),
            ));
        }
        let now = self.now();
        let actor = customer_actor(scope);
        // A fresh observation from the read-back (reservation needs fresh inventory).
        let mut observation = crate::numbers::inventory::NewObservation::offered(
            resource.inventory(),
            now,
            chrono::Duration::minutes(15),
        );
        observation.offered_capabilities = caps.clone();
        self.numbers.observe(ctx, resource.id, observation).await?;
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.assignment.reassign",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(existing) => {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("assignment"))?;
                return Ok(Assigned {
                    assignment: self.numbers.assignment(scope, id).await?,
                    replayed: true,
                });
            }
        };
        let reservation = self
            .numbers
            .reserve_in(
                &txn,
                ctx,
                scope,
                actor.clone(),
                resource.id,
                self.settings.reservation_default_ttl,
                crate::numbers::reservations::ReservationBasis {
                    basis: crate::numbers::reservations::HoldBasis::EligibilityDecision,
                    eligibility_decision_ref: Some(decision),
                    capabilities: caps.clone(),
                    for_principal: request.owner_principal_id,
                },
                now,
            )
            .await?;
        let evidence = Uuid::new_v4();
        self.numbers
            .confirm_hold_in(&txn, ctx, actor.clone(), reservation.id, evidence, now)
            .await?;
        let opened = self
            .numbers
            .open_assignment_in(
                &txn,
                ctx,
                scope,
                &actor,
                OpenAssignment {
                    id: Uuid::new_v4(),
                    number_resource_id: resource.id,
                    ownership_class: class,
                    owner_ref,
                    capability_snapshot: capability_names(&caps),
                    policy_snapshot_ref: None,
                    kyc_profile_ref: None,
                    predecessor_assignment_id: None,
                },
                "REASSIGNMENT_REQUESTED",
                now,
            )
            .await?;
        let admitted = self
            .numbers
            .admit_in(&txn, ctx, &actor, &opened, decision, now)
            .await?;
        let reserved = self
            .numbers
            .hold_for_assignment_in(&txn, ctx, &actor, &admitted, reservation.id, now)
            .await?;
        let profile = self
            .numbers
            .create_routing_profile_in(&txn, ctx, scope, &actor, request.targets, now)
            .await?;
        let pinned = self
            .numbers
            .pin_routing_in(
                &txn,
                ctx,
                &actor,
                &reserved,
                profile.profile_id,
                "ROUTING_ASSIGNED",
                now,
            )
            .await?;
        if let Some(pool) = request.pool_id {
            self.numbers
                .add_pool_member_in(&txn, ctx, scope, &actor, pool, &pinned, now)
                .await?;
        }
        // Still provisioned at the provider: RESERVED → PROVISIONING → PROVISIONED on the
        // read-back evidence (no provider provisioning is repeated).
        for to in [LifecycleState::Provisioning, LifecycleState::Provisioned] {
            let r = crate::numbers::resources::lock(&txn, resource.id).await?;
            crate::numbers::lifecycle::transition_in(
                &txn,
                ctx,
                r.id,
                crate::numbers::lifecycle::TransitionRequest::new(
                    to,
                    r.revision,
                    actor.clone(),
                    "REASSIGNMENT_PROVIDER_CONFIRMED",
                )
                .with_evidence(evidence),
                now,
            )
            .await?;
        }
        // New route configurations: the consumers acknowledge them afresh.
        let mut families: Vec<crate::numbers::acquisitions::RouteFamily> = caps
            .iter()
            .filter_map(|c| crate::numbers::acquisitions::RouteFamily::of(*c))
            .collect();
        families.sort();
        families.dedup();
        for family in families {
            let binding = bindings
                .iter()
                .find(|b| {
                    b.status == "ACTIVE"
                        && b.capabilities
                            .iter()
                            .any(|c| family.capabilities().iter().any(|f| f.as_str() == c))
                })
                .expect("checked above");
            let route = self
                .numbers
                .request_route_configuration_in(&txn, ctx, resource.id, family, binding, now)
                .await?;
            let submitted = commands
                .request_route_in(
                    &txn,
                    ctx,
                    resource.id,
                    family == crate::numbers::acquisitions::RouteFamily::Voice,
                    route.route_ref,
                    &format!("route:{}", route.id.simple()),
                )
                .await?;
            self.numbers
                .attach_route_command_in(&txn, &route, submitted.command.id, now)
                .await?;
        }
        idempotency::complete(&txn, record, 201, None, Some(pinned.id), now).await?;
        txn.commit().await?;
        crate::metrics::inc("numbering_reassignments_total", &[]);
        Ok(Assigned {
            assignment: pinned,
            replayed: false,
        })
    }
}
