//! Workstream O: outbound SMS — authorization (sender grant, ownership, spoofing),
//! idempotency, durable submission and unknown outcomes, delivery receipts, segments and
//! cost, and the send-time safety controls. Step 19's certification list maps onto the
//! tests below (numbers in the doc comments).

mod common;

use axum::http::StatusCode;
use common::{Customer, FakeNumber, Harness};
use serde_json::{json, Value};
use sms_gateway::providers::{deterministic::Script, ReceiptStatus};
use uuid::Uuid;

const DCN: &str = "+15550109001";
const TO: &str = "+15550104242";

fn setup(h: &Harness) -> (Customer, FakeNumber) {
    let c = h.customer(&["sms.send", "sms.read"]);
    let n = h.number(&c, DCN, Some(("PRINCIPAL", Uuid::new_v4())));
    (c, n)
}

fn body(n: &FakeNumber, to: &str, text: &str) -> Value {
    json!({ "from": { "numberResourceId": n.id }, "to": to, "text": text })
}

fn key() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

async fn status_of(h: &Harness, c: &Customer, id: &str) -> Value {
    h.get(Some(&c.token), &format!("/v1/sms/messages/{id}"))
        .await
        .json
}

async fn count(h: &Harness, sql: &str) -> i64 {
    h.scalar(sql).await
}

