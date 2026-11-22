//! ADR 0068: the admin bridge's operator reads (`/internal/v1/operator/*`).
//!
//! * `GET /internal/v1/operator/tenants`
//! * `GET /internal/v1/operator/tenants/{tenantId}/applications`
//! * `GET /internal/v1/operator/tenants/{tenantId}/environments`
//! * `GET /internal/v1/operator/tenants/{tenantId}/service-accounts`
//! * `GET /internal/v1/operator/tenants/{tenantId}/scope-grants`
//! * `GET /internal/v1/operator/tenants/{tenantId}/credentials`
//! * `GET /internal/v1/operator/tenants/{tenantId}/principals`
//! * `GET /internal/v1/operator/tenants/{tenantId}/origins`
//! * `GET /internal/v1/operator/principals`
//! * `GET /internal/v1/operator/oauth-apps`
//! * `GET /internal/v1/operator/oauth-apps/{appId}/grants`
//! * `GET /internal/v1/operator/audit`
//!
//! Real router, real service tokens: the bridge with `platform-identity.administer` and a
//! strict `hq:` operator reads; every other caller, a missing or malformed operator, and
//! attribution from a caller that may not claim an operator are refused. No read returns
//! a credential secret or verifier, or a principal's external reference.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::Value;
use uuid::Uuid;

const HQ: &str = "hq:staff-0001";

async fn bridge(h: &Harness, path: &str) -> Reply {
    h.call_with_headers(
        &h.peers.bridge,
        Method::GET,
        path,
        None,
        None,
        None,
        &[("x-cf-operator", HQ.to_string())],
    )
    .await
}

trait ExpectStatus {
    fn expect_status(self, status: StatusCode);
}

impl ExpectStatus for Reply {
    fn expect_status(self, status: StatusCode) {
        assert_eq!(self.status, status, "{}", self.body);
    }
}

fn items(r: &Reply) -> Vec<Value> {
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body["items"].as_array().unwrap().clone()
}

#[test]
fn the_trust_bundle_admits_the_bridge_with_administer() {
    // Builds the test trust policy (registry validation of every grant) without a database.
    let (_runtime, peers) = service_auth();
    assert_eq!(peers.bridge.issuer().as_str(), "cfmeet.admin-bridge");
}

