//! App-to-phone (Part E §44) on Calling's side, with a scripted Telephony and Privacy
//! Relay: authorization, routing, spend, durable originate (grant per submission, no
//! number), normalized events, bridge + media attachment, CONNECTED only on evidence,
//! hangup both ways, usage settlement, unknown outcomes, dedupe / order / quarantine.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    media::provider::ProviderParticipant,
    routing::{
        engine::{Evidence, RouteHealth},
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
    telephony::{leg_event, Answer, FakePublicMedia, FakeTelephony},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

struct World {
    w: AppWorld,
    privacy: Arc<FakePrivacyRelay>,
    telephony: Arc<FakeTelephony>,
    media: Arc<FakePublicMedia>,
    contact: Uuid,
    session: Uuid,
    caller: Device,
}

fn public_policy() -> RoutingPolicy {
    RoutingPolicy {
        allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::AppToPstn],
        public_calling_enabled: true,
        destinations: DestinationPolicy {
            allow: vec!["NG".into()],
            deny: vec![],
            default: CountryDecision::Deny,
        },
        ..RoutingPolicy::default()
    }
}

fn spend_policy() -> SpendPolicy {
    SpendPolicy {
        currency: "NGN".into(),
        leg_allowance_minor: 5_000,
        max_leg_seconds: 3600,
        daily_limit_minor: 50_000,
        monthly_limit_minor: 500_000,
        max_concurrent_legs: 5,
        max_originations_per_minute: 20,
        max_per_party_per_hour: 10,
        max_per_principal_per_hour: None,
        max_per_service_account_per_hour: None,
        max_per_context_per_hour: None,
        enabled: true,
    }
}

async fn world() -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let w = AppWorld::with_public_network(privacy.clone(), telephony.clone(), Some(media.clone()))
        .await?;
    privacy.allow(w.w.initiator.id().as_uuid());
    let contact = privacy.add_contact("NG");
    let env = w.w.consumer.env(EnvironmentKind::Production);
    let scope = w.w.scope();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env.id.as_uuid(),
        &public_policy(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    set_spend(&w, spend_policy()).await;
    let caller = w.device(&w.w.initiator, "caller-main").await;
    Some(World {
        w,
        privacy,
        telephony,
        media,
        contact,
        session: Uuid::new_v4(),
        caller,
    })
}

