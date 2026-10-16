//! Part IV C4 / C6: transactional outbox — atomicity with the state change, leased
//! multi-instance publication, per-aggregate ordering, bounded retry, dead-lettering,
//! lost-claim safety, immutability and the platform event envelope.
mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::Duration;
use common::{actor, exec, scope, Harness};
use numbering::{
    domain::{Clock, RequestContext, SharedClock},
    infra::{
        outbox::{self, Delivery, EventTransport, NewEvent, OutboxPublisher, PublisherSettings},
        RetryPolicy,
    },
    numbers::{EnvironmentKind, InventorySource, OwnershipClass},
};
use sea_orm::TransactionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

/// Records deliveries; answers from a per-event-type script (default: delivered).
#[derive(Default)]
struct Recorder {
    delivered: Mutex<Vec<Value>>,
    script: Mutex<HashMap<String, Vec<Delivery>>>,
}

impl Recorder {
    fn script(&self, event_type: &str, outcomes: Vec<Delivery>) {
        self.script
            .lock()
            .unwrap()
            .insert(event_type.into(), outcomes);
    }

    fn delivered(&self) -> Vec<Value> {
        self.delivered.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl EventTransport for Recorder {
    async fn deliver(&self, envelope: &Value) -> Delivery {
        let kind = envelope["event_type"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let outcome = {
            let mut script = self.script.lock().unwrap();
            match script.get_mut(&kind) {
                Some(list) if !list.is_empty() => list.remove(0),
                _ => Delivery::Delivered,
            }
        };
        if outcome == Delivery::Delivered {
            self.delivered.lock().unwrap().push(envelope.clone());
        }
        outcome
    }
}

fn settings(max_attempts: i32) -> PublisherSettings {
    PublisherSettings {
        lease: Duration::seconds(30),
        max_attempts,
        retry: RetryPolicy {
            base: Duration::seconds(2),
            max: Duration::seconds(30),
        },
        batch: 100,
    }
}

fn publisher(h: &Harness, transport: Arc<Recorder>, max_attempts: i32) -> OutboxPublisher {
    let clock: SharedClock = Arc::new(h.clock.clone());
    OutboxPublisher::new(
        h.conn().clone(),
        transport,
        clock,
        settings(max_attempts),
        "test",
    )
}

fn event(aggregate: Uuid, n: i64) -> NewEvent {
    NewEvent {
        event_type: "number.test.recorded",
        aggregate_type: "TEST",
        aggregate_id: aggregate,
        scope: None,
        payload: json!({ "n": n }),
    }
}

#[tokio::test]
async fn events_commit_and_roll_back_with_the_state_change() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    // Rolled back: neither the change nor its event exists.
    let txn = h.conn().begin().await.unwrap();
    outbox::enqueue(&txn, event(Uuid::new_v4(), 1), &ctx, h.now())
        .await
        .unwrap();
    txn.rollback().await.unwrap();
    assert_eq!(h.outbox_count().await, 0);
    // Committed: exactly one event, with correlation and causation.
    let txn = h.conn().begin().await.unwrap();
    let id = outbox::enqueue(&txn, event(Uuid::new_v4(), 2), &ctx, h.now())
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(h.outbox_count().await, 1);
    let row = common::strings(
        h.conn(),
        &format!("SELECT correlation_id || '|' || causation_id || '|' || status FROM numbering_outbox_events WHERE id = '{id}'"),
    )
    .await;
    assert_eq!(
        row,
        vec![format!(
            "{}|{}|PENDING",
            ctx.correlation_id.as_str(),
            ctx.request_id.as_str()
        )]
    );
    // A domain operation that fails writes no event (e.g. a refused reservation).
    let r = h.available_number(InventorySource::LiveProvider).await;
    let before = h.outbox_count().await;
    assert!(h
        .numbers
        .reserve(
            &ctx,
            &scope(EnvironmentKind::Sandbox),
            actor(),
            r.id,
            Duration::minutes(5),
            None
        )
        .await
        .is_err());
    assert_eq!(h.outbox_count().await, before);
}

#[tokio::test]
async fn the_database_refuses_unsafe_or_malformed_events_and_never_loses_one() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    for (event_type, payload) in [
        ("number.test.recorded", json!({ "e164": "+15550100001" })),
        (
            "number.test.recorded",
            json!({ "nested": { "api_key": "x" } }),
        ),
        ("number.test.recorded", json!({ "provider_secret": "x" })),
        ("number.test.recorded", json!({ "phone": "x" })),
        ("number_test.recorded", json!({})),
        ("calling.test.recorded", json!({})),
        ("number.recorded", json!({})),
    ] {
        let err = outbox::enqueue(
            h.conn(),
            NewEvent {
                event_type,
                aggregate_type: "TEST",
                aggregate_id: Uuid::new_v4(),
                scope: None,
                payload,
            },
            &ctx,
            h.now(),
        )
        .await;
        assert!(err.is_err(), "{event_type}");
    }
    let id = outbox::enqueue(h.conn(), event(Uuid::new_v4(), 1), &ctx, h.now())
        .await
        .unwrap();
    // Content is immutable; events are never deleted or truncated.
    for sql in [
        format!("UPDATE numbering_outbox_events SET payload = '{{}}' WHERE id = '{id}'"),
        format!(
            "UPDATE numbering_outbox_events SET event_type = 'number.test.other' WHERE id = '{id}'"
        ),
        format!("DELETE FROM numbering_outbox_events WHERE id = '{id}'"),
        "TRUNCATE numbering_outbox_events".to_string(),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
}

#[tokio::test]
async fn publication_preserves_per_aggregate_order_and_conforms_to_the_platform_envelope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Real domain events: a full journey in a sandbox scope.
    let s = scope(EnvironmentKind::Sandbox);
    let j = h.active(&s, OwnershipClass::Personal).await;
    let transport = Arc::new(Recorder::default());
    let p = publisher(&h, transport.clone(), 5);
    let mut total = 0;
    loop {
        let report = p.publish_due().await.unwrap();
        if report.claimed == 0 {
            break;
        }
        total += report.published;
    }
    assert_eq!(total as i64, h.outbox_count().await);
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE status <> 'PUBLISHED'")
            .await,
        0
    );
    let delivered = transport.delivered();
    // Per aggregate, delivery order = commit order (revisions ascend).
    let mut last: HashMap<String, i64> = HashMap::new();
    for e in &delivered {
        if e["event_type"] == "number.assignment.changed" {
            let key = e["payload"]["assignment_id"].as_str().unwrap().to_string();
            let rev = e["payload"]["revision"].as_i64().unwrap();
            assert!(rev > *last.get(&key).unwrap_or(&0), "out of order: {e}");
            last.insert(key, rev);
        }
    }
    // Every envelope validates against the platform envelope and the numbering event
    // contract; scoped events carry their ownership, none carries a number.
    let platform = common::contracts::validator("platform/events/envelope/v1.schema.json");
    let numbering_events = common::contracts::validator("numbering/events/v1.schema.json");
    for e in &delivered {
        assert!(platform.is_valid(e), "platform envelope: {e}");
        assert!(
            numbering_events.is_valid(e),
            "numbering event contract: {e}"
        );
        assert!(!e.to_string().contains("+1555"), "a number leaked: {e}");
        if e["event_type"] == "number.assignment.changed" {
            assert_eq!(e["ownership"]["tenant_id"], s.tenant_id.to_string());
        }
    }
    assert!(delivered
        .iter()
        .any(|e| e["payload"]["assignment_id"] == j.assignment.id.to_string()));
}

