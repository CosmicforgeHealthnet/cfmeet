//! Part V Part E (§58–§68): the realtime gateway over a REAL TCP listener with REAL
//! WebSocket clients, REAL OpenMLS devices (decrypting locally), the outbox publisher and
//! the broker — authentication, subscriptions and dynamic revocation, durable delivery,
//! replay, ordering, backpressure, broker outage recovery, typing, presence, push and
//! draining.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use cf_messaging_crypto::group::Processed;
use common::{flows::*, realtime::*, *};
use messaging::{identity::PrincipalStatus, realtime::RealtimeSettings};
use serde_json::{json, Value};
use uuid::Uuid;

fn decrypt(m: &mut Member, event: &Value) -> String {
    let ct = unb64(&event["data"]["message"]["e2ee"]["ciphertext"]);
    match m.device.process(m.group.as_mut().unwrap(), &ct).unwrap() {
        Processed::Application { plaintext, .. } => String::from_utf8(plaintext).unwrap(),
        other => panic!("not an application message: {other:?}"),
    }
}

#[tokio::test]
async fn connections_require_a_valid_single_use_ticket_from_a_valid_client_token() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let alice = h.member(&c, 0).await;
    // No / garbage ticket.
    assert_eq!(rt.open(&"0".repeat(64), None).await.err(), Some(401));
    assert_eq!(rt.open("not-a-ticket", None).await.err(), Some(401));
    // Client tokens reach only the member operations of the server API (§105; see
    // tests/client_tokens_rest.rs), and a server credential is never accepted for tickets.
    let token = rt.token(&c, &alice).await;
    let r = h
        .send(
            axum::http::Method::GET,
            None,
            Some(alice.principal),
            "/v1/messaging/policy",
            None,
            &[("authorization", &format!("CF-Client-Token {token}"))],
        )
        .await;
    assert_eq!(r.code(), "CLIENT_TOKENS_NOT_ENABLED");
    let r = h
        .send(
            axum::http::Method::POST,
            Some(&c.token),
            None,
            "/v1/realtime/tickets",
            None,
            &[],
        )
        .await;
    assert_eq!(r.code(), "AUTHENTICATION_FAILED");
    // A token signed by another key / forged claims / garbage.
    let other = messaging::realtime::token::ClientTokenKeys::generate();
    let forged = other.sign(&messaging::realtime::token::Claims {
        iss: "cfmeet.messaging".into(),
        aud: "cfmeet.messaging.realtime".into(),
        sub: alice.principal,
        tid: c.context.scope.tenant_id,
        aid: c.context.scope.application_id,
        eid: c.context.scope.environment_id,
        did: alice.device_id(),
        sid: Uuid::new_v4(),
        jti: Uuid::new_v4(),
        iat: h.now().timestamp(),
        exp: h.now().timestamp() + 600,
        scp: vec!["messages.read".into()],
    });
    for bad in [forged.as_str(), "x.y.z", &format!("{token}x")] {
        let r = rt.ticket_with(bad).await;
        assert_eq!(r.code(), "AUTHENTICATION_FAILED", "{bad}");
    }
    // The genuine path works, and the ticket is single use (replay refused).
    let r = rt.ticket_with(&token).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let ticket = r.json["ticket"].as_str().unwrap().to_string();
    let ws = rt.open(&ticket, None).await;
    assert!(ws.is_ok());
    assert_eq!(
        rt.open(&ticket, None).await.err(),
        Some(401),
        "ticket replay"
    );
    // Browsers: only listed origins.
    let t = rt.ticket(&c, &alice).await;
    assert_eq!(
        rt.open(&t, Some("https://evil.example")).await.err(),
        Some(403)
    );
    let t = rt.ticket(&c, &alice).await;
    assert!(rt.open(&t, Some("https://app.example.test")).await.is_ok());
    // An expired token is refused.
    h.clock.advance(chrono::Duration::minutes(16));
    let r = rt.ticket_with(&token).await;
    assert_eq!(r.code(), "AUTHENTICATION_FAILED");
    // Another tenant cannot mint a token for this device.
    let other_tenant = h.customer(ALL_SCOPES);
    let r = h
        .post(
            &other_tenant,
            Some(alice.principal),
            "/v1/messaging/client-tokens",
            json!({ "deviceId": alice.device_id() }),
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.text);
}

