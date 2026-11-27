//! ADR 0058 Amendment A2 — automatic organization service bindings.
//!
//! * `/internal/v1/organization-scopes/{org}/primary/service-bindings/{service}` answers
//!   only `cfmeet.organizations` holding `platform-identity.bind-organization-services`
//!   (the scope permission alone is not enough);
//! * a binding is created inside the organization's own triple with the closed scope set,
//!   idempotently (repeated and concurrent calls return one binding) and the raw credential
//!   is never returned;
//! * renewal (near expiry), rotation and revocation replace / end the credential so the
//!   predecessor no longer authenticates; revocation is terminal and idempotent;
//! * a suspended tenant refuses new bindings and reads back as inactive;
//! * a same-named service account outside the registry is never adopted;
//! * every change is audited with actor `cfmeet.organizations` and the organization id.

mod common;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::*;
use platform_access::domain::CredentialId;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

fn scope_path(org: Uuid) -> String {
    format!("/internal/v1/organization-scopes/{org}/primary")
}

fn binding_path(org: Uuid, service: &str) -> String {
    format!("{}/service-bindings/{service}", scope_path(org))
}

async fn as_org(h: &Harness, method: Method, path: &str, body: Option<Value>) -> Reply {
    h.call(&h.peers.organizations, method, path, body, None, None)
        .await
}

