//! Phone-to-phone privacy relay (Part E §46) on Calling's side: ONE Call, two public
//! CallLegs (the inbound caller's and the dialed protected callee's), two Telephony legs,
//! a PUBLIC_LEGS bridge; CONNECTED only when both legs are connected and the bridge is
//! active; hangup of either side ends the other; per-leg evidence and usage; neither
//! party's number ever reaches Calling.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::{events::TelephonyEvent, spend::SpendPolicy, LegControl},
};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use common::{
    app::AppWorld,
    privacy::FakePrivacyRelay,
    scalar_i64,
    telephony::{leg_event, FakeTelephony},
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

struct World {
    w: AppWorld,
    privacy: Arc<FakePrivacyRelay>,
    telephony: Arc<FakeTelephony>,
    a: Uuid,
    b: Uuid,
}

async fn world() -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let w = AppWorld::with_public_network(privacy.clone(), telephony.clone(), None).await?;
    let a = privacy.add_contact("NG");
    let b = privacy.add_contact("NG");
    let scope = w.w.scope();
    let env = w.w.consumer.env(EnvironmentKind::Production).id.as_uuid();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::PstnRelay],
            public_calling_enabled: true,
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
            leg_allowance_minor: 2_000,
            max_leg_seconds: 3600,
            daily_limit_minor: 100_000,
            monthly_limit_minor: 1_000_000,
            max_concurrent_legs: 10,
            max_originations_per_minute: 30,
            max_per_party_per_hour: 10,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let credential = w.w.token.credential.id.as_uuid();
    w.h()
        .conn()
        .execute_unprepared(&format!(
            "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, service_account_id, \
               credential_id, enabled, revision, created_at, updated_at) \
             SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, now(), now() \
             FROM platform_api_credentials WHERE id = '{credential}'"
        ))
        .await
        .unwrap();
    Some(World {
        w,
        privacy,
        telephony,
        a,
        b,
    })
}

