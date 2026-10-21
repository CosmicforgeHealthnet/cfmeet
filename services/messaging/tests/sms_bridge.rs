//! Part V H §§106–112 — the SMS Bridge, conversation side, against a scripted SMS
//! Gateway: explicit disclosure, never an E2EE downgrade, outbound transport state kept
//! apart from in-app state, transport refusals that never disturb in-app messaging, and
//! inbound SMS as MANAGED `SMS` messages.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPlatformContext;
use common::{flows::ALL_SCOPES, *};
use messaging::sms_bridge::{self, SmsTransport, TransportError, DISCLOSURE_VERSION};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Default)]
struct ScriptedGateway {
    calls: Mutex<Vec<(Method, String, Option<Value>)>>,
    /// Next submissions refused with this reason (opt-out, spend, ...).
    refuse: Mutex<Option<&'static str>>,
    down: Mutex<bool>,
}

#[async_trait]
impl SmsTransport for ScriptedGateway {
    async fn call(
        &self,
        _: &DelegatedPlatformContext,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), TransportError> {
        if *self.down.lock().unwrap() {
            return Err(TransportError::Unavailable);
        }
        self.calls
            .lock()
            .unwrap()
            .push((method.clone(), path.to_string(), body.cloned()));
        if path == "/internal/v1/sms-bridge/bindings" {
            return Ok((
                201,
                json!({ "bindingId": Uuid::new_v4(), "remote": { "masked": "+*******3456" }, "status": "ACTIVE" }),
            ));
        }
        if path.ends_with("/close") {
            return Ok((200, json!({ "status": "CLOSED" })));
        }
        if path == "/internal/v1/sms-bridge/messages" {
            if let Some(reason) = *self.refuse.lock().unwrap() {
                return Err(TransportError::Refused(
                    422,
                    "SMS_BLOCKED".into(),
                    Some(reason.into()),
                ));
            }
            return Ok((
                202,
                json!({ "smsMessageId": Uuid::new_v4(), "status": "AUTHORIZED", "segmentCount": 1 }),
            ));
        }
        // Status of a submitted message: the carrier confirmed delivery.
        Ok((200, json!({ "status": "DELIVERED", "segmentCount": 1 })))
    }
}

async fn harness() -> Option<(Harness, Arc<ScriptedGateway>)> {
    let gw = Arc::new(ScriptedGateway::default());
    let g = gw.clone();
    let h = Harness::build(move |s| s.sms = g).await?;
    Some((h, gw))
}

fn bridge_body(number: Uuid, party: Uuid) -> Value {
    json!({ "numberResourceId": number, "to": "+15550123456", "externalPrincipalId": party,
            "disclosureVersion": DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true })
}

