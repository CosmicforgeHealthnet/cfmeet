//! Routing grants (Part IV Workstream K §145): how the services that carry traffic learn,
//! from Numbering's CANONICAL state, where a number routes and who may use it.
//!
//! * **Inbound resolution** — the carrier of a family's traffic (Telephony: voice, SMS
//!   Gateway: SMS) presents the received number; Numbering answers from the number's
//!   ACTIVE assignment (owner scope, ownership class, routing target) only when the number
//!   is ACTIVE, the family's inbound capability usable and its route READY and
//!   acknowledged by that very consumer — with a signed INBOUND grant bound to the routing
//!   revision. Unknown, unassigned or unroutable numbers get an outcome, never a guess.
//! * **Outbound identity** — Calling (caller identity, audience Telephony) or the SMS
//!   Gateway (SMS sender) asks, in the DELEGATED customer scope, for a single-use grant to
//!   present one of that scope's ACTIVE numbers for one operation. A number of another
//!   scope is indistinguishable from a missing one; the outbound capability must be
//!   usable; a personal / professional number is used by its owner only and an employee's
//!   direct line by its routed employee. No arbitrary caller id or sender exists.
//! * **Verification** — the audience presents the grant; Numbering checks the signature
//!   and then CURRENT state: the grant recorded and not revoked, the assignment still the
//!   number's ACTIVE one, the routing revision unchanged, the capability still usable; an
//!   outbound grant is consumed once for its operation. A routing change, a transfer or
//!   the assignment leaving ACTIVE revokes outstanding grants immediately.
//! * **Production safety** — synthetic test inventory never resolves or authorizes
//!   anything in a protected runtime (staging / production).
//!
//! Grants are signed with Numbering's DEDICATED grant key (`cf-number-grant+jwt`), never
//! its service-auth key; signing material never leaves this module and no customer API
//! returns a grant.

use std::sync::Arc;

use cf_service_auth::{
    number_grant::{
        GrantScope, GrantTarget, NumberGrantFailure, NumberGrantKind, NumberGrantRequest,
        NumberGrantSigner, NumberGrantTrust, NumberGrantVerifier, VerifiedNumberGrant,
    },
    RuntimeEnvironment, ServiceId, SharedClock,
};
use chrono::{DateTime, TimeZone, Utc};
use sea_orm::TransactionTrait;
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

use crate::{
    domain::RequestContext,
    numbers::{
        acquisitions::RouteFamily,
        assignments::AssignmentRow,
        e164,
        grants::{revoke_reason, GrantRow, NewGrant},
        ownership,
        routing::RoutingTargets,
        AssignmentStatus, Capability, DomainError, DomainResult, InventorySource, LifecycleState,
        NumberDomain, OwnerKind, OwnershipClass, SmsTargetKind, VoiceTargetKind,
    },
};

/// Stable reasons a resolution is not routable or a grant does not verify.
pub mod reason {
    pub const UNKNOWN_NUMBER: &str = "UNKNOWN_NUMBER";
    pub const NOT_ASSIGNED: &str = "NOT_ASSIGNED";
    pub const NOT_ACTIVE: &str = "NOT_ACTIVE";
    pub const CAPABILITY_NOT_USABLE: &str = "CAPABILITY_NOT_USABLE";
    pub const ROUTE_NOT_READY: &str = "ROUTE_NOT_READY";
    pub const SYNTHETIC_NOT_ROUTABLE: &str = "SYNTHETIC_NOT_ROUTABLE";
    pub const GRANT_INVALID: &str = "GRANT_INVALID";
    pub const GRANT_EXPIRED: &str = "GRANT_EXPIRED";
    pub const GRANT_REVOKED: &str = "GRANT_REVOKED";
    pub const AUDIENCE_MISMATCH: &str = "AUDIENCE_MISMATCH";
    pub const STALE_ROUTING_REVISION: &str = "STALE_ROUTING_REVISION";
    pub const ASSIGNMENT_NOT_ACTIVE: &str = "ASSIGNMENT_NOT_ACTIVE";
    pub const GRANT_ALREADY_USED: &str = "GRANT_ALREADY_USED";
    pub const OPERATION_MISMATCH: &str = "OPERATION_MISMATCH";
    pub const ALL: &[&str] = &[
        UNKNOWN_NUMBER,
        NOT_ASSIGNED,
        NOT_ACTIVE,
        CAPABILITY_NOT_USABLE,
        ROUTE_NOT_READY,
        SYNTHETIC_NOT_ROUTABLE,
        GRANT_INVALID,
        GRANT_EXPIRED,
        GRANT_REVOKED,
        AUDIENCE_MISMATCH,
        STALE_ROUTING_REVISION,
        ASSIGNMENT_NOT_ACTIVE,
        GRANT_ALREADY_USED,
        OPERATION_MISMATCH,
    ];
}

