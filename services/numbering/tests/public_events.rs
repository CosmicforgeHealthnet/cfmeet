//! Part VI §53: Numbering publishes its public number lifecycle events directly to the
//! Developer Platform (`developer-event.publish`). The receiver here is a real HTTP
//! server verifying Numbering's real service token against a trust bundle granting only
//! `developer-event.publish`; the events are the real ones of a full number journey.
mod common;

use std::{collections::BTreeSet, sync::Arc};

use cf_service_auth::{RuntimeEnvironment, ServiceId};
use chrono::Duration;
use common::{
    receiver::{producer_client, signing_key, Receiver},
    scope, Harness,
};
use numbering::{
    config::NumberingConfig,
    domain::SharedClock,
    infra::{
        outbox::{OutboxPublisher, PublishReport, PublisherSettings},
        platform_events::{
            is_public, DeveloperPlatformEvents, PublicEventsDestination, PUBLIC_EVENT_TYPES,
        },
        RetryPolicy,
    },
    numbers::{EnvironmentKind, OwnershipClass},
};
use serde_json::Value;

async fn developer_platform(h: &Harness) -> (Receiver, OutboxPublisher) {
    let me = ServiceId::numbering();
    let key = signing_key("numbering-dest-1");
    let dp = Receiver::developer_platform(&me, &key, RuntimeEnvironment::Test).await;
    let client = producer_client(me, key, RuntimeEnvironment::Test, &[&dp]);
    let clock: SharedClock = Arc::new(h.clock.clone());
    let publisher = OutboxPublisher::new(
        h.conn().clone(),
        Arc::new(DeveloperPlatformEvents::new(client)),
        clock,
        PublisherSettings {
            lease: Duration::seconds(30),
            max_attempts: 5,
            retry: RetryPolicy::default(),
            batch: 50,
        },
        "test",
    );
    (dp, publisher)
}

async fn drain(p: &OutboxPublisher) -> PublishReport {
    let mut total = PublishReport::default();
    loop {
        let r = p.publish_due().await.unwrap();
        if r.claimed == 0 {
            return total;
        }
        total.claimed += r.claimed;
        total.published += r.published;
        total.retried += r.retried;
        total.dead_lettered += r.dead_lettered;
    }
}

