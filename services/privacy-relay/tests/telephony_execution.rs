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

#[tokio::test]
async fn the_caller_identity_ignores_other_sessions_tenants_and_ended_or_disabled_numbers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = setup(&h).await;
    // Session B of the SAME tenant and a session of ANOTHER tenant each hold a number.
    let s_b = setup_in(&h, s.c.clone()).await;
    let s_other = setup(&h).await;
    let n1 = h.add_relay_number(RELAY, "pb-cert-1").await;
    h.add_relay_number("+44 20 7946 0501", "pb-cert-2").await;
    assert_eq!(
        h.assign_relay_number(&s_b.c, &s_b.session).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        h.assign_relay_number(&s_other.c, &s_other.session)
            .await
            .status,
        StatusCode::CREATED
    );
    let d = authorize(&h, &s, "PUBLIC_RELAY").await;
    let r = resolve(&h, &s, &d).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(
        r.json["resolution"].get("callerIdentity").is_none(),
        "another session's or tenant's number is never session A's identity: {}",
        r.text
    );
    assert!(!r.text.contains("79460500") && !r.text.contains("79460501"));
    // Session B's own identity is its own (with the assignment id Telephony persists).
    let d_b = authorize(&h, &s_b, "PUBLIC_RELAY").await;
    let r_b = resolve(&h, &s_b, &d_b).await;
    assert_eq!(r_b.status, StatusCode::OK, "{}", r_b.text);
    assert!(r_b.json["resolution"]["callerIdentity"]["assignmentId"]
        .as_str()
        .is_some_and(|id| Uuid::parse_str(id).is_ok()));
    // RELEASED: after an explicit release the identity is gone.
    let rel = h
        .post(
            &format!(
                "/internal/v1/sessions/{}/relay-number-assignment/release",
                s_b.session
            ),
            &s_b.c,
            json!({}),
        )
        .await;
    assert_eq!(rel.status, StatusCode::OK, "{}", rel.text);
    let r_b = resolve(&h, &s_b, &authorize(&h, &s_b, "PUBLIC_RELAY").await).await;
    assert_eq!(r_b.status, StatusCode::OK, "{}", r_b.text);
    assert!(r_b.json["resolution"].get("callerIdentity").is_none());
    // DISABLED inventory number: the other tenant's number is disabled by an operator.
    let r_o = resolve(&h, &s_other, &authorize(&h, &s_other, "PUBLIC_RELAY").await).await;
    assert!(r_o.json["resolution"].get("callerIdentity").is_some());
    let disabled = if r_o.json["resolution"]["callerIdentity"]["e164"] == "+442079460500" {
        n1
    } else {
        h.relay
            .list_relay_numbers()
            .await
            .unwrap()
            .into_iter()
            .find(|n| n.id != n1)
            .unwrap()
            .id
    };
    h.relay.disable_relay_number(disabled).await.unwrap();
    let r_o = resolve(&h, &s_other, &authorize(&h, &s_other, "PUBLIC_RELAY").await).await;
    assert_eq!(r_o.status, StatusCode::OK, "{}", r_o.text);
    assert!(r_o.json["resolution"].get("callerIdentity").is_none());
    // EXPIRED: an expired assignment means an expired session, which resolves nothing.
    h.clock.set(common::at(61));
    h.relay.run_sweep().await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments WHERE status = 'ACTIVE'")
            .await,
        0
    );
    let telephony = h.ids.telephony.clone();
    let r = h
        .send_as(
            &telephony,
            Method::POST,
            &format!("/internal/v1/contacts/{}/resolve-for-execution", s.contact),
            Some(&s.c),
            Some(
                json!({ "grantId": Uuid::new_v4(), "grantSecret": "x", "callId": Uuid::new_v4(),
                         "callLegId": Uuid::new_v4(), "executionAttemptId": Uuid::new_v4() }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(!r.text.contains("7946"));
}

/// Like [`setup`], in an existing consumer scope (a second session of the same tenant).
async fn setup_in(h: &Harness, c: Consumer) -> Setup {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(&c, "+44 20 7946 0812").await["contactReference"]
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
