//! Phone-to-app (Part E §45) on Calling's side: Telephony's authenticated inbound offer
//! becomes ONE canonical Call (origin PUBLIC_INBOUND) that rings the authorized app
//! principals through the ordinary Part C path; the answer is bridged to the public
//! leg; CONNECTED only on bridge + leg evidence + the answerer's media; the caller's
//! hangup / abandon and the app's hangup propagate; refusals reveal nothing.

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
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::{
    app::{AppWorld, Device},
    privacy::FakePrivacyRelay,
    scalar_i64,
    telephony::{leg_event, FakePublicMedia, FakeTelephony},
    TestResponse,
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

struct World {
    w: AppWorld,
    privacy: Arc<FakePrivacyRelay>,
    telephony: Arc<FakeTelephony>,
    media: Arc<FakePublicMedia>,
    caller_contact: Uuid,
    session: Uuid,
    callee: Device,
}

async fn world() -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let w = AppWorld::with_public_network(privacy.clone(), telephony.clone(), Some(media.clone()))
        .await?;
    privacy.allow(w.w.recipient.id().as_uuid());
    let caller_contact = privacy.add_contact("NG");
    let scope = w.w.scope();
    let env = w.w.consumer.env(EnvironmentKind::Production).id.as_uuid();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::PstnToApp],
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
    let callee = w.device(&w.w.recipient, "callee-main").await;
    Some(World {
        w,
        privacy,
        telephony,
        media,
        caller_contact,
        session: Uuid::new_v4(),
        callee,
    })
}

impl World {
    async fn configure_owner(&self) {
        let credential = self.w.w.token.credential.id.as_uuid();
        self.w
            .h()
            .conn()
            .execute_unprepared(&format!(
                "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, service_account_id, \
                   credential_id, enabled, revision, created_at, updated_at) \
                 SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, now(), now() \
                 FROM platform_api_credentials WHERE id = '{credential}'"
            ))
            .await
            .unwrap();
    }

    fn offer(&self, telephony_leg: Uuid) -> Value {
        let s = self.w.w.scope();
        json!({
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
                "relaySessionId": self.session,
                "relaySessionVersion": 1,
                "assignmentId": Uuid::new_v4(),
                "assignmentGeneration": 1,
                "validUntil": chrono::Utc::now() + chrono::Duration::hours(1),
            },
            "callerContactReference": self.caller_contact,
            "allowedTargets": [{ "principalId": self.w.w.recipient.id() }],
            "maxDurationSeconds": 3600,
        })
    }

    async fn send(&self, signer: Arc<ServiceTokenSigner>, path: &str, body: Value) -> TestResponse {
        let signed = signer
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
        self.w
            .h()
            .request_with(Method::POST, path, None, Some(body), &refs)
            .await
    }

    async fn offer_as_telephony(&self, telephony_leg: Uuid) -> TestResponse {
        self.send(
            self.w.h().peers.telephony.clone(),
            "/internal/v1/telephony/inbound-calls",
            self.offer(telephony_leg),
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

    /// An accepted inbound call: (call, public leg, telephony leg).
    async fn ringing(&self) -> (Uuid, Uuid, Uuid) {
        let t = Uuid::new_v4();
        let r = self.offer_as_telephony(t).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["routeClass"], "PSTN_TO_APP");
        let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
        let leg: Uuid = r.json["callLegId"].as_str().unwrap().parse().unwrap();
        (call, leg, t)
    }
}

fn inbound(mut e: Value) -> Value {
    e["direction"] = json!("INBOUND");
    e["routeClass"] = json!("PSTN_TO_APP");
    e
}

