//! Part V H §§106–112 — the SMS Bridge, transport side: bindings, bridged sends under
//! every customer-send control, separate transport state, and durable inbound pushes.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPlatformContext;
use common::{Customer, FakeNumber, Harness};
use serde_json::{json, Value};
use sms_gateway::sms::bridge::{self, BridgeInbound, PushFailure};
use uuid::Uuid;

const PARTY: &str = "+15550123456";
const OTHER: &str = "+15550129999";
const DCN: &str = "+15550100001";

fn setup(h: &Harness) -> (Customer, FakeNumber, DelegatedPlatformContext) {
    let c = h.customer(&["sms.send", "sms.read"]);
    let n = h.number(&c, DCN, Some(("APPLICATION", c.context.application_id)));
    let d = c.context.owner_scope().delegated(None);
    (c, n, d)
}

async fn bind(
    h: &Harness,
    d: &DelegatedPlatformContext,
    n: &FakeNumber,
    bridge_ref: Uuid,
    to: &str,
) -> common::Response {
    h.internal_call(
        &h.peers.messaging,
        Method::POST,
        "/internal/v1/sms-bridge/bindings",
        Some(json!({ "bridgeRef": bridge_ref, "numberResourceId": n.id, "to": to })),
        Some(d.clone()),
    )
    .await
}

async fn submit(
    h: &Harness,
    d: &DelegatedPlatformContext,
    bridge_ref: Uuid,
    message: Uuid,
    text: &str,
) -> common::Response {
    h.internal_call(
        &h.peers.messaging,
        Method::POST,
        "/internal/v1/sms-bridge/messages",
        Some(json!({ "bridgeRef": bridge_ref, "messageRef": message, "text": text })),
        Some(d.clone()),
    )
    .await
}

