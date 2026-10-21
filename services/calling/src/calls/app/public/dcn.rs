//! Part IV Workstream L: calls to and from Digital Communications Numbers (DCNs).
//!
//! **Inbound** (`POST /internal/v1/telephony/dcn-inbound-calls`, `telephony-inbound.offer`):
//! Telephony offers a call it received on a DCN together with Numbering's signed
//! INBOUND_VOICE grant and the caller's opaque ContactReference (Privacy Relay protected
//! the caller under the same grant). Calling verifies the grant against Numbering's
//! CURRENT state (`grant-verifications`, audience Calling) and routes ONLY to the target
//! Numbering verified — never to a target the offer merely claims:
//!
//! * `PRINCIPAL` — PSTN_TO_APP to that application principal (no Privacy Relay session:
//!   the number belongs to the customer, not to a relay);
//! * `ENTERPRISE_EXTENSION` / `ENTERPRISE_RING_GROUP` / `ENTERPRISE_QUEUE` — the enterprise
//!   router, entered through the extension (for a group / queue: the scope's ACTIVE
//!   extension that publishes it);
//! * anything else — refused (`DCN_TARGET_UNSUPPORTED`): the dialog is rejected.
//!
//! A Numbering outage refuses DCN calls (fail closed); relay-number, enterprise-device and
//! application calls never depend on Numbering.
//!
//! **Outbound** presentation policy ([`AppCallService::dcn_presentation`]): Numbering
//! decides who OWNS a number; Calling decides whether THIS initiating principal may
//! present an organization-owned number — an employee does not get every organizational
//! DCN merely by belonging to the organization.

use cf_service_auth::{CorrelationId, DelegatedPlatformContext};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{exec, stmt};
use crate::{
    calls::{
        app::AppCallService,
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        authorizations::NewAuthorization,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallAuthorizationId, CallLegId, CallRouteId},
        kinds::{AuthorizationReason, CallKind, LegDirection, LegKind, RouteClass},
        legs::NewLeg,
        routes::{NewRoute, PolicyVersion},
        service::{CallCreator, NewCall, NewMediaBinding},
        transitions::CallAction,
    },
    domain::{ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId},
    numbering::{DcnAssignmentView, NumberingError, VerifiedInboundGrant},
    platform::principals::{PrincipalReference, ResolvedPrincipal},
    routing::{
        engine::{
            ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk, EndpointClass,
            Evidence, InterconnectClass, RouteDecision, RouteDirection, RouteInputs,
        },
        RoutingClass,
    },
    telephony::spend,
};

/// Authorization policy label of a DCN-routed Call (Numbering's verified routing).
pub const DCN_ROUTING_POLICY: &str = "DCN_ROUTING_V1";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DcnOfferScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DcnOfferTarget {
    pub kind: String,
    #[serde(rename = "ref")]
    pub target_ref: Uuid,
}

/// Numbering's resolution as relayed by Telephony (claims; verified before use).
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DcnResolution {
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub ownership_class: String,
    pub target: DcnOfferTarget,
    /// Numbering's signed INBOUND_VOICE grant (a bearer token: never logged).
    pub grant: String,
}

impl std::fmt::Debug for DcnResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DcnResolution")
            .field("number_resource_id", &self.number_resource_id)
            .field("routing_revision", &self.routing_revision)
            .finish_non_exhaustive()
    }
}

/// Telephony's DCN inbound offer (references only — never the called or calling number).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DcnInboundOffer {
    pub schema_version: String,
    pub telephony_leg_id: Uuid,
    pub correlation_id: String,
    pub scope: DcnOfferScope,
    pub dcn_resolution: DcnResolution,
    pub caller_contact_reference: Uuid,
    pub max_duration_seconds: i32,
    /// Part IV L closure: the class of the Telephony route the call arrived on (PUBLIC
    /// when absent).
    #[serde(default)]
    pub interconnect_class: InterconnectClass,
}

#[derive(Debug, FromQueryResult)]
pub(crate) struct DcnOwner {
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub chain_active: bool,
}

#[derive(Debug, FromQueryResult)]
struct Existing {
    call_id: Uuid,
    id: Uuid,
}

pub(crate) fn answer(call_id: Uuid, call_leg_id: Uuid) -> Value {
    json!({ "callId": call_id, "callLegId": call_leg_id, "routeClass": "PSTN_TO_APP" })
}

