//! Part VI §53: Privacy Relay publishes its committed envelopes to Calling (default,
//! unchanged), to the Developer Platform, or to both (shadow comparison). The receivers
//! are real HTTP servers verifying Privacy Relay's real service token against trust
//! bundles granting exactly `platform-event.publish` (Calling) / `developer-event.publish`
//! (Developer Platform); every receiver's outcome is durable, so a retry re-sends only to
//! the receiver that has not accepted.

mod common;

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use cf_service_auth::{ApplicationEnvironmentKind as Kind, RuntimeEnvironment, ServiceId};
use common::{
    receiver::{producer_client, signing_key, Receiver},
    *,
};
use jsonschema::{Registry, Resource, Validator};
use privacy_relay::{
    config::RelayConfig,
    events::{EventDestination, HttpEventPublisher, PublicEventsDestination},
    service::RelayService,
};
use serde_json::{json, Value};

fn read(rel: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap()
}

fn validator(reference: &str) -> Validator {
    let mut builder = Registry::new();
    for c in read("manifest.json")["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    let registry = builder.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(
            &json!({ "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "$ref": format!("https://schemas.cfmeet.com/{reference}") }),
        )
        .unwrap()
}

async fn count(h: &Harness, sql: &str) -> i64 {
    use sea_orm::ConnectionTrait;
    h.db.conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

struct Receivers {
    calling: Receiver,
    dp: Receiver,
    relay: RelayService,
}

/// The relay wired exactly as `main` wires `PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION`.
async fn wired(h: &Harness, destination: PublicEventsDestination) -> Receivers {
    let me = ServiceId::privacy_relay();
    let key = signing_key("privacy-relay-dest-1");
    let calling = Receiver::calling(&me, &key, RuntimeEnvironment::Test).await;
    let dp = Receiver::developer_platform(&me, &key, RuntimeEnvironment::Test).await;
    let client = producer_client(me, key, RuntimeEnvironment::Test, &[&calling, &dp]);
    let relay = h
        .relay
        .clone()
        .with_event_publisher(Arc::new(HttpEventPublisher::new(client.clone())))
        .with_developer_event_publisher(Arc::new(HttpEventPublisher::to(
            client,
            EventDestination::DeveloperPlatform,
        )))
        .with_public_events_destination(destination);
    Receivers { calling, dp, relay }
}

async fn two_events(h: &Harness) -> i64 {
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h
        .create_context(&c, context_request(&[a, b], "BIDIRECTIONAL", &[]))
        .await;
    h.lifecycle(&c, ctx["contextId"].as_str().unwrap(), "revoke", 1)
        .await;
    count(
        h,
        "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING'",
    )
    .await
}

fn assert_conformant(envelopes: &[Value]) {
    let envelope = validator("platform/events/envelope/v1.schema.json");
    for e in envelopes {
        assert!(envelope.is_valid(e), "envelope: {e}");
        assert_eq!(e["producer_service"], "cfmeet.privacy-relay");
        assert!(e["ownership"]["application_environment_id"].is_string());
        let text = e.to_string();
        for leak in ["+1", "+234", "contact_plaintext", "token", "secret"] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
    }
}

#[test]
fn the_default_destination_is_calling() {
    let config = RelayConfig::from_lookup(&|k: &str| {
        (k == "DATABASE_URL").then(|| "postgres://u:p@127.0.0.1/db".to_string())
    })
    .unwrap();
    assert_eq!(config.public_events, PublicEventsDestination::Calling);
    assert_eq!(
        config.public_events.destinations(),
        &[EventDestination::Calling]
    );
    for (value, expected) in [
        (
            "developer-platform",
            Some(PublicEventsDestination::DeveloperPlatform),
        ),
        ("both", Some(PublicEventsDestination::Both)),
        ("local", None),
    ] {
        let parsed = RelayConfig::from_lookup(&|k: &str| match k {
            "DATABASE_URL" => Some("postgres://u:p@127.0.0.1/db".to_string()),
            "PRIVACY_RELAY_PUBLIC_EVENTS_DESTINATION" => Some(value.to_string()),
            _ => None,
        });
        assert_eq!(parsed.ok().map(|c| c.public_events), expected, "{value}");
    }
}

#[tokio::test]
async fn the_default_publishes_to_calling_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pending = two_events(&h).await;
    let r = wired(&h, PublicEventsDestination::default()).await;
    assert_eq!(r.relay.publish_outbox(100).await.unwrap() as i64, pending);
    assert_eq!(r.calling.accepted().len() as i64, pending);
    assert!(r.dp.attempts().is_empty());
    assert_conformant(&r.calling.accepted());
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PUBLISHED' \
                     AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome IS NULL"
        )
        .await,
        pending
    );
}

