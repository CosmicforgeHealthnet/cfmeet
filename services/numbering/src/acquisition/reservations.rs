//! Reservations with provider holds (Part IV Workstream H §142.3–§142.4).
//!
//! Creating a reservation, in order: the trusted scope; the number is in the scope's
//! inventory class and allocatable with a fresh offer; every requested capability is
//! supported (E resolver); the eligibility basis — a recorded ELIGIBLE decision of the
//! scope for this number's country / type / capabilities, or a pre-eligibility hold where
//! policy permits it (never PRODUCTION) — then ONE transaction records the scoped
//! idempotency key, the PENDING reservation (resource RESERVING; the row lock and the
//! one-current-reservation index let exactly one of concurrent claims win) and the durable
//! `RESERVE` provider command. The provider is called by the dispatch worker with the
//! command id as provider idempotency key; its answer (hold, refusal, unknown) is applied
//! by G's executor. A timeout after possible execution is never retried blindly.
//!
//! Ending a reservation never assumes the provider released anything: a HELD reservation
//! that is cancelled or expires keeps its number RESERVED with the release PENDING, a
//! `RELEASE_HOLD` command gives the hold back, and only the provider's evidence (or a
//! read-back) returns the number to inventory. A reservation whose hold is still being
//! requested, or whose outcome is unknown, is never ended under the provider's feet.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::Acquisition;
use crate::{
    domain::RequestContext,
    infra::{
        idempotency::{self, Begin, IdempotencyScope},
        jobs::{self, ClaimedJob, JobHandler, JobOutcome, NewJob, RetrySafety},
    },
    numbers::{
        reservations::{HoldBasis, ReservationBasis, ReservationRow},
        resources::lock,
        Actor, ActorKind, Capability, DomainError, DomainResult, InventoryClass, LifecycleState,
        OwnerScope, ReservationStatus,
    },
    providers::commands::{CommandRow, CommandStatus},
};

pub const EXPIRE_JOB: &str = "reservation.expire";
/// Owed releases submitted at most this many times before an operator incident.
pub const MAX_RELEASE_SUBMISSIONS: i64 = 5;

/// A reservation as asked by a customer.
#[derive(Debug, Clone)]
pub struct ReservationRequest {
    pub number_resource_id: Uuid,
    /// The routable capabilities the number is reserved for (each must be supported).
    pub capabilities: Vec<Capability>,
    pub hold_seconds: Option<i64>,
    pub eligibility_decision_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct Reserved {
    pub reservation: ReservationRow,
    pub replayed: bool,
}

/// What the expiry worker did with one reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// Not current any more (converted, cancelled, failed, already expired).
    NotCurrent,
    NotDue,
    /// Expired; nothing was held at a provider (or the domain fixture owns no command).
    Expired,
    /// Expired with the provider release PENDING and a `RELEASE_HOLD` submitted.
    ReleaseRequested,
    /// The provider's answer to the hold (or an acquisition) is still in flight: retried
    /// later, never ended under it.
    Deferred,
}

pub fn customer_actor(scope: &OwnerScope) -> Actor {
    Actor::new(
        ActorKind::Customer,
        format!("service-account:{}", scope.service_account_id.simple()),
    )
}

fn idempotency_scope(scope: &OwnerScope) -> IdempotencyScope {
    IdempotencyScope::Customer {
        tenant_id: scope.tenant_id,
        application_id: scope.application_id,
        environment_id: scope.environment_id,
    }
}

