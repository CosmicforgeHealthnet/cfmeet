//! X3 `authorizeWakeups` (`POST /internal/v1/notifications/wakeups/authorize`,
//! contract packages/contracts/messaging/notification-owner/v1.openapi.json): service
//! authorization, and PARITY with Messaging's own push path — for the same fixtures as
//! tests/notification_authorization.rs (real PostgreSQL, router, outbox, OpenMLS devices
//! and the recording push provider), the endpoint's entitled set equals the principals
//! that actually receive a push when the queued jobs are dispatched.
mod common;

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::{flows::*, realtime::*, *};
use messaging::{
    identity::{AuthorityUnavailable, OwnerScope, PrincipalDirectory, PrincipalStatus},
    realtime::push,
};
use serde_json::{json, Value};
use uuid::Uuid;

const ROUTE: &str = "/internal/v1/notifications/wakeups/authorize";

async fn authorize(h: &Harness, signer: &ServiceTokenSigner, body: Value) -> Response {
    h.call_internal(signer, Method::POST, ROUTE, Some(body), None, &[])
        .await
}

/// The notifications peer's answer for one event (200 asserted).
async fn wakeups(h: &Harness, body: Value) -> Value {
    let r = authorize(h, &peers().notifications, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    contract_shape(&r.json);
    r.json
}

/// The contract's response schema (`additionalProperties: false` throughout).
fn contract_shape(answer: &Value) {
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(answer),
        [
            "conversationActive",
            "conversationKind",
            "entitled",
            "securityProfile"
        ]
        .map(String::from)
        .into(),
        "{answer}"
    );
    assert!(answer["conversationActive"].is_boolean());
    assert!(["PRIVATE_E2EE", "MANAGED"].contains(&answer["securityProfile"].as_str().unwrap()));
    assert!(["DIRECT", "GROUP"].contains(&answer["conversationKind"].as_str().unwrap()));
    for e in answer["entitled"].as_array().unwrap() {
        assert_eq!(
            keys(e),
            ["accountRef", "mentionsRecipient", "principal"]
                .map(String::from)
                .into(),
            "{e}"
        );
        uuid(&e["principal"]);
        assert!(e["mentionsRecipient"].is_boolean());
        assert!(
            e["accountRef"].is_null()
                || messaging::notifications::account_ref(e["accountRef"].as_str().unwrap())
                    .is_some(),
            "{e}"
        );
    }
}

fn entitled(answer: &Value) -> BTreeSet<Uuid> {
    answer["entitled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| uuid(&e["principal"]))
        .collect()
}

fn handle_of(principal: Uuid) -> String {
    format!("wakeup-test-{principal}")
}

async fn register_push(h: &Harness, c: &Customer, m: &Member) {
    let r = h
        .put(
            c,
            Some(m.principal),
            &format!("/v1/messaging/devices/{}/push-registration", m.device_id()),
            json!({ "platform": "LOCAL_TEST", "handle": handle_of(m.principal) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// Waits until the committed event produced `n` push jobs.
async fn queued(h: &Harness, n: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while h.scalar("SELECT count(*) FROM messaging_push_jobs").await < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "committed event never produced {n} jobs"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The conversation event cursor of the latest created message of `conversation`.
async fn last_message_event(h: &Harness, conversation: &Value) -> i64 {
    h.scalar(&format!(
        "SELECT max(conversation_event_sequence) FROM messaging_outbox_events \
         WHERE conversation_id = '{}' AND event_type = 'messaging.message.created'",
        cid(conversation)
    ))
    .await
}

/// Dispatches every due job; returns the principals a push was actually sent to.
async fn delivered(rt: &Rt) -> BTreeSet<Uuid> {
    push::dispatch_due(&rt.h.state, 100, 3).await.unwrap();
    rt.push
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(handle, _)| Uuid::parse_str(handle.strip_prefix("wakeup-test-").unwrap()).unwrap())
        .collect()
}

fn body(conversation: &Value, sequence: i64, sender: Option<Uuid>) -> Value {
    json!({
        "conversationId": cid(conversation),
        "eventSequence": sequence,
        "senderPrincipal": sender.map(|s| s.to_string()).unwrap_or_default(),
    })
}

#[tokio::test]
async fn authorize_wakeups_requires_the_notification_service_permission() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conversation, people) = h.managed_group(&c, 2).await;
    let r = h.send_managed(&c, &conversation, people[0], "hello").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let sequence = last_message_event(&h, &conversation).await;
    let request = body(&conversation, sequence, Some(people[0]));
    let peers = peers();

    // No service token.
    let r = h
        .send(Method::POST, None, None, ROUTE, Some(request.clone()), &[])
        .await;
    assert_eq!(
        (r.status.as_u16(), r.code()),
        (401, "INTERNAL_AUTHENTICATION_FAILED"),
        "{}",
        r.text
    );
    // A customer credential is not a service identity.
    let r = h
        .send(
            Method::POST,
            Some(&c.token),
            Some(people[0]),
            ROUTE,
            Some(request.clone()),
            &[],
        )
        .await;
    assert_eq!(r.status.as_u16(), 401, "{}", r.text);
    // An untrusted issuer.
    let r = authorize(&h, &h.untrusted, request.clone()).await;
    assert_eq!(
        (r.status.as_u16(), r.code()),
        (401, "INTERNAL_AUTHENTICATION_FAILED"),
        "{}",
        r.text
    );
    // Trusted peers without `messaging-wakeups.authorize`.
    for (name, signer) in [
        ("meeting", &peers.meeting),
        ("communications", &peers.communications),
        ("gateway", &peers.gateway),
        ("sms_gateway", &peers.sms_gateway),
    ] {
        let r = authorize(&h, signer, request.clone()).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "INTERNAL_PERMISSION_DENIED"),
            "{name}: {}",
            r.text
        );
        assert!(r.json.get("entitled").is_none());
    }
    // The Notification Service holding it.
    let answer = wakeups(&h, request.clone()).await;
    assert_eq!(entitled(&answer), BTreeSet::from([people[1]]));
    // The token is single-use: a replay of the same signed request is refused.
    let signed = peers
        .notifications
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::messaging(),
        ))
        .unwrap();
    let mut statuses = Vec::new();
    for _ in 0..2 {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri(ROUTE)
            .header("content-type", "application/json");
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        let r = h
            .raw(b.body(Body::from(request.to_string())).unwrap())
            .await;
        statuses.push((r.status.as_u16(), r.code().to_string()));
    }
    assert_eq!(statuses[0].0, 200, "{statuses:?}");
    assert_eq!(
        statuses[1],
        (401, "INTERNAL_AUTHENTICATION_FAILED".to_string())
    );
}