#[tokio::test]
async fn bridged_sends_run_every_transport_control_and_keep_their_own_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (_c, n, d) = setup(&h);
    let r_ref = Uuid::new_v4();
    let r = bind(&h, &d, &n, r_ref, PARTY).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(
        !r.text.contains("0123456"),
        "the party's number is never returned: {}",
        r.text
    );
    assert!(r.json["remote"]["masked"].as_str().unwrap().len() < PARTY.len());
    let again = bind(&h, &d, &n, r_ref, PARTY).await;
    assert_eq!(
        (again.status, &again.json["bindingId"]),
        (StatusCode::OK, &r.json["bindingId"])
    );
    // One ACTIVE bridge per party and number.
    let dup = bind(&h, &d, &n, Uuid::new_v4(), PARTY).await;
    assert_eq!(
        dup.json["error"]["reason"], "PARTY_ALREADY_BRIDGED",
        "{}",
        dup.text
    );
    // The number stays sealed at rest.
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_bridge_bindings WHERE remote_sealed LIKE '%0123456%'")
            .await,
        0
    );

    // A bridged message is an ordinary, idempotent send.
    let m = Uuid::new_v4();
    let s = submit(&h, &d, r_ref, m, "hello from the conversation").await;
    assert_eq!(s.status, StatusCode::ACCEPTED, "{}", s.text);
    assert_eq!(s.json["status"], "AUTHORIZED");
    assert!(
        s.json.get("content").is_none() && !s.text.contains("hello"),
        "{}",
        s.text
    );
    let replay = submit(&h, &d, r_ref, m, "hello from the conversation").await;
    assert_eq!(
        (replay.status, &replay.json["smsMessageId"]),
        (StatusCode::OK, &s.json["smsMessageId"])
    );
    h.dispatch().await;
    let id = s.json["smsMessageId"].as_str().unwrap();
    let st = h
        .internal_call(
            &h.peers.messaging,
            Method::GET,
            &format!("/internal/v1/sms-bridge/messages/{id}"),
            None,
            Some(d.clone()),
        )
        .await;
    assert_eq!(st.status, StatusCode::OK, "{}", st.text);
    assert_eq!(st.json["status"], "SUBMITTED", "{}", st.text);
    assert!(st.json["cost"].is_object());

    // The party opts out by STOP: the next bridged send is refused and recorded, never sent.
    h.inbound(&h.inbound_callback(PARTY, DCN, "STOP")).await;
    let blocked = submit(&h, &d, r_ref, Uuid::new_v4(), "are you there?").await;
    assert_eq!(
        blocked.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        blocked.text
    );
    assert_eq!(blocked.json["error"]["code"], "SMS_BLOCKED");
    assert_eq!(blocked.json["error"]["reason"], "RECIPIENT_OPTED_OUT");

    // Authorization: delegated scope required; untrusted peers refused; other scopes blind.
    let r = h
        .internal_call(
            &h.peers.messaging,
            Method::POST,
            "/internal/v1/sms-bridge/messages",
            Some(json!({ "bridgeRef": r_ref, "messageRef": Uuid::new_v4(), "text": "x" })),
            None,
        )
        .await;
    assert_eq!(
        r.json["error"]["code"], "DELEGATED_CONTEXT_REQUIRED",
        "{}",
        r.text
    );
    let r = h
        .internal_call(
            &h.peers.telephony,
            Method::POST,
            "/internal/v1/sms-bridge/messages",
            Some(json!({ "bridgeRef": r_ref, "messageRef": Uuid::new_v4(), "text": "x" })),
            Some(d.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let foreign = h
        .customer(&["sms.send"])
        .context
        .owner_scope()
        .delegated(None);
    let r = submit(&h, &foreign, r_ref, Uuid::new_v4(), "x").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // A closed bridge sends nothing more.
    let r = h
        .internal_call(
            &h.peers.messaging,
            Method::POST,
            &format!("/internal/v1/sms-bridge/bindings/{r_ref}/close"),
            None,
            Some(d.clone()),
        )
        .await;
    assert_eq!(r.json["status"], "CLOSED", "{}", r.text);
    let r = submit(&h, &d, r_ref, Uuid::new_v4(), "after close").await;
    assert_eq!(r.json["error"]["reason"], "BRIDGE_CLOSED", "{}", r.text);
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<(Value, Value)>>,
    fail: Mutex<Option<PushFailure>>,
}

#[async_trait]
impl BridgeInbound for Recorder {
    async fn deliver(
        &self,
        delegated: &DelegatedPlatformContext,
        body: &Value,
    ) -> Result<(), PushFailure> {
        if let Some(f) = *self.fail.lock().unwrap() {
            return Err(f);
        }
        self.seen
            .lock()
            .unwrap()
            .push((serde_json::to_value(delegated).unwrap(), body.clone()));
        Ok(())
    }
}

#[tokio::test]
async fn inbound_sms_of_a_bridged_party_reach_messaging_durably() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n, d) = setup(&h);
    let r_ref = Uuid::new_v4();
    bind(&h, &d, &n, r_ref, PARTY).await;
    let r = h
        .inbound(&h.inbound_callback(PARTY, DCN, "running 10 minutes late"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Another party of the same number is not bridged: it stays an ordinary inbound SMS.
    h.inbound(&h.inbound_callback(OTHER, DCN, "unrelated"))
        .await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_bridge_deliveries WHERE status = 'PENDING'")
            .await,
        1
    );
    let rec = Arc::new(Recorder::default());
    // Messaging down: retried with backoff, nothing lost.
    *rec.fail.lock().unwrap() = Some(PushFailure::Unavailable);
    assert_eq!(
        bridge::deliver_due(&h.state, rec.as_ref(), 10)
            .await
            .unwrap(),
        0
    );
    assert_eq!(h.scalar("SELECT count(*) FROM sms_bridge_deliveries WHERE status = 'PENDING' AND attempt_count = 1 AND last_error = 'UNAVAILABLE'").await, 1);
    *rec.fail.lock().unwrap() = None;
    h.sql("UPDATE sms_bridge_deliveries SET next_attempt_at = now() - interval '1 hour'")
        .await
        .unwrap();
    assert_eq!(
        bridge::deliver_due(&h.state, rec.as_ref(), 10)
            .await
            .unwrap(),
        1
    );
    let seen = rec.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (delegated, body) = &seen[0];
    assert_eq!(
        delegated["application_environment_id"],
        json!(c.context.environment_id)
    );
    assert_eq!(body["bridgeRef"], json!(r_ref));
    assert_eq!(body["text"], "running 10 minutes late");
    assert!(
        !body.to_string().contains("0123456"),
        "no number crosses to Messaging: {body}"
    );
    // Delivered once: a later pass sends nothing again.
    assert_eq!(
        bridge::deliver_due(&h.state, rec.as_ref(), 10)
            .await
            .unwrap(),
        0
    );
    // A permanent refusal dead-letters (no endless retry).
    h.inbound(&h.inbound_callback(PARTY, DCN, "second")).await;
    *rec.fail.lock().unwrap() = Some(PushFailure::Rejected);
    bridge::deliver_due(&h.state, rec.as_ref(), 10)
        .await
        .unwrap();
    assert_eq!(
        h.scalar("SELECT count(*) FROM sms_bridge_deliveries WHERE status = 'DEAD_LETTER'")
            .await,
        1
    );
    // Unconfigured Messaging: stays durably PENDING.
    *rec.fail.lock().unwrap() = None;
    h.inbound(&h.inbound_callback(PARTY, DCN, "third")).await;
    bridge::deliver_due(&h.state, &bridge::UnconfiguredBridgeInbound, 10)
        .await
        .unwrap();
    assert_eq!(h.scalar("SELECT count(*) FROM sms_bridge_deliveries WHERE status = 'PENDING' AND last_error = 'NOT_CONFIGURED'").await, 1);
}