/// Opens the scoped idempotency record of a customer operation inside `txn`:
/// `Ok(Ok(record))` to run it, `Ok(Err(resource_id))` to replay the stored result.
pub(crate) async fn begin_idempotent(
    txn: &DatabaseTransaction,
    scope: &OwnerScope,
    operation: &str,
    key: &str,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> DomainResult<Result<Uuid, Option<Uuid>>> {
    if !idempotency::valid_key(key) {
        return Err(DomainError::Validation(
            "an idempotency key is 8–128 of [A-Za-z0-9._:-]".into(),
        ));
    }
    match idempotency::begin(
        txn,
        &idempotency_scope(scope),
        operation,
        key,
        fingerprint,
        now,
        Duration::hours(24),
    )
    .await?
    {
        Begin::New(record) => Ok(Ok(record)),
        Begin::Replay { resource_id, .. } => Ok(Err(resource_id)),
        Begin::Mismatch => Err(DomainError::conflict(
            "IDEMPOTENCY_KEY_REUSED",
            "the idempotency key was used for a different request",
        )),
        Begin::InProgress => Err(DomainError::conflict(
            "REQUEST_IN_PROGRESS",
            "a request with this idempotency key is still in progress",
        )),
    }
}

impl Acquisition {
    /// The provider hold of a reservation, if a provider confirmed one through a command
    /// (domain-created reservations have none).
    async fn hold_command(&self, reservation_id: Uuid) -> DomainResult<Option<CommandRow>> {
        let Some(commands) = &self.providers else {
            return Ok(None);
        };
        Ok(commands
            .reservation_commands(reservation_id)
            .await?
            .into_iter()
            .find(|c| c.operation == "RESERVE"))
    }

    /// The eligibility basis of a new reservation (see the module rules).
    async fn reservation_basis(
        &self,
        scope: &OwnerScope,
        resource: &crate::numbers::resources::ResourceRow,
        request: &ReservationRequest,
        now: DateTime<Utc>,
    ) -> DomainResult<ReservationBasis> {
        let required = || {
            DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                "an ELIGIBLE decision of this scope for this number, still in force, is required",
            )
        };
        match request.eligibility_decision_id {
            Some(id) => {
                let d = self
                    .numbers
                    .eligibility_decision(scope, id)
                    .await
                    .map_err(|_| required())?;
                let class = InventoryClass::of(resource.inventory());
                let covered = request
                    .capabilities
                    .iter()
                    .all(|c| d.requested_capabilities.iter().any(|x| x == c.as_str()));
                if !d.valid_for(scope, now)
                    || d.number_resource_id.is_some_and(|n| n != resource.id)
                    || d.country_code != resource.country_code
                    || d.number_type != resource.number_type
                    || d.inventory_class != class.as_str()
                    || !covered
                {
                    return Err(required());
                }
                Ok(ReservationBasis {
                    basis: HoldBasis::EligibilityDecision,
                    eligibility_decision_ref: Some(d.id),
                    capabilities: request.capabilities.clone(),
                    for_principal: None,
                })
            }
            None if self
                .settings
                .permits_pre_eligibility_hold(scope.environment_kind) =>
            {
                Ok(ReservationBasis {
                    basis: HoldBasis::PreEligibilityHold,
                    eligibility_decision_ref: None,
                    capabilities: request.capabilities.clone(),
                    for_principal: None,
                })
            }
            None => Err(required()),
        }
    }

    /// Creates a reservation (see the module rules). `idempotency_key` is required.
    pub async fn create_reservation(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        request: ReservationRequest,
        idempotency_key: &str,
    ) -> DomainResult<Reserved> {
        let commands = self.commands()?;
        if request.capabilities.iter().any(|c| !c.is_routable())
            || request.capabilities.len() > Capability::ALL.len()
        {
            return Err(DomainError::Validation(
                "a reservation names routable capabilities (voice / SMS / MMS)".into(),
            ));
        }
        let ttl = match request.hold_seconds {
            Some(s) => Duration::seconds(s),
            None => self.settings.reservation_default_ttl,
        };
        if !(crate::numbers::reservations::MIN_TTL_SECONDS
            ..=crate::numbers::reservations::MAX_TTL_SECONDS)
            .contains(&ttl.num_seconds())
        {
            return Err(DomainError::Validation(
                "a reservation lasts 1 minute to 24 hours".into(),
            ));
        }
        let mut caps: Vec<&str> = request.capabilities.iter().map(|c| c.as_str()).collect();
        caps.sort();
        caps.dedup();
        let fingerprint = idempotency::fingerprint(&[
            b"reservation.create",
            request.number_resource_id.as_bytes(),
            caps.join(",").as_bytes(),
            &ttl.num_seconds().to_be_bytes(),
            request
                .eligibility_decision_id
                .map(|d| d.to_string())
                .unwrap_or_default()
                .as_bytes(),
        ]);
        let now = self.now();
        // Replays first: a repeated request returns the original reservation even after
        // the number left inventory (because of that very reservation).
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.reservation.create",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(existing) => {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("reservation"))?;
                return Ok(Reserved {
                    reservation: self.numbers.reservation(scope, id).await?,
                    replayed: true,
                });
            }
        };
        // Validation outside the claim (reads only; the claim below re-checks under lock).
        let resource = self.numbers.resource(request.number_resource_id).await?;
        if !scope.may_use(resource.inventory()) {
            // Another class's number is indistinguishable from a missing one.
            return Err(DomainError::NotFound("number resource"));
        }
        let resolution = self.numbers.resolve_capabilities(resource.id).await?;
        if let Some(c) = request
            .capabilities
            .iter()
            .find(|c| !resolution.get(**c).supported)
        {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_SUPPORTED",
                format!(
                    "{c} is not supported for this number ({})",
                    resolution.get(*c).reason.unwrap_or("UNKNOWN")
                ),
            ));
        }
        // Workstream T: the allocation risk decision (recorded when it refuses).
        let risk = crate::stewardship::risk::evaluate_allocation_in(
            self.numbers.db(),
            ctx,
            &self.settings.risk,
            scope,
            &crate::stewardship::risk::AllocationRequest {
                country: resource.country_code.clone(),
                ownership_class: None,
            },
            now,
        )
        .await?;
        match risk.outcome.as_str() {
            "DENY" => {
                return Err(DomainError::precondition(
                    "RISK_DENIED",
                    format!(
                        "allocation refused by the risk policy: {}",
                        risk.reasons.join(", ")
                    ),
                ))
            }
            "REVIEW" => {
                return Err(DomainError::precondition(
                    "RISK_REVIEW_REQUIRED",
                    format!("allocation needs review: {}", risk.reasons.join(", ")),
                ))
            }
            _ => {}
        }
        let basis = self
            .reservation_basis(scope, &resource, &request, now)
            .await?;
        let (reservation, _) = commands
            .request_hold_in(
                &txn,
                ctx,
                scope,
                customer_actor(scope),
                resource.id,
                ttl,
                basis,
                &format!("rsv:{}", record.simple()),
            )
            .await
            .map_err(|e| match e {
                // Never an oracle on another scope's claim: the number is just unavailable.
                DomainError::Conflict {
                    code: "RESERVATION_EXISTS" | "COMMAND_IN_FLIGHT",
                    ..
                } => DomainError::precondition(
                    "NOT_ALLOCATABLE",
                    "the number is not available for allocation",
                ),
                other => other,
            })?;
        idempotency::complete(&txn, record, 202, None, Some(reservation.id), now).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_reservations_created_total",
            &[("basis", reservation_basis_label(&reservation))],
        );
        Ok(Reserved {
            reservation,
            replayed: false,
        })
    }

    /// Cancels a reservation of the caller's scope.
    pub async fn cancel_reservation(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        reservation_id: Uuid,
    ) -> DomainResult<ReservationRow> {
        let current = self.numbers.reservation(scope, reservation_id).await?;
        match current.status() {
            ReservationStatus::Held => {}
            ReservationStatus::Pending => {
                return Err(DomainError::conflict(
                    "RESERVATION_BUSY",
                    "the provider hold is still being requested; cancel once it is settled",
                ))
            }
            ReservationStatus::Unknown => {
                return Err(DomainError::precondition(
                    "RECONCILIATION_REQUIRED",
                    "the provider's answer to the hold is unknown and is being reconciled",
                ))
            }
            other => {
                return Err(DomainError::InvalidTransition {
                    entity: "reservation",
                    from: other.as_str().into(),
                    to: "CANCELLED".into(),
                })
            }
        }
        self.end_reservation(
            ctx,
            &current,
            ReservationStatus::Cancelled,
            "RESERVATION_CANCELLED",
            Some(scope),
        )
        .await
    }

    /// Ends a HELD reservation (cancellation / expiry): with a provider hold, the release
    /// is requested and PENDING; a domain hold ends through the D primitives.
    async fn end_reservation(
        &self,
        ctx: &RequestContext,
        current: &ReservationRow,
        to: ReservationStatus,
        reason: &str,
        scope: Option<&OwnerScope>,
    ) -> DomainResult<ReservationRow> {
        let held_by_provider = self.hold_command(current.id).await?.is_some_and(|c| {
            matches!(
                c.status(),
                CommandStatus::Succeeded | CommandStatus::ReconciledSucceeded
            )
        });
        if !held_by_provider {
            let actor = scope
                .map(customer_actor)
                .unwrap_or_else(|| Actor::system("reservation-expiry"));
            return match (to, scope) {
                (ReservationStatus::Cancelled, Some(s)) => {
                    self.numbers
                        .cancel_reservation(ctx, s, actor, current.id)
                        .await
                }
                _ => {
                    self.numbers
                        .expire_reservation(ctx, actor, current.id)
                        .await
                }
            };
        }
        let commands = self.commands()?;
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let resource = lock(&txn, current.number_resource_id).await?;
        if resource.lifecycle_state() != LifecycleState::Reserved {
            // Provisioning (or its uncertainty) owns the number now.
            return Err(DomainError::conflict(
                "RESERVATION_BUSY",
                "the reserved number is being acquired; the reservation cannot end now",
            ));
        }
        let ended = self
            .numbers
            .end_for_release_in(&txn, ctx, current.id, to, reason, now)
            .await?;
        commands
            .request_release_hold_in(&txn, ctx, &ended, now)
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_reservations_ended_total",
            &[("to", to.as_str()), ("release", "REQUESTED")],
        );
        Ok(ended)
    }

    /// Expires one reservation if due (the `reservation.expire` job; see [`Expiry`]).
    pub async fn expire_reservation(
        &self,
        ctx: &RequestContext,
        reservation_id: Uuid,
    ) -> DomainResult<Expiry> {
        let current = self.numbers.reservation_any(reservation_id).await?;
        let now = self.now();
        if !matches!(
            current.status(),
            ReservationStatus::Pending | ReservationStatus::Held
        ) {
            return Ok(Expiry::NotCurrent);
        }
        if !current.is_expired_at(now) {
            return Ok(Expiry::NotDue);
        }
        let hold = self.hold_command(current.id).await?;
        if current.status() == ReservationStatus::Pending {
            return match hold {
                // The provider's answer decides (a late hold is recorded and released).
                Some(_) => Ok(Expiry::Deferred),
                None => {
                    self.numbers
                        .expire_reservation(ctx, Actor::system("reservation-expiry"), current.id)
                        .await?;
                    Ok(Expiry::Expired)
                }
            };
        }
        match self
            .end_reservation(
                ctx,
                &current,
                ReservationStatus::Expired,
                "RESERVATION_EXPIRED",
                None,
            )
            .await
        {
            Ok(row) if row.provider_release_state == "PENDING" => Ok(Expiry::ReleaseRequested),
            Ok(_) => Ok(Expiry::Expired),
            Err(DomainError::Conflict {
                code: "RESERVATION_BUSY" | "COMMAND_IN_FLIGHT",
                ..
            }) => Ok(Expiry::Deferred),
            Err(DomainError::VersionConflict(_)) => Ok(Expiry::Deferred),
            Err(e) => Err(e),
        }
    }

    /// The reservation sweep: schedules an expiry job for every due reservation and
    /// (re)submits every owed provider release. Returns `(scheduled, releases)`.
    pub async fn sweep_reservations(
        &self,
        ctx: &RequestContext,
        limit: u64,
    ) -> DomainResult<(usize, usize)> {
        let now = self.now();
        let mut scheduled = 0;
        for r in self.numbers.reservations_due(now, limit).await? {
            let (_, created) = jobs::enqueue(
                self.numbers.db(),
                NewJob {
                    kind: EXPIRE_JOB,
                    dedupe_key: Some(format!("expire:{}", r.id)),
                    payload: json!({ "reservation_id": r.id }),
                    retry_safety: RetrySafety::Idempotent,
                    max_attempts: 20,
                    run_at: now,
                },
                ctx,
                now,
            )
            .await?;
            scheduled += usize::from(created);
        }
        let mut releases = 0;
        let Some(commands) = &self.providers else {
            return Ok((scheduled, releases));
        };
        for r in self.numbers.releases_owed(limit).await? {
            let previous = commands
                .reservation_commands(r.id)
                .await?
                .into_iter()
                .filter(|c| c.operation == "RELEASE_HOLD")
                .collect::<Vec<_>>();
            if previous.iter().any(|c| !c.status().is_final()) {
                continue;
            }
            if previous.len() as i64 >= MAX_RELEASE_SUBMISSIONS {
                crate::providers::incidents::raise(
                    self.numbers.db(),
                    crate::providers::incidents::Category::ReconciliationConflict,
                    &format!("release-hold-exhausted:{}", r.id),
                    crate::providers::incidents::Subject {
                        number_resource_id: Some(r.number_resource_id),
                        ..Default::default()
                    },
                    json!({ "reason": "RELEASE_HOLD_ATTEMPTS_EXHAUSTED" }),
                    now,
                )
                .await?;
                continue;
            }
            let txn = self.numbers.db().begin().await?;
            lock(&txn, r.number_resource_id).await?;
            match commands.request_release_hold_in(&txn, ctx, &r, now).await {
                Ok(_) => {
                    txn.commit().await?;
                    releases += 1;
                }
                Err(DomainError::Conflict { .. }) => txn.rollback().await?,
                Err(e) => return Err(e),
            }
        }
        Ok((scheduled, releases))
    }

    /// The `RESERVE` / `RELEASE_HOLD` command evidence of a reservation (provider-neutral
    /// summary: operation, status, attempts, failure code).
    pub async fn reservation_provider_status(
        &self,
        reservation_id: Uuid,
    ) -> DomainResult<Vec<Value>> {
        let Some(commands) = &self.providers else {
            return Ok(vec![]);
        };
        Ok(commands
            .reservation_commands(reservation_id)
            .await?
            .iter()
            .map(|c| {
                json!({
                    "operation": c.operation,
                    "status": c.status,
                    "attempts": c.attempts,
                    "failureCode": c.failure_code,
                    "updatedAt": c.updated_at,
                })
            })
            .collect())
    }
}

