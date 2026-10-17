//! Reservations (Part IV §D5): the transactional primitives of allocation.
//!
//! * A reservation belongs to the TRUSTED scope that made it (tenant / application /
//!   environment / service account from Platform Access); other scopes cannot see it.
//! * Only AVAILABLE / REASSIGNABLE resources with a fresh observation can be reserved;
//!   quarantined or uncertain resources never are (also database-enforced). One number has
//!   at most one current reservation (unique index; concurrent attempts serialize on the
//!   resource row and exactly one wins).
//! * A reservation is PENDING until a provider hold is confirmed (HELD): an inventory
//!   observation is not an allocation.
//! * Expiry is exclusive at `expires_at`. An expired (or cancelled) reservation does NOT
//!   imply the provider released its hold: a held live number stays RESERVED with its
//!   release PENDING until release evidence is recorded; a live PENDING reservation whose
//!   hold request may have reached the provider becomes UNKNOWN. Synthetic inventory needs
//!   no provider release.
//!
//! Provider reservation execution and the expiry worker are later workstreams; these are
//! the domain operations they will call.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    lifecycle::{allocatable, transition_in, TransitionRequest},
    resources::{lock, ResourceRow},
    stmt, Actor, DomainError, DomainResult, InventorySource, LifecycleState, NumberDomain,
    OwnerScope, ReleaseState, ReservationStatus, UncertainOperation,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, NewEvent},
};

pub const MIN_TTL_SECONDS: i64 = 60;
pub const MAX_TTL_SECONDS: i64 = 24 * 3600;

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReservationRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub service_account_id: Uuid,
    pub requested_for_principal_id: Option<Uuid>,
    pub status: String,
    pub revision: i64,
    pub expires_at: DateTime<Utc>,
    pub provider_hold_ref: Option<Uuid>,
    pub provider_release_state: String,
    pub release_evidence_ref: Option<Uuid>,
    pub correlation_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Why the hold exists (`DIRECT`, `ELIGIBILITY_DECISION`, `PRE_ELIGIBILITY_HOLD`).
    pub hold_basis: String,
    pub eligibility_decision_ref: Option<Uuid>,
    pub requested_capabilities: Vec<String>,
    /// What a CONVERTED reservation became (`ASSIGNMENT`, `ACQUISITION`).
    pub converted_to: Option<String>,
}

text_enum!(
    /// Why a reservation holds a number (Workstream H): a domain-level hold (D primitives,
    /// operators, fixtures), a recorded ELIGIBLE decision of the reserving scope, or a
    /// temporary hold before eligibility where policy permits it (never in PRODUCTION;
    /// provisioning always requires eligibility).
    HoldBasis {
        Direct = "DIRECT",
        EligibilityDecision = "ELIGIBILITY_DECISION",
        PreEligibilityHold = "PRE_ELIGIBILITY_HOLD",
    }
);

/// The recorded basis of a new reservation.
#[derive(Debug, Clone)]
pub struct ReservationBasis {
    pub basis: HoldBasis,
    pub eligibility_decision_ref: Option<Uuid>,
    pub capabilities: Vec<super::Capability>,
    pub for_principal: Option<Uuid>,
}

impl ReservationBasis {
    pub fn direct(for_principal: Option<Uuid>) -> Self {
        Self {
            basis: HoldBasis::Direct,
            eligibility_decision_ref: None,
            capabilities: vec![],
            for_principal,
        }
    }
}