#[tokio::test]
async fn a_phone_calls_a_relay_number_and_an_app_user_answers() {
    let Some(x) = world().await else { return };
    x.configure_owner().await;
    let (call, leg, t) = x.ringing().await;
    let (state, _) = x.w.state(call).await;
    assert_eq!(state, CallState::Ringing);
    // One canonical Call: the caller is a TELEPHONE_ENDPOINT principal referencing only
    // the opaque ContactReference; the recipient rings on its endpoint.
    let origin = scalar_i64(
        x.w.h().conn(),
        &format!("SELECT count(*) FROM calls WHERE id = '{call}' AND origin = 'PUBLIC_INBOUND'"),
    )
    .await;
    assert_eq!(origin, 1);
    let caller_principal = scalar_i64(
        x.w.h().conn(),
        &format!(
            "SELECT count(*) FROM platform_principals p JOIN calls c ON c.initiator_principal_id = p.id \
             WHERE c.id = '{call}' AND p.kind = 'TELEPHONE_ENDPOINT' AND p.external_reference = '{}'",
            x.caller_contact
        ),
    )
    .await;
    assert_eq!(caller_principal, 1);
    // A repeated offer for the same Telephony leg is the same Call.
    let again = x.offer_as_telephony(t).await;
    assert_eq!(again.json["callId"], json!(call));
    assert_eq!(
        x.event(inbound(leg_event("LEG_RINGING", call, leg, t, None, 1)))
            .await,
        "APPLIED"
    );

    // The app user answers on one device: the public leg is bridged to the RTC side.
    let invitation = x.w.invitation_for(call, x.callee.endpoint).await.unwrap();
    let (_, version) = x.w.state(call).await;
    let r = x.w.accept(call, invitation, &x.callee, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.w.state(call).await.0, CallState::Answered);
    let bridges = x.telephony.bridges();
    assert_eq!(bridges.len(), 1);
    assert_eq!(bridges[0]["kind"], "APPLICATION_MEDIA");
    assert_eq!(bridges[0]["telephonyLegIds"], json!([t]));
    assert_eq!(x.media.attached.lock().unwrap().len(), 1);

    // Telephony answers the caller when media joined, then reports the bridge.
    assert_eq!(
        x.event(inbound(leg_event("LEG_ANSWERED", call, leg, t, None, 2)))
            .await,
        "APPLIED"
    );
    assert_eq!(
        x.event(inbound(leg_event("LEG_CONNECTED", call, leg, t, None, 3)))
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
    let mut est = json!({
        "schemaVersion": "1", "eventId": Uuid::new_v4(), "eventType": "BRIDGE_ESTABLISHED",
        "occurredAt": chrono::Utc::now(), "bridgeId": bridge, "callId": call,
        "bridgeKind": "APPLICATION_MEDIA", "telephonyLegIds": [t], "applicationMedia": true,
    });
    assert_eq!(x.event(est.take()).await, "APPLIED");
    assert_eq!(
        x.w.state(call).await.0,
        CallState::Answered,
        "the answerer's media is required"
    );
    let session = x.w.session_on(call, x.callee.endpoint).await.unwrap();
    x.w.connect(call, session).await;
    assert_eq!(x.w.state(call).await.0, CallState::Connected);

    // The app user hangs up: Telephony ends the leg, then the Call ends.
    let (_, version) = x.w.state(call).await;
    let r = x.w.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.w.state(call).await.0, CallState::Ending);
    let controls = x.telephony.controls();
    assert_eq!(controls.len(), 1);
    assert_eq!((controls[0].0, controls[0].1), (t, LegControl::Terminate));
    let mut ended = inbound(leg_event("LEG_ENDED", call, leg, t, None, 4));
    ended["endReason"] = json!("COMPLETED");
    assert_eq!(x.event(ended).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Ended);
}

#[tokio::test]
async fn the_caller_abandons_while_ringing_and_unanswered_calls_release_the_leg() {
    let Some(x) = world().await else { return };
    x.configure_owner().await;
    let (call, leg, t) = x.ringing().await;
    let mut ended = inbound(leg_event("LEG_ENDED", call, leg, t, None, 1));
    ended["endReason"] = json!("COMPLETED");
    assert_eq!(x.event(ended).await, "APPLIED");
    assert_eq!(x.w.state(call).await.0, CallState::Cancelled);
    assert!(x.w.invitation_for(call, x.callee.endpoint).await.is_none());

    // Nobody answers: ring offers expire, the Call is NO_ANSWER and the public leg is
    // released at Telephony (never left open).
    let (call2, _, t2) = x.ringing().await;
    x.w.h().clock.advance(chrono::Duration::seconds(120));
    x.w.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(x.w.state(call2).await.0, CallState::NoAnswer);
    assert!(x
        .telephony
        .controls()
        .iter()
        .any(|(leg, action, _)| *leg == t2 && *action == LegControl::Terminate));
}

#[tokio::test]
async fn inbound_calls_are_refused_without_an_owner_authority_or_from_another_service() {
    let Some(x) = world().await else { return };
    // No inbound owner configured: refused (fail closed).
    let r = x.offer_as_telephony(Uuid::new_v4()).await;
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED", "{}", r.text);
    x.configure_owner().await;
    // Privacy Relay no longer authorizes the caller.
    x.privacy
        .set_mode(common::privacy::Mode::Deny("CONTEXT_REVOKED"));
    let r = x.offer_as_telephony(Uuid::new_v4()).await;
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED", "{}", r.text);
    x.privacy.set_mode(common::privacy::Mode::Policy);
    // Only Telephony may offer inbound calls.
    let r = x
        .send(
            x.w.h().peers.privacy_relay.clone(),
            "/internal/v1/telephony/inbound-calls",
            x.offer(Uuid::new_v4()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Nothing was created by any of the refusals.
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            "SELECT count(*) FROM calls WHERE origin = 'PUBLIC_INBOUND'"
        )
        .await,
        0
    );
}
