//! Service boundaries and isolation (Part IV §B6.5–8): unauthenticated and untrusted
//! callers, customer credentials on the internal API, permission least privilege,
//! forged tenant / application / environment claims, cross-tenant and cross-environment
//! access.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext};
use common::*;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn every_internal_path_requires_a_valid_service_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    for path in [
        "/internal/v1/credentials/authenticate",
        "/internal/v1/identity/changes",
        "/internal/v1/admin/tenants",
        "/internal/v1/does-not-exist",
    ] {
        // No token.
        let r = h
            .raw(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
        // A CUSTOMER credential is never an internal credential.
        let r = h
            .raw(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // A correctly signing but untrusted service, and a trusted name with a foreign key.
    for signer in [&h.peers.untrusted, &h.peers.rogue_calling] {
        let r = h
            .call(
                signer,
                Method::POST,
                "/internal/v1/credentials/authenticate",
                Some(json!({ "credential": secret })),
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    // Probes stay public and say nothing sensitive.
    let r = h
        .raw(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = h
        .raw(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["migrations"], "current");
}

#[tokio::test]
async fn permissions_are_least_privilege_per_caller() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    // Calling and Numbering cannot administer identity.
    for signer in [&h.peers.calling, &h.peers.numbering] {
        let r = h
            .call(
                signer,
                Method::POST,
                "/internal/v1/admin/tenants",
                Some(json!({ "slug": "x", "displayName": "X", "classification": "EXTERNAL" })),
                None,
                Some(OPERATOR),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    }
    // The operator console cannot authenticate customer credentials or read the feed.
    let r = h
        .call(
            &h.peers.operator,
            Method::POST,
            "/internal/v1/credentials/authenticate",
            Some(json!({ "credential": "x" })),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Numbering cannot register / tombstone principals (Calling only).
    let r = h
        .call(
            &h.peers.numbering,
            Method::POST,
            "/internal/v1/principals/telephone-endpoints",
            Some(json!({ "contactReference": Uuid::new_v4() })),
            Some(c.delegated("SANDBOX")),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Administration needs a named operator.
    let r = h
        .call(
            &h.peers.operator,
            Method::POST,
            "/internal/v1/admin/tenants",
            Some(json!({ "slug": "x", "displayName": "X", "classification": "EXTERNAL" })),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
}

#[tokio::test]
async fn forged_or_inconsistent_delegated_contexts_are_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let b = h.consumer("globex", "EXTERNAL", &["calls.read"]).await;
    let forged = [
        // Tenant of A, application and environment of B.
        DelegatedPlatformContext::new(
            a.tenant_id,
            b.application_id,
            b.env("SANDBOX"),
            ApplicationEnvironmentKind::Sandbox,
            b.sa("SANDBOX"),
        ),
        // A's environment, B's service account.
        DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.env("SANDBOX"),
            ApplicationEnvironmentKind::Sandbox,
            b.sa("SANDBOX"),
        ),
        // Kind claim does not match the environment.
        DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.env("SANDBOX"),
            ApplicationEnvironmentKind::Production,
            a.sa("SANDBOX"),
        ),
        // A's sandbox service account claimed in A's production environment.
        DelegatedPlatformContext::new(
            a.tenant_id,
            a.application_id,
            a.env("PRODUCTION"),
            ApplicationEnvironmentKind::Production,
            a.sa("SANDBOX"),
        ),
        // Ids that do not exist.
        DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            ApplicationEnvironmentKind::Sandbox,
            Uuid::new_v4(),
        ),
    ];
    for ctx in forged {
        let r = h
            .as_calling(
                Method::GET,
                "/internal/v1/identity/context",
                None,
                Some(ctx),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
        assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID");
    }
    // No delegated context at all.
    let r = h
        .as_calling(Method::GET, "/internal/v1/identity/context", None, None)
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_REQUIRED");
    // A consistent context resolves to the trusted chain.
    let r = h
        .as_calling(
            Method::GET,
            "/internal/v1/identity/context",
            None,
            Some(a.delegated("TEST")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(uuid(&r.body["tenant"]["id"]), a.tenant_id);
    assert_eq!(r.body["environment"]["kind"], "TEST");
    assert_eq!(r.body["authorityMode"], "FULL");
}

#[tokio::test]
async fn a_request_body_can_never_choose_the_tenant() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    // Unknown fields (e.g. a tenantId in the body) are refused, not ignored.
    let r = h
        .as_calling(Method::POST, "/internal/v1/principals/resolve",
            Some(json!({ "tenantId": a.tenant_id, "references": [{ "principalId": Uuid::new_v4() }] })),
            Some(a.delegated("SANDBOX")))
        .await;
    assert!(r.status.is_client_error(), "{}", r.body);
}

#[tokio::test]
async fn principals_and_credentials_of_other_tenants_and_environments_are_invisible() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let b = h.consumer("globex", "EXTERNAL", &["calls.read"]).await;
    let a_env = a.env("SANDBOX");
    let p = h
        .admin_ok(Method::POST, &format!("/internal/v1/admin/environments/{a_env}/principals"),
            Some(json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": "member-1" })))
        .await;
    let pid = uuid(&p["principal"]["id"]);
    let resolve = |kind: serde_json::Value| json!({ "references": [{ "principalId": pid }, kind] });
    let by_ref =
        json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": "member-1" });
    // Own scope: resolved by id and by reference.
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve(by_ref.clone())),
            Some(a.delegated("SANDBOX")),
        )
        .await;
    assert_eq!(r.body["results"][0]["outcome"], "RESOLVED", "{}", r.body);
    assert_eq!(r.body["results"][1]["outcome"], "RESOLVED");
    // Other tenant, other environment of the same application: indistinguishable from missing.
    for ctx in [b.delegated("SANDBOX"), a.delegated("PRODUCTION")] {
        let r = h
            .as_calling(
                Method::POST,
                "/internal/v1/principals/resolve",
                Some(resolve(by_ref.clone())),
                Some(ctx),
            )
            .await;
        assert_eq!(r.body["results"][0]["outcome"], "NOT_FOUND");
        assert_eq!(r.body["results"][1]["outcome"], "NOT_FOUND");
    }
    // Credential status of another tenant: NOT_FOUND.
    let (view, _) = h.issue(a.sa("SANDBOX"), &["calls.read"]).await;
    let path = format!("/internal/v1/credentials/{}", view["id"].as_str().unwrap());
    assert_eq!(
        h.as_calling(Method::GET, &path, None, Some(a.delegated("SANDBOX")))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.as_calling(Method::GET, &path, None, Some(b.delegated("SANDBOX")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.as_calling(Method::GET, &path, None, Some(a.delegated("TEST")))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // A sandbox credential presented in production context still authenticates only as
    // its own (sandbox) environment: environment comes from the credential, never input.
    let (_, secret) = h.issue(a.sa("SANDBOX"), &["calls.read"]).await;
    let r = h.authenticate(&secret).await;
    assert_eq!(
        uuid(&r.body["authority"]["environment"]["id"]),
        a.env("SANDBOX")
    );
}

#[tokio::test]
async fn principal_tombstones_are_scoped_and_irreversible() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("acme", "EXTERNAL", &[]).await;
    let b = h.consumer("globex", "EXTERNAL", &[]).await;
    let env = a.env("SANDBOX");
    let p = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{env}/principals"),
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "person-9" })),
        )
        .await;
    let pid = uuid(&p["principal"]["id"]);
    let path = format!("/internal/v1/principals/{pid}/tombstone");
    // Only a caller granted `platform-principal.register` (Calling), naming its operator.
    let r = h
        .call(
            &h.peers.numbering,
            Method::POST,
            &path,
            Some(json!({ "operator": OPERATOR })),
            Some(b.delegated("SANDBOX")),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = h
        .as_calling(Method::POST, &path, Some(json!({ "operator": "" })), None)
        .await;
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    let r = h
        .as_calling(
            Method::POST,
            &format!("/internal/v1/principals/{}/tombstone", Uuid::new_v4()),
            Some(json!({ "operator": OPERATOR })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h
        .as_calling(
            Method::POST,
            &path,
            Some(json!({ "operator": OPERATOR })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["principal"]["status"], "DEACTIVATED");
    assert_eq!(
        r.body["principal"]["externalReference"],
        format!("tombstoned:{pid}")
    );
    // Idempotent; and it can never be reactivated.
    let again = h
        .as_calling(
            Method::POST,
            &path,
            Some(json!({ "operator": OPERATOR })),
            None,
        )
        .await;
    assert_eq!(again.body["alreadyTombstoned"], true);
    let r = h
        .admin(
            Method::POST,
            &format!("/internal/v1/admin/principals/{pid}/status"),
            Some(json!({ "status": "ACTIVE" })),
        )
        .await;
    assert!(r.status.is_client_error());
}