async fn set_spend(w: &AppWorld, p: SpendPolicy) {
    let scope = w.w.scope();
    calling::telephony::spend::store_policy(
        w.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &p,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
}

impl World {
    async fn call(&self, key: &str) -> TestResponse {
        self.call_to(self.contact, key).await
    }

    async fn call_to(&self, contact: Uuid, key: &str) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    async fn public_leg(&self, call: Uuid) -> (Uuid, Uuid) {
        let row = self
            .w
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT l.id, a.id FROM call_legs l JOIN call_attempts a ON a.leg_id = l.id \
                 WHERE l.call_id = $1 AND l.leg_kind = 'PUBLIC_NETWORK' ORDER BY a.attempt_number DESC LIMIT 1",
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

    /// Dialing call with its public leg, attempt and Telephony leg.
    async fn dialing(&self, key: &str) -> (Uuid, Uuid, Uuid, Uuid) {
        let r = self.call(key).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let (leg, attempt) = self.public_leg(call).await;
        let t = self
            .telephony
            .telephony_leg_of(leg)
            .expect("telephony accepted the originate");
        (call, leg, attempt, t)
    }

    /// Initiator media connected (signed provider callbacks through the real route).
    async fn connect_initiator(&self, call: Uuid) {
        let session = self
            .w
            .session_on(call, self.caller.endpoint)
            .await
            .expect("initiator session");
        self.w.connect(call, session).await;
    }

    /// The reserved bridge identity joins the RTC resource.
    async fn bridge_joins(&self, call: Uuid, identity: &str) -> TestResponse {
        let p = ProviderParticipant {
            identity: identity.to_string(),
            generation: None,
            permissions: calling::media::MediaPermissions::NONE,
            active: true,
            published_audio: true,
            published_video: false,
            joined_at: Some(chrono::Utc::now()),
        };
        self.w.media().inject(&AppWorld::room(call), p.clone());
        let (auth, body) =
            self.w
                .media()
                .signed_event("participant_joined", &AppWorld::room(call), Some(&p));
        self.w.callback(&auth, body).await
    }
}

async fn state(w: &AppWorld, call: Uuid) -> CallState {
    w.state(call).await.0
}

#[tokio::test]
async fn an_app_user_calls_a_protected_phone_and_hangs_up() {
    let Some(x) = world().await else { return };
    let (call, leg, attempt, t) = x.dialing("a2p-1").await;
    assert_eq!(state(&x.w, call).await, CallState::Dialing);

    // The command Telephony received: complete proof, scope, a single-use grant, the
    // spend reservation — never a telephone number or a caller identity.
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let (delegated, cmd) = &originates[0];
    assert_eq!(delegated.tenant_id(), x.w.w.scope().tenant_id().as_uuid());
    assert_eq!(cmd["call"]["routeClass"], "APP_TO_PSTN");
    assert_eq!(cmd["call"]["callLegId"], json!(leg));
    assert_eq!(cmd["destination"]["contactReference"], json!(x.contact));
    assert_eq!(cmd["destination"]["country"], "NG");
    assert!(cmd["destination"]["grantSecret"].as_str().unwrap().len() >= 32);
    assert!(cmd["spend"]["reservedAmountMinor"].as_i64().unwrap() > 0);
    assert!(cmd.get("callerId").is_none() && cmd.get("fromNumber").is_none());
    let grants = x.privacy.grants();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].1.call_leg_id, leg);
    // Calling stores no grant secret.
    let stored = scalar_i64(
        x.w.h().conn(),
        "SELECT count(*) FROM call_telephony_commands WHERE payload::text LIKE '%grant-secret%'",
    )
    .await;
    assert_eq!(stored, 0);

    // Normalized events drive Calling's state.
    assert_eq!(
        x.event(leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1))
            .await,
        "APPLIED"
    );
    assert_eq!(state(&x.w, call).await, CallState::Ringing);
    assert_eq!(
        x.event(leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 2))
            .await,
        "APPLIED"
    );
    assert_eq!(state(&x.w, call).await, CallState::Answered);
    // A bridge to the application media was requested and the RTC side attached with the
    // single-use join code under the reserved identity.
    assert_eq!(x.telephony.bridges().len(), 1);
    assert_eq!(x.telephony.bridges()[0]["kind"], "APPLICATION_MEDIA");
    assert_eq!(x.telephony.bridges()[0]["telephonyLegIds"], json!([t]));
    let attached = x.media.attached.lock().unwrap().clone();
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].0, AppWorld::room(call));
    assert!(attached[0].1.starts_with("cfm-pstn-"));
    assert!(attached[0].2.starts_with("cfmj"));
    let r = x.bridge_joins(call, &attached[0].1).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // A request accepted is not CONNECTED; neither is a connected leg without the bridge
    // and the initiator's media.
    assert_eq!(
        x.event(leg_event("LEG_CONNECTED", call, leg, t, Some(attempt), 3))
            .await,
        "APPLIED"
    );
    assert_eq!(state(&x.w, call).await, CallState::Answered);
    let bridge_id: Uuid = x
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
    let mut est = leg_event("BRIDGE_ESTABLISHED", call, leg, t, None, 1);
    est.as_object_mut().unwrap().remove("callLegId");
    est["bridgeId"] = json!(bridge_id);
    assert_eq!(x.event(est).await, "APPLIED");
    assert_eq!(state(&x.w, call).await, CallState::Answered);
    x.connect_initiator(call).await;
    assert_eq!(state(&x.w, call).await, CallState::Connected);

    // The phone hangs up: the Call ends (ENDING -> ENDED) with no live public leg.
    let mut ended = leg_event("LEG_ENDED", call, leg, t, Some(attempt), 4);
    ended["endReason"] = json!("COMPLETED");
    ended["legStatus"] = json!("ENDED");
    assert_eq!(x.event(ended).await, "APPLIED");
    assert_eq!(state(&x.w, call).await, CallState::Ended);

    // Usage settles the reservation exactly once.
    let mut usage = leg_event("USAGE_FINALIZED", call, leg, t, Some(attempt), 5);
    usage["usage"] = json!({ "connectedDurationMs": 61_000, "billableSeconds": 61, "costMinor": 1234,
                             "costKnown": true, "currency": "NGN", "destinationCountry": "NG" });
    assert_eq!(x.event(usage.clone()).await, "APPLIED");
    usage["eventId"] = json!(Uuid::new_v4());
    usage["sequence"] = json!(6);
    assert_eq!(x.event(usage).await, "APPLIED");
    let settled = scalar_i64(
        x.w.h().conn(),
        &format!("SELECT settled_minor FROM call_spend_reservations WHERE call_leg_id = '{leg}' AND status = 'SETTLED'"),
    )
    .await;
    assert_eq!(settled, 1234);

    // Calling's database holds no telephone number of the contact or the relay.
    let dump = common::dump_database(x.w.h().conn()).await;
    for needle in ["+234", "2348", "grant-secret"] {
        assert!(
            !dump.contains(needle),
            "{needle} leaked into Calling's database"
        );
    }
}

