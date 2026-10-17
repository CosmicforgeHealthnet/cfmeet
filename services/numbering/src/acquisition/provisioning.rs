//! Provisioning and activation prerequisites (Part IV Workstream I §143).
//!
//! Provisioning a reservation, in order: the reservation is the caller's and still
//! holds the number (HELD and not expired, or already taken over by an assignment of the
//! same scope); eligibility (KYC, risk, limits, capabilities) is RE-EVALUATED by the F
//! engine for this number and the provider serving it — a stale or revoked basis never
//! provisions; every requested capability is supported (E resolver); the provider's hold
//! is read back and must still be this reservation's (where the provider supports
//! inspection). Then ONE transaction records the scoped idempotency key, the acquisition
//! (PROVISIONING), the number's PROVISIONING transition and the durable `PROVISION`
//! command. G's executor applies the provider's answer: a proven acquisition creates the
//! binding and PROVISIONED (acquisition PROVISIONED, a HELD reservation CONVERTED); a
//! refusal reverts it (FAILED, retryable); an unknown outcome is read back, never resent.
//!
//! A provisioned number is acquired by the platform — NOT an active customer number.
//! Configuring its voice / SMS routes ([`Acquisition::configure_routes`]) asks the
//! provider (durable `CONFIGURE_*` commands) to route each family to a stable opaque
//! route reference on the current binding; the configuration becomes CONFIGURED only on
//! the provider's acknowledgement, and the family is READY only once its consumer
//! (Telephony / SMS Gateway) acknowledged that revision through its authenticated
//! contract. Nothing here fabricates either acknowledgement.

use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
    reservations::{begin_idempotent, customer_actor},
    Acquisition,
};
use crate::{
    domain::RequestContext,
    infra::idempotency,
    numbers::{
        acquisitions::{
            AcquisitionRow, AcquisitionStatus, FamilyReadiness, RouteFamily, RouteStatus,
        },
        eligibility::{EligibilityDecisionRow, EligibilityRequest, Standing},
        kyc::{EligibilityOutcome, SubjectKind},
        reservations::ReservationRow,
        stmt, Capability, DomainError, DomainResult, InventorySource, NumberType, OwnerScope,
        OwnershipClass, ReservationStatus,
    },
    providers::{
        commands::{CommandRow, CommandStatus},
        incidents, ProviderNumberStatus, ProviderOperation, ProviderOutcome,
    },
};

#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    pub reservation_id: Uuid,
    /// The routable capabilities to provision (each must be supported).
    pub capabilities: Vec<Capability>,
    /// The eligibility decision to re-evaluate (defaults to the reservation's or the
    /// assignment's basis).
    pub eligibility_decision_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct Provisioned {
    pub acquisition: AcquisitionRow,
    pub replayed: bool,
}

/// The provider-neutral status of an acquisition.
#[derive(Debug, Clone)]
pub struct AcquisitionStatusView {
    pub acquisition: AcquisitionRow,
    /// The provisioning command (status, attempts, failure code), when submitted.
    pub provisioning: Option<CommandRow>,
    /// The routable capabilities the active binding serves (empty until PROVISIONED).
    pub served_capabilities: Vec<String>,
    /// Per capability family: provider route configuration and consumer readiness.
    pub routes: Vec<FamilyReadiness>,
    /// Synthetic test inventory never carries live public traffic.
    pub live_routing: bool,
}

impl AcquisitionStatusView {
    /// Every family of the served capabilities is ready.
    pub fn activation_ready(&self) -> bool {
        self.acquisition.status() == AcquisitionStatus::Provisioned
            && !self.routes.is_empty()
            && self.routes.iter().all(|r| r.ready)
    }
}

fn parse<T>(value: Option<T>) -> DomainResult<T> {
    value.ok_or_else(|| DomainError::Validation("corrupt eligibility decision".into()))
}

