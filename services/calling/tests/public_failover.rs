//! Provider failover on Calling's side (Part E §51, §142–§152): Calling decides whether
//! another attempt is allowed (the Route Plan's failover policy, before CONNECTED, after
//! a DEFINITE provider-side failure); Telephony decides which interconnect executes it.
//! A failover attempt is a new CallLeg + CallAttempt (chained) + spend reservation +
//! ORIGINATE. UNKNOWN never fails over; the callee's own outcomes never fail over; a
//! late success of the failed attempt never resurrects it; APP_RTC is untouched by
//! public-route trouble.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    routing::{
        engine::{CandidateRoute, Evidence, RouteHealth},
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::{events::TelephonyEvent, spend::SpendPolicy},
};
use common::{
    app::{AppWorld, Device},
    privacy::FakePrivacyRelay,
    scalar_i64,
    telephony::{leg_event, FakeTelephony},
    TestResponse,
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

struct World {
    w: AppWorld,
    telephony: Arc<FakeTelephony>,
    contact: Uuid,
    session: Uuid,
    caller: Device,
}

fn route(r: &str, health: RouteHealth) -> CandidateRoute {
    CandidateRoute {
        route_ref: r.into(),
        health,
        capacity_available: Evidence::Known(true),
        estimated_leg_cost_minor: Evidence::Known(100),
        quality_permille: Evidence::Unknown,
    }
}

async fn world(max_failover_attempts: u8) -> Option<World> {
    world_with(max_failover_attempts, 1).await
}

async fn world_with(max_failover_attempts: u8, max_concurrent_legs: i32) -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    telephony.set_routes(Evidence::Known(vec![
        route("ng-a", RouteHealth::Healthy),
        route("ng-b", RouteHealth::Healthy),
    ]));
    let w = AppWorld::with_public_network(privacy.clone(), telephony.clone(), None).await?;
    privacy.allow(w.w.initiator.id().as_uuid());
    let contact = privacy.add_contact("NG");
    let scope = w.w.scope();
    let env = w.w.consumer.env(EnvironmentKind::Production).id.as_uuid();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::AppToPstn],
            public_calling_enabled: true,
            max_failover_attempts,
            destinations: DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec![],
                default: CountryDecision::Deny,
            },
            ..RoutingPolicy::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    calling::telephony::spend::store_policy(
        w.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: env,
        },
        &SpendPolicy {
            currency: "NGN".into(),
            leg_allowance_minor: 5_000,
            max_leg_seconds: 3600,
            daily_limit_minor: 50_000,
            monthly_limit_minor: 500_000,
            max_concurrent_legs,
            max_originations_per_minute: 20,
            max_per_party_per_hour: 10,
            max_per_principal_per_hour: None,
            max_per_service_account_per_hour: None,
            max_per_context_per_hour: None,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let caller = w.device(&w.w.initiator, "caller-main").await;
    Some(World {
        w,
        telephony,
        contact,
        session: Uuid::new_v4(),
        caller,
    })
}

impl World {
    async fn call(&self, key: &str) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": self.contact,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    async fn event(&self, value: Value) -> &'static str {
        let event: TelephonyEvent = serde_json::from_value(value).unwrap();
        self.w
            .h()
            .state
            .app
            .apply_telephony_event(&event)
            .await
            .unwrap()
            .as_str()
    }

    /// The party's current public leg and its latest attempt.
    async fn current(&self, call: Uuid) -> (Uuid, Uuid) {
        let row = self
            .w
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT p.call_leg_id, a.id FROM call_public_parties p JOIN call_attempts a ON a.leg_id = p.call_leg_id \
                 WHERE p.call_id = $1 ORDER BY a.attempt_number DESC LIMIT 1",
                vec![call.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        (
            row.try_get_by_index(0).unwrap(),
            row.try_get_by_index(1).unwrap(),
        )
    }

    async fn dialing(&self, key: &str) -> (Uuid, Uuid, Uuid, Uuid) {
        let r = self.call(key).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let (leg, attempt) = self.current(call).await;
        let t = self.telephony.telephony_leg_of(leg).unwrap();
        (call, leg, attempt, t)
    }

    async fn scalar(&self, sql: &str) -> i64 {
        scalar_i64(self.w.h().conn(), sql).await
    }
}

