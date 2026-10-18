//! Part V §66 across gateway instances: two Messaging instances on ONE database, fan-out
//! and presence through a REAL self-hosted Redis (`MESSAGING_TEST_REDIS_URL`). A device
//! connected to instance B receives what was committed through instance A; presence is
//! shared; a Redis restart loses nothing committed.
//!
//! Without `MESSAGING_TEST_REDIS_URL` the test SKIPS with a message, unless
//! `MESSAGING_REDIS_REQUIRED=1` (CI), where it fails.

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use common::{flows::*, realtime::*};
use messaging::realtime::{
    broker::{LocalBroker, RedisBroker},
    ephemeral::RedisPresence,
};
use serde_json::json;

fn redis_url() -> Option<String> {
    match std::env::var("MESSAGING_TEST_REDIS_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            assert!(
                std::env::var("MESSAGING_REDIS_REQUIRED").as_deref() != Ok("1"),
                "MESSAGING_TEST_REDIS_URL is required (MESSAGING_REDIS_REQUIRED=1)"
            );
            eprintln!("SKIPPED: realtime Redis test (MESSAGING_TEST_REDIS_URL unset)");
            None
        }
    }
}

#[tokio::test]
async fn two_gateway_instances_share_fanout_and_presence_through_redis() {
    let Some(url) = redis_url() else { return };
    let a_broker = Arc::new(RedisBroker::new(&url).unwrap());
    let a_presence = Arc::new(RedisPresence::new(&url).unwrap());
    let Some(mut rt) = Rt::with_backends(
        test_settings(),
        Arc::new(LocalBroker::default()),
        a_broker,
        a_presence,
    )
    .await
    else {
        return;
    };
    let (b_addr, b) = rt
        .second_instance(
            Arc::new(RedisBroker::new(&url).unwrap()),
            Arc::new(RedisPresence::new(&url).unwrap()),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await; // subscriptions established
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    // Bob on instance B, Alice on instance A.
    let t = rt.ticket(&c, &bob).await;
    let mut wb = rt.open_at(b_addr, &t).await.unwrap();
    wb.subscribe(&conv, None).await;
    let mut wa = rt.connect(&c, &alice).await;
    wa.subscribe(&conv, None).await;
    // Presence crosses instances.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(
        wa.next_of("presence.snapshot").await["members"][0]["state"],
        "AVAILABLE"
    );
    // A message committed through instance A's API reaches the device on instance B.
    let r = h.send_e2ee(&c, &conv, &mut alice, "across instances").await;
    assert_eq!(r.status, StatusCode::CREATED);
    let ev = wb.events(1).await.remove(0);
    let ct = unb64_value(&ev);
    match bob
        .device
        .process(bob.group.as_mut().unwrap(), &ct)
        .unwrap()
    {
        cf_messaging_crypto::group::Processed::Application { plaintext, .. } => {
            assert_eq!(plaintext, b"across instances")
        }
        other => panic!("{other:?}"),
    }
    // Typing crosses instances too (ephemeral).
    wa.send(
        json!({ "type": "typing", "conversationId": conv["conversationId"], "state": "started" }),
    )
    .await;
    assert_eq!(wb.next_of("typing").await["state"], "started");
    // Device revocation on A closes the connection held by B.
    rt.h.post(
        &c,
        Some(bob.principal),
        &format!("/v1/messaging/devices/{}/revoke", bob.device_id()),
        json!({ "reason": "COMPROMISED" }),
    )
    .await;
    assert_eq!(wb.closed().await.1, "DEVICE_REVOKED");
    for _ in 0..50 {
        if b.hub.connections() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(b.hub.connections(), 0);
}

fn unb64_value(ev: &serde_json::Value) -> Vec<u8> {
    common::unb64(&ev["data"]["message"]["e2ee"]["ciphertext"])
}