#[tokio::test]
async fn failures_back_off_and_exhausted_or_refused_events_are_dead_lettered_not_dropped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    let transport = Arc::new(Recorder::default());
    transport.script(
        "number.test.recorded",
        vec![Delivery::Retryable("timeout".into()); 3],
    );
    let p = publisher(&h, transport.clone(), 3);
    let a = Uuid::new_v4();
    outbox::enqueue(h.conn(), event(a, 1), &ctx, h.now())
        .await
        .unwrap();
    let r = p.publish_due().await.unwrap();
    assert_eq!((r.claimed, r.retried), (1, 1));
    // Not due again before the backoff…
    assert_eq!(p.publish_due().await.unwrap().claimed, 0);
    h.clock.advance(Duration::seconds(2));
    assert_eq!(p.publish_due().await.unwrap().retried, 1);
    h.clock.advance(Duration::seconds(3));
    assert_eq!(
        p.publish_due().await.unwrap().claimed,
        0,
        "the second backoff doubles (4s)"
    );
    h.clock.advance(Duration::seconds(1));
    // …and the third failure exhausts the attempts.
    assert_eq!(p.publish_due().await.unwrap().dead_lettered, 1);
    let row = common::strings(
        h.conn(),
        "SELECT status || '|' || attempts || '|' || last_error FROM numbering_outbox_events",
    )
    .await;
    assert_eq!(row, vec!["DEAD_LETTER|3|timeout".to_string()]);
    // A permanent refusal dead-letters at once; dead letters are visible in readiness.
    let ctx2 = RequestContext::generate();
    let rejecting = Arc::new(Recorder::default());
    rejecting.script(
        "number.test.recorded",
        vec![Delivery::Rejected("schema".into())],
    );
    outbox::enqueue(h.conn(), event(Uuid::new_v4(), 2), &ctx2, h.now())
        .await
        .unwrap();
    assert_eq!(
        publisher(&h, rejecting, 10)
            .publish_due()
            .await
            .unwrap()
            .dead_lettered,
        1
    );
    let ready = h
        .raw(
            axum::http::Request::get("/ready")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ready.body["outboxDeadLetter"], 2);
    // A failing event blocks later events of ITS aggregate only while it is pending.
    let blocked = Uuid::new_v4();
    let flaky = Arc::new(Recorder::default());
    flaky.script(
        "number.test.recorded",
        vec![Delivery::Retryable("down".into())],
    );
    outbox::enqueue(h.conn(), event(blocked, 1), &ctx, h.now())
        .await
        .unwrap();
    outbox::enqueue(h.conn(), event(blocked, 2), &ctx, h.now())
        .await
        .unwrap();
    outbox::enqueue(h.conn(), event(Uuid::new_v4(), 1), &ctx, h.now())
        .await
        .unwrap();
    let p = publisher(&h, flaky.clone(), 10);
    let r = p.publish_due().await.unwrap();
    assert_eq!(
        (r.claimed, r.retried, r.published),
        (2, 1, 1),
        "the second event of the failing aggregate waits"
    );
    h.clock.advance(Duration::seconds(2));
    let r = p.publish_due().await.unwrap();
    assert_eq!(r.published, 1);
    let r = p.publish_due().await.unwrap();
    assert_eq!(r.published, 1);
    let order: Vec<i64> = flaky
        .delivered()
        .iter()
        .filter(|e| e["payload"]["n"].is_i64())
        .map(|e| e["payload"]["n"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![1, 1, 2]);
}

