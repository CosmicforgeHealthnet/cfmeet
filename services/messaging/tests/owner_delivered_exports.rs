//! Owner-delivered exports (ADR 0068, admin bridge final round, brief §1):
//! `POST /internal/v1/operator/exports` (`messaging.export.request`, R4) registers a
//! MANAGED conversation export for a verified recipient (a CF Meet account principal who
//! is a member); the existing worker builds it; only that recipient, signed in to their
//! own CF Meet console and relayed by a FIRST_PARTY backend on the delegated mirror, sees
//! it (`GET /internal/v1/delegated/v1/messaging/export-deliveries`) and claims it ONCE
//! (`POST /internal/v1/delegated/v1/messaging/exports/{exportId}/claim`). The tenant's
//! routes never see it; HQ gets references only
//! (`GET /internal/v1/operator/exports/{exportId}`).

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_object_storage::MemoryObjectStore;
use common::*;
use messaging::identity::PrincipalStatus;
use serde_json::{json, Value};
use uuid::Uuid;

const OP: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVAL: [(&str, &str); 3] = [
    ("x-cf-approver", "hq:staff-0002"),
    ("x-cf-approval-ref", "appr-0001"),
    (
        "x-cf-intent-digest",
        "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
    ),
];

async fn command(h: &Harness, body: Value, key: Uuid, approved: bool) -> Response {
    let peers = peers();
    let key = key.to_string();
    let mut headers: Vec<(&str, &str)> = vec![OP, ("idempotency-key", key.as_str())];
    if approved {
        headers.extend(APPROVAL);
    }
    h.call_internal(
        &peers.admin_bridge,
        Method::POST,
        "/internal/v1/operator/exports",
        Some(body),
        None,
        &headers,
    )
    .await
}

async fn read(h: &Harness, path: &str) -> Response {
    let peers = peers();
    h.call_internal(&peers.admin_bridge, Method::GET, path, None, None, &[OP])
        .await
}

/// A CF Meet account principal (NATIVE_CF_USER with its account reference).
fn account(h: &Harness, c: &Customer) -> Uuid {
    let p = h.principal_of_kind(c, "NATIVE_CF_USER");
    h.principals.native.lock().unwrap().insert(
        (c.context.scope.environment_id, p),
        format!("cfmeet-user:{}", Uuid::new_v4()),
    );
    p
}

/// The recipient's console: the Meeting Service relaying `acting` on the delegated mirror
/// of `c`'s scope.
async fn console(h: &Harness, c: &Customer, acting: Uuid, method: Method, path: &str) -> Response {
    let peers = peers();
    let acting = acting.to_string();
    let body = (method == Method::POST).then(|| json!({}));
    h.call_internal(
        &peers.meeting,
        method,
        path,
        body,
        Some(c.context.scope.delegated()),
        &[("cf-acting-principal", acting.as_str())],
    )
    .await
}

const DELIVERIES: &str = "/internal/v1/delegated/v1/messaging/export-deliveries";