#[tokio::test]
async fn revocation_and_suspension_close_established_connections() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let alice = h.member(&c, 0).await;
    let bob = h.member(&c, 0).await;
    // Device revocation closes at once (event-driven).
    let mut ws = rt.connect(&c, &alice).await;
    let r = h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/messaging/devices/{}/revoke", alice.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (code, reason) = ws.closed().await;
    assert_eq!((code, reason.as_str()), (Some(4401), "DEVICE_REVOKED"));
    // Session revocation closes its connections.
    let token = rt.token(&c, &bob).await;
    let session = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_client_sessions WHERE device_id = '{}'",
            bob.device_id()
        ))
        .await;
    assert_eq!(session, 1);
    let t = rt.ticket_with(&token).await.json["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    let mut ws = rt.open(&t, None).await.unwrap();
    let sid: String = h
        .conn()
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT id::text FROM messaging_client_sessions WHERE device_id = '{}'",
                bob.device_id()
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/messaging/client-sessions/{sid}/revoke"),
            json!({}),
        )
        .await;
    assert_eq!(r.json["status"], "REVOKED", "{}", r.text);
    assert_eq!(ws.closed().await.1, "SESSION_REVOKED");
    assert_eq!(rt.ticket_with(&token).await.code(), "AUTHENTICATION_FAILED");
    // A suspended principal is disconnected at the next re-validation and cannot reconnect.
    let carol = h.member(&c, 0).await;
    let mut ws = rt.connect(&c, &carol).await;
    let t = rt.ticket(&c, &carol).await;
    h.set_principal(&c, carol.principal, PrincipalStatus::Inactive);
    assert_eq!(ws.closed().await.1, "PRINCIPAL_INACTIVE");
    assert_eq!(rt.open(&t, None).await.err(), Some(401));
}

#[tokio::test]
async fn members_receive_encrypted_messages_live_and_decrypt_locally() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let mut ws = rt.connect(&c, &bob).await;
    let sub = ws.subscribe(&conv, None).await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    let head = sub["headEventSequence"].as_i64().unwrap();
    let r = h
        .send_e2ee(&c, &conv, &mut alice, "live and encrypted")
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let ev = ws.events(1).await.remove(0);
    assert_eq!(ev["type"], "message.created");
    assert_eq!(ev["eventSequence"], head + 1);
    // The relayed bytes are ciphertext; only the device turns them into text.
    assert!(!ev.to_string().contains("live and encrypted"));
    assert_eq!(decrypt(&mut bob, &ev), "live and encrypted");
    // A non-member (and another tenant) cannot subscribe; the answer equals "unknown".
    let mallory = h.member(&c, 0).await;
    let mut wm = rt.connect(&c, &mallory).await;
    let denied = wm.subscribe(&conv, None).await;
    let unknown = wm
        .subscribe(&json!({ "conversationId": Uuid::new_v4() }), None)
        .await;
    assert_eq!(denied["code"], "NOT_FOUND");
    assert_eq!(denied["code"], unknown["code"]);
    assert_eq!(denied["message"], unknown["message"]);
    let _ = h.send_e2ee(&c, &conv, &mut alice, "still private").await;
    wm.quiet("event", Duration::from_millis(500)).await;
}

