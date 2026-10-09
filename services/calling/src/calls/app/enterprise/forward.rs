//! Public forwarding of an enterprise Call (Part F §58, §84, §87–§88).
//!
//! ```text
//! router ─▶ ForwardPublic (a registered public contact) ─▶ context FORWARDED, the Call
//!   keeps ringing state, no public leg yet  (inside the routing transaction)
//! after commit / worker ─▶ every Part E gate, outside any lock:
//!   enterprise policy (forwarding + public forwarding + external calling, destination
//!   country list) ─▶ Privacy Relay CURRENT authority (session, PUBLIC_RELAY route,
//!   contact covered, verified) ─▶ routing engine (environment policy, public route flags,
//!   live-telecom permission, Telephony route availability, spend headroom) ─▶ relay
//!   identity
//! ─▶ one transaction under the Call lock: route plan + public leg + attempt + CALLEE
//!   party + durable spend reservation (limits / concurrency) + ORIGINATE command
//! any refusal ─▶ NO_ROUTE / <bounded reason> and the Call fails safely
//! ```
//!
//! Nothing here bypasses Part E: a forward configured by an administrator gets no
//! exemption. The public leg is bridged to the caller's RTC media exactly like an
//! APP_TO_PSTN call. Real PSTN forwarding stays pending the external real-carrier
//! certification (ADR 0016); tests exercise it against a simulated Telephony.

use cf_service_auth::{ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext};
use sea_orm::{ConnectionTrait, EntityTrait, QuerySelect, TransactionTrait};
use uuid::Uuid;

use super::{context_of, exec, record_step, stmt, update_context, ContextRow, EnterprisePhase};
use crate::{
    calls::{
        app::{
            public::{enqueue_originate_in, live_telecom_allowed, OriginateSpec},
            scope_of, system_cmd, AppCallService,
        },
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        command::CallCommandContext,
        errors::CallResult,
        ids::{CallLegId, CallRouteId},
        kinds::{FailureCategory, LegDirection, LegKind, RouteClass},
        legs::NewLeg,
        routes::{NewRoute, PolicyVersion},
        state::CallState,
    },
    enterprise::{
        destinations::{Destination, DestinationType},
        forwarding,
        policy::effective_policy,
    },
    models::{call_invitations, calls},
    privacy_relay::{AuthorizationPurpose, CallAuthorization, PrivacyRelayError, RelayRoute},
    routing::{
        engine::{
            ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk, EndpointClass,
            Evidence, RouteCapability, RouteDecision, RouteDirection, RouteInputs,
        },
        RoutingClass,
    },
    telephony::spend,
};

type Rung = (calls::Model, Vec<call_invitations::Model>);

