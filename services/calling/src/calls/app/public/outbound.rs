//! APP_TO_PSTN (Part E §44): an application participant calls a protected contact over
//! the public network (an opaque ContactReference — never a number).

use chrono::Duration;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use super::{enqueue_in, exec, CommandKind};
use crate::{
    calls::{
        app::{create::CreatedAppCall, AppCallService},
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        authorizations::NewAuthorization,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallAuthorizationId, CallLegId, CallRouteId},
        kinds::{AuthorizationReason, CallKind, LegDirection, LegKind, RouteClass},
        legs::NewLeg,
        participants::DisplayName,
        routes::{NewRoute, PolicyVersion},
        service::{CommandKey, NewCall, NewMediaBinding},
        transitions::CallAction,
    },
    domain::PrincipalId,
    endpoints::CallEndpointId,
    internal_auth::delegation::delegated_context,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, ResolvedPrincipal},
    },
    privacy_relay::{
        AuthorizationPurpose, CallAuthorization, PrivacyDecision, PrivacyRelayError, RelayRoute,
        PRIVACY_RELAY_POLICY,
    },
    routing::{
        engine::{
            ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk, EndpointClass,
            Evidence, RouteCapability, RouteDecision, RouteDenial, RouteDirection, RouteInputs,
            PAID_PLAN_VALIDITY_SECONDS,
        },
        RoutingClass,
    },
    telephony::spend::{self, SpendScope},
};

/// `POST /v1/calls` with a protected contact target.
#[derive(Debug, Clone)]
pub struct CreatePublicCall {
    pub kind: CallKind,
    pub initiator: PrincipalId,
    pub initiator_endpoint: CallEndpointId,
    pub target_contact: Uuid,
    pub initiator_display_name: Option<DisplayName>,
    pub privacy_relay_session: Uuid,
    pub key: CommandKey,
}

/// A route denial as the stable public error.
pub(crate) fn denial_error(denial: RouteDenial) -> CallError {
    match denial {
        RouteDenial::NotAuthorized => CallError::AuthorizationDenied,
        RouteDenial::LiveTelecomNotAllowed => CallError::LiveTelecomNotAllowed,
        RouteDenial::RouteUnavailable | RouteDenial::CapacityExhausted => {
            CallError::PublicRouteUnavailable(denial.as_str())
        }
        RouteDenial::SpendLimitExceeded => CallError::TelecomSpendLimitExceeded(denial.as_str()),
        other => CallError::PublicRouteDenied(other.as_str()),
    }
}