#[test]
fn the_default_destination_is_the_developer_platform() {
    let base = |k: &str| (k == "DATABASE_URL").then(|| "postgres://localhost/numbering".into());
    let config = NumberingConfig::from_lookup(base).unwrap();
    assert_eq!(
        config.public_events,
        PublicEventsDestination::DeveloperPlatform
    );
    let none = NumberingConfig::from_lookup(|k| match k {
        "DATABASE_URL" => Some("postgres://localhost/numbering".into()),
        "NUMBERING_PUBLIC_EVENTS_DESTINATION" => Some("none".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(none.public_events, PublicEventsDestination::None);
    for invalid in ["calling", "both", "local"] {
        assert!(NumberingConfig::from_lookup(|k| match k {
            "DATABASE_URL" => Some("postgres://localhost/numbering".into()),
            "NUMBERING_PUBLIC_EVENTS_DESTINATION" => Some(invalid.into()),
            _ => None,
        })
        .is_err());
    }
}

#[tokio::test]
async fn public_lifecycle_events_of_customer_scopes_reach_the_developer_platform_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let j = h.active(&s, OwnershipClass::Personal).await;
    let (dp, p) = developer_platform(&h).await;
    let r = drain(&p).await;
    assert_eq!(r.published as i64, h.outbox_count().await, "{r:?}");
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE status <> 'PUBLISHED'")
            .await,
        0,
        "internal facts are acknowledged, not left pending"
    );
    let public = h
        .scalar(&format!(
            "SELECT count(*) FROM numbering_outbox_events WHERE environment_id IS NOT NULL \
               AND event_type IN ({})",
            PUBLIC_EVENT_TYPES
                .iter()
                .map(|t| format!("'{t}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await;
    let seen = dp.accepted();
    assert!(public > 0);
    assert_eq!(seen.len() as i64, public, "exactly the public events");
    assert_eq!(dp.attempts().len(), seen.len(), "each sent once");
    let ids: BTreeSet<&str> = seen
        .iter()
        .map(|e| e["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), seen.len());
    let platform = common::contracts::validator("platform/events/envelope/v1.schema.json");
    let numbering_events = common::contracts::validator("numbering/events/v1.schema.json");
    for e in &seen {
        assert!(is_public(e));
        assert_eq!(e["producer_service"], "cfmeet.numbering");
        assert!(e["event_type"].as_str().unwrap().starts_with("number."));
        assert_eq!(e["ownership"]["tenant_id"], s.tenant_id.to_string());
        assert_eq!(
            e["ownership"]["application_environment_id"],
            s.environment_id.to_string()
        );
        assert!(platform.is_valid(e), "platform envelope: {e}");
        assert!(numbering_events.is_valid(e), "numbering contract: {e}");
        let keys: Vec<&String> = e["payload"].as_object().unwrap().keys().collect();
        for k in keys {
            for unsafe_key in [
                "provider_ref",
                "provider_id",
                "provider_number",
                "kyc",
                "e164",
            ] {
                assert!(!k.contains(unsafe_key), "{k} in {e}");
            }
        }
        let text = e.to_string();
        assert!(!text.contains("+1555"), "a number leaked: {e}");
    }
    // The hop continues each event's committed correlation id.
    for (e, correlation) in dp.attempts() {
        assert_eq!(e["correlation_id"].as_str().unwrap(), correlation);
    }
    let types: BTreeSet<&str> = seen
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(types.contains("number.reservation.changed"), "{types:?}");
    assert!(types.contains("number.assignment.changed"), "{types:?}");
    assert!(seen
        .iter()
        .any(|e| e["payload"]["assignment_id"] == j.assignment.id.to_string()));
    // Nothing is published twice.
    assert_eq!(drain(&p).await.claimed, 0);
    assert_eq!(dp.attempts().len(), seen.len());
}

#[tokio::test]
async fn failures_retry_only_the_failed_event_and_refusals_dead_letter() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    h.active(&s, OwnershipClass::Personal).await;
    let (dp, p) = developer_platform(&h).await;
    // Unavailable, then throttled (transient), then a permanent refusal.
    dp.script(&[
        (503, "UNAVAILABLE"),
        (429, "RATE_LIMITED"),
        (422, "INVALID_EVENT"),
    ]);
    let r = drain(&p).await;
    assert!(r.retried >= 1, "{r:?}");
    for _ in 0..3 {
        h.clock.advance(Duration::minutes(10));
        drain(&p).await;
    }
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM numbering_outbox_events WHERE status = 'DEAD_LETTER' \
               AND last_error = 'REFUSED INVALID_EVENT'"
        )
        .await,
        1,
        "a 4xx refusal dead-letters at once"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE status = 'PENDING'")
            .await,
        0
    );
    // Every other public event was accepted exactly once despite the retries.
    let accepted: Vec<Value> = dp.accepted();
    let ids: BTreeSet<String> = accepted
        .iter()
        .map(|e| e["event_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), accepted.len());
    assert_eq!(dp.attempts().len(), accepted.len() + 3);
}

#[tokio::test]
async fn an_untrusted_key_is_refused_by_the_developer_platform() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    h.active(&s, OwnershipClass::Personal).await;
    let me = ServiceId::numbering();
    let dp = Receiver::developer_platform(
        &me,
        &signing_key("numbering-dest-1"),
        RuntimeEnvironment::Test,
    )
    .await;
    let rogue = producer_client(
        me,
        signing_key("numbering-dest-2"),
        RuntimeEnvironment::Test,
        &[&dp],
    );
    let clock: SharedClock = Arc::new(h.clock.clone());
    let p = OutboxPublisher::new(
        h.conn().clone(),
        Arc::new(DeveloperPlatformEvents::new(rogue)),
        clock,
        PublisherSettings {
            lease: Duration::seconds(30),
            max_attempts: 5,
            retry: RetryPolicy::default(),
            batch: 50,
        },
        "test",
    );
    drain(&p).await;
    assert!(dp.unauthenticated() >= 1);
    assert!(dp.accepted().is_empty());
}
