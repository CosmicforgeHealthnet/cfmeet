//! Acquisitions, route configurations and consumer readiness (Part IV Workstream I).
//!
//! * An **acquisition** is the durable provisioning intent of one reservation of a
//!   customer scope (`number_acquisitions`): the re-evaluated eligibility decision, the
//!   routable capabilities, the `PROVISION` command and — only once the provider's
//!   acquisition is proven — the binding. It is the difference between "the platform
//!   acquired a number" (PROVISIONED) and an ACTIVE DCN assignment, which needs an owner,
//!   eligibility, routing and readiness on top.
//! * A **route configuration** records that a provider was asked to route a capability
//!   family (VOICE / SMS) of a number to an opaque CF Meet route reference, on which
//!   binding (and binding revision), with which command, and the provider's
//!   acknowledgement evidence. A response for a superseded configuration, or for a binding
//!   that has since changed, never makes a route CONFIGURED.
//! * **Readiness** is the authenticated acknowledgement, by the service that carries a
//!   family's traffic (Telephony for voice, the SMS Gateway for SMS), that it routes the
//!   CONFIGURED revision. Numbering never fabricates it.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    bindings::BindingRow, reservations::ReservationRow, resources::lock, stmt, Capability,
    DomainError, DomainResult, NumberDomain, OwnerScope,
};
use crate::{
    domain::RequestContext,
    infra::outbox::{self, EventScope, NewEvent},
};

text_enum!(AcquisitionStatus {
    Provisioning = "PROVISIONING",
    Provisioned = "PROVISIONED",
    Failed = "FAILED",
    Unknown = "UNKNOWN",
});

text_enum!(
    /// A capability family routed by one provider configuration.
    RouteFamily {
        Voice = "VOICE",
        Sms = "SMS",
    }
);

impl RouteFamily {
    pub fn of(c: Capability) -> Option<Self> {
        match c {
            Capability::VoiceInbound | Capability::VoiceOutbound => Some(RouteFamily::Voice),
            Capability::SmsInbound | Capability::SmsOutbound | Capability::MmsRich => {
                Some(RouteFamily::Sms)
            }
            _ => None,
        }
    }

    pub fn capabilities(&self) -> &'static [Capability] {
        match self {
            RouteFamily::Voice => &[Capability::VoiceInbound, Capability::VoiceOutbound],
            RouteFamily::Sms => &[
                Capability::SmsInbound,
                Capability::SmsOutbound,
                Capability::MmsRich,
            ],
        }
    }

    /// The service that carries the family's traffic and confirms its readiness.
    pub fn consumer(&self) -> &'static str {
        match self {
            RouteFamily::Voice => cf_service_auth::services::TELEPHONY,
            RouteFamily::Sms => cf_service_auth::services::SMS_GATEWAY,
        }
    }
}

text_enum!(RouteStatus {
    Requested = "REQUESTED",
    Configured = "CONFIGURED",
    Failed = "FAILED",
    Unknown = "UNKNOWN",
    Superseded = "SUPERSEDED",
});

/// The acquisition edges (mirrored by `number_acquisition_transition_allowed`).
pub fn acquisition_allowed(from: AcquisitionStatus, to: AcquisitionStatus) -> bool {
    use AcquisitionStatus as A;
    matches!(
        (from, to),
        (A::Provisioning, A::Provisioned | A::Failed | A::Unknown)
            | (A::Unknown, A::Provisioned | A::Failed)
            | (A::Failed, A::Provisioning)
    )
}

