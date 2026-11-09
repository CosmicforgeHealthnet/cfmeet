//! Audit closure MSG-018 (server-side receipt-event coalescing) and MSG-009 (realtime
//! nudge for Communications Hub timeline cards), over a REAL TCP listener with REAL
//! WebSocket clients, the outbox publisher, the receipt coalescer and PostgreSQL.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPrincipalKind;
use common::{flows::*, realtime::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

/// The next durable event of type `t` (other events are skipped).
async fn event_of(ws: &mut Ws, t: &str) -> Value {
    loop {
        let e = ws.events(1).await.remove(0);
        if e["type"] == t {
            return e;
        }
    }
}

/// Asserts no durable event of type `t` arrives within `wait` (other frames are kept).
async fn no_event_of(ws: &mut Ws, t: &str, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match ws.recv(left).await {
            Observed::Frame(f) if f["type"] == "event" && f["event"]["type"] == t => {
                panic!("unexpected {t}: {f}")
            }
            Observed::Frame(f) => ws.seen.push(f),
            Observed::Timeout => return,
            Observed::Closed(c, r) => panic!("closed: {c:?} {r}"),
        }
    }
}

async fn count(h: &Harness, sql: &str) -> i64 {
    h.scalar(sql).await
}

fn member(principal: Uuid, device: cf_messaging_crypto::device::Device) -> Member {
    Member {
        principal,
        device,
        group: None,
        cursor: 0,
    }
}

