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

// ---------------------------------------------------------------------------------------
// Part VI §53: direct publication to the Developer Platform (receivers)
// ---------------------------------------------------------------------------------------

mod destinations {
    use std::collections::BTreeSet;

    use cf_service_auth::{RuntimeEnvironment, ServiceId};
    use common::receiver::{producer_client, signing_key, Receiver};
    use messaging::webhook_relay::{
        CallingPlatformEvents, DeveloperPlatformEvents, EventDestination, PublicEventsDestination,
    };

    use super::*;

    struct Receivers {
        calling: Receiver,
        dp: Receiver,
        calling_sink: CallingPlatformEvents,
        dp_sink: DeveloperPlatformEvents,
    }

    /// Real receivers verifying Messaging's real service token.
    async fn receivers() -> Receivers {
        let me = ServiceId::messaging();
        let key = signing_key("messaging-dest-1");
        let calling = Receiver::calling(&me, &key, RuntimeEnvironment::Test).await;
        let dp = Receiver::developer_platform(&me, &key, RuntimeEnvironment::Test).await;
        let client = producer_client(me, key, RuntimeEnvironment::Test, &[&calling, &dp]);
        Receivers {
            calling,
            dp,
            calling_sink: CallingPlatformEvents::new(client.clone()),
            dp_sink: DeveloperPlatformEvents::new(client),
        }
    }

    impl Receivers {
        /// The sinks `main` wires for `destination`.
        fn sinks(
            &self,
            destination: PublicEventsDestination,
        ) -> Vec<(EventDestination, &dyn PlatformEventSink)> {
            destination
                .destinations()
                .iter()
                .map(|d| {
                    let sink: &dyn PlatformEventSink = match d {
                        EventDestination::Calling => &self.calling_sink,
                        EventDestination::DeveloperPlatform => &self.dp_sink,
                    };
                    (*d, sink)
                })
                .collect()
        }
    }

    fn assert_safe(envelopes: &[Value]) {
        for e in envelopes {
            assert_eq!(e["producer_service"], "cfmeet.messaging");
            conforms("platform/events/envelope/v1.schema.json", e);
            assert!(e["ownership"]["application_environment_id"].is_string());
            let text = e.to_string();
            for leak in ["secret words", "ciphertext", "\"text\"", "epoch_secret"] {
                assert!(!text.contains(leak), "{leak} in {text}");
            }
        }
    }

    async fn queued(h: &Harness) -> i64 {
        h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING'")
            .await
    }

    async fn due_now(h: &Harness) {
        h.sql("UPDATE messaging_webhook_relay SET next_attempt_at = now() - interval '1 day' WHERE status = 'PENDING'")
            .await
            .unwrap();
    }

    #[test]
    fn the_default_destination_is_calling() {
        assert_eq!(
            PublicEventsDestination::default().destinations(),
            &[EventDestination::Calling]
        );
        assert_eq!(
            PublicEventsDestination::parse("developer-platform"),
            Some(PublicEventsDestination::DeveloperPlatform)
        );
        assert_eq!(
            PublicEventsDestination::parse("both"),
            Some(PublicEventsDestination::Both)
        );
        assert_eq!(PublicEventsDestination::parse("local"), None);
    }

