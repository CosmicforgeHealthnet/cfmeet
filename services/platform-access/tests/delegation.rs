//! Part VI §50 / §51 — the Developer Platform delegation API and the receiver-side
//! re-verification of gateway-authenticated credentials.
//!
//! * `/internal/v1/delegation/**` is reachable only with `platform-identity.delegate`
//!   (Developer Platform) and a named human actor; tenants are always EXTERNAL, PRODUCTION
//!   environments are refused, every change is audited under `devplat.<actor>`.
//! * `POST /internal/v1/credentials/delegated-authority` returns the CURRENT authority
//!   chain of the credential id signed into the delegated context — and only when it
//!   belongs to exactly the delegated scope; revocation takes effect immediately.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};

const ACTOR: &str = "u-0123456789abcdef0123456789abcdef";

async fn dp(h: &Harness, method: Method, path: &str, body: Option<Value>, actor: bool) -> Reply {
    let outbound = cf_service_auth::OutboundServiceRequest::new_operation(
        cf_service_auth::ServiceId::platform_access(),
    );
    let signed = h.peers.developer_platform.sign(&outbound).unwrap();
    let mut builder = axum::http::Request::builder().method(method).uri(path);
    for (name, value) in signed.headers().iter() {
        builder = builder.header(name, value);
    }
    if actor {
        builder = builder.header("x-cf-delegated-actor", ACTOR);
    }
    let request = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(axum::body::Body::empty()).unwrap(),
    };
    h.raw(request).await
}