#[tokio::test]
async fn a_managed_conversation_bridges_to_sms_with_explicit_disclosure_and_separate_transport_state(
) {
    let Some((h, gw)) = harness().await else {
        return;
    };
    // An EXTERNAL tenant (its scope is also valid as a delegated context for inbound SMS).
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, member) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let party = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    let number = Uuid::new_v4();
    let path = format!("/v1/conversations/{cid}/sms-bridges");

    // The disclosure is explicit and versioned.
    let d = h.get(&c, None, "/v1/messaging/sms-bridge/disclosure").await;
    assert_eq!(d.json["version"], DISCLOSURE_VERSION, "{}", d.text);
    assert_eq!(d.json["encryption"]["endToEnd"], false);
    let mut unacknowledged = bridge_body(number, party);
    unacknowledged["smsDisclosureAcknowledged"] = json!(false);
    assert_eq!(
        h.post(&c, Some(owner), &path, unacknowledged)
            .await
            .reason(),
        "SMS_DISCLOSURE_REQUIRED"
    );
    let mut stale = bridge_body(number, party);
    stale["disclosureVersion"] = json!("sms-bridge-2020-01");
    assert_eq!(
        h.post(&c, Some(owner), &path, stale).await.reason(),
        "SMS_DISCLOSURE_REQUIRED"
    );
    // Only administrators bridge; the party must be a telephone endpoint.
    let r = h
        .post(&c, Some(member), &path, bridge_body(number, party))
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(&c, Some(owner), &path, bridge_body(number, member))
        .await;
    assert_eq!(
        r.reason(),
        "EXTERNAL_PARTY_NOT_A_TELEPHONE_ENDPOINT",
        "{}",
        r.text
    );

    let r = h
        .post(&c, Some(owner), &path, bridge_body(number, party))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let bridge = r.json["bridgeId"].as_str().unwrap().to_string();
    assert_eq!(
        (
            r.json["securityProfile"].as_str(),
            r.json["encryption"]["endToEnd"].as_bool()
        ),
        (Some("MANAGED"), Some(false))
    );
    assert!(!r.text.contains("0123456"), "{}", r.text);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_sms_bridges b WHERE b::text LIKE '%0123456%'")
            .await,
        0,
        "the number is never stored"
    );
    assert_eq!(
        h.post(&c, Some(owner), &path, bridge_body(number, party))
            .await
            .reason(),
        "BRIDGE_EXISTS"
    );

    // Outbound: a member's message is queued, submitted, then followed to delivery.
    let sent = h.send_managed(&c, &conv, member, "see you at 3").await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = sent.json["messageId"].as_str().unwrap().to_string();
    assert_eq!(sent.json["channel"], "IN_APP");
    let tpath = format!("/v1/conversations/{cid}/messages/{mid}/sms-transport");
    assert_eq!(
        h.get(&c, Some(owner), &tpath).await.json["transportStatus"],
        "QUEUED"
    );
    assert_eq!(sms_bridge::run_due(&h.state, 10).await.unwrap(), 1);
    let submitted = gw
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p, _)| p == "/internal/v1/sms-bridge/messages")
        .cloned()
        .unwrap();
    assert_eq!(submitted.2.as_ref().unwrap()["text"], "see you at 3");
    assert_eq!(submitted.2.as_ref().unwrap()["bridgeRef"], json!(bridge));
    assert_eq!(
        h.get(&c, Some(owner), &tpath).await.json["transportStatus"],
        "AUTHORIZED"
    );
    h.clock.advance(chrono::Duration::hours(1));
    sms_bridge::run_due(&h.state, 10).await.unwrap();
    let t = h.get(&c, Some(owner), &tpath).await;
    assert_eq!(
        (
            t.json["transportStatus"].as_str(),
            t.json["inAppStateIndependent"].as_bool()
        ),
        (Some("DELIVERED"), Some(true)),
        "{}",
        t.text
    );
    // Metered once, as segments only (no price).
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_usage_records WHERE usage_type = 'SMS_BRIDGED' AND quantity = 1")
            .await,
        1
    );

    // A transport refusal (the party opted out) never disturbs the in-app message.
    *gw.refuse.lock().unwrap() = Some("RECIPIENT_OPTED_OUT");
    let blocked = h.send_managed(&c, &conv, owner, "still there?").await;
    let bmid = blocked.json["messageId"].as_str().unwrap().to_string();
    sms_bridge::run_due(&h.state, 10).await.unwrap();
    let t = h
        .get(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/messages/{bmid}/sms-transport"),
        )
        .await;
    assert_eq!(
        (
            t.json["transportStatus"].as_str(),
            t.json["blockedReason"].as_str()
        ),
        (Some("BLOCKED"), Some("RECIPIENT_OPTED_OUT"))
    );
    let list = h
        .get(
            &c,
            Some(member),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    assert!(list.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["messageId"] == json!(bmid) && m["lifecycle"] == "ACTIVE"));
    *gw.refuse.lock().unwrap() = None;

    // Inbound: the party's SMS becomes a MANAGED SMS message (idempotent).
    let sms_id = Uuid::new_v4();
    let inbound = json!({ "bridgeRef": bridge, "smsMessageId": sms_id, "text": "running late", "segmentCount": 1 });
    let peers = peers();
    let r = h
        .call_internal(
            &peers.sms_gateway,
            Method::POST,
            "/internal/v1/sms-bridge/inbound",
            Some(inbound.clone()),
            Some(c.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (
            r.json["channel"].as_str(),
            r.json["securityProfile"].as_str()
        ),
        (Some("SMS"), Some("MANAGED"))
    );
    let again = h
        .call_internal(
            &peers.sms_gateway,
            Method::POST,
            "/internal/v1/sms-bridge/inbound",
            Some(inbound.clone()),
            Some(c.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(
        again.json["messageId"], r.json["messageId"],
        "idempotent per SMS id"
    );
    let list = h
        .get(
            &c,
            Some(member),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    let sms = list.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["channel"] == "SMS")
        .cloned()
        .unwrap();
    assert_eq!(
        (
            sms["senderPrincipalId"].clone(),
            sms["managed"]["text"].clone()
        ),
        (json!(party), json!("running late"))
    );
    // The party's own message is never sent back to it by SMS.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_sms_transports WHERE direction = 'OUTBOUND'")
            .await,
        2
    );
    // Only the SMS Gateway may deliver inbound SMS.
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            "/internal/v1/sms-bridge/inbound",
            Some(inbound),
            Some(c.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    // Closing: no more SMS either way.
    let r = h
        .post(
            &c,
            Some(owner),
            &format!("{path}/{bridge}/close"),
            json!({}),
        )
        .await;
    assert_eq!(r.json["status"], "CLOSED", "{}", r.text);
    let late = json!({ "bridgeRef": bridge, "smsMessageId": Uuid::new_v4(), "text": "hello?" });
    let r = h
        .call_internal(
            &peers.sms_gateway,
            Method::POST,
            "/internal/v1/sms-bridge/inbound",
            Some(late),
            Some(c.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(r.reason(), "BRIDGE_CLOSED", "{}", r.text);
    h.send_managed(&c, &conv, owner, "after close").await;
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_sms_transports WHERE direction = 'OUTBOUND'")
            .await,
        2
    );
    // §118 / Appendix D: the transitions above recorded their domain events, and every
    // event conforms to the published event contract.
    let types = h.conforming_event_types().await;
    assert!(
        types.contains("messaging.message.sms-transport-changed"),
        "sms-transport-changed not recorded: {types:?}"
    );
}

#[tokio::test]
async fn private_e2ee_conversations_are_never_bridged_or_fed_carrier_plaintext() {
    let Some((h, _gw)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, members) = h.e2ee_group(&c, 2).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let party = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    let r = h
        .post(
            &c,
            Some(members[0].principal),
            &format!("/v1/conversations/{cid}/sms-bridges"),
            bridge_body(Uuid::new_v4(), party),
        )
        .await;
    assert_eq!(r.reason(), "E2EE_CONVERSATION_NOT_BRIDGEABLE", "{}", r.text);
    // The database refuses it too, whatever code path tries.
    let refused = h
        .sql(&format!(
            "INSERT INTO messaging_sms_bridges (id, tenant_id, application_id, environment_id, environment_kind, service_account_id, \
               conversation_id, number_resource_id, external_principal_id, remote_masked, disclosure_version, created_by_principal_id, created_at) \
             SELECT gen_random_uuid(), tenant_id, application_id, environment_id, 'SANDBOX', gen_random_uuid(), id, gen_random_uuid(), \
               gen_random_uuid(), '***', '{DISCLOSURE_VERSION}', gen_random_uuid(), now() FROM messaging_conversations WHERE id = '{cid}'"
        ))
        .await;
    assert!(refused.unwrap_err().contains("never an E2EE downgrade"));
    // And an SMS-channel message can only ever be MANAGED.
    let n = h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND security_profile = 'PRIVATE_E2EE'")).await;
    if n > 0 {
        assert!(h
            .sql(&format!(
                "UPDATE messaging_messages SET channel = 'SMS' WHERE conversation_id = '{cid}'"
            ))
            .await
            .is_err());
    }
}

#[tokio::test]
async fn an_unreachable_or_unconfigured_sms_gateway_never_affects_in_app_messaging() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let party = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    let r = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/sms-bridges"),
            bridge_body(Uuid::new_v4(), party),
        )
        .await;
    assert_eq!(r.reason(), "SMS_BRIDGE_NOT_CONFIGURED", "{}", r.text);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_sms_bridges").await,
        0,
        "nothing half-created"
    );
    let sent = h.send_managed(&c, &conv, people[1], "in-app only").await;
    assert_eq!(sent.status, StatusCode::CREATED);
}