async fn events(h: &Harness) -> Vec<String> {
    let rows = h
        .text("SELECT string_agg(event_type, ',' ORDER BY sequence) FROM sms_outbox_events")
        .await
        .unwrap_or_default();
    rows.split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn provider_ref(h: &Harness, message_id: &str) -> String {
    let key = format!("sms-{}", Uuid::parse_str(message_id).unwrap().simple());
    h.provider.held_by_key(&key).unwrap().provider_message_ref
}

/// 1, 12, 14, 19, 20, 22: an authorized message is submitted, delivered, its cost
/// confirmed once, every step announced, and the reads expose status but no secrets.
#[tokio::test]
async fn an_authorized_message_is_submitted_delivered_and_its_cost_confirmed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let text = "Your table is ready. Reply to confirm.";
    let r = h.send(&c, body(&n, TO, text), &key()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Message", &r.json);
    assert_eq!(r.json["status"], "AUTHORIZED");
    assert_eq!(r.json["direction"], "OUTBOUND");
    assert_eq!(r.json["remote"]["masked"], "+***4242");
    assert_eq!(r.json["cost"]["estimatedMinor"], 5);
    let id = r.json["messageId"].as_str().unwrap().to_string();
    // Nothing reached the provider yet; the sender grant was consumed for THIS message.
    assert!(h.provider.submissions().is_empty());
    let released = h.numbering.released.lock().unwrap().clone();
    assert_eq!(
        released,
        vec![(
            format!("sms:{}", Uuid::parse_str(&id).unwrap().simple()),
            n.id
        )]
    );

    let d = h.dispatch().await;
    assert_eq!((d.claimed, d.submitted), (1, 1), "{d:?}");
    let held = h.provider.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].from, DCN, "sent from the Numbering-released sender");
    assert_eq!(held[0].to, TO);
    assert_eq!(held[0].body, text);
    assert_eq!(status_of(&h, &c, &id).await["status"], "SUBMITTED");

    let reference = provider_ref(&h, &id);
    let r = h
        .receipt(&h.receipt_callback(&reference, ReceiptStatus::Delivered, None))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["disposition"], "APPLIED");
    let m = status_of(&h, &c, &id).await;
    assert_eq!(m["status"], "DELIVERED");
    assert_eq!(m["cost"]["confirmedMinor"], 4);
    assert!(m["deliveredAt"].is_string() && m["submittedAt"].is_string());
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Message", &m);
    assert_eq!(
        count(&h, "SELECT count(*) FROM sms_usage_records WHERE status = 'CONFIRMED' AND confirmed_cost_minor = 4").await,
        1
    );
    assert_eq!(
        events(&h).await,
        [
            "sms-gateway.message.requested",
            "sms-gateway.message.submitted",
            "sms-gateway.message.delivered",
        ]
    );
    for payload in h
        .text("SELECT json_agg(payload)::text FROM sms_outbox_events")
        .await
        .into_iter()
    {
        for p in serde_json::from_str::<Vec<Value>>(&payload).unwrap() {
            let t = format!(
                "sms-gateway.message.{}",
                match p["status"].as_str().unwrap() {
                    "AUTHORIZED" => "requested",
                    "SUBMITTED" => "submitted",
                    _ => "delivered",
                }
            );
            common::conforms(
                "sms-gateway/events/v1.schema.json",
                &json!({ "event_type": t, "payload": p }),
            );
        }
    }
    // No plaintext at rest, no provider reference or number in any response.
    let dump = h.dump().await;
    assert!(
        !dump.contains("15550104242") && !dump.contains("table is ready"),
        "plaintext at rest"
    );
    let page = h.get(Some(&c.token), "/v1/sms/messages").await;
    assert!(!page.text.contains(&reference) && !page.text.contains("15550104242"));
    // A read needs sms.read; another environment never sees it.
    let sender_only = h.credential(&c, &["sms.send"]);
    assert_eq!(
        h.get(Some(&sender_only.token), &format!("/v1/sms/messages/{id}"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let stranger = h.customer(&["sms.read", "sms.send"]);
    assert_eq!(
        h.get(Some(&stranger.token), &format!("/v1/sms/messages/{id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

/// 2, 4, 5, 6: only the current owner's ACTIVE, SMS-capable number can send; a refusal is
/// recorded with SENDER_NOT_AUTHORIZED and nothing reaches the provider.
#[tokio::test]
async fn only_the_scopes_own_active_sms_capable_number_can_send() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let other = h.customer(&["sms.send", "sms.read"]);
    let theirs = h.number(&other, "+15550109002", None);
    let no_sms = h.number(&c, "+15550109003", None);
    h.numbering.update(no_sms.id, |x| x.outbound_sms = false);
    let suspended = h.number(&c, "+15550109004", None);
    h.numbering
        .update(suspended.id, |x| x.unavailable = Some("SUSPENDED".into()));
    for from in [&theirs, &no_sms, &suspended] {
        let r = h.send(&c, body(from, TO, "hello"), &key()).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
        assert_eq!(r.code(), "SMS_BLOCKED");
        assert_eq!(r.reason(), "SENDER_NOT_AUTHORIZED");
        let id = r.json["error"]["messageId"].as_str().unwrap();
        let m = status_of(&h, &c, id).await;
        assert_eq!(m["status"], "FAILED");
        assert_eq!(m["blockedReason"], "SENDER_NOT_AUTHORIZED");
    }
    // A principal-bound number sends only for that principal.
    let p = Uuid::new_v4();
    h.numbering.update(n.id, |x| x.sender_principal = Some(p));
    let r = h.send(&c, body(&n, TO, "hello"), &key()).await;
    assert_eq!(r.reason(), "SENDER_NOT_AUTHORIZED");
    let mut b = body(&n, TO, "hello");
    b["principalId"] = json!(p);
    let r = h.send(&c, b, &key()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    // An inactive principal is refused before Numbering is asked.
    let q = Uuid::new_v4();
    h.principals.inactive.lock().unwrap().insert(q);
    let mut b = body(&n, TO, "hello");
    b["principalId"] = json!(q);
    let r = h.send(&c, b, &key()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Nothing but the authorized one ever reaches the provider.
    h.dispatch().await;
    assert_eq!(h.provider.submissions().len(), 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_outbox_events WHERE event_type = 'sms-gateway.message.blocked'").await,
        4
    );
}

/// 3: the sender is a Number Resource only — a sender id, a raw number or any extra field
/// cannot be expressed, and the scope never comes from the body.
#[tokio::test]
async fn sender_ids_raw_senders_and_foreign_scopes_cannot_be_expressed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    for bad in [
        json!({ "from": "+15550100999", "to": TO, "text": "x" }),
        json!({ "from": { "numberResourceId": n.id, "senderId": "MYBRAND" }, "to": TO, "text": "x" }),
        json!({ "from": { "numberResourceId": n.id, "e164": "+15550100999" }, "to": TO, "text": "x" }),
        json!({ "from": { "numberResourceId": n.id }, "to": TO, "text": "x", "tenantId": Uuid::new_v4() }),
        json!({ "from": { "numberResourceId": n.id }, "to": TO, "threadId": Uuid::new_v4(), "text": "x" }),
        json!({ "from": { "numberResourceId": n.id }, "text": "x" }),
        json!({ "from": { "numberResourceId": n.id }, "to": "5550104242", "text": "x" }),
        json!({ "from": { "numberResourceId": n.id }, "to": TO, "text": "" }),
        json!({ "from": { "numberResourceId": n.id }, "to": TO, "text": "x", "validitySeconds": 5 }),
    ] {
        let r = h.send(&c, bad.clone(), &key()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
    assert_eq!(count(&h, "SELECT count(*) FROM sms_transports").await, 0);
    assert!(h.numbering.released.lock().unwrap().is_empty());
}

/// 7, 8, 9: the same key replays the same message, a different request with it is
/// refused, concurrent duplicates create one message and the sender grant is single-use.
#[tokio::test]
async fn idempotent_and_concurrent_duplicate_sends_create_one_message_and_use_one_grant() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let r = h
        .post(Some(&c.token), "/v1/sms/messages", body(&n, TO, "x"), None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.code(), "IDEMPOTENCY_KEY_REQUIRED");
    let k = key();
    let first = h.send(&c, body(&n, TO, "hello"), &k).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    let again = h.send(&c, body(&n, TO, "hello"), &k).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json["messageId"], first.json["messageId"]);
    let other = h.send(&c, body(&n, TO, "different"), &k).await;
    assert_eq!(other.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(other.code(), "IDEMPOTENCY_KEY_REUSED");
    // Another credential's key space is its own (service account).
    // Ten concurrent requests with one new key.
    let k = key();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let router = h.router();
        let (token, b, k) = (c.token.clone(), body(&n, TO, "concurrent"), k.clone());
        set.spawn(async move {
            use http_body_util::BodyExt;
            use tower::ServiceExt;
            let req = axum::http::Request::builder()
                .method("POST")
                .uri("/v1/sms/messages")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", k)
                .body(axum::body::Body::from(b.to_string()))
                .unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        });
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut accepted = 0;
    while let Some(r) = set.join_next().await {
        let (status, json) = r.unwrap();
        assert!(
            status == StatusCode::ACCEPTED || status == StatusCode::OK,
            "{status} {json}"
        );
        accepted += usize::from(status == StatusCode::ACCEPTED);
        ids.insert(json["messageId"].as_str().unwrap().to_string());
    }
    assert_eq!(ids.len(), 1, "one message");
    assert_eq!(accepted, 1);
    assert_eq!(count(&h, "SELECT count(*) FROM sms_transports").await, 2);
    // Every grant operation reference is distinct (single use) and bound to one message.
    let released = h.numbering.released.lock().unwrap().clone();
    let ops: std::collections::BTreeSet<_> = released.iter().map(|(o, _)| o.clone()).collect();
    assert_eq!(ops.len(), released.len());
    // Numbering refuses an expired / revoked grant: recorded, never sent.
    *h.numbering.refuse_sender.lock().unwrap() = Some("GRANT_EXPIRED".into());
    let r = h.send(&c, body(&n, TO, "late"), &key()).await;
    assert_eq!(r.reason(), "SENDER_NOT_AUTHORIZED");
    *h.numbering.refuse_sender.lock().unwrap() = None;
    let d = h.dispatch().await;
    assert_eq!(d.submitted, 2);
    assert_eq!(h.provider.submissions().len(), 2);
    // A replay of a blocked message is the same refusal.
    let k = key();
    let blocked = h.send(&c, body(&n, "+33612345678", "x"), &k).await;
    let replay = h.send(&c, body(&n, "+33612345678", "x"), &k).await;
    assert_eq!(replay.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        replay.json["error"]["messageId"],
        blocked.json["error"]["messageId"]
    );
}

/// 10, 11, 21: a timeout after transmission is UNKNOWN and never resubmitted; read-back
/// resolves it; a confirmed non-receipt fails it; no answer leaves it UNKNOWN.
#[tokio::test]
async fn ambiguous_timeouts_are_unknown_never_resubmitted_and_resolved_by_read_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    h.provider.script(&[
        Script::TimeoutAfterCreate,
        Script::TimeoutBeforeCreate,
        Script::Malformed,
    ]);
    let mut ids = Vec::new();
    for text in [
        "created then timed out",
        "never created",
        "malformed answer",
    ] {
        let r = h.send(&c, body(&n, TO, text), &key()).await;
        ids.push(r.json["messageId"].as_str().unwrap().to_string());
        let d = h.dispatch().await;
        assert_eq!(d.unknown, 1, "{d:?}");
    }
    for id in &ids {
        assert_eq!(status_of(&h, &c, id).await["status"], "UNKNOWN");
    }
    assert_eq!(h.scalar("SELECT count(*) FROM sms_outbox_events WHERE event_type = 'sms-gateway.message.unknown'").await, 3);
    // Repeated passes never submit again (and cannot: the live-attempt index).
    for _ in 0..3 {
        let d = h.dispatch().await;
        assert_eq!(d.claimed, 0);
    }
    assert_eq!(
        h.provider.submissions().len(),
        2,
        "the never-created one did not reach the provider"
    );
    // Read-back: no answer → still UNKNOWN.
    h.provider.set_query_unknown(true);
    h.advance(31);
    h.dispatch().await;
    for id in &ids {
        assert_eq!(status_of(&h, &c, id).await["status"], "UNKNOWN");
    }
    h.provider.set_query_unknown(false);
    // Found → SUBMITTED (bound), NotFound within the grace → still UNKNOWN.
    h.advance(60);
    let d = h.dispatch().await;
    assert_eq!(d.reconciled, 2, "{d:?}");
    assert_eq!(status_of(&h, &c, &ids[0]).await["status"], "SUBMITTED");
    assert_eq!(status_of(&h, &c, &ids[2]).await["status"], "SUBMITTED");
    assert_eq!(status_of(&h, &c, &ids[1]).await["status"], "UNKNOWN");
    // After the grace the provider's "never received" fails it (not resent).
    h.advance(900);
    h.dispatch().await;
    let m = status_of(&h, &c, &ids[1]).await;
    assert_eq!(m["status"], "FAILED");
    assert_eq!(m["failureCode"], "NOT_TRANSMITTED");
    assert_eq!(h.provider.submissions().len(), 2);
    // The read-back message's receipt now matches.
    let reference = provider_ref(&h, &ids[0]);
    let r = h
        .receipt(&h.receipt_callback(&reference, ReceiptStatus::Delivered, None))
        .await;
    assert_eq!(r.json["disposition"], "APPLIED");
    assert_eq!(status_of(&h, &c, &ids[0]).await["status"], "DELIVERED");
    // The UNKNOWN attempts are now SUBMITTED by evidence; never a second attempt.
    assert_eq!(h.scalar("SELECT count(*) FROM sms_attempts").await, 3);
}

/// 21: a worker that died mid-submission leaves an UNKNOWN (lease recovery), definitely
/// unsent attempts retry with backoff, and concurrent dispatchers submit each message once.
#[tokio::test]
async fn restarts_recover_safely_and_concurrent_dispatchers_submit_each_message_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    // A claim whose worker vanished (simulated: an attempt CLAIMED with an old lease).
    let r = h.send(&c, body(&n, TO, "crashed worker"), &key()).await;
    let crashed = r.json["messageId"].as_str().unwrap().to_string();
    h.sql(&format!(
        "WITH a AS (INSERT INTO sms_attempts (id, transport_id, attempt_number, status, lease_owner, lease_until, provider_id, started_at) \
           VALUES (gen_random_uuid(), '{crashed}', 1, 'CLAIMED', 'dead-instance', now() - interval '1 hour', 'deterministic', now() - interval '1 hour') RETURNING 1) \
         UPDATE sms_transports SET status = 'SUBMITTING', attempt_count = 1, version = version + 1 WHERE id = '{crashed}'"
    ))
    .await
    .unwrap();
    let d = h.dispatch().await;
    assert_eq!(d.recovered_unknown, 1, "{d:?}");
    assert_eq!(status_of(&h, &c, &crashed).await["status"], "UNKNOWN");
    assert!(h.provider.submissions().is_empty(), "never resubmitted");

    // Definitely-unsent attempts: retried with backoff, then success.
    h.provider
        .script(&[Script::NotSent("THROTTLED"), Script::NotSent("THROTTLED")]);
    let r = h.send(&c, body(&n, TO, "throttled twice"), &key()).await;
    let retried = r.json["messageId"].as_str().unwrap().to_string();
    assert_eq!(h.dispatch().await.not_sent, 1);
    assert_eq!(status_of(&h, &c, &retried).await["status"], "AUTHORIZED");
    assert_eq!(h.dispatch().await.claimed, 0, "backoff");
    h.advance(10);
    assert_eq!(h.dispatch().await.not_sent, 1);
    h.advance(60);
    assert_eq!(h.dispatch().await.submitted, 1);
    assert_eq!(status_of(&h, &c, &retried).await["status"], "SUBMITTED");
    // Attempts are spent: FAILED with the provider's code.
    h.provider.script(&[Script::NotSent("THROTTLED"); 3]);
    let r = h.send(&c, body(&n, TO, "throttled forever"), &key()).await;
    let spent = r.json["messageId"].as_str().unwrap().to_string();
    for _ in 0..3 {
        h.dispatch().await;
        h.advance(600);
    }
    let m = status_of(&h, &c, &spent).await;
    assert_eq!(m["status"], "FAILED");
    assert_eq!(m["failureCode"], "THROTTLED");

    // Twenty messages, four concurrent dispatchers: twenty submissions, each once.
    let before = h.provider.submissions().len();
    for i in 0..20 {
        h.send(
            &c,
            body(&n, &format!("+1555010{:04}", 5000 + i), "batch"),
            &key(),
        )
        .await;
    }
    let (a, b, c2, d2) = tokio::join!(h.dispatch(), h.dispatch(), h.dispatch(), h.dispatch());
    assert_eq!(a.submitted + b.submitted + c2.submitted + d2.submitted, 20);
    let subs = h.provider.submissions();
    let mut unique = subs[before..].to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 20);
    assert_eq!(subs.len() - before, 20);
}

/// 5: a sender revoked between authorization and submission never reaches the provider.
#[tokio::test]
async fn a_sender_revoked_after_authorization_is_stopped_before_the_provider() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let r = h.send(&c, body(&n, TO, "revoked meanwhile"), &key()).await;
    let id = r.json["messageId"].as_str().unwrap().to_string();
    // Numbering reassigns the number (new assignment) before dispatch.
    h.numbering
        .update(n.id, |x| x.assignment_id = Uuid::new_v4());
    let d = h.dispatch().await;
    assert_eq!(d.sender_revoked, 1, "{d:?}");
    let m = status_of(&h, &c, &id).await;
    assert_eq!(m["status"], "FAILED");
    assert_eq!(m["failureCode"], "SENDER_REVOKED");
    assert!(h.provider.submissions().is_empty());
    // Numbering unreachable at dispatch: not sent, retried later (fail closed).
    let n2 = h.number(&c, "+15550109007", None);
    let r = h.send(&c, body(&n2, TO, "numbering down"), &key()).await;
    let id2 = r.json["messageId"].as_str().unwrap().to_string();
    *h.numbering.verify_down.lock().unwrap() = true;
    assert_eq!(h.dispatch().await.not_sent, 1);
    assert_eq!(status_of(&h, &c, &id2).await["status"], "AUTHORIZED");
    *h.numbering.verify_down.lock().unwrap() = false;
    h.advance(30);
    assert_eq!(h.dispatch().await.submitted, 1);
    // A message past its validity before dispatch expires unsent.
    let mut b = body(&n2, TO, "too late");
    b["validitySeconds"] = json!(60);
    let r = h.send(&c, b, &key()).await;
    let id3 = r.json["messageId"].as_str().unwrap().to_string();
    h.advance(61);
    let d = h.dispatch().await;
    assert_eq!(d.expired, 1);
    assert_eq!(status_of(&h, &c, &id3).await["status"], "EXPIRED");
    assert_eq!(h.provider.submissions().len(), 1);
}

/// 12, 13: failures, duplicate and reordered receipts, unknown references and forged
/// receipts: no regression, no duplicate cost.
#[tokio::test]
async fn receipts_are_deduplicated_order_safe_and_never_regress_a_final_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let a = h.send(&c, body(&n, TO, "will deliver"), &key()).await.json["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    let b = h
        .send(&c, body(&n, "+15550104243", "will fail"), &key())
        .await
        .json["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    h.dispatch().await;
    let (ra, rb) = (provider_ref(&h, &a), provider_ref(&h, &b));
    // Delivered, then a duplicate, a stale acceptance and a late failure.
    let delivered = h.receipt_callback(&ra, ReceiptStatus::Delivered, None);
    assert_eq!(h.receipt(&delivered).await.json["disposition"], "APPLIED");
    let dup = h.receipt(&delivered).await;
    assert_eq!(dup.json["status"], "DUPLICATE");
    assert_eq!(
        h.receipt(&h.receipt_callback(&ra, ReceiptStatus::Accepted, None))
            .await
            .json["disposition"],
        "STALE"
    );
    assert_eq!(
        h.receipt(&h.receipt_callback(&ra, ReceiptStatus::Failed, Some("CARRIER_REJECTED")))
            .await
            .json["disposition"],
        "STALE"
    );
    assert_eq!(status_of(&h, &c, &a).await["status"], "DELIVERED");
    // The provider's cost (reported at acceptance) is confirmed once per message, never
    // again by a receipt.
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM sms_usage_records WHERE status = 'CONFIRMED'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &h,
            "SELECT COALESCE(sum(confirmed_cost_minor), 0)::bigint FROM sms_usage_records"
        )
        .await,
        8
    );
    // A failure with a provider error class (bounded upper-snake, never provider text).
    let r = h
        .receipt(&h.receipt_callback(&rb, ReceiptStatus::Failed, Some("handset unreachable!")))
        .await;
    assert_eq!(r.json["disposition"], "APPLIED");
    let m = status_of(&h, &c, &b).await;
    assert_eq!(m["status"], "FAILED");
    assert_eq!(m["failureCode"], "PROVIDER_ERROR");
    let r = h
        .receipt(&h.receipt_callback(&rb, ReceiptStatus::Delivered, None))
        .await;
    assert_eq!(
        r.json["disposition"], "STALE",
        "no resurrection after FAILED"
    );
    // An unknown reference is kept as evidence only.
    let r = h
        .receipt(&h.receipt_callback("dsms-unknown", ReceiptStatus::Delivered, None))
        .await;
    assert_eq!(r.json["disposition"], "UNMATCHED");
    // Forged, tampered and stale receipts are refused before parsing.
    let (headers, mut raw) = h.receipt_callback(&ra, ReceiptStatus::Failed, None);
    raw[5] ^= 1;
    assert_eq!(
        h.receipt(&(headers, raw)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let old = h.receipt_callback(&rb, ReceiptStatus::Delivered, None);
    h.advance(3600);
    assert_eq!(h.receipt(&old).await.status, StatusCode::UNAUTHORIZED);
    // An inbound callback on the receipt path is malformed.
    let r = h.receipt(&h.inbound_callback(TO, DCN, "hi")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_outbox_events WHERE event_type IN ('sms-gateway.message.delivered', 'sms-gateway.message.failed')").await,
        2,
        "one terminal event per message"
    );
}

/// 14: GSM-7 / UCS-2 classification, segments and estimates; the segment limit.
#[tokio::test]
async fn segments_and_cost_follow_the_encoding() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    for (text, encoding, segments) in [
        ("a".repeat(160), "GSM7", 1),
        ("a".repeat(161), "GSM7", 2),
        ("€".repeat(80), "GSM7", 1),
        ("€".repeat(81), "GSM7", 2),
        ("ü".repeat(160), "GSM7", 1),
        ("é".repeat(160), "GSM7", 1),
        ("ж".repeat(70), "UCS2", 1),
        ("ж".repeat(71), "UCS2", 2),
        ("🙂".repeat(35), "UCS2", 1),
        ("🙂".repeat(36), "UCS2", 2),
    ] {
        let r = h.send(&c, body(&n, TO, &text), &key()).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{text}: {}", r.text);
        assert_eq!(r.json["encoding"], encoding, "{text}");
        assert_eq!(r.json["segmentCount"], segments, "{text}");
        assert_eq!(r.json["cost"]["estimatedMinor"], 5 * segments, "{text}");
    }
    let r = h.send(&c, body(&n, TO, &"a".repeat(1531)), &key()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.code(), "CONTENT_TOO_LONG");
    let r = h.send(&c, body(&n, TO, "bell\u{7}"), &key()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

/// 15: rate windows (tenant, environment, number, principal, recipient) are 429s that are
/// not recorded.
#[tokio::test]
async fn rate_windows_refuse_with_429_and_record_nothing() {
    let Some(h) = Harness::build(|s| {
        s.controls.rate_per_minute = 3;
        s.controls.destination_rate_per_hour = 2;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    assert_eq!(
        h.send(&c, body(&n, TO, "one"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        h.send(&c, body(&n, TO, "two"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    let r = h.send(&c, body(&n, TO, "three"), &key()).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text);
    assert_eq!(r.reason(), "DESTINATION_RATE_LIMITED");
    assert!(r.headers.contains_key("retry-after"));
    assert_eq!(
        h.send(&c, body(&n, "+15550104243", "four"), &key())
            .await
            .status,
        StatusCode::ACCEPTED
    );
    let r = h.send(&c, body(&n, "+15550104244", "five"), &key()).await;
    assert_eq!(r.reason(), "RATE_LIMITED");
    assert_eq!(
        count(&h, "SELECT count(*) FROM sms_transports").await,
        3,
        "rate refusals are not recorded"
    );
    h.advance(60);
    assert_eq!(
        h.send(&c, body(&n, "+15550104244", "five"), &key())
            .await
            .status,
        StatusCode::ACCEPTED
    );

    let Some(h) = Harness::build(|s| {
        s.controls.number_rate_per_minute = 1;
        s.controls.principal_rate_per_minute = 1;
        s.controls.tenant_rate_per_minute = 100;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    let n2 = h.number(&c, "+15550109002", None);
    assert_eq!(
        h.send(&c, body(&n, TO, "a"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        h.send(&c, body(&n, "+15550104243", "b"), &key())
            .await
            .reason(),
        "NUMBER_RATE_LIMITED"
    );
    let p = Uuid::new_v4();
    let mut b = body(&n2, TO, "c");
    b["principalId"] = json!(p);
    assert_eq!(
        h.send(&c, b.clone(), &key()).await.status,
        StatusCode::ACCEPTED
    );
    h.advance(60);
    let mut b2 = body(&n, "+15550104245", "d");
    b2["principalId"] = json!(p);
    assert_eq!(
        h.send(&c, b2.clone(), &key()).await.status,
        StatusCode::ACCEPTED
    );
    let mut b3 = body(&n2, "+15550104246", "e");
    b3["principalId"] = json!(p);
    assert_eq!(
        h.send(&c, b3, &key()).await.reason(),
        "PRINCIPAL_RATE_LIMITED"
    );
}

/// 16, 17, 18: destination policy, premium ranges, spend ceilings, the cost-spike and
/// provider-anomaly circuits, opt-outs and abuse — recorded with reason codes, before any
/// provider call, even though Numbering would have issued a grant.
#[tokio::test]
async fn safety_controls_block_before_the_provider_with_explainable_reasons() {
    let Some(h) = Harness::build(|s| {
        s.controls.daily_spend_limit_minor = 12;
        s.controls.abuse_identical_recipients = 2;
        s.controls.repeated_destination_limit = 2;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    let blocked = |r: &common::Response| r.reason().to_string();
    // Destination policy.
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+33612345678", "x"), &key()).await),
        "DESTINATION_NOT_ALLOWED"
    );
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+19005550100", "x"), &key()).await),
        "PREMIUM_DESTINATION_BLOCKED"
    );
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+449098790000", "x"), &key()).await),
        "PREMIUM_DESTINATION_BLOCKED"
    );
    // Opt-out by the customer, then by the recipient's STOP (inbound), lifted by START.
    let r = h
        .post(
            Some(&c.token),
            "/v1/sms/opt-outs",
            json!({ "to": "+15550104250", "numberResourceId": n.id }),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/OptOut", &r.json);
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+15550104250", "x"), &key()).await),
        "RECIPIENT_OPTED_OUT"
    );
    h.inbound(&h.inbound_callback("+15550104251", DCN, "STOP"))
        .await;
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+15550104251", "x"), &key()).await),
        "RECIPIENT_OPTED_OUT"
    );
    h.inbound(&h.inbound_callback("+15550104251", DCN, "START"))
        .await;
    assert_eq!(
        h.send(&c, body(&n, "+15550104251", "welcome back"), &key())
            .await
            .status,
        StatusCode::ACCEPTED
    );
    // Repeats to one recipient, and identical content fanned out.
    assert_eq!(
        h.send(&c, body(&n, TO, "promo"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        blocked(&h.send(&c, body(&n, "+15550104260", "promo"), &key()).await),
        "SPEND_LIMIT_EXCEEDED"
    );
    // (spend: 5 + 5 = 10 spent; the next estimate of 5 would exceed 12)
    let spent = h.scalar("SELECT COALESCE(sum(estimated_cost_minor), 0)::bigint FROM sms_usage_records WHERE direction = 'OUTBOUND'").await;
    assert_eq!(spent, 10);
    // Blocked messages are recorded FAILED with sealed content and an sms.blocked event.
    let reasons = h
        .text("SELECT string_agg(blocked_reason, ',' ORDER BY created_at) FROM sms_transports WHERE blocked_reason IS NOT NULL")
        .await
        .unwrap();
    assert_eq!(
        reasons,
        "DESTINATION_NOT_ALLOWED,PREMIUM_DESTINATION_BLOCKED,PREMIUM_DESTINATION_BLOCKED,RECIPIENT_OPTED_OUT,RECIPIENT_OPTED_OUT,SPEND_LIMIT_EXCEEDED"
    );
    assert!(!h.dump().await.contains("welcome back"));
    h.dispatch().await;
    assert_eq!(
        h.provider.submissions().len(),
        2,
        "only the allowed messages reached the provider"
    );

    // Abuse and repeats (fresh harness, generous spend).
    let Some(h) = Harness::build(|s| {
        s.controls.abuse_identical_recipients = 2;
        s.controls.repeated_destination_limit = 2;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    for to in ["+15550104261", "+15550104262"] {
        assert_eq!(
            h.send(&c, body(&n, to, "Win a prize now"), &key())
                .await
                .status,
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(
        blocked(
            &h.send(&c, body(&n, "+15550104263", "win a  PRIZE now"), &key())
                .await
        ),
        "ABUSE_SUSPECTED"
    );
    assert_eq!(
        h.send(&c, body(&n, TO, "reminder"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        h.send(&c, body(&n, TO, "reminder"), &key()).await.status,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        blocked(&h.send(&c, body(&n, TO, "reminder"), &key()).await),
        "REPEATED_DESTINATION"
    );
}

/// 17: the cost-spike circuit (confirmed cost far above the estimate) and the provider
/// anomaly circuit (too many rejected / unknown attempts) stop sending.
#[tokio::test]
async fn cost_spike_and_provider_anomaly_circuits_open() {
    let Some(h) = Harness::build(|s| {
        s.controls.segment_cost_minor = 1;
        s.controls.cost_spike_factor = 3;
        s.controls.cost_spike_min_samples = 2;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    for to in ["+15550104271", "+15550104272"] {
        let id = h.send(&c, body(&n, to, "x"), &key()).await.json["messageId"]
            .as_str()
            .unwrap()
            .to_string();
        h.dispatch().await;
        // The provider confirms 4 per segment against an estimate of 1.
        h.receipt(&h.receipt_callback(&provider_ref(&h, &id), ReceiptStatus::Delivered, None))
            .await;
    }
    let r = h.send(&c, body(&n, "+15550104273", "x"), &key()).await;
    assert_eq!(r.reason(), "COST_SPIKE_CIRCUIT_OPEN", "{}", r.text);

    let Some(h) = Harness::build(|s| {
        s.controls.anomaly_min_attempts = 3;
        s.controls.anomaly_percent = 60;
    })
    .await
    else {
        return;
    };
    let (c, n) = setup(&h);
    h.provider.script(&[
        Script::Reject("INVALID_DESTINATION"),
        Script::Reject("INVALID_DESTINATION"),
        Script::Accept,
    ]);
    for to in ["+15550104281", "+15550104282", "+15550104283"] {
        h.send(&c, body(&n, to, "x"), &key()).await;
        h.dispatch().await;
    }
    let r = h.send(&c, body(&n, "+15550104284", "x"), &key()).await;
    assert_eq!(r.reason(), "PROVIDER_ANOMALY", "{}", r.text);
    // The window passes: the circuit closes again.
    h.advance(16 * 60);
    assert_eq!(
        h.send(&c, body(&n, "+15550104284", "x"), &key())
            .await
            .status,
        StatusCode::ACCEPTED
    );
}

/// Replies by thread (no number in the request) go to the thread's remote party from the
/// thread's own number only.
#[tokio::test]
async fn replies_by_thread_reach_the_threads_party_from_its_own_number() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    h.inbound(&h.inbound_callback(TO, DCN, "Is 3pm free?"))
        .await;
    let t = h.get(Some(&c.token), "/v1/sms/threads").await;
    let thread = t.json["threads"][0]["threadId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = h
        .send(&c, json!({ "from": { "numberResourceId": n.id }, "threadId": thread, "text": "Yes, 3pm works." }), &key())
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    assert_eq!(r.json["threadId"], json!(thread));
    h.dispatch().await;
    assert_eq!(h.provider.held()[0].to, TO);
    // Another number cannot reply in this thread; another environment cannot see it.
    let n2 = h.number(&c, "+15550109002", None);
    let r = h
        .send(
            &c,
            json!({ "from": { "numberResourceId": n2.id }, "threadId": thread, "text": "x" }),
            &key(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let stranger = h.customer(&["sms.send"]);
    let n3 = h.number(&stranger, "+15550109003", None);
    let r = h
        .send(
            &stranger,
            json!({ "from": { "numberResourceId": n3.id }, "threadId": thread, "text": "x" }),
            &key(),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let t = h
        .get(
            Some(&c.token),
            &format!("/v1/sms/threads/{thread}/messages"),
        )
        .await;
    assert_eq!(t.json["messages"].as_array().unwrap().len(), 2);
}

/// Sending needs sms.send and a provider; no provider = 503 and nothing recorded.
#[tokio::test]
async fn sending_needs_sms_send_and_a_configured_provider() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let reader = h.credential(&c, &["sms.read"]);
    let r = h.send(&reader, body(&n, TO, "x"), &key()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    let r = h
        .post(
            Some(&reader.token),
            "/v1/sms/opt-outs",
            json!({ "to": TO }),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = h
        .post(None, "/v1/sms/messages", body(&n, TO, "x"), Some(&key()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The same database and authorities, but no provider configured.
    let no_provider = Harness::on(h.db.clone(), |s| {
        s.provider = None;
        s.authority = h.authority.clone();
        s.numbering = h.numbering.clone();
    });
    let r = no_provider.send(&c, body(&n, TO, "x"), &key()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.code(), "PROVIDER_UNAVAILABLE");
    // Numbering unreachable: 503, nothing recorded.
    h.numbering.set_down(true);
    let r = h.send(&c, body(&n, TO, "x"), &key()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.code(), "NUMBERING_UNAVAILABLE");
    assert_eq!(count(&h, "SELECT count(*) FROM sms_transports").await, 0);
}