#[tokio::test]
async fn the_app_user_hangs_up_and_the_public_leg_is_terminated() {
    let Some(x) = world().await else { return };
    let (call, leg, attempt, t) = x.dialing("a2p-hangup").await;
    x.event(leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1))
        .await;
    let (_, version) = x.w.state(call).await;
    // Cancel while ringing: CANCELLED, and Telephony is asked to end the leg.
    let r = x.w.command(call, "cancel", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(state(&x.w, call).await, CallState::Cancelled);
    let controls = x.telephony.controls();
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0].0, t);
    assert_eq!(controls[0].1, LegControl::Terminate);
    // A late answer never resurrects the Call.
    assert_eq!(
        x.event(leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 2))
            .await,
        "IGNORED_TERMINAL"
    );
    assert_eq!(state(&x.w, call).await, CallState::Cancelled);
    assert!(x.telephony.bridges().is_empty());
}

#[tokio::test]
async fn busy_no_answer_and_failure_end_the_call_with_the_leg_outcome() {
    let Some(x) = world().await else { return };
    for (n, (event, end, expected)) in [
        ("LEG_BUSY", "BUSY", CallState::Busy),
        ("LEG_NO_ANSWER", "NO_ANSWER", CallState::NoAnswer),
        ("LEG_FAILED", "FAILED", CallState::Failed),
    ]
    .into_iter()
    .enumerate()
    {
        let (call, leg, attempt, t) = x.dialing(&format!("a2p-out-{n}")).await;
        let mut e = leg_event(event, call, leg, t, Some(attempt), 1);
        e["endReason"] = json!(end);
        e["legStatus"] = json!("FAILED");
        e["definite"] = json!(true);
        if event == "LEG_FAILED" {
            e["failureCategory"] = json!("NETWORK");
        }
        assert_eq!(x.event(e).await, "APPLIED");
        assert_eq!(state(&x.w, call).await, expected);
    }
}

#[tokio::test]
async fn unknown_outcome_is_not_failure_and_events_are_deduplicated_ordered_and_matched() {
    let Some(x) = world().await else { return };
    let (call, leg, attempt, t) = x.dialing("a2p-unknown").await;
    let mut unknown = leg_event("LEG_STATUS_UNKNOWN", call, leg, t, Some(attempt), 1);
    unknown["legStatus"] = json!("UNKNOWN");
    unknown["definite"] = json!(false);
    assert_eq!(x.event(unknown.clone()).await, "APPLIED");
    // UNKNOWN: the Call keeps dialing, the attempt is UNKNOWN (not FAILED), no new leg,
    // no second origination.
    assert_eq!(state(&x.w, call).await, CallState::Dialing);
    let outcome = scalar_i64(
        x.w.h().conn(),
        &format!("SELECT count(*) FROM call_attempts WHERE id = '{attempt}' AND outcome = 'UNKNOWN' AND unknown_outcome"),
    )
    .await;
    assert_eq!(outcome, 1);
    assert_eq!(x.telephony.executions().len(), 1);
    // Duplicate delivery is a no-op.
    assert_eq!(x.event(unknown).await, "DUPLICATE");
    // A late answer resolves it (evidence), an older sequence never regresses.
    assert_eq!(
        x.event(leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 3))
            .await,
        "APPLIED"
    );
    assert_eq!(
        x.event(leg_event("LEG_RINGING", call, leg, t, Some(attempt), 2))
            .await,
        "STALE"
    );
    assert_eq!(state(&x.w, call).await, CallState::Answered);
    // An event of an execution Calling never commanded attaches to nothing.
    assert_eq!(
        x.event(leg_event(
            "LEG_ANSWERED",
            call,
            leg,
            Uuid::new_v4(),
            None,
            9
        ))
        .await,
        "UNMATCHED"
    );
    assert_eq!(
        x.event(leg_event("LEG_ANSWERED", Uuid::new_v4(), leg, t, None, 9))
            .await,
        "UNMATCHED"
    );
}

