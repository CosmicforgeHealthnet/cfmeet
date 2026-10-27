//! Part VI §54 / §55 — delegated credential self-service, reversible suspension, scope
//! grants and production-environment activation (`/internal/v1/delegation/**`).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};

const ACTOR: &str = "u-0123456789abcdef0123456789abcdef";
const OPERATOR_COMMAND: &str = "o-0123456789abcdef0123456789abcdef";

async fn dp_as(h: &Harness, method: Method, path: &str, body: Option<Value>, actor: &str) -> Reply {
    let outbound = cf_service_auth::OutboundServiceRequest::new_operation(
        cf_service_auth::ServiceId::platform_access(),
    );
    let signed = h.peers.developer_platform.sign(&outbound).unwrap();
    let mut builder = axum::http::Request::builder().method(method).uri(path);
    for (name, value) in signed.headers().iter() {
        builder = builder.header(name, value);
    }
    builder = builder.header("x-cf-delegated-actor", actor);
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(axum::body::Body::empty()).unwrap(),
    };
    h.raw(request).await
}

async fn dp(h: &Harness, method: Method, path: &str, body: Option<Value>) -> Reply {
    dp_as(h, method, path, body, ACTOR).await
}

async fn audit_count(h: &Harness, action: &str) -> i64 {
    h.conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT count(*) FROM platform_access_audit WHERE action = '{action}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

#[tokio::test]
async fn delegated_credentials_are_issued_once_suspended_resumed_and_revoked() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("dcred", "EXTERNAL", &["sms.read", "calls.read"])
        .await;
    let other = h.consumer("dcred-other", "EXTERNAL", &["sms.read"]).await;
    let env = c.env("SANDBOX");
    let sa = c.sa("SANDBOX");
    let base = format!("/internal/v1/delegation/environments/{env}");
    let issued = dp(
        &h,
        Method::POST,
        &format!("{base}/service-accounts/{sa}/credentials"),
        Some(json!({ "scopes": ["sms.read"], "label": "ci" })),
    )
    .await;
    assert_eq!(issued.status, StatusCode::CREATED, "{}", issued.body);
    assert_eq!(
        issued
            .headers
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let secret = issued.body["secret"].as_str().unwrap().to_string();
    let cred = uuid(&issued.body["credential"]["id"]);
    assert!(issued.body["credential"].get("verifier").is_none());
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    // Listing / reading return metadata only.
    let list = dp(
        &h,
        Method::GET,
        &format!("{base}/service-accounts/{sa}/credentials"),
        None,
    )
    .await;
    assert!(!list.body.to_string().contains(&secret));
    // Another environment's credential is NOT_FOUND on this environment's path.
    let foreign = dp(
        &h,
        Method::GET,
        &format!(
            "/internal/v1/delegation/environments/{}/credentials/{cred}",
            other.env("SANDBOX")
        ),
        None,
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    // Scopes outside the application's allowed set are refused.
    let bad = dp(
        &h,
        Method::POST,
        &format!("{base}/service-accounts/{sa}/credentials"),
        Some(json!({ "scopes": ["numbers.manage"] })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.body);
    // Suspension: refused at authentication and re-verification at once; reversible.
    let s = dp(
        &h,
        Method::POST,
        &format!("{base}/credentials/{cred}/suspend"),
        Some(json!({ "reasonCode": "INCIDENT_RESPONSE" })),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    assert_eq!(s.body["effectiveState"], "SUSPENDED");
    assert!(s.body["suspendedAt"].is_string());
    let a = h.authenticate(&secret).await;
    assert_eq!(
        (a.body["outcome"].as_str(), a.body["reason"].as_str()),
        (Some("REJECTED"), Some("credential_suspended"))
    );
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/credentials/delegated-authority",
            None,
            Some(c.delegated("SANDBOX").with_initiating_credential(cred)),
        )
        .await;
    assert_eq!(r.body["reason"], "credential_suspended");
    let rot = dp(
        &h,
        Method::POST,
        &format!("{base}/credentials/{cred}/rotate"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        rot.status,
        StatusCode::CONFLICT,
        "a suspended credential is not rotated"
    );
    let again = dp(
        &h,
        Method::POST,
        &format!("{base}/credentials/{cred}/suspend"),
        Some(json!({ "reasonCode": "INCIDENT_RESPONSE" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "idempotent");
    let res = dp(
        &h,
        Method::POST,
        &format!("{base}/credentials/{cred}/resume"),
        None,
    )
    .await;
    assert_eq!(res.body["effectiveState"], "ACTIVE", "{}", res.body);
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    // Rotation: bounded overlap; the successor's secret is new.
    let rot = dp(
        &h,
        Method::POST,
        &format!("{base}/credentials/{cred}/rotate"),
        Some(json!({ "overlapMinutes": 1 })),
    )
    .await;
    assert_eq!(rot.status, StatusCode::CREATED, "{}", rot.body);
    let successor = rot.body["secret"].as_str().unwrap().to_string();
    assert_ne!(successor, secret);
    let rev = dp(
        &h,
        Method::POST,
        &format!(
            "{base}/credentials/{}/revoke",
            uuid(&rot.body["credential"]["id"])
        ),
        None,
    )
    .await;
    assert_eq!(rev.body["effectiveState"], "REVOKED");
    assert_eq!(
        h.authenticate(&successor).await.body["reason"],
        "credential_revoked"
    );
    // Operator-created PRODUCTION environments stay operator-only.
    let prod = dp(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/environments/{}/service-accounts/{}/credentials",
            c.env("PRODUCTION"),
            c.sa("PRODUCTION")
        ),
        Some(json!({ "scopes": ["sms.read"] })),
    )
    .await;
    assert_eq!(
        prod.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        prod.body
    );
    let lt = dp(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/environments/{}/live-telecom",
            c.env("PRODUCTION")
        ),
        Some(json!({ "allowed": true })),
    )
    .await;
    assert_eq!(lt.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Every change audited under the Developer Platform actor.
    for (action, n) in [
        ("CREDENTIAL_SUSPENDED", 2),
        ("CREDENTIAL_RESUMED", 1),
        ("CREDENTIAL_ROTATED", 1),
    ] {
        assert_eq!(audit_count(&h, action).await, n, "{action}");
    }
}

#[tokio::test]
async fn scope_grants_and_production_activation_are_delegated_under_trust_rules() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = dp(
        &h,
        Method::POST,
        "/internal/v1/delegation/tenants",
        Some(json!({ "slug": "prodco", "displayName": "ProdCo" })),
    )
    .await;
    let tenant = uuid(&t.body["id"]);
    let a = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/tenants/{tenant}/applications"),
        Some(json!({ "slug": "app", "displayName": "App" })),
    )
    .await;
    let app = uuid(&a.body["id"]);
    let sbx = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/applications/{app}/environments"),
        Some(json!({ "kind": "SANDBOX" })),
    )
    .await;
    assert_eq!(sbx.status, StatusCode::CREATED);
    let grants = format!("/internal/v1/delegation/applications/{app}/scopes");
    // STANDARD for SANDBOX / TEST: allowed; PRODUCTION needs an activation.
    let g = dp(
        &h,
        Method::POST,
        &grants,
        Some(json!({ "scope": "sms.send", "environmentKinds": ["SANDBOX", "TEST"] })),
    )
    .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.body);
    let p = dp(
        &h,
        Method::POST,
        &grants,
        Some(json!({ "scope": "sms.send", "environmentKinds": ["PRODUCTION"] })),
    )
    .await;
    assert_eq!(p.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", p.body);
    // FIRST_PARTY_ONLY: never for a member actor; an operator command still needs a
    // FIRST_PARTY tenant; PRIVILEGED: never through delegation.
    for (scope, actor) in [
        ("events.private_delivery", ACTOR),
        ("events.private_delivery", OPERATOR_COMMAND),
        ("numbers.admin", OPERATOR_COMMAND),
    ] {
        let r = dp_as(
            &h,
            Method::POST,
            &grants,
            Some(json!({ "scope": scope, "environmentKinds": ["SANDBOX"] })),
            actor,
        )
        .await;
        assert!(r.status.is_client_error(), "{scope} {actor}: {}", r.body);
    }
    let fp = h.consumer("fpco", "FIRST_PARTY", &[]).await;
    let ok = dp_as(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/applications/{}/scopes",
            fp.application_id
        ),
        Some(json!({ "scope": "events.private_delivery", "environmentKinds": ["SANDBOX"] })),
        OPERATOR_COMMAND,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    // Activation: PRODUCTION created once for the approval reference.
    let act = format!("/internal/v1/delegation/applications/{app}/production-environment");
    let e = dp(
        &h,
        Method::POST,
        &act,
        Some(json!({ "approvalReference": "par_0123" })),
    )
    .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.body);
    assert_eq!(e.body["kind"], "PRODUCTION");
    let prod = uuid(&e.body["id"]);
    let same = dp(
        &h,
        Method::POST,
        &act,
        Some(json!({ "approvalReference": "par_0123" })),
    )
    .await;
    assert_eq!(uuid(&same.body["id"]), prod, "idempotent on the reference");
    let other = dp(
        &h,
        Method::POST,
        &act,
        Some(json!({ "approvalReference": "par_other" })),
    )
    .await;
    assert_eq!(other.status, StatusCode::CONFLICT);
    assert_eq!(audit_count(&h, "PRODUCTION_ENVIRONMENT_ACTIVATED").await, 1);
    // Now PRODUCTION grants, identities, credentials and live telecom are delegable.
    let p = dp(
        &h,
        Method::POST,
        &grants,
        Some(json!({ "scope": "sms.send", "environmentKinds": ["SANDBOX", "TEST", "PRODUCTION"] })),
    )
    .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    let sa = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/environments/{prod}/service-accounts"),
        Some(json!({ "name": "prod-backend" })),
    )
    .await;
    assert_eq!(sa.status, StatusCode::CREATED, "{}", sa.body);
    let sa = uuid(&sa.body["id"]);
    let cred = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/environments/{prod}/service-accounts/{sa}/credentials"),
        Some(json!({ "scopes": ["sms.send"] })),
    )
    .await;
    assert_eq!(cred.status, StatusCode::CREATED, "{}", cred.body);
    let secret = cred.body["secret"].as_str().unwrap().to_string();
    assert!(
        secret.starts_with("cfmk_live_"),
        "production credentials carry the live tag"
    );
    let lt = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/environments/{prod}/live-telecom"),
        Some(json!({ "allowed": true })),
    )
    .await;
    assert_eq!(lt.body["liveTelecomAllowed"], true, "{}", lt.body);
    // Suspension of the activated environment: its credentials lose full authority.
    let s = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/environments/{prod}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    let a = h.authenticate(&secret).await;
    assert_eq!(
        a.body["authority"]["authorityMode"], "SAFETY_ONLY",
        "{}",
        a.body
    );
    // Withdrawing a scope removes it from every credential of the application.
    let w = dp(&h, Method::DELETE, &format!("{grants}/sms.send"), None).await;
    assert_eq!(w.body["removed"], true);
    let w = dp(
        &h,
        Method::DELETE,
        &format!("/internal/v1/delegation/applications/{app}/scopes/numbers.admin"),
        None,
    )
    .await;
    assert_eq!(w.status, StatusCode::UNPROCESSABLE_ENTITY);
}
