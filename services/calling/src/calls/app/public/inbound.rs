//! PSTN_TO_APP (Part E §45): a call from the public network to a relay number, offered by
//! Telephony after it validated the SIP peer, resolved the relay mapping and protected
//! the caller (`POST /internal/v1/telephony/inbound-calls`, `telephony-inbound.offer`).
//!
//! Calling creates ONE canonical Call (origin PUBLIC_INBOUND) — never a Telephony-owned
//! business call: the caller is a public-party principal whose only reference is the
//! opaque ContactReference; the targets are the application principals Privacy Relay
//! currently authorizes (CALLBACK_RELAY route). Endpoints ring through the ordinary
//! Part C path (multi-device, single winner, ANSWERED_ELSEWHERE); once one accepts, the
//! public leg is bridged to the RTC side and the Call is CONNECTED only on Telephony's
//! bridge evidence plus the answerer's media.

use cf_service_auth::{ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{bridge, exec, stmt};
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
        state::CallState,
        transitions::CallAction,
    },
    domain::{ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId},
    platform::principals::{PrincipalReference, ResolvedPrincipal},
    privacy_relay::{
        AuthorizationPurpose, CallAuthorization, PrivacyRelayError, RelayRoute,
        PRIVACY_RELAY_POLICY,
    },
    routing::{
        engine::{
            ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk, EndpointClass,
            Evidence, RouteDecision, RouteDirection, RouteInputs,
        },
        RoutingClass,
    },
    telephony::spend::{self, SpendScope},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundResolution {
    pub decision_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    pub relay_session_id: Uuid,
    pub relay_session_version: i64,
    pub assignment_id: Uuid,
    pub assignment_generation: i64,
    pub valid_until: DateTime<Utc>,
}

/// Telephony's inbound offer (references only — never the called or calling number).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboundOffer {
    pub schema_version: String,
    pub telephony_leg_id: Uuid,
    pub correlation_id: String,
    pub scope: OfferScope,
    pub inbound_resolution: InboundResolution,
    pub caller_contact_reference: Uuid,
    #[serde(default)]
    pub allowed_targets: Vec<Value>,
    pub max_duration_seconds: i32,
}

#[derive(Debug, FromQueryResult)]
struct Owner {
    service_account_id: Uuid,
    credential_id: Uuid,
    kind: String,
}

#[derive(Debug, FromQueryResult)]
struct Existing {
    call_id: Uuid,
    id: Uuid,
}

fn answer(call_id: Uuid, call_leg_id: Uuid) -> Value {
    json!({ "callId": call_id, "callLegId": call_leg_id, "routeClass": "PSTN_TO_APP" })
}

