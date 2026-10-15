//! Part G §67: Privacy Relay's transactional outbox is published durably — committed
//! envelopes are handed to the (transport-neutral) publisher once, concurrent workers never
//! publish the same row twice, a failing transport keeps rows PENDING with backoff and a
//! bounded failure code, bounded retries end in DEAD_LETTER, and an operator replay
//! publishes it again. Without a publisher nothing is lost (rows stay PENDING).

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use privacy_relay::events::{EventPublisher, OUTBOX_MAX_ATTEMPTS};
use serde_json::Value;

#[derive(Default)]
struct Recorder {
    published: Mutex<Vec<Value>>,
    down: Mutex<bool>,
}

#[async_trait]
impl EventPublisher for Recorder {
    async fn publish(&self, _correlation: &str, envelope: &Value) -> Result<(), &'static str> {
        if *self.down.lock().unwrap() {
            return Err("UNAVAILABLE");
        }
        self.published.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

async fn count(h: &Harness, sql: &str) -> i64 {
    use sea_orm::ConnectionTrait;
    let row =
        h.db.conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
    row.try_get_by_index::<i64>(0).unwrap()
}

#[tokio::test]
async fn committed_events_are_published_once_retried_and_dead_lettered() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h
        .create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    h.lifecycle(&c, &id, "revoke", 1).await;
    let pending = count(
        &h,
        "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'",
    )
    .await;
    assert!(
        pending >= 2,
        "created + revoked are committed with their state"
    );
    // Without a publisher: nothing is published, nothing is lost.
    assert_eq!(h.relay.publish_outbox(100).await.unwrap(), 0);
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' AND attempt_count = 0").await,
        pending
    );
    // Two workers race: every row is published exactly once.
    let recorder = Arc::new(Recorder::default());
    let one = h.relay.clone().with_event_publisher(recorder.clone());
    let two = h.sibling().with_event_publisher(recorder.clone());
    let (x, y) = tokio::join!(one.publish_outbox(100), two.publish_outbox(100));
    assert_eq!((x.unwrap() + y.unwrap()) as i64, pending);
    let published = recorder.published.lock().unwrap().clone();
    assert_eq!(published.len() as i64, pending);
    let ids: std::collections::BTreeSet<&str> = published
        .iter()
        .map(|e| e["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len() as i64, pending, "no duplicates");
    assert!(published
        .iter()
        .all(|e| e["producer_service"] == "cfmeet.privacy-relay" && e["ownership"].is_object()));
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status <> 'PUBLISHED'"
        )
        .await,
        0
    );
    // Transport down: retried with backoff and a bounded code, then dead-lettered.
    h.lifecycle(&c, &id, "revoke", 2).await; // a no-op or conflict: nothing new for sure
    let ctx = h
        .create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    let fresh = count(
        &h,
        "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'",
    )
    .await;
    assert!(fresh >= 1, "{ctx}");
    *recorder.down.lock().unwrap() = true;
    assert_eq!(one.publish_outbox(100).await.unwrap(), 0);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' \
                    AND last_error_code = 'UNAVAILABLE' AND lease_until IS NULL \
                    AND next_attempt_at > updated_at - interval '1 second'"
        )
        .await,
        fresh
    );
    for i in 0..OUTBOX_MAX_ATTEMPTS {
        h.clock.set(at(10 + i as i64 * 10));
        one.publish_outbox(100).await.unwrap();
    }
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER'"
        )
        .await,
        fresh
    );
    // Operator replay after recovery: published exactly once more.
    *recorder.down.lock().unwrap() = false;
    let dead: String = {
        use sea_orm::ConnectionTrait;
        h.db.conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT event_id FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' LIMIT 1"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    };
    assert!(one.replay_dead_lettered_event(&dead).await.unwrap());
    assert!(!one.replay_dead_lettered_event(&dead).await.unwrap());
    let before = recorder.published.lock().unwrap().len();
    assert_eq!(one.publish_outbox(100).await.unwrap(), 1);
    assert_eq!(recorder.published.lock().unwrap().len(), before + 1);
}

/// A transport that refuses with a fixed code (permanent or transient).
struct Refusing(&'static str, Mutex<usize>);

#[async_trait]
impl EventPublisher for Refusing {
    async fn publish(&self, _correlation: &str, _envelope: &Value) -> Result<(), &'static str> {
        *self.1.lock().unwrap() += 1;
        Err(self.0)
    }
}