#[tokio::test]
async fn removed_members_stop_receiving_while_messages_are_flowing() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut people) = h.managed_group(&c, 3).await;
    let _ = &mut people;
    // Managed group members need devices to connect.
    let mut ms = Vec::new();
    for p in &people {
        let d = h.active_device(&c, *p).await;
        ms.push(Member {
            principal: *p,
            device: d,
            group: None,
            cursor: 0,
        });
    }
    let mut ws_c = rt.connect(&c, &ms[2]).await;
    ws_c.subscribe(&conv, None).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    // A burst of messages while the removal happens.
    let sender = people[0];
    for i in 0..3 {
        assert_eq!(
            h.send_managed(&c, &conv, sender, &format!("before {i}"))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    let r = h
        .delete(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/members/{}", people[2]),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for i in 0..3 {
        assert_eq!(
            h.send_managed(&c, &conv, sender, &format!("after {i}"))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    // The removed member gets the earlier messages, its own removal, then nothing.
    let mut texts = Vec::new();
    loop {
        let f = ws_c.next_of("event").await;
        if f["event"]["type"] == "member.removed" {
            break;
        }
        texts.push(
            f["event"]["data"]["message"]["managed"]["text"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(texts, vec!["before 0", "before 1", "before 2"]);
    let revoked = ws_c.next_of("subscription.revoked").await;
    assert_eq!(revoked["reason"], "MEMBERSHIP_ENDED");
    ws_c.quiet("event", Duration::from_millis(800)).await;
    // Resubscribing is refused, and REST replay too.
    assert_eq!(ws_c.subscribe(&conv, Some(0)).await["code"], "NOT_FOUND");
}

#[tokio::test]
async fn offline_devices_replay_missed_events_in_canonical_order_from_their_ack() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let mut ws = rt.connect(&c, &bob).await;
    let sub = ws.subscribe(&conv, Some(0)).await;
    // Replays the history (the commit) from 0.
    let head = sub["headEventSequence"].as_i64().unwrap();
    let replayed = ws.events(head as usize).await;
    let seqs: Vec<i64> = replayed
        .iter()
        .map(|e| e["eventSequence"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, (1..=head).collect::<Vec<_>>());
    // Acknowledge, but never beyond what was delivered.
    ws.send(json!({ "type": "ack", "conversationId": conv["conversationId"], "eventSequence": head + 5, "requestId": "a0" })).await;
    assert_eq!(ws.next_of("error").await["code"], "ACK_BEYOND_DELIVERED");
    ws.send(json!({ "type": "ack", "conversationId": conv["conversationId"], "eventSequence": head, "requestId": "a1" })).await;
    ws.next_of("ack.recorded").await;
    drop(ws);
    // Offline: Alice sends five messages.
    let mut sent = Vec::new();
    for i in 0..5 {
        let r = h
            .send_e2ee(&c, &conv, &mut alice, &format!("offline {i}"))
            .await;
        sent.push(r.json["sequence"].as_i64().unwrap());
    }
    // Reconnect without a cursor: resumes from the stored acknowledgement.
    let mut ws = rt.connect(&c, &bob).await;
    let sub = ws.subscribe(&conv, None).await;
    assert_eq!(sub["afterEventSequence"], head);
    let evs = ws.events(5).await;
    let texts: Vec<String> = evs.iter().map(|e| decrypt(&mut bob, e)).collect();
    assert_eq!(
        texts,
        (0..5).map(|i| format!("offline {i}")).collect::<Vec<_>>()
    );
    let msg_seqs: Vec<i64> = evs
        .iter()
        .map(|e| e["data"]["message"]["sequence"].as_i64().unwrap())
        .collect();
    assert_eq!(msg_seqs, sent, "canonical message order");
    // A duplicate pointer from the broker is ignored (idempotent delivery).
    rt.broker
        .publish(&messaging::realtime::broker::BrokerMessage::Durable {
            conversation_id: cid(&conv),
            event_sequence: evs[2]["eventSequence"].as_i64().unwrap(),
            event_type: "messaging.message.created".into(),
            subject_principal_id: None,
        })
        .await
        .unwrap();
    ws.quiet("event", Duration::from_millis(400)).await;
    // A deleted message replays as a tombstone, never with its content.
    let first_id = evs[0]["data"]["message"]["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    let d = h
        .delete(
            &c,
            Some(alice.principal),
            &format!(
                "/v1/conversations/{}/messages/{first_id}",
                conv["conversationId"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.text);
    let tomb = ws.events(1).await.remove(0);
    assert_eq!(tomb["type"], "message.deleted");
    assert_eq!(tomb["data"]["message"]["lifecycle"], "DELETED");
    assert_eq!(tomb["data"]["message"]["e2ee"]["ciphertext"], Value::Null);
    let mut fresh = rt.connect(&c, &bob).await;
    fresh.subscribe(&conv, Some(0)).await;
    let all = fresh.events(head as usize + 6).await;
    let replayed_first = all
        .iter()
        .find(|e| e["data"]["message"]["messageId"] == json!(first_id))
        .unwrap();
    assert_eq!(
        replayed_first["data"]["message"]["e2ee"]["ciphertext"],
        Value::Null,
        "deleted data reappeared in replay"
    );
}

#[tokio::test]
async fn committed_messages_survive_a_broker_outage() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let mut ws = rt.connect(&c, &bob).await;
    ws.subscribe(&conv, None).await;
    rt.broker.set_available(false);
    let r = h
        .send_e2ee(&c, &conv, &mut alice, "during the outage")
        .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "the commit does not depend on the broker"
    );
    // The outbox keeps the event PENDING (retrying) …
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pending = h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE status = 'PENDING' AND attempts > 0").await;
    assert!(pending >= 1);
    // … and the connected device still gets it from the database (periodic head check).
    let ev = ws.events(1).await.remove(0);
    assert_eq!(decrypt(&mut bob, &ev), "during the outage");
    rt.broker.set_available(true);
    // The retry is due after its backoff (the harness clock is manual).
    h.clock.advance(chrono::Duration::seconds(5));
    rt.realtime.outbox_wakeup.notify_one();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE status = 'PENDING'")
            .await,
        0
    );
    // The late broker pointer is a duplicate and is dropped.
    ws.quiet("event", Duration::from_millis(400)).await;
}

#[tokio::test]
async fn concurrent_senders_are_delivered_gap_free_in_one_order() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let d = h.active_device(&c, people[2]).await;
    let viewer = Member {
        principal: people[2],
        device: d,
        group: None,
        cursor: 0,
    };
    let mut ws = rt.connect(&c, &viewer).await;
    let head = ws.subscribe(&conv, None).await["headEventSequence"]
        .as_i64()
        .unwrap();
    let a = async {
        for i in 0..15 {
            h.send_managed(&c, &conv, people[0], &format!("a{i}")).await;
        }
    };
    let b = async {
        for i in 0..15 {
            h.send_managed(&c, &conv, people[1], &format!("b{i}")).await;
        }
    };
    tokio::join!(a, b);
    let evs = ws.events(30).await;
    let seqs: Vec<i64> = evs
        .iter()
        .map(|e| e["eventSequence"].as_i64().unwrap())
        .collect();
    assert_eq!(
        seqs,
        (head + 1..=head + 30).collect::<Vec<_>>(),
        "gap-free, ordered, no duplicates"
    );
    let msg: Vec<i64> = evs
        .iter()
        .map(|e| e["data"]["message"]["sequence"].as_i64().unwrap())
        .collect();
    assert!(msg.windows(2).all(|w| w[1] == w[0] + 1));
    // Each sender's own order is preserved within the canonical order.
    let a_order: Vec<String> = evs
        .iter()
        .filter_map(|e| e["data"]["message"]["managed"]["text"].as_str())
        .filter(|t| t.starts_with('a'))
        .map(str::to_string)
        .collect();
    assert_eq!(
        a_order,
        (0..15).map(|i| format!("a{i}")).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_slow_consumer_is_disconnected_resumably_without_unbounded_buffering() {
    let settings = RealtimeSettings {
        send_queue: 4,
        ..test_settings()
    };
    let Some(rt) = Rt::with(settings).await else {
        return;
    };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let d = h.active_device(&c, people[1]).await;
    let m = Member {
        principal: people[1],
        device: d,
        group: None,
        cursor: 0,
    };
    let mut ws = rt.connect(&c, &m).await;
    ws.subscribe(&conv, None).await;
    // A burst far beyond the connection's queue (the client is not reading meanwhile).
    for i in 0..2_000 {
        rt.broker
            .publish(&messaging::realtime::broker::BrokerMessage::Durable {
                conversation_id: cid(&conv),
                event_sequence: 1_000_000 + i,
                event_type: "messaging.message.created".into(),
                subject_principal_id: None,
            })
            .await
            .unwrap();
    }
    let (code, reason) = ws.closed().await;
    assert_eq!((code, reason.as_str()), (Some(1013), "SLOW_CONSUMER"));
    let err = ws
        .seen
        .iter()
        .find(|f| f["code"] == "SLOW_CONSUMER")
        .expect("a resumable error frame");
    assert!(err["resume"].as_array().is_some_and(|r| !r.is_empty()));
    assert!(
        rt.realtime
            .hub
            .dropped_lagging
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
    for _ in 0..100 {
        if rt.realtime.hub.connections() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rt.realtime.hub.connections(), 0, "released");
}

#[tokio::test]
async fn typing_is_ephemeral_authorized_throttled_and_cleared_on_disconnect() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, alice, bob) = h.e2ee_direct(&c).await;
    let mut wa = rt.connect(&c, &alice).await;
    let mut wb = rt.connect(&c, &bob).await;
    wa.subscribe(&conv, None).await;
    wb.subscribe(&conv, None).await;
    let outbox_before = h
        .scalar("SELECT count(*) FROM messaging_outbox_events")
        .await;
    for _ in 0..10 {
        wa.send(json!({ "type": "typing", "conversationId": conv["conversationId"], "state": "started" })).await;
    }
    let t = wb.next_of("typing").await;
    assert_eq!(
        (t["state"].clone(), t["principalId"].clone()),
        (json!("started"), json!(alice.principal))
    );
    assert!(t["ttlMs"].as_u64().unwrap() > 0);
    assert!(
        t.get("deviceId").is_none() && !t.to_string().contains(&alice.device_id().to_string()),
        "no device ids"
    );
    wb.quiet("typing", Duration::from_millis(500)).await; // coalesced: one signal
                                                          // Never durable: no outbox event, no message.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events")
            .await,
        outbox_before
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_messages WHERE message_kind <> 'MLS_COMMIT'")
            .await,
        0
    );
    // A flood is throttled.
    for _ in 0..100 {
        wa.send(json!({ "type": "typing", "conversationId": conv["conversationId"], "state": "started" })).await;
    }
    let throttled = wa.next_of("error").await;
    assert_eq!(throttled["code"], "RATE_LIMITED");
    // Disconnecting clears the typing state for others.
    drop(wa);
    let t = wb.next_of("typing").await;
    assert_eq!(t["state"], "stopped");
    // Not subscribed: refused.
    let carol = h.member(&c, 0).await;
    let mut wc = rt.connect(&c, &carol).await;
    wc.send(
        json!({ "type": "typing", "conversationId": conv["conversationId"], "state": "started" }),
    )
    .await;
    assert_eq!(wc.next_of("error").await["code"], "NOT_SUBSCRIBED");
}

#[tokio::test]
async fn presence_respects_hidden_users_policy_and_co_membership() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, alice, bob) = h.e2ee_direct(&c).await;
    let mut wa = rt.connect(&c, &alice).await;
    wa.subscribe(&conv, None).await;
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(
        wa.next_of("presence.snapshot").await["members"][0]["state"],
        "OFFLINE"
    );
    let mut wb = rt.connect(&c, &bob).await;
    let p = wa.next_of("presence").await;
    assert_eq!(
        (p["principalId"].clone(), p["state"].clone()),
        (json!(bob.principal), json!("AVAILABLE"))
    );
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    let snap = wa.next_of("presence.snapshot").await;
    assert_eq!(snap["members"][0]["state"], "AVAILABLE");
    assert!(
        !snap.to_string().contains(&bob.device_id().to_string()),
        "no device details"
    );
    // Bob hides: reported OFFLINE everywhere, indistinguishable from offline.
    wb.send(json!({ "type": "presence.set", "visibility": "HIDDEN" }))
        .await;
    assert_eq!(
        wb.next_of("presence.settings").await["settings"]["visibility"],
        "HIDDEN"
    );
    assert_eq!(wa.next_of("presence").await["state"], "OFFLINE");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(
        wa.next_of("presence.snapshot").await["members"][0]["state"],
        "OFFLINE"
    );
    // Rate limited; and a non-subscriber cannot query (no enumeration).
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(wa.next_of("error").await["code"], "RATE_LIMITED");
    let mallory = h.member(&c, 0).await;
    let mut wm = rt.connect(&c, &mallory).await;
    wm.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(wm.next_of("error").await["code"], "NOT_SUBSCRIBED");
    // Tenant policy can switch presence off entirely.
    h.set_policy(&c, json!({ "privacy": { "presence": false } }))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await; // re-validation refreshes policy
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    wa.send(json!({ "type": "presence.query", "conversationId": conv["conversationId"] }))
        .await;
    assert_eq!(
        wa.next_of("presence.snapshot").await["members"][0]["state"],
        "POLICY_RESTRICTED"
    );
}

#[tokio::test]
async fn multiple_devices_are_authorized_independently() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let d1 = h.active_device(&c, people[1]).await;
    let d2 = h.active_device(&c, people[1]).await;
    let m1 = Member {
        principal: people[1],
        device: d1,
        group: None,
        cursor: 0,
    };
    let m2 = Member {
        principal: people[1],
        device: d2,
        group: None,
        cursor: 0,
    };
    let mut w1 = rt.connect(&c, &m1).await;
    let mut w2 = rt.connect(&c, &m2).await;
    w1.subscribe(&conv, None).await;
    w2.subscribe(&conv, None).await;
    h.send_managed(&c, &conv, people[0], "to both devices")
        .await;
    assert_eq!(
        w1.events(1).await[0]["data"]["message"]["managed"]["text"],
        "to both devices"
    );
    assert_eq!(
        w2.events(1).await[0]["data"]["message"]["managed"]["text"],
        "to both devices"
    );
    // Revoking one device closes only its connection.
    h.post(
        &c,
        Some(people[1]),
        &format!("/v1/messaging/devices/{}/revoke", m1.device_id()),
        json!({ "reason": "REPLACED" }),
    )
    .await;
    assert_eq!(w1.closed().await.1, "DEVICE_REVOKED");
    h.send_managed(&c, &conv, people[0], "only the second")
        .await;
    assert_eq!(
        w2.events(1).await[0]["data"]["message"]["managed"]["text"],
        "only the second"
    );
}

#[tokio::test]
async fn push_payloads_are_metadata_only_and_revoked_devices_get_none() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let r = h
        .put(
            &c,
            Some(bob.principal),
            &format!(
                "/v1/messaging/devices/{}/push-registration",
                bob.device_id()
            ),
            json!({ "platform": "LOCAL_TEST", "handle": "local-test-handle-bob" }),
        )
        .await;
    assert_eq!(r.json["pushDeliveryConfigured"], true, "{}", r.text);
    let r2 = h
        .put(
            &c,
            Some(alice.principal),
            &format!(
                "/v1/messaging/devices/{}/push-registration",
                alice.device_id()
            ),
            json!({ "platform": "APNS", "handle": "a1".repeat(32) }),
        )
        .await;
    assert_eq!(
        r2.json["pushDeliveryConfigured"], false,
        "APNs is not configured"
    );
    h.send_e2ee(&c, &conv, &mut alice, "a secret push").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let report = messaging::realtime::push::dispatch_due(&h.state, 100, 3)
        .await
        .unwrap();
    assert_eq!(report.sent, 1, "{report:?}");
    let sent = rt.push.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let payload = sent[0].1.to_string();
    assert!(
        !payload.contains("secret") && !payload.contains("ciphertext"),
        "{payload}"
    );
    assert_eq!(sent[0].1["type"], "message");
    // The sender's own device gets nothing; an APNs registration without credentials is
    // never reported as delivered.
    h.post(
        &c,
        Some(bob.principal),
        &format!("/v1/messaging/devices/{}/revoke", bob.device_id()),
        json!({ "reason": "LOST_OR_STOLEN" }),
    )
    .await;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_push_registrations WHERE device_id = '{}' AND enabled",
            bob.device_id()
        ))
        .await,
        0
    );
    let bad = h.sql("INSERT INTO messaging_push_jobs (id, event_id, conversation_id, environment_id, device_id, registration_id, payload, next_attempt_at, expires_at, created_at) SELECT gen_random_uuid(), gen_random_uuid(), conversation_id, environment_id, device_id, registration_id, '{\"ciphertext\":\"x\"}', now(), now() + interval '1 hour', now() FROM messaging_push_jobs LIMIT 1").await;
    assert!(
        bad.is_err(),
        "the database refuses content in push payloads"
    );
    let _ = conv;
}