    #[tokio::test]
    async fn the_default_relays_to_calling_only_and_developer_platform_only_to_it() {
        let Some(h) = Harness::new().await else {
            return;
        };
        let f = h.first_party(false);
        let (conv, people) = h.managed_group(&f, 2).await;
        let n = queued(&h).await;
        let r = receivers().await;
        let report = webhook_relay::relay_due_to(
            &h.state,
            &r.sinks(PublicEventsDestination::default()),
            500,
        )
        .await
        .unwrap();
        assert_eq!(report.relayed as i64, n);
        assert_eq!(r.calling.accepted().len() as i64, n);
        assert!(r.dp.attempts().is_empty());
        assert_safe(&r.calling.accepted());
        assert_eq!(
            h.scalar(
                "SELECT count(*) FROM messaging_webhook_relay WHERE status = 'RELAYED' \
                        AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome IS NULL"
            )
            .await,
            n
        );
        // developer-platform: only the Developer Platform, the same envelopes.
        let m = h
            .send_managed(&f, &conv, people[0], "managed secret words")
            .await;
        assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);
        let n = queued(&h).await;
        assert!(n >= 1);
        let report = webhook_relay::relay_due_to(
            &h.state,
            &r.sinks(PublicEventsDestination::DeveloperPlatform),
            500,
        )
        .await
        .unwrap();
        assert_eq!(report.relayed as i64, n);
        assert_eq!(r.dp.accepted().len() as i64, n);
        assert_safe(&r.dp.accepted());
        assert!(r
            .dp
            .accepted()
            .iter()
            .any(|e| e["event_type"] == "messaging.message.created"));
        for (e, correlation) in r.dp.attempts() {
            assert_eq!(e["correlation_id"].as_str().unwrap(), correlation);
        }
        assert_eq!(
            h.scalar(
                "SELECT count(*) FROM messaging_webhook_relay WHERE status = 'RELAYED' \
                        AND developer_platform_outcome = 'ACCEPTED' AND calling_outcome IS NULL"
            )
            .await,
            n
        );
    }

    #[tokio::test]
    async fn both_relays_once_each_and_a_retry_resends_only_to_the_failed_receiver() {
        let Some(h) = Harness::new().await else {
            return;
        };
        let f = h.first_party(false);
        h.managed_group(&f, 2).await;
        let n = queued(&h).await;
        let r = receivers().await;
        let sinks = r.sinks(PublicEventsDestination::Both);
        // The Developer Platform is down: Calling accepts, the row waits for the other.
        r.dp.script(&[(503, "UNAVAILABLE")]);
        let report = webhook_relay::relay_due_to(&h.state, &sinks, 500)
            .await
            .unwrap();
        assert_eq!((report.relayed, report.retried), (0, 1), "{report:?}");
        assert_eq!(r.calling.accepted().len(), 1);
        assert_eq!(
            h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'PENDING' AND attempts = 1 \
                        AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome IS NULL \
                        AND last_error = 'DEVELOPER_PLATFORM_UNAVAILABLE'")
                .await,
            1
        );
        // Recovered: everything is relayed; the retried row goes to the Developer Platform only.
        due_now(&h).await;
        let report = webhook_relay::relay_due_to(&h.state, &sinks, 500)
            .await
            .unwrap();
        assert_eq!(report.relayed as i64, n, "{report:?}");
        assert_eq!(r.calling.attempts().len() as i64, n, "once each at Calling");
        assert_eq!(r.dp.accepted().len() as i64, n);
        assert_eq!(r.dp.attempts().len() as i64, n + 1);
        let ids = |rx: &Receiver| -> BTreeSet<String> {
            rx.accepted()
                .iter()
                .map(|e| e["event_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(&r.calling), ids(&r.dp));
        assert_eq!(
            h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status <> 'RELAYED' \
                        OR calling_outcome <> 'ACCEPTED' OR developer_platform_outcome <> 'ACCEPTED'")
                .await,
            0
        );
    }

    #[tokio::test]
    async fn refusals_dead_letter_as_before_without_holding_calling_back() {
        let Some(h) = Harness::new().await else {
            return;
        };
        let f = h.first_party(false);
        let (conv, people) = h.managed_group(&f, 2).await;
        let r = receivers().await;
        // Drain the group's events first.
        webhook_relay::relay_due_to(&h.state, &r.sinks(PublicEventsDestination::Both), 500)
            .await
            .unwrap();
        // Both: the Developer Platform refuses, Calling accepts -> dead letter, visibly.
        h.send_managed(&f, &conv, people[1], "x").await;
        r.dp.script(&[(422, "INVALID_EVENT")]);
        let report =
            webhook_relay::relay_due_to(&h.state, &r.sinks(PublicEventsDestination::Both), 500)
                .await
                .unwrap();
        assert_eq!(report.dead_lettered, 1, "{report:?}");
        assert_eq!(
            h.scalar("SELECT count(*) FROM messaging_webhook_relay WHERE status = 'DEAD_LETTER' \
                        AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome = 'REJECTED' \
                        AND last_error = 'DEVELOPER_PLATFORM_REJECTED'")
                .await,
            1
        );
        // Calling alone (default): a 4xx refusal dead-letters with today's code.
        h.send_managed(&f, &conv, people[1], "y").await;
        r.calling.script(&[(400, "INVALID_EVENT")]);
        let report = webhook_relay::relay_due_to(
            &h.state,
            &r.sinks(PublicEventsDestination::default()),
            500,
        )
        .await
        .unwrap();
        assert_eq!(report.dead_lettered, 1, "{report:?}");
        assert_eq!(
            h.scalar(
                "SELECT count(*) FROM messaging_webhook_relay WHERE status = 'DEAD_LETTER' \
                        AND last_error = 'REJECTED' AND calling_outcome = 'REJECTED'"
            )
            .await,
            1
        );
    }
}
