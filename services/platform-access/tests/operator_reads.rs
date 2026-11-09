//! INV-002: the operator / delegation reads and the operator principal tombstone that
//! the operation-coverage baseline still listed as untested.
//!
//! * `GET /internal/v1/admin/applications/{applicationId}`
//! * `POST /internal/v1/admin/principals/{principalId}/tombstone`
//! * `GET /internal/v1/delegation/applications/{applicationId}`
//!
//! Each route is exercised through the real router with real service tokens: missing
//! token (401), a trusted peer without the route's permission (403), the missing operator
//! / actor header where a mutation needs one, a malformed id (400), an unknown id (404)
//! and the success path.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ACTOR: &str = "u-0123456789abcdef0123456789abcdef";

/// A Developer Platform delegation call (`platform-identity.delegate`).
async fn dp(h: &Harness, method: Method, path: &str, actor: bool) -> Reply {
    let outbound = cf_service_auth::OutboundServiceRequest::new_operation(
        cf_service_auth::ServiceId::platform_access(),
    );
    let signed = h.peers.developer_platform.sign(&outbound).unwrap();
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in signed.headers().iter() {
        builder = builder.header(name, value);
    }
    if actor {
        builder = builder.header("x-cf-delegated-actor", ACTOR);
    }
    h.raw(builder.body(Body::empty()).unwrap()).await
}

async fn unsigned(h: &Harness, method: Method, path: &str) -> Reply {
    h.raw(
        Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn operator_reads_an_application_by_id() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme-read", "EXTERNAL", &["calls.read"]).await;
    let path = format!("/internal/v1/admin/applications/{}", c.application_id);

    // No service token.
    let r = unsigned(&h, Method::GET, &path).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
    // A trusted peer without platform-identity.administer.
    for signer in [&h.peers.calling, &h.peers.developer_platform] {
        let r = h
            .call(signer, Method::GET, &path, None, None, Some(OPERATOR))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    }
    // An unknown key under a trusted identity name is not authenticated.
    let r = h
        .call(&h.peers.rogue_calling, Method::GET, &path, None, None, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Malformed and unknown ids.
    let r = h
        .admin(
            Method::GET,
            "/internal/v1/admin/applications/not-a-uuid",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = h
        .admin(
            Method::GET,
            &format!("/internal/v1/admin/applications/{}", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.code(), "NOT_FOUND");

    // Success: a read needs no operator header, and returns the stored application.
    let r = h
        .call(&h.peers.operator, Method::GET, &path, None, None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(uuid(&r.body["id"]), c.application_id);
    assert_eq!(uuid(&r.body["tenantId"]), c.tenant_id);
    assert_eq!(r.body["slug"], "app");
    assert_eq!(r.body["status"], "ACTIVE");

    // The read reflects an operator status change.
    h.admin_ok(
        Method::POST,
        &format!("{path}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = h.admin(Method::GET, &path, None).await;
    assert_eq!(r.body["status"], "SUSPENDED", "{}", r.body);
}

async fn principal(h: &Harness, env: Uuid, reference: &str) -> Uuid {
    let p = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{env}/principals"),
            Some(
                json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": reference }),
            ),
        )
        .await;
    uuid(&p["principal"]["id"])
}

async fn audit_count(h: &Harness, principal: Uuid) -> i64 {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    h.conn()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM platform_access_audit WHERE target_id = $1 AND action = 'PRINCIPAL_TOMBSTONED'",
            [principal.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

#[tokio::test]
async fn operator_tombstones_a_principal_once_and_idempotently() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme-tomb", "EXTERNAL", &["calls.read"]).await;
    let pid = principal(&h, c.env("SANDBOX"), "member-to-erase").await;
    let path = format!("/internal/v1/admin/principals/{pid}/tombstone");

    // 401 / 403 / missing operator: nothing changes.
    let r = unsigned(&h, Method::POST, &path).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
    let r = h
        .call(
            &h.peers.calling,
            Method::POST,
            &path,
            None,
            None,
            Some(OPERATOR),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let r = h
        .call(&h.peers.operator, Method::POST, &path, None, None, None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    assert_eq!(h.scalar_principals("member-to-erase").await, 1);

    // Malformed / unknown principal.
    let r = h
        .admin(
            Method::POST,
            "/internal/v1/admin/principals/xyz/tombstone",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = h
        .admin(
            Method::POST,
            &format!("/internal/v1/admin/principals/{}/tombstone", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.code(), "NOT_FOUND");

    // Success: the external reference is erased (replaced by a non-identifying one).
    let r = h.admin(Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(uuid(&r.body["principal"]["id"]), pid);
    assert_eq!(r.body["alreadyTombstoned"], false);
    assert_eq!(r.body["principal"]["status"], "DEACTIVATED", "{}", r.body);
    assert!(
        r.body["principal"]["tombstonedAt"].is_string(),
        "{}",
        r.body
    );
    assert_eq!(
        r.body["principal"]["externalReference"],
        format!("tombstoned:{pid}")
    );
    assert_eq!(h.scalar_principals("member-to-erase").await, 0);
    assert_eq!(audit_count(&h, pid).await, 1);

    // Replay: idempotent, reported as already tombstoned, still audited.
    let again: Value = h.admin_ok(Method::POST, &path, None).await;
    assert_eq!(again["alreadyTombstoned"], true);
    assert_eq!(again["principal"]["status"], "DEACTIVATED");
    assert_eq!(audit_count(&h, pid).await, 2);
}

#[tokio::test]
async fn the_developer_platform_reads_an_application_by_id() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme-dp", "EXTERNAL", &[]).await;
    let path = format!("/internal/v1/delegation/applications/{}", c.application_id);

    let r = unsigned(&h, Method::GET, &path).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
    // The operator console administers but does not hold platform-identity.delegate.
    for signer in [&h.peers.operator, &h.peers.calling, &h.peers.gateway] {
        let r = h
            .call(signer, Method::GET, &path, None, None, Some(OPERATOR))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    }
    let r = h
        .call(&h.peers.untrusted, Method::GET, &path, None, None, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);

    let r = dp(
        &h,
        Method::GET,
        "/internal/v1/delegation/applications/123",
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/delegation/applications/{}", Uuid::new_v4()),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.code(), "NOT_FOUND");

    // A read needs no named actor.
    let r = dp(&h, Method::GET, &path, false).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(uuid(&r.body["id"]), c.application_id);
    assert_eq!(uuid(&r.body["tenantId"]), c.tenant_id);
    assert_eq!(r.body["status"], "ACTIVE");
}
