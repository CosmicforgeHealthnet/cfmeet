//! Interconnect failover at Telephony (Part E §51, §142–§152): which eligible
//! interconnect executes is Telephony's choice (capability, country, health, circuit,
//! capacity, cost, attempt history); a failover attempt is a new CallLeg of the same Call
//! to the same protected party, and Telephony refuses it while an earlier attempt is live
//! or UNKNOWN — so a late success of the first attempt can never run beside a second.

mod common;

use axum::http::{Method, StatusCode};
use common::Harness;
use serde_json::{json, Value};
use telephony::{
    domain::NormalizedEvent,
    providers::{deterministic::Script, UnknownReason},
    routing::{CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_SECONDS},
};
use uuid::Uuid;

const ORIGINATE: &str = "/internal/v1/legs/originate";

/// An ORIGINATE for a NEW CallLeg of `call` to `contact`.
fn attempt(h: &Harness, call: Uuid, contact: Uuid) -> Value {
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    body["call"]["callId"] = json!(call);
    body
}

async fn submit(h: &Harness, body: &Value) -> common::Response {
    h.calling(Method::POST, ORIGINATE, Some(body.clone())).await
}

async fn leg_of(r: &common::Response) -> Uuid {
    r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn route_of(h: &Harness, leg: Uuid) -> Option<String> {
    h.text(&format!(
        "SELECT route_id FROM telephony_legs WHERE id = '{leg}'"
    ))
    .await
}

#[tokio::test]
async fn a_definite_provider_failure_lets_the_next_attempt_use_another_interconnect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (call, contact) = (
        Uuid::new_v4(),
        h.privacy.add_contact("+2348031234567", "NG"),
    );
    h.provider_a
        .script(&[Script::ProviderError { transmitted: true }]);
    let r = submit(&h, &attempt(&h, call, contact)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let first = leg_of(&r).await;
    h.run().await;
    assert_eq!(h.leg_status(first).await, "FAILED");
    assert_eq!(route_of(&h, first).await.as_deref(), Some("ng-a"));
    let failed = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == json!(first) && e["eventType"] == "LEG_FAILED")
        .unwrap();
    assert_eq!(
        (
            failed["definite"].clone(),
            failed["failureCategory"].clone()
        ),
        (json!(true), json!("NETWORK"))
    );
    // Calling's failover attempt: a new CallLeg of the same Call to the same party.
    let r = submit(&h, &attempt(&h, call, contact)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let second = leg_of(&r).await;
    h.run().await;
    // Attempt history: interconnect a already failed for this party -> b executes.
    assert_eq!(route_of(&h, second).await.as_deref(), Some("ng-b"));
    assert_eq!(h.leg_status(second).await, "ORIGINATING");
    assert_eq!(h.provider_b.effects().len(), 1);
    assert!(h.provider_a.effects().is_empty());
    // Another party of the same Call is not affected by that history.
    let other = h.privacy.add_contact("+2348039999999", "NG");
    let r = submit(&h, &attempt(&h, call, other)).await;
    let third = leg_of(&r).await;
    h.run().await;
    assert_eq!(route_of(&h, third).await.as_deref(), Some("ng-a"));
}

#[tokio::test]
async fn unknown_blocks_any_second_attempt_until_reconciled() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (call, contact) = (
        Uuid::new_v4(),
        h.privacy.add_contact("+2348031234567", "NG"),
    );
    h.provider_a
        .script(&[Script::LoseRequest(UnknownReason::TimeoutAfterSend)]);
    let first = leg_of(&submit(&h, &attempt(&h, call, contact)).await).await;
    h.run().await;
    assert_eq!(h.leg_status(first).await, "UNKNOWN");
    // No blind failover: a second attempt to the same party is refused while UNKNOWN.
    let r = submit(&h, &attempt(&h, call, contact)).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "EXECUTION_IN_PROGRESS"),
        "{}",
        r.text
    );
    assert!(h.provider_b.requests().is_empty());
    // Reconciliation proves the first attempt absent: now another one may run, and the
    // interconnect that lost it is not used again for this party.
    h.advance(10);
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(h.leg_status(first).await, "FAILED");
    let r = submit(&h, &attempt(&h, call, contact)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let second = leg_of(&r).await;
    h.run().await;
    assert_eq!(route_of(&h, second).await.as_deref(), Some("ng-b"));
}

