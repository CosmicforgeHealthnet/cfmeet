//! Part E evolutions of Privacy Relay's Telephony-facing answers:
//!
//! * an AUTHORIZED public-network decision carries the protected contacts' routing facts
//!   (country, verification) — never a number — so Calling's Routing Engine can decide;
//! * `resolve-for-execution` returns the caller identity of the leg: the relay number lent
//!   to THAT session (none without an active, outbound-capable assignment);
//! * (a RESOLVED inbound mapping names its owner scope: `relay_inbound.rs`).

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use common::{Consumer, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

const CONTACT: &str = "+44 20 7946 0811";
const RELAY: &str = "+44 20 7946 0500";

struct Setup {
    c: Consumer,
    principal: Uuid,
    contact: String,
    session: String,
}

async fn setup(h: &Harness) -> Setup {
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(&c, &contact, "VERIFIED", None).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let session = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    Setup {
        c,
        principal,
        contact,
        session,
    }
}

async fn authorize(h: &Harness, s: &Setup, route: &str) -> Value {
    let r = h
        .post(
            "/internal/v1/communication/authorize",
            &s.c,
            json!({ "relaySessionId": s.session, "source": { "principalId": s.principal },
                    "targets": [{ "contactReferenceId": s.contact }], "channel": "VOICE",
                    "route": route, "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

async fn resolve(h: &Harness, s: &Setup, decision: &Value) -> common::Response {
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(
            &format!("/internal/v1/contacts/{}/resolution-grants", s.contact),
            &s.c,
            json!({
            "relaySessionId": s.session, "decisionId": decision["decisionId"], "callId": call,
            "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let grant = &g.json["grant"];
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{}/resolve-for-execution", s.contact),
        Some(&s.c),
        Some(
            json!({ "grantId": grant["grantId"], "grantSecret": grant["grantSecret"],
                     "callId": call, "callLegId": leg, "executionAttemptId": Uuid::new_v4() }),
        ),
    )
    .await
}

#[tokio::test]
async fn public_decisions_carry_contact_routing_facts_and_never_numbers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = setup(&h).await;
    let d = authorize(&h, &s, "PUBLIC_RELAY").await;
    assert_eq!(d["decision"], "AUTHORIZED");
    assert_eq!(
        d["contactFacts"],
        json!([{ "contactReference": s.contact, "country": "GB", "verification": "VERIFIED" }])
    );
    assert!(!d.to_string().contains("7946"), "{d}");
    // An application-route decision about principals carries no contact facts.
    let app = h
        .post(
            "/internal/v1/communication/authorize",
            &s.c,
            json!({ "relaySessionId": s.session, "source": { "principalId": s.principal },
                    "targets": [{ "contactReferenceId": s.contact }], "channel": "VOICE",
                    "route": "APPLICATION", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(app.json["decision"], "DENIED");
    assert!(app.json.get("contactFacts").is_none());
}

#[tokio::test]
async fn the_caller_identity_is_the_relay_number_lent_to_that_session_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = setup(&h).await;
    // No relay number lent yet: the destination resolves, but no caller identity exists,
    // so Telephony must refuse to originate (it never invents one).
    let d = authorize(&h, &s, "PUBLIC_RELAY").await;
    let r = resolve(&h, &s, &d).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["resolution"].get("callerIdentity").is_none());

    h.add_relay_number(RELAY, "pb-cert").await;
    let assignment = h.assign_relay_number(&s.c, &s.session).await;
    assert_eq!(
        assignment.status,
        StatusCode::CREATED,
        "{}",
        assignment.text
    );
    let d = authorize(&h, &s, "PUBLIC_RELAY").await;
    let r = resolve(&h, &s, &d).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["resolution"]["callerIdentity"]["e164"],
        "+442079460500"
    );
    assert_eq!(
        r.json["resolution"]["callerIdentity"]["assignmentId"],
        assignment.json["assignment"]["assignmentId"]
    );
}