#[tokio::test]
async fn developer_platform_only_publishes_to_the_developer_platform() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pending = two_events(&h).await;
    let r = wired(&h, PublicEventsDestination::DeveloperPlatform).await;
    assert_eq!(r.relay.publish_outbox(100).await.unwrap() as i64, pending);
    assert!(r.calling.attempts().is_empty(), "Calling receives nothing");
    let seen = r.dp.accepted();
    assert_eq!(seen.len() as i64, pending);
    assert_conformant(&seen);
    for (e, correlation) in r.dp.attempts() {
        assert_eq!(e["correlation_id"].as_str().unwrap(), correlation);
    }
    assert_eq!(r.relay.publish_outbox(100).await.unwrap(), 0, "once");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PUBLISHED' \
                     AND developer_platform_outcome = 'ACCEPTED' AND calling_outcome IS NULL"
        )
        .await,
        pending
    );
}

#[tokio::test]
async fn both_publishes_once_each_and_a_retry_resends_only_to_the_failed_receiver() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pending = two_events(&h).await;
    let r = wired(&h, PublicEventsDestination::Both).await;
    // The Developer Platform is down for every first attempt.
    r.dp.script(&vec![(503, "UNAVAILABLE"); pending as usize]);
    assert_eq!(r.relay.publish_outbox(100).await.unwrap(), 0);
    assert_eq!(r.calling.accepted().len() as i64, pending);
    assert!(r.dp.accepted().is_empty());
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' \
                     AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome IS NULL \
                     AND last_error_code = 'DEVELOPER_PLATFORM_UNAVAILABLE'"
        )
        .await,
        pending
    );
    // The retry reaches the Developer Platform only.
    h.clock.set(at(10));
    assert_eq!(r.relay.publish_outbox(100).await.unwrap() as i64, pending);
    assert_eq!(
        r.calling.attempts().len() as i64,
        pending,
        "once each at Calling"
    );
    assert_eq!(r.dp.accepted().len() as i64, pending);
    assert_eq!(r.dp.attempts().len() as i64, 2 * pending);
    let ids = |rx: &Receiver| -> BTreeSet<String> {
        rx.accepted()
            .iter()
            .map(|e| e["event_id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&r.calling), ids(&r.dp));
    assert_conformant(&r.dp.accepted());
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PUBLISHED' \
                     AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome = 'ACCEPTED'"
        )
        .await,
        pending
    );
}

#[tokio::test]
async fn refusals_dead_letter_as_before_without_holding_calling_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pending = two_events(&h).await;
    let r = wired(&h, PublicEventsDestination::Both).await;
    // The Developer Platform refuses one envelope for good; Calling accepts both.
    r.dp.script(&[(422, "INVALID_EVENT")]);
    assert_eq!(
        r.relay.publish_outbox(100).await.unwrap() as i64,
        pending - 1
    );
    assert_eq!(r.calling.accepted().len() as i64, pending);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' \
                     AND calling_outcome = 'ACCEPTED' AND developer_platform_outcome = 'REJECTED' \
                     AND last_error_code = 'DEVELOPER_PLATFORM_MALFORMED'"
        )
        .await,
        1
    );
    // Single receiver: a 4xx refusal still dead-letters at once (unchanged semantics).
    let pending = two_events(&h).await;
    let r = wired(&h, PublicEventsDestination::DeveloperPlatform).await;
    r.dp.script(&[(400, "INVALID_EVENT")]);
    r.relay.publish_outbox(100).await.unwrap();
    assert_eq!(r.dp.accepted().len() as i64, pending - 1);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'DEAD_LETTER' \
                     AND developer_platform_outcome = 'REJECTED'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn an_unconfigured_receiver_holds_its_events_without_spending_attempts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pending = two_events(&h).await;
    let me = ServiceId::privacy_relay();
    let key = signing_key("privacy-relay-dest-1");
    let calling = Receiver::calling(&me, &key, RuntimeEnvironment::Test).await;
    let client = producer_client(me, key, RuntimeEnvironment::Test, &[&calling]);
    // `both`, but only Calling has a publisher.
    let relay = h
        .relay
        .clone()
        .with_event_publisher(Arc::new(HttpEventPublisher::new(client)))
        .with_public_events_destination(PublicEventsDestination::Both);
    assert_eq!(relay.publish_outbox(100).await.unwrap(), 0);
    assert_eq!(calling.accepted().len() as i64, pending);
    assert_eq!(
        count(&h, "SELECT count(*) FROM privacy_relay_outbox WHERE status = 'PENDING' AND attempt_count = 0 \
                     AND calling_outcome = 'ACCEPTED' AND last_error_code = 'PUBLISHER_NOT_CONFIGURED'").await,
        pending
    );
    h.clock.set(at(10));
    relay.publish_outbox(100).await.unwrap();
    assert_eq!(
        calling.attempts().len() as i64,
        pending,
        "never re-sent to Calling"
    );
}