impl World {
    async fn offer(&self, telephony_leg: Uuid) -> Value {
        let s = self.w.w.scope();
        let body = json!({
            "schemaVersion": "1",
            "telephonyLegId": telephony_leg,
            "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            "scope": {
                "tenantId": s.tenant_id().as_uuid(),
                "applicationId": s.application_id().as_uuid(),
                "environmentId": s.environment_id().as_uuid(),
            },
            "inboundResolution": {
                "decisionId": Uuid::new_v4(),
                "contextId": self.privacy.context_id,
                "contextVersion": 1,
                "relaySessionId": Uuid::new_v4(),
                "relaySessionVersion": 1,
                "assignmentId": Uuid::new_v4(),
                "assignmentGeneration": 1,
                "validUntil": chrono::Utc::now() + chrono::Duration::hours(1),
            },
            "callerContactReference": self.a,
            "allowedTargets": [{ "contactReferenceId": self.b }],
            "maxDurationSeconds": 3600,
        });
        let signed = self
            .w
            .h()
            .peers
            .telephony
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let r = self
            .w
            .h()
            .request_with(
                Method::POST,
                "/internal/v1/telephony/inbound-calls",
                None,
                Some(body),
                &refs,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        r.json
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

    /// (call, leg A, telephony A, leg B, telephony B, attempt B)
    async fn relay_call(&self) -> (Uuid, Uuid, Uuid, Uuid, Uuid, Uuid) {
        let ta = Uuid::new_v4();
        let answer = self.offer(ta).await;
        assert_eq!(answer["routeClass"], "PSTN_RELAY");
        let call: Uuid = answer["callId"].as_str().unwrap().parse().unwrap();
        let leg_a: Uuid = answer["callLegId"].as_str().unwrap().parse().unwrap();
        let row = self
            .w
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT l.id, a.id FROM call_legs l JOIN call_attempts a ON a.leg_id = l.id \
                     WHERE l.call_id = '{call}' AND l.direction = 'OUTBOUND' AND l.leg_kind = 'PUBLIC_NETWORK'"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        let leg_b: Uuid = row.try_get_by_index(0).unwrap();
        let attempt_b: Uuid = row.try_get_by_index(1).unwrap();
        let tb = self
            .telephony
            .telephony_leg_of(leg_b)
            .expect("callee originated");
        (call, leg_a, ta, leg_b, tb, attempt_b)
    }
}

fn relay(mut e: Value, inbound: bool) -> Value {
    e["routeClass"] = json!("PSTN_RELAY");
    if inbound {
        e["direction"] = json!("INBOUND");
    }
    e
}

#[tokio::test]
async fn a_protected_caller_reaches_a_protected_callee_through_one_call() {
    let Some(x) = world().await else { return };
    let (call, leg_a, ta, leg_b, tb, attempt_b) = x.relay_call().await;
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    // The callee leg: PSTN_RELAY, no application media, B's ContactReference and grant.
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let cmd = &originates[0].1;
    assert_eq!(cmd["call"]["routeClass"], "PSTN_RELAY");
    assert_eq!(cmd["media"], "NONE");
    assert_eq!(cmd["destination"]["contactReference"], json!(x.b));
    assert!(
        cmd.to_string().find(&x.a.to_string()).is_none(),
        "the caller is never the destination"
    );
    assert_eq!(x.privacy.grants()[0].1.call_leg_id, leg_b);

    // Leg-independent evidence: A was ringing already, B rings then answers.
    assert_eq!(
        x.event(relay(
            leg_event("LEG_RINGING", call, leg_a, ta, None, 1),
            true
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(
        x.w.state(call).await.0,
        CallState::Dialing,
        "the caller's own leg never rings the Call"
    );
    assert_eq!(
        x.event(relay(
            leg_event("LEG_RINGING", call, leg_b, tb, Some(attempt_b), 1),
            false
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Ringing);
    assert_eq!(
        x.event(relay(
            leg_event("LEG_ANSWERED", call, leg_b, tb, Some(attempt_b), 2),
            false
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Answered);
    let bridges = x.telephony.bridges();
    assert_eq!(bridges.len(), 1);
    assert_eq!(bridges[0]["kind"], "PUBLIC_LEGS");
    let mut members: Vec<Value> = bridges[0]["telephonyLegIds"].as_array().unwrap().clone();
    members.sort_by_key(|v| v.to_string());
    let mut expected = vec![json!(ta), json!(tb)];
    expected.sort_by_key(|v| v.to_string());
    assert_eq!(members, expected);

    // CONNECTED only with both legs connected AND the bridge active.
    assert_eq!(
        x.event(relay(
            leg_event("LEG_CONNECTED", call, leg_b, tb, Some(attempt_b), 3),
            false
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(
        x.event(relay(
            leg_event("LEG_CONNECTED", call, leg_a, ta, None, 2),
            true
        ))
        .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Answered);
    let bridge: Uuid = x
        .w
        .h()
        .conn()
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT telephony_bridge_id FROM call_public_bridges WHERE call_id = '{call}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let est = json!({
        "schemaVersion": "1", "eventId": Uuid::new_v4(), "eventType": "BRIDGE_ESTABLISHED",
        "occurredAt": chrono::Utc::now(), "bridgeId": bridge, "callId": call,
        "bridgeKind": "PUBLIC_LEGS", "telephonyLegIds": [ta, tb], "applicationMedia": false,
    });
    assert_eq!(x.event(est).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Connected);

    // The caller hangs up: B is terminated, then the Call ends.
    let mut a_end = relay(leg_event("LEG_ENDED", call, leg_a, ta, None, 3), true);
    a_end["endReason"] = json!("COMPLETED");
    assert_eq!(x.event(a_end).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Ending);
    assert!(x
        .telephony
        .controls()
        .iter()
        .any(|(leg, action, _)| *leg == tb && *action == LegControl::Terminate));
    let mut b_end = relay(
        leg_event("LEG_ENDED", call, leg_b, tb, Some(attempt_b), 4),
        false,
    );
    b_end["endReason"] = json!("COMPLETED");
    assert_eq!(x.event(b_end).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Ended);

    // Usage per leg: two facts, two settlements, one logical Call.
    for (leg, t, seq) in [(leg_a, ta, 4), (leg_b, tb, 5)] {
        let mut u = relay(
            leg_event("USAGE_FINALIZED", call, leg, t, None, seq),
            leg == leg_a,
        );
        u["usage"] = json!({ "connectedDurationMs": 30_000, "billableSeconds": 30, "costMinor": 300,
                             "costKnown": true, "currency": "NGN", "destinationCountry": "NG" });
        assert_eq!(x.event(u).await, "APPLIED");
    }
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            &format!("SELECT count(*) FROM call_spend_reservations WHERE call_id = '{call}' AND status = 'SETTLED'"),
        )
        .await,
        2
    );
    // One Call, two public legs.
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            &format!("SELECT count(*) FROM call_legs WHERE call_id = '{call}' AND leg_kind = 'PUBLIC_NETWORK'"),
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(x.w.h().conn(), "SELECT count(*) FROM calls").await,
        1
    );
}

#[tokio::test]
async fn a_busy_callee_releases_the_caller() {
    let Some(x) = world().await else { return };
    let (call, _leg_a, ta, leg_b, tb, attempt_b) = x.relay_call().await;
    let mut busy = relay(
        leg_event("LEG_BUSY", call, leg_b, tb, Some(attempt_b), 1),
        false,
    );
    busy["endReason"] = json!("BUSY");
    assert_eq!(x.event(busy).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Busy);
    assert!(x
        .telephony
        .controls()
        .iter()
        .any(|(leg, action, _)| *leg == ta && *action == LegControl::Terminate));
    assert!(x.telephony.bridges().is_empty());
}