impl AppCallService {
    /// Accepts (or refuses) an inbound public call. Idempotent per Telephony leg.
    pub async fn accept_inbound_offer(&self, offer: &InboundOffer) -> CallResult<Value> {
        if offer.schema_version != "1"
            || !(30..=86_400).contains(&offer.max_duration_seconds)
            || offer.allowed_targets.len() > 64
            || CorrelationId::parse(&offer.correlation_id).is_err()
        {
            return Err(CallError::Validation("malformed inbound offer"));
        }
        // Replay: the same Telephony leg always maps to the same Call / CallLeg.
        if let Some(e) = Existing::find_by_statement(stmt(
            "SELECT call_id, id FROM call_legs WHERE execution_binding_ref = $1",
            vec![offer.telephony_leg_id.into()],
        ))
        .one(self.db())
        .await?
        {
            return Ok(answer(e.call_id, e.id));
        }
        let s = &offer.scope;
        let scope = CallScope::from_parts(
            TenantId::from_uuid(s.tenant_id),
            ApplicationId::from_uuid(s.application_id),
            EnvironmentId::from_uuid(s.environment_id),
        );
        // The environment's explicit inbound owner (opt-in; fail closed).
        let owner = Owner::find_by_statement(stmt(
            "SELECT o.service_account_id, o.credential_id, e.kind FROM call_inbound_owners o \
             JOIN platform_environments e ON e.id = o.environment_id \
             WHERE o.environment_id = $1 AND o.tenant_id = $2 AND o.application_id = $3 AND o.enabled",
            vec![
                s.environment_id.into(),
                s.tenant_id.into(),
                s.application_id.into(),
            ],
        ))
        .one(self.db())
        .await?
        .ok_or(CallError::PublicRouteDenied("INBOUND_NOT_CONFIGURED"))?;
        let provider = self.provider()?.clone();
        // Targets: application principals Privacy Relay listed for this caller.
        let mut target_ids: Vec<Uuid> = offer
            .allowed_targets
            .iter()
            .filter_map(|t| t["principalId"].as_str())
            .filter_map(|t| Uuid::parse_str(t).ok())
            .collect();
        target_ids.sort();
        target_ids.dedup();
        target_ids.truncate(self.config.max_group_participants.saturating_sub(1));
        let mut targets: Vec<ResolvedPrincipal> = Vec::new();
        for id in &target_ids {
            if let Ok(p) = self
                .principals
                .resolve_in(
                    scope.tenant_id(),
                    scope.application_id(),
                    scope.environment_id(),
                    &PrincipalReference::Id(PrincipalId::from_uuid(*id)),
                )
                .await
            {
                targets.push(p);
            }
        }
        if targets.is_empty() {
            return Err(CallError::PublicRouteDenied("NO_APPLICATION_TARGET"));
        }
        let caller = self
            .principals
            .ensure_public_party_principal(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                offer.caller_contact_reference,
            )
            .await
            .map_err(|_| CallError::TargetUnavailable)?;
        // CURRENT authority (not the offer's snapshot): the caller contact may reach the
        // targets over the CALLBACK_RELAY route of this relay session.
        let kind = match owner.kind.as_str() {
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            "TEST" => ApplicationEnvironmentKind::Test,
            _ => ApplicationEnvironmentKind::Sandbox,
        };
        let delegated = DelegatedPlatformContext::new(
            s.tenant_id,
            s.application_id,
            s.environment_id,
            kind,
            owner.service_account_id,
        );
        let correlation = CorrelationId::parse(&offer.correlation_id)
            .unwrap_or_else(|_| CorrelationId::generate());
        let target_uuids: Vec<Uuid> = targets.iter().map(|t| t.id().as_uuid()).collect();
        let decision = self
            .privacy
            .authorize_call(
                &delegated,
                &correlation,
                &CallAuthorization {
                    relay_session_id: offer.inbound_resolution.relay_session_id,
                    source_principal_id: caller.id().as_uuid(),
                    source_contact: Some(offer.caller_contact_reference),
                    target_principal_ids: target_uuids.clone(),
                    target_contacts: Vec::new(),
                    route: RelayRoute::CallbackRelay,
                    purpose: AuthorizationPurpose::CallCreate,
                },
            )
            .await
            .map_err(|e| match e {
                PrivacyRelayError::NotFound => CallError::NotFound,
                PrivacyRelayError::Unavailable => CallError::PrivacyAuthorizationUnavailable,
            })?;
        let fact = decision
            .contact_fact(offer.caller_contact_reference)
            .cloned();
        if !decision.is_authorized_for(offer.inbound_resolution.relay_session_id)
            || decision.context_id != offer.inbound_resolution.context_id
            || fact.is_none()
        {
            tracing::info!(reason = %decision.reason, "inbound public call refused by privacy relay");
            return Err(CallError::AuthorizationDenied);
        }
        // Routing (§37): PSTN_TO_APP only after every gate.
        let now = self.calls.now();
        let mut destinations = Vec::new();
        let probe = self.db().begin().await?;
        for t in &targets {
            let endpoints = self
                .endpoints
                .eligible_in(
                    &probe,
                    scope,
                    t.id().as_uuid(),
                    self.config.max_endpoints_per_participant,
                )
                .await?;
            destinations.push(DestinationFacts {
                class: EndpointClass::Application,
                app_reachable: Evidence::Known(!endpoints.is_empty()),
                contact: None,
            });
        }
        probe.commit().await?;
        let headroom = match spend::headroom(self.db(), s.environment_id, now).await? {
            Some(h) => Evidence::Known(h),
            None => Evidence::Known(0),
        };
        let policy = crate::routing::store::load_policy(self.db(), s.environment_id).await?;
        let inputs = RouteInputs {
            direction: RouteDirection::Inbound,
            source: EndpointClass::PUBLIC_NETWORK,
            source_contact: Some(ContactFacts {
                contact_reference: offer.caller_contact_reference,
                verification: Evidence::Known(ContactVerification::Verified),
                country: match fact.as_ref().and_then(|f| f.country.clone()) {
                    Some(c) => Evidence::Known(c),
                    None => Evidence::Unknown,
                },
            }),
            destinations,
            requested: None,
            authorized: true,
            permitted_classes: vec![RoutingClass::PstnToApp],
            relay_session_id: Some(decision.relay_session_id),
            context_id: Some(decision.context_id),
            live_telecom_allowed: super::outbound::live_telecom_allowed(
                self.db(),
                s.environment_id,
            )
            .await?,
            flags: self.public.flags,
            routes: Evidence::Known(Vec::new()),
            spend_headroom_minor: headroom,
            operational_incident: Evidence::Unknown,
            duplicate_risk: DuplicateRisk::None,
        };
        let mut plan = match crate::routing::engine::select(&policy.policy, &inputs, now) {
            RouteDecision::Selected(plan) => *plan,
            RouteDecision::Denied(denial) => {
                tracing::info!(denial = denial.as_str(), "inbound public route denied");
                return Err(super::outbound::denial_error(denial));
            }
        };
        plan.policy_version = policy.version_label();

        let cmd = CallCommandContext::system(
            scope,
            ServiceAccountId::from_uuid(owner.service_account_id),
        );
        let creator = CallCreator {
            cmd: cmd.clone(),
            service_account_id: ServiceAccountId::from_uuid(owner.service_account_id),
            credential_id: CredentialId::from_uuid(owner.credential_id),
            origin: "PUBLIC_INBOUND",
        };
        let txn = self.db().begin().await?;
        let mut involved = target_uuids.clone();
        involved.push(caller.id().as_uuid());
        self.guard_privacy_decision(&txn, scope, &decision, &involved)
            .await?;
        let (call, _caller_participant, _) = self
            .calls
            .create_call_as_in(
                &txn,
                &creator,
                &caller,
                NewCall {
                    kind: CallKind::Voice,
                    communication_context_ref: Some(decision.context_id),
                    privacy_relay_ref: Some(decision.relay_session_id),
                    initiator_display_name: None,
                },
            )
            .await?;
        let mut policy_snapshot = SafeAttributes::new();
        policy_snapshot.insert("call_kind", SafeValue::Text("VOICE".into()))?;
        policy_snapshot.insert("target_count", SafeValue::from(targets.len()))?;
        policy_snapshot.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
        policy_snapshot.insert("relay_route", SafeValue::Text("CALLBACK_RELAY".into()))?;
        policy_snapshot.insert(
            "inbound_resolution_id",
            SafeValue::Text(offer.inbound_resolution.decision_id.to_string()),
        )?;
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
        for target in &targets {
            let (participant, _) = self
                .calls
                .add_participant_in(&txn, &cmd, &call, target, None)
                .await?;
            self.calls
                .ensure_media_permissions_in(&txn, &call, &participant)
                .await?;
        }
        let call = self.locked(&txn, &call).await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        let mut snapshot = SafeAttributes::new();
        snapshot.insert("reason", SafeValue::Text("INBOUND_RELAY_NUMBER".into()))?;
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
        exec(
            &txn,
            "UPDATE call_legs SET execution_binding_ref = $2 WHERE id = $1 AND execution_binding_ref IS NULL",
            vec![public_leg.id.into(), offer.telephony_leg_id.into()],
        )
        .await?;
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
        exec(
            &txn,
            "INSERT INTO call_public_parties (id, call_id, tenant_id, application_id, environment_id, role, \
               contact_reference, country, call_leg_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, 'CALLER', $6, $7, $8, $9)",
            vec![
                Uuid::new_v4().into(),
                call.id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                offer.caller_contact_reference.into(),
                fact.and_then(|f| f.country).into(),
                public_leg.id.into(),
                now.into(),
            ],
        )
        .await?;
        if let Err(denial) = spend::reserve_in(
            &txn,
            SpendScope {
                tenant_id: call.tenant_id,
                application_id: call.application_id,
                environment_id: call.environment_id,
            },
            call.id,
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
                    max_participants: ((targets.len() + 1) * 2) as i32,
                },
            )
            .await?;
        txn.commit().await?;
        tracing::info!(call_id = %call.id, call_leg_id = %public_leg.id, telephony_leg_id = %offer.telephony_leg_id, "inbound public call accepted (PSTN_TO_APP); ringing application endpoints");
        // Media resource, then ringing of every target (ordinary Part C path).
        self.advance_routing(call.id).await?;
        Ok(answer(call.id, public_leg.id))
    }

    /// After an application participant answered: a PSTN_TO_APP Call bridges its public
    /// leg to the RTC side (the edge answers the caller only when media joins).
    pub(crate) async fn after_application_answer(&self, call_id: Uuid) {
        let result: CallResult<bool> = async {
            let txn = self.db().begin().await?;
            let Some(call) = <crate::models::calls::Entity as sea_orm::EntityTrait>::find_by_id(call_id)
                .one(&txn)
                .await?
            else {
                return Ok(false);
            };
            if call.origin != "PUBLIC_INBOUND" || call.current_state != CallState::Answered {
                txn.commit().await?;
                return Ok(false);
            }
            let row = txn
                .query_one(stmt(
                    "SELECT execution_binding_ref FROM call_legs WHERE call_id = $1 AND leg_kind = 'PUBLIC_NETWORK' \
                     AND direction = 'INBOUND' AND execution_binding_ref IS NOT NULL \
                     AND state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')",
                    vec![call.id.into()],
                ))
                .await?;
            let Some(row) = row else {
                txn.commit().await?;
                return Ok(false);
            };
            let telephony_leg: Uuid = row.try_get_by_index(0)?;
            bridge::request_bridge_in(
                &txn,
                &call,
                "APPLICATION_MEDIA",
                &[telephony_leg],
                self.calls.now(),
            )
            .await?;
            txn.commit().await?;
            Ok(true)
        }
        .await;
        match result {
            Ok(true) => self.dispatch_call(call_id).await,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%call_id, %error, "inbound bridge request pending; reconciliation retries")
            }
        }
    }
}
