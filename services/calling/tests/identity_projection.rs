//! Calling's identity projection (ADR 0032 §6): the Platform Access change feed is applied
//! once and in commit order across concurrent Calling instances, replaying a page never
//! regresses a newer state (revision guard), and status changes made at Platform Access
//! reach Calling's principal resolution through the feed.

mod common;

use axum::http::Method;
use calling::{
    domain::{EnvironmentKind, PrincipalId, TenantClassification},
    platform::{
        principals::{PrincipalReference, PrincipalResolutionError},
        projection::apply_change,
    },
};
use common::Harness;
use sea_orm::TransactionTrait;
use serde_json::json;
use uuid::Uuid;

/// Identity administration at Platform Access WITHOUT syncing Calling (as when another
/// operator acts while Calling instances run).
async fn admin(
    h: &Harness,
    method: Method,
    path: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let (status, body) = h.platform.pa.admin(method, path, Some(body)).await;
    assert!((200..300).contains(&status), "{path}: {status} {body}");
    body
}

#[tokio::test]
async fn concurrent_instances_apply_the_feed_once_in_commit_order() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let pa = h.db.pa();
    // A burst of identity changes nobody has applied yet.
    let mut tenants = Vec::new();
    for i in 0..12 {
        let t = admin(
            &h,
            Method::POST,
            "/internal/v1/admin/tenants",
            json!({ "slug": format!("burst-{i}"), "displayName": "Burst", "classification": "EXTERNAL" }),
        )
        .await;
        let id = t["id"].as_str().unwrap().to_string();
        admin(
            &h,
            Method::POST,
            &format!("/internal/v1/admin/tenants/{id}/applications"),
            json!({ "slug": "app", "displayName": "App" }),
        )
        .await;
        tenants.push(id);
    }
    let last = pa
        .scalar_i64("SELECT max(sequence) FROM platform_identity_changes")
        .await;

    // Three Calling instances sync at once.
    let (a, b) = (h.sibling(), h.sibling());
    let (ra, rb, rc) = tokio::join!(
        a.state.identity.sync(),
        b.state.identity.sync(),
        h.state.identity.sync()
    );
    let applied: usize = [ra.unwrap(), rb.unwrap(), rc.unwrap()]
        .iter()
        .map(|r| r.changes)
        .sum();
    assert_eq!(h.state.identity.cursor().await.unwrap(), last);
    // Every change was applied exactly once overall (each page by one instance).
    let total = pa
        .scalar_i64("SELECT count(*) FROM platform_identity_changes")
        .await as usize;
    assert_eq!(applied, total, "pages applied more than once or skipped");
    for table in ["platform_tenants", "platform_applications"] {
        assert_eq!(
            common::scalar_i64(h.conn(), &format!("SELECT count(*) FROM {table}")).await,
            pa.scalar_i64(&format!("SELECT count(*) FROM {table}"))
                .await,
            "{table}"
        );
    }
    // Same ids, same revisions.
    for id in &tenants {
        assert_eq!(
            common::scalar_i64(
                h.conn(),
                &format!("SELECT pa_revision FROM platform_tenants WHERE id = '{id}'")
            )
            .await,
            pa.scalar_i64(&format!(
                "SELECT revision FROM platform_tenants WHERE id = '{id}'"
            ))
            .await,
        );
    }
    // Nothing more to do.
    assert_eq!(h.state.identity.sync().await.unwrap().changes, 0);
}

#[tokio::test]
async fn replaying_the_feed_never_regresses_a_newer_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    h.platform
        .set_tenant_status(c.tenant.id, calling::domain::TenantStatus::Suspended)
        .await
        .unwrap();
    // The whole feed again, oldest first — including the tenant's ACTIVE revision 1.
    let page = h.state.identity.authority().changes(0, 500).await.unwrap();
    assert!(page
        .changes
        .iter()
        .any(|c| c.entity_kind == "TENANT" && c.revision == Some(1)));
    let txn = h.conn().begin().await.unwrap();
    for change in &page.changes {
        apply_change(&txn, change, chrono::Utc::now())
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
    assert_eq!(
        h.platform.tenant(c.tenant.id).await.unwrap().status,
        calling::domain::TenantStatus::Suspended,
        "an older snapshot was applied over a newer one"
    );
}

#[tokio::test]
async fn principal_changes_at_platform_access_reach_calling_through_the_feed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(
            c.sa(EnvironmentKind::Production),
            &[calling::platform::scopes::names::PRINCIPALS_READ],
        )
        .await;
    let ctx = h.ctx(issued.secret.expose_once()).await;
    // Registered at Platform Access; Calling has not synced.
    let registered = admin(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/principals",
            c.env(EnvironmentKind::Production).id
        ),
        json!({ "kind": "NATIVE_CF_USER", "externalReference": "late-user" }),
    )
    .await;
    let id = PrincipalId::from_uuid(
        Uuid::parse_str(registered["principal"]["id"].as_str().unwrap()).unwrap(),
    );
    // Resolution catches up on demand instead of reporting NOT_FOUND.
    let resolved = h
        .state
        .principals
        .resolve(&ctx, &PrincipalReference::Id(id))
        .await
        .unwrap();
    assert_eq!(resolved.id(), id);

    // Suspended at Platform Access: the next sync makes Calling refuse it.
    admin(
        &h,
        Method::POST,
        &format!("/internal/v1/admin/principals/{id}/status"),
        json!({ "status": "SUSPENDED" }),
    )
    .await;
    h.state.identity.sync().await.unwrap();
    assert!(matches!(
        h.state
            .principals
            .resolve(&ctx, &PrincipalReference::Id(id))
            .await,
        Err(PrincipalResolutionError::Inactive)
    ));
    // A principal of ANOTHER tenant is never found, even after a catch-up.
    let other = h.consumer("other", TenantClassification::External).await;
    let foreign = admin(
        &h,
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/principals",
            other.env(EnvironmentKind::Production).id
        ),
        json!({ "kind": "NATIVE_CF_USER", "externalReference": "foreign-user" }),
    )
    .await;
    let foreign = PrincipalId::from_uuid(
        Uuid::parse_str(foreign["principal"]["id"].as_str().unwrap()).unwrap(),
    );
    assert!(matches!(
        h.state
            .principals
            .resolve(&ctx, &PrincipalReference::Id(foreign))
            .await,
        Err(PrincipalResolutionError::NotFound)
    ));
}
