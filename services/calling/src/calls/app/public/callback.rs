//! CALLBACK_RELAY (Part E §47): for low-connectivity users, CF Meet calls the requester's
//! own verified contact (leg A) and, once A ANSWERED, the protected callee (leg B), then
//! bridges the two. Only through the normal Calling API, only when the callback switch,
//! the routing policy and Privacy Relay (CALLBACK_RELAY) allow it, with both contacts
//! verified, both legs' spend reserved and an idempotency key. The second leg is never
//! dialed before the first answered; if it fails, the first leg is released.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{exec, stmt};
use crate::{
    calls::{
        app::{create::CreatedAppCall, AppCallService},
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        authorizations::NewAuthorization,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallAuthorizationId, CallLegId, CallRouteId},
        kinds::{AuthorizationReason, CallKind, LegDirection, LegKind, RouteClass, RouteState},
        legs::NewLeg,
        routes::{NewRoute, PolicyVersion},
        service::{route_of, CommandKey, NewCall},
        transitions::CallAction,
    },
    domain::PrincipalId,
    models::calls,
    platform::{context::PlatformAuthContext, principals::PrincipalReference},
    privacy_relay::{ContactFact, PrivacyDecision, RelayRoute, PRIVACY_RELAY_POLICY},
    routing::{
        engine::{
            ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk, EndpointClass,
            Evidence, RouteCapability, RouteDecision, RouteDirection, RouteInputs,
            PAID_PLAN_VALIDITY_SECONDS,
        },
        RoutingClass,
    },
    telephony::spend,
};

/// `POST /v1/calls` with `route: CALLBACK_RELAY`.
#[derive(Debug, Clone)]
pub struct CreateCallbackRelay {
    /// The application principal requesting the callback.
    pub initiator: PrincipalId,
    /// The requester's own verified protected contact (called first).
    pub initiator_contact: Uuid,
    /// The protected callee (called once the first leg answered).
    pub target_contact: Uuid,
    pub privacy_relay_session: Uuid,
    pub key: CommandKey,
}

fn facts(contact: Uuid, f: &ContactFact) -> ContactFacts {
    ContactFacts {
        contact_reference: contact,
        verification: if f.verification == "VERIFIED" {
            Evidence::Known(ContactVerification::Verified)
        } else {
            Evidence::Unknown
        },
        country: match &f.country {
            Some(c) => Evidence::Known(c.clone()),
            None => Evidence::Unknown,
        },
    }
}

#[derive(Debug, FromQueryResult)]
struct StoredDecision {
    communication_context_ref: Uuid,
    privacy_relay_ref: Uuid,
    context_version: i64,
    session_version: i64,
    decision_ref: Uuid,
    strategy: String,
    policy_version: String,
    valid_until: DateTime<Utc>,
    decided_at: DateTime<Utc>,
}

