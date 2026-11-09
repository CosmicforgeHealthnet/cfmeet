//! Blueprint Section 4 — the legacy organization export is complete, consistent, read-only
//! and carries the frozen two-sided checksum (`docs/api/organizations-internal-v1.md` §5).
mod common;

use axum::http::{Method, StatusCode};
use backend::services::organization_export::{checksum, export};
use common::*;
use serde_json::json;

#[tokio::test]
async fn export_preserves_ids_history_and_checksum_without_writing() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let admin = app.admin("exporter").await;
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": "Acme Clinic", "slug": "acme-clinic", "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let org = v["id"].as_str().unwrap().to_string();
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{org}/members/{}", member.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert!(s.is_success(), "{v}");

    let before = app
        .scalar("SELECT (SELECT count(*) FROM organizations) + (SELECT count(*) FROM organization_memberships)")
        .await;
    let doc = export(&app.db.conn).await.unwrap();
    assert_eq!(doc.counts.organizations, 1);
    assert_eq!(doc.counts.memberships, 2);
    assert_eq!(doc.organizations[0].id.to_string(), org);
    assert_eq!(doc.organizations[0].created_by, Some(admin.id));
    assert_eq!(doc.checksum, checksum(&doc.organizations, &doc.memberships));
    assert_eq!(doc.checksum.len(), 64);
    let after = app
        .scalar("SELECT (SELECT count(*) FROM organizations) + (SELECT count(*) FROM organization_memberships)")
        .await;
    assert_eq!(before, after);

    // Deterministic: a second export of unchanged data has the same checksum.
    assert_eq!(export(&app.db.conn).await.unwrap().checksum, doc.checksum);
}
