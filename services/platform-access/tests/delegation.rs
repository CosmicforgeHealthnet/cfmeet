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

#[tokio::test]
async fn the_developer_platform_delegates_service_account_create_and_suspend_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("sacust", "EXTERNAL", &["sms.read"]).await;
    let other = h.consumer("saother", "EXTERNAL", &[]).await;
    let sandbox = c.env("SANDBOX");
    let base = format!("/internal/v1/delegation/environments/{sandbox}/service-accounts");
    // Creation names the human actor and is scoped to the persisted environment.
    let created = dp(
        &h,
        Method::POST,
        &base,
        Some(json!({ "name": "ci-runner", "description": "CI" })),
        true,
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["status"], "ACTIVE");
    assert_eq!(created.body["environmentId"], json!(sandbox));
    assert_eq!(created.body["tenantId"], json!(c.tenant_id));
    let sa = uuid(&created.body["id"]);
    let anon = dp(
        &h,
        Method::POST,
        &base,
        Some(json!({ "name": "anon" })),
        false,
    )
    .await;
    assert_eq!(anon.code(), "OPERATOR_REQUIRED");
    let dup = dp(
        &h,
        Method::POST,
        &base,
        Some(json!({ "name": "ci-runner" })),
        true,
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT, "{}", dup.body);
    let bad = dp(
        &h,
        Method::POST,
        &base,
        Some(json!({ "name": "x", "tenantId": other.tenant_id })),
        true,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "unknown field refused");
    // List / get within the environment.
    let list = dp(&h, Method::GET, &base, None, false).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let names: Vec<&str> = list.body["serviceAccounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["backend", "ci-runner"]);
    let one = dp(&h, Method::GET, &format!("{base}/{sa}"), None, false).await;
    assert_eq!(one.body["id"], json!(sa));
    // Another tenant's service account (or environment) is NOT_FOUND through this path.
    for path in [
        format!("{base}/{}", other.sa("SANDBOX")),
        format!(
            "/internal/v1/delegation/environments/{}/service-accounts/{sa}",
            other.env("SANDBOX")
        ),
    ] {
        let r = dp(&h, Method::GET, &path, None, false).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.body);
        let r = dp(
            &h,
            Method::POST,
            &format!("{path}/status"),
            Some(json!({ "status": "SUSPENDED" })),
            true,
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.body);
    }
    assert_eq!(
        dp(
            &h,
            Method::GET,
            &format!(
                "/internal/v1/delegation/environments/{}/service-accounts",
                uuid::Uuid::new_v4()
            ),
            None,
            false
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Suspension stops the service account's credentials authenticating; resume restores.
    let (_cred, secret) = h.issue(sa, &["sms.read"]).await;
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    let status = |s: &str| Some(json!({ "status": s }));
    let r = dp(
        &h,
        Method::POST,
        &format!("{base}/{sa}/status"),
        status("SUSPENDED"),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["status"], "SUSPENDED");
    let refused = h.authenticate(&secret).await;
    assert_eq!(refused.body["outcome"], "REJECTED", "{}", refused.body);
    assert_eq!(refused.body["reason"], "service_account_inactive");
    let r = dp(
        &h,
        Method::POST,
        &format!("{base}/{sa}/status"),
        status("ACTIVE"),
        true,
    )
    .await;
    assert_eq!(r.body["status"], "ACTIVE");
    assert_eq!(
        h.authenticate(&secret).await.body["outcome"],
        "AUTHENTICATED"
    );
    // Revocation is never delegated (and nothing is ever deleted).
    let r = dp(
        &h,
        Method::POST,
        &format!("{base}/{sa}/status"),
        status("REVOKED"),
        true,
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    let r = dp(&h, Method::DELETE, &format!("{base}/{sa}"), None, true).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    // PRODUCTION: same rule as the environment itself.
    let prod = c.env("PRODUCTION");
    let prod_base = format!("/internal/v1/delegation/environments/{prod}/service-accounts");
    let r = dp(
        &h,
        Method::POST,
        &prod_base,
        Some(json!({ "name": "prod-bot" })),
        true,
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    let r = dp(
        &h,
        Method::POST,
        &format!("{prod_base}/{}/status", c.sa("PRODUCTION")),
        status("SUSPENDED"),
        true,
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    assert_eq!(
        dp(&h, Method::GET, &prod_base, None, false).await.status,
        StatusCode::OK,
        "reads stay available"
    );
    // Audited under the delegated actor: create + suspend + resume.
    let audited = h
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT count(*) AS n FROM platform_access_audit \
                 WHERE actor = 'devplat.{ACTOR}' AND target_id = '{sa}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(audited, 3);
    // Only the Developer Platform holds the delegation permission.
    let calling = h
        .as_calling(Method::POST, &base, Some(json!({ "name": "evil" })), None)
        .await;
    assert_eq!(calling.status, StatusCode::FORBIDDEN);
}

/// Section 15 (ORG-013 / ORG-014): an EXTERNAL tenant can be renamed through delegation
/// (audited, idempotent) and found by its creation slug; a FIRST_PARTY tenant can be
/// neither renamed nor looked up that way.
#[tokio::test]
async fn delegated_rename_and_slug_lookup_are_external_only_and_audited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = dp(
        &h,
        Method::POST,
        "/internal/v1/delegation/tenants",
        Some(json!({ "slug": "org-0123456789abcdef0123", "displayName": "Acme" })),
        true,
    )
    .await;
    assert_eq!(t.status, StatusCode::CREATED, "{}", t.body);
    let tenant = uuid(&t.body["id"]);
    // Lookup by slug finds it.
    let found = dp(
        &h,
        Method::GET,
        "/internal/v1/delegation/tenant-slugs/org-0123456789abcdef0123",
        None,
        false,
    )
    .await;
    assert_eq!(found.status, StatusCode::OK, "{}", found.body);
    assert_eq!(uuid(&found.body["id"]), tenant);
    let missing = dp(
        &h,
        Method::GET,
        "/internal/v1/delegation/tenant-slugs/org-ffffffffffffffffffff",
        None,
        false,
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    // Rename: named actor required, audited once, a repeat is a no-op.
    let path = format!("/internal/v1/delegation/tenants/{tenant}/display-name");
    let anon = dp(
        &h,
        Method::POST,
        &path,
        Some(json!({ "displayName": "Acme Labs" })),
        false,
    )
    .await;
    assert_eq!(anon.code(), "OPERATOR_REQUIRED");
    for _ in 0..2 {
        let r = dp(
            &h,
            Method::POST,
            &path,
            Some(json!({ "displayName": "  Acme Labs " })),
            true,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(r.body["displayName"], "Acme Labs");
    }
    let renamed = h
        .admin_ok(
            Method::GET,
            &format!("/internal/v1/admin/tenants/{tenant}"),
            None,
        )
        .await;
    assert_eq!(renamed["displayName"], "Acme Labs");
    let audit = h
        .admin_ok(
            Method::GET,
            &format!("/internal/v1/admin/audit?tenantId={tenant}&action=TENANT_RENAMED"),
            None,
        )
        .await;
    assert_eq!(audit["items"].as_array().unwrap().len(), 2, "{audit}");
    assert_eq!(audit["items"][0]["actor"], format!("devplat.{ACTOR}"));
    let bad = dp(
        &h,
        Method::POST,
        &path,
        Some(json!({ "displayName": "" })),
        true,
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    // FIRST_PARTY: operator-managed.
    let fp = h.consumer("fp-co", "FIRST_PARTY", &[]).await;
    let r = dp(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/delegation/tenants/{}/display-name",
            fp.tenant_id
        ),
        Some(json!({ "displayName": "Renamed" })),
        true,
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    let r = dp(
        &h,
        Method::GET,
        "/internal/v1/delegation/tenant-slugs/fp-co",
        None,
        false,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// Section 15 (PLT-009): the operator audit trail is readable — operator-only, filtered,
/// keyset-paginated newest first.
#[tokio::test]
async fn the_operator_audit_trail_is_readable_by_operators_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("audited", "EXTERNAL", &["calls.read"]).await;
    // consumer(): tenant + application + scope + 3 environments + 3 service accounts.
    let all = h
        .admin_ok(
            Method::GET,
            &format!(
                "/internal/v1/admin/audit?tenantId={}&limit=200",
                c.tenant_id
            ),
            None,
        )
        .await;
    let items = all["items"].as_array().unwrap();
    assert_eq!(items.len(), 9, "{all}");
    assert!(all["nextCursor"].is_null());
    assert!(items.iter().any(|i| i["action"] == "TENANT_CREATED"));
    assert!(items.iter().all(|i| i["actor"] == OPERATOR));
    // Pages of 4 walk the same rows in the same order.
    let mut walked = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut path = format!("/internal/v1/admin/audit?tenantId={}&limit=4", c.tenant_id);
        if let Some(cur) = &cursor {
            path.push_str(&format!("&cursor={cur}"));
        }
        let page = h.admin_ok(Method::GET, &path, None).await;
        walked.extend(page["items"].as_array().unwrap().iter().cloned());
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(&walked, items);
    let envs = h
        .admin_ok(
            Method::GET,
            &format!(
                "/internal/v1/admin/audit?tenantId={}&action=ENVIRONMENT_CREATED",
                c.tenant_id
            ),
            None,
        )
        .await;
    assert_eq!(envs["items"].as_array().unwrap().len(), 3);
    for bad in [
        "?action=NOT_AN_ACTION",
        "?targetKind=GALAXY",
        "?limit=0",
        "?limit=201",
        "?cursor=garbage",
        "?actor=has%20space",
        "?unknown=1",
    ] {
        let r = h
            .admin(Method::GET, &format!("/internal/v1/admin/audit{bad}"), None)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.body);
    }
    // Operator-only: a named operator is required, and no other service may read it.
    let unnamed = h
        .call(
            &h.peers.operator,
            Method::GET,
            "/internal/v1/admin/audit",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(unnamed.code(), "OPERATOR_REQUIRED");
    let calling = h
        .as_calling(Method::GET, "/internal/v1/admin/audit", None, None)
        .await;
    assert_eq!(calling.status, StatusCode::FORBIDDEN);
    let delegate = dp(&h, Method::GET, "/internal/v1/admin/audit", None, true).await;
    assert_eq!(delegate.status, StatusCode::FORBIDDEN);
}