#[tokio::test]
async fn concurrent_publishers_never_deliver_the_same_claim_and_a_dead_publisher_is_taken_over() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    for n in 0..60 {
        outbox::enqueue(h.conn(), event(Uuid::new_v4(), n), &ctx, h.now())
            .await
            .unwrap();
    }
    let transport = Arc::new(Recorder::default());
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let p = Arc::new(publisher(&h, transport.clone(), 5));
        tasks.push(tokio::spawn(async move {
            let mut published = 0;
            loop {
                let r = p.publish_due().await.unwrap();
                if r.claimed == 0 {
                    return published;
                }
                published += r.published;
            }
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    assert_eq!(total, 60);
    let mut ns: Vec<i64> = transport
        .delivered()
        .iter()
        .map(|e| e["payload"]["n"].as_i64().unwrap())
        .collect();
    ns.sort();
    assert_eq!(
        ns,
        (0..60).collect::<Vec<_>>(),
        "each event delivered exactly once"
    );

    // A publisher that claimed and died: its lease expires, another instance takes over,
    // and the dead one's late completion is refused (lost claim), not double-applied.
    outbox::enqueue(h.conn(), event(Uuid::new_v4(), 100), &ctx, h.now())
        .await
        .unwrap();
    let dead = publisher(&h, transport.clone(), 5);
    let dead_token = Uuid::new_v4();
    let claimed = dead.claim(dead_token).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let other = publisher(&h, transport.clone(), 5);
    assert_eq!(
        other.publish_due().await.unwrap().claimed,
        0,
        "leased by the dead publisher"
    );
    h.clock.advance(Duration::seconds(31));
    assert_eq!(other.publish_due().await.unwrap().published, 1);
    let mut report = Default::default();
    dead.deliver_claimed(&claimed[0], dead_token, &mut report)
        .await
        .unwrap();
    assert_eq!(report.lost_claims, 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE status = 'PUBLISHED'")
            .await,
        61
    );
    let _ = h.clock.now();
}
