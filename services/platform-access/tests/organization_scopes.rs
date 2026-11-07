//! Blueprint Section 7 (ADR 0058) — governed FIRST_PARTY organization scopes.
//!
//! * `/internal/v1/organization-scopes/**` answers only `cfmeet.organizations` holding
//!   `platform-identity.provision-first-party`; the Developer Platform (EXTERNAL
//!   delegation), the operator console and other services are refused;
//! * provisioning is idempotent on `(organization, capability)` — repeated and concurrent
//!   calls return one triple — and two organizations always get distinct triples;
//! * the triple is FIRST_PARTY / PRODUCTION, issues no credential and grants no scope;
//! * suspension / resumption acts on the tenant, idempotently, with audit rows carrying
//!   the organization id as an opaque reference;
//! * a tenant holding the reserved slug outside the registry is never adopted.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

fn path(org: Uuid) -> String {
    format!("/internal/v1/organization-scopes/{org}/primary")
}

async fn as_org(h: &Harness, method: Method, path: &str, body: Option<serde_json::Value>) -> Reply {
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

#[tokio::test]
async fn provisioning_is_idempotent_first_party_and_distinct_per_organization() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let first = as_org(&h, Method::PUT, &path(a), None).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let v = &first.body;
    assert_eq!(v["organizationRef"], a.to_string());
    assert_eq!(v["capability"], "primary");
    assert_eq!(v["tenant"]["classification"], "FIRST_PARTY");
    assert_eq!(v["tenant"]["slug"], format!("org-{}", a.simple()));
    assert_eq!(v["environment"]["kind"], "PRODUCTION");
    assert_eq!(v["verified"], true);
    assert_eq!(v["active"], true);
    assert_eq!(v["requestedBy"], "cfmeet.organizations");
    // A repeat (lost response, retry) returns the same triple with 200.
    let again = as_org(&h, Method::PUT, &path(a), None).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    for k in ["tenantId", "applicationId", "environmentId"] {
        assert_eq!(again.body[k], v[k], "{k}");
    }
    // Concurrent first calls for another organization serialize into one triple.
    let pb = path(b);
    let calls = (0..4).map(|_| as_org(&h, Method::PUT, &pb, None));
    let replies = futures_util::future::join_all(calls).await;
    let created = replies
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    assert_eq!(created, 1, "exactly one creator");
    let tenants: std::collections::BTreeSet<_> = replies
        .iter()
        .map(|r| {
            assert!(r.status.is_success(), "{}", r.body);
            r.body["tenantId"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(tenants.len(), 1);
    // Two organizations never share any level of the triple.
    let tb = replies[0].body.clone();
    for k in ["tenantId", "applicationId", "environmentId"] {
        assert_ne!(tb[k], v[k], "{k} shared across organizations");
    }
    // The read-back matches.
    let read = as_org(&h, Method::GET, &path(a), None).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["tenantId"], v["tenantId"]);
    let missing = as_org(&h, Method::GET, &path(Uuid::new_v4()), None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.code(), "NOT_FOUND");
    // Exactly one tenant / application / environment per organization; no credential,
    // service account, principal or scope grant was created by this path.
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_organization_scopes".into()
        )
        .await,
        2
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_tenants WHERE classification = 'FIRST_PARTY' AND slug LIKE 'org-%'".into()).await,
        2
    );
    for table in [
        "platform_api_credentials",
        "platform_service_accounts",
        "platform_principals",
        "platform_application_scopes",
    ] {
        assert_eq!(
            scalar(&h, format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    // The audit names the organization as an opaque reference, once per creation.
    assert_eq!(
        scalar(
            &h,
            format!(
                "SELECT count(*) FROM platform_access_audit WHERE action = 'ORGANIZATION_SCOPE_PROVISIONED' AND detail->>'organizationRef' = '{a}' AND actor = 'cfmeet.organizations'"
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &h,
            format!("SELECT count(*) FROM platform_access_audit WHERE detail->>'organizationRef' = '{a}'")
        )
        .await,
        4,
        "tenant, application, environment and scope rows"
    );
    // Unknown capability and malformed ids are refused before anything is written.
    let bad = as_org(
        &h,
        Method::PUT,
        &format!(
            "/internal/v1/organization-scopes/{}/calling",
            Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(bad.code(), "VALIDATION_ERROR");
    let nil = as_org(&h, Method::PUT, &path(Uuid::nil()), None).await;
    assert_eq!(nil.code(), "VALIDATION_ERROR");
    let malformed = as_org(
        &h,
        Method::PUT,
        "/internal/v1/organization-scopes/not-a-uuid/primary",
        None,
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    // The scope row is immutable.
    assert!(h
        .conn()
        .execute_unprepared("UPDATE platform_organization_scopes SET capability = 'other'")
        .await
        .is_err());
    assert!(h
        .conn()
        .execute_unprepared("DELETE FROM platform_organization_scopes")
        .await
        .is_err());
}

#[tokio::test]
async fn only_the_organizations_service_reaches_organization_scopes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    for signer in [
        &h.peers.developer_platform,
        &h.peers.operator,
        &h.peers.calling,
        &h.peers.meeting,
        &h.peers.gateway,
    ] {
        for (method, p, body) in [
            (Method::PUT, path(org), None),
            (Method::GET, path(org), None),
            (
                Method::POST,
                format!("{}/status", path(org)),
                Some(json!({ "status": "SUSPENDED" })),
            ),
        ] {
            let r = h.call(signer, method, &p, body, None, None).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{p}: {}", r.body);
            assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
        }
    }
    // Untrusted / rogue keys never authenticate.
    let r = h
        .call(
            &h.peers.untrusted,
            Method::PUT,
            &path(org),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Nothing was created.
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_tenants".into()).await,
        0
    );
    // The EXTERNAL delegation path still cannot reach a FIRST_PARTY organization tenant's
    // PRODUCTION environment (it is not an approved production activation).
    let created = as_org(&h, Method::PUT, &path(org), None).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let env = created.body["environmentId"].as_str().unwrap().to_string();
    let r = h
        .call(
            &h.peers.developer_platform,
            Method::POST,
            &format!("/internal/v1/delegation/environments/{env}/status"),
            Some(json!({ "status": "SUSPENDED" })),
            None,
            None,
        )
        .await;
    assert_ne!(r.status, StatusCode::OK, "{}", r.body);
}

#[tokio::test]
async fn suspension_follows_the_tenant_idempotently_and_isolates_organizations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let va = as_org(&h, Method::PUT, &path(a), None).await.body;
    let vb = as_org(&h, Method::PUT, &path(b), None).await.body;
    let status = format!("{}/status", path(a));
    let s = as_org(
        &h,
        Method::POST,
        &status,
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    assert_eq!(s.body["tenant"]["status"], "SUSPENDED");
    assert_eq!(s.body["active"], false);
    assert_eq!(s.body["verified"], true);
    // Repeating writes nothing new.
    as_org(
        &h,
        Method::POST,
        &status,
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let audits = |org: Uuid| {
        format!(
            "SELECT count(*) FROM platform_access_audit WHERE action = 'TENANT_STATUS_SET' AND detail->>'organizationRef' = '{org}'"
        )
    };
    assert_eq!(scalar(&h, audits(a)).await, 1);
    // Organization B is untouched.
    let rb = as_org(&h, Method::GET, &path(b), None).await;
    assert_eq!(rb.body["tenant"]["status"], "ACTIVE");
    assert_eq!(rb.body["tenantId"], vb["tenantId"]);
    assert_eq!(scalar(&h, audits(b)).await, 0);
    // Resume.
    let r = as_org(
        &h,
        Method::POST,
        &status,
        Some(json!({ "status": "ACTIVE" })),
    )
    .await;
    assert_eq!(r.body["tenant"]["status"], "ACTIVE");
    assert_eq!(r.body["tenantId"], va["tenantId"]);
    assert_eq!(scalar(&h, audits(a)).await, 2);
    // Unknown scope and invalid statuses.
    let unknown = as_org(
        &h,
        Method::POST,
        &format!("{}/status", path(Uuid::new_v4())),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(unknown.code(), "NOT_FOUND");
    let invalid = as_org(
        &h,
        Method::POST,
        &status,
        Some(json!({ "status": "DELETED" })),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_foreign_tenant_holding_the_reserved_slug_is_never_adopted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = Uuid::new_v4();
    // An operator (or anyone) created a tenant with the organization's reserved slug.
    let t = h
        .admin(
            Method::POST,
            "/internal/v1/admin/tenants",
            Some(json!({
                "slug": format!("org-{}", org.simple()),
                "displayName": "Squatter",
                "classification": "EXTERNAL",
            })),
        )
        .await;
    assert!(t.status.is_success(), "{}", t.body);
    let r = as_org(&h, Method::PUT, &path(org), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.code(), "CONFLICT");
    // Nothing half-created survives the refused transaction.
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM platform_organization_scopes".into()
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM platform_applications".into()).await,
        0
    );
    let read = as_org(&h, Method::GET, &path(org), None).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}
