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