impl ReservationRow {
    pub fn status(&self) -> ReservationStatus {
        ReservationStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    /// Expired at `now` (exclusive bound: expired exactly at `expires_at`).
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

async fn lock_reservation(txn: &DatabaseTransaction, id: Uuid) -> DomainResult<ReservationRow> {
    ReservationRow::find_by_statement(stmt(
        "SELECT * FROM number_reservations WHERE id = $1 FOR UPDATE",
        vec![id.into()],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::NotFound("reservation"))
}

/// The state a resource returns to when its allocation is given back: the state it was
/// reserved from (AVAILABLE or REASSIGNABLE), from the lifecycle audit.
async fn origin_state(
    txn: &DatabaseTransaction,
    resource_id: Uuid,
) -> DomainResult<LifecycleState> {
    #[derive(FromQueryResult)]
    struct From {
        from_state: Option<String>,
    }
    let from = From::find_by_statement(stmt(
        "SELECT from_state FROM number_lifecycle_events WHERE number_resource_id = $1 AND to_state = 'RESERVING' \
         ORDER BY sequence DESC LIMIT 1",
        vec![resource_id.into()],
    ))
    .one(txn)
    .await?
    .and_then(|f| f.from_state)
    .and_then(|s| LifecycleState::parse(&s));
    Ok(match from {
        Some(LifecycleState::Reassignable) => LifecycleState::Reassignable,
        _ => LifecycleState::Available,
    })
}

struct Change<'a> {
    status: ReservationStatus,
    release: Option<ReleaseState>,
    hold_ref: Option<Uuid>,
    evidence_ref: Option<Uuid>,
    reason: &'a str,
}

async fn update_reservation(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    current: &ReservationRow,
    change: Change<'_>,
    now: DateTime<Utc>,
) -> DomainResult<ReservationRow> {
    let ends = !change.status.is_current();
    let row = ReservationRow::find_by_statement(stmt(
        "UPDATE number_reservations SET status = $3, revision = revision + 1, updated_at = $4, \
           ended_at = CASE WHEN $5 THEN COALESCE(ended_at, $4) ELSE ended_at END, \
           provider_release_state = COALESCE($6, provider_release_state), \
           provider_hold_ref = COALESCE($7, provider_hold_ref), \
           release_evidence_ref = COALESCE($8, release_evidence_ref) \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            current.id.into(),
            current.revision.into(),
            change.status.as_str().into(),
            now.into(),
            ends.into(),
            change.release.map(|r| r.as_str().to_string()).into(),
            change.hold_ref.into(),
            change.evidence_ref.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("reservation"))?;
    event(txn, ctx, &row, change.reason, now).await?;
    Ok(row)
}

async fn event(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    r: &ReservationRow,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        txn,
        NewEvent {
            event_type: "number.reservation.changed",
            aggregate_type: "RESERVATION",
            aggregate_id: r.id,
            scope: Some(crate::infra::outbox::EventScope {
                tenant_id: r.tenant_id,
                application_id: r.application_id,
                environment_id: r.environment_id,
            }),
            payload: json!({
                "reservation_id": r.id,
                "number_resource_id": r.number_resource_id,
                "status": r.status,
                "revision": r.revision,
                "expires_at": r.expires_at,
                "provider_release_state": r.provider_release_state,
                "reason_code": reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

fn refuse_unallocatable(row: &ResourceRow) -> DomainError {
    match row.lifecycle_state() {
        LifecycleState::Reserving | LifecycleState::Reserved => DomainError::conflict(
            "RESERVATION_EXISTS",
            "the number already has a current reservation",
        ),
        LifecycleState::UnknownProviderState => DomainError::precondition(
            "RECONCILIATION_REQUIRED",
            "the number's provider state is uncertain and must be reconciled first",
        ),
        LifecycleState::Quarantined => {
            DomainError::precondition("NOT_ALLOCATABLE", "the number is quarantined")
        }
        _ => DomainError::precondition(
            "NOT_ALLOCATABLE",
            "the number is not available for allocation",
        ),
    }
}

impl NumberDomain {
    /// Reserves a number for the trusted scope (PENDING until a provider hold).
    pub async fn reserve(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        resource_id: Uuid,
        ttl: Duration,
        for_principal: Option<Uuid>,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let out = self
            .reserve_in(
                &txn,
                ctx,
                scope,
                actor,
                resource_id,
                ttl,
                ReservationBasis::direct(for_principal),
                now,
            )
            .await?;
        txn.commit().await?;
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn reserve_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        resource_id: Uuid,
        ttl: Duration,
        basis: ReservationBasis,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl.num_seconds()) {
            return Err(DomainError::Validation(
                "a reservation lasts 1 minute to 24 hours".into(),
            ));
        }
        let row = lock(txn, resource_id).await?;
        if !scope.may_use(row.inventory()) {
            return Err(DomainError::precondition(
                "ENVIRONMENT_INVENTORY_MISMATCH",
                "SANDBOX / TEST environments use synthetic test inventory only; PRODUCTION never does",
            ));
        }
        if !allocatable(row.lifecycle_state()) {
            return Err(refuse_unallocatable(&row));
        }
        if !row.is_fresh(now) {
            return Err(DomainError::precondition(
                "INVENTORY_NOT_FRESH",
                "the number's latest observation has expired; observe it again before reserving",
            ));
        }
        if basis.capabilities.iter().any(|c| !c.is_routable()) {
            return Err(DomainError::Validation(
                "a reservation names routable capabilities only".into(),
            ));
        }
        let mut capabilities: Vec<String> = basis
            .capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        capabilities.sort();
        capabilities.dedup();
        let reservation = ReservationRow::find_by_statement(stmt(
            "INSERT INTO number_reservations (id, number_resource_id, tenant_id, application_id, environment_id, \
               environment_kind, service_account_id, requested_for_principal_id, status, revision, expires_at, \
               correlation_id, created_at, updated_at, hold_basis, eligibility_decision_ref, requested_capabilities) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'PENDING', 1, $9, $10, $11, $11, $12, $13, $14) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                resource_id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                scope.environment_kind.as_str().into(),
                scope.service_account_id.into(),
                basis.for_principal.into(),
                (now + ttl).into(),
                ctx.correlation_id.as_str().into(),
                now.into(),
                basis.basis.as_str().into(),
                basis.eligibility_decision_ref.into(),
                capabilities.into(),
            ],
        ))
        .one(txn)
        .await?
        .expect("insert returns the row");
        transition_in(
            txn,
            ctx,
            resource_id,
            TransitionRequest::new(
                LifecycleState::Reserving,
                row.revision,
                actor,
                "RESERVATION_REQUESTED",
            ),
            now,
        )
        .await?;
        event(txn, ctx, &reservation, "RESERVATION_REQUESTED", now).await?;
        Ok(reservation)
    }

    /// Records the confirmed provider hold (evidence ref): PENDING → HELD, resource
    /// RESERVING → RESERVED.
    pub async fn confirm_hold(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        hold_ref: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let out = self
            .confirm_hold_in(&txn, ctx, actor, reservation_id, hold_ref, now)
            .await?;
        txn.commit().await?;
        Ok(out)
    }

    pub(crate) async fn confirm_hold_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        hold_ref: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        let resource = lock(txn, current.number_resource_id).await?;
        if current.status() != ReservationStatus::Pending {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: "HELD".into(),
            });
        }
        if current.is_expired_at(now) {
            return Err(DomainError::precondition(
                "RESERVATION_EXPIRED",
                "the reservation has expired",
            ));
        }
        let held = update_reservation(
            txn,
            ctx,
            &current,
            Change {
                status: ReservationStatus::Held,
                release: None,
                hold_ref: Some(hold_ref),
                evidence_ref: None,
                reason: "PROVIDER_HOLD_CONFIRMED",
            },
            now,
        )
        .await?;
        transition_in(
            txn,
            ctx,
            resource.id,
            TransitionRequest::new(
                LifecycleState::Reserved,
                resource.revision,
                actor,
                "PROVIDER_HOLD_CONFIRMED",
            )
            .with_evidence(hold_ref),
            now,
        )
        .await?;
        Ok(held)
    }

    /// The provider's answer to the hold request is unknown: the reservation is UNKNOWN and
    /// the resource UNKNOWN_PROVIDER_STATE until reconciliation.
    pub async fn mark_reservation_unknown(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let out = self
            .mark_reservation_unknown_in(&txn, ctx, actor, reservation_id, now)
            .await?;
        txn.commit().await?;
        Ok(out)
    }

    pub(crate) async fn mark_reservation_unknown_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        let resource = lock(txn, current.number_resource_id).await?;
        if current.status() != ReservationStatus::Pending {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: "UNKNOWN".into(),
            });
        }
        let unknown = update_reservation(
            txn,
            ctx,
            &current,
            Change {
                status: ReservationStatus::Unknown,
                release: Some(ReleaseState::Unknown),
                hold_ref: None,
                evidence_ref: None,
                reason: "PROVIDER_OUTCOME_UNKNOWN",
            },
            now,
        )
        .await?;
        transition_in(
            txn,
            ctx,
            resource.id,
            TransitionRequest::new(
                LifecycleState::UnknownProviderState,
                resource.revision,
                actor,
                "PROVIDER_OUTCOME_UNKNOWN",
            )
            .uncertain(UncertainOperation::Reserve),
            now,
        )
        .await?;
        Ok(unknown)
    }

    /// Reconciliation's answer for an UNKNOWN reservation (inside `txn`): the hold
    /// happened (HELD, with the read-back evidence as hold reference) or it did not
    /// (FAILED, nothing to release). The resource is resolved separately by the caller.
    pub(crate) async fn resolve_unknown_reservation_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation_id: Uuid,
        held: bool,
        evidence_ref: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        if current.status() != ReservationStatus::Unknown {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: if held { "HELD" } else { "FAILED" }.into(),
            });
        }
        update_reservation(
            txn,
            ctx,
            &current,
            if held {
                Change {
                    status: ReservationStatus::Held,
                    release: Some(ReleaseState::NotStarted),
                    hold_ref: Some(evidence_ref),
                    evidence_ref: None,
                    reason: "RECONCILED_HOLD_CONFIRMED",
                }
            } else {
                Change {
                    status: ReservationStatus::Failed,
                    release: Some(ReleaseState::NotRequired),
                    hold_ref: None,
                    evidence_ref: None,
                    reason: "RECONCILED_HOLD_ABSENT",
                }
            },
            now,
        )
        .await
    }

    /// The provider definitively refused the hold: FAILED, the resource returns to where
    /// it was reserved from.
    pub async fn fail_reservation(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let out = self
            .fail_reservation_in(&txn, ctx, actor, reservation_id, now)
            .await?;
        txn.commit().await?;
        Ok(out)
    }

    pub(crate) async fn fail_reservation_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        let resource = lock(txn, current.number_resource_id).await?;
        if current.status() != ReservationStatus::Pending {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: "FAILED".into(),
            });
        }
        let failed = update_reservation(
            txn,
            ctx,
            &current,
            Change {
                status: ReservationStatus::Failed,
                release: Some(ReleaseState::NotRequired),
                hold_ref: None,
                evidence_ref: None,
                reason: "PROVIDER_HOLD_REFUSED",
            },
            now,
        )
        .await?;
        let origin = origin_state(txn, resource.id).await?;
        transition_in(
            txn,
            ctx,
            resource.id,
            TransitionRequest::new(origin, resource.revision, actor, "PROVIDER_HOLD_REFUSED"),
            now,
        )
        .await?;
        Ok(failed)
    }

    /// Ends a current reservation (expiry or cancellation) WITHOUT assuming the provider
    /// released anything.
    #[allow(clippy::too_many_arguments)]
    async fn end_reservation(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        current: ReservationRow,
        txn: DatabaseTransaction,
        to: ReservationStatus,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let resource = lock(&txn, current.number_resource_id).await?;
        let synthetic = resource.inventory() == InventorySource::SyntheticTest;
        let (release, resource_to) = match (current.status(), synthetic) {
            (ReservationStatus::Pending | ReservationStatus::Held, true) => (
                ReleaseState::NotRequired,
                Some(origin_state(&txn, resource.id).await?),
            ),
            // A held live number stays RESERVED until the provider release is evidenced.
            (ReservationStatus::Held, false) => (ReleaseState::Pending, None),
            // The hold request may have reached the provider: uncertain, never assumed.
            (ReservationStatus::Pending, false) => (
                ReleaseState::Unknown,
                Some(LifecycleState::UnknownProviderState),
            ),
            (status, _) => {
                return Err(DomainError::InvalidTransition {
                    entity: "reservation",
                    from: status.as_str().into(),
                    to: to.as_str().into(),
                })
            }
        };
        let ended = update_reservation(
            &txn,
            ctx,
            &current,
            Change {
                status: to,
                release: Some(release),
                hold_ref: None,
                evidence_ref: None,
                reason,
            },
            now,
        )
        .await?;
        if let Some(state) = resource_to {
            let mut request = TransitionRequest::new(state, resource.revision, actor, reason);
            if state == LifecycleState::UnknownProviderState {
                request = request.uncertain(UncertainOperation::Reserve);
            }
            transition_in(&txn, ctx, resource.id, request, now).await?;
        }
        txn.commit().await?;
        Ok(ended)
    }

    /// Expires a reservation at or after its `expires_at` (refused before).
    pub async fn expire_reservation(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_reservation(&txn, reservation_id).await?;
        if !current.is_expired_at(now) {
            return Err(DomainError::precondition(
                "NOT_EXPIRED",
                "the reservation has not expired yet",
            ));
        }
        self.end_reservation(
            ctx,
            actor,
            current,
            txn,
            ReservationStatus::Expired,
            "RESERVATION_EXPIRED",
            now,
        )
        .await
    }

    /// Cancels a current reservation of the caller's own scope.
    pub async fn cancel_reservation(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        actor: Actor,
        reservation_id: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let current = lock_reservation(&txn, reservation_id).await?;
        if !current.in_scope(scope) {
            return Err(DomainError::NotFound("reservation"));
        }
        self.end_reservation(
            ctx,
            actor,
            current,
            txn,
            ReservationStatus::Cancelled,
            "RESERVATION_CANCELLED",
            now,
        )
        .await
    }

    /// Records the provider's release evidence of an ended reservation's hold; the
    /// resource returns to inventory.
    pub async fn confirm_reservation_release(
        &self,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        evidence_ref: Uuid,
    ) -> DomainResult<ReservationRow> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let row = self
            .confirm_reservation_release_in(&txn, ctx, actor, reservation_id, evidence_ref, now)
            .await?;
        txn.commit().await?;
        Ok(row)
    }

    /// See [`Self::confirm_reservation_release`] (inside `txn`). An UNKNOWN release that
    /// read-back proved is confirmed the same way.
    pub(crate) async fn confirm_reservation_release_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        actor: Actor,
        reservation_id: Uuid,
        evidence_ref: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        let resource = lock(txn, current.number_resource_id).await?;
        let ended = matches!(
            current.status(),
            ReservationStatus::Expired | ReservationStatus::Cancelled
        );
        let awaiting = matches!(
            ReleaseState::parse(&current.provider_release_state),
            Some(ReleaseState::Pending | ReleaseState::Unknown)
        );
        if !ended || !awaiting {
            return Err(DomainError::precondition(
                "RELEASE_NOT_PENDING",
                "only an ended reservation awaiting its provider release can be confirmed",
            ));
        }
        // Final reservations change only their release bookkeeping.
        let row = ReservationRow::find_by_statement(stmt(
            "UPDATE number_reservations SET provider_release_state = 'CONFIRMED', release_evidence_ref = $3, \
               revision = revision + 1, updated_at = $4 WHERE id = $1 AND revision = $2 RETURNING *",
            vec![current.id.into(), current.revision.into(), evidence_ref.into(), now.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("reservation"))?;
        event(txn, ctx, &row, "PROVIDER_RELEASE_CONFIRMED", now).await?;
        if matches!(
            resource.lifecycle_state(),
            LifecycleState::Reserved | LifecycleState::Reserving
        ) {
            let origin = origin_state(txn, resource.id).await?;
            transition_in(
                txn,
                ctx,
                resource.id,
                TransitionRequest::new(
                    origin,
                    resource.revision,
                    actor,
                    "PROVIDER_RELEASE_CONFIRMED",
                )
                .with_evidence(evidence_ref),
                now,
            )
            .await?;
        }
        Ok(row)
    }

    /// A reservation of the caller's scope (other scopes: not found).
    pub async fn reservation(&self, scope: &OwnerScope, id: Uuid) -> DomainResult<ReservationRow> {
        ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .filter(|r| r.in_scope(scope))
        .ok_or(DomainError::NotFound("reservation"))
    }
}