/// §28 / §108: SMS Bridge eligibility is part of the context policy. A MANAGED context is
/// eligible by default; a tenant that opts the context out gets no bridge, and an
/// E2EE-only context can never be made eligible.
#[tokio::test]
async fn sms_bridge_eligibility_follows_the_context_policy() {
    let Some((h, _gw)) = harness().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let owner = people[0];
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let party = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    let effective = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(
        effective.json["effective"]["contexts"]["GROUP"]["smsBridgeEligible"], true,
        "{}",
        effective.text
    );
    assert_eq!(
        effective.json["effective"]["contexts"]["DIRECT"]["smsBridgeEligible"],
        false
    );
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED",
                                         "smsBridgeEligible": false } } }),
    )
    .await;
    let r = h
        .post(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/sms-bridges"),
            bridge_body(Uuid::new_v4(), party),
        )
        .await;
    assert_eq!(r.reason(), "SMS_BRIDGE_NOT_ELIGIBLE", "{}", r.text);
    let v = h.get(&c, None, "/v1/messaging/policy").await;
    let bad = h
        .put(&c, None, "/v1/messaging/policy",
             json!({ "expectedVersion": v.json["version"], "policy": { "contexts": { "DIRECT": { "smsBridgeEligible": true } } } }))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text);
}