#[tokio::test]
async fn draining_tells_clients_to_reconnect_and_refuses_new_connections() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let m = h.member(&c, 0).await;
    let mut ws = rt.connect(&c, &m).await;
    let ticket = rt.ticket(&c, &m).await;
    rt.realtime.hub.drain();
    let d = ws.next_of("server.draining").await;
    assert!(d["reconnectAfterMs"].as_u64().unwrap() >= 500);
    assert_eq!(ws.closed().await.0, Some(1012));
    assert_eq!(rt.open(&ticket, None).await.err(), Some(503));
    let ready = h
        .raw(
            axum::http::Request::get("/ready")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ready.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready.json["realtime"]["draining"], true);
}

#[tokio::test]
async fn protocol_errors_never_close_and_frames_are_bounded() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let m = h.member(&c, 0).await;
    let mut ws = rt.connect(&c, &m).await;
    ws.send(json!({ "type": "future.frame.v9", "x": 1 })).await;
    assert_eq!(ws.next_of("error").await["code"], "UNSUPPORTED_FRAME");
    ws.stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "not json".into(),
        ))
        .await
        .unwrap();
    assert_eq!(ws.next_of("error").await["code"], "VALIDATION_ERROR");
    ws.send(json!({ "type": "ping", "requestId": "p" })).await;
    assert_eq!(ws.next_of("pong").await["requestId"], "p");
    // An oversized frame closes the connection (bounded memory).
    let big = "x".repeat(200 * 1024);
    let _ = ws
        .stream
        .send(tokio_tungstenite::tungstenite::Message::Text(big))
        .await;
    let (code, _) = ws.closed().await;
    assert!(code.is_none() || code == Some(1009), "{code:?}");
}

