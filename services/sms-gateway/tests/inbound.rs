//! Workstream N: inbound SMS — authenticated provider callbacks, Numbering resolution,
//! current-owner storage and quarantine, threads, opt-out keywords, reads, retention and
//! the `sms-gateway.message.received` event.

mod common;

use axum::http::{HeaderValue, StatusCode};
use common::{Customer, FakeNumber, Harness};
use serde_json::Value;
use uuid::Uuid;

const REMOTE: &str = "+15550104242";
const DCN: &str = "+15550109001";
const TEXT: &str = "Is the 3pm slot still free? ref 7731";

fn principal() -> Uuid {
    Uuid::new_v4()
}

/// A customer with sms.read and a DCN routed to a principal.
fn setup(h: &Harness) -> (Customer, FakeNumber, Uuid) {
    let c = h.customer(&["sms.read"]);
    let p = principal();
    let n = h.number(&c, DCN, Some(("PRINCIPAL", p)));
    (c, n, p)
}

async fn count(h: &Harness, table: &str) -> i64 {
    h.scalar(&format!("SELECT count(*) FROM {table}")).await
}

#[tokio::test]
async fn a_message_to_a_routed_number_is_stored_sealed_for_the_current_owner_and_announced() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n, p) = setup(&h);
    let r = h.inbound(&h.inbound_callback(REMOTE, DCN, TEXT)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "ACCEPTED");
    assert_eq!(r.json["disposition"], "APPLIED");
    common::conforms(
        "sms-gateway/common/v1.schema.json#/$defs/CallbackAck",
        &r.json,
    );

    // Stored for the environment NUMBERING named, with its facts.
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    common::conforms(
        "sms-gateway/common/v1.schema.json#/$defs/MessagePage",
        &page.json,
    );
    let m = &page.json["messages"][0];
    assert_eq!(page.json["messages"].as_array().unwrap().len(), 1);
    assert_eq!(m["direction"], "INBOUND");
    assert_eq!(m["status"], "RECEIVED");
    assert_eq!(m["numberResourceId"], serde_json::json!(n.id));
    assert_eq!(m["content"], TEXT);
    assert_eq!(m["remote"]["masked"], "+***4242");
    assert_eq!(m["routing"]["targetKind"], "PRINCIPAL");
    assert_eq!(m["routing"]["targetRef"], serde_json::json!(p));
    assert_eq!(m["principalId"], serde_json::json!(p));
    assert_eq!(m["encoding"], "GSM7");
    assert_eq!(m["segmentCount"], 1);
    let one = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/messages/{}", m["messageId"].as_str().unwrap()),
        )
        .await;
    assert_eq!(one.status, StatusCode::OK);
    common::conforms(
        "sms-gateway/common/v1.schema.json#/$defs/Message",
        &one.json,
    );
    assert_eq!(&one.json, m);
    // Never a full number or a provider reference in a response.
    for body in [&page.text, &one.text] {
        assert!(
            !body.contains("15550104242") && !body.contains("15550109001"),
            "{body}"
        );
        assert!(
            !body.contains("din-") && !body.contains("deterministic"),
            "{body}"
        );
    }
    // The assignment facts and the inbound grant are kept with the message.
    assert_eq!(
        h.text("SELECT assignment_id::text FROM sms_transports")
            .await
            .unwrap(),
        n.assignment_id.to_string()
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_transports WHERE inbound_grant_id IS NOT NULL")
            .await,
        1
    );
    // Nothing in the database holds the sender or the content in clear.
    let dump = h.dump().await;
    assert!(
        !dump.contains("15550104242") && !dump.contains("3pm slot"),
        "plaintext at rest"
    );
    // The thread, the provider event, the usage record and the event.
    assert_eq!(count(&h, "sms_transport_threads").await, 1);
    assert_eq!(
        h.text("SELECT disposition FROM sms_delivery_events")
            .await
            .as_deref(),
        Some("APPLIED")
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_usage_records WHERE direction = 'INBOUND'")
            .await,
        1
    );
    let payload: Value = serde_json::from_str(
        &h.text("SELECT payload::text FROM sms_outbox_events WHERE event_type = 'sms-gateway.message.received'")
            .await
            .unwrap(),
    )
    .unwrap();
    common::conforms(
        "sms-gateway/events/v1.schema.json",
        &serde_json::json!({ "event_type": "sms-gateway.message.received", "payload": payload }),
    );
    assert_eq!(payload["target_ref"], serde_json::json!(p));
}