fn reservation_basis_label(r: &ReservationRow) -> &'static str {
    HoldBasis::parse(&r.hold_basis)
        .map(|b| b.as_str())
        .unwrap_or("DIRECT")
}

/// The `reservation.expire` job handler.
pub struct ExpiryHandler {
    pub acquisition: Arc<Acquisition>,
}

#[async_trait::async_trait]
impl JobHandler for ExpiryHandler {
    fn kind(&self) -> &'static str {
        EXPIRE_JOB
    }

    async fn run(&self, job: &ClaimedJob) -> JobOutcome {
        let Some(id) = job
            .payload
            .get("reservation_id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return JobOutcome::Failed {
                code: "MALFORMED_JOB",
                message: "no reservation id".into(),
            };
        };
        match self
            .acquisition
            .expire_reservation(&job.context(), id)
            .await
        {
            Ok(Expiry::Deferred) => JobOutcome::Retry {
                code: "RESERVATION_BUSY",
                message: "the provider's answer is still in flight".into(),
            },
            Ok(_) => JobOutcome::Succeeded,
            Err(DomainError::Database { summary, .. }) => JobOutcome::Retry {
                code: "DATABASE",
                message: summary,
            },
            Err(e) => JobOutcome::Failed {
                code: "EXPIRY_REFUSED",
                message: e.code().to_string(),
            },
        }
    }
}