impl AppCallService {
    pub async fn create_callback_relay(
        &self,
        ctx: &PlatformAuthContext,
        request: CreateCallbackRelay,
    ) -> CallResult<CreatedAppCall> {
        let scope = CallScope::of_platform(ctx);
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            return self.replay_create(scope, prior).await;
        }
        self.require_new_calls(false)?;
        if request.initiator_contact == request.target_contact {
            return Err(CallError::Validation(
                "initiatorContactReference and targetContactReference must differ.",
            ));
        }
        let initiator = self
            .principals
            .resolve(ctx, &PrincipalReference::Id(request.initiator))
            .await
            .map_err(|e| super::super::create::resolution(e, true))?;
        if !initiator.kind().may_initiate_application_call() {
            return Err(CallError::PrincipalCategoryNotAllowed);
        }
        // Part H §70: a block on the callee contact refuses the relay before Privacy Relay.
        self.refuse_blocked_contact(scope, initiator.id().as_uuid(), request.target_contact)
            .await?;
        let decision = self
            .public_privacy_authority(
                ctx,
                &initiator,
                request.privacy_relay_session,
                Some(request.initiator_contact),
                vec![request.target_contact],
                RelayRoute::CallbackRelay,
            )
            .await?;
        let (Some(fa), Some(fb)) = (
            decision.contact_fact(request.initiator_contact).cloned(),
            decision.contact_fact(request.target_contact).cloned(),
        ) else {
            return Err(CallError::AuthorizationDenied);
        };
        let environment = scope.environment_id().as_uuid();
        let routes = match fa.country.as_deref() {
            Some(c) => {
                self.public
                    .telephony
                    .availability(c, RouteCapability::VoiceOutbound, ctx.correlation_id())
                    .await
            }
            None => Evidence::Unknown,
        };
        let now = self.calls.now();
        let headroom = match spend::headroom(self.db(), environment, now).await? {
            Some(h) => Evidence::Known(h),
            None => Evidence::Known(0),
        };
        let live = super::outbound::live_telecom_allowed(self.db(), environment).await?;
        let policy = crate::routing::store::load_policy(self.db(), environment).await?;
        let inputs = RouteInputs {
            direction: RouteDirection::Outbound,
            source: EndpointClass::PUBLIC_NETWORK,
            source_contact: Some(facts(request.initiator_contact, &fa)),
            destinations: vec![DestinationFacts::protected_contact(facts(
                request.target_contact,
                &fb,
            ))],
            requested: Some(RoutingClass::CallbackRelay),
            authorized: true,
            permitted_classes: vec![RoutingClass::CallbackRelay],
            relay_session_id: Some(decision.relay_session_id),
            context_id: Some(decision.context_id),
            live_telecom_allowed: live,
            flags: self.public.flags,
            routes,
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
                tracing::info!(denial = denial.as_str(), "callback relay route denied");
                self.note_route_denial(scope, initiator.id().as_uuid(), denial, now)
                    .await;
                return Err(super::outbound::denial_error(denial));
            }
        };
        plan.policy_version = policy.version_label();
        // Both parties see the session's relay identity (never each other's number).
        self.ensure_relay_identity(
            ctx,
            &initiator,
            decision.relay_session_id,
            fa.country.as_deref(),
        )
        .await?;
        let candidate_routes = plan.candidate_routes.clone();
        let party_a = self
            .principals
            .ensure_public_party_principal(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                request.initiator_contact,
            )
            .await
            .map_err(|_| CallError::TargetUnavailable)?;
        let party_b = self
            .principals
            .ensure_public_party_principal(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                request.target_contact,
            )
            .await
            .map_err(|_| CallError::TargetUnavailable)?;

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
        let (call, _, _) = self
            .calls
            .create_requested_call_in(
                &txn,
                ctx,
                &initiator,
                NewCall {
                    kind: CallKind::Voice,
                    communication_context_ref: Some(decision.context_id),
                    privacy_relay_ref: Some(decision.relay_session_id),
                    initiator_display_name: None,
                },
            )
            .await?;
        self.calls
            .record_command_in(&txn, &cmd, &request.key, Some(call.id), None, None, None)
            .await?;
        let mut policy_snapshot = SafeAttributes::new();
        policy_snapshot.insert("call_kind", SafeValue::Text("VOICE".into()))?;
        policy_snapshot.insert("target_count", SafeValue::from(1usize))?;
        policy_snapshot.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
        policy_snapshot.insert("relay_route", SafeValue::Text("CALLBACK_RELAY".into()))?;
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
            crate::privacy_relay::AuthorizationPurpose::CallCreate,
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
        for party in [&party_a, &party_b] {
            self.calls
                .add_participant_in(&txn, &cmd, &call, party, None)
                .await?;
        }
        let call = self.locked(&txn, &call).await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        let mut snapshot = SafeAttributes::new();
        snapshot.insert("reason", SafeValue::Text("CALLBACK_RELAY_REQUESTED".into()))?;
        let route_policy_version = plan.policy_version.clone();
        let route = self
            .calls
            .record_route_in(
                &txn,
                &cmd,
                &call,
                NewRoute {
                    route_class: RouteClass::PublicNetwork,
                    policy_version: PolicyVersion::parse(&route_policy_version)?,
                    decision_snapshot: snapshot,
                    plan: Some(plan),
                },
            )
            .await?;
        let route_id = CallRouteId::from_uuid(route.id);
        let mut legs = Vec::new();
        for _ in 0..2 {
            legs.push(
                self.calls
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
                    .await?,
            );
        }
        let (leg_a, leg_b) = (&legs[0], &legs[1]);
        let attempt_a = self
            .calls
            .record_attempt_in(
                &txn,
                &cmd,
                &call,
                NewAttempt {
                    route_id: Some(route_id),
                    leg_id: Some(CallLegId::from_uuid(leg_a.id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        for (role, contact, country, leg) in [
            (
                "CALLER",
                request.initiator_contact,
                fa.country.clone(),
                leg_a.id,
            ),
            (
                "CALLEE",
                request.target_contact,
                fb.country.clone(),
                leg_b.id,
            ),
        ] {
            exec(
                &txn,
                "INSERT INTO call_public_parties (id, call_id, tenant_id, application_id, environment_id, role, \
                   contact_reference, country, call_leg_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                vec![
                    Uuid::new_v4().into(),
                    call.id.into(),
                    call.tenant_id.into(),
                    call.application_id.into(),
                    call.environment_id.into(),
                    role.into(),
                    contact.into(),
                    country.into(),
                    leg.into(),
                    now.into(),
                ],
            )
            .await?;
        }
        // Both legs' allowance is reserved now: the second leg is never refused for
        // spend after the requester already answered.
        let mut reservation_a = None;
        for (leg, contact) in [
            (leg_a.id, request.initiator_contact),
            (leg_b.id, request.target_contact),
        ] {
            match spend::reserve_in(&txn, &call, leg, Some(contact), now).await? {
                Ok(r) => {
                    if leg == leg_a.id {
                        reservation_a = Some(r);
                    }
                }
                Err(denial) => {
                    txn.rollback().await?;
                    spend::notify_refusal(self.db(), &call, denial, now).await;
                    return Err(CallError::TelecomSpendLimitExceeded(denial.as_str()));
                }
            }
        }
        let reservation_a =
            reservation_a.ok_or(CallError::Conflict("spend reservation missing"))?;
        super::outbound::enqueue_originate_in(
            &txn,
            &super::outbound::OriginateSpec {
                call: &call,
                call_leg_id: leg_a.id,
                attempt_id: attempt_a.id,
                route_id: route.id,
                route_class: "CALLBACK_RELAY",
                route_policy_version: &route_policy_version,
                authorization_id: authorization.id,
                decision: &decision,
                contact_reference: request.initiator_contact,
                country: fa.country.as_deref(),
                reservation: &reservation_a,
                media: "NONE",
                candidate_routes: &candidate_routes,
                correlation_id: cmd.correlation_id().as_str(),
                generation: 1,
            },
            now,
        )
        .await?;
        let route_row = route_of(&txn, &call, route_id).await?;
        self.calls
            .move_route_in(&txn, &cmd, &call, route_row, RouteState::Active)
            .await?;
        let call = self.locked(&txn, &call).await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginDialing)
            .await?
            .call;
        let body = self.create_response(&txn, &call).await?;
        self.calls
            .complete_call_command_in(
                &txn,
                call.id,
                crate::calls::kinds::CommandOperation::CreateCall,
                201,
                &body,
                Some(call.version),
            )
            .await?;
        txn.commit().await?;
        tracing::info!(call_id = %call.id, "callback relay requested; dialing the requester's contact first");
        self.dispatch_call(call.id).await;
        Ok(CreatedAppCall {
            status: 201,
            body,
            replayed: false,
        })
    }

    /// The first leg of a callback relay answered: dial the callee now (once). Inside
    /// the event's transaction (Call row locked, so this is serialized and idempotent).
    /// Current authority is re-checked when the grant is requested at dispatch.
    pub(crate) async fn originate_callee_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<bool> {
        #[derive(FromQueryResult)]
        struct Callee {
            contact_reference: Uuid,
            country: Option<String>,
            call_leg_id: Uuid,
        }
        let Some(callee) = Callee::find_by_statement(stmt(
            "SELECT p.contact_reference, p.country, p.call_leg_id FROM call_public_parties p \
             JOIN call_legs l ON l.id = p.call_leg_id \
             WHERE p.call_id = $1 AND p.role = 'CALLEE' AND l.direction = 'OUTBOUND' AND l.state = 'CREATED' \
               AND NOT EXISTS (SELECT 1 FROM call_telephony_commands c WHERE c.call_leg_id = p.call_leg_id \
                               AND c.kind = 'ORIGINATE')",
            vec![call.id.into()],
        ))
        .one(txn)
        .await?
        else {
            return Ok(false);
        };
        let stored = StoredDecision::find_by_statement(stmt(
            "SELECT communication_context_ref, privacy_relay_ref, context_version, session_version, decision_ref, \
               strategy, policy_version, valid_until, decided_at FROM call_privacy_authorizations \
             WHERE call_id = $1 AND purpose = 'CALL_CREATE' ORDER BY created_at DESC LIMIT 1",
            vec![call.id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(CallError::AuthorizationRequired)?;
        let now = self.calls.now();
        if stored.valid_until <= now {
            return Err(CallError::AuthorizationDenied);
        }
        let decision = PrivacyDecision {
            decision_id: stored.decision_ref,
            decision: "AUTHORIZED".into(),
            reason: "AUTHORIZED".into(),
            context_id: stored.communication_context_ref,
            context_version: stored.context_version,
            relay_session_id: stored.privacy_relay_ref,
            relay_session_version: stored.session_version,
            strategy: stored.strategy,
            policy_version: stored.policy_version,
            valid_until: Some(
                stored
                    .valid_until
                    .min(now + Duration::seconds(PAID_PLAN_VALIDITY_SECONDS)),
            ),
            decided_at: stored.decided_at,
            route: Some("CALLBACK_RELAY".into()),
            contact_facts: Vec::new(),
        };
        let authorization = self
            .calls
            .effective_authorization(txn, call)
            .await?
            .ok_or(CallError::AuthorizationRequired)?;
        #[derive(FromQueryResult)]
        struct RouteRow {
            id: Uuid,
            policy_version: String,
        }
        let route = RouteRow::find_by_statement(stmt(
            "SELECT r.id, r.policy_version FROM call_routes r JOIN call_legs l ON l.route_id = r.id WHERE l.id = $1",
            vec![callee.call_leg_id.into()],
        ))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)?;
        let reservation = spend::reservation_of(txn, callee.call_leg_id)
            .await?
            .ok_or(CallError::TelecomSpendLimitExceeded(
                "SPEND_RESERVATION_MISSING",
            ))?;
        let cmd = super::super::system_cmd(call);
        let attempt = self
            .calls
            .record_attempt_in(
                txn,
                &cmd,
                call,
                NewAttempt {
                    route_id: Some(CallRouteId::from_uuid(route.id)),
                    leg_id: Some(CallLegId::from_uuid(callee.call_leg_id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        let plan_routes: Vec<String> = txn
            .query_one(stmt(
                "SELECT COALESCE(route_plan -> 'candidateRoutes', '[]'::jsonb) FROM call_routes WHERE id = $1",
                vec![route.id.into()],
            ))
            .await?
            .and_then(|r| r.try_get_by_index::<serde_json::Value>(0).ok())
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        super::outbound::enqueue_originate_in(
            txn,
            &super::outbound::OriginateSpec {
                call,
                call_leg_id: callee.call_leg_id,
                attempt_id: attempt.id,
                route_id: route.id,
                route_class: "CALLBACK_RELAY",
                route_policy_version: &route.policy_version,
                authorization_id: authorization.id,
                decision: &decision,
                contact_reference: callee.contact_reference,
                country: callee.country.as_deref(),
                reservation: &reservation,
                media: "NONE",
                candidate_routes: &plan_routes,
                correlation_id: cmd.correlation_id().as_str(),
                generation: 1,
            },
            now,
        )
        .await?;
        tracing::info!(call_id = %call.id, call_leg_id = %callee.call_leg_id, "callback relay: first leg answered; dialing the callee");
        Ok(true)
    }
}