#[test]
fn http_failures_are_classified_permanent_or_transient() {
    use privacy_relay::events::{classify_status, is_permanent};
    for (status, code, permanent) in [
        (400, "MALFORMED", true),
        (422, "MALFORMED", true),
        (413, "MALFORMED", true),
        (403, "FORBIDDEN", true),
        (401, "UNAUTHENTICATED", false),
        (404, "REJECTED", false),
        (408, "TIMEOUT", false),
        (429, "THROTTLED", false),
        (500, "UNAVAILABLE", false),
        (503, "UNAVAILABLE", false),
    ] {
        assert_eq!(classify_status(status), code, "{status}");
        assert_eq!(is_permanent(code), permanent, "{status}");
    }
    assert!(is_permanent("MALFORMED_ENVELOPE"));
    assert!(!is_permanent("TIMEOUT") && !is_permanent("PUBLISHER_NOT_CONFIGURED"));
}

#[tokio::test]
async fn a_permanent_refusal_is_dead_lettered_at_once_and_raises_one_incident() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    h.create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    h.create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    let pending = count(
        &h,
        "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'",
    )
    .await;
    assert_eq!(pending, 2);
    // Calling refuses the envelope as malformed: no retry can fix it.
    let refusing = Arc::new(Refusing("MALFORMED", Mutex::new(0)));
    let worker = h.relay.clone().with_event_publisher(refusing.clone());
    assert_eq!(worker.publish_outbox(100).await.unwrap(), 0);
    assert_eq!(*refusing.1.lock().unwrap(), 2, "one attempt each");
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' AND attempt_count = 1 AND last_error_code = 'MALFORMED'").await,
        2
    );
    // One deduplicated OPEN incident with two occurrences, a safe summary only.
    let incident = rows(
        h.conn(),
        "SELECT category, dedupe_key, occurrences::text, status, summary FROM privacy_relay_incidents",
    )
    .await;
    assert_eq!(incident.len(), 1, "{incident:?}");
    assert_eq!(incident[0][0], "OUTBOX_DEAD_LETTER");
    assert_eq!(incident[0][1], "OUTBOX_DEAD_LETTER:MALFORMED");
    assert_eq!(
        (incident[0][2].as_str(), incident[0][3].as_str()),
        ("2", "OPEN")
    );
    assert!(!incident[0][4].contains("evt_"));
    // A transient refusal is retried, never dead-lettered on its first attempt.
    h.create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    let transient = Arc::new(Refusing("THROTTLED", Mutex::new(0)));
    let worker = h.relay.clone().with_event_publisher(transient.clone());
    worker.publish_outbox(100).await.unwrap();
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' AND last_error_code = 'THROTTLED'").await,
        1
    );
    // Later, with a healthy transport: the transient row is published, the dead letters
    // are never retried (only an operator replay brings them back).
    h.clock.set(at(600));
    let recorder = Arc::new(Recorder::default());
    let healthy = h.relay.clone().with_event_publisher(recorder.clone());
    assert_eq!(healthy.publish_outbox(100).await.unwrap(), 1);
    assert_eq!(*refusing.1.lock().unwrap(), 2);
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' AND attempt_count = 1").await,
        2
    );
}

/// Publishes nothing: blocks until its worker task is aborted (a crashed instance).
struct Hanging(tokio::sync::Notify);

#[async_trait]
impl EventPublisher for Hanging {
    async fn publish(&self, _correlation: &str, _envelope: &Value) -> Result<(), &'static str> {
        self.0.notify_one();
        std::future::pending::<()>().await;
        Err("UNAVAILABLE")
    }
}

#[tokio::test]
async fn a_crashed_publishers_lease_expires_and_another_instance_publishes_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    for _ in 0..3 {
        h.create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
            .await;
    }
    // Instance A claims every row (lease) and dies mid-publication.
    let hanging = Arc::new(Hanging(tokio::sync::Notify::new()));
    let crashed = h.relay.clone().with_event_publisher(hanging.clone());
    let task = tokio::spawn(async move { crashed.publish_outbox(100).await });
    hanging.0.notified().await;
    task.abort();
    let _ = task.await;
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' AND lease_until IS NOT NULL AND attempt_count = 1").await,
        3,
        "claimed under a lease"
    );
    // While the lease holds, another instance does not touch the rows.
    let recorder = Arc::new(Recorder::default());
    let survivor = h.sibling().with_event_publisher(recorder.clone());
    assert_eq!(survivor.publish_outbox(100).await.unwrap(), 0);
    assert!(recorder.published.lock().unwrap().is_empty());
    // The lease expires: the survivor publishes each row exactly once.
    h.clock.set(t0() + chrono::Duration::seconds(61));
    assert_eq!(survivor.publish_outbox(100).await.unwrap(), 3);
    assert_eq!(survivor.publish_outbox(100).await.unwrap(), 0);
    let published = recorder.published.lock().unwrap().clone();
    let ids: std::collections::BTreeSet<&str> = published
        .iter()
        .map(|e| e["event_id"].as_str().unwrap())
        .collect();
    assert_eq!((published.len(), ids.len()), (3, 3));
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PUBLISHED' AND lease_until IS NULL AND attempt_count = 2").await,
        3
    );
}
