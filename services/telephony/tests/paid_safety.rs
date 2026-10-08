//! Paid-action safety at Telephony (Part E §160–§162, §192–§193): the durable call
//! duration limit, operator disable of one interconnect / route without touching
//! anything else, abuse signals, and bounded metrics.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::Harness;
use http_body_util::BodyExt;
use serde_json::json;
use telephony::{domain::NormalizedEvent, metrics};
use tower::ServiceExt;
use uuid::Uuid;

async fn answered(h: &Harness, max_seconds: i64) -> (Uuid, String) {
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    body["spend"]["maxDurationSeconds"] = json!(max_seconds);
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    let correlation = h.correlation_of(leg).await;
    for event in [NormalizedEvent::LegRinging, NormalizedEvent::LegAnswered] {
        let e = h.provider_a.progress(&correlation, event, h.now());
        h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    }
    (leg, correlation)
}

#[tokio::test]
async fn the_call_duration_limit_is_enforced_durably() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = answered(&h, 60).await;
    h.advance(45);
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "ANSWERED");
    h.advance(20);
    h.run().await;
    // Ended at the provider and here; Calling hears a normal end (and the usage fact).
    assert_eq!(h.leg_status(leg).await, "ENDED");
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "DURATION_LIMIT"
    );
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'DURATION_LIMIT_ENFORCED'"
        )
        .await,
        1
    );
    h.run().await;
    let types = h.calling.event_types_for(leg);
    assert!(types.contains(&"LEG_ENDED".to_string()), "{types:?}");
    // The scan repeats harmlessly.
    h.advance(60);
    h.run().await;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_reconciliation_runs WHERE telephony_leg_id = '{leg}' AND action = 'MARKED_ENDED'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn an_operator_disables_one_route_or_interconnect_without_disabling_telephony() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Route a disabled (what `telephony-admin route disable ng-a` does): b carries calls.
    h.sql("UPDATE telecom_routes SET status = 'DISABLED', version = version + 1 WHERE id = 'ng-a'")
        .await;
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(
        h.text(&format!(
            "SELECT route_id FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .as_deref(),
        Some("ng-b")
    );
    // Interconnect b disabled too: nothing is left — a definite, unsent failure.
    h.sql(
        "UPDATE telecom_providers SET status = 'DISABLED', version = version + 1 WHERE id = 'dp-b'",
    )
    .await;
    let (leg, _) = h.originate().await;
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
    assert!(h.provider_a.requests().is_empty() && h.provider_b.requests().len() == 1);
    // Telephony itself stays up and ready; only public reach is affected.
    let ready = h.calling(Method::GET, "/internal/v1/readiness", None).await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.text);
    assert_eq!(ready.json["status"], "ready");
    // Re-enabling restores it.
    h.sql("UPDATE telecom_routes SET status = 'ENABLED', version = version + 1 WHERE id = 'ng-a'")
        .await;
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
}

#[tokio::test]
async fn abuse_signals_are_raised_without_numbers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let before = metrics::counter(
        "telephony_abuse_signals_total",
        &[("kind", "REPEATED_NO_ANSWER")],
    );
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    for _ in 0..3 {
        let body = h.originate_body(contact, Uuid::new_v4(), 1);
        let r = h
            .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
            .await;
        let leg: Uuid = r.json["leg"]["telephonyLegId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        h.run().await;
        let correlation = h.correlation_of(leg).await;
        let e = h
            .provider_a
            .progress(&correlation, NormalizedEvent::LegNoAnswer, h.now());
        h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    }
    h.advance(61);
    h.run().await;
    let evidence = h
        .text("SELECT restricted_evidence::text FROM telephony_incidents WHERE category = 'ABUSE_SIGNAL' \
               AND restricted_evidence ->> 'kind' = 'REPEATED_NO_ANSWER'")
        .await
        .expect("signal raised");
    assert!(evidence.contains(&contact.to_string()), "{evidence}");
    assert!(!evidence.contains("+234"), "{evidence}");
    assert!(
        metrics::counter(
            "telephony_abuse_signals_total",
            &[("kind", "REPEATED_NO_ANSWER")]
        ) > before
    );
    // Rate-limited per instance: an immediate sweep does not re-scan.
    let n = h
        .scalar(
            "SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'ABUSE_SIGNAL' \
                      AND restricted_evidence ->> 'kind' = 'REPEATED_NO_ANSWER'",
        )
        .await;
    h.run().await;
    assert_eq!(
        h.scalar(
            "SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'ABUSE_SIGNAL' \
                  AND restricted_evidence ->> 'kind' = 'REPEATED_NO_ANSWER'"
        )
        .await,
        n
    );
}

#[tokio::test]
async fn metrics_are_bounded_and_never_carry_identifiers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, _) = answered(&h, 3600).await;
    let _ = leg;
    h.provider_a
        .script(&[telephony::providers::deterministic::Script::LoseRequest(
            telephony::providers::UnknownReason::TimeoutAfterSend,
        )]);
    h.originate().await;
    h.run().await;
    let text = h.svc.metrics_text().await.unwrap();
    for expected in [
        "telephony_origination_requests_total{route_class=\"APP_TO_PSTN\"}",
        "telephony_originations_total{outcome=\"ACCEPTED\"}",
        "telephony_originations_total{outcome=\"UNKNOWN\",reason=\"TIMEOUT_AFTER_SEND\"}",
        "telephony_leg_transitions_total{status=\"RINGING\"}",
        "telephony_setup_latency_ms_count",
        "telephony_provider_events_total{disposition=\"APPLIED\"}",
        "telephony_routes{state=\"HEALTHY\"} 2",
        "telephony_route_circuits{state=\"CLOSED\"} 2",
        "telephony_live_legs{state=\"UNKNOWN\"} 1",
        "telephony_outbox_backlog",
    ] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
    // No identifier of any kind is a label value.
    let uuid = regex_lite_uuid(&text);
    assert!(uuid.is_none(), "an id leaked into metrics: {uuid:?}");
    for forbidden in ["+234", "dp-a", "dp-b", "ng-a", "ng-b", "cfm-", "sip:"] {
        assert!(!text.contains(forbidden), "{forbidden} in metrics");
    }
    // Served on its own listener router (never the callback ingress).
    let r = telephony::metrics::router(h.svc.clone())
        .oneshot(
            Request::get("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("telephony_originations_total"));
    let r = h
        .raw(Request::builder().method(Method::GET).uri("/metrics"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "not on the service router");
}

/// The first UUID-shaped token in `text`, if any.
fn regex_lite_uuid(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .find(|t| {
            t.len() == 36
                && t.chars().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == '-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
        })
        .map(str::to_string)
}