impl Acquisition {
    /// Re-evaluates the eligibility of `decision_id` (the caller's) for `resource_id`,
    /// `capabilities` and the provider serving the number, with the caller's CURRENT
    /// standing. Anything but ELIGIBLE refuses (`ELIGIBILITY_REQUIRED`).
    pub(crate) async fn reevaluate_eligibility(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        decision_id: Uuid,
        resource_id: Uuid,
        capabilities: &[Capability],
    ) -> DomainResult<EligibilityDecisionRow> {
        let required = |detail: &str| {
            DomainError::precondition(
                "ELIGIBILITY_REQUIRED",
                format!("eligibility re-evaluation refused: {detail}"),
            )
        };
        let base = self
            .numbers
            .eligibility_decision(scope, decision_id)
            .await
            .map_err(|_| required("no such decision in this scope"))?;
        let provider_id = match &self.providers {
            Some(c) => c.provider_object_of(resource_id).await.ok().map(|(p, _)| p),
            None => None,
        };
        let decision = self
            .numbers
            .evaluate_eligibility(
                ctx,
                scope,
                standing,
                &self.eligibility,
                EligibilityRequest {
                    subject_kind: parse(SubjectKind::parse(&base.subject_kind))?,
                    subject_ref: base.subject_ref,
                    country: base.country_code.clone(),
                    number_type: parse(NumberType::parse(&base.number_type))?,
                    ownership_class: parse(OwnershipClass::parse(&base.ownership_class))?,
                    capabilities: capabilities.to_vec(),
                    number_resource_id: Some(resource_id),
                    provider_id,
                },
            )
            .await?;
        if decision.outcome() != EligibilityOutcome::Eligible {
            return Err(required(&decision.outcome));
        }
        Ok(decision)
    }

    /// The provider still holds the number for this reservation (read-back), where the
    /// provider supports inspection and a provider hold was confirmed by a command.
    async fn verify_hold(
        &self,
        reservation: &ReservationRow,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let commands = self.commands()?;
        let Some(hold) = commands
            .reservation_commands(reservation.id)
            .await?
            .into_iter()
            .find(|c| c.operation == "RESERVE")
        else {
            return Ok(());
        };
        if !matches!(
            hold.status(),
            CommandStatus::Succeeded | CommandStatus::ReconciledSucceeded
        ) {
            return Err(DomainError::precondition(
                "HOLD_REQUIRED",
                "the provider hold is not confirmed",
            ));
        }
        let resource = self
            .numbers
            .resource(reservation.number_resource_id)
            .await?;
        let Some(provider) = commands
            .registry()
            .for_inventory(&hold.provider_id, resource.inventory())
        else {
            return Err(DomainError::precondition(
                "PROVIDER_UNAVAILABLE",
                "the provider holding the number is not available",
            ));
        };
        if !provider.descriptor().supports(ProviderOperation::Inspect) {
            return Ok(());
        }
        let external_ref = hold.request().external_ref;
        match provider.inspect(&external_ref).await {
            ProviderOutcome::Succeeded(state) => {
                let ours = state.status == ProviderNumberStatus::Held
                    && state.owned_by_us
                    && state.held_by_request == Some(hold.id)
                    && state.hold_expires_at.is_none_or(|t| t > now);
                if ours {
                    return Ok(());
                }
                incidents::raise(
                    self.numbers.db(),
                    incidents::Category::ProviderStateDrift,
                    &format!("hold-lost:{}", reservation.id),
                    incidents::Subject {
                        provider_id: Some(hold.provider_id.clone()),
                        number_resource_id: Some(reservation.number_resource_id),
                        command_id: Some(hold.id),
                    },
                    serde_json::json!({ "reason": "HOLD_NOT_CONFIRMED_AT_PROVISIONING", "providerStatus": state.status.as_str() }),
                    now,
                )
                .await?;
                Err(DomainError::precondition(
                    "HOLD_LOST",
                    "the provider no longer holds the number for this reservation",
                ))
            }
            _ => Err(DomainError::precondition(
                "PROVIDER_UNAVAILABLE",
                "the provider hold could not be read back",
            )),
        }
    }