/// Whether the environment may produce real telecom effects (production, active,
/// `live_telecom_allowed`).
pub(crate) async fn live_telecom_allowed<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> CallResult<bool> {
    let row = db
        .query_one(super::stmt(
            "SELECT (live_telecom_allowed AND kind = 'PRODUCTION' AND status = 'ACTIVE') \
             FROM platform_environments WHERE id = $1",
            vec![environment_id.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

impl AppCallService {
    /// Privacy Relay's CURRENT decision for a public-network communication; only an
    /// AUTHORIZED decision for the asked session and route passes (fail closed).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn public_privacy_authority(
        &self,
        ctx: &PlatformAuthContext,
        source: &ResolvedPrincipal,
        relay_session: Uuid,
        source_contact: Option<Uuid>,
        target_contacts: Vec<Uuid>,
        route: RelayRoute,
    ) -> CallResult<PrivacyDecision> {
        let delegated = delegated_context(ctx, Some(source)).map_err(|_| CallError::NotFound)?;
        let decision = self
            .privacy
            .authorize_call(
                &delegated,
                ctx.correlation_id(),
                &CallAuthorization {
                    relay_session_id: relay_session,
                    source_principal_id: source.id().as_uuid(),
                    source_contact,
                    target_principal_ids: Vec::new(),
                    target_contacts: target_contacts.clone(),
                    route,
                    purpose: AuthorizationPurpose::CallCreate,
                },
            )
            .await
            .map_err(|e| match e {
                PrivacyRelayError::NotFound => CallError::NotFound,
                PrivacyRelayError::Unavailable => CallError::PrivacyAuthorizationUnavailable,
            })?;
        let covers_contacts = target_contacts
            .iter()
            .chain(source_contact.iter())
            .all(|c| decision.contact_fact(*c).is_some());
        if !decision.is_authorized_for(relay_session)
            || decision.route.as_deref() != Some(route.as_str())
            || !covers_contacts
        {
            tracing::info!(
                privacy_relay_ref = %relay_session,
                decision_id = %decision.decision_id,
                reason = %decision.reason,
                route = route.as_str(),
                "public-network call refused by privacy relay"
            );
            return Err(CallError::AuthorizationDenied);
        }
        Ok(decision)
    }

    /// `POST /v1/calls` → APP_TO_PSTN.
    pub async fn create_protected_contact_call(
        &self,
        ctx: &PlatformAuthContext,
        request: CreatePublicCall,
    ) -> CallResult<CreatedAppCall> {
        let scope = CallScope::of_platform(ctx);
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            return self.replay_create(scope, prior).await;
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        if request.kind != CallKind::Voice {
            return Err(CallError::CapabilityUnavailable(
                "public-network calls are VOICE only",
            ));
        }
        let initiator = self
            .principals
            .resolve(ctx, &PrincipalReference::Id(request.initiator))
            .await
            .map_err(|e| super::super::create::resolution(e, true))?;
        let initiator_endpoint = self
            .endpoints
            .active_endpoint(scope, request.initiator, request.initiator_endpoint)
            .await?;
        // Current Privacy Relay authority over the PUBLIC_RELAY route (no lock held).
        let decision = self
            .public_privacy_authority(
                ctx,
                &initiator,
                request.privacy_relay_session,
                None,
                vec![request.target_contact],
                RelayRoute::PublicRelay,
            )
            .await?;
        let fact = decision
            .contact_fact(request.target_contact)
            .cloned()
            .ok_or(CallError::AuthorizationDenied)?;
        // Routing inputs gathered before any lock: Telephony's normalized availability,
        // the spend headroom, the environment's live-telecom permission.
        let environment = scope.environment_id().as_uuid();
        let routes = match fact.country.as_deref() {
            Some(country) => {
                self.public
                    .telephony
                    .availability(
                        country,
                        RouteCapability::VoiceOutbound,
                        ctx.correlation_id(),
                    )
                    .await
            }
            None => Evidence::Unknown,
        };
        let now = self.calls.now();
        let headroom = match spend::headroom(self.db(), environment, now).await? {
            Some(h) => Evidence::Known(h),
            None => Evidence::Known(0),
        };
        let live = live_telecom_allowed(self.db(), environment).await?;
        let policy = crate::routing::store::load_policy(self.db(), environment).await?;
        let inputs = RouteInputs {
            direction: RouteDirection::Outbound,
            source: EndpointClass::Application,
            source_contact: None,
            destinations: vec![DestinationFacts::protected_contact(ContactFacts {
                contact_reference: request.target_contact,
                verification: if fact.verification == "VERIFIED" {
                    Evidence::Known(ContactVerification::Verified)
                } else {
                    Evidence::Unknown
                },
                country: match &fact.country {
                    Some(c) => Evidence::Known(c.clone()),
                    None => Evidence::Unknown,
                },
            })],
            requested: Some(RoutingClass::AppToPstn),
            authorized: true,
            permitted_classes: vec![RoutingClass::AppToPstn],
            relay_session_id: Some(decision.relay_session_id),
            context_id: Some(decision.context_id),
            live_telecom_allowed: live,
            flags: self.public.flags,
            routes,
            spend_headroom_minor: headroom,
            operational_incident: Evidence::Unknown,
            duplicate_risk: DuplicateRisk::None,
        };
        let mut plan = match crate::routing::engine::select(&policy.policy, &inputs, now) {
            RouteDecision::Selected(plan) => *plan,
            RouteDecision::Denied(denial) => {
                tracing::info!(denial = denial.as_str(), "public route denied");
                return Err(denial_error(denial));
            }
        };
        plan.policy_version = policy.version_label();

        let cmd = CallCommandContext::customer(ctx);
        let txn = self.db().begin().await?;
        if let Some(prior) = self
            .calls
            .begin_command_in(&txn, scope, &request.key)
            .await?
        {
            txn.rollback().await?;
            return self.replay_create(scope, prior).await;
        }
        self.guard_privacy_decision(&txn, scope, &decision, &[initiator.id().as_uuid()])
            .await?;
        let (call, initiator_participant, _) = self
            .calls
            .create_requested_call_in(
                &txn,
                ctx,
                &initiator,
                NewCall {
                    kind: request.kind,
                    communication_context_ref: Some(decision.context_id),
                    privacy_relay_ref: Some(decision.relay_session_id),
                    initiator_display_name: request.initiator_display_name.clone(),
                },
            )
            .await?;
        self.calls
            .record_command_in(&txn, &cmd, &request.key, Some(call.id), None, None, None)
            .await?;
        let mut policy_snapshot = SafeAttributes::new();
        policy_snapshot.insert("call_kind", SafeValue::Text(request.kind.to_string()))?;
        policy_snapshot.insert("target_count", SafeValue::from(1usize))?;
        policy_snapshot.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
        policy_snapshot.insert("relay_route", SafeValue::Text("PUBLIC_RELAY".into()))?;
        super::super::privacy::privacy_evidence(&mut policy_snapshot, &decision)?;
        let authorization = self
            .calls
            .record_authorization_in(
                &txn,
                &cmd,
                &call,
                NewAuthorization {
                    reason: AuthorizationReason::PolicySatisfied,
                    policy_version: PolicyVersion::parse(PRIVACY_RELAY_POLICY)?,
                    policy_snapshot,
                    limits_snapshot: SafeAttributes::new(),
                },
            )
            .await?;
        self.record_privacy_decision(
            &txn,
            &call,
            &decision,
            AuthorizationPurpose::CallCreate,
            None,
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
        self.calls
            .ensure_media_permissions_in(&txn, &call, &initiator_participant)
            .await?;
        self.calls
            .insert_session(
                &txn,
                &cmd,
                &call,
                &initiator_participant,
                initiator_endpoint.kind.session_category(),
                Some(initiator_endpoint.id.as_uuid()),
                None,
            )
            .await?;
        let call = self.locked(&txn, &call).await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        let mut snapshot = SafeAttributes::new();
        snapshot.insert("reason", SafeValue::Text("PROTECTED_CONTACT_TARGET".into()))?;
        if let Some(c) = &plan.destination_country {
            snapshot.insert("destination_country", SafeValue::Text(c.clone()))?;
        }
        let candidate_routes = plan.candidate_routes.clone();
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
        // The public leg (executed by Telephony), then the application side.
        let public_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(route_id),
                    participant_id: None,
                    direction: LegDirection::Outbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await?;
        let public_attempt = self
            .calls
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
        // The application side (initiator's RTC media): its attempt is the latest, so
        // media-resource outcomes never touch the public attempt.
        let app_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(route_id),
                    participant_id: None,
                    direction: LegDirection::Outbound,
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
        exec(
            &txn,
            "INSERT INTO call_public_parties (id, call_id, tenant_id, application_id, environment_id, role, \
               contact_reference, country, call_leg_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, 'CALLEE', $6, $7, $8, $9)",
            vec![
                Uuid::new_v4().into(),
                call.id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                request.target_contact.into(),
                fact.country.clone().into(),
                public_leg.id.into(),
                now.into(),
            ],
        )
        .await?;
        // Durable spend reservation (serialized on the environment's policy row).
        let reservation = match spend::reserve_in(
            &txn,
            SpendScope {
                tenant_id: call.tenant_id,
                application_id: call.application_id,
                environment_id: call.environment_id,
            },
            call.id,
            public_leg.id,
            Some(request.target_contact),
            now,
        )
        .await?
        {
            Ok(r) => r,
            Err(denial) => {
                txn.rollback().await?;
                tracing::info!(
                    reason = denial.as_str(),
                    "telecom spend reservation refused"
                );
                return Err(CallError::TelecomSpendLimitExceeded(denial.as_str()));
            }
        };
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
        // The ORIGINATE command: complete authority proof, no number, no grant secret
        // (a fresh single-use grant is attached at every submission).
        let valid_until = decision
            .valid_until
            .ok_or(CallError::AuthorizationDenied)?
            .min(now + Duration::seconds(PAID_PLAN_VALIDITY_SECONDS));
        let command_id = Uuid::new_v4();
        let payload = json!({
            "schemaVersion": "1",
            "commandId": command_id,
            "idempotencyKey": format!("originate:{}:{}", public_leg.id, public_attempt.id),
            "correlationId": cmd.correlation_id().as_str(),
            "expectedGeneration": 1,
            "call": {
                "callId": call.id,
                "callLegId": public_leg.id,
                "attemptId": public_attempt.id,
                "routeId": route.id,
                "routeClass": "APP_TO_PSTN",
                "routePolicyVersion": route_policy_version,
            },
            "scope": {
                "tenantId": call.tenant_id,
                "applicationId": call.application_id,
                "environmentId": call.environment_id,
            },
            "authorization": {
                "callAuthorizationId": authorization.id,
                "callPolicyVersion": PRIVACY_RELAY_POLICY,
                "privacyDecisionId": decision.decision_id,
                "contextId": decision.context_id,
                "contextVersion": decision.context_version,
                "relaySessionId": decision.relay_session_id,
                "relaySessionVersion": decision.relay_session_version,
                "decidedAt": decision.decided_at,
                "validUntil": valid_until,
            },
            "destination": {
                "contactReference": request.target_contact,
                "country": fact.country,
            },
            "spend": {
                "reservationId": reservation.id,
                "reservedAmountMinor": reservation.amount_minor,
                "currency": reservation.currency,
                "maxDurationSeconds": reservation.max_duration_seconds,
            },
            "media": "APPLICATION",
            "candidateRoutes": candidate_routes,
        });
        let key = payload["idempotencyKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        enqueue_in(
            &txn,
            &call,
            CommandKind::Originate,
            Some(public_leg.id),
            Some(public_attempt.id),
            None,
            &key,
            payload,
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            call_leg_id = %public_leg.id,
            request_id = %cmd.request_id(),
            "public-network call routed (APP_TO_PSTN); ensuring media resource"
        );
        // Media resource, then DIALING (the ORIGINATE is dispatched once DIALING).
        self.advance_routing(call.id).await?;
        self.dispatch_call(call.id).await;
        let txn = self.db().begin().await?;
        let call = self.locked(&txn, &call).await?;
        let body = self.create_response(&txn, &call).await?;
        txn.commit().await?;
        Ok(CreatedAppCall {
            status: 201,
            body,
            replayed: false,
        })
    }
}