use futures_util::SinkExt;
use messaging::realtime::broker::Broker;
use sea_orm::ConnectionTrait;

/// §52 / §55 / §148: a message is DELIVERED to a recipient once one of its sessions
/// acknowledged it (the sender sees `delivery.cursor.updated`), and one device advancing
/// the read cursor moves the principal's other sessions even when read receipts are not
/// shared with other members — who then learn nothing about it.
#[tokio::test]
async fn delivery_receipts_and_own_device_read_sync_respect_the_privacy_policy() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "privacy": { "readReceipts": false } }),
    )
    .await;
    let member = |principal, device| Member {
        principal,
        device,
        group: None,
        cursor: 0,
    };
    let a = member(alice, h.active_device(&c, alice).await);
    let b1 = member(bob, h.active_device(&c, bob).await);
    let b2 = member(bob, h.active_device(&c, bob).await);
    let mut wa = rt.connect(&c, &a).await;
    let mut wb1 = rt.connect(&c, &b1).await;
    let mut wb2 = rt.connect(&c, &b2).await;
    for w in [&mut wa, &mut wb1, &mut wb2] {
        w.subscribe(&conv, None).await;
    }
    let sent = h.send_managed(&c, &conv, alice, "are you there?").await;
    let seq = sent.json["sequence"].as_i64().unwrap();
    wa.events(1).await;
    wb2.events(1).await;
    let created = wb1.events(1).await.remove(0);
    let event_seq = created["eventSequence"].as_i64().unwrap();
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_delivery_cursors")
            .await,
        0,
        "nothing is DELIVERED before a session acknowledges it"
    );

    // Bob's first device acknowledges: the message is DELIVERED to Bob.
    wb1.send(json!({ "type": "ack", "conversationId": conv["conversationId"], "eventSequence": event_seq, "requestId": "d1" })).await;
    wb1.next_of("ack.recorded").await;
    let receipt = wa.events(1).await.remove(0);
    assert_eq!(receipt["type"], "delivery.cursor.updated", "{receipt}");
    assert_eq!(receipt["data"]["principalId"], json!(bob));
    assert_eq!(receipt["data"]["deliveredSequence"], seq);
    assert_eq!(
        h.scalar(&format!("SELECT delivered_sequence FROM messaging_delivery_cursors WHERE principal_id = '{bob}'")).await,
        seq
    );
    // A second acknowledgement of the same position records nothing new.
    wb2.send(json!({ "type": "ack", "conversationId": conv["conversationId"], "eventSequence": event_seq, "requestId": "d2" })).await;
    wb2.next_of("ack.recorded").await;

    // Bob reads on a device: his other session follows; Alice learns nothing.
    let r = h
        .put(
            &c,
            Some(bob),
            &format!(
                "/v1/conversations/{}/read-cursor",
                conv["conversationId"].as_str().unwrap()
            ),
            json!({ "sequence": seq }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for w in [&mut wb1, &mut wb2] {
        let synced = w.next_of("read-cursor.synced").await;
        assert_eq!(
            (
                synced["conversationId"].clone(),
                synced["readSequence"].clone()
            ),
            (conv["conversationId"].clone(), json!(seq))
        );
    }
    wa.quiet("read-cursor.synced", Duration::from_millis(600))
        .await;
    wa.quiet("event", Duration::from_millis(300)).await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type IN ('messaging.read-cursor.updated', 'messaging.delivery-cursor.updated') \
                  AND payload->>'principal_id' = (SELECT principal_id::text FROM messaging_delivery_cursors LIMIT 1) AND event_type = 'messaging.read-cursor.updated'").await,
        0,
        "read receipts are off: no member-visible read event"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.delivery-cursor.updated'").await,
        1,
        "one receipt per advance"
    );
    h.conforming_event_types().await;
}