#[tokio::test]
async fn an_owner_delivered_export_reaches_only_its_recipient_once() {
    let store = Arc::new(MemoryObjectStore::new());
    let s2 = store.clone();
    let Some(h) = Harness::build(move |s| s.storage = s2).await else {
        return;
    };
    let c = h.first_party(true);
    let tenant = c.context.scope.tenant_id;
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"],
                 "defaultProfile": "MANAGED", "exportsEnabled": true } } }),
    )
    .await;
    let recipient = account(&h, &c);
    let other = h.principal(&c);
    let outsider = account(&h, &c);
    let conv = h
        .create_conversation(
            &c,
            recipient,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(conv.status, StatusCode::CREATED, "{}", conv.text);
    let conv = conv.json;
    let cid = uuid(&conv["conversationId"]);
    let added = h
        .post(
            &c,
            Some(recipient),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": other }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);
    h.send_managed(&c, &conv, recipient, "mine to export").await;
    let body = |p: Uuid| {
        json!({ "tenantId": tenant, "conversationId": cid, "recipientPrincipalId": p,
                "reasonCode": "DATA_SUBJECT_REQUEST" })
    };
    let rows = || h.scalar("SELECT count(*) FROM messaging_exports");

    // Refusals change nothing.
    let r = command(&h, body(recipient), Uuid::new_v4(), false).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::FORBIDDEN, "SECOND_PERSON_REQUIRED"),
        "{}",
        r.text
    );
    let peers = peers();
    let r = h
        .call_internal(
            &peers.admin_bridge,
            Method::POST,
            "/internal/v1/operator/exports",
            Some(body(recipient)),
            None,
            &[OP, APPROVAL[0], APPROVAL[1], APPROVAL[2]],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = command(&h, body(other), Uuid::new_v4(), true).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::CONFLICT, "RECIPIENT_NOT_VERIFIED"),
        "{}",
        r.text
    );
    let r = command(&h, body(outsider), Uuid::new_v4(), true).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::CONFLICT, "RECIPIENT_NOT_A_MEMBER"),
        "{}",
        r.text
    );
    let mut wrong = body(recipient);
    wrong["tenantId"] = json!(Uuid::new_v4());
    let r = command(&h, wrong, Uuid::new_v4(), true).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let mut extra = body(recipient);
    extra["email"] = json!("x");
    let r = command(&h, extra, Uuid::new_v4(), true).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // An end-to-end encrypted conversation is never exported by the server.
    let (e2ee, a, _b) = h.e2ee_direct(&c).await;
    h.principals.native.lock().unwrap().insert(
        (c.context.scope.environment_id, a.principal),
        format!("cfmeet-user:{}", Uuid::new_v4()),
    );
    h.set_principal(
        &c,
        a.principal,
        PrincipalStatus::Active {
            kind: "NATIVE_CF_USER".into(),
        },
    );
    let mut enc = body(a.principal);
    enc["conversationId"] = e2ee["conversationId"].clone();
    let r = command(&h, enc, Uuid::new_v4(), true).await;
    assert_eq!(r.code(), "E2EE_EXPORT_NOT_AVAILABLE", "{}", r.text);
    assert_eq!(rows().await, 0);

    // Requested: a reference only.
    let key = Uuid::new_v4();
    let r = command(&h, body(recipient), key, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "REQUESTED");
    assert_eq!(r.json["version"], Value::Null);
    let res = &r.json["resource"];
    assert_eq!(res["exportId"], json!(key));
    assert_eq!(res["exportState"], "REQUESTED");
    assert_eq!(res["delivery"], "OWNER_DELIVERED");
    assert_eq!(res["deliveryChannel"], "CONSOLE_REVEAL_ONCE");
    assert!(Uuid::parse_str(res["deliveryRef"].as_str().unwrap()).is_ok());
    assert_eq!(res["recipientMasked"], Value::Null);
    assert!(!r.text.contains("exp/") && !r.text.contains("http"));
    let again = command(&h, body(recipient), key, true).await;
    assert_eq!(again.json, r.json);
    assert_eq!(rows().await, 1);
    let rec = read(&h, &format!("/internal/v1/operator/operations/{key}")).await;
    assert_eq!(rec.json["source_status"], "REQUESTED", "{}", rec.text);
    assert_eq!(rec.json["source_version"], Value::Null);

    // The tenant never sees it; nor does anyone before it is ready.
    let eid = key;
    let t = h
        .get(&c, None, &format!("/v1/messaging/exports/{eid}"))
        .await;
    assert_eq!(t.status, StatusCode::NOT_FOUND, "{}", t.text);
    let t = h
        .post(
            &c,
            None,
            &format!("/v1/messaging/exports/{eid}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(t.status, StatusCode::NOT_FOUND, "{}", t.text);
    let claim_path = format!("/internal/v1/delegated/v1/messaging/exports/{eid}/claim");
    // Never on the customer surface, not even for a first-party credential.
    let r = h
        .post(
            &c,
            Some(recipient),
            &format!("/v1/messaging/exports/{eid}/claim"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = h
        .get(&c, Some(recipient), "/v1/messaging/export-deliveries")
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let early = console(&h, &c, recipient, Method::POST, &claim_path).await;
    assert_eq!(early.reason(), "EXPORT_NOT_READY", "{}", early.text);

    messaging::compliance::run_once(&h.state).await.unwrap();
    let hq = read(&h, &format!("/internal/v1/operator/exports/{eid}")).await;
    assert_eq!(hq.status, StatusCode::OK, "{}", hq.text);
    assert_eq!(hq.json["exportState"], "READY");
    assert_eq!(hq.json["sha256"].as_str().unwrap().len(), 64);
    assert!(hq.json["size"].as_i64().unwrap() > 0);
    assert!(!hq.text.contains("exp/"));

    // Another member's console: 404, and an empty list.
    let r = console(&h, &c, other, Method::POST, &claim_path).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let list = console(&h, &c, other, Method::GET, DELIVERIES).await;
    assert_eq!(list.json["items"], json!([]), "{}", list.text);

    // The recipient's console lists it and claims it once.
    let list = console(&h, &c, recipient, Method::GET, DELIVERIES).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    assert_eq!(list.json["items"][0]["exportId"], json!(eid));
    assert_eq!(list.json["items"][0]["exportState"], "READY");
    let got = console(&h, &c, recipient, Method::POST, &claim_path).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.text);
    assert!(got.json["url"]
        .as_str()
        .unwrap()
        .contains("response-content-disposition=attachment"));
    let twice = console(&h, &c, recipient, Method::POST, &claim_path).await;
    assert_eq!(
        (twice.status, twice.reason()),
        (StatusCode::CONFLICT, "EXPORT_ALREADY_DELIVERED"),
        "{}",
        twice.text
    );
    let hq = read(&h, &format!("/internal/v1/operator/exports/{eid}")).await;
    assert_eq!(hq.json["exportState"], "DELIVERED");
    assert!(hq.json["deliveredAt"].is_string());
    let all = read(&h, "/internal/v1/operator/exports?limit=10").await;
    assert_eq!(all.json["items"][0]["delivery"], "OWNER_DELIVERED");
    assert_eq!(
        all.json["items"][0]["deliveryChannel"],
        "CONSOLE_REVEAL_ONCE"
    );
    // The worker published no tenant event for it.
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_outbox_events WHERE aggregate_id = '{eid}'"
        ))
        .await,
        0
    );
    assert!(!store.is_empty());

    // Seven days later it expires; the copy is queued for deletion.
    h.clock.advance(chrono::Duration::days(8));
    messaging::compliance::run_once(&h.state).await.unwrap();
    let hq = read(&h, &format!("/internal/v1/operator/exports/{eid}")).await;
    assert_eq!(hq.json["exportState"], "EXPIRED");
    let r = console(&h, &c, recipient, Method::POST, &claim_path).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // A tenant's own export is not an owner-delivered reference.
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": cid }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let own = r.json["exportId"].as_str().unwrap();
    let hq = read(&h, &format!("/internal/v1/operator/exports/{own}")).await;
    assert_eq!(hq.status, StatusCode::NOT_FOUND, "{}", hq.text);
}

#[tokio::test]
async fn only_the_bridge_requests_or_reads_owner_delivered_exports() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    let key = Uuid::new_v4().to_string();
    let body = json!({ "tenantId": Uuid::new_v4(), "conversationId": Uuid::new_v4(),
                       "recipientPrincipalId": Uuid::new_v4(), "reasonCode": "DATA_SUBJECT_REQUEST" });
    // The Meeting Service (another trusted peer) is refused before anything is read.
    let r = h
        .call_internal(
            &peers.meeting,
            Method::POST,
            "/internal/v1/operator/exports",
            Some(body.clone()),
            None,
            &[("idempotency-key", key.as_str())],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // The bridge without an operator.
    let r = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            &format!("/internal/v1/operator/exports/{}", Uuid::new_v4()),
            None,
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // An unknown export.
    let r = read(
        &h,
        &format!("/internal/v1/operator/exports/{}", Uuid::new_v4()),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // An unknown conversation (with the approval) is 404, nothing recorded.
    let r = command(&h, body, Uuid::new_v4(), true).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_admin_operations")
            .await,
        0
    );
}