/// Reservation operations of the acquisition workflows (Workstreams H–I): ending a
/// reservation whose provider hold must be given back, late hold confirmations, release
/// bookkeeping, conversion into a provisioned acquisition, listing and sweeps.
impl NumberDomain {
    /// Ends a HELD reservation (EXPIRED / CANCELLED) whose provider hold must be released
    /// through a `RELEASE_HOLD` command: the release is PENDING and the number stays
    /// RESERVED until the provider's release is evidenced (synthetic inventory included —
    /// the deterministic provider holds it too).
    pub(crate) async fn end_for_release_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation_id: Uuid,
        to: ReservationStatus,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        lock(txn, current.number_resource_id).await?;
        if current.status() != ReservationStatus::Held
            || !matches!(
                to,
                ReservationStatus::Expired | ReservationStatus::Cancelled
            )
        {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: to.as_str().into(),
            });
        }
        update_reservation(
            txn,
            ctx,
            &current,
            Change {
                status: to,
                release: Some(ReleaseState::Pending),
                hold_ref: None,
                evidence_ref: None,
                reason,
            },
            now,
        )
        .await
    }

    /// The provider confirmed a hold whose reservation had already expired: the hold
    /// exists and is recorded (EXPIRED, release PENDING), so it is given back, never
    /// silently kept or forgotten. The number stays RESERVING until the release.
    pub(crate) async fn record_late_hold_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation_id: Uuid,
        hold_ref: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        lock(txn, current.number_resource_id).await?;
        if current.status() != ReservationStatus::Pending || !current.is_expired_at(now) {
            return Err(DomainError::precondition(
                "RELEASE_NOT_PENDING",
                "only an expired pending reservation records a late hold",
            ));
        }
        update_reservation(
            txn,
            ctx,
            &current,
            Change {
                status: ReservationStatus::Expired,
                release: Some(ReleaseState::Pending),
                hold_ref: Some(hold_ref),
                evidence_ref: None,
                reason: "LATE_HOLD_RELEASE_REQUIRED",
            },
            now,
        )
        .await
    }

    /// Release bookkeeping of an ended reservation (`PENDING` / `UNKNOWN`): a release
    /// whose outcome is unknown is never assumed to have happened.
    pub(crate) async fn set_release_state_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation_id: Uuid,
        state: ReleaseState,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        if current.status().is_current()
            || !matches!(state, ReleaseState::Pending | ReleaseState::Unknown)
        {
            return Err(DomainError::precondition(
                "RELEASE_NOT_PENDING",
                "release bookkeeping applies to ended reservations only",
            ));
        }
        if current.provider_release_state == state.as_str() {
            return Ok(current);
        }
        let row = ReservationRow::find_by_statement(stmt(
            "UPDATE number_reservations SET provider_release_state = $3, revision = revision + 1, updated_at = $4 \
             WHERE id = $1 AND revision = $2 RETURNING *",
            vec![current.id.into(), current.revision.into(), state.as_str().into(), now.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("reservation"))?;
        event(txn, ctx, &row, reason, now).await?;
        Ok(row)
    }

    /// HELD → CONVERTED into a provisioned ACQUISITION (the provider acquisition is
    /// committed: a later expiry can no longer release it).
    pub(crate) async fn convert_to_acquisition_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationRow> {
        let current = lock_reservation(txn, reservation_id).await?;
        if current.status() != ReservationStatus::Held {
            return Err(DomainError::InvalidTransition {
                entity: "reservation",
                from: current.status.clone(),
                to: "CONVERTED".into(),
            });
        }
        let row = ReservationRow::find_by_statement(stmt(
            "UPDATE number_reservations SET status = 'CONVERTED', converted_to = 'ACQUISITION', revision = revision + 1, \
               updated_at = $3, ended_at = $3 WHERE id = $1 AND revision = $2 RETURNING *",
            vec![current.id.into(), current.revision.into(), now.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("reservation"))?;
        event(txn, ctx, &row, "PROVISIONED_ACQUISITION", now).await?;
        Ok(row)
    }

    /// The caller scope's reservations, newest first (bounded keyset page). `before` is
    /// the `(created_at, id)` of the last row of the previous page.
    pub async fn list_reservations(
        &self,
        scope: &OwnerScope,
        status: Option<ReservationStatus>,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: u64,
    ) -> DomainResult<Vec<ReservationRow>> {
        let (at, id) = before.unzip();
        Ok(ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND ($4::text IS NULL OR status = $4) \
               AND ($5::timestamptz IS NULL OR (created_at, id) < ($5, $6)) \
             ORDER BY created_at DESC, id DESC LIMIT $7",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                status.map(|s| s.as_str().to_string()).into(),
                at.into(),
                id.into(),
                (limit.clamp(1, 100) as i64).into(),
            ],
        ))
        .all(&self.db)
        .await?)
    }

    /// Current reservations whose expiry has passed (bounded, oldest first).
    pub async fn reservations_due(
        &self,
        now: DateTime<Utc>,
        limit: u64,
    ) -> DomainResult<Vec<ReservationRow>> {
        Ok(ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE status IN ('PENDING', 'HELD') AND expires_at <= $1 \
             ORDER BY expires_at LIMIT $2",
            vec![now.into(), (limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// Ended reservations whose provider release is still owed (PENDING) — the release
    /// command is (re)submitted for them.
    pub async fn releases_owed(&self, limit: u64) -> DomainResult<Vec<ReservationRow>> {
        Ok(ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE provider_release_state = 'PENDING' \
               AND status IN ('EXPIRED', 'CANCELLED') ORDER BY updated_at LIMIT $1",
            vec![(limit.clamp(1, 1_000) as i64).into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// A reservation by id, whatever its scope (workers only).
    pub async fn reservation_any(&self, id: Uuid) -> DomainResult<ReservationRow> {
        ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .ok_or(DomainError::NotFound("reservation"))
    }
}