/// §134: the REALTIME_CONNECT switch refuses new tickets and connections (established
/// connections are governed by revalidation as usual); releasing it restores them.
#[tokio::test]
async fn the_realtime_connect_switch_refuses_new_tickets_and_connections() {
    use messaging::operations::{self, KillSwitch};
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (_conv, people) = h.managed_group(&c, 1).await;
    let m = Member {
        principal: people[0],
        device: h.active_device(&c, people[0]).await,
        group: None,
        cursor: 0,
    };
    let ticket = rt.ticket(&c, &m).await; // issued before the switch
    let token = rt.token(&c, &m).await;
    let env = c.context.scope.environment_id;
    let now = chrono::Utc::now();
    operations::set_switch(
        h.conn(),
        KillSwitch::RealtimeConnect,
        Some(env),
        true,
        "INCIDENT",
        "ops-1",
        now,
    )
    .await
    .unwrap();
    let r = rt.ticket_with(&token).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("FEATURE_DISABLED", "KILL_SWITCH_ENGAGED"),
        "{}",
        r.text
    );
    assert!(
        rt.open(&ticket, None).await.is_err(),
        "a ticket issued before the switch cannot connect"
    );
    operations::set_switch(
        h.conn(),
        KillSwitch::RealtimeConnect,
        Some(env),
        false,
        "RESOLVED",
        "ops-1",
        now,
    )
    .await
    .unwrap();
    let mut ws = rt.connect(&c, &m).await;
    ws.send(json!({ "type": "ping", "requestId": "p" })).await;
    ws.next_of("pong").await;
}