#[tokio::test]
async fn telephony_alone_may_deliver_events() {
    let Some(x) = world().await else { return };
    let (call, leg, attempt, t) = x.dialing("a2p-auth").await;
    let send = |signer: Arc<ServiceTokenSigner>, body: Value| {
        let h = x.w.h();
        async move {
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
            h.request_with(
                Method::POST,
                "/internal/v1/telephony/events",
                None,
                Some(body),
                &refs,
            )
            .await
        }
    };
    let body = leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1);
    let denied = send(x.w.h().peers.privacy_relay.clone(), body.clone()).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.text);
    let ok = send(x.w.h().peers.telephony.clone(), body).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(ok.json["disposition"], "APPLIED");
    assert_eq!(state(&x.w, call).await, CallState::Ringing);
}

#[tokio::test]
async fn every_gate_fails_closed_before_any_paid_action() {
    let Some(x) = world().await else { return };
    // Unknown contact (not authorized by Privacy Relay).
    let r = x.call_to(Uuid::new_v4(), "gate-contact").await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("CALL_NOT_AUTHORIZED")),
        "{}",
        r.text
    );
    // Country not allowed by routing policy.
    let gb = x.privacy.add_contact("GB");
    let r = x.call_to(gb, "gate-country").await;
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED", "{}", r.text);
    // No healthy route.
    x.telephony.set_routes(Evidence::Known(vec![
        calling::routing::engine::CandidateRoute {
            route_ref: "ng-a".into(),
            health: RouteHealth::Unavailable,
            capacity_available: Evidence::Known(true),
            estimated_leg_cost_minor: Evidence::Known(100),
            quality_permille: Evidence::Unknown,
        },
    ]));
    let r = x.call("gate-route").await;
    assert_eq!(
        r.json["error"]["code"], "PUBLIC_ROUTE_UNAVAILABLE",
        "{}",
        r.text
    );
    x.telephony.set_routes(common::telephony::healthy_routes());
    // Spend: concurrency 1 then the next call is refused.
    set_spend(
        &x.w,
        SpendPolicy {
            max_concurrent_legs: 1,
            ..spend_policy()
        },
    )
    .await;
    let ok = x.call("gate-spend-1").await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
    let r = x.call("gate-spend-2").await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            Some("TELECOM_SPEND_LIMIT_EXCEEDED")
        ),
        "{}",
        r.text
    );
    // Privacy Relay down: fail closed.
    x.privacy.set_mode(common::privacy::Mode::Unavailable);
    let r = x.call("gate-pr").await;
    assert_eq!(
        r.json["error"]["code"], "PRIVACY_AUTHORIZATION_UNAVAILABLE",
        "{}",
        r.text
    );
    // Exactly one paid origination happened across all of the above.
    assert_eq!(x.telephony.executions().len(), 1);
}

#[tokio::test]
async fn a_lost_response_is_retried_with_the_same_command_and_never_duplicates() {
    let Some(x) = world().await else { return };
    x.telephony.script(&[Answer::AcceptButLoseResponse]);
    let r = x.call("a2p-lost").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (leg, _) = x.public_leg(call).await;
    // Not bound yet: the response was lost.
    assert_eq!(x.telephony.executions().len(), 1);
    // The worker retries the SAME command (new grant, same idempotency key).
    x.w.h()
        .conn()
        .execute_unprepared(
            "UPDATE call_telephony_commands SET next_attempt_at = '2000-01-01T00:00:00Z'",
        )
        .await
        .unwrap();
    x.w.h()
        .state
        .app
        .dispatch_telephony_commands(10)
        .await
        .unwrap();
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 2);
    assert_eq!(
        originates[0].1["idempotencyKey"],
        originates[1].1["idempotencyKey"]
    );
    assert_eq!(originates[0].1["commandId"], originates[1].1["commandId"]);
    assert_eq!(x.telephony.executions().len(), 1, "one paid effect");
    let bound = scalar_i64(
        x.w.h().conn(),
        &format!("SELECT count(*) FROM call_legs WHERE id = '{leg}' AND execution_binding_ref IS NOT NULL"),
    )
    .await;
    assert_eq!(bound, 1);
}

use sea_orm::ConnectionTrait;