fn numbering_error(e: NumberingError) -> CallError {
    match e {
        NumberingError::Unavailable => {
            crate::metrics::inc(
                "calling_dcn_refusals_total",
                &[("reason", "NUMBERING_UNAVAILABLE")],
            );
            CallError::PublicRouteUnavailable("NUMBERING_UNAVAILABLE")
        }
        NumberingError::Refused(_) => {
            crate::metrics::inc("calling_dcn_refusals_total", &[("reason", "GRANT_REFUSED")]);
            CallError::AuthorizationDenied
        }
    }
}

fn refuse(reason: &'static str) -> CallError {
    crate::metrics::inc("calling_dcn_refusals_total", &[("reason", reason)]);
    CallError::PublicRouteDenied(reason)
}

/// Enterprise target kinds of Numbering's voice routing profile Calling can enter.
fn enterprise_target(kind: &str) -> Option<crate::enterprise::destinations::DestinationType> {
    use crate::enterprise::destinations::DestinationType;
    match kind {
        "ENTERPRISE_EXTENSION" => Some(DestinationType::Extension),
        "ENTERPRISE_RING_GROUP" => Some(DestinationType::RingGroup),
        "ENTERPRISE_QUEUE" => Some(DestinationType::Queue),
        _ => None,
    }
}

impl AppCallService {
    /// Accepts (or refuses) an inbound call to a DCN. Idempotent per Telephony leg.
    pub async fn accept_dcn_offer(&self, offer: &DcnInboundOffer) -> CallResult<Value> {
        let r = &offer.dcn_resolution;
        if offer.schema_version != "1"
            || !(30..=86_400).contains(&offer.max_duration_seconds)
            || CorrelationId::parse(&offer.correlation_id).is_err()
            || r.grant.len() > 4096
            || r.routing_revision < 1
        {
            return Err(CallError::Validation("malformed DCN offer"));
        }
        if let Some(e) = Existing::find_by_statement(stmt(
            "SELECT call_id, id FROM call_legs WHERE execution_binding_ref = $1",
            vec![offer.telephony_leg_id.into()],
        ))
        .one(self.db())
        .await?
        {
            return Ok(answer(e.call_id, e.id));
        }
        crate::metrics::inc("calling_call_requests_total", &[("kind", "dcn_inbound")]);
        self.require_new_calls(false)?;
        let correlation = CorrelationId::parse(&offer.correlation_id)
            .unwrap_or_else(|_| CorrelationId::generate());
        // Numbering's CURRENT verdict on the grant (signature, audience, expiry, revoked,
        // assignment and routing revision still current).
        let verified = self
            .numbering
            .verify_inbound_voice(&r.grant, &correlation)
            .await
            .map_err(numbering_error)?;
        // The relayed claims must be exactly what Numbering verified (a substituted
        // scope, number, revision or target is refused, never "corrected").
        let s = &offer.scope;
        let same = verified.scope.tenant_id == s.tenant_id
            && verified.scope.application_id == s.application_id
            && verified.scope.environment_id == s.environment_id
            && verified.number_resource_id == r.number_resource_id
            && verified.assignment_id == r.assignment_id
            && verified.routing_revision == r.routing_revision
            && verified.target.kind == r.target.kind
            && verified.target.target_ref == r.target.target_ref;
        if !same {
            tracing::warn!(telephony_leg_id = %offer.telephony_leg_id, "DCN offer does not match the verified grant");
            crate::metrics::inc(
                "calling_dcn_refusals_total",
                &[("reason", "GRANT_MISMATCH")],
            );
            return Err(CallError::AuthorizationDenied);
        }
        let scope = CallScope::from_parts(
            TenantId::from_uuid(s.tenant_id),
            ApplicationId::from_uuid(s.application_id),
            EnvironmentId::from_uuid(s.environment_id),
        );
        let interconnect = self
            .dcn_interconnect(
                s.environment_id,
                offer.interconnect_class,
                verified.synthetic,
            )
            .await?;
        let owner = self.dcn_owner(scope).await?;
        if super::super::blocks::contact_blocked_by_application(
            self.db(),
            s.environment_id,
            offer.caller_contact_reference,
        )
        .await?
        {
            return Err(refuse("CALLER_BLOCKED"));
        }
        let caller = self
            .principals
            .ensure_public_party_principal(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                ServiceAccountId::from_uuid(owner.service_account_id),
                offer.caller_contact_reference,
            )
            .await
            .map_err(super::super::create::public_party)?;
        match verified.target.kind.as_str() {
            "PRINCIPAL" => {
                self.accept_dcn_principal(
                    offer,
                    &verified,
                    scope,
                    &owner,
                    &caller,
                    correlation,
                    interconnect,
                )
                .await
            }
            kind => match enterprise_target(kind) {
                Some(destination_type) => {
                    self.accept_dcn_enterprise(
                        offer,
                        &verified,
                        scope,
                        &owner,
                        &caller,
                        destination_type,
                    )
                    .await
                }
                None => Err(refuse("DCN_TARGET_UNSUPPORTED")),
            },
        }
    }