fn capability_of(kind: NumberGrantKind) -> Capability {
    match kind {
        NumberGrantKind::InboundVoice => Capability::VoiceInbound,
        NumberGrantKind::InboundSms => Capability::SmsInbound,
        NumberGrantKind::CallerIdentity => Capability::VoiceOutbound,
        NumberGrantKind::SmsSender => Capability::SmsOutbound,
    }
}

/// A signed grant handed to the requesting service (the token is sensitive).
pub struct IssuedGrant {
    pub record: GrantRow,
    token: SecretString,
}

impl IssuedGrant {
    pub fn expose_for_delivery(&self) -> &str {
        self.token.expose_secret()
    }
}

impl std::fmt::Debug for IssuedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedGrant")
            .field("record", &self.record.id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// The routing of a received call / message.
#[derive(Debug)]
pub enum InboundResolution {
    /// Routed: the owner scope, class, target and the inbound grant.
    Routed {
        assignment: Box<AssignmentRow>,
        target: (String, Uuid),
        routing_revision: i64,
        grant: Box<IssuedGrant>,
    },
    /// Assigned and ready, but this family routes nowhere (e.g. an offboarded line).
    NotRouted {
        assignment: Box<AssignmentRow>,
        routing_revision: i64,
    },
    /// Not routable now (stable reason).
    Unavailable { reason: &'static str },
}

/// The outcome of a verification.
#[derive(Debug)]
pub struct Verification {
    pub reason: Option<&'static str>,
    pub grant: Option<GrantRow>,
    pub verified: Option<VerifiedNumberGrant>,
    /// The number to present (outbound grants, to their audience only).
    pub e164: Option<String>,
    pub consumed: bool,
}

impl Verification {
    pub fn valid(&self) -> bool {
        self.reason.is_none()
    }

    fn refused(reason: &'static str) -> Self {
        Self {
            reason: Some(reason),
            grant: None,
            verified: None,
            e164: None,
            consumed: false,
        }
    }
}

/// The current assignment of a number in the caller's delegated scope (no number).
#[derive(Debug, Clone)]
pub struct CurrentAssignment {
    pub assignment: AssignmentRow,
    pub targets: Option<RoutingTargets>,
    pub routing_revision: i64,
    pub number_active: bool,
    pub voice_outbound: bool,
    pub sms_outbound: bool,
}

/// The scope a delegated outbound request acts in.
#[derive(Debug, Clone, Copy)]
pub struct DelegatedScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Option<Uuid>,
}

/// Numbering's grant issuer and authoritative verifier.
#[derive(Clone)]
pub struct RoutingGrants {
    numbers: NumberDomain,
    signer: Arc<NumberGrantSigner>,
    verifier: Arc<NumberGrantVerifier>,
}

impl std::fmt::Debug for RoutingGrants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingGrants")
            .field("signer", &self.signer)
            .finish()
    }
}

fn at(unix: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(unix, 0).single().unwrap_or_else(Utc::now)
}

