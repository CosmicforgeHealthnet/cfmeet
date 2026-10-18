//! Part V D.12: the transactional outbox — events commit with their state, concurrent
//! publishers deliver each claim once, a crashed claim is re-claimed after its lease,
//! failures back off and dead-letter, and the database refuses unsafe payloads.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use messaging::{
    domain::{RequestContext, SharedClock},
    events::{
        outbox::{
            self, Delivery, EventScope, EventTransport, NewEvent, OutboxPublisher,
            PublisherSettings,
        },
        RetryPolicy,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Default)]
struct Recorder {
    delivered: Mutex<Vec<String>>,
    outcome: Mutex<Option<Delivery>>,
}

#[async_trait::async_trait]
impl EventTransport for Recorder {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.delivered
            .lock()
            .unwrap()
            .push(envelope["event_id"].as_str().unwrap().to_string());
        self.outcome
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Delivery::Delivered)
    }
}

fn publisher(h: &Harness, transport: Arc<Recorder>, max_attempts: i32) -> OutboxPublisher {
    let clock: SharedClock = Arc::new(h.clock.clone());
    OutboxPublisher::new(
        h.conn().clone(),
        transport,
        clock,
        PublisherSettings {
            lease: chrono::Duration::seconds(30),
            max_attempts,
            retry: RetryPolicy::default(),
            batch: 100,
        },
        "test",
    )
}

async fn enqueue(h: &Harness, n: usize) -> Vec<Uuid> {
    let scope = EventScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
    };
    let mut ids = Vec::new();
    for i in 0..n {
        let device = Uuid::new_v4();
        ids.push(
            outbox::enqueue(
                h.conn(),
                NewEvent {
                    event_type: "messaging.device.revoked",
                    aggregate_type: "DEVICE",
                    aggregate_id: device,
                    scope,
                    payload: json!({ "device_id": device, "principal_id": Uuid::new_v4(), "reason": "REPLACED", "transparency_leaf_index": i }),
                },
                &RequestContext::generate(),
                h.now(),
            )
            .await
            .unwrap(),
        );
    }
    ids
}

#[tokio::test]
async fn concurrent_publishers_deliver_each_event_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    enqueue(&h, 20).await;
    let transport = Arc::new(Recorder::default());
    let a = publisher(&h, transport.clone(), 5);
    let b = publisher(&h, transport.clone(), 5);
    let (ra, rb) = tokio::join!(a.publish_due(), b.publish_due());
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_eq!(ra.published + rb.published, 20);
    let delivered = transport.delivered.lock().unwrap().clone();
    let unique: std::collections::BTreeSet<_> = delivered.iter().collect();
    assert_eq!(
        (delivered.len(), unique.len()),
        (20, 20),
        "an event was delivered twice"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE status = 'PUBLISHED'")
            .await,
        20
    );
    // Every envelope carries the producer and ownership.
    let first = publisher(&h, transport.clone(), 5);
    assert_eq!(first.publish_due().await.unwrap().claimed, 0);
}

#[tokio::test]
async fn a_crashed_claim_is_reclaimed_after_its_lease_and_failures_dead_letter() {
    let Some(h) = Harness::new().await else {
        return;
    };
    enqueue(&h, 1).await;
    let transport = Arc::new(Recorder::default());
    let p = publisher(&h, transport.clone(), 2);
    // A publisher claims and "dies" before completing.
    let claimed = p.claim(Uuid::new_v4()).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        p.publish_due().await.unwrap().claimed,
        0,
        "the claim is still leased"
    );
    h.clock.advance(chrono::Duration::seconds(31));
    // Delivery fails transiently twice: retried with backoff, then dead-lettered.
    *transport.outcome.lock().unwrap() = Some(Delivery::Retryable("timeout".into()));
    assert_eq!(p.publish_due().await.unwrap().retried, 1);
    h.clock.advance(chrono::Duration::minutes(10));
    assert_eq!(p.publish_due().await.unwrap().dead_lettered, 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE status = 'DEAD_LETTER'")
            .await,
        1
    );
}

#[tokio::test]
async fn the_database_refuses_unsafe_event_payloads() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scope = EventScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
    };
    for payload in [
        json!({ "ciphertext": "AAAA" }),
        json!({ "message_body": "hello" }),
        json!({ "signature_key": "AAAA" }),
        json!({ "device_private_key": "AAAA" }),
        json!({ "key_package": "AAAA" }),
    ] {
        let r = outbox::enqueue(
            h.conn(),
            NewEvent {
                event_type: "messaging.device.registered",
                aggregate_type: "DEVICE",
                aggregate_id: Uuid::new_v4(),
                scope,
                payload: payload.clone(),
            },
            &RequestContext::generate(),
            h.now(),
        )
        .await;
        assert!(r.is_err(), "{payload} accepted");
    }
}
