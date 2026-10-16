//! The identity change feed: every identity mutation is published in its own transaction,
//! in commit order, without verifier material; pages are cursor-based and replayable.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};

async fn page(h: &Harness, after: i64, limit: u64) -> Value {
    let r = h
        .as_calling(Method::POST, "/internal/v1/identity/changes", Some(json!({ "after": after, "limit": limit })), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body
}

#[tokio::test]
async fn every_identity_mutation_is_published_without_verifiers() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", &["calls.read"]).await;
    let (view, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let body = page(&h, 0, 500).await;
    let changes = body["changes"].as_array().unwrap();
    let kinds: Vec<&str> = changes.iter().map(|c| c["entityKind"].as_str().unwrap()).collect();
    for k in ["TENANT", "APPLICATION", "APPLICATION_SCOPE", "ENVIRONMENT", "SERVICE_ACCOUNT", "CREDENTIAL", "CREDENTIAL_SCOPE"] {
        assert!(kinds.contains(&k), "{k}: {kinds:?}");
    }
    // Parents before children; sequences strictly increasing.
    let pos = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
    assert!(pos("TENANT") < pos("APPLICATION"));
    assert!(pos("ENVIRONMENT") < pos("SERVICE_ACCOUNT"));
    assert!(pos("SERVICE_ACCOUNT") < pos("CREDENTIAL"));
    let seqs: Vec<i64> = changes.iter().map(|c| c["sequence"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    let credential = changes.iter().find(|c| c["entityKind"] == "CREDENTIAL").unwrap();
    assert_eq!(credential["snapshot"]["id"], view["id"]);
    let text = body.to_string();
    assert!(!text.contains("verifier"), "verifier material leaked into the feed");
    assert!(!text.contains(&secret[secret.len() - 64..]));
    // The database refuses verifier keys in the feed and any edit of it.
    assert!(h
        .conn()
        .execute_unprepared(
            "INSERT INTO platform_identity_changes (entity_kind, operation, entity_key, snapshot) \
             VALUES ('TENANT', 'INSERT', 'x', '{\"verifier\": \"00\"}')"
        )
        .await
        .is_err());
    assert!(h.conn().execute_unprepared("DELETE FROM platform_identity_changes").await.is_err());
    assert!(h.conn().execute_unprepared("UPDATE platform_identity_changes SET revision = 9").await.is_err());
}

#[tokio::test]
async fn updates_carry_increasing_revisions_and_pages_resume_from_the_cursor() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", &[]).await;
    let head = page(&h, 0, 500).await["nextAfter"].as_i64().unwrap();
    for status in ["SUSPENDED", "ACTIVE", "SUSPENDED"] {
        h.admin_ok(Method::POST, &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id), Some(json!({ "status": status })))
            .await;
    }
    // A no-op status change publishes nothing.
    h.admin_ok(Method::POST, &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id), Some(json!({ "status": "SUSPENDED" })))
        .await;
    let first = page(&h, head, 2).await;
    assert_eq!(first["changes"].as_array().unwrap().len(), 2);
    assert_eq!(first["hasMore"], true);
    let second = page(&h, first["nextAfter"].as_i64().unwrap(), 2).await;
    assert_eq!(second["changes"].as_array().unwrap().len(), 1);
    assert_eq!(second["hasMore"], false);
    let revisions: Vec<i64> = first["changes"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["changes"].as_array().unwrap())
        .map(|c| c["revision"].as_i64().unwrap())
        .collect();
    assert_eq!(revisions, vec![2, 3, 4]);
    // Replaying from the same cursor returns the same page (idempotent consumers).
    assert_eq!(page(&h, head, 2).await, first);
}

#[tokio::test]
async fn a_rolled_back_mutation_publishes_nothing() {
    let Some(h) = Harness::new().await else { return };
    h.consumer("acme", "EXTERNAL", &[]).await;
    let before: i64 = h
        .conn()
        .query_one(Statement::from_string(DbBackend::Postgres, "SELECT count(*) FROM platform_identity_changes"))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    // A duplicate slug fails inside its transaction: no row, no change, no audit.
    let r = h
        .admin(Method::POST, "/internal/v1/admin/tenants",
            Some(json!({ "slug": "acme", "displayName": "dup", "classification": "EXTERNAL" })))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let after: i64 = h
        .conn()
        .query_one(Statement::from_string(DbBackend::Postgres, "SELECT count(*) FROM platform_identity_changes"))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(before, after);
}