#[tokio::test]
async fn unknown_conversation_or_event_is_not_found_and_input_is_validated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conversation, people) = h.managed_group(&c, 2).await;
    let r = h.send_managed(&c, &conversation, people[0], "hello").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let sequence = last_message_event(&h, &conversation).await;
    let notifications = peers().notifications;
    for request in [
        // Unknown conversation.
        json!({ "conversationId": Uuid::new_v4(), "eventSequence": sequence, "senderPrincipal": "" }),
        // Unknown event in a known conversation.
        body(&conversation, sequence + 1000, None),
        // A sender the event does not record.
        body(&conversation, sequence, Some(people[1])),
    ] {
        let r = authorize(&h, &notifications, request.clone()).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (404, "NOT_FOUND"),
            "{request}: {}",
            r.text
        );
        assert!(!r.text.contains(&people[1].to_string()));
        assert!(r.json.get("entitled").is_none());
    }
    for request in [
        body(&conversation, 0, None),
        json!({ "conversationId": cid(&conversation), "eventSequence": sequence, "senderPrincipal": "nobody" }),
        json!({ "conversationId": cid(&conversation), "eventSequence": sequence }),
        json!({ "conversationId": cid(&conversation), "eventSequence": sequence, "senderPrincipal": "", "extra": 1 }),
        json!({ "conversationId": cid(&conversation), "eventSequence": sequence, "senderPrincipal": "",
                "recheckPrincipals": (0..201).map(|_| Uuid::new_v4()).collect::<Vec<_>>() }),
    ] {
        let r = authorize(&h, &notifications, request.clone()).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (400, "VALIDATION_ERROR"),
            "{request}: {}",
            r.text
        );
    }
}