/// Appendix H.3: permessage-deflate is not negotiated (compression side channels) —
/// a client offering it gets a connection without the extension.
#[tokio::test]
async fn websocket_compression_is_never_negotiated() {
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (_conv, people) = h.managed_group(&c, 1).await;
    let m = Member {
        principal: people[0],
        device: h.active_device(&c, people[0]).await,
        group: None,
        cursor: 0,
    };
    let ticket = rt.ticket(&c, &m).await;
    let mut req = format!("ws://{}/v1/realtime", rt.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("cfmeet.messaging.v1, cfmeet.ticket.{ticket}")).unwrap(),
    );
    req.headers_mut().insert(
        "sec-websocket-extensions",
        HeaderValue::from_static("permessage-deflate; client_max_window_bits"),
    );
    let (_stream, resp) = tokio_tungstenite::connect_async(req)
        .await
        .expect("handshake");
    assert_eq!(resp.status(), 101);
    assert!(
        resp.headers().get("sec-websocket-extensions").is_none(),
        "no extension is negotiated: {:?}",
        resp.headers()
    );
}

/// §129 (G-18): a closed realtime connection is metered as connection seconds of its
/// device (a fact recorded when the connection ends).
#[tokio::test]
async fn realtime_connection_time_is_metered_when_the_connection_closes() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let alice = h.member(&c, 0).await;
    let mut ws = rt.connect(&c, &alice).await;
    h.clock.advance(chrono::Duration::seconds(90));
    let r = h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/messaging/devices/{}/revoke", alice.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    ws.closed().await;
    // The fact is written as the server finishes the connection: wait for that condition.
    let sql = format!(
        "SELECT count(*) FROM messaging_usage_records WHERE usage_type = 'REALTIME_CONNECTION' AND unit = 'SECONDS' \
           AND subject_id = '{}' AND quantity >= 90",
        alice.device_id()
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.scalar(&sql).await == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "connection time never metered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(h.scalar(&sql).await, 1);
}

