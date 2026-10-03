//! ORGANIZATION_ONLY against the authoritative organization directory, and
//! DOMAIN_RESTRICTED against verified account identity (A–D closure, items 2–3).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
    ($opts:expr) => {
        match TestApp::with($opts).await {
            Some(a) => a,
            None => return,
        }
    };
}

async fn create_org(app: &TestApp, admin: &User, slug: &str, owner: &User) -> Uuid {
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": slug, "slug": slug, "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    uuid(&v["id"])
}

async fn set_member(
    app: &TestApp,
    by: &User,
    org: Uuid,
    user: Uuid,
    role: &str,
    status: &str,
) -> (StatusCode, Value) {
    app.call(
        Method::PUT,
        &format!("/api/v2/organizations/{org}/members/{user}"),
        Auth::Bearer(&by.token),
        Some(json!({ "role": role, "status": status })),
    )
    .await
}

async fn org_conference(app: &TestApp, owner: &User, org: Uuid) -> Uuid {
    app.live_conference(
        owner,
        json!({ "title": "org", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
    )
    .await
    .1
}

#[tokio::test]
async fn org01_membership_is_resolved_server_side() {
    let app = app!();
    let admin = app.admin("platform").await;
    let owner = app.user("org-owner").await;
    let insider = app.user("insider").await;
    let outsider = app.user("outsider").await;
    let other_member = app.user("other-org").await;
    let acme = create_org(&app, &admin, "acme", &owner).await;
    let globex = create_org(&app, &admin, "globex", &other_member).await;
    let (s, v) = set_member(&app, &owner, acme, insider.id, "MEMBER", "ACTIVE").await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (_, mine) = app
        .get("/api/v2/me/organizations", Auth::Bearer(&insider.token))
        .await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(uuid(&mine[0]["organization"]["id"]), acme);

    let cid = org_conference(&app, &owner, acme).await;
    let (s, v) = app.join(cid, Auth::Bearer(&insider.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "active member joins: {v}");
    let (s, v) = app
        .join(cid, Auth::Bearer(&outsider.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
    let (s, v) = app
        .join(cid, Auth::Bearer(&other_member.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "cross-organization denied"
    );
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "anon" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    // Client-supplied organization authority is rejected.
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&outsider.token),
            json!({ "organizationId": acme }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    // Non-members cannot create spaces for an organization; a foreign org id is refused.
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&outsider.token),
            json!({ "title": "x", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": acme }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, _) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&owner.token),
            json!({ "title": "x", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": globex }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn org02_inactive_membership_or_organization_is_denied_on_reconnect() {
    let app = app!();
    let admin = app.admin("platform").await;
    let owner = app.user("org-owner").await;
    let member = app.user("member").await;
    let late = app.user("late").await;
    let acme = create_org(&app, &admin, "acme2", &owner).await;
    set_member(&app, &owner, acme, member.id, "MEMBER", "ACTIVE").await;
    set_member(&app, &owner, acme, late.id, "MEMBER", "ACTIVE").await;
    let cid = org_conference(&app, &owner, acme).await;
    assert_eq!(
        app.join(cid, Auth::Bearer(&member.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );

    // Suspension takes effect immediately, including on reconnect of an admitted participant.
    let (s, _) = set_member(&app, &owner, acme, member.id, "MEMBER", "SUSPENDED").await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
    // Removed membership.
    set_member(&app, &owner, acme, member.id, "MEMBER", "REMOVED").await;
    assert_eq!(
        app.join(cid, Auth::Bearer(&member.token), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Suspended organization denies everyone except explicit owners of the space.
    assert_eq!(
        app.join(cid, Auth::Bearer(&late.token), json!({})).await.0,
        StatusCode::OK
    );
    let (s, _) = app
        .patch(
            &format!("/api/v2/admin/organizations/{acme}"),
            Auth::Bearer(&admin.token),
            json!({ "status": "SUSPENDED" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        app.join(cid, Auth::Bearer(&late.token), json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    // A deactivated account loses organization membership entirely.
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE users SET status='inactive' WHERE id='{}'",
            late.id
        ))
        .await
        .unwrap();
    assert_eq!(
        app.join(cid, Auth::Bearer(&late.token), json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn org03_membership_management_authority() {
    let app = app!();
    let admin = app.admin("platform").await;
    let owner = app.user("org-owner").await;
    let org_admin = app.user("org-admin").await;
    let member = app.user("member").await;
    let stranger = app.user("stranger").await;
    let acme = create_org(&app, &admin, "acme3", &owner).await;
    let (s, _) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&owner.token),
            json!({ "name": "x", "slug": "x" }),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "only platform admins create organizations"
    );
    assert_eq!(
        set_member(&app, &owner, acme, org_admin.id, "ADMIN", "ACTIVE")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        set_member(&app, &org_admin, acme, member.id, "MEMBER", "ACTIVE")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        set_member(&app, &org_admin, acme, member.id, "ADMIN", "ACTIVE")
            .await
            .0,
        StatusCode::FORBIDDEN,
        "admins cannot mint admins"
    );
    assert_eq!(
        set_member(&app, &org_admin, acme, owner.id, "MEMBER", "SUSPENDED")
            .await
            .0,
        StatusCode::FORBIDDEN,
        "admins cannot touch owners"
    );
    assert_eq!(
        set_member(&app, &member, acme, member.id, "OWNER", "ACTIVE")
            .await
            .0,
        StatusCode::FORBIDDEN,
        "no self-promotion"
    );
    assert_eq!(
        set_member(&app, &stranger, acme, stranger.id, "MEMBER", "ACTIVE")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (s, list) = app
        .get(
            &format!("/api/v2/organizations/{acme}/members"),
            Auth::Bearer(&org_admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(
        app.get(
            &format!("/api/v2/organizations/{acme}/members"),
            Auth::Bearer(&member.token)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // DB invariant: a space cannot reference a non-existent organization.
    let err = app
        .db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, organization_id) VALUES (gen_random_uuid(), '{}', 'zzz-zzzz-zzq', 't', '{}')",
            owner.id,
            Uuid::new_v4()
        ))
        .await;
    assert!(err.is_err());
}

async fn insert_user(app: &TestApp, email: &str, auth_type: &str, status: &str) -> User {
    let id = Uuid::new_v4();
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO users (id, username, email, auth_type, role, status) VALUES ('{id}', 'u{}', '{email}', '{auth_type}', 'user', '{status}')",
            &id.simple().to_string()[..8]
        ))
        .await
        .unwrap();
    User {
        id,
        token: app
            .state
            .jwt_service
            .generate_access_token(id, "user")
            .unwrap(),
        email: email.into(),
    }
}

async fn domain_conference(app: &TestApp) -> Uuid {
    let owner = app.user("owner").await;
    app.live_conference(&owner, json!({ "title": "d", "accessPolicy": "DOMAIN_RESTRICTED", "accessConfig": { "allowedDomains": ["acme.io"] } }))
        .await
        .1
}

#[tokio::test]
async fn dom01_domain_comes_from_verified_account_only() {
    let app = app!();
    let cid = domain_conference(&app).await;
    let verified = insert_user(&app, "Alice@ACME.io", "local", "active").await;
    let unverified = insert_user(&app, "bob@acme.io", "local", "pending_verification").await;
    let foreign = insert_user(&app, "eve@evil.io", "local", "active").await;
    let (s, v) = app
        .join(cid, Auth::Bearer(&verified.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "verified (case-insensitive) domain: {v}");
    let (s, v) = app
        .join(cid, Auth::Bearer(&unverified.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "unverified email is not proof"
    );
    assert_eq!(
        app.join(cid, Auth::Bearer(&foreign.token), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Request-body email/domain claims are rejected outright.
    for body in [
        json!({ "email": "eve@acme.io" }),
        json!({ "domain": "acme.io" }),
    ] {
        let (s, v) = app.join(cid, Auth::Bearer(&foreign.token), body).await;
        assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    }
    assert_eq!(
        app.join(cid, Auth::None, json!({ "displayName": "x@acme.io" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn dom02_without_email_verification_only_provider_verified_emails_count() {
    let app = app!(TestOptions {
        require_email_verification: false,
        ..TestOptions::default()
    });
    let cid = domain_conference(&app).await;
    let local = insert_user(&app, "carol@acme.io", "local", "active").await;
    let google = insert_user(&app, "dave@acme.io", "google", "active").await;
    let (s, v) = app.join(cid, Auth::Bearer(&local.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "unverified local email"
    );
    assert_eq!(
        app.join(cid, Auth::Bearer(&google.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
}