impl RoutingGrants {
    /// Builds the issuer and its own verifier (the signer's public key).
    pub fn new(numbers: NumberDomain, signer: NumberGrantSigner, clock: SharedClock) -> Self {
        let trust = NumberGrantTrust::issuer_keys(
            signer.issuer(),
            signer.runtime_environment(),
            &[signer.public_jwk()],
        )
        .expect("the signer's own public key is a valid trust key");
        Self {
            numbers,
            verifier: Arc::new(NumberGrantVerifier::new(trust, clock)),
            signer: Arc::new(signer),
        }
    }

    pub fn signer(&self) -> &NumberGrantSigner {
        &self.signer
    }

    fn protected_runtime(&self) -> bool {
        matches!(
            self.signer.runtime_environment(),
            RuntimeEnvironment::Staging | RuntimeEnvironment::Production
        )
    }

    async fn issue(
        &self,
        ctx: &RequestContext,
        request: NumberGrantRequest,
        requested_by: &ServiceId,
    ) -> DomainResult<IssuedGrant> {
        let grant = self
            .signer
            .sign(&request)
            .map_err(|e| DomainError::Validation(e.to_string()))?;
        let txn = self.numbers.db().begin().await?;
        let record = self
            .numbers
            .record_grant_in(
                &txn,
                NewGrant {
                    token_id: grant.token_id().as_str().to_string(),
                    grant_kind: request.kind.as_str(),
                    number_resource_id: request.number_resource_id,
                    assignment_id: request.assignment_id,
                    tenant_id: request.scope.tenant_id,
                    application_id: request.scope.application_id,
                    environment_id: request.scope.environment_id,
                    routing_revision: request.routing_revision,
                    audience: request.audience.as_str().to_string(),
                    requested_by: requested_by.as_str().to_string(),
                    principal_id: request.principal_id,
                    target_kind: request.target.as_ref().map(|t| t.kind.clone()),
                    target_ref: request.target.as_ref().map(|t| t.target_ref),
                    operation_ref: request.operation_ref.clone(),
                    issued_at: at(grant.issued_at()),
                    expires_at: at(grant.expires_at()),
                    correlation_id: ctx.correlation_id.as_str().to_string(),
                },
            )
            .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "numbering_routing_grants_issued_total",
            &[("kind", request.kind.as_str())],
        );
        Ok(IssuedGrant {
            record,
            token: SecretString::from(grant.expose_for_delivery().to_string()),
        })
    }

    /// The number's CURRENT open assignment if it is ACTIVE.
    async fn active_assignment(&self, resource_id: Uuid) -> DomainResult<Option<AssignmentRow>> {
        Ok(self
            .numbers
            .open_assignment_of(resource_id)
            .await?
            .filter(|a| a.status() == AssignmentStatus::Active))
    }

    async fn usable(
        &self,
        resource_id: Uuid,
        assignment: &AssignmentRow,
        c: Capability,
    ) -> DomainResult<bool> {
        if !assignment
            .capability_snapshot
            .iter()
            .any(|x| x == c.as_str())
        {
            return Ok(false);
        }
        Ok(self
            .numbers
            .resolve_capabilities(resource_id)
            .await?
            .get(c)
            .usable)
    }

    async fn current_targets(
        &self,
        assignment: &AssignmentRow,
    ) -> DomainResult<Option<RoutingTargets>> {
        let (Some(p), Some(r)) = (
            assignment.routing_profile_id,
            assignment.routing_profile_revision,
        ) else {
            return Ok(None);
        };
        let row = self.numbers.routing_profile_revision(p, r).await?;
        let voice = VoiceTargetKind::parse(&row.voice_target_kind)
            .ok_or_else(|| DomainError::Validation("corrupt routing profile".into()))?;
        let sms = SmsTargetKind::parse(&row.sms_target_kind)
            .ok_or_else(|| DomainError::Validation("corrupt routing profile".into()))?;
        Ok(Some(RoutingTargets {
            voice: (voice, row.voice_target_ref),
            sms: (sms, row.sms_target_ref),
        }))
    }

    /// The delegated scope's CURRENT assignment of a number, for the consumer that must
    /// apply its own membership policy before requesting an outbound grant (Workstream L:
    /// Calling checks the routed employee / extension / queue / ring group). A number of
    /// another scope, or without an ACTIVE assignment, is indistinguishable from a missing
    /// one. Never the telephone number.
    pub async fn current_assignment(
        &self,
        scope: DelegatedScope,
        number_resource_id: Uuid,
    ) -> DomainResult<CurrentAssignment> {
        let not_found = || DomainError::NotFound("number");
        let resource = self
            .numbers
            .resource(number_resource_id)
            .await
            .map_err(|_| not_found())?;
        let assignment = self
            .active_assignment(resource.id)
            .await?
            .filter(|a| {
                a.tenant_id == scope.tenant_id
                    && a.application_id == scope.application_id
                    && a.environment_id == scope.environment_id
            })
            .ok_or_else(not_found)?;
        let targets = self.current_targets(&assignment).await?;
        let protected =
            resource.inventory() == InventorySource::SyntheticTest && self.protected_runtime();
        let active = resource.lifecycle_state() == LifecycleState::Active && !protected;
        let voice_outbound = active
            && self
                .usable(resource.id, &assignment, Capability::VoiceOutbound)
                .await?;
        let sms_outbound = active
            && self
                .usable(resource.id, &assignment, Capability::SmsOutbound)
                .await?;
        Ok(CurrentAssignment {
            routing_revision: resource.routing_revision,
            number_active: active,
            voice_outbound,
            sms_outbound,
            targets,
            assignment,
        })
    }

    /// Resolves a received call / message (see the module rules). `consumer` is the
    /// authenticated caller; it must carry the family's traffic.
    pub async fn resolve_inbound(
        &self,
        ctx: &RequestContext,
        consumer: &ServiceId,
        family: RouteFamily,
        number: &str,
    ) -> DomainResult<InboundResolution> {
        if consumer.as_str() != family.consumer() {
            return Err(DomainError::precondition(
                "CONSUMER_MISMATCH",
                "only the carrier of the family's traffic resolves it",
            ));
        }
        let e164 = e164::normalize(number)?;
        let unavailable = |reason| Ok(InboundResolution::Unavailable { reason });
        let resource = match self
            .numbers
            .resources_by_e164(&e164)
            .await?
            .into_iter()
            .next()
        {
            Some(r) => r,
            None => return unavailable(reason::UNKNOWN_NUMBER),
        };
        if resource.inventory() == InventorySource::SyntheticTest && self.protected_runtime() {
            return unavailable(reason::SYNTHETIC_NOT_ROUTABLE);
        }
        if resource.lifecycle_state() != LifecycleState::Active {
            return unavailable(reason::NOT_ACTIVE);
        }
        let Some(assignment) = self.active_assignment(resource.id).await? else {
            return unavailable(reason::NOT_ASSIGNED);
        };
        let (kind, capability) = match family {
            RouteFamily::Voice => (NumberGrantKind::InboundVoice, Capability::VoiceInbound),
            RouteFamily::Sms => (NumberGrantKind::InboundSms, Capability::SmsInbound),
        };
        if !self.usable(resource.id, &assignment, capability).await? {
            return unavailable(reason::CAPABILITY_NOT_USABLE);
        }
        let ready = self
            .numbers
            .activation_readiness(resource.id, &[capability])
            .await?
            .into_iter()
            .any(|r| r.ready && r.acknowledged_by.iter().any(|s| s == consumer.as_str()));
        if !ready {
            return unavailable(reason::ROUTE_NOT_READY);
        }
        let targets = self.current_targets(&assignment).await?;
        let target = targets.and_then(|t| match family {
            RouteFamily::Voice => match t.voice {
                (VoiceTargetKind::None, _) | (_, None) => None,
                (k, Some(r)) => Some((k.as_str().to_string(), r)),
            },
            RouteFamily::Sms => match t.sms {
                (SmsTargetKind::None, _) | (_, None) => None,
                (k, Some(r)) => Some((k.as_str().to_string(), r)),
            },
        });
        let Some(target) = target else {
            return Ok(InboundResolution::NotRouted {
                routing_revision: resource.routing_revision,
                assignment: Box::new(assignment),
            });
        };
        let principal = match OwnershipClass::parse(&assignment.ownership_class) {
            Some(class) if class.owner_kind() == OwnerKind::Principal => Some(assignment.owner_ref),
            _ => (target.0 == "PRINCIPAL").then_some(target.1),
        };
        // Voice is delivered by Calling; SMS stays with the SMS Gateway (the messaging
        // consumer of later workstreams).
        let audience = match family {
            RouteFamily::Voice => ServiceId::calling(),
            RouteFamily::Sms => ServiceId::sms_gateway(),
        };
        let grant = self
            .issue(
                ctx,
                NumberGrantRequest {
                    kind,
                    audience,
                    number_resource_id: resource.id,
                    assignment_id: assignment.id,
                    routing_revision: resource.routing_revision,
                    scope: GrantScope {
                        tenant_id: assignment.tenant_id,
                        application_id: assignment.application_id,
                        environment_id: assignment.environment_id,
                    },
                    principal_id: principal,
                    target: Some(GrantTarget {
                        kind: target.0.clone(),
                        target_ref: target.1,
                    }),
                    operation_ref: None,
                },
                consumer,
            )
            .await?;
        Ok(InboundResolution::Routed {
            assignment: Box::new(assignment),
            target,
            routing_revision: resource.routing_revision,
            grant: Box::new(grant),
        })
    }

    /// A single-use outbound grant (CALLER_IDENTITY for Calling → Telephony, SMS_SENDER
    /// for the SMS Gateway) for a number the delegated scope actively owns.
    pub async fn authorize_outbound(
        &self,
        ctx: &RequestContext,
        requester: &ServiceId,
        kind: NumberGrantKind,
        scope: DelegatedScope,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> DomainResult<IssuedGrant> {
        let audience = match kind {
            NumberGrantKind::CallerIdentity => ServiceId::telephony(),
            NumberGrantKind::SmsSender => ServiceId::sms_gateway(),
            _ => return Err(DomainError::Validation("not an outbound grant".into())),
        };
        let not_found = || DomainError::NotFound("number");
        let resource = self
            .numbers
            .resource(number_resource_id)
            .await
            .map_err(|_| not_found())?;
        let assignment = self
            .active_assignment(resource.id)
            .await?
            .filter(|a| {
                a.tenant_id == scope.tenant_id
                    && a.application_id == scope.application_id
                    && a.environment_id == scope.environment_id
            })
            .ok_or_else(not_found)?;
        if resource.inventory() == InventorySource::SyntheticTest && self.protected_runtime() {
            return Err(DomainError::precondition(
                reason::SYNTHETIC_NOT_ROUTABLE,
                "synthetic test inventory never carries traffic in a protected runtime",
            ));
        }
        if resource.lifecycle_state() != LifecycleState::Active {
            return Err(DomainError::precondition(
                reason::NOT_ACTIVE,
                "the number is not active",
            ));
        }
        if !self
            .usable(resource.id, &assignment, capability_of(kind))
            .await?
        {
            return Err(DomainError::precondition(
                reason::CAPABILITY_NOT_USABLE,
                format!("{} is not usable on this number", capability_of(kind)),
            ));
        }
        let class = OwnershipClass::parse(&assignment.ownership_class)
            .ok_or_else(|| DomainError::Validation("corrupt ownership class".into()))?;
        // Who may present the number: the owner of a personal / professional number, the
        // routed employee of a direct line; organizational numbers: the organization.
        let permitted = match class.owner_kind() {
            OwnerKind::Principal => scope.principal_id == Some(assignment.owner_ref),
            OwnerKind::Organization if class == OwnershipClass::EmployeeDirect => {
                let employee = self
                    .current_targets(&assignment)
                    .await?
                    .and_then(|t| ownership::routed_principal(class, assignment.owner_ref, &t));
                employee.is_some() && scope.principal_id == employee
            }
            OwnerKind::Organization => true,
        };
        if !permitted {
            return Err(DomainError::precondition(
                "PRINCIPAL_NOT_PERMITTED",
                "the initiating principal may not use this number",
            ));
        }
        self.issue(
            ctx,
            NumberGrantRequest {
                kind,
                audience,
                number_resource_id: resource.id,
                assignment_id: assignment.id,
                routing_revision: resource.routing_revision,
                scope: GrantScope {
                    tenant_id: assignment.tenant_id,
                    application_id: assignment.application_id,
                    environment_id: assignment.environment_id,
                },
                principal_id: scope.principal_id,
                target: None,
                operation_ref: Some(operation_ref.to_string()),
            },
            requester,
        )
        .await
        .map_err(|e| match e {
            DomainError::Validation(_) => {
                DomainError::Validation("an operation reference is 8–128 of [A-Za-z0-9._:-]".into())
            }
            other => other,
        })
    }

    /// Verifies a grant presented by `presenter` against CURRENT state; `consume` performs
    /// the single use of an outbound grant for `operation_ref`.
    pub async fn verify(
        &self,
        presenter: &ServiceId,
        token: &str,
        consume: bool,
        operation_ref: Option<&str>,
    ) -> DomainResult<Verification> {
        let verified = match self.verifier.verify_any_audience(token) {
            Ok(v) => v,
            Err(NumberGrantFailure::Expired) => {
                return Ok(Verification::refused(reason::GRANT_EXPIRED))
            }
            Err(failure) => {
                tracing::warn!(failure = failure.as_str(), presenter = %presenter, "routing grant refused");
                return Ok(Verification::refused(reason::GRANT_INVALID));
            }
        };
        if &verified.audience != presenter {
            return Ok(Verification::refused(reason::AUDIENCE_MISMATCH));
        }
        let Some(record) = self
            .numbers
            .grant_by_token(verified.token_id.as_str())
            .await?
        else {
            return Ok(Verification::refused(reason::GRANT_INVALID));
        };
        let refuse = |reason: &'static str, record: GrantRow| Verification {
            reason: Some(reason),
            grant: Some(record),
            verified: None,
            e164: None,
            consumed: false,
        };
        if record.revoked_at.is_some() {
            return Ok(refuse(reason::GRANT_REVOKED, record));
        }
        let resource = self.numbers.resource(record.number_resource_id).await?;
        if resource.routing_revision != record.routing_revision {
            self.numbers
                .revoke_grant(record.id, revoke_reason::STALE_ROUTING_REVISION)
                .await?;
            return Ok(refuse(reason::STALE_ROUTING_REVISION, record));
        }
        let current = self.active_assignment(resource.id).await?;
        if resource.lifecycle_state() != LifecycleState::Active
            || current.as_ref().map(|a| a.id) != Some(record.assignment_id)
        {
            return Ok(refuse(reason::ASSIGNMENT_NOT_ACTIVE, record));
        }
        let assignment = current.expect("checked");
        if !self
            .usable(resource.id, &assignment, capability_of(verified.kind))
            .await?
        {
            return Ok(refuse(reason::CAPABILITY_NOT_USABLE, record));
        }
        let mut consumed = false;
        if verified.kind.is_outbound() {
            if record.consumed_at.is_some() {
                return Ok(refuse(reason::GRANT_ALREADY_USED, record));
            }
            if consume {
                if operation_ref != record.operation_ref.as_deref() {
                    return Ok(refuse(reason::OPERATION_MISMATCH, record));
                }
                if !self
                    .numbers
                    .consume_grant(record.id, presenter.as_str())
                    .await?
                {
                    return Ok(refuse(reason::GRANT_ALREADY_USED, record));
                }
                consumed = true;
            }
        } else if consume {
            return Err(DomainError::Validation(
                "only an outbound grant is consumed".into(),
            ));
        }
        Ok(Verification {
            reason: None,
            e164: verified.kind.is_outbound().then(|| resource.e164.clone()),
            grant: Some(record),
            verified: Some(verified),
            consumed,
        })
    }
}
