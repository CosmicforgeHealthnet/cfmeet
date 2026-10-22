//! Part V §105 / §152: an external company's end-user device calls the Messaging REST API
//! DIRECTLY with its short-lived client token — member operations only, as its own
//! principal and device, revoked with its session or device.

mod common;

use axum::http::{Method, StatusCode};
use common::{flows::*, realtime::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

async fn with_token(
    h: &Harness,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Response {
    let auth = format!("CF-Client-Token {token}");
    let mut headers = vec![("authorization", auth.as_str())];
    headers.extend_from_slice(extra);
    h.send(method, None, None, path, body, &headers).await
}

#[tokio::test]
async fn end_user_devices_use_member_operations_directly_with_their_client_token() {
    let Some(rt) = Rt::start().await else { return };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let bob_device = Member {
        principal: bob,
        device: h.active_device(&c, bob).await,
        group: None,
        cursor: 0,
    };
    let bob_other = h.active_device(&c, bob).await.device_id();
    let issued = h
        .post(
            &c,
            Some(bob),
            "/v1/messaging/client-tokens",
            json!({ "deviceId": bob_device.device_id() }),
        )
        .await;
    assert_eq!(issued.status, StatusCode::CREATED, "{}", issued.text);
    let token = issued.json["token"].as_str().unwrap().to_string();
    let session = issued.json["sessionId"].as_str().unwrap().to_string();

    // Member operations, as the token's own principal (no backend credential involved).
    let msgs = format!("/v1/conversations/{cid}/messages");
    let sent = with_token(
        h,
        &token,
        Method::POST,
        &msgs,
        Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "from my phone" })),
        &[],
    )
    .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert_eq!(sent.json["senderPrincipalId"], json!(bob));
    let list = with_token(h, &token, Method::GET, &msgs, None, &[]).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    assert!(list.text.contains("from my phone"));
    let seq = sent.json["sequence"].as_i64().unwrap();
    let r = with_token(
        h,
        &token,
        Method::PUT,
        &format!("/v1/conversations/{cid}/read-cursor"),
        Some(json!({ "sequence": seq })),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = with_token(h, &token, Method::GET, "/v1/conversations", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.text.contains(&cid));
    let own = format!("/v1/messaging/devices/{}/welcomes", bob_device.device_id());
    assert_eq!(
        with_token(h, &token, Method::GET, &own, None, &[])
            .await
            .status,
        StatusCode::OK
    );

    // Another device of the same principal is not this token's.
    let other = format!("/v1/messaging/devices/{bob_other}/welcomes");
    assert_eq!(
        with_token(h, &token, Method::GET, &other, None, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // The token acts only for its principal.
    let alice_s = alice.to_string();
    let r = with_token(
        h,
        &token,
        Method::GET,
        &msgs,
        None,
        &[("cf-acting-principal", alice_s.as_str())],
    )
    .await;
    assert_eq!(r.reason(), "ACTING_PRINCIPAL_MISMATCH", "{}", r.text);
    // A conversation it is not a member of, and another tenant's, look absent.
    let other_conv = h
        .create_conversation(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    let ocid = other_conv.json["conversationId"].as_str().unwrap();
    assert_eq!(
        with_token(
            h,
            &token,
            Method::GET,
            &format!("/v1/conversations/{ocid}/messages"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let t2 = h.customer(ALL_SCOPES);
    let (foreign, _) = h.managed_group(&t2, 1).await;
    let fcid = foreign["conversationId"].as_str().unwrap();
    assert_eq!(
        with_token(
            h,
            &token,
            Method::GET,
            &format!("/v1/conversations/{fcid}/messages"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    // Everything outside the member allow-list stays with the backend credential.
    for (method, path, body) in [
        (
            Method::POST,
            "/v1/conversations".to_string(),
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        ),
        (
            Method::POST,
            format!("/v1/conversations/{cid}/members"),
            Some(json!({ "principalId": Uuid::new_v4() })),
        ),
        (Method::GET, "/v1/messaging/policy".to_string(), None),
        (
            Method::POST,
            "/v1/messaging/client-tokens".to_string(),
            Some(json!({ "deviceId": bob_device.device_id() })),
        ),
        (
            Method::POST,
            format!("/v1/messaging/devices/{}/revoke", bob_device.device_id()),
            Some(json!({ "reason": "LOST_OR_STOLEN" })),
        ),
        (
            Method::POST,
            "/v1/messaging/exports".to_string(),
            Some(json!({ "conversationId": cid })),
        ),
        (
            Method::POST,
            "/v1/messaging/legal-holds".to_string(),
            Some(json!({ "scopeKind": "ENVIRONMENT", "reasonCategory": "LITIGATION" })),
        ),
    ] {
        let r = with_token(h, &token, method.clone(), &path, body, &[]).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "CLIENT_TOKENS_NOT_ENABLED"),
            "{method} {path}: {}",
            r.text
        );
    }
    let forged = with_token(h, "abc.def.ghi", Method::GET, &msgs, None, &[]).await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);

    // Revoking the session ends REST access at once.
    let r = h
        .post(
            &c,
            Some(bob),
            &format!("/v1/messaging/client-sessions/{session}/revoke"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let after = with_token(h, &token, Method::GET, &msgs, None, &[]).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED, "{}", after.text);

    // A revoked device's tokens are refused too.
    let token2 = rt.token(&c, &bob_device).await;
    assert_eq!(
        with_token(h, &token2, Method::GET, &msgs, None, &[])
            .await
            .status,
        StatusCode::OK
    );
    let r = h
        .post(
            &c,
            Some(bob),
            &format!("/v1/messaging/devices/{}/revoke", bob_device.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        with_token(h, &token2, Method::GET, &msgs, None, &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Nothing a refused request carried was committed.
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND sender_principal_id = '{bob}'")).await,
        1
    );
}

/// The published contract and the server's allow-list agree exactly: an operation lists
/// `clientToken` in its security if and only if the server accepts client tokens there.
#[test]
fn the_contract_documents_exactly_the_client_token_routes() {
    let doc: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/messaging/http/v1.openapi.json"
    ))
    .unwrap();
    let mut documented = 0;
    for (path, ops) in doc["paths"].as_object().unwrap() {
        if path == "/v1/realtime/tickets" {
            continue; // authenticates its own client token (not a REST member operation)
        }
        for (method, op) in ops.as_object().unwrap() {
            let Ok(m) = method.to_uppercase().parse::<Method>() else {
                continue;
            };
            let lists = op["security"]
                .as_array()
                .is_some_and(|s| s.iter().any(|r| r.get("clientToken").is_some()));
            let accepts = messaging::http::middleware::client_token_route(&m, path).is_some();
            assert_eq!(lists, accepts, "{method} {path}");
            documented += usize::from(lists);
        }
    }
    assert!(documented >= 50, "{documented}");
}