/// A refused public forward (bounded, safe reason).
struct Refused(&'static str);

impl AppCallService {
    /// Inside the routing transaction: the Call waits (phase FORWARDED) while the Part E
    /// gates run after commit. Only an application (RTC) caller can be forwarded to a
    /// public contact (its media path is the Call's RTC room).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn begin_public_forward_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
        destination: Destination,
    ) -> CallResult<Rung> {
        let now = self.calls.now();
        if call.origin != "APPLICATION" {
            record_step(
                txn,
                call,
                "NO_ROUTE",
                Some(destination),
                "PUBLIC_FORWARD_MEDIA_UNSUPPORTED",
                None,
                now,
            )
            .await?;
            return self
                .enterprise_no_route(txn, cmd, call, context, FailureCategory::Routing)
                .await;
        }
        update_context(
            txn,
            context,
            EnterprisePhase::Forwarded,
            Some(destination),
            None,
            now,
        )
        .await?;
        record_step(
            txn,
            call,
            "FORWARD_PUBLIC",
            Some(destination),
            "AWAITING_PUBLIC_AUTHORITY",
            None,
            now,
        )
        .await?;
        crate::metrics::inc(
            "calling_enterprise_public_forwards_total",
            &[("result", "REQUESTED")],
        );
        let call = self.locked(txn, call).await?;
        Ok((call, Vec::new()))
    }

    /// Worker pass (and after-commit kick): Calls waiting for their public forward.
    pub(crate) async fn public_forward_tick(&self, limit: u64) -> CallResult<usize> {
        let rows = self
            .db()
            .query_all(stmt(
                "SELECT x.call_id FROM enterprise_call_contexts x \
                  WHERE x.phase = 'FORWARDED' AND x.current_destination_type = 'PUBLIC_CONTACT' \
                    AND NOT EXISTS (SELECT 1 FROM call_public_parties p \
                                     WHERE p.call_id = x.call_id AND p.role = 'CALLEE') \
                  ORDER BY x.updated_at LIMIT $1",
                vec![(limit as i64).into()],
            ))
            .await?;
        let mut done = 0;
        for row in rows {
            let call_id: Uuid = row.try_get_by_index(0)?;
            if self.execute_public_forward(call_id).await? {
                done += 1;
            }
        }
        Ok(done)
    }

    /// Runs the Part E gates for one waiting Call and places its public leg, or fails the
    /// Call safely. Idempotent (a placed forward has its CALLEE party).
    pub(crate) async fn execute_public_forward(&self, call_id: Uuid) -> CallResult<bool> {
        let Some(call) = calls::Entity::find_by_id(call_id).one(self.db()).await? else {
            return Ok(false);
        };
        let Some(context) = context_of(self.db(), call_id).await? else {
            return Ok(false);
        };
        let Some(destination) = pending_forward(self.db(), &call, &context).await? else {
            return Ok(false);
        };
        let result = self.gate_and_place(&call, &context, destination).await?;
        match result {
            Ok(placed) => Ok(placed),
            Err(Refused(reason)) => {
                self.refuse_public_forward(call_id, destination, reason)
                    .await?;
                Ok(true)
            }
        }
    }

    async fn gate_and_place(
        &self,
        call: &calls::Model,
        context: &ContextRow,
        destination: Destination,
    ) -> CallResult<Result<bool, Refused>> {
        let scope = scope_of(call);
        // The kill switch stops new public effects (the waiting Call fails safely).
        if !self.enterprise_flags.enterprise_calling {
            return Ok(Err(Refused("ENTERPRISE_CALLING_DISABLED")));
        }
        let policy = effective_policy(self.db(), scope, context.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        // Gate 1: the enterprise policy.
        let rules = &policy.rules;
        if !(rules.forwarding_allowed
            && rules.public_forwarding_allowed
            && rules.external_public_calling_allowed)
        {
            return Ok(Err(Refused("PUBLIC_FORWARDING_NOT_PERMITTED")));
        }
        let contact =
            match forwarding::public_contact_row(self.db(), scope, destination.id, false).await {
                Ok(c) if c.status == "ACTIVE" => c,
                _ => return Ok(Err(Refused("DESTINATION_DISABLED"))),
            };
        // Gate 2: Privacy Relay's CURRENT authority, acting for the application that placed
        // the Call, for the Call's caller towards the protected contact.
        let Some(kind) = environment_kind(self.db(), call.environment_id).await? else {
            return Ok(Err(Refused("ENVIRONMENT_UNAVAILABLE")));
        };
        let delegated = DelegatedPlatformContext::new(
            call.tenant_id,
            call.application_id,
            call.environment_id,
            kind,
            call.created_by_service_account_id,
        );
        let correlation = CorrelationId::generate();
        let decision = match self
            .privacy
            .authorize_call(
                &delegated,
                &correlation,
                &CallAuthorization {
                    relay_session_id: contact.privacy_relay_session_id,
                    source_principal_id: call.initiator_principal_id,
                    source_contact: None,
                    target_principal_ids: Vec::new(),
                    target_contacts: vec![contact.contact_reference],
                    route: RelayRoute::PublicRelay,
                    purpose: AuthorizationPurpose::CallCreate,
                },
            )
            .await
        {
            Ok(d) => d,
            Err(PrivacyRelayError::NotFound) => {
                return Ok(Err(Refused("PRIVACY_AUTHORITY_DENIED")))
            }
            // Fail closed: an unreachable authority never lets a forward through.
            Err(PrivacyRelayError::Unavailable) => {
                return Ok(Err(Refused("PRIVACY_AUTHORITY_UNAVAILABLE")))
            }
        };
        let Some(fact) = decision.contact_fact(contact.contact_reference).cloned() else {
            return Ok(Err(Refused("PRIVACY_AUTHORITY_DENIED")));
        };
        if !decision.is_authorized_for(contact.privacy_relay_session_id)
            || decision.route.as_deref() != Some(RelayRoute::PublicRelay.as_str())
        {
            tracing::info!(
                call_id = %call.id,
                decision_id = %decision.decision_id,
                reason = %decision.reason,
                "public forward refused by privacy relay"
            );
            return Ok(Err(Refused("PRIVACY_AUTHORITY_DENIED")));
        }
        // Gate 3: the contact is verified and its country is one the enterprise allows.
        if fact.verification != "VERIFIED" {
            return Ok(Err(Refused("CONTACT_NOT_VERIFIED")));
        }
        let Some(country) = fact.country.clone() else {
            return Ok(Err(Refused("DESTINATION_COUNTRY_UNKNOWN")));
        };
        if !rules.public_destination_countries.contains(&country) {
            return Ok(Err(Refused("DESTINATION_COUNTRY_NOT_PERMITTED")));
        }
        // Gate 4: the environment's routing policy with every Part E input (public route
        // flags, live-telecom permission, Telephony route availability, spend headroom).
        let environment = call.environment_id;
        let routes = self
            .public
            .telephony
            .availability(&country, RouteCapability::VoiceOutbound, &correlation)
            .await;
        let now = self.calls.now();
        let headroom = match spend::headroom(self.db(), environment, now).await? {
            Some(h) => Evidence::Known(h),
            None => Evidence::Known(0),
        };
        let live = live_telecom_allowed(self.db(), environment).await?;
        let routing_policy = crate::routing::store::load_policy(self.db(), environment).await?;
        let inputs = RouteInputs {
            direction: RouteDirection::Outbound,
            source: EndpointClass::Application,
            source_contact: None,
            destinations: vec![DestinationFacts::protected_contact(ContactFacts {
                contact_reference: contact.contact_reference,
                verification: Evidence::Known(ContactVerification::Verified),
                country: Evidence::Known(country.clone()),
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
        let mut plan = match crate::routing::engine::select(&routing_policy.policy, &inputs, now) {
            RouteDecision::Selected(plan) => *plan,
            RouteDecision::Denied(denial) => {
                crate::metrics::inc(
                    "calling_public_route_denials_total",
                    &[("reason", denial.as_str())],
                );
                return Ok(Err(Refused(denial.as_str())));
            }
        };
        plan.policy_version = routing_policy.version_label();
        // Gate 5: the relay identity the forwarded-to party sees.
        if self
            .privacy
            .ensure_relay_identity(
                &delegated,
                &correlation,
                decision.relay_session_id,
                &country,
            )
            .await
            .is_err()
        {
            return Ok(Err(Refused("RELAY_IDENTITY_UNAVAILABLE")));
        }

        // Every gate passed: one transaction under the Call lock.
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call.id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            txn.rollback().await?;
            return Ok(Ok(false));
        };
        let Some(context) = context_of(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(Ok(false));
        };
        if pending_forward(&txn, &call, &context).await? != Some(destination) {
            txn.rollback().await?;
            return Ok(Ok(false));
        }
        if self
            .guard_privacy_decision(&txn, scope, &decision, &[call.initiator_principal_id])
            .await
            .is_err()
        {
            txn.rollback().await?;
            return Ok(Err(Refused("PRIVACY_AUTHORITY_DENIED")));
        }
        let Some(authorization_id) = effective_authorization(&txn, call.id).await? else {
            txn.rollback().await?;
            return Ok(Err(Refused("NOT_AUTHORIZED")));
        };
        // From now on the Call is a protected communication of that context / session:
        // Privacy Relay's active-call enforcement (Part D) reaches it like any other.
        let bound = exec(
            &txn,
            "UPDATE calls SET communication_context_ref = $2, privacy_relay_ref = $3, \
               updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1 AND (communication_context_ref IS NULL OR communication_context_ref = $2) \
               AND (privacy_relay_ref IS NULL OR privacy_relay_ref = $3)",
            vec![
                call.id.into(),
                decision.context_id.into(),
                decision.relay_session_id.into(),
                now.into(),
            ],
        )
        .await?;
        if bound != 1 {
            txn.rollback().await?;
            return Ok(Err(Refused("PRIVACY_CONTEXT_MISMATCH")));
        }
        let cmd = system_cmd(&call);
        let mut snapshot = SafeAttributes::new();
        snapshot.insert(
            "reason",
            SafeValue::Text("ENTERPRISE_PUBLIC_FORWARD".into()),
        )?;
        snapshot.insert("destination_country", SafeValue::Text(country.clone()))?;
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
        let attempt = self
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
                contact.contact_reference.into(),
                Some(country.clone()).into(),
                public_leg.id.into(),
                now.into(),
            ],
        )
        .await?;
        // Durable spend reservation: limits and concurrency (serialized per environment).
        let reservation = match spend::reserve_in(
            &txn,
            &call,
            public_leg.id,
            Some(contact.contact_reference),
            now,
        )
        .await?
        {
            Ok(r) => r,
            Err(denial) => {
                txn.rollback().await?;
                return Ok(Err(Refused(denial.as_str())));
            }
        };
        self.record_privacy_decision(
            &txn,
            &call,
            &decision,
            AuthorizationPurpose::CallCreate,
            None,
        )
        .await?;
        enqueue_originate_in(
            &txn,
            &OriginateSpec {
                call: &call,
                call_leg_id: public_leg.id,
                attempt_id: attempt.id,
                route_id: route.id,
                route_class: "APP_TO_PSTN",
                route_policy_version: &route_policy_version,
                authorization_id,
                decision: &decision,
                contact_reference: contact.contact_reference,
                country: Some(&country),
                reservation: &reservation,
                media: "APPLICATION",
                candidate_routes: &candidate_routes,
                correlation_id: cmd.correlation_id().as_str(),
                generation: 1,
            },
            now,
        )
        .await?;
        record_step(
            &txn,
            &call,
            "FORWARD_PUBLIC",
            Some(destination),
            "ORIGINATING",
            None,
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_public_forwards_total",
            &[("result", "ORIGINATING")],
        );
        tracing::info!(call_id = %call.id, call_leg_id = %public_leg.id, "enterprise call forwarded to a public contact");
        self.dispatch_call(call.id).await;
        Ok(Ok(true))
    }

    /// A refused forward: NO_ROUTE evidence and the Call fails safely (never left waiting).
    async fn refuse_public_forward(
        &self,
        call_id: Uuid,
        destination: Destination,
        reason: &'static str,
    ) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let Some(call) = calls::Entity::find_by_id(call_id)
            .lock_exclusive()
            .one(&txn)
            .await?
        else {
            return Ok(());
        };
        let Some(context) = context_of(&txn, call_id).await? else {
            return Ok(());
        };
        if pending_forward(&txn, &call, &context).await? != Some(destination) {
            txn.rollback().await?;
            return Ok(());
        }
        let now = self.calls.now();
        tracing::info!(call_id = %call_id, reason, "enterprise public forward refused");
        crate::metrics::inc(
            "calling_enterprise_public_forwards_total",
            &[("result", "REFUSED")],
        );
        record_step(
            &txn,
            &call,
            "NO_ROUTE",
            Some(destination),
            reason,
            None,
            now,
        )
        .await?;
        let cmd = system_cmd(&call);
        let (call, _) = self
            .enterprise_no_route(&txn, &cmd, &call, &context, FailureCategory::Routing)
            .await?;
        let mut pending = crate::calls::app::PendingSignals::default();
        self.call_signal(
            &txn,
            &mut pending,
            crate::signaling::signals::SignalType::Failed,
            &call,
            cmd.correlation_id().as_str(),
            serde_json::Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        self.dispatch_call(call.id).await;
        Ok(())
    }
}

impl AppCallService {
    /// The forwarded-to public party answered (inside the event's transaction): the
    /// enterprise outcome is FORWARDED_PUBLIC.
    pub(crate) async fn enterprise_public_answered_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<()> {
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(());
        };
        if context.phase != EnterprisePhase::Forwarded.as_str() || context.outcome.is_some() {
            return Ok(());
        }
        let now = self.calls.now();
        update_context(
            txn,
            &context,
            EnterprisePhase::Forwarded,
            None,
            Some("FORWARDED_PUBLIC"),
            now,
        )
        .await?;
        record_step(
            txn,
            call,
            "FORWARD_PUBLIC",
            context.destination(),
            "ANSWERED",
            None,
            now,
        )
        .await?;
        crate::metrics::inc(
            "calling_enterprise_call_outcomes_total",
            &[("outcome", "FORWARDED_PUBLIC")],
        );
        Ok(())
    }
}

/// The public contact a Call still waits to be forwarded to (None: nothing pending).
async fn pending_forward<C: ConnectionTrait>(
    db: &C,
    call: &calls::Model,
    context: &ContextRow,
) -> CallResult<Option<Destination>> {
    if call.current_state.is_terminal()
        || !matches!(call.current_state, CallState::Dialing | CallState::Ringing)
        || context.phase != EnterprisePhase::Forwarded.as_str()
    {
        return Ok(None);
    }
    let Some(destination) = context
        .destination()
        .filter(|d| d.kind == DestinationType::PublicContact)
    else {
        return Ok(None);
    };
    let placed = db
        .query_one(stmt(
            "SELECT 1 FROM call_public_parties WHERE call_id = $1 AND role = 'CALLEE'",
            vec![call.id.into()],
        ))
        .await?
        .is_some();
    Ok((!placed).then_some(destination))
}

async fn environment_kind<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> CallResult<Option<ApplicationEnvironmentKind>> {
    let row = db
        .query_one(stmt(
            "SELECT kind FROM platform_environments WHERE id = $1 AND status = 'ACTIVE'",
            vec![environment_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: String = row.try_get_by_index(0)?;
    Ok(Some(match kind.as_str() {
        "PRODUCTION" => ApplicationEnvironmentKind::Production,
        "TEST" => ApplicationEnvironmentKind::Test,
        _ => ApplicationEnvironmentKind::Sandbox,
    }))
}

/// The Call's effective APPROVED authorization.
async fn effective_authorization<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<Option<Uuid>> {
    let row = db
        .query_one(stmt(
            "SELECT id FROM call_authorizations WHERE call_id = $1 AND decision = 'APPROVED' \
               AND superseded_at IS NULL ORDER BY decided_at DESC LIMIT 1",
            vec![call_id.into()],
        ))
        .await?;
    Ok(row.map(|r| r.try_get_by_index::<Uuid>(0)).transpose()?)
}