#[tokio::test]
async fn a_late_primary_success_suppresses_the_secondary() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (call, contact) = (
        Uuid::new_v4(),
        h.privacy.add_contact("+2348031234567", "NG"),
    );
    // A times out; reconciliation has started; A then succeeds late.
    h.provider_a.script(&[Script::AcceptButLoseResponse(
        UnknownReason::TimeoutAfterSend,
    )]);
    let first = leg_of(&submit(&h, &attempt(&h, call, contact)).await).await;
    h.run().await;
    assert_eq!(h.leg_status(first).await, "UNKNOWN");
    let correlation = h.correlation_of(first).await;
    // Concurrent failover attempts while A is unresolved: all refused.
    let bodies: Vec<Value> = (0..4).map(|_| attempt(&h, call, contact)).collect();
    let results = futures::future::join_all(bodies.iter().map(|b| submit(&h, b))).await;
    assert!(results
        .iter()
        .all(|r| r.status == StatusCode::CONFLICT && r.code() == "EXECUTION_IN_PROGRESS"));
    // A's late success arrives: it binds and continues.
    let ringing = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegRinging, h.now());
    h.svc.ingest_provider_event("dp-a", ringing).await.unwrap();
    assert_eq!(h.leg_status(first).await, "RINGING");
    // B is still refused (A is live), and never executed anywhere.
    let r = submit(&h, &attempt(&h, call, contact)).await;
    assert_eq!(r.code(), "EXECUTION_IN_PROGRESS");
    for _ in 0..3 {
        h.run().await;
    }
    assert!(h.provider_b.requests().is_empty());
    assert_eq!(h.provider_a.effects().len(), 1);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_legs WHERE calling_call_id = '{call}'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn when_no_interconnect_is_left_the_attempt_fails_safely() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (call, contact) = (
        Uuid::new_v4(),
        h.privacy.add_contact("+2348031234567", "NG"),
    );
    h.provider_a
        .script(&[Script::ProviderError { transmitted: true }]);
    h.provider_b
        .script(&[Script::ProviderError { transmitted: true }]);
    for _ in 0..2 {
        let leg = leg_of(&submit(&h, &attempt(&h, call, contact)).await).await;
        h.run().await;
        assert_eq!(h.leg_status(leg).await, "FAILED");
    }
    // Both interconnects failed for this party: a third attempt is never sent.
    let leg = leg_of(&submit(&h, &attempt(&h, call, contact)).await).await;
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "NOT_SENT"
    );
    assert_eq!(
        h.provider_a.requests().len() + h.provider_b.requests().len(),
        2
    );
}

#[tokio::test]
async fn circuit_breaker_health_and_capacity_steer_new_calls() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Consecutive provider-side failures open a's circuit (an incident records it).
    h.provider_a.script(
        &(0..CIRCUIT_FAILURE_THRESHOLD)
            .map(|_| Script::ProviderError { transmitted: true })
            .collect::<Vec<_>>(),
    );
    for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
        let (leg, _) = h.originate().await;
        h.run().await;
        assert_eq!(h.leg_status(leg).await, "FAILED");
    }
    assert_eq!(
        h.text("SELECT circuit_state FROM telecom_routes WHERE id = 'ng-a'")
            .await
            .unwrap(),
        "OPEN"
    );
    assert!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'PROVIDER_CIRCUIT_OPENED'"
        )
        .await
            >= 1
    );
    // New calls go to b while a is open; availability reports a unavailable.
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(route_of(&h, leg).await.as_deref(), Some("ng-b"));
    let availability = h
        .calling(
            Method::GET,
            "/internal/v1/routes/availability?country=NG&capability=VOICE_OUTBOUND",
            None,
        )
        .await;
    assert_eq!(availability.status, StatusCode::OK, "{}", availability.text);
    let text = availability.text.clone();
    assert!(!text.contains("dp-a") && !text.contains("dp-b"), "{text}");
    // After the open period one probe is allowed (HALF_OPEN); its success closes it.
    h.advance(CIRCUIT_OPEN_SECONDS + 1);
    let (probe, _) = h.originate().await;
    h.run().await;
    assert_eq!(route_of(&h, probe).await.as_deref(), Some("ng-a"));
    assert_eq!(
        h.text("SELECT circuit_state FROM telecom_routes WHERE id = 'ng-a'")
            .await
            .unwrap(),
        "CLOSED"
    );
    // Health: an unavailable interconnect is skipped.
    h.sql("UPDATE telecom_routes SET health = 'UNAVAILABLE' WHERE id = 'ng-a'")
        .await;
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(route_of(&h, leg).await.as_deref(), Some("ng-b"));
    h.sql("UPDATE telecom_routes SET health = 'HEALTHY' WHERE id = 'ng-a'")
        .await;
    // Capacity: never more paid legs than a route permits.
    h.sql("UPDATE telecom_routes SET max_concurrent_legs = 0 WHERE id IN ('ng-a', 'ng-b')")
        .await;
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert_eq!(
        h.text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await
        .unwrap(),
        "CAPACITY_EXHAUSTED"
    );
}