#[tokio::test]
async fn callbacks_are_authenticated_before_anything_is_parsed_or_resolved() {
    let Some(h) = Harness::new().await else {
        return;
    };
    setup(&h);
    let (headers, body) = h.inbound_callback(REMOTE, DCN, TEXT);
    // No signature.
    let r = h
        .callback(
            "/provider-callbacks/v1/sms/deterministic/inbound",
            Default::default(),
            body.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert_eq!(r.code(), "CALLBACK_REJECTED");
    // A tampered body.
    let mut tampered = body.clone();
    let i = tampered.len() - 3;
    tampered[i] ^= 0x01;
    let r = h.inbound(&(headers.clone(), tampered)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A replay outside the timestamp window.
    h.advance(3600);
    let r = h.inbound(&(headers.clone(), body.clone())).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Another signature secret.
    let forger = sms_gateway::providers::deterministic::DeterministicSmsProvider::new(
        "deterministic",
        b"another-secret-of-thirty-two-bytes-0123456789",
    );
    let r = h
        .inbound(&forger.inbound_callback(REMOTE, DCN, TEXT, h.now()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // An unknown provider id.
    let signed = h.inbound_callback(REMOTE, DCN, TEXT);
    let r = h
        .callback(
            "/provider-callbacks/v1/sms/carrier-x/inbound",
            signed.0.clone(),
            signed.1.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // A correctly signed body that is not a callback.
    let junk = br#"{"kind":"INBOUND"}"#.to_vec();
    let r = h
        .inbound(&(
            h.provider.sign("din-junk", h.now().timestamp(), &junk),
            junk,
        ))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // A duplicated signature header.
    let (mut headers, body) = h.inbound_callback(REMOTE, DCN, TEXT);
    headers.append("x-provider-signature", HeaderValue::from_static("v1=00"));
    let r = h.inbound(&(headers, body)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Nothing was resolved, stored or announced.
    assert_eq!(*h.numbering.resolutions.lock().unwrap(), 0);
    for table in [
        "sms_delivery_events",
        "sms_transports",
        "sms_quarantined_inbound",
        "sms_outbox_events",
    ] {
        assert_eq!(count(&h, table).await, 0, "{table}");
    }
    // The genuine callback still works afterwards.
    let r = h.inbound(&signed).await;
    assert_eq!(r.json["disposition"], "APPLIED", "{}", r.text);
}

#[tokio::test]
async fn duplicate_and_concurrent_duplicate_callbacks_store_the_message_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, _, _) = setup(&h);
    let signed = h.inbound_callback(REMOTE, DCN, TEXT);
    let first = h.inbound(&signed).await;
    assert_eq!(first.json["status"], "ACCEPTED");
    let again = h.inbound(&signed).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json["status"], "DUPLICATE");
    assert_eq!(again.json["disposition"], "APPLIED");
    // Ten concurrent deliveries of a new event: one message.
    let signed = h.inbound_callback(REMOTE, DCN, "second message");
    let results = futures_join(&h, &signed, 10).await;
    assert_eq!(
        results.iter().filter(|s| s.as_str() == "ACCEPTED").count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|s| s.as_str() == "DUPLICATE").count(),
        9
    );
    assert_eq!(count(&h, "sms_transports").await, 2);
    assert_eq!(count(&h, "sms_delivery_events").await, 2);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_outbox_events WHERE event_type = 'sms-gateway.message.received'").await,
        2
    );
    // Both are in ONE thread with the remote party.
    let t = h.get(Some(&c.token), "/v1/sms/threads").await;
    assert_eq!(t.json["threads"].as_array().unwrap().len(), 1);
    assert_eq!(t.json["threads"][0]["messageCount"], 2);
}

async fn futures_join(
    h: &Harness,
    signed: &(axum::http::HeaderMap, Vec<u8>),
    n: usize,
) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..n {
        let router = h.router();
        let (headers, body) = signed.clone();
        set.spawn(async move {
            use http_body_util::BodyExt;
            use tower::ServiceExt;
            let mut b = axum::http::Request::builder()
                .method("POST")
                .uri("/provider-callbacks/v1/sms/deterministic/inbound");
            for (k, v) in headers.iter() {
                b = b.header(k, v);
            }
            let resp = router
                .oneshot(b.body(axum::body::Body::from(body)).unwrap())
                .await
                .unwrap();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            v["status"].as_str().unwrap_or("ERROR").to_string()
        });
    }
    let mut out = Vec::new();
    while let Some(r) = set.join_next().await {
        out.push(r.unwrap());
    }
    out
}