async fn read(h: &Harness, c: &Customer, conv: &Value, who: Uuid, seq: i64) -> i64 {
    let r = h
        .put(
            c,
            Some(who),
            &format!("/v1/conversations/{}/read-cursor", cid(conv)),
            json!({ "sequence": seq }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["readSequence"].as_i64().unwrap()
}

/// MSG-018: a burst of reads publishes ONE receipt event at once (the leading edge) and
/// ONE more with the highest cursor when the window ends (the trailing edge), instead of
/// one conversation-sequenced event per read. Published cursors only grow; a repeated
/// cursor publishes nothing; the canonical read cursor is exact at every step; a member
/// who stops sharing inside a window publishes nothing more; and `receipts.query` (what a
/// reconnecting client sends) returns the CURRENT cursors, including a pending one, with
/// the same privacy filtering. Delivery receipts (acknowledgements) coalesce the same way.
#[tokio::test]
async fn receipt_events_are_coalesced_per_member_and_window_and_snapshots_read_current_cursors() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (alice, bob, carol) = (people[0], people[1], people[2]);
    let a = member(alice, h.active_device(&c, alice).await);
    let b = member(bob, h.active_device(&c, bob).await);
    let mut wa = rt.connect(&c, &a).await;
    let mut wb = rt.connect(&c, &b).await;
    wa.subscribe(&conv, None).await;
    wb.subscribe(&conv, None).await;
    let mut seqs = Vec::new();
    for i in 0..6 {
        let sent = h.send_managed(&c, &conv, alice, &format!("m{i}")).await;
        seqs.push(sent.json["sequence"].as_i64().unwrap());
    }
    let read_events = |principal: Uuid| {
        format!("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.read-cursor.updated' AND payload->>'principal_id' = '{principal}'")
    };

    // Bob reads through the burst: one event at once, nothing more inside the window.
    for (i, s) in seqs.iter().enumerate().take(5) {
        assert_eq!(read(h, &c, &conv, bob, *s).await, *s, "read {i}");
    }
    assert_eq!(count(h, &read_events(bob)).await, 1);
    let first = event_of(&mut wa, "read.cursor.updated").await;
    assert_eq!(
        (
            &first["data"]["principalId"],
            &first["data"]["readSequence"]
        ),
        (&json!(bob), &json!(seqs[0]))
    );
    // The current state is readable at once (the pending cursor is not hidden).
    wa.send(json!({ "type": "receipts.query", "conversationId": cid(&conv), "requestId": "r1" }))
        .await;
    let snap = wa.next_of("receipts.snapshot").await;
    assert_eq!(snap["truncated"], false);
    let bob_row = snap["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["principalId"] == json!(bob))
        .cloned()
        .unwrap();
    assert_eq!(bob_row["readSequence"], seqs[4]);
    assert!(
        snap["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["principalId"] != json!(alice)),
        "the asker is not listed"
    );
    // Rate limited per conversation (a reconnect storm cannot hammer it).
    wa.send(json!({ "type": "receipts.query", "conversationId": cid(&conv), "requestId": "r2" }))
        .await;
    let limited = wa.next_of("error").await;
    assert_eq!(limited["code"], "RATE_LIMITED", "{limited}");

    // The window ends: exactly one more event, with the HIGHEST cursor.
    no_event_of(&mut wa, "read.cursor.updated", Duration::from_millis(400)).await;
    h.clock.advance(chrono::Duration::seconds(2));
    let trailing = event_of(&mut wa, "read.cursor.updated").await;
    assert_eq!(trailing["data"]["readSequence"], seqs[4]);
    assert!(trailing["eventSequence"].as_i64() > first["eventSequence"].as_i64());
    assert_eq!(count(h, &read_events(bob)).await, 2);
    // Repeating a published cursor publishes nothing, even in a new window.
    h.clock.advance(chrono::Duration::seconds(5));
    read(h, &c, &conv, bob, seqs[4]).await;
    // An older cursor never moves anything back.
    assert_eq!(read(h, &c, &conv, bob, seqs[1]).await, seqs[4]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(h, &read_events(bob)).await, 2);
    assert_eq!(
        count(h, &format!("SELECT emitted_sequence FROM messaging_receipt_emissions WHERE principal_id = '{bob}' AND receipt_kind = 'READ'")).await,
        seqs[4]
    );
    // A database guard keeps the published cursor monotonic.
    assert!(h
        .sql(&format!("UPDATE messaging_receipt_emissions SET emitted_sequence = 1 WHERE principal_id = '{bob}' AND receipt_kind = 'READ'"))
        .await
        .is_err());

    // A new window opens with the next advance (leading edge again) ...
    read(h, &c, &conv, bob, seqs[5]).await;
    let lead = event_of(&mut wa, "read.cursor.updated").await;
    assert_eq!(lead["data"]["readSequence"], seqs[5]);

    // ... and a member who stops sharing inside a window publishes nothing more; the
    // snapshot stops showing its read position too.
    let more = h.send_managed(&c, &conv, alice, "m6").await;
    let last = more.json["sequence"].as_i64().unwrap();
    read(h, &c, &conv, carol, last).await;
    let carol_lead = event_of(&mut wa, "read.cursor.updated").await;
    assert_eq!(carol_lead["data"]["principalId"], json!(carol));
    read(h, &c, &conv, bob, last).await; // pending for bob (inside his window)
    let s = h
        .put(
            &c,
            Some(bob),
            "/v1/messaging/presence/settings",
            json!({ "visibility": "CONVERSATION_MEMBERS", "shareReadReceipts": false }),
        )
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text);
    h.clock.advance(chrono::Duration::seconds(2));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count(
            h,
            "SELECT count(*) FROM messaging_receipt_emissions WHERE due_at IS NOT NULL"
        )
        .await,
        0,
        "the window was closed"
    );
    assert_eq!(
        count(h, &read_events(bob)).await,
        3,
        "withheld: bob stopped sharing"
    );
    assert_eq!(
        count(
            h,
            &format!(
                "SELECT read_sequence FROM messaging_read_cursors WHERE principal_id = '{bob}'"
            )
        )
        .await,
        last,
        "the canonical cursor is exact regardless"
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    wa.send(json!({ "type": "receipts.query", "conversationId": cid(&conv), "requestId": "r3" }))
        .await;
    let snap = wa.next_of("receipts.snapshot").await;
    for m in snap["members"].as_array().unwrap() {
        if m["principalId"] == json!(bob) {
            assert!(m.get("readSequence").is_none(), "{m}");
        }
        if m["principalId"] == json!(carol) {
            assert_eq!(m["readSequence"], last);
        }
    }

    // Delivery receipts: Bob's device acknowledges event after event; Alice sees one
    // delivery receipt at once and one with the highest position after the window.
    let delivered_events = format!("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.delivery-cursor.updated' AND payload->>'principal_id' = '{bob}'");
    let before = count(h, &delivered_events).await;
    let mut acked = Vec::new();
    for i in 0..4 {
        let sent = h.send_managed(&c, &conv, alice, &format!("d{i}")).await;
        let seq = sent.json["sequence"].as_i64().unwrap();
        let ev = loop {
            let e = event_of(&mut wb, "message.created").await;
            if e["data"]["message"]["sequence"] == json!(seq) {
                break e;
            }
        };
        wb.send(json!({ "type": "ack", "conversationId": cid(&conv), "eventSequence": ev["eventSequence"], "requestId": format!("a{i}") }))
            .await;
        wb.next_of("ack.recorded").await;
        acked.push(seq);
    }
    // (Reading past messages also delivers them, so the first window may already be open.)
    h.clock.advance(chrono::Duration::seconds(2));
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = count(h, &delivered_events).await;
    assert!(
        after - before <= 2,
        "at most a leading and a trailing receipt per window, got {}",
        after - before
    );
    assert_eq!(
        count(h, &format!("SELECT (payload->>'delivered_sequence')::bigint FROM messaging_outbox_events WHERE event_type = 'messaging.delivery-cursor.updated' AND payload->>'principal_id' = '{bob}' ORDER BY sequence DESC LIMIT 1")).await,
        *acked.last().unwrap(),
        "the last published delivery receipt carries the highest position"
    );

    // Not subscribed: no receipts.
    let outsider_conv = h
        .create_conversation(
            &c,
            carol,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    wa.send(json!({ "type": "receipts.query", "conversationId": outsider_conv.json["conversationId"], "requestId": "r4" }))
        .await;
    let refused = wa.next_of("error").await;
    assert_eq!(refused["code"], "NOT_SUBSCRIBED", "{refused}");
    h.conforming_event_types().await;
}

/// The personal scope of `c` becomes the one Communications governs.
fn govern(h: &mut Harness, c: &Customer) {
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
}

async fn hub_event(h: &Harness, c: &Customer, actor: Uuid, body: Value) -> Response {
    let ctx = c
        .context
        .scope
        .delegated()
        .with_initiating_principal(actor, DelegatedPrincipalKind::NativeCfUser);
    h.call_internal(
        &peers().communications,
        Method::POST,
        "/internal/v1/communications/events",
        Some(body),
        Some(ctx),
        &[],
    )
    .await
}

fn nudge(conversation: Uuid, version: i64, reason: &str) -> Value {
    json!({ "schemaVersion": 1, "eventId": Uuid::new_v4(), "aggregateId": conversation, "version": version,
            "type": "communications.timeline.changed",
            "data": { "conversationId": conversation, "timelineVersion": version, "reason": reason } })
}

/// MSG-009: the Hub's durable `communications.timeline.changed` becomes ONE
/// `timeline.changed` event in the conversation's own stream per higher timeline version:
/// delivered live to the members' subscriptions (never to anyone else), replayable by a
/// member who was offline or joined later (inside its history window), and never
/// duplicated or regressed by a redelivered, repeated or out-of-order Hub event. A
/// conversation outside the delegated scope is acknowledged without effect; a malformed
/// nudge is refused.
#[tokio::test]
async fn hub_timeline_nudges_reach_members_once_per_version() {
    let Some(mut rt) = Rt::start().await else {
        return;
    };
    let c = rt.h.first_party(true);
    let (conv, people) = rt.h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let other = rt.h.first_party(true);
    let (foreign, _) = rt.h.managed_group(&other, 1).await;
    let outsider = rt.h.principal(&c);
    let own =
        rt.h.create_conversation(
            &c,
            outsider,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(own.status, StatusCode::CREATED, "{}", own.text);
    let a = member(alice, rt.h.active_device(&c, alice).await);
    let b = member(bob, rt.h.active_device(&c, bob).await);
    let o = member(outsider, rt.h.active_device(&c, outsider).await);
    let mut wa = rt.connect(&c, &a).await;
    let mut wb = rt.connect(&c, &b).await;
    let mut wo = rt.connect(&c, &o).await;
    wa.subscribe(&conv, None).await;
    wb.subscribe(&conv, None).await;
    wo.subscribe(&own.json, None).await;
    // The outsider cannot subscribe to the conversation it is not a member of.
    wo.send(json!({ "type": "subscribe", "conversationId": cid(&conv), "requestId": "x" }))
        .await;
    assert_eq!(wo.next_of("error").await["code"], "NOT_FOUND");
    govern(&mut rt.h, &c);
    // The Communications integration's own (separately versioned) history: the event
    // inbox. Deployments with personal communications apply it before starting.
    use messaging_migration::MigratorTrait;
    messaging_migration::CommunicationsMigrator::up(rt.h.conn(), None)
        .await
        .unwrap();
    let h = &rt.h;
    let conv_id = cid(&conv);
    let announced = format!("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.timeline.changed' AND conversation_id = '{conv_id}'");

    let first = nudge(conv_id, 1, "CALL_STARTED");
    let r = hub_event(h, &c, alice, first.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json, json!({ "applied": true, "duplicate": false }));
    for w in [&mut wa, &mut wb] {
        let e = event_of(w, "timeline.changed").await;
        assert_eq!(e["conversationId"], json!(conv_id));
        assert_eq!(
            e["data"],
            json!({ "timelineVersion": 1, "reason": "CALL_STARTED" })
        );
    }
    no_event_of(&mut wo, "timeline.changed", Duration::from_millis(300)).await;

    // Redelivery of the same Hub event, a repeated version, an older version: nothing new.
    let again = hub_event(h, &c, alice, first).await;
    assert_eq!(again.json["duplicate"], true, "{}", again.text);
    for stale in [
        nudge(conv_id, 1, "CALL_UPDATED"),
        nudge(conv_id, 0, "CALL_UPDATED"),
    ] {
        let r = hub_event(h, &c, alice, stale).await;
        if r.status == StatusCode::OK {
            assert_eq!(r.json["applied"], true, "{}", r.text);
        } else {
            assert_eq!(r.code(), "VALIDATION_ERROR", "version 0 is malformed");
        }
    }
    assert_eq!(count(h, &announced).await, 1);
    // The next versions arrive (even out of order: 3 before 2 announces 3 only).
    for (v, reason) in [(3, "MEETING_SHARED"), (2, "CALL_UPDATED")] {
        let r = hub_event(h, &c, alice, nudge(conv_id, v, reason)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    let e = event_of(&mut wb, "timeline.changed").await;
    assert_eq!(e["data"]["timelineVersion"], 3);
    no_event_of(&mut wb, "timeline.changed", Duration::from_millis(300)).await;
    assert_eq!(count(h, &announced).await, 2);

    // Malformed nudges are refused; another scope's conversation changes nothing.
    let mut wrong = nudge(conv_id, 4, "CALL_UPDATED");
    wrong["aggregateId"] = json!(Uuid::new_v4());
    assert_eq!(
        hub_event(h, &c, alice, wrong).await.code(),
        "VALIDATION_ERROR"
    );
    let mut wrong = nudge(conv_id, 4, "CALL_UPDATED");
    wrong["data"]["reason"] = json!("SOMETHING_ELSE");
    assert_eq!(
        hub_event(h, &c, alice, wrong).await.code(),
        "VALIDATION_ERROR"
    );
    let mut wrong = nudge(conv_id, 4, "CALL_UPDATED");
    wrong["data"]["meetingCode"] = json!("abc-defg");
    assert_eq!(
        hub_event(h, &c, alice, wrong).await.code(),
        "VALIDATION_ERROR"
    );
    let foreign_id = cid(&foreign);
    let r = hub_event(h, &c, alice, nudge(foreign_id, 1, "CALL_STARTED")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        count(h, &format!("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.timeline.changed' AND conversation_id = '{foreign_id}'")).await,
        0
    );

    // A member who joins later synchronizes through the replay, inside its window. (The
    // scripted policy client has no Hub target: customer writes happen ungoverned.)
    rt.h.state.communications = None;
    let late = rt.h.principal(&c);
    let added =
        rt.h.post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{conv_id}/members"),
            json!({ "principalId": late }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);
    govern(&mut rt.h, &c);
    let h = &rt.h;
    let r = hub_event(h, &c, alice, nudge(conv_id, 5, "CALL_STARTED")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let visible_from = count(h, &format!("SELECT visible_from_event FROM messaging_conversation_members WHERE conversation_id = '{conv_id}' AND principal_id = '{late}'")).await;
    let page = h
        .get(
            &c,
            Some(late),
            &format!("/v1/conversations/{conv_id}/events?after=0&limit=1000"),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let timeline: Vec<&Value> = page.json["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "timeline.changed")
        .collect();
    assert!(timeline
        .iter()
        .all(|e| e["eventSequence"].as_i64().unwrap() > visible_from));
    assert_eq!(timeline.last().unwrap()["data"]["timelineVersion"], 5);
    // Bob was connected throughout: the live stream got v5 as well.
    let e = event_of(&mut wb, "timeline.changed").await;
    assert_eq!(e["data"]["timelineVersion"], 5);
    rt.h.conforming_event_types().await;
}