#[tokio::test]
async fn the_bridge_reads_the_identity_hierarchy_without_secrets() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("bridge-reads", "EXTERNAL", &["calls.read"])
        .await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let reference = "partner-user-ada@example.test";
    let p = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{}/principals", c.env("SANDBOX")),
            Some(serde_json::json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": reference })),
        )
        .await;
    let principal_id = uuid(&p["principal"]["id"]);
    let t = c.tenant_id;

    let tenants = items(&bridge(&h, "/internal/v1/operator/tenants?q=bridge-reads").await);
    assert!(tenants.iter().any(|x| x["id"] == t.to_string()));
    assert_eq!(
        items(
            &bridge(
                &h,
                &format!("/internal/v1/operator/tenants/{t}/applications")
            )
            .await
        )
        .len(),
        1
    );
    assert_eq!(
        items(
            &bridge(
                &h,
                &format!("/internal/v1/operator/tenants/{t}/environments")
            )
            .await
        )
        .len(),
        3
    );
    let sas = items(
        &bridge(
            &h,
            &format!("/internal/v1/operator/tenants/{t}/service-accounts"),
        )
        .await,
    );
    assert_eq!(sas.len(), 3);
    assert!(sas.iter().all(|x| x.get("description").is_none()));
    let grants = items(
        &bridge(
            &h,
            &format!("/internal/v1/operator/tenants/{t}/scope-grants"),
        )
        .await,
    );
    assert!(grants.iter().any(|x| x["scope"] == "calls.read"));

    let creds = bridge(
        &h,
        &format!("/internal/v1/operator/tenants/{t}/credentials"),
    )
    .await;
    let list = items(&creds);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["lastFour"].as_str().unwrap().len(), 4);
    let text = creds.body.to_string();
    assert!(!text.contains(&secret), "never the secret");
    for k in [
        "verifier",
        "verifierAlgorithm",
        "verifierKeyVersion",
        "label",
    ] {
        assert!(list[0].get(k).is_none(), "{k} must not be relayed");
    }

    // Partner users: matched by the exact external reference, never echoed back.
    let users = bridge(
        &h,
        &format!("/internal/v1/operator/tenants/{t}/principals?kind=EXTERNAL_APPLICATION_PRINCIPAL&q={reference}"),
    )
    .await;
    let found = items(&users);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["id"], principal_id.to_string());
    assert!(
        !users.body.to_string().contains(reference),
        "the external reference is never returned"
    );
    let records = bridge(
        &h,
        &format!("/internal/v1/operator/principals?q={reference}"),
    )
    .await;
    assert_eq!(items(&records).len(), 1);
    assert!(!records.body.to_string().contains(reference));
    let by_id = bridge(
        &h,
        &format!("/internal/v1/operator/principals?q={principal_id}"),
    )
    .await;
    assert_eq!(items(&by_id).len(), 1);
    bridge(&h, "/internal/v1/operator/principals")
        .await
        .expect_status(StatusCode::BAD_REQUEST);

    let origins = items(&bridge(&h, &format!("/internal/v1/operator/tenants/{t}/origins")).await);
    assert_eq!(origins.len(), 3);
    assert!(origins
        .iter()
        .all(|o| o["origins"].as_array().unwrap().is_empty()));

    assert!(items(&bridge(&h, "/internal/v1/operator/oauth-apps").await).is_empty());
    bridge(
        &h,
        &format!("/internal/v1/operator/oauth-apps/{}/grants", Uuid::new_v4()),
    )
    .await
    .expect_status(StatusCode::NOT_FOUND);

    let audit = items(&bridge(&h, &format!("/internal/v1/operator/audit?tenantId={t}")).await);
    assert!(!audit.is_empty());
    assert!(audit.iter().all(|a| a.get("detail").is_none()));

    // Unknown tenants are 404.
    for path in [
        "applications",
        "environments",
        "service-accounts",
        "scope-grants",
        "credentials",
        "principals",
        "origins",
    ] {
        bridge(
            &h,
            &format!("/internal/v1/operator/tenants/{}/{path}", Uuid::new_v4()),
        )
        .await
        .expect_status(StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn pages_follow_the_keyset_cursor_and_bad_input_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("bridge-pages", "EXTERNAL", &[]).await;
    let t = c.tenant_id;
    let first = bridge(
        &h,
        &format!("/internal/v1/operator/tenants/{t}/environments?limit=2"),
    )
    .await;
    assert_eq!(items(&first).len(), 2);
    let next = first.body["nextCursor"].as_str().unwrap().to_string();
    let second = bridge(
        &h,
        &format!("/internal/v1/operator/tenants/{t}/environments?limit=2&cursor={next}"),
    )
    .await;
    assert_eq!(items(&second).len(), 1);
    assert!(second.body["nextCursor"].is_null());
    for bad in [
        format!("/internal/v1/operator/tenants/{t}/environments?limit=0"),
        format!("/internal/v1/operator/tenants/{t}/environments?limit=101"),
        format!("/internal/v1/operator/tenants/{t}/environments?cursor=nope"),
        format!("/internal/v1/operator/tenants/{t}/environments?unknown=1"),
        format!("/internal/v1/operator/tenants/{t}/principals?kind=ROBOT"),
        format!("/internal/v1/operator/tenants/{t}/scope-grants?cursor=no-colon"),
        "/internal/v1/operator/audit?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z"
            .to_string(),
        "/internal/v1/operator/audit?since=2026-11-20T00:00:00Z&until=2026-11-01T00:00:00Z"
            .to_string(),
        "/internal/v1/operator/audit?since=yesterday".to_string(),
        "/internal/v1/operator/audit?cursor=x".to_string(),
    ] {
        bridge(&h, &bad)
            .await
            .expect_status(StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_the_bridge_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("bridge-gates", "EXTERNAL", &[]).await;
    let path = format!("/internal/v1/operator/tenants/{}/environments", c.tenant_id);
    // Without platform-identity.administer.
    let r = h
        .call(&h.peers.calling, Method::GET, &path, None, None, None)
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // The operator console administers, but these reads are the bridge's (named HQ operator).
    let r = h.admin(Method::GET, &path, None).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::BAD_REQUEST, "OPERATOR_REQUIRED")
    );
    // The bridge without X-CF-Operator, or with a malformed one.
    let r = h
        .call(&h.peers.bridge, Method::GET, &path, None, None, None)
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let r = h
        .call_with_headers(
            &h.peers.bridge,
            Method::GET,
            &path,
            None,
            None,
            None,
            &[("x-cf-operator", "staff-0001".to_string())],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
}

#[tokio::test]
async fn attribution_from_a_caller_that_may_not_claim_an_operator_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("bridge-foreign", "EXTERNAL", &[]).await;
    let path = format!("/internal/v1/admin/applications/{}", c.application_id);
    // A service holding no operator role cannot send attribution, on any route.
    let r = h
        .call_with_headers(
            &h.peers.calling,
            Method::GET,
            &path,
            None,
            None,
            None,
            &[("x-cf-approver", "hq:staff-0002".to_string())],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // The operator console keeps its legacy named operator but never sends approvals.
    let r = h
        .call_with_headers(
            &h.peers.operator,
            Method::GET,
            &path,
            None,
            None,
            Some(OPERATOR),
            &[("x-cf-approver", "hq:staff-0002".to_string())],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    assert_eq!(
        h.admin(Method::GET, &path, None).await.status,
        StatusCode::OK
    );
    // The bridge's self-approval is refused before any handler.
    let r = h
        .call_with_headers(
            &h.peers.bridge,
            Method::GET,
            &path,
            None,
            None,
            None,
            &[
                ("x-cf-operator", HQ.to_string()),
                ("x-cf-approver", HQ.to_string()),
                ("x-cf-approval-ref", "appr-1".to_string()),
                ("x-cf-intent-digest", "a".repeat(64)),
            ],
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
}