    /// The interconnect a DCN call runs over (Part IV L closure). The synthetic test
    /// interconnect needs ALL of: the route it uses is Telephony's synthetic test route,
    /// Numbering's inventory class of the number is SYNTHETIC, Calling's synthetic switch
    /// is on (local / test runtimes only) and the environment is a SANDBOX / TEST one. A
    /// synthetic number never runs over a real interconnect, nor a real number over a test
    /// route. A real number over a real route is an ordinary paid public call (live
    /// telecom required, unchanged).
    pub(crate) async fn dcn_interconnect(
        &self,
        environment_id: Uuid,
        route: InterconnectClass,
        synthetic_number: bool,
    ) -> CallResult<InterconnectClass> {
        match (route, synthetic_number) {
            (InterconnectClass::Public, false) => Ok(InterconnectClass::Public),
            (InterconnectClass::SyntheticTest, true) => {
                if self
                    .synthetic_interconnect_permitted(environment_id)
                    .await?
                {
                    Ok(InterconnectClass::SyntheticTest)
                } else {
                    Err(refuse("SYNTHETIC_INTERCONNECT_NOT_ALLOWED"))
                }
            }
            _ => Err(refuse("INTERCONNECT_MISMATCH")),
        }
    }

    /// Calling's synthetic switch is on and `environment_id` is an ACTIVE sandbox / test
    /// environment (never a production one, whatever its live-telecom setting).
    pub(crate) async fn synthetic_interconnect_permitted(
        &self,
        environment_id: Uuid,
    ) -> CallResult<bool> {
        if !self.public.synthetic_interconnect {
            return Ok(false);
        }
        let row = self
            .db()
            .query_one(stmt(
                "SELECT (kind IN ('SANDBOX', 'TEST') AND status = 'ACTIVE') FROM platform_environments WHERE id = $1",
                vec![environment_id.into()],
            ))
            .await?;
        Ok(row
            .map(|r| r.try_get_by_index::<bool>(0))
            .transpose()?
            .unwrap_or(false))
    }

    /// The environment's explicit inbound owner (opt-in; its whole chain ACTIVE).
    pub(crate) async fn dcn_owner(&self, scope: CallScope) -> CallResult<DcnOwner> {
        let owner = DcnOwner::find_by_statement(stmt(
            &format!(
                "SELECT o.service_account_id, o.credential_id, {} AS chain_active \
                 FROM call_inbound_owners o \
                 JOIN platform_environments e ON e.id = o.environment_id \
                 JOIN platform_applications a ON a.id = o.application_id \
                 JOIN platform_tenants t ON t.id = o.tenant_id \
                 JOIN platform_service_accounts sa ON sa.id = o.service_account_id \
                 JOIN platform_api_credentials c ON c.id = o.credential_id \
                 WHERE o.environment_id = $1 AND o.tenant_id = $2 AND o.application_id = $3 AND o.enabled",
                super::OWNER_CHAIN_ACTIVE_SQL
            ),
            vec![
                scope.environment_id().as_uuid().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                self.calls.now().into(),
            ],
        ))
        .one(self.db())
        .await?
        .ok_or(CallError::PublicRouteDenied("INBOUND_NOT_CONFIGURED"))?;
        if !owner.chain_active {
            return Err(CallError::PublicRouteDenied("SCOPE_SUSPENDED"));
        }
        Ok(owner)
    }