#[tokio::test]
async fn messages_that_cannot_be_attributed_are_quarantined_and_never_delivered() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    // Assigned but SMS routes nowhere; released (unavailable); unknown to Numbering.
    let not_routed = h.number(&c, "+15550109002", None);
    let released = h.number(&c, "+15550109003", Some(("PRINCIPAL", principal())));
    h.numbering
        .update(released.id, |n| n.unavailable = Some("NOT_ACTIVE".into()));
    for (to, reason) in [
        (not_routed.e164.as_str(), "NOT_ROUTED"),
        (released.e164.as_str(), "NOT_ACTIVE"),
        ("+15550109999", "UNKNOWN_NUMBER"),
    ] {
        let r = h.inbound(&h.inbound_callback(REMOTE, to, TEXT)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["disposition"], "QUARANTINED", "{to}");
        assert_eq!(
            h.scalar(&format!(
                "SELECT count(*) FROM sms_quarantined_inbound WHERE reason = '{reason}'"
            ))
            .await,
            1,
            "{reason}"
        );
    }
    // Never a transport, an event or a customer-visible message.
    assert_eq!(count(&h, "sms_transports").await, 0);
    assert_eq!(count(&h, "sms_outbox_events").await, 0);
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert_eq!(page.json["messages"].as_array().unwrap().len(), 0);
    // Quarantined content is sealed.
    assert!(!h.dump().await.contains("3pm slot"));
}