fn provider_failure(
    call: Uuid,
    leg: Uuid,
    t: Uuid,
    attempt: Uuid,
    category: &str,
    end: &str,
) -> Value {
    let mut e = leg_event("LEG_FAILED", call, leg, t, Some(attempt), 2);
    e["legStatus"] = json!("FAILED");
    e["definite"] = json!(true);
    e["failureCategory"] = json!(category);
    e["endReason"] = json!(end);
    e
}

#[tokio::test]
async fn a_definite_provider_failure_fails_over_to_a_new_attempt() {
    let Some(x) = world(1).await else { return };
    let (call, leg_a, attempt_a, ta) = x.dialing("fo-1").await;
    let mut caller_signals =
        calling::signaling::RealtimeSignalBus::subscribe(x.w.h().bus.as_ref(), x.caller.endpoint);
    assert_eq!(
        x.event(provider_failure(
            call, leg_a, ta, attempt_a, "NETWORK", "FAILED"
        ))
        .await,
        "APPLIED"
    );
    // Part G §66: the caller learns the route changed on the SAME Call (no provider,
    // route or number detail). (Frames on the socket are contract-checked in `signaling`.)
    let mut route_changed = None;
    while let Some(signal) = caller_signals.try_recv() {
        if signal["type"] == "call.route_changed" {
            route_changed = Some(signal);
        }
    }
    let signal = route_changed.expect("call.route_changed signal");
    assert_eq!(signal["callId"], json!(call));
    assert_eq!(signal["data"], json!({ "failover": true }));
    assert!(!signal.to_string().contains("ng-a") && !signal.to_string().contains("ng-b"));
    // The Call keeps dialing; a NEW leg / attempt / reservation / ORIGINATE exists.
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    let (leg_b, attempt_b) = x.current(call).await;
    assert_ne!(leg_b, leg_a);
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 2, "the failover ORIGINATE was dispatched");
    let b = &originates[1].1;
    assert_eq!(b["call"]["callLegId"], json!(leg_b));
    assert_eq!(b["call"]["attemptId"], json!(attempt_b));
    assert_eq!(b["destination"]["contactReference"], json!(x.contact));
    assert_eq!(b["call"]["routeClass"], "APP_TO_PSTN");
    assert!(
        b["destination"]["grantSecret"].as_str().unwrap().len() >= 32,
        "a fresh grant"
    );
    assert_ne!(
        b["spend"]["reservationId"],
        originates[0].1["spend"]["reservationId"]
    );
    // The attempt chain and the leg / reservation states.
    assert_eq!(
        x.scalar(&format!(
            "SELECT count(*) FROM call_attempts WHERE id = '{attempt_b}' AND previous_attempt_id = '{attempt_a}'"
        ))
        .await,
        1
    );
    assert_eq!(
        x.scalar(&format!(
            "SELECT count(*) FROM call_attempts WHERE id = '{attempt_a}' AND outcome = 'FAILED'"
        ))
        .await,
        1
    );
    assert_eq!(
        x.scalar(&format!(
            "SELECT count(*) FROM call_spend_reservations WHERE call_leg_id = '{leg_a}' AND status = 'RELEASED'"
        ))
        .await,
        1,
        "max one concurrent leg: the failed leg's reservation was released first"
    );
    // The new leg drives the Call.
    let tb = x.telephony.telephony_leg_of(leg_b).unwrap();
    assert_eq!(
        x.event(leg_event(
            "LEG_RINGING",
            call,
            leg_b,
            tb,
            Some(attempt_b),
            1
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Ringing);
    // A late success of the failed attempt never resurrects it.
    let mut late = leg_event("LEG_ANSWERED", call, leg_a, ta, Some(attempt_a), 3);
    late["legStatus"] = json!("ANSWERED");
    assert_ne!(x.event(late).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Ringing);
    // Budget: one failover. The second definite failure ends the Call.
    assert_eq!(
        x.event(provider_failure(
            call, leg_b, tb, attempt_b, "ROUTING", "FAILED"
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Failed);
    assert_eq!(x.telephony.originates().len(), 2);
}

#[tokio::test]
async fn unknown_never_fails_over_but_proven_absence_does() {
    let Some(x) = world(1).await else { return };
    let (call, leg_a, attempt_a, ta) = x.dialing("fo-unknown").await;
    let mut unknown = leg_event("LEG_STATUS_UNKNOWN", call, leg_a, ta, Some(attempt_a), 1);
    unknown["legStatus"] = json!("UNKNOWN");
    unknown["definite"] = json!(false);
    assert_eq!(x.event(unknown).await, "APPLIED");
    assert_eq!(
        x.telephony.originates().len(),
        1,
        "no failover while UNKNOWN"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    // Reconciliation proved the first attempt absent at the provider: now it may.
    assert_eq!(
        x.event(provider_failure(
            call,
            leg_a,
            ta,
            attempt_a,
            "EXECUTION",
            "ABSENT_AT_PROVIDER"
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.telephony.originates().len(), 2);
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
}

#[tokio::test]
async fn callee_outcomes_policy_and_missing_routes_never_fail_over() {
    // (reservations stay held until Telephony's usage fact: allow several calls)
    let Some(x) = world_with(2, 10).await else {
        return;
    };
    // The callee's own answer: busy.
    let (call, leg, attempt, t) = x.dialing("fo-busy").await;
    let mut busy = leg_event("LEG_BUSY", call, leg, t, Some(attempt), 2);
    busy["definite"] = json!(true);
    busy["endReason"] = json!("BUSY");
    x.event(busy).await;
    assert_eq!(x.w.state(call).await.0, CallState::Busy);
    // Target unavailable (a callee-side failure), and "no interconnect left" (NOT_SENT).
    for (key, category, end) in [
        ("fo-target", "TARGET_UNAVAILABLE", "FAILED"),
        ("fo-noroute", "ROUTING", "NOT_SENT"),
        ("fo-policy", "POLICY", "NOT_SENT"),
    ] {
        let (call, leg, attempt, t) = x.dialing(key).await;
        let before = x.telephony.originates().len();
        x.event(provider_failure(call, leg, t, attempt, category, end))
            .await;
        assert_eq!(x.w.state(call).await.0, CallState::Failed, "{key}");
        assert_eq!(x.telephony.originates().len(), before, "{key}: no failover");
    }
}

#[tokio::test]
async fn a_plan_without_failover_fails_the_call() {
    let Some(x) = world(0).await else { return };
    let (call, leg, attempt, t) = x.dialing("fo-none").await;
    x.event(provider_failure(call, leg, t, attempt, "NETWORK", "FAILED"))
        .await;
    assert_eq!(x.w.state(call).await.0, CallState::Failed);
    assert_eq!(x.telephony.originates().len(), 1);
}

#[tokio::test]
async fn public_route_trouble_never_disables_app_calls() {
    let Some(x) = world(1).await else { return };
    // Every public interconnect is down (e.g. circuits open).
    x.telephony.set_routes(Evidence::Known(vec![
        route("ng-a", RouteHealth::Unavailable),
        route("ng-b", RouteHealth::Unavailable),
    ]));
    let r = x.call("fo-down").await;
    assert_eq!(
        r.json["error"]["code"], "PUBLIC_ROUTE_UNAVAILABLE",
        "{}",
        r.text
    );
    // An application call in the same environment works as always.
    let callee = x.w.device(&x.w.w.recipient, "callee-main").await;
    let _ = callee;
    let r =
        x.w.create_call(
            "VOICE",
            &x.w.w.initiator,
            &x.caller,
            &[&x.w.w.recipient],
            "fo-app",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Telephony could not even be asked: APP_RTC needs nothing from it.
    x.telephony.set_routes(Evidence::Unknown);
    let r =
        x.w.create_call(
            "VOICE",
            &x.w.w.initiator,
            &x.caller,
            &[&x.w.w.recipient],
            "fo-app-2",
        )
        .await;
    assert!(
        r.status == StatusCode::CREATED
            || r.json["error"]["code"] == "CONCURRENT_CALL_LIMIT_EXCEEDED",
        "{}",
        r.text
    );
}
