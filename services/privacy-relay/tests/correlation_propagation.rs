//! §85: one logical operation, one correlation id. A command arriving with an inbound
//! correlation id carries it into every record (audit, outbox envelope, enforcement job)
//! and every outbound hop it causes (the synchronous enforcement delivery, the durable
//! retry by another instance after a failure, and the outbox publication to Calling) —
//! no hop mints a fresh one.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use cf_service_auth::{
    ApplicationEnvironmentKind as Kind, CorrelationId, OutboundServiceRequest, ServiceId,
};
use chrono::Duration;
use common::*;
use privacy_relay::{enforcement::DeliveryFailure, events::EventPublisher};
use serde_json::{json, Value};

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, String)>>);

#[async_trait]
impl EventPublisher for Recorder {
    async fn publish(&self, correlation: &str, envelope: &Value) -> Result<(), &'static str> {
        self.0.lock().unwrap().push((
            correlation.to_string(),
            envelope["correlation_id"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        ));
        Ok(())
    }
}

/// POST as Calling for `consumer`, continuing `correlation`.
async fn post_continuing(
    h: &Harness,
    correlation: &CorrelationId,
    path: &str,
    consumer: &Consumer,
    body: Value,
) -> Response {
    let outbound = OutboundServiceRequest::continuing(ServiceId::privacy_relay(), correlation)
        .with_delegated_context(consumer.delegated());
    let signed = h.ids.calling.sign(&outbound).unwrap();
    let mut builder = Request::builder().method(Method::POST).uri(path);
    for (name, value) in signed.headers().iter() {
        builder = builder.header(name, value);
    }
    h.raw(builder, Some(body)).await
}

#[tokio::test]
async fn the_inbound_correlation_id_reaches_every_record_and_outbound_hop() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let create = CorrelationId::generate();
    let r = post_continuing(
        &h,
        &create,
        "/internal/v1/contexts",
        &c,
        json!({ "idempotencyKey": "corr-create", "request": context_request(&[a, b], "BIDIRECTIONAL", &[]) }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["context"]["contextId"].as_str().unwrap().to_string();

    // Revoke while Calling is down: the synchronous attempt fails, a retry by ANOTHER
    // instance later applies it — both continue the revoke's correlation id.
    h.calling.fail_with(Some(DeliveryFailure::Unavailable));
    let revoke = CorrelationId::generate();
    let r = post_continuing(
        &h,
        &revoke,
        &format!("/internal/v1/contexts/{id}/revoke"),
        &c,
        json!({ "idempotencyKey": "corr-revoke", "request": { "expectedVersion": 1 } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    h.calling.fail_with(None);
    h.clock.advance(Duration::minutes(5));
    assert_eq!(h.sibling().run_enforcement_sweep(10).await.unwrap(), 1);
    let delivered = h.calling.delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].correlation_id.as_str(), revoke.as_str());

    // Records: every audit event / outbox envelope / job of each command carries its id.
    let (cs, rs) = (create.as_str(), revoke.as_str());
    assert_eq!(
        rows(
            h.conn(),
            "SELECT DISTINCT correlation_id FROM relay_audit_events WHERE event_type = 'CONTEXT_CREATED'"
        )
        .await,
        vec![vec![cs.to_string()]]
    );
    assert_eq!(
        rows(
            h.conn(),
            "SELECT DISTINCT correlation_id FROM relay_audit_events WHERE event_type <> 'CONTEXT_CREATED'"
        )
        .await,
        vec![vec![rs.to_string()]],
        "revocation, enforcement request, failure and the retried application"
    );
    assert_eq!(
        rows(
            h.conn(),
            "SELECT DISTINCT correlation_id FROM privacy_relay_enforcement_jobs"
        )
        .await,
        vec![vec![rs.to_string()]]
    );
    // The publication hop to Calling continues each envelope's id.
    let recorder = Arc::new(Recorder::default());
    let publisher = h.sibling().with_event_publisher(recorder.clone());
    let n = publisher.publish_outbox(100).await.unwrap();
    assert!(n >= 4, "{n}");
    let published = recorder.0.lock().unwrap().clone();
    assert!(published.iter().all(|(hop, envelope)| hop == envelope));
    let mut ids: Vec<&str> = published.iter().map(|(hop, _)| hop.as_str()).collect();
    ids.sort();
    ids.dedup();
    let mut expected = vec![cs, rs];
    expected.sort();
    assert_eq!(ids, expected, "no hop minted a fresh correlation id");
}

#[tokio::test]
async fn an_envelope_without_a_valid_correlation_id_is_never_published_under_a_fresh_one() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    h.create_context(&c, context_request(&principals(2), "BIDIRECTIONAL", &[]))
        .await;
    // Corrupt the stored envelope's correlation id behind the service's back.
    exec(
        h.conn(),
        "ALTER TABLE privacy_relay_outbox DISABLE TRIGGER USER",
    )
    .await
    .unwrap();
    exec(
        h.conn(),
        "UPDATE privacy_relay_outbox SET envelope = envelope - 'correlation_id'",
    )
    .await
    .unwrap();
    let recorder = Arc::new(Recorder::default());
    let publisher = h.relay.clone().with_event_publisher(recorder.clone());
    assert_eq!(publisher.publish_outbox(100).await.unwrap(), 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' AND last_error_code = 'MALFORMED_ENVELOPE'").await,
        1,
        "a permanent failure: dead-lettered, never re-correlated"
    );
}