/// The route configuration edges (mirrored by
/// `number_route_configuration_transition_allowed`).
pub fn route_allowed(from: RouteStatus, to: RouteStatus) -> bool {
    use RouteStatus as R;
    matches!(
        (from, to),
        (
            R::Requested,
            R::Configured | R::Failed | R::Unknown | R::Superseded
        ) | (R::Unknown, R::Configured | R::Failed | R::Superseded)
            | (R::Configured, R::Superseded)
    )
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquisitionRow {
    pub id: Uuid,
    pub reservation_id: Uuid,
    pub number_resource_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub status: String,
    pub requested_capabilities: Vec<String>,
    pub eligibility_decision_ref: Uuid,
    pub provision_command_id: Option<Uuid>,
    pub binding_id: Option<Uuid>,
    pub failure_code: Option<String>,
    pub revision: i64,
    pub correlation_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl AcquisitionRow {
    pub fn status(&self) -> AcquisitionStatus {
        AcquisitionStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn in_scope(&self, scope: &OwnerScope) -> bool {
        self.tenant_id == scope.tenant_id
            && self.application_id == scope.application_id
            && self.environment_id == scope.environment_id
    }

    pub fn capabilities(&self) -> Vec<Capability> {
        self.requested_capabilities
            .iter()
            .filter_map(|c| Capability::parse(c))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteConfigurationRow {
    pub id: Uuid,
    pub number_resource_id: Uuid,
    pub family: String,
    pub route_ref: Uuid,
    pub route_revision: i64,
    #[serde(skip)]
    pub binding_id: Uuid,
    pub binding_revision: i64,
    pub status: String,
    #[serde(skip)]
    pub command_id: Option<Uuid>,
    pub evidence_ref: Option<Uuid>,
    pub failure_code: Option<String>,
    pub revision: i64,
    pub correlation_id: String,
    pub requested_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl RouteConfigurationRow {
    pub fn status(&self) -> RouteStatus {
        RouteStatus::parse(&self.status).expect("CHECK-constrained status")
    }

    pub fn family(&self) -> RouteFamily {
        RouteFamily::parse(&self.family).expect("CHECK-constrained family")
    }
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessRow {
    pub id: Uuid,
    pub route_configuration_id: Uuid,
    pub number_resource_id: Uuid,
    pub family: String,
    pub route_revision: i64,
    pub consumer_service: String,
    pub status: String,
    pub correlation_id: String,
    pub acknowledged_at: DateTime<Utc>,
    pub withdrawn_at: Option<DateTime<Utc>>,
}

/// Whether one family of a number is ready to carry traffic (provider route configured
/// on the current binding AND acknowledged by its consumer).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyReadiness {
    pub family: RouteFamily,
    pub route_ref: Option<Uuid>,
    pub route_revision: Option<i64>,
    pub route_status: Option<String>,
    pub acknowledged_by: Vec<String>,
    pub ready: bool,
    /// Why it is not ready (`None`: ready).
    pub reason: Option<&'static str>,
}

/// Stable reasons a family is not ready.
pub mod readiness {
    pub const ROUTE_NOT_CONFIGURED: &str = "ROUTE_NOT_CONFIGURED";
    pub const ROUTE_CONFIGURATION_PENDING: &str = "ROUTE_CONFIGURATION_PENDING";
    pub const ROUTE_CONFIGURATION_FAILED: &str = "ROUTE_CONFIGURATION_FAILED";
    pub const BINDING_CHANGED: &str = "BINDING_CHANGED";
    pub const CAPABILITY_NOT_BOUND: &str = "CAPABILITY_NOT_BOUND";
    pub const CONSUMER_NOT_READY: &str = "CONSUMER_NOT_READY";
    pub const ALL: &[&str] = &[
        ROUTE_NOT_CONFIGURED,
        ROUTE_CONFIGURATION_PENDING,
        ROUTE_CONFIGURATION_FAILED,
        BINDING_CHANGED,
        CAPABILITY_NOT_BOUND,
        CONSUMER_NOT_READY,
    ];
}

async fn acquisition_event<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    a: &AcquisitionRow,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        db,
        NewEvent {
            event_type: "number.acquisition.changed",
            aggregate_type: "ACQUISITION",
            aggregate_id: a.id,
            scope: Some(EventScope {
                tenant_id: a.tenant_id,
                application_id: a.application_id,
                environment_id: a.environment_id,
            }),
            payload: json!({
                "acquisition_id": a.id,
                "reservation_id": a.reservation_id,
                "number_resource_id": a.number_resource_id,
                "status": a.status,
                "revision": a.revision,
                "requested_capabilities": a.requested_capabilities,
                "failure_code": a.failure_code,
                "reason_code": reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

async fn route_event<C: ConnectionTrait>(
    db: &C,
    ctx: &RequestContext,
    r: &RouteConfigurationRow,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<()> {
    outbox::enqueue(
        db,
        NewEvent {
            event_type: "number.route.changed",
            aggregate_type: "NUMBER_RESOURCE",
            aggregate_id: r.number_resource_id,
            scope: None,
            payload: json!({
                "number_resource_id": r.number_resource_id,
                "family": r.family,
                "route_ref": r.route_ref,
                "route_revision": r.route_revision,
                "binding_revision": r.binding_revision,
                "status": r.status,
                "reason_code": reason,
            }),
        },
        ctx,
        now,
    )
    .await?;
    Ok(())
}

async fn lock_acquisition_by_command(
    txn: &DatabaseTransaction,
    command_id: Uuid,
) -> DomainResult<Option<AcquisitionRow>> {
    Ok(AcquisitionRow::find_by_statement(stmt(
        "SELECT * FROM number_acquisitions WHERE provision_command_id = $1 FOR UPDATE",
        vec![command_id.into()],
    ))
    .one(txn)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn set_acquisition(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    current: &AcquisitionRow,
    to: AcquisitionStatus,
    binding_id: Option<Uuid>,
    failure_code: Option<&str>,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<AcquisitionRow> {
    if !acquisition_allowed(current.status(), to) {
        return Err(DomainError::InvalidTransition {
            entity: "acquisition",
            from: current.status.clone(),
            to: to.as_str().into(),
        });
    }
    let done = matches!(
        to,
        AcquisitionStatus::Provisioned | AcquisitionStatus::Failed
    );
    let row = AcquisitionRow::find_by_statement(stmt(
        "UPDATE number_acquisitions SET status = $3, binding_id = COALESCE($4, binding_id), failure_code = $5, \
           revision = revision + 1, updated_at = $6, completed_at = CASE WHEN $7 THEN $6 ELSE NULL END \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            current.id.into(),
            current.revision.into(),
            to.as_str().into(),
            binding_id.into(),
            failure_code.map(str::to_string).into(),
            now.into(),
            done.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("acquisition"))?;
    acquisition_event(txn, ctx, &row, reason, now).await?;
    crate::metrics::inc("numbering_acquisitions_total", &[("status", to.as_str())]);
    Ok(row)
}

async fn lock_route_by_command(
    txn: &DatabaseTransaction,
    command_id: Uuid,
) -> DomainResult<Option<RouteConfigurationRow>> {
    Ok(RouteConfigurationRow::find_by_statement(stmt(
        "SELECT * FROM number_route_configurations WHERE command_id = $1 FOR UPDATE",
        vec![command_id.into()],
    ))
    .one(txn)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn set_route(
    txn: &DatabaseTransaction,
    ctx: &RequestContext,
    current: &RouteConfigurationRow,
    to: RouteStatus,
    evidence_ref: Option<Uuid>,
    failure_code: Option<&str>,
    reason: &str,
    now: DateTime<Utc>,
) -> DomainResult<RouteConfigurationRow> {
    if !route_allowed(current.status(), to) {
        return Err(DomainError::InvalidTransition {
            entity: "route configuration",
            from: current.status.clone(),
            to: to.as_str().into(),
        });
    }
    let row = RouteConfigurationRow::find_by_statement(stmt(
        "UPDATE number_route_configurations SET status = $3, evidence_ref = COALESCE($4, evidence_ref), \
           failure_code = $5, revision = revision + 1, updated_at = $6, \
           acknowledged_at = CASE WHEN $3 = 'CONFIGURED' THEN $6 ELSE acknowledged_at END, \
           superseded_at = CASE WHEN $3 = 'SUPERSEDED' THEN $6 ELSE superseded_at END \
         WHERE id = $1 AND revision = $2 RETURNING *",
        vec![
            current.id.into(),
            current.revision.into(),
            to.as_str().into(),
            evidence_ref.into(),
            failure_code.map(str::to_string).into(),
            now.into(),
        ],
    ))
    .one(txn)
    .await?
    .ok_or(DomainError::VersionConflict("route configuration"))?;
    route_event(txn, ctx, &row, reason, now).await?;
    Ok(row)
}

impl NumberDomain {
    // -----------------------------------------------------------------------------------
    // Acquisitions
    // -----------------------------------------------------------------------------------

    /// Opens (or, after a definite failure, re-opens) the acquisition of `reservation`
    /// (inside the provisioning transaction, before the `PROVISION` command).
    pub(crate) async fn open_acquisition_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        reservation: &ReservationRow,
        decision_ref: Uuid,
        capabilities: &[Capability],
        now: DateTime<Utc>,
    ) -> DomainResult<AcquisitionRow> {
        let mut caps: Vec<String> = capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        caps.sort();
        caps.dedup();
        let existing = AcquisitionRow::find_by_statement(stmt(
            "SELECT * FROM number_acquisitions WHERE reservation_id = $1 FOR UPDATE",
            vec![reservation.id.into()],
        ))
        .one(txn)
        .await?;
        let row = match existing {
            None => AcquisitionRow::find_by_statement(stmt(
                "INSERT INTO number_acquisitions (id, reservation_id, number_resource_id, tenant_id, application_id, \
                   environment_id, environment_kind, status, requested_capabilities, eligibility_decision_ref, \
                   correlation_id, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'PROVISIONING', $8, $9, $10, $11, $11) RETURNING *",
                vec![
                    Uuid::new_v4().into(),
                    reservation.id.into(),
                    reservation.number_resource_id.into(),
                    reservation.tenant_id.into(),
                    reservation.application_id.into(),
                    reservation.environment_id.into(),
                    reservation.environment_kind.clone().into(),
                    caps.into(),
                    decision_ref.into(),
                    ctx.correlation_id.as_str().into(),
                    now.into(),
                ],
            ))
            .one(txn)
            .await?
            .expect("insert returns the row"),
            Some(a) if a.status() == AcquisitionStatus::Failed => AcquisitionRow::find_by_statement(stmt(
                "UPDATE number_acquisitions SET status = 'PROVISIONING', requested_capabilities = $3, \
                   eligibility_decision_ref = $4, provision_command_id = NULL, failure_code = NULL, completed_at = NULL, \
                   revision = revision + 1, updated_at = $5 WHERE id = $1 AND revision = $2 RETURNING *",
                vec![a.id.into(), a.revision.into(), caps.into(), decision_ref.into(), now.into()],
            ))
            .one(txn)
            .await?
            .ok_or(DomainError::VersionConflict("acquisition"))?,
            Some(_) => {
                return Err(DomainError::conflict(
                    "ACQUISITION_EXISTS",
                    "this reservation is already being (or has been) provisioned",
                ))
            }
        };
        acquisition_event(txn, ctx, &row, "PROVISION_REQUESTED", now).await?;
        Ok(row)
    }

    /// Records the `PROVISION` command of an acquisition (same transaction).
    pub(crate) async fn attach_provision_command_in(
        &self,
        txn: &DatabaseTransaction,
        acquisition: &AcquisitionRow,
        command_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<AcquisitionRow> {
        AcquisitionRow::find_by_statement(stmt(
            "UPDATE number_acquisitions SET provision_command_id = $3, revision = revision + 1, updated_at = $4 \
             WHERE id = $1 AND revision = $2 RETURNING *",
            vec![acquisition.id.into(), acquisition.revision.into(), command_id.into(), now.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("acquisition"))
    }

    /// The provider's acquisition is proven (definite or reconciled success of the
    /// command): PROVISIONED with its binding; a still-HELD reservation becomes CONVERTED
    /// (a later expiry can no longer release the committed acquisition).
    pub(crate) async fn acquisition_provisioned_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        binding_id: Uuid,
        reason: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let Some(a) = lock_acquisition_by_command(txn, command_id).await? else {
            return Ok(());
        };
        set_acquisition(
            txn,
            ctx,
            &a,
            AcquisitionStatus::Provisioned,
            Some(binding_id),
            None,
            reason,
            now,
        )
        .await?;
        let reservation = ReservationRow::find_by_statement(stmt(
            "SELECT * FROM number_reservations WHERE id = $1",
            vec![a.reservation_id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::NotFound("reservation"))?;
        if reservation.status == "HELD" {
            self.convert_to_acquisition_in(txn, ctx, reservation.id, now)
                .await?;
        }
        Ok(())
    }

    /// The provisioning definitely did not happen (refusal, or read-back proved it).
    pub(crate) async fn acquisition_failed_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        failure_code: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        if let Some(a) = lock_acquisition_by_command(txn, command_id).await? {
            if matches!(
                a.status(),
                AcquisitionStatus::Provisioning | AcquisitionStatus::Unknown
            ) {
                set_acquisition(
                    txn,
                    ctx,
                    &a,
                    AcquisitionStatus::Failed,
                    None,
                    Some(failure_code),
                    "PROVISION_FAILED",
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The provisioning outcome is unknown: never assumed, reconciled from read-back.
    pub(crate) async fn acquisition_unknown_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        if let Some(a) = lock_acquisition_by_command(txn, command_id).await? {
            if a.status() == AcquisitionStatus::Provisioning {
                set_acquisition(
                    txn,
                    ctx,
                    &a,
                    AcquisitionStatus::Unknown,
                    None,
                    None,
                    "PROVIDER_OUTCOME_UNKNOWN",
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// An acquisition of the caller's scope (other scopes: not found).
    pub async fn acquisition(&self, scope: &OwnerScope, id: Uuid) -> DomainResult<AcquisitionRow> {
        AcquisitionRow::find_by_statement(stmt(
            "SELECT * FROM number_acquisitions WHERE id = $1",
            vec![id.into()],
        ))
        .one(&self.db)
        .await?
        .filter(|a| a.in_scope(scope))
        .ok_or(DomainError::NotFound("acquisition"))
    }

    /// The acquisition of a reservation, whatever its scope (internal).
    pub async fn acquisition_of_reservation(
        &self,
        reservation_id: Uuid,
    ) -> DomainResult<Option<AcquisitionRow>> {
        Ok(AcquisitionRow::find_by_statement(stmt(
            "SELECT * FROM number_acquisitions WHERE reservation_id = $1",
            vec![reservation_id.into()],
        ))
        .one(&self.db)
        .await?)
    }

    /// The caller scope's acquisitions, newest first (bounded keyset page).
    pub async fn acquisitions(
        &self,
        scope: &OwnerScope,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: u64,
    ) -> DomainResult<Vec<AcquisitionRow>> {
        let (at, id) = before.unzip();
        Ok(AcquisitionRow::find_by_statement(stmt(
            "SELECT * FROM number_acquisitions WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND ($4::timestamptz IS NULL OR (created_at, id) < ($4, $5)) ORDER BY created_at DESC, id DESC LIMIT $6",
            vec![
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                at.into(),
                id.into(),
                (limit.clamp(1, 100) as i64).into(),
            ],
        ))
        .all(&self.db)
        .await?)
    }

    // -----------------------------------------------------------------------------------
    // Route configurations
    // -----------------------------------------------------------------------------------

    /// The current configuration of every family of a number.
    pub async fn current_routes(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<Vec<RouteConfigurationRow>> {
        Ok(RouteConfigurationRow::find_by_statement(stmt(
            "SELECT * FROM number_route_configurations WHERE number_resource_id = $1 \
               AND status IN ('REQUESTED', 'CONFIGURED', 'UNKNOWN') ORDER BY family",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// Every configuration of a number, oldest first (history).
    pub async fn route_history(
        &self,
        resource_id: Uuid,
    ) -> DomainResult<Vec<RouteConfigurationRow>> {
        Ok(RouteConfigurationRow::find_by_statement(stmt(
            "SELECT * FROM number_route_configurations WHERE number_resource_id = $1 ORDER BY family, route_revision",
            vec![resource_id.into()],
        ))
        .all(&self.db)
        .await?)
    }

    /// Requests a new configuration of `family` on `binding` (supersedes the current
    /// one; the route reference of the family is stable, its revision increments).
    pub(crate) async fn request_route_configuration_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        resource_id: Uuid,
        family: RouteFamily,
        binding: &BindingRow,
        now: DateTime<Utc>,
    ) -> DomainResult<RouteConfigurationRow> {
        lock(txn, resource_id).await?;
        let history = RouteConfigurationRow::find_by_statement(stmt(
            "SELECT * FROM number_route_configurations WHERE number_resource_id = $1 AND family = $2 \
             ORDER BY route_revision DESC FOR UPDATE",
            vec![resource_id.into(), family.as_str().into()],
        ))
        .all(txn)
        .await?;
        if let Some(current) = history.iter().find(|r| {
            matches!(
                r.status(),
                RouteStatus::Requested | RouteStatus::Configured | RouteStatus::Unknown
            )
        }) {
            if current.status() != RouteStatus::Configured {
                return Err(DomainError::conflict(
                    "COMMAND_IN_FLIGHT",
                    "a route configuration of this family is still in flight",
                ));
            }
            set_route(
                txn,
                ctx,
                current,
                RouteStatus::Superseded,
                None,
                None,
                "ROUTE_RECONFIGURED",
                now,
            )
            .await?;
        }
        let route_ref = history
            .first()
            .map(|r| r.route_ref)
            .unwrap_or_else(Uuid::new_v4);
        let route_revision = history.first().map(|r| r.route_revision + 1).unwrap_or(1);
        let row = RouteConfigurationRow::find_by_statement(stmt(
            "INSERT INTO number_route_configurations (id, number_resource_id, family, route_ref, route_revision, binding_id, \
               binding_revision, status, correlation_id, requested_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'REQUESTED', $8, $9, $9) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                resource_id.into(),
                family.as_str().into(),
                route_ref.into(),
                route_revision.into(),
                binding.id.into(),
                binding.binding_revision.into(),
                ctx.correlation_id.as_str().into(),
                now.into(),
            ],
        ))
        .one(txn)
        .await?
        .expect("insert returns the row");
        route_event(txn, ctx, &row, "ROUTE_CONFIGURATION_REQUESTED", now).await?;
        Ok(row)
    }

    /// Records the `CONFIGURE_*` command of a route configuration (same transaction).
    pub(crate) async fn attach_route_command_in(
        &self,
        txn: &DatabaseTransaction,
        route: &RouteConfigurationRow,
        command_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<RouteConfigurationRow> {
        RouteConfigurationRow::find_by_statement(stmt(
            "UPDATE number_route_configurations SET command_id = $3, revision = revision + 1, updated_at = $4 \
             WHERE id = $1 AND revision = $2 RETURNING *",
            vec![route.id.into(), route.revision.into(), command_id.into(), now.into()],
        ))
        .one(txn)
        .await?
        .ok_or(DomainError::VersionConflict("route configuration"))
    }

    /// The provider acknowledged a route configuration (definite or read-back). A
    /// response for a superseded configuration changes nothing; one for a configuration
    /// whose binding changed since the request FAILS it (`STALE_BINDING`); one naming a
    /// different route than requested is a conflict (`ROUTE_ACK_CONFLICT`), never applied.
    pub(crate) async fn route_configured_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        acknowledged_route: Option<Uuid>,
        evidence_ref: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        let Some(route) = lock_route_by_command(txn, command_id).await? else {
            return Ok(());
        };
        if !matches!(
            route.status(),
            RouteStatus::Requested | RouteStatus::Unknown
        ) {
            crate::metrics::inc("numbering_route_stale_acknowledgements_total", &[]);
            return Ok(());
        }
        if acknowledged_route.is_some_and(|r| r != route.route_ref) {
            return Err(DomainError::precondition(
                "ROUTE_ACK_CONFLICT",
                "the provider acknowledged a different route than requested",
            ));
        }
        let resource = lock(txn, route.number_resource_id).await?;
        let binding_active = BindingRow::find_by_statement(stmt(
            "SELECT * FROM number_provider_bindings WHERE id = $1 AND status = 'ACTIVE'",
            vec![route.binding_id.into()],
        ))
        .one(txn)
        .await?
        .is_some();
        if !binding_active || resource.binding_revision != route.binding_revision {
            set_route(
                txn,
                ctx,
                &route,
                RouteStatus::Failed,
                None,
                Some("STALE_BINDING"),
                "ROUTE_BINDING_CHANGED",
                now,
            )
            .await?;
            return Ok(());
        }
        set_route(
            txn,
            ctx,
            &route,
            RouteStatus::Configured,
            Some(evidence_ref),
            None,
            "ROUTE_CONFIGURED",
            now,
        )
        .await?;
        // The resource's routing references and revision follow the configured route.
        let column = match route.family() {
            RouteFamily::Voice => "voice_route_ref",
            RouteFamily::Sms => "sms_route_ref",
        };
        txn.execute(stmt(
            &format!(
                "UPDATE number_resources SET {column} = $2, routing_revision = routing_revision + 1, \
                   revision = revision + 1, updated_at = $3 WHERE id = $1"
            ),
            vec![resource.id.into(), route.route_ref.into(), now.into()],
        ))
        .await?;
        Ok(())
    }

    /// A definite refusal (or read-back proving the route was not set).
    pub(crate) async fn route_failed_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        failure_code: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        if let Some(route) = lock_route_by_command(txn, command_id).await? {
            if matches!(
                route.status(),
                RouteStatus::Requested | RouteStatus::Unknown
            ) {
                set_route(
                    txn,
                    ctx,
                    &route,
                    RouteStatus::Failed,
                    None,
                    Some(failure_code),
                    "ROUTE_CONFIGURATION_FAILED",
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The configuration outcome is unknown (reconciled from read-back).
    pub(crate) async fn route_unknown_in(
        &self,
        txn: &DatabaseTransaction,
        ctx: &RequestContext,
        command_id: Uuid,
        now: DateTime<Utc>,
    ) -> DomainResult<()> {
        if let Some(route) = lock_route_by_command(txn, command_id).await? {
            if route.status() == RouteStatus::Requested {
                set_route(
                    txn,
                    ctx,
                    &route,
                    RouteStatus::Unknown,
                    None,
                    None,
                    "PROVIDER_OUTCOME_UNKNOWN",
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Consumer readiness
    // -----------------------------------------------------------------------------------

    /// The consumer of a family acknowledges the CONFIGURED revision `route_revision` of
    /// `route_ref` (idempotent). Only Telephony confirms voice, only the SMS Gateway SMS.
    pub async fn acknowledge_route(
        &self,
        ctx: &RequestContext,
        consumer: &str,
        resource_id: Uuid,
        family: RouteFamily,
        route_ref: Uuid,
        route_revision: i64,
    ) -> DomainResult<ReadinessRow> {
        if consumer != family.consumer() {
            return Err(DomainError::precondition(
                "CONSUMER_MISMATCH",
                "this service does not carry the family's traffic",
            ));
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        let route = RouteConfigurationRow::find_by_statement(stmt(
            "SELECT * FROM number_route_configurations WHERE number_resource_id = $1 AND family = $2 \
               AND status IN ('REQUESTED', 'CONFIGURED', 'UNKNOWN') FOR UPDATE",
            vec![resource_id.into(), family.as_str().into()],
        ))
        .one(&txn)
        .await?
        .ok_or(DomainError::NotFound("route configuration"))?;
        if route.route_ref != route_ref || route.route_revision != route_revision {
            return Err(DomainError::precondition(
                "STALE_ROUTE_REVISION",
                "the acknowledged route revision is not the current one",
            ));
        }
        if route.status() != RouteStatus::Configured {
            return Err(DomainError::precondition(
                "ROUTE_NOT_CONFIGURED",
                "the provider has not acknowledged this route configuration",
            ));
        }
        if let Some(existing) = ReadinessRow::find_by_statement(stmt(
            "SELECT * FROM number_route_readiness WHERE route_configuration_id = $1 AND consumer_service = $2 \
               AND status = 'READY'",
            vec![route.id.into(), consumer.into()],
        ))
        .one(&txn)
        .await?
        {
            txn.commit().await?;
            return Ok(existing);
        }
        let row = ReadinessRow::find_by_statement(stmt(
            "INSERT INTO number_route_readiness (id, route_configuration_id, number_resource_id, family, route_revision, \
               consumer_service, status, correlation_id, acknowledged_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'READY', $7, $8) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                route.id.into(),
                resource_id.into(),
                family.as_str().into(),
                route.route_revision.into(),
                consumer.into(),
                ctx.correlation_id.as_str().into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .expect("insert returns the row");
        route_event(&txn, ctx, &route, "CONSUMER_READY", now).await?;
        txn.commit().await?;
        Ok(row)
    }

    /// Whether every family of `capabilities` is ready on a number (see
    /// [`FamilyReadiness`]): the provider route CONFIGURED on the CURRENT binding that
    /// serves the capabilities, AND acknowledged by the family's consumer.
    pub async fn activation_readiness(
        &self,
        resource_id: Uuid,
        capabilities: &[Capability],
    ) -> DomainResult<Vec<FamilyReadiness>> {
        let resource = self.resource(resource_id).await?;
        let bindings = self.bindings(resource_id).await?;
        let routes = self.current_routes(resource_id).await?;
        let mut families: Vec<RouteFamily> = capabilities
            .iter()
            .filter_map(|c| RouteFamily::of(*c))
            .collect();
        families.sort();
        families.dedup();
        let mut out = Vec::new();
        for family in families {
            let needed: Vec<&str> = capabilities
                .iter()
                .filter(|c| RouteFamily::of(**c) == Some(family))
                .map(|c| c.as_str())
                .collect();
            let route = routes.iter().find(|r| r.family() == family);
            let acknowledged: Vec<String> = match route {
                Some(r) => ReadinessRow::find_by_statement(stmt(
                    "SELECT * FROM number_route_readiness WHERE route_configuration_id = $1 AND status = 'READY'",
                    vec![r.id.into()],
                ))
                .all(&self.db)
                .await?
                .into_iter()
                .map(|x| x.consumer_service)
                .collect(),
                None => vec![],
            };
            let bound = bindings.iter().any(|b| {
                b.status == "ACTIVE" && needed.iter().all(|n| b.capabilities.iter().any(|c| c == n))
            });
            let reason = match route {
                _ if !bound => Some(readiness::CAPABILITY_NOT_BOUND),
                None => Some(readiness::ROUTE_NOT_CONFIGURED),
                Some(r) if r.status() != RouteStatus::Configured => {
                    Some(readiness::ROUTE_CONFIGURATION_PENDING)
                }
                Some(r) if r.binding_revision != resource.binding_revision => {
                    Some(readiness::BINDING_CHANGED)
                }
                Some(_) if !acknowledged.iter().any(|s| s == family.consumer()) => {
                    Some(readiness::CONSUMER_NOT_READY)
                }
                Some(_) => None,
            };
            out.push(FamilyReadiness {
                family,
                route_ref: route.map(|r| r.route_ref),
                route_revision: route.map(|r| r.route_revision),
                route_status: route.map(|r| r.status.clone()),
                acknowledged_by: acknowledged,
                ready: reason.is_none(),
                reason,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_edges() {
        assert_eq!(
            RouteFamily::of(Capability::VoiceOutbound),
            Some(RouteFamily::Voice)
        );
        assert_eq!(RouteFamily::of(Capability::MmsRich), Some(RouteFamily::Sms));
        assert_eq!(RouteFamily::of(Capability::MobileNumberClass), None);
        assert_eq!(RouteFamily::Voice.consumer(), "cfmeet.telephony");
        assert_eq!(RouteFamily::Sms.consumer(), "cfmeet.sms-gateway");
        // PROVISIONED is final; UNKNOWN leaves only to a definite outcome.
        for to in AcquisitionStatus::ALL {
            assert!(!acquisition_allowed(AcquisitionStatus::Provisioned, *to));
        }
        assert!(!acquisition_allowed(
            AcquisitionStatus::Unknown,
            AcquisitionStatus::Provisioning
        ));
        // Only a requested / unknown configuration becomes CONFIGURED.
        let into: Vec<_> = RouteStatus::ALL
            .iter()
            .filter(|s| route_allowed(**s, RouteStatus::Configured))
            .collect();
        assert_eq!(into, vec![&RouteStatus::Requested, &RouteStatus::Unknown]);
    }
}