#[tokio::test]
async fn a_numbering_outage_fails_closed_and_the_providers_retry_is_applied_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, _, _) = setup(&h);
    let signed = h.inbound_callback(REMOTE, DCN, TEXT);
    h.numbering.set_down(true);
    let r = h.inbound(&signed).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.code(), "NUMBERING_UNAVAILABLE");
    for table in [
        "sms_delivery_events",
        "sms_transports",
        "sms_quarantined_inbound",
    ] {
        assert_eq!(
            count(&h, table).await,
            0,
            "nothing stored while unattributable: {table}"
        );
    }
    h.numbering.set_down(false);
    let r = h.inbound(&signed).await;
    assert_eq!(r.json["status"], "ACCEPTED");
    assert_eq!(r.json["disposition"], "APPLIED");
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert_eq!(page.json["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_reassigned_number_routes_to_its_current_owner_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, n, _) = setup(&h);
    let b = h.customer(&["sms.read"]);
    h.inbound(&h.inbound_callback(REMOTE, DCN, "for the first owner"))
        .await;
    // Numbering moves the number to B (new assignment, new revision).
    let new_assignment = Uuid::new_v4();
    let pb = principal();
    h.numbering.update(n.id, |x| {
        x.scope = b.scope();
        x.assignment_id = new_assignment;
        x.routing_revision = 2;
        x.sms_target = Some(("PRINCIPAL".into(), pb));
    });
    h.inbound(&h.inbound_callback(REMOTE, DCN, "for the second owner"))
        .await;
    let pa = h.get(Some(&a.token), "/v1/sms/messages").await;
    let pb_page = h.get(Some(&b.token), "/v1/sms/messages").await;
    assert_eq!(pa.json["messages"].as_array().unwrap().len(), 1);
    assert_eq!(pa.json["messages"][0]["content"], "for the first owner");
    assert_eq!(pb_page.json["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        pb_page.json["messages"][0]["content"],
        "for the second owner"
    );
    assert_eq!(
        pb_page.json["messages"][0]["routing"]["targetRef"],
        serde_json::json!(pb)
    );
    // Neither sees the other's message or thread (NOT_FOUND, not FORBIDDEN).
    let theirs = pb_page.json["messages"][0]["messageId"].as_str().unwrap();
    let r = h
        .get(Some(&a.token), &format!("/v1/sms/messages/{theirs}"))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let thread = pb_page.json["messages"][0]["threadId"].as_str().unwrap();
    let r = h
        .get(
            Some(&a.token),
            &format!("/v1/sms/threads/{thread}/messages"),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_ne!(
        pa.json["messages"][0]["threadId"],
        pb_page.json["messages"][0]["threadId"]
    );
    // The second message carries the new assignment's facts.
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM sms_transports WHERE assignment_id = '{new_assignment}' AND routing_revision = 2"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn stop_and_start_keywords_toggle_the_recipients_opt_out_for_that_number() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n, _) = setup(&h);
    let fp = h.state.protection.number_fingerprint(REMOTE);
    let blocked = |h: &Harness| {
        let conn = h.conn().clone();
        let fp = fp.clone();
        let env = c.context.environment_id;
        async move {
            sms_gateway::sms::controls::opted_out(&conn, env, n.id, &fp)
                .await
                .unwrap()
        }
    };
    h.inbound(&h.inbound_callback(REMOTE, DCN, "STOP")).await;
    assert!(blocked(&h).await);
    // A repeated STOP is idempotent; a sentence containing "stop" is not a keyword.
    h.inbound(&h.inbound_callback(REMOTE, DCN, " stop ")).await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_optout_blocks WHERE lifted_at IS NULL")
            .await,
        1
    );
    h.inbound(&h.inbound_callback(REMOTE, DCN, "START")).await;
    assert!(!blocked(&h).await);
    h.inbound(&h.inbound_callback(REMOTE, DCN, "please stop by at 3"))
        .await;
    assert!(!blocked(&h).await);
    // The keywords are messages too.
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert_eq!(page.json["messages"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn reads_need_sms_read_stay_in_the_environment_and_page_with_opaque_cursors() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n, _) = setup(&h);
    let other = h.number(&c, "+15550109004", Some(("ORGANIZATION", Uuid::new_v4())));
    for i in 0..5 {
        h.advance(1);
        h.inbound(&h.inbound_callback(REMOTE, DCN, &format!("message {i}")))
            .await;
    }
    h.advance(1);
    h.inbound(&h.inbound_callback("+447700900123", &other.e164, "to the other number"))
        .await;

    // Scope and authentication.
    let sender_only = h.credential(&c, &["sms.send"]);
    for path in ["/v1/sms/messages", "/v1/sms/threads"] {
        let r = h.get(Some(&sender_only.token), path).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
        assert_eq!(h.get(None, path).await.status, StatusCode::UNAUTHORIZED);
    }
    // Another environment sees nothing.
    let stranger = h.customer(&["sms.read"]);
    let r = h.get(Some(&stranger.token), "/v1/sms/messages").await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 0);
    let r = h.get(Some(&stranger.token), "/v1/sms/threads").await;
    assert_eq!(r.json["threads"].as_array().unwrap().len(), 0);

    // Filters.
    let r = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/messages?numberResourceId={}", other.id),
        )
        .await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 1);
    let r = h
        .get(Some(&c.token), "/v1/sms/messages?direction=OUTBOUND")
        .await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 0);
    let r = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/threads?numberResourceId={}", n.id),
        )
        .await;
    assert_eq!(r.json["threads"].as_array().unwrap().len(), 1);
    common::conforms(
        "sms-gateway/common/v1.schema.json#/$defs/ThreadPage",
        &r.json,
    );

    // Pages of two, newest first, no overlap, then the end.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("/v1/sms/messages?limit=2&cursor={c}"),
            None => "/v1/sms/messages?limit=2".into(),
        };
        let r = h.get(Some(&c.token), &path).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        for m in r.json["messages"].as_array().unwrap() {
            seen.push(m["content"].as_str().unwrap().to_string());
        }
        match r.json["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        seen,
        [
            "to the other number",
            "message 4",
            "message 3",
            "message 2",
            "message 1",
            "message 0"
        ]
    );
    // The thread's own messages.
    let t = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/threads?numberResourceId={}", n.id),
        )
        .await;
    let thread = t.json["threads"][0]["threadId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/threads/{thread}/messages?limit=100"),
        )
        .await;
    assert_eq!(r.json["messages"].as_array().unwrap().len(), 5);

    // Invalid queries are uniform 400s; unknown ids are 404s.
    for bad in [
        "/v1/sms/messages?limit=0",
        "/v1/sms/messages?limit=101",
        "/v1/sms/messages?cursor=not-a-cursor",
        "/v1/sms/messages?direction=SIDEWAYS",
        "/v1/sms/messages?tenantId=00000000-0000-4000-8000-000000000000",
        "/v1/sms/threads?numberResourceId=not-a-uuid",
    ] {
        let r = h.get(Some(&c.token), bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
        assert_eq!(r.code(), "VALIDATION_ERROR");
    }
    for missing in [
        format!("/v1/sms/messages/{}", Uuid::new_v4()),
        "/v1/sms/messages/not-a-uuid".to_string(),
        format!("/v1/sms/threads/{}/messages", Uuid::new_v4()),
    ] {
        assert_eq!(
            h.get(Some(&c.token), &missing).await.status,
            StatusCode::NOT_FOUND,
            "{missing}"
        );
    }
}

