//! ADR 0068 A4 — owner-delivered API keys in the partner's console, through the REAL
//! router (CI: real PostgreSQL): a key HQ staff issued through the admin bridge is listed
//! (never a secret) and revealed ONCE to a signed-in human member who manages credentials,
//! with no-store and no idempotent replay; machines are refused before Platform Access.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

async fn user(
    c: &Control,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Response {
    c.req(As::User(token), method, path, body).await
}

#[tokio::test]
async fn an_owner_delivered_key_is_listed_and_revealed_once_to_a_human_member() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@keys.example");
    let o = user(
        &c,
        &owner,
        Method::POST,
        "/v1/organizations",
        Some(json!({ "displayName": "Acme" })),
    )
    .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let org = id(&o.json);
    let p = user(
        &c,
        &owner,
        Method::POST,
        &format!("/v1/organizations/{org}/projects"),
        Some(json!({ "displayName": "Backend" })),
    )
    .await;
    let project = id(&p.json);
    let e = user(
        &c,
        &owner,
        Method::POST,
        &format!("/v1/projects/{project}/environments"),
        Some(json!({ "kind": "SANDBOX" })),
    )
    .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    let env = id(&e.json);
    let sa = user(
        &c,
        &owner,
        Method::POST,
        &format!("/v1/environments/{env}/service-accounts"),
        Some(json!({ "name": "backend" })),
    )
    .await;
    assert_eq!(sa.status, StatusCode::CREATED, "{}", sa.json);
    // HQ staff issued a key through the admin bridge: Platform Access holds it.
    let (delivery, credential, secret) = c.pa.hold_delivery(env, id(&sa.json));

    let list = format!("/v1/environments/{env}/credential-deliveries");
    let r = user(&c, &owner, Method::GET, &list, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}CredentialDeliveryList"), &r.json);
    assert_eq!(r.json["items"][0]["deliveryRef"], json!(delivery));
    assert_eq!(r.json["items"][0]["credentialId"], json!(credential));
    assert!(
        !r.json.to_string().contains(&secret),
        "never a secret in the list"
    );

    // A machine credential is refused before Platform Access is asked.
    let reveal = format!("/v1/environments/{env}/credential-deliveries/{delivery}/reveal");
    let machine = c.pa.machine(env, &["credentials.manage"]);
    let r = c
        .req(As::Machine(&machine), Method::POST, &reveal, None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    assert!(c.pa.deliveries.lock().unwrap().contains_key(&delivery));

    // The one reveal: the secret, no-store, recorded without the secret.
    let r = user(&c, &owner, Method::POST, &reveal, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}IssuedCredential"), &r.json);
    assert_eq!(r.json["secret"], secret.as_str());
    assert_eq!(r.json["credential"]["id"], json!(credential));
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    let act = user(
        &c,
        &owner,
        Method::GET,
        &format!("/v1/organizations/{org}/activity"),
        None,
    )
    .await;
    let revealed: Vec<&Value> = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "credential.delivery_revealed")
        .collect();
    assert_eq!(revealed.len(), 1);
    assert_eq!(revealed[0]["summary"]["deliveryRef"], json!(delivery));
    assert!(!act.json.to_string().contains(&secret));
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_idempotency_records r WHERE row_to_json(r)::text LIKE '%{secret}%'"
        ))
        .await,
        0,
        "never stored for a replay"
    );

    // Nothing is resent.
    let r = user(&c, &owner, Method::POST, &reveal, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    let r = user(&c, &owner, Method::GET, &list, None).await;
    assert_eq!(r.json["items"], json!([]));

    // A member of another workspace sees nothing of this environment.
    let (stranger, _) = c.sessions.sign_in("other@keys.example");
    let r = user(&c, &stranger, Method::GET, &list, None).await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::NOT_FOUND,
        "{}",
        r.json
    );
}