    /// Provisions a reservation (see the module rules). `idempotency_key` is required.
    pub async fn provision(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        standing: Standing,
        request: ProvisionRequest,
        idempotency_key: &str,
    ) -> DomainResult<Provisioned> {
        let commands = self.commands()?;
        if request.capabilities.is_empty()
            || request.capabilities.iter().any(|c| !c.is_routable())
            || request.capabilities.len() > Capability::ALL.len()
        {
            return Err(DomainError::Validation(
                "provisioning names the routable capabilities to serve".into(),
            ));
        }
        let mut caps: Vec<Capability> = request.capabilities.clone();
        caps.sort();
        caps.dedup();
        let names: Vec<&str> = caps.iter().map(|c| c.as_str()).collect();
        let fingerprint = idempotency::fingerprint(&[
            b"acquisition.create",
            request.reservation_id.as_bytes(),
            names.join(",").as_bytes(),
            request
                .eligibility_decision_id
                .map(|d| d.to_string())
                .unwrap_or_default()
                .as_bytes(),
        ]);
        let now = self.now();
        {
            // Replays first (outside the work: the stored acquisition is the result).
            let txn = self.numbers.db().begin().await?;
            if let Err(existing) = begin_idempotent(
                &txn,
                scope,
                "numbers.acquisition.create",
                idempotency_key,
                &fingerprint,
                now,
            )
            .await?
            {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("acquisition"))?;
                return Ok(Provisioned {
                    acquisition: self.numbers.acquisition(scope, id).await?,
                    replayed: true,
                });
            }
            txn.rollback().await?;
        }
        let reservation = self
            .numbers
            .reservation(scope, request.reservation_id)
            .await?;
        // Workstream W / T: the provisioning kill switch and the provider cost breaker.
        crate::stewardship::controls::require_open(
            self.numbers.db(),
            crate::stewardship::controls::Control::NumberProvisioning,
        )
        .await?;
        if let Ok((provider_id, _)) = commands
            .provider_object_of(reservation.number_resource_id)
            .await
        {
            if crate::stewardship::risk::cost_breaker_open(
                self.numbers.db(),
                &self.settings.risk,
                &provider_id,
                now,
            )
            .await?
            {
                return Err(DomainError::precondition(
                    "PROVIDER_COST_BREAKER_OPEN",
                    "provisioning at this provider is paused by its cost circuit breaker",
                ));
            }
        }
        // The acquisition basis: a HELD, unexpired reservation, or one an assignment of
        // this scope took over (and still holds).
        let assignment = match (reservation.status(), reservation.converted_to.as_deref()) {
            (ReservationStatus::Held, _) => {
                if reservation.is_expired_at(now) {
                    return Err(DomainError::precondition(
                        "RESERVATION_EXPIRED",
                        "the reservation has expired",
                    ));
                }
                None
            }
            (ReservationStatus::Converted, Some("ASSIGNMENT")) => {
                #[derive(FromQueryResult)]
                struct Basis {
                    eligibility_decision_ref: Option<Uuid>,
                }
                let basis = Basis::find_by_statement(stmt(
                    "SELECT eligibility_decision_ref FROM dcn_assignments WHERE reservation_id = $1 \
                       AND status IN ('RESERVED', 'ACTIVATING') AND tenant_id = $2 AND application_id = $3 \
                       AND environment_id = $4",
                    vec![
                        reservation.id.into(),
                        scope.tenant_id.into(),
                        scope.application_id.into(),
                        scope.environment_id.into(),
                    ],
                ))
                .one(self.numbers.db())
                .await?
                .ok_or_else(|| {
                    DomainError::precondition(
                        "HOLD_REQUIRED",
                        "the reservation no longer holds the number",
                    )
                })?;
                basis.eligibility_decision_ref
            }
            _ => {
                return Err(DomainError::precondition(
                    "HOLD_REQUIRED",
                    "only a reservation that holds the number can be provisioned",
                ))
            }
        };
        let decision_id = request
            .eligibility_decision_id
            .or(reservation.eligibility_decision_ref)
            .or(assignment)
            .ok_or_else(|| {
                DomainError::precondition(
                    "ELIGIBILITY_REQUIRED",
                    "provisioning requires an eligibility decision (a pre-eligibility hold never provisions)",
                )
            })?;
        let resource = self
            .numbers
            .resource(reservation.number_resource_id)
            .await?;
        if resource.inventory() == InventorySource::SyntheticTest
            && matches!(
                commands.registry().runtime(),
                cf_service_auth::RuntimeEnvironment::Staging
                    | cf_service_auth::RuntimeEnvironment::Production
            )
        {
            return Err(DomainError::precondition(
                "ENVIRONMENT_INVENTORY_MISMATCH",
                "synthetic inventory is never provisioned in a protected runtime",
            ));
        }
        let resolution = self.numbers.resolve_capabilities(resource.id).await?;
        if let Some(c) = caps.iter().find(|c| !resolution.get(**c).supported) {
            return Err(DomainError::precondition(
                "CAPABILITY_NOT_SUPPORTED",
                format!(
                    "{c} is not supported for this number ({})",
                    resolution.get(*c).reason.unwrap_or("UNKNOWN")
                ),
            ));
        }
        let decision = self
            .reevaluate_eligibility(ctx, scope, standing, decision_id, resource.id, &caps)
            .await?;
        self.verify_hold(&reservation, now).await?;

        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.acquisition.create",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(existing) => {
                txn.commit().await?;
                let id = existing.ok_or(DomainError::NotFound("acquisition"))?;
                return Ok(Provisioned {
                    acquisition: self.numbers.acquisition(scope, id).await?,
                    replayed: true,
                });
            }
        };
        // The reservation must be exactly the one that was checked (no stale basis).
        let locked = ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE id = $1 FOR UPDATE",
            vec![reservation.id.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("reservation"))?;
        if locked.revision != reservation.revision
            || (locked.status == "HELD" && locked.is_expired_at(now))
        {
            return Err(DomainError::VersionConflict("reservation"));
        }
        let acquisition = self
            .numbers
            .open_acquisition_in(&txn, ctx, &locked, decision.id, &caps, now)
            .await?;
        let submitted = commands
            .request_provision_in(
                &txn,
                ctx,
                customer_actor(scope),
                resource.id,
                caps.clone(),
                &format!("acq:{}", record.simple()),
            )
            .await
            .map_err(|e| match e {
                DomainError::Conflict {
                    code: "COMMAND_IN_FLIGHT",
                    ..
                } => DomainError::conflict(
                    "ACQUISITION_EXISTS",
                    "the number is already being provisioned",
                ),
                other => other,
            })?;
        let acquisition = self
            .numbers
            .attach_provision_command_in(&txn, &acquisition, submitted.command.id, now)
            .await?;
        idempotency::complete(&txn, record, 202, None, Some(acquisition.id), now).await?;
        txn.commit().await?;
        Ok(Provisioned {
            acquisition,
            replayed: false,
        })
    }

    /// Requests provider route configuration of the families of `capabilities` on a
    /// PROVISIONED acquisition of the caller (see the module rules). A family already
    /// CONFIGURED on the current binding, or whose configuration is in flight, is left
    /// as is. `idempotency_key` is required.
    pub async fn configure_routes(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        acquisition_id: Uuid,
        capabilities: &[Capability],
        idempotency_key: &str,
    ) -> DomainResult<(AcquisitionRow, bool)> {
        let commands = self.commands()?;
        let mut families: Vec<RouteFamily> = capabilities
            .iter()
            .filter_map(|c| RouteFamily::of(*c))
            .collect();
        families.sort();
        families.dedup();
        if families.is_empty()
            || capabilities.iter().any(|c| !c.is_routable())
            || capabilities.len() > Capability::ALL.len()
        {
            return Err(DomainError::Validation(
                "route configuration names routable capabilities (voice / SMS / MMS)".into(),
            ));
        }
        let acquisition = self.numbers.acquisition(scope, acquisition_id).await?;
        if acquisition.status() != AcquisitionStatus::Provisioned {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "routes are configured once the acquisition is provisioned",
            ));
        }
        let resource_id = acquisition.number_resource_id;
        let bindings = self.numbers.bindings(resource_id).await?;
        for c in capabilities {
            let bound = bindings
                .iter()
                .any(|b| b.status == "ACTIVE" && b.capabilities.iter().any(|x| x == c.as_str()));
            if !bound {
                return Err(DomainError::precondition(
                    "CAPABILITY_NOT_SUPPORTED",
                    format!("{c} is not served by the number's provider binding"),
                ));
            }
        }
        let mut names: Vec<&str> = capabilities.iter().map(|c| c.as_str()).collect();
        names.sort();
        names.dedup();
        let fingerprint = idempotency::fingerprint(&[
            b"routes.configure",
            acquisition_id.as_bytes(),
            names.join(",").as_bytes(),
        ]);
        let now = self.now();
        let txn = self.numbers.db().begin().await?;
        let record = match begin_idempotent(
            &txn,
            scope,
            "numbers.routes.configure",
            idempotency_key,
            &fingerprint,
            now,
        )
        .await?
        {
            Ok(record) => record,
            Err(_) => {
                txn.commit().await?;
                return Ok((self.numbers.acquisition(scope, acquisition_id).await?, true));
            }
        };
        let resource = crate::numbers::resources::lock(&txn, resource_id).await?;
        if !matches!(
            resource.lifecycle_state(),
            crate::numbers::LifecycleState::Provisioned
                | crate::numbers::LifecycleState::Active
                | crate::numbers::LifecycleState::Suspended
        ) {
            return Err(DomainError::precondition(
                "PROVISIONING_REQUIRED",
                "routes are configured on a provisioned number",
            ));
        }
        let current = self.numbers.current_routes(resource_id).await?;
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
            if let Some(existing) = current.iter().find(|r| r.family() == family) {
                let settled = existing.status() == RouteStatus::Configured
                    && existing.binding_revision == resource.binding_revision;
                let in_flight = matches!(
                    existing.status(),
                    RouteStatus::Requested | RouteStatus::Unknown
                );
                if settled || in_flight {
                    continue;
                }
            }
            let route = self
                .numbers
                .request_route_configuration_in(&txn, ctx, resource_id, family, binding, now)
                .await?;
            let submitted = commands
                .request_route_in(
                    &txn,
                    ctx,
                    resource_id,
                    family == RouteFamily::Voice,
                    route.route_ref,
                    &format!("route:{}", route.id.simple()),
                )
                .await?;
            self.numbers
                .attach_route_command_in(&txn, &route, submitted.command.id, now)
                .await?;
        }
        idempotency::complete(&txn, record, 202, None, Some(acquisition_id), now).await?;
        txn.commit().await?;
        Ok((acquisition, false))
    }

    /// The provider-neutral status of an acquisition of the caller's scope.
    pub async fn acquisition_status(
        &self,
        scope: &OwnerScope,
        acquisition_id: Uuid,
    ) -> DomainResult<AcquisitionStatusView> {
        let acquisition = self.numbers.acquisition(scope, acquisition_id).await?;
        self.status_view(acquisition).await
    }

    pub(crate) async fn status_view(
        &self,
        acquisition: AcquisitionRow,
    ) -> DomainResult<AcquisitionStatusView> {
        let provisioning = match (&self.providers, acquisition.provision_command_id) {
            (Some(c), Some(id)) => Some(c.command(id).await?),
            _ => None,
        };
        let resource = self
            .numbers
            .resource(acquisition.number_resource_id)
            .await?;
        let mut served: Vec<String> = if acquisition.status() == AcquisitionStatus::Provisioned {
            self.numbers
                .bindings(resource.id)
                .await?
                .into_iter()
                .filter(|b| b.status == "ACTIVE")
                .flat_map(|b| b.capabilities)
                .collect()
        } else {
            vec![]
        };
        served.sort();
        served.dedup();
        let caps: Vec<Capability> = served.iter().filter_map(|c| Capability::parse(c)).collect();
        let routes = if caps.is_empty() {
            vec![]
        } else {
            self.numbers
                .activation_readiness(resource.id, &caps)
                .await?
        };
        Ok(AcquisitionStatusView {
            live_routing: resource.inventory() != InventorySource::SyntheticTest,
            acquisition,
            provisioning,
            served_capabilities: served,
            routes,
        })
    }
}