#[tokio::test]
async fn content_and_quarantine_are_purged_after_their_retention() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, _, _) = setup(&h);
    h.inbound(&h.inbound_callback(REMOTE, DCN, TEXT)).await;
    h.inbound(&h.inbound_callback(REMOTE, "+15550109999", TEXT))
        .await;
    let r = sms_gateway::sms::workers::sweep(&h.state).await.unwrap();
    assert_eq!(
        (r.contents_purged, r.quarantined_purged),
        (0, 0),
        "within retention"
    );
    h.advance(31 * 86_400);
    let (a, b) = tokio::join!(
        sms_gateway::sms::workers::sweep(&h.state),
        sms_gateway::sms::workers::sweep(&h.state)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.contents_purged + b.contents_purged, 1);
    assert_eq!(a.quarantined_purged + b.quarantined_purged, 1);
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    let m = &page.json["messages"][0];
    assert_eq!(m["content"], Value::Null);
    assert_eq!(m["contentPurged"], true);
    assert_eq!(m["status"], "RECEIVED");
    assert_eq!(m["remote"]["masked"], "+***4242");
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Message", m);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_transports WHERE content_sealed IS NOT NULL")
            .await,
        0
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_quarantined_inbound WHERE content_sealed IS NOT NULL")
            .await,
        0
    );
}

#[tokio::test]
async fn invalid_numbers_and_content_are_refused_and_recorded_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    setup(&h);
    for (from, to, text) in [
        ("BANKALERT", DCN, TEXT),
        (REMOTE, "12345", TEXT),
        (REMOTE, DCN, &"x".repeat(1601) as &str),
    ] {
        let signed = h.inbound_callback(from, to, text);
        let r = h.inbound(&signed).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["disposition"], "REFUSED");
        let again = h.inbound(&signed).await;
        assert_eq!(again.json["status"], "DUPLICATE");
    }
    assert_eq!(count(&h, "sms_transports").await, 0);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_delivery_events WHERE disposition = 'REFUSED'")
            .await,
        3
    );
    assert_eq!(
        *h.numbering.resolutions.lock().unwrap(),
        0,
        "never resolved"
    );
}