async fn scalar(h: &Harness, sql: String) -> i64 {
    h.conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn provision(h: &Harness, org: Uuid) -> Value {
    let r = as_org(h, Method::PUT, &scope_path(org), None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r.body
}

async fn authenticates(h: &Harness, credential: &Value) -> bool {
    let id = CredentialId::from_uuid(Uuid::parse_str(credential.as_str().unwrap()).unwrap());
    h.state
        .platform
        .credential(id)
        .await
        .unwrap()
        .effective_state
        .authenticates()
}

fn assert_no_secret(body: &Value) {
    let text = body.to_string();
    for forbidden in ["secret", "verifier", "publicId", "masked", "cfk_", "token"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}

#[tokio::test]
async fn bindings_are_idempotent_inside_the_organization_triple_and_never_return_a_secret() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    let scope = provision(&h, org).await;
    let first = as_org(&h, Method::PUT, &binding_path(org, "calling"), None).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let v = &first.body;
    assert_no_secret(v);
    assert_eq!(v["organizationRef"], org.to_string());
    assert_eq!(v["capability"], "primary");
    assert_eq!(v["service"], "calling");
    for k in ["tenantId", "applicationId", "environmentId"] {
        assert_eq!(v[k], scope[k], "{k}");
    }
    let required = json!([
        "calls.read",
        "enterprise.config.read",
        "enterprise.config.write"
    ]);
    assert_eq!(v["requiredScopes"], required);
    assert_eq!(v["scopes"], required);
    assert_eq!(v["verified"], true);
    assert_eq!(v["active"], true);
    assert_eq!(v["revision"], 1);
    assert_eq!(v["revokedAt"], Value::Null);
    assert_eq!(v["serviceAccount"]["status"], "ACTIVE");
    assert_eq!(v["serviceAccount"]["id"], v["serviceAccountId"]);
    assert_eq!(v["credential"]["id"], v["credentialId"]);
    assert_eq!(v["credential"]["status"], "ACTIVE");
    assert_eq!(v["credential"]["authenticates"], true);
    assert!(authenticates(&h, &v["credentialId"]).await);
    // The service account and credential live in the organization's own environment.
    let env = scope["environmentId"].as_str().unwrap();
    assert_eq!(
        scalar(
            &h,
            format!(
                "SELECT count(*) FROM platform_service_accounts WHERE environment_id = '{env}' AND name = 'org-calling'"
            )
        )
        .await,
        1
    );
    // Repeat (lost response): 200, same ids, nothing new.
    let again = as_org(&h, Method::PUT, &binding_path(org, "calling"), None).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    for k in ["serviceAccountId", "credentialId", "revision"] {
        assert_eq!(again.body[k], v[k], "{k}");
    }
    let read = as_org(&h, Method::GET, &binding_path(org, "calling"), None).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["credentialId"], v["credentialId"]);
    assert_no_secret(&read.body);
    // Concurrent first calls for Messaging serialize into one binding.
    let p = binding_path(org, "messaging");
    let replies =
        futures_util::future::join_all((0..4).map(|_| as_org(&h, Method::PUT, &p, None))).await;
    assert_eq!(
        replies
            .iter()
            .filter(|r| r.status == StatusCode::CREATED)
            .count(),
        1,
        "exactly one creator"
    );
    let creds: std::collections::BTreeSet<_> = replies
        .iter()
        .map(|r| {
            assert!(r.status.is_success(), "{}", r.body);
            assert_no_secret(&r.body);
            r.body["credentialId"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(creds.len(), 1);
    assert_eq!(replies[0].body["scopes"], json!(["messages.read"]));
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_organization_service_bindings".into()
        )
        .await,
        2
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_api_credentials".into()).await,
        2
    );
    // Audit: actor Organizations, the organization as an opaque reference.
    let audit = |action: &str| {
        format!(
            "SELECT count(*) FROM platform_access_audit WHERE action = '{action}' AND actor = 'cfmeet.organizations' AND detail->>'organizationRef' = '{org}'"
        )
    };
    assert_eq!(scalar(&h, audit("SERVICE_ACCOUNT_CREATED")).await, 2);
    assert_eq!(scalar(&h, audit("CREDENTIAL_CREATED")).await, 2);
    assert_eq!(scalar(&h, audit("SCOPE_GRANTED")).await, 4);
    // Unknown service, unknown organization scope, nil and malformed ids.
    let bad = as_org(&h, Method::PUT, &binding_path(org, "telephony"), None).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    assert_eq!(bad.code(), "VALIDATION_ERROR");
    let bad_cap = as_org(
        &h,
        Method::PUT,
        &format!("/internal/v1/organization-scopes/{org}/calling/service-bindings/calling"),
        None,
    )
    .await;
    assert_eq!(bad_cap.code(), "VALIDATION_ERROR");
    let unprovisioned = Uuid::new_v4();
    for method in [Method::PUT, Method::GET, Method::DELETE] {
        let r = as_org(&h, method, &binding_path(unprovisioned, "calling"), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    }
    let rotate_unknown = as_org(
        &h,
        Method::POST,
        &format!("{}/rotate", binding_path(unprovisioned, "calling")),
        None,
    )
    .await;
    assert_eq!(rotate_unknown.status, StatusCode::NOT_FOUND);
    // Provisioned scope but never bound: GET 404.
    let other = Uuid::new_v4();
    provision(&h, other).await;
    let never = as_org(&h, Method::GET, &binding_path(other, "messaging"), None).await;
    assert_eq!(never.status, StatusCode::NOT_FOUND);
    let nil = as_org(&h, Method::PUT, &binding_path(Uuid::nil(), "calling"), None).await;
    assert_eq!(nil.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn only_organizations_with_the_binding_permission_reach_bindings() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    provision(&h, org).await;
    let p = binding_path(org, "calling");
    for signer in [
        &h.peers.developer_platform,
        &h.peers.operator,
        &h.peers.calling,
        &h.peers.meeting,
        &h.peers.gateway,
        &h.peers.messaging,
    ] {
        for (method, path) in [
            (Method::PUT, p.clone()),
            (Method::GET, p.clone()),
            (Method::DELETE, p.clone()),
            (Method::POST, format!("{p}/rotate")),
        ] {
            let r = h.call(signer, method, &path, None, None, None).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.body);
            assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
        }
    }
    let r = h
        .call(&h.peers.untrusted, Method::PUT, &p, None, None, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_service_accounts".into()).await,
        0
    );

    // Organizations holding only the scope permission cannot bind.
    let Some(h2) =
        Harness::with_organizations_permissions(&["platform-identity.provision-first-party"]).await
    else {
        return;
    };
    let org2 = Uuid::new_v4();
    provision(&h2, org2).await;
    let r = as_org(&h2, Method::PUT, &binding_path(org2, "calling"), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    assert_eq!(
        scalar(&h2, "SELECT count(*) FROM platform_service_accounts".into()).await,
        0
    );
}

#[tokio::test]
async fn rotation_renewal_and_revocation_end_the_predecessor() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    provision(&h, org).await;
    let p = binding_path(org, "messaging");
    let created = as_org(&h, Method::PUT, &p, None).await.body;
    let c1 = created["credentialId"].clone();

    // Forced rotation.
    let rotated = as_org(&h, Method::POST, &format!("{p}/rotate"), None).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_no_secret(&rotated.body);
    let c2 = rotated.body["credentialId"].clone();
    assert_ne!(c1, c2);
    assert_eq!(
        rotated.body["serviceAccountId"],
        created["serviceAccountId"]
    );
    assert_eq!(rotated.body["revision"], 2);
    assert_eq!(rotated.body["active"], true);
    assert_eq!(rotated.body["scopes"], json!(["messages.read"]));
    assert!(
        !authenticates(&h, &c1).await,
        "predecessor still authenticates"
    );
    assert!(authenticates(&h, &c2).await);

    // A fresh credential is left alone by PUT; near expiry PUT renews it.
    let same = as_org(&h, Method::PUT, &p, None).await;
    assert_eq!(same.status, StatusCode::OK);
    assert_eq!(same.body["credentialId"], c2);
    h.clock.advance(Duration::days(340));
    let renewed = as_org(&h, Method::PUT, &p, None).await;
    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.body);
    let c3 = renewed.body["credentialId"].clone();
    assert_ne!(c3, c2);
    assert_eq!(renewed.body["revision"], 3);
    assert_eq!(renewed.body["active"], true);
    assert!(!authenticates(&h, &c2).await);
    // An expired credential is replaced too (no overlap, fresh issue).
    h.clock.advance(Duration::days(400));
    let expired = as_org(&h, Method::GET, &p, None).await;
    assert_eq!(expired.body["active"], false);
    assert_eq!(expired.body["credential"]["status"], "EXPIRED");
    let replaced = as_org(&h, Method::PUT, &p, None).await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.body);
    let c4 = replaced.body["credentialId"].clone();
    assert_ne!(c4, c3);
    assert_eq!(replaced.body["active"], true);

    // Revocation: terminal and idempotent.
    let revoked = as_org(&h, Method::DELETE, &p, None).await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["active"], false);
    assert_eq!(revoked.body["credential"]["authenticates"], false);
    assert_eq!(revoked.body["serviceAccount"]["status"], "REVOKED");
    assert!(revoked.body["revokedAt"].is_string());
    assert!(!authenticates(&h, &c4).await);
    let revision = revoked.body["revision"].clone();
    let again = as_org(&h, Method::DELETE, &p, None).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["revision"], revision, "idempotent");
    let reprovision = as_org(&h, Method::PUT, &p, None).await;
    assert_eq!(
        reprovision.status,
        StatusCode::CONFLICT,
        "{}",
        reprovision.body
    );
    let rotate = as_org(&h, Method::POST, &format!("{p}/rotate"), None).await;
    assert_eq!(rotate.status, StatusCode::CONFLICT);
    let audit = |action: &str| {
        format!(
            "SELECT count(*) FROM platform_access_audit WHERE action = '{action}' AND actor = 'cfmeet.organizations' AND detail->>'organizationRef' = '{org}' AND detail->>'service' = 'messaging'"
        )
    };
    assert_eq!(scalar(&h, audit("CREDENTIAL_ROTATED")).await, 2);
    assert_eq!(scalar(&h, audit("CREDENTIAL_REVOKED")).await, 4);
    assert_eq!(scalar(&h, audit("SERVICE_ACCOUNT_STATUS_SET")).await, 1);
}

