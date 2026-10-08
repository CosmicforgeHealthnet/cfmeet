//! Low-connectivity callback relay (Part E §47) on Calling's side: requested through
//! POST /v1/calls; the requester's verified contact is called first; the callee only
//! after the first leg ANSWERED; a failed second leg releases the first; one Call, two
//! legs, two usage facts; an idempotent retry never adds a paid leg.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::{events::TelephonyEvent, spend::SpendPolicy, LegControl},
};
use common::{
    app::AppWorld,
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
    mine: Uuid,
    theirs: Uuid,
    session: Uuid,
}

async fn world(callback_policy: bool) -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let w = AppWorld::with_public_network(privacy.clone(), telephony.clone(), None).await?;
    privacy.allow(w.w.initiator.id().as_uuid());
    let mine = privacy.add_contact("NG");
    let theirs = privacy.add_contact("NG");
    let scope = w.w.scope();
    let env = w.w.consumer.env(EnvironmentKind::Production).id.as_uuid();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::CallbackRelay],
            public_calling_enabled: true,
            callback_relay_enabled: callback_policy,
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
            max_per_principal_per_hour: None,
            max_per_service_account_per_hour: None,
            max_per_context_per_hour: None,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    Some(World {
        w,
        telephony,
        mine,
        theirs,
        session: Uuid::new_v4(),
    })
}

impl World {
    async fn request(&self, key: &str) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "callbackRequested": true,
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorContactReference": self.mine,
                    "targetContactReference": self.theirs,
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

    async fn leg_of_role(&self, call: Uuid, role: &str) -> Uuid {
        self.w
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT call_leg_id FROM call_public_parties WHERE call_id = '{call}' AND role = '{role}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    }
}

fn cb(mut e: Value) -> Value {
    e["routeClass"] = json!("CALLBACK_RELAY");
    e
}

#[tokio::test]
async fn the_requester_is_called_first_then_the_callee_then_both_are_bridged() {
    let Some(x) = world(true).await else { return };
    let r = x.request("cb-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    let (leg_a, leg_b) = (
        x.leg_of_role(call, "CALLER").await,
        x.leg_of_role(call, "CALLEE").await,
    );
    // Only the first leg is dialed — to the requester's own contact.
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    assert_eq!(
        originates[0].1["destination"]["contactReference"],
        json!(x.mine)
    );
    assert_eq!(originates[0].1["call"]["routeClass"], "CALLBACK_RELAY");
    let ta = x.telephony.telephony_leg_of(leg_a).unwrap();
    // Retrying the same request never adds a paid leg.
    let again = x.request("cb-1").await;
    assert_eq!(again.json["call"]["callId"], json!(call));
    assert_eq!(x.telephony.executions().len(), 1);

    // The requester's phone rings: the Call does not (the callee is not dialed yet).
    assert_eq!(
        x.event(cb(leg_event("LEG_RINGING", call, leg_a, ta, None, 1)))
            .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    assert_eq!(
        x.telephony.originates().len(),
        1,
        "no second leg before the first answered"
    );
    // The requester answers: NOW the callee is dialed (once, even if repeated).
    assert_eq!(
        x.event(cb(leg_event("LEG_ANSWERED", call, leg_a, ta, None, 2)))
            .await,
        "APPLIED"
    );
    assert_eq!(
        x.event(cb(leg_event("LEG_CONNECTED", call, leg_a, ta, None, 3)))
            .await,
        "APPLIED"
    );
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 2);
    assert_eq!(
        originates[1].1["destination"]["contactReference"],
        json!(x.theirs)
    );
    assert_eq!(originates[1].1["call"]["callLegId"], json!(leg_b));
    let tb = x.telephony.telephony_leg_of(leg_b).unwrap();
    assert!(x.telephony.bridges().is_empty());

    // The callee rings and answers: the Call is answered; both legs are bridged.
    assert_eq!(
        x.event(cb(leg_event("LEG_RINGING", call, leg_b, tb, None, 1)))
            .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Ringing);
    assert_eq!(
        x.event(cb(leg_event("LEG_ANSWERED", call, leg_b, tb, None, 2)))
            .await,
        "APPLIED"
    );
    assert_eq!(x.w.state(call).await.0, CallState::Answered);
    let bridges = x.telephony.bridges();
    assert_eq!(bridges.len(), 1);
    assert_eq!(bridges[0]["kind"], "PUBLIC_LEGS");
    assert_eq!(
        x.event(cb(leg_event("LEG_CONNECTED", call, leg_b, tb, None, 3)))
            .await,
        "APPLIED"
    );
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

    // Two usage facts, one logical Call.
    for (leg, t, seq) in [(leg_a, ta, 4), (leg_b, tb, 4)] {
        let mut u = cb(leg_event("USAGE_FINALIZED", call, leg, t, None, seq));
        u["usage"] = json!({ "connectedDurationMs": 60_000, "billableSeconds": 60, "costMinor": 600,
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
}

#[tokio::test]
async fn a_failed_second_leg_releases_the_first() {
    let Some(x) = world(true).await else { return };
    let r = x.request("cb-fail").await;
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (leg_a, leg_b) = (
        x.leg_of_role(call, "CALLER").await,
        x.leg_of_role(call, "CALLEE").await,
    );
    let ta = x.telephony.telephony_leg_of(leg_a).unwrap();
    x.event(cb(leg_event("LEG_ANSWERED", call, leg_a, ta, None, 1)))
        .await;
    let tb = x.telephony.telephony_leg_of(leg_b).unwrap();
    let mut busy = cb(leg_event("LEG_NO_ANSWER", call, leg_b, tb, None, 1));
    busy["endReason"] = json!("NO_ANSWER");
    assert_eq!(x.event(busy).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::NoAnswer);
    // The requester's answered leg is never left open.
    assert!(x
        .telephony
        .controls()
        .iter()
        .any(|(leg, action, _)| *leg == ta && *action == LegControl::Terminate));
}

#[tokio::test]
async fn the_requester_hanging_up_first_cancels_without_dialing_the_callee() {
    let Some(x) = world(true).await else { return };
    let r = x.request("cb-cancel").await;
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let leg_a = x.leg_of_role(call, "CALLER").await;
    let ta = x.telephony.telephony_leg_of(leg_a).unwrap();
    let mut end = cb(leg_event("LEG_ENDED", call, leg_a, ta, None, 1));
    end["endReason"] = json!("COMPLETED");
    assert_eq!(x.event(end).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Cancelled);
    assert_eq!(
        x.telephony.originates().len(),
        1,
        "the callee was never dialed"
    );
}

#[tokio::test]
async fn callback_relay_requires_its_policy_and_switch() {
    let Some(x) = world(false).await else { return };
    let r = x.request("cb-off").await;
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED", "{}", r.text);
    assert!(x.telephony.originates().is_empty());
    // Shape: a callback needs both contacts and no endpoint.
    let r = x
        .w
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "callbackRequested": true, "initiatorPrincipalId": x.w.w.initiator.id(),
                    "targetContactReference": x.theirs, "privacyRelaySessionId": x.session }),
            &[("idempotency-key", "cb-shape")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}