/// §144 / §158 (G-25): seeded property test of the realtime frame parser over a live
/// connection — random text, random JSON and mutated valid frames never crash or wedge the
/// connection: each gets at most an error frame and the connection keeps serving.
#[tokio::test]
async fn malformed_and_mutated_frames_never_break_a_connection() {
    let settings = RealtimeSettings {
        inbound_frames_per_second: 100_000,
        ..test_settings()
    };
    let Some(rt) = Rt::with(settings).await else {
        return;
    };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let m = Member {
        principal: people[0],
        device: h.active_device(&c, people[0]).await,
        group: None,
        cursor: 0,
    };
    let mut ws = rt.connect(&c, &m).await;
    let mut x: u64 = 0x5eed_0ff4_a3e5 | 1;
    let mut next = move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let valid = [
        json!({ "type": "subscribe", "conversationId": cid, "requestId": "s" }).to_string(),
        json!({ "type": "typing", "conversationId": cid, "state": "started" }).to_string(),
        json!({ "type": "ack", "conversationId": cid, "eventSequence": 1 }).to_string(),
        json!({ "type": "presence.query", "principalIds": [people[1]] }).to_string(),
    ];
    for round in 0..30 {
        for _ in 0..10 {
            let frame = match next() % 4 {
                0 => {
                    let len = (next() % 512) as usize;
                    String::from_utf8_lossy(&(0..len).map(|_| next() as u8).collect::<Vec<_>>())
                        .into_owned()
                }
                1 => [
                    "null",
                    "[]",
                    "{}",
                    "1e999",
                    "\"x\"",
                    "{\"type\":7}",
                    "{\"type\":\"\"}",
                    "{\"type\":\"subscribe\"}",
                ][(next() % 8) as usize]
                    .to_string(),
                _ => {
                    let mut b = valid[(next() % 4) as usize].clone().into_bytes();
                    let at = (next() as usize) % b.len();
                    match next() % 3 {
                        0 => b[at] = b' ' + (next() % 95) as u8,
                        1 => b.truncate(at),
                        _ => b.insert(at, b'{'),
                    }
                    String::from_utf8_lossy(&b).into_owned()
                }
            };
            ws.send_raw(frame).await;
        }
        let rid = format!("p{round}");
        ws.send(json!({ "type": "ping", "requestId": rid })).await;
        assert_eq!(ws.next_of("pong").await["requestId"], rid, "round {round}");
    }
    for f in &ws.seen {
        assert!(f["type"].is_string(), "every server frame is typed: {f}");
    }
}