#[tokio::test]
async fn the_developer_platform_delegates_the_workspace_hierarchy_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A tenant is always EXTERNAL whatever the caller would like.
    let t = dp(
        &h,
        Method::POST,
        "/internal/v1/delegation/tenants",
        Some(json!({ "slug": "acme-dev", "displayName": "Acme" })),
        true,
    )
    .await;
    assert_eq!(t.status, StatusCode::CREATED, "{}", t.body);
    assert_eq!(t.body["classification"], "EXTERNAL");
    let tenant = uuid(&t.body["id"]);
    let forged = dp(
        &h,
        Method::POST,
        "/internal/v1/delegation/tenants",
        Some(json!({ "slug": "acme-fp", "displayName": "Acme", "classification": "FIRST_PARTY" })),
        true,
    )
    .await;
    assert_eq!(
        forged.status,
        StatusCode::BAD_REQUEST,
        "unknown field refused"
    );
    // Without a named actor nothing changes.
    let anon = dp(
        &h,
        Method::POST,
        "/internal/v1/delegation/tenants",
        Some(json!({ "slug": "acme-anon", "displayName": "Acme" })),
        false,
    )
    .await;
    assert_eq!(anon.code(), "OPERATOR_REQUIRED");
    // Project = application, SANDBOX / TEST environments; PRODUCTION is Section 55.
    let a = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/tenants/{tenant}/applications"),
        Some(json!({ "slug": "web", "displayName": "Web" })),
        true,
    )
    .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.body);
    let app = uuid(&a.body["id"]);
    for kind in ["SANDBOX", "TEST"] {
        let e = dp(
            &h,
            Method::POST,
            &format!("/internal/v1/delegation/applications/{app}/environments"),
            Some(json!({ "kind": kind })),
            true,
        )
        .await;
        assert_eq!(e.status, StatusCode::CREATED, "{}", e.body);
        assert_eq!(e.body["liveTelecomAllowed"], false);
    }
    let prod = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/applications/{app}/environments"),
        Some(json!({ "kind": "PRODUCTION" })),
        true,
    )
    .await;
    assert_eq!(
        prod.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        prod.body
    );
    assert_eq!(prod.code(), "POLICY_VIOLATION");
    let list = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/delegation/applications/{app}/environments"),
        None,
        false,
    )
    .await;
    assert_eq!(list.body["environments"].as_array().unwrap().len(), 2);
    let sandbox = uuid(&list.body["environments"][0]["id"]);
    // Suspension of a non-production environment, of the project and of the tenant.
    let s = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/environments/{sandbox}/status"),
        Some(json!({ "status": "SUSPENDED" })),
        true,
    )
    .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    let s = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/applications/{app}/status"),
        Some(json!({ "status": "SUSPENDED" })),
        true,
    )
    .await;
    assert_eq!(s.body["status"], "SUSPENDED");
    let s = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/delegation/tenants/{tenant}/status"),
        Some(json!({ "status": "SUSPENDED" })),
        true,
    )
    .await;
    assert_eq!(s.body["status"], "SUSPENDED");
    // A PRODUCTION environment (operator-created) cannot be changed by delegation.
    let c = h.consumer("opcust", "EXTERNAL", &[]).await;
    let p = dp(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/environments/{}/status",
            c.env("PRODUCTION")
        ),
        Some(json!({ "status": "SUSPENDED" })),
        true,
    )
    .await;
    assert_eq!(p.code(), "POLICY_VIOLATION");
    // Every change is audited under the delegated human actor.
    let audited = h
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT count(*) AS n FROM platform_access_audit WHERE actor = 'devplat.{ACTOR}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(audited, 7);
    // Other services never hold the delegation permission.
    let calling = h
        .as_calling(
            Method::POST,
            "/internal/v1/delegation/tenants",
            Some(json!({ "slug": "evil", "displayName": "Evil" })),
            None,
        )
        .await;
    assert_eq!(calling.status, StatusCode::FORBIDDEN);
    let gw = h
        .call(
            &h.peers.gateway,
            Method::GET,
            &format!("/internal/v1/delegation/tenants/{tenant}"),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(gw.status, StatusCode::FORBIDDEN);
    // And the Developer Platform never reaches operator administration.
    let admin = dp(
        &h,
        Method::POST,
        "/internal/v1/admin/tenants",
        Some(json!({ "slug": "x1", "displayName": "X", "classification": "FIRST_PARTY" })),
        true,
    )
    .await;
    assert_eq!(admin.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn receivers_re_verify_the_gateway_credential_live() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("gwcust", "EXTERNAL", &["sms.read", "calls.read"])
        .await;
    let other = h.consumer("gwother", "EXTERNAL", &["sms.read"]).await;
    let (cred, _secret) = h.issue(c.sa("SANDBOX"), &["sms.read"]).await;
    let cred_id = uuid(&cred["id"]);
    let path = "/internal/v1/credentials/delegated-authority";
    let call = |ctx| h.as_calling(Method::POST, path, None, Some(ctx));
    // The credential's own chain: scopes and FULL authority, as at authentication.
    let r = call(c.delegated("SANDBOX").with_initiating_credential(cred_id)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["outcome"], "AUTHENTICATED");
    assert_eq!(r.body["authority"]["scopes"], json!(["sms.read"]));
    assert_eq!(r.body["authority"]["authorityMode"], "FULL");
    // Another scope's context, or another environment of the same app: unknown.
    for ctx in [
        other
            .delegated("SANDBOX")
            .with_initiating_credential(cred_id),
        c.delegated("TEST").with_initiating_credential(cred_id),
    ] {
        let r = call(ctx).await;
        assert_eq!(r.body["outcome"], "REJECTED");
        assert_eq!(r.body["reason"], "unknown_credential");
    }
    // No credential in the signed context: refused.
    let r = call(c.delegated("SANDBOX")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Revocation is effective immediately.
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/credentials/{cred_id}/revoke"),
        None,
    )
    .await;
    let r = call(c.delegated("SANDBOX").with_initiating_credential(cred_id)).await;
    assert_eq!(r.body["reason"], "credential_revoked");
    // Tenant suspension degrades to SAFETY_ONLY exactly like authentication.
    let (cred2, _) = h.issue(c.sa("SANDBOX"), &["sms.read"]).await;
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        Some(json!({"status": "SUSPENDED"})),
    )
    .await;
    let r = call(
        c.delegated("SANDBOX")
            .with_initiating_credential(uuid(&cred2["id"])),
    )
    .await;
    assert_eq!(r.body["authority"]["authorityMode"], "SAFETY_ONLY");
    // The Developer Platform may not use it (credential authentication is not its grant).
    let r = h
        .call(
            &h.peers.developer_platform,
            Method::POST,
            path,
            None,
            Some(c.delegated("SANDBOX").with_initiating_credential(cred_id)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}
