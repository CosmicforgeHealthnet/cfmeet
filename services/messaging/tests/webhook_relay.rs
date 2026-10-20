//! Part V I §§116–118 — webhooks via the existing platform system: Messaging relays its
//! public-worthy committed events, as platform envelopes, to Calling's platform-event
//! ingress; no content of any security profile is ever relayed.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::StatusCode;
use common::{flows::ALL_SCOPES, *};
use messaging::webhook_relay::{self, PlatformEventSink, RelayFailure};
use serde_json::Value;

#[derive(Default)]
struct Sink {
    seen: Mutex<Vec<Value>>,
    fail: Mutex<Option<RelayFailure>>,
}

#[async_trait]
impl PlatformEventSink for Sink {
    async fn publish(&self, envelope: &Value) -> Result<(), RelayFailure> {
        if let Some(f) = *self.fail.lock().unwrap() {
            return Err(f);
        }
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

#[tokio::test]
async fn public_events_are_relayed_as_platform_envelopes_without_content() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    // PRIVATE_E2EE: a real MLS group; its commits and welcomes are never relayed.
    let (econv, mut a, _b) = h.e2ee_direct(&c).await;
    let sent = h.send_e2ee(&c, &econv, &mut a, "e2ee secret words").await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    // MANAGED.
    let f = h.first_party(false);
    let (mconv, people) = h.managed_group(&f, 2).await;
    let m = h
        .send_managed(&f, &mconv, people[0], "managed secret words")
        .await;
    assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);

    let queued = h
        .scalar("SELECT count(*) FROM messaging_webhook_relay")
        .await;
    let control = h
        .scalar(
            "SELECT count(*) FROM messaging_webhook_relay r JOIN messaging_outbox_events o ON o.id = r.event_id \
              WHERE o.event_type LIKE 'messaging.mls.%' OR o.event_type LIKE 'messaging.device.%' \
                 OR (o.event_type = 'messaging.message.created' AND o.payload->>'message_kind' <> 'APPLICATION')",
        )
        .await;
    assert_eq!(control, 0, "control, MLS and device facts are never queued");

    let sink = Arc::new(Sink::default());
    let r = webhook_relay::relay_due(&h.state, sink.as_ref(), 500)
        .await
        .unwrap();
    assert_eq!(r.relayed as i64, queued);
    let seen = sink.seen.lock().unwrap().clone();
    let types: Vec<&str> = seen
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    for t in [
        "messaging.conversation.created",
        "messaging.member.added",
        "messaging.message.created",
    ] {
        assert!(types.contains(&t), "{t} in {types:?}");
    }
    for e in &seen {
        assert_eq!(e["producer_service"], "cfmeet.messaging");
        conforms("platform/events/envelope/v1.schema.json", e);
        conforms(
            "messaging/events/v1.schema.json",
            &serde_json::json!({ "event_type": e["event_type"], "payload": e["payload"] }),
        );
        assert!(e["event_id"].as_str().unwrap().starts_with("evt_"));
        assert!(e["ownership"]["application_environment_id"].is_string());
        assert!(e["payload"]["conversation_id"].is_string(), "{e}");
        let text = e.to_string();
        for leak in [
            "secret words",
            "ciphertext",
            "\"text\"",
            "epoch_secret",
            "key_package",
        ] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
    }
    // The E2EE application message is relayed as ids only.
    let mid = sent.json["messageId"].as_str().unwrap();
    let e = seen
        .iter()
        .find(|e| e["payload"]["message_id"] == mid)
        .expect("the E2EE message.created");
    assert_eq!(e["payload"]["message_kind"], "APPLICATION");
    // Relayed once.
    assert_eq!(
        webhook_relay::relay_due(&h.state, sink.as_ref(), 500)
            .await
            .unwrap()
            .relayed,
        0
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status <> 'RELAYED'")
            .await,
        0
    );
}

#[tokio::test]
async fn relay_failures_retry_dead_letter_and_wait_for_configuration() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let f = h.first_party(false);
    let (conv, people) = h.managed_group(&f, 2).await;
    let sink = Arc::new(Sink::default());
    // Unconfigured: nothing lost, not counted as attempts.
    webhook_relay::relay_due(&h.state, &webhook_relay::UnconfiguredSink, 50)
        .await
        .unwrap();
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING' AND last_error = 'NOT_CONFIGURED' AND attempts = 0").await,
        1
    );
    // Calling down: retried with backoff.
    *sink.fail.lock().unwrap() = Some(RelayFailure::Unavailable);
    h.sql("UPDATE messaging_webhook_relay SET next_attempt_at = now() - interval '1 day'")
        .await
        .unwrap();
    let r = webhook_relay::relay_due(&h.state, sink.as_ref(), 50)
        .await
        .unwrap();
    assert_eq!((r.relayed, r.retried), (0, 1));
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE last_error = 'UNAVAILABLE' AND attempts = 1 AND next_attempt_at > now()").await,
        1
    );
    // Recovered: everything due is relayed, in order.
    *sink.fail.lock().unwrap() = None;
    h.sql("UPDATE messaging_webhook_relay SET next_attempt_at = now() - interval '1 day'")
        .await
        .unwrap();
    let r = webhook_relay::relay_due(&h.state, sink.as_ref(), 50)
        .await
        .unwrap();
    assert!(r.relayed >= 2, "{r:?}");
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status <> 'RELAYED'")
            .await,
        0
    );
    // A permanent refusal dead-letters, visibly.
    h.send_managed(&f, &conv, people[1], "x").await;
    *sink.fail.lock().unwrap() = Some(RelayFailure::Rejected);
    let r = webhook_relay::relay_due(&h.state, sink.as_ref(), 50)
        .await
        .unwrap();
    assert_eq!(r.dead_lettered, 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'DEAD_LETTER' AND last_error = 'REJECTED'").await,
        1
    );
}