#[tokio::test]
async fn a_routing_change_racing_delivery_is_caught_at_the_final_boundary_and_quarantined() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, n, _) = setup(&h);
    let b = h.customer(&["sms.read"]);
    // Numbering resolves for A, then the number moves to B before the message is stored.
    let to_b = b.scope();
    *h.numbering.after_resolve.lock().unwrap() = Some(Box::new(move |numbers| {
        let x = numbers.iter_mut().find(|x| x.id == n.id).unwrap();
        x.scope = to_b;
        x.assignment_id = Uuid::new_v4();
        x.routing_revision += 1;
    }));
    let r = h
        .inbound(&h.inbound_callback(REMOTE, DCN, "late content for nobody"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["disposition"], "QUARANTINED");
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_quarantined_inbound WHERE reason = 'ROUTING_CHANGED'")
            .await,
        1
    );
    // Neither the previous nor the next owner sees it; no transport, thread or event.
    for owner in [&a, &b] {
        let page = h.get(Some(&owner.token), "/v1/sms/messages").await;
        assert_eq!(page.json["messages"].as_array().unwrap().len(), 0);
    }
    for table in [
        "sms_transports",
        "sms_transport_threads",
        "sms_outbox_events",
        "sms_usage_records",
    ] {
        assert_eq!(count(&h, table).await, 0, "{table}");
    }
    assert!(!h.dump().await.contains("late content"));
    // The next message is resolved afresh and reaches the CURRENT owner.
    let r = h
        .inbound(&h.inbound_callback(REMOTE, DCN, "for the new owner"))
        .await;
    assert_eq!(r.json["disposition"], "APPLIED");
    let page = h.get(Some(&b.token), "/v1/sms/messages").await;
    assert_eq!(page.json["messages"][0]["content"], "for the new owner");
    assert_eq!(
        *h.numbering.verifications.lock().unwrap(),
        2,
        "every delivery is re-verified"
    );
}

#[tokio::test]
async fn numbering_unreachable_at_the_final_boundary_stores_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, _, _) = setup(&h);
    let signed = h.inbound_callback(REMOTE, DCN, TEXT);
    *h.numbering.verify_down.lock().unwrap() = true;
    let r = h.inbound(&signed).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.code(), "NUMBERING_UNAVAILABLE");
    for table in [
        "sms_delivery_events",
        "sms_transports",
        "sms_transport_threads",
        "sms_outbox_events",
    ] {
        assert_eq!(count(&h, table).await, 0, "{table}");
    }
    *h.numbering.verify_down.lock().unwrap() = false;
    let r = h.inbound(&signed).await;
    assert_eq!(r.json["disposition"], "APPLIED");
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert_eq!(page.json["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn out_of_order_callbacks_are_each_stored_once_and_the_thread_never_moves_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, _, _) = setup(&h);
    // Two provider events, the later one delivered first (both within the window).
    let earlier = h.provider.inbound_callback(
        REMOTE,
        DCN,
        "first written",
        h.now() - chrono::Duration::seconds(60),
    );
    let later = h.inbound_callback(REMOTE, DCN, "second written");
    assert_eq!(h.inbound(&later).await.json["disposition"], "APPLIED");
    let thread_after_later = h
        .text("SELECT last_message_at::text FROM sms_transport_threads")
        .await;
    h.advance(1);
    assert_eq!(h.inbound(&earlier).await.json["disposition"], "APPLIED");
    // Replays of either are duplicates.
    assert_eq!(h.inbound(&later).await.json["status"], "DUPLICATE");
    assert_eq!(h.inbound(&earlier).await.json["status"], "DUPLICATE");
    assert_eq!(count(&h, "sms_transports").await, 2);
    let t = h.get(Some(&c.token), "/v1/sms/threads").await;
    assert_eq!(t.json["threads"][0]["messageCount"], 2);
    assert!(
        h.text("SELECT last_message_at::text FROM sms_transport_threads")
            .await
            >= thread_after_later
    );
    // The provider's own timestamps are kept as evidence.
    assert_eq!(
        h.scalar("SELECT count(DISTINCT occurred_at) FROM sms_delivery_events")
            .await,
        2
    );
}