/// GROUP PRIVATE_E2EE: the sender and a removed member are excluded, a member who joined
/// after the event is outside its history window; the entitled set equals the principals
/// the queued pushes reach.
#[tokio::test]
async fn group_entitlement_matches_push_delivery() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut members) = h.e2ee_group(&c, 4).await;
    for m in &members {
        register_push(h, &c, m).await;
    }
    let dave = members.pop().unwrap();
    let carol = members.pop().unwrap();
    let bob = members.pop().unwrap();
    let mut alice = members.pop().unwrap();
    let sent = h
        .send_e2ee(&c, &conversation, &mut alice, "client-side test content")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    // One job per member device other than the sender's.
    queued(h, 3).await;
    let sequence = last_message_event(h, &conversation).await;
    let before = wakeups(h, body(&conversation, sequence, Some(alice.principal))).await;
    assert_eq!(
        entitled(&before),
        BTreeSet::from([bob.principal, carol.principal, dave.principal])
    );
    assert_eq!(before["conversationActive"], true);
    assert_eq!(before["securityProfile"], "PRIVATE_E2EE");
    assert_eq!(before["conversationKind"], "GROUP");
    for e in before["entitled"].as_array().unwrap() {
        // PRIVATE_E2EE mentions stay inside the ciphertext; not a governed first-party
        // scope, so no account reference.
        assert_eq!(e["mentionsRecipient"], false);
        assert_eq!(e["accountRef"], Value::Null);
    }
    // No content, ciphertext or message metadata in the answer.
    let text = before.to_string();
    for forbidden in [
        "ciphertext",
        "text",
        "clientMessageId",
        "deviceId",
        "sequence\"",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}: {text}");
    }

    // Dave leaves; Erin joins after the event (her history starts later).
    let r = h
        .send(
            Method::DELETE,
            Some(&c.token),
            Some(dave.principal),
            &format!(
                "/v1/conversations/{}/members/{}",
                cid(&conversation),
                dave.principal
            ),
            None,
            &[],
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let erin = h.member(&c, 2).await;
    let r = h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{}/members", cid(&conversation)),
            json!({ "principalId": erin.principal }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    register_push(h, &c, &erin).await;

    // The sender is derived from the event when the caller leaves it empty.
    let answer = wakeups(h, body(&conversation, sequence, None)).await;
    assert_eq!(
        entitled(&answer),
        BTreeSet::from([bob.principal, carol.principal])
    );
    assert_eq!(
        answer,
        wakeups(h, body(&conversation, sequence, Some(alice.principal))).await
    );
    // Dispatch-time recheck: restricted to the principals asked about.
    let recheck = wakeups(
        h,
        json!({
            "conversationId": cid(&conversation),
            "eventSequence": sequence,
            "senderPrincipal": alice.principal,
            "recheckPrincipals": [carol.principal, dave.principal, erin.principal, alice.principal, Uuid::new_v4()],
        }),
    )
    .await;
    assert_eq!(entitled(&recheck), BTreeSet::from([carol.principal]));
    let none = wakeups(
        h,
        json!({ "conversationId": cid(&conversation), "eventSequence": sequence,
                "senderPrincipal": "", "recheckPrincipals": [] }),
    )
    .await;
    assert!(entitled(&none).is_empty());

    // Parity: exactly the entitled principals receive the push.
    assert_eq!(delivered(&rt).await, entitled(&answer));
}

/// DIRECT: the peer is entitled until they block the sender; afterwards neither the
/// endpoint nor the queued push wakes them.
#[tokio::test]
async fn direct_block_removes_the_entitlement_like_the_push_path() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conversation, mut alice, bob) = h.e2ee_direct(&c).await;
    register_push(h, &c, &alice).await;
    register_push(h, &c, &bob).await;
    let sent = h
        .send_e2ee(&c, &conversation, &mut alice, "client-side test content")
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    queued(h, 1).await;
    let sequence = last_message_event(h, &conversation).await;
    let before = wakeups(h, body(&conversation, sequence, Some(alice.principal))).await;
    assert_eq!(entitled(&before), BTreeSet::from([bob.principal]));
    assert_eq!(before["conversationKind"], "DIRECT");
    let r = h
        .post(
            &c,
            Some(bob.principal),
            "/v1/messaging/blocks",
            json!({ "principalId": alice.principal }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let after = wakeups(h, body(&conversation, sequence, None)).await;
    assert!(entitled(&after).is_empty(), "{after}");
    assert_eq!(after["conversationActive"], true);
    assert_eq!(delivered(&rt).await, entitled(&after));
}

/// MANAGED: `mentionsRecipient` marks exactly the mentioned members; delivery parity.
/// A conversation that is no longer ACTIVE entitles nobody, on both paths.
#[tokio::test]
async fn managed_mentions_and_conversation_state_match_the_push_path() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    h.allow_managed(&c, "GROUP").await;
    let mut people = Vec::new();
    for _ in 0..3 {
        let m = h.member(&c, 0).await;
        register_push(h, &c, &m).await;
        people.push(m);
    }
    let r = h
        .create_conversation(
            &c,
            people[0].principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    for p in &people[1..] {
        let a = h
            .post(
                &c,
                Some(people[0].principal),
                &format!("/v1/conversations/{}/members", cid(&conversation)),
                json!({ "principalId": p.principal }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    let (alice, bob, carol) = (
        people[0].principal,
        people[1].principal,
        people[2].principal,
    );
    let r = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{}/messages", cid(&conversation)),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "for @bob", "mentionedPrincipalIds": [bob] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    queued(h, 2).await;
    let sequence = last_message_event(h, &conversation).await;
    let answer = wakeups(h, body(&conversation, sequence, Some(alice))).await;
    assert_eq!(answer["securityProfile"], "MANAGED");
    let flags: HashMap<Uuid, bool> = answer["entitled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                uuid(&e["principal"]),
                e["mentionsRecipient"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(flags, HashMap::from([(bob, true), (carol, false)]));
    assert!(!answer.to_string().contains("for @bob"));
    assert_eq!(delivered(&rt).await, entitled(&answer));

    // A second message, then the conversation stops being ACTIVE before dispatch.
    rt.push.sent.lock().unwrap().clear();
    let r = h.send_managed(&c, &conversation, alice, "second").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    queued(h, 4).await;
    let second = last_message_event(h, &conversation).await;
    h.sql(&format!(
        "UPDATE messaging_conversations SET lifecycle_state = 'LOCKED', version = version + 1 WHERE id = '{}'",
        cid(&conversation)
    ))
    .await
    .unwrap();
    let locked = wakeups(h, body(&conversation, second, None)).await;
    assert_eq!(locked["conversationActive"], false);
    assert!(entitled(&locked).is_empty());
    assert_eq!(delivered(&rt).await, entitled(&locked));
}

/// A principal directory that knows the account references of some principals.
struct Accounts {
    inner: Arc<FakePrincipals>,
    references: HashMap<Uuid, String>,
}

#[async_trait]
impl PrincipalDirectory for Accounts {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        self.inner.resolve(scope, principal_id).await
    }

    async fn native_user_references(
        &self,
        _scope: &OwnerScope,
        principals: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, AuthorityUnavailable> {
        if *self.inner.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        Ok(principals
            .iter()
            .filter_map(|p| self.references.get(p).map(|r| (*p, r.clone())))
            .collect())
    }
}

/// `accountRef`: the `cfmeet-user:<uuid>` reference Platform Access holds for a native CF
/// user, in the governed first-party personal scope only; `null` otherwise (and for any
/// malformed reference); the directory being down fails closed.
#[tokio::test]
async fn account_refs_come_from_native_user_references_in_the_personal_scope() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conversation, people) = h.managed_group(&c, 4).await;
    let r = h.send_managed(&c, &conversation, people[0], "hello").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let sequence = last_message_event(&h, &conversation).await;
    let account = Uuid::new_v4();
    h.state.principals = Arc::new(Accounts {
        inner: h.principals.clone(),
        references: HashMap::from([
            (people[1], format!("cfmeet-user:{account}")),
            (people[2], "acct:not-a-cf-meet-account".into()),
            (people[0], format!("cfmeet-user:{}", Uuid::new_v4())),
        ]),
    });
    let refs = |answer: &Value| -> HashMap<Uuid, Value> {
        answer["entitled"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (uuid(&e["principal"]), e["accountRef"].clone()))
            .collect()
    };
    // Not the governed personal scope: no account mapping.
    let answer = wakeups(&h, body(&conversation, sequence, None)).await;
    assert!(refs(&answer).values().all(Value::is_null), "{answer}");
    // CF Meet's personal scope.
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
    let answer = wakeups(&h, body(&conversation, sequence, None)).await;
    assert_eq!(
        refs(&answer),
        HashMap::from([
            (people[1], json!(format!("cfmeet-user:{account}"))),
            (people[2], Value::Null),
            (people[3], Value::Null),
        ])
    );
    *h.principals.down.lock().unwrap() = true;
    let r = authorize(
        &h,
        &peers().notifications,
        body(&conversation, sequence, None),
    )
    .await;
    assert_eq!(
        (r.status.as_u16(), r.code()),
        (503, "SERVICE_UNAVAILABLE"),
        "{}",
        r.text
    );
}

/// The route is the frozen contract's only operation, with its permission.
#[test]
fn the_route_is_the_contract_operation() {
    let doc: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/messaging/notification-owner/v1.openapi.json"
    ))
    .unwrap();
    let paths = doc["paths"].as_object().unwrap();
    assert_eq!(paths.keys().collect::<Vec<_>>(), vec![ROUTE]);
    let op = &paths[ROUTE]["post"];
    assert_eq!(op["operationId"], "authorizeWakeups");
    assert_eq!(
        op["x-cfmeet-internal-permission"],
        cf_service_auth::InternalPermission::MessagingWakeupsAuthorize.as_str()
    );
    assert_eq!(
        format!(
            "/internal/v1{}",
            messaging::notifications::AUTHORIZE_WAKEUPS
        ),
        ROUTE
    );
}