#[tokio::test]
async fn a_suspended_tenant_refuses_bindings_and_reads_back_inactive() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    provision(&h, a).await;
    provision(&h, b).await;
    let bound = as_org(&h, Method::PUT, &binding_path(a, "calling"), None).await;
    assert_eq!(bound.status, StatusCode::CREATED);
    let s = as_org(
        &h,
        Method::POST,
        &format!("{}/status", scope_path(a)),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    let read = as_org(&h, Method::GET, &binding_path(a, "calling"), None).await;
    assert_eq!(read.body["active"], false);
    assert_eq!(read.body["verified"], true);
    assert_eq!(read.body["credential"]["authenticates"], false);
    for service in ["calling", "messaging"] {
        let r = as_org(&h, Method::PUT, &binding_path(a, service), None).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{service}: {}", r.body);
    }
    // Another organization is unaffected; resumption restores the binding.
    let rb = as_org(&h, Method::PUT, &binding_path(b, "calling"), None).await;
    assert_eq!(rb.status, StatusCode::CREATED);
    assert_ne!(rb.body["serviceAccountId"], bound.body["serviceAccountId"]);
    as_org(
        &h,
        Method::POST,
        &format!("{}/status", scope_path(a)),
        Some(json!({ "status": "ACTIVE" })),
    )
    .await;
    let resumed = as_org(&h, Method::PUT, &binding_path(a, "calling"), None).await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.body);
    assert_eq!(resumed.body["credentialId"], bound.body["credentialId"]);
    assert_eq!(resumed.body["active"], true);
}

#[tokio::test]
async fn a_same_named_service_account_outside_the_registry_is_never_adopted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    let scope = provision(&h, org).await;
    let env = scope["environmentId"].as_str().unwrap();
    let sa = h
        .admin(
            Method::POST,
            &format!("/internal/v1/admin/environments/{env}/service-accounts"),
            Some(json!({ "name": "org-calling" })),
        )
        .await;
    assert!(sa.status.is_success(), "{}", sa.body);
    let r = as_org(&h, Method::PUT, &binding_path(org, "calling"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_organization_service_bindings".into()
        )
        .await,
        0
    );
    // Nothing half-created: no credential, no grant from the refused transaction.
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_api_credentials".into()).await,
        0
    );
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_application_scopes".into()
        )
        .await,
        0
    );
    // The other service binds normally.
    let m = as_org(&h, Method::PUT, &binding_path(org, "messaging"), None).await;
    assert_eq!(m.status, StatusCode::CREATED, "{}", m.body);
}