    /// PSTN_TO_APP to the principal Numbering routes the DCN to.
    #[allow(clippy::too_many_arguments)]
    async fn accept_dcn_principal(
        &self,
        offer: &DcnInboundOffer,
        verified: &VerifiedInboundGrant,
        scope: CallScope,
        owner: &DcnOwner,
        caller: &ResolvedPrincipal,
        correlation: CorrelationId,
        interconnect: InterconnectClass,
    ) -> CallResult<Value> {
        let provider = self.provider()?.clone();
        let target = self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(verified.target.target_ref)),
            )
            .await
            .map_err(|_| refuse("DCN_TARGET_UNAVAILABLE"))?;
        let environment = scope.environment_id().as_uuid();
        if super::super::blocks::principals_blocking_contact(
            self.db(),
            environment,
            offer.caller_contact_reference,
            &[target.id().as_uuid()],
        )
        .await?
        .contains(&target.id().as_uuid())
        {
            return Err(refuse("CALLER_BLOCKED"));
        }
        let now = self.calls.now();
        let probe = self.db().begin().await?;
        let endpoints = self
            .endpoints
            .eligible_in(
                &probe,
                scope,
                target.id().as_uuid(),
                self.config.max_endpoints_per_participant,
            )
            .await?;
        probe.commit().await?;
        let headroom = match spend::headroom(self.db(), environment, now).await? {
            Some(h) => Evidence::Known(h),
            None => Evidence::Known(0),
        };
        let policy = crate::routing::store::load_policy(self.db(), environment).await?;
        let inputs = RouteInputs {
            direction: RouteDirection::Inbound,
            source: EndpointClass::PUBLIC_NETWORK,
            source_contact: Some(ContactFacts {
                contact_reference: offer.caller_contact_reference,
                verification: Evidence::Known(ContactVerification::Verified),
                country: Evidence::Unknown,
            }),
            destinations: vec![DestinationFacts {
                class: EndpointClass::Application,
                app_reachable: Evidence::Known(!endpoints.is_empty()),
                contact: None,
            }],
            requested: None,
            authorized: true,
            permitted_classes: vec![RoutingClass::PstnToApp],
            relay_session_id: None,
            context_id: None,
            live_telecom_allowed: super::outbound::live_telecom_allowed(self.db(), environment)
                .await?,
            interconnect,
            flags: self.public.flags,
            routes: Evidence::Known(Vec::new()),
            spend_headroom_minor: headroom,
            operational_incident: Evidence::Unknown,
            duplicate_risk: DuplicateRisk::None,
            compliance: crate::routing::compliance::facts(
                self.db(),
                environment,
                &self.compliance_overlay,
            )
            .await,
        };
        let mut plan = match crate::routing::engine::select(&policy.policy, &inputs, now) {
            RouteDecision::Selected(plan) => *plan,
            RouteDecision::Denied(denial) => {
                tracing::info!(denial = denial.as_str(), "DCN inbound route denied");
                return Err(super::outbound::denial_error(denial));
            }
        };
        plan.policy_version = policy.version_label();
        let cmd = CallCommandContext::system_correlated(
            scope,
            ServiceAccountId::from_uuid(owner.service_account_id),
            Some(correlation.as_str()),
        );
        let creator = CallCreator {
            cmd: cmd.clone(),
            service_account_id: ServiceAccountId::from_uuid(owner.service_account_id),
            credential_id: CredentialId::from_uuid(owner.credential_id),
            origin: "PUBLIC_INBOUND",
        };
        let txn = self.db().begin().await?;
        let (call, _caller_participant, _) = self
            .calls
            .create_call_as_in(
                &txn,
                &creator,
                caller,
                NewCall {
                    kind: CallKind::Voice,
                    communication_context_ref: None,
                    privacy_relay_ref: None,
                    initiator_display_name: None,
                },
            )
            .await?;
        let authorization = self
            .calls
            .record_authorization_in(
                &txn,
                &cmd,
                &call,
                NewAuthorization {
                    reason: AuthorizationReason::PolicySatisfied,
                    policy_version: PolicyVersion::parse(DCN_ROUTING_POLICY)?,
                    policy_snapshot: dcn_evidence(verified, "PRINCIPAL")?,
                    limits_snapshot: SafeAttributes::new(),
                },
            )
            .await?;
        let call = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                1,
                CallAction::Authorize {
                    authorization_id: CallAuthorizationId::from_uuid(authorization.id),
                },
            )
            .await?
            .call;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginResolution)
            .await?
            .call;
        let (participant, _) = self
            .calls
            .add_participant_in(&txn, &cmd, &call, &target, None)
            .await?;
        self.calls
            .ensure_media_permissions_in(&txn, &call, &participant)
            .await?;
        let call = self.locked(&txn, &call).await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        let mut snapshot = SafeAttributes::new();
        snapshot.insert("reason", SafeValue::Text("INBOUND_DCN".into()))?;
        let route_policy_version = plan.policy_version.clone();
        let route = self
            .calls
            .record_route_in(
                &txn,
                &cmd,
                &call,
                NewRoute {
                    route_class: RouteClass::Mixed,
                    policy_version: PolicyVersion::parse(&route_policy_version)?,
                    decision_snapshot: snapshot,
                    plan: Some(plan),
                },
            )
            .await?;
        let route_id = CallRouteId::from_uuid(route.id);
        let public_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(route_id),
                    participant_id: None,
                    direction: LegDirection::Inbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await?;
        bind_and_record_public_leg(&txn, &call, public_leg.id, offer, now).await?;
        self.calls
            .record_attempt_in(
                &txn,
                &cmd,
                &call,
                NewAttempt {
                    route_id: Some(route_id),
                    leg_id: Some(CallLegId::from_uuid(public_leg.id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        let app_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(route_id),
                    participant_id: None,
                    direction: LegDirection::Inbound,
                    kind: LegKind::Application,
                },
            )
            .await?;
        self.calls
            .record_attempt_in(
                &txn,
                &cmd,
                &call,
                NewAttempt {
                    route_id: Some(route_id),
                    leg_id: Some(CallLegId::from_uuid(app_leg.id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        if let Err(denial) = spend::reserve_in(
            &txn,
            &call,
            public_leg.id,
            Some(offer.caller_contact_reference),
            now,
        )
        .await?
        {
            txn.rollback().await?;
            return Err(CallError::TelecomSpendLimitExceeded(denial.as_str()));
        }
        self.calls
            .record_media_binding_in(
                &txn,
                &call,
                NewMediaBinding {
                    route_id: route.id,
                    leg_id: app_leg.id,
                    adapter: provider.adapter_name(),
                    max_participants: 4,
                },
            )
            .await?;
        txn.commit().await?;
        crate::metrics::inc("calling_dcn_inbound_calls_total", &[("kind", "PRINCIPAL")]);
        tracing::info!(call_id = %call.id, call_leg_id = %public_leg.id, telephony_leg_id = %offer.telephony_leg_id, "DCN inbound call accepted (PSTN_TO_APP); ringing the routed principal");
        self.advance_routing(call.id).await?;
        Ok(answer(call.id, public_leg.id))
    }

    /// Outbound presentation policy for a DCN (create time AND dispatch time): Numbering's
    /// current assignment in the delegated scope, the number active and voice-outbound
    /// usable, and THIS initiating principal entitled to present it:
    ///
    /// * principal-owned (personal / professional): the owner only;
    /// * organization-owned: the principal must be the number's routed voice target, or an
    ///   ACTIVE enterprise principal reached through it (the extension's own principal /
    ///   member, a ring group's member, a queue's ACTIVE member). Organizational scope
    ///   alone never suffices.
    ///
    /// `Err(PublicRouteDenied(CALLER_IDENTITY_NOT_PERMITTED))` otherwise; a Numbering
    /// outage is `PublicRouteUnavailable(NUMBERING_UNAVAILABLE)` (fail closed).
    pub(crate) async fn dcn_presentation(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation: &CorrelationId,
        scope: CallScope,
        principal_id: Uuid,
        number_resource_id: Uuid,
    ) -> CallResult<DcnAssignmentView> {
        let view = match self
            .numbering
            .current_assignment(delegated, correlation, number_resource_id)
            .await
        {
            Ok(v) => v,
            Err(NumberingError::Unavailable) => {
                return Err(numbering_error(NumberingError::Unavailable))
            }
            Err(NumberingError::Refused(_)) => return Err(refuse("CALLER_IDENTITY_NOT_PERMITTED")),
        };
        if view.number_resource_id != number_resource_id
            || !view.number_active
            || !view.voice_outbound
        {
            return Err(refuse("CALLER_IDENTITY_NOT_PERMITTED"));
        }
        let permitted = match view.owner_kind.as_str() {
            "PRINCIPAL" => view.owner_ref == principal_id,
            "ORGANIZATION" => match &view.voice {
                Some(t) => {
                    principal_reached_by(self.db(), scope, &t.kind, t.target_ref, principal_id)
                        .await?
                }
                None => false,
            },
            _ => false,
        };
        if !permitted {
            return Err(refuse("CALLER_IDENTITY_NOT_PERMITTED"));
        }
        Ok(view)
    }
}

/// Whether `principal` is an ACTIVE principal of `scope` reached through the voice target
/// (`kind`, `target`) of an organization-owned number.
pub(crate) async fn principal_reached_by<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    kind: &str,
    target: Uuid,
    principal: Uuid,
) -> CallResult<bool> {
    let values = |extra: Vec<sea_orm::Value>| {
        let mut v: Vec<sea_orm::Value> = vec![
            scope.tenant_id().as_uuid().into(),
            scope.application_id().as_uuid().into(),
            scope.environment_id().as_uuid().into(),
            principal.into(),
        ];
        v.extend(extra);
        v
    };
    // The principal itself must be ACTIVE in the scope (an offboarded member presents
    // nothing).
    let active = "EXISTS (SELECT 1 FROM platform_principals p WHERE p.tenant_id = $1 AND p.application_id = $2 \
                  AND p.environment_id = $3 AND p.id = $4 AND p.status = 'ACTIVE')";
    let sql = match kind {
        "PRINCIPAL" => format!("SELECT {active} AND $4 = $5"),
        "ENTERPRISE_EXTENSION" => format!(
            "SELECT {active} AND EXISTS (SELECT 1 FROM enterprise_extensions x \
               WHERE x.tenant_id = $1 AND x.application_id = $2 AND x.environment_id = $3 AND x.id = $5 \
                 AND x.status = 'ACTIVE' AND ( \
                   (x.destination_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER') AND x.destination_id = $4) \
                   OR (x.destination_type = 'RING_GROUP' AND EXISTS (SELECT 1 FROM enterprise_ring_group_members m \
                        WHERE m.ring_group_id = x.destination_id AND m.member_id = $4 \
                          AND m.member_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER'))) \
                   OR (x.destination_type = 'QUEUE' AND EXISTS (SELECT 1 FROM enterprise_queue_members q \
                        WHERE q.queue_id = x.destination_id AND q.principal_id = $4 AND q.status = 'ACTIVE'))))"
        ),
        "ENTERPRISE_RING_GROUP" => format!(
            "SELECT {active} AND EXISTS (SELECT 1 FROM enterprise_ring_group_members m \
               JOIN enterprise_ring_groups g ON g.id = m.ring_group_id \
               WHERE g.tenant_id = $1 AND g.application_id = $2 AND g.environment_id = $3 AND g.id = $5 \
                 AND g.status = 'ACTIVE' AND m.member_id = $4 AND m.member_type IN ('PRINCIPAL', 'ENTERPRISE_MEMBER'))"
        ),
        "ENTERPRISE_QUEUE" => format!(
            "SELECT {active} AND EXISTS (SELECT 1 FROM enterprise_queue_members q \
               JOIN enterprise_call_queues c ON c.id = q.queue_id \
               WHERE c.tenant_id = $1 AND c.application_id = $2 AND c.environment_id = $3 AND c.id = $5 \
                 AND c.status = 'ACTIVE' AND q.principal_id = $4 AND q.status = 'ACTIVE')"
        ),
        "ENTERPRISE_DEPARTMENT" => format!(
            "SELECT {active} AND EXISTS (SELECT 1 FROM enterprise_department_members d \
               JOIN enterprise_departments e ON e.id = d.department_id \
               WHERE e.tenant_id = $1 AND e.application_id = $2 AND e.environment_id = $3 AND e.id = $5 \
                 AND e.status = 'ACTIVE' AND d.principal_id = $4)"
        ),
        _ => return Ok(false),
    };
    let row = db
        .query_one(stmt(&sql, values(vec![target.into()])))
        .await?;
    Ok(match row {
        Some(r) => r.try_get_by_index::<Option<bool>>(0)?.unwrap_or(false),
        None => false,
    })
}

/// The bounded, number-free evidence of a DCN-routed Call's authorization.
pub(crate) fn dcn_evidence(
    verified: &VerifiedInboundGrant,
    target: &'static str,
) -> CallResult<SafeAttributes> {
    let mut snapshot = SafeAttributes::new();
    snapshot.insert("call_kind", SafeValue::Text("VOICE".into()))?;
    snapshot.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
    snapshot.insert("dcn_target", SafeValue::Text(target.into()))?;
    snapshot.insert(
        "dcn_number_resource_id",
        SafeValue::Text(verified.number_resource_id.to_string()),
    )?;
    snapshot.insert(
        "dcn_assignment_id",
        SafeValue::Text(verified.assignment_id.to_string()),
    )?;
    snapshot.insert(
        "dcn_routing_revision",
        SafeValue::from(verified.routing_revision as usize),
    )?;
    snapshot.insert(
        "dcn_grant_id",
        SafeValue::Text(verified.grant_id.to_string()),
    )?;
    Ok(snapshot)
}

/// Binds the caller's public leg to its Telephony execution and records the caller as a
/// public party (opaque ContactReference; no country — Calling never learns it).
pub(crate) async fn bind_and_record_public_leg<C: ConnectionTrait>(
    txn: &C,
    call: &crate::models::calls::Model,
    public_leg_id: Uuid,
    offer: &DcnInboundOffer,
    now: chrono::DateTime<chrono::Utc>,
) -> CallResult<()> {
    exec(
        txn,
        "UPDATE call_legs SET execution_binding_ref = $2 WHERE id = $1 AND execution_binding_ref IS NULL",
        vec![public_leg_id.into(), offer.telephony_leg_id.into()],
    )
    .await?;
    exec(
        txn,
        "INSERT INTO call_public_parties (id, call_id, tenant_id, application_id, environment_id, role, \
           contact_reference, country, call_leg_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, 'CALLER', $6, NULL, $7, $8)",
        vec![
            Uuid::new_v4().into(),
            call.id.into(),
            call.tenant_id.into(),
            call.application_id.into(),
            call.environment_id.into(),
            offer.caller_contact_reference.into(),
            public_leg_id.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(())
}

impl AppCallService {
    /// Dispatch side of a DCN presentation (Part IV L): re-checks the presentation policy
    /// for the Call's initiator NOW (an offboarded or re-routed member presents nothing)
    /// and obtains Numbering's single-use CALLER_IDENTITY grant for exactly this
    /// origination (`leg:{callLegId}:{commandId}`, the reference Telephony consumes it
    /// with). Returns Telephony's `presentedIdentity` block.
    pub(crate) async fn dcn_presented_identity(
        &self,
        call: &crate::models::calls::Model,
        correlation: &CorrelationId,
        number_resource_id: Uuid,
        call_leg_id: Uuid,
        command_id: Uuid,
    ) -> CallResult<Value> {
        let scope = super::super::scope_of(call);
        let initiator = self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(call.initiator_principal_id)),
            )
            .await
            .map_err(|_| refuse("CALLER_IDENTITY_NOT_PERMITTED"))?;
        let delegated = self.delegated_for(call).await?.with_initiating_principal(
            initiator.id().as_uuid(),
            crate::internal_auth::delegation::principal_kind(initiator.kind()),
        );
        self.dcn_presentation(
            &delegated,
            correlation,
            scope,
            initiator.id().as_uuid(),
            number_resource_id,
        )
        .await?;
        let operation_ref = format!("leg:{}:{}", call_leg_id.simple(), command_id.simple());
        let grant = self
            .numbering
            .presentation_grant(&delegated, correlation, number_resource_id, &operation_ref)
            .await
            .map_err(|e| match e {
                NumberingError::Unavailable => numbering_error(NumberingError::Unavailable),
                NumberingError::Refused(_) => refuse("CALLER_IDENTITY_NOT_PERMITTED"),
            })?;
        tracing::info!(call_id = %call.id, call_leg_id = %call_leg_id, grant_id = %grant.grant_id, "caller-identity grant obtained for a DCN origination");
        Ok(json!({ "numberResourceId": number_resource_id, "grant": grant.grant }))
    }
}
