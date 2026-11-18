//! ADR 0064 workspace app installs (`ORGANIZATIONS_APP_INSTALLS_ENABLED`):
//!
//! * the policy section `apps` (default ADMINS, no allow list, user installs allowed) is
//!   validated strictly, compare-and-swapped like every section and refused while off;
//! * `POST /internal/v1/organizations/{id}/app-install-check` (`organization-apps.evaluate`)
//!   decides NOT_A_MEMBER / ORGANIZATION_NOT_READY / ROLE_REQUIRED / NOT_ALLOWLISTED;
//! * off: the route is 404 like any unknown route and policies are exactly as before.

mod common;

use common::{s, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

fn check_path(org: &str) -> String {
    format!("/internal/v1/organizations/{org}/app-install-check")
}

async fn check(h: &Harness, org: &str, account: Uuid, app: Uuid) -> Value {
    h.by(&h.peers.meeting)
        .post(
            &check_path(org),
            json!({ "accountId": account, "appId": app }),
        )
        .await
        .expect(200)
        .clone()
}

async fn ready(h: &Harness, org: &str) {
    h.exec(&format!(
        "UPDATE organizations SET provisioning_state = 'READY' WHERE id = '{org}'"
    ))
    .await
    .unwrap();
}

async fn set_policy(h: &Harness, org: &str, owner: Uuid, patch: Value) -> Value {
    let current = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{org}/policies"))
        .await
        .expect(200)
        .clone();
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{org}/policies"),
            json!({ "expectedRevision": current["revision"], "policy": patch }),
        )
        .await
        .expect(200)
        .clone()
}

#[tokio::test]
async fn the_install_check_follows_membership_readiness_tier_and_allow_list() {
    let Some(h) = Harness::with_app_installs(true).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "apps-a").await["id"]);
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    let guest = Uuid::new_v4();
    h.add_member(&org, owner, admin, "ADMIN").await;
    h.add_member(&org, owner, member, "MEMBER").await;
    h.add_member(&org, owner, guest, "GUEST").await;
    let app = Uuid::new_v4();

    // Not provisioned yet: members learn the organization is not ready; strangers nothing.
    let v = check(&h, &org, owner, app).await;
    assert_eq!(
        v,
        json!({ "allowed": false, "reason": "ORGANIZATION_NOT_READY" })
    );
    let stranger = Uuid::new_v4();
    let v = check(&h, &org, stranger, app).await;
    assert_eq!(v, json!({ "allowed": false, "reason": "NOT_A_MEMBER" }));
    let v = check(&h, &Uuid::new_v4().to_string(), owner, app).await;
    assert_eq!(v, json!({ "allowed": false, "reason": "NOT_A_MEMBER" }));

    ready(&h, &org).await;
    // Default policy: ADMINS (owner and admin), nobody else.
    for (who, allowed) in [
        (owner, true),
        (admin, true),
        (member, false),
        (guest, false),
    ] {
        let v = check(&h, &org, who, app).await;
        if allowed {
            assert_eq!(v, json!({ "allowed": true }));
        } else {
            assert_eq!(v, json!({ "allowed": false, "reason": "ROLE_REQUIRED" }));
        }
    }
    // MEMBERS tier with an allow list.
    let other_app = Uuid::new_v4();
    let p = set_policy(
        &h,
        &org,
        owner,
        json!({ "apps": { "installTier": "MEMBERS", "allowList": [app] } }),
    )
    .await;
    assert_eq!(p["policy"]["apps"]["installTier"], "MEMBERS");
    assert_eq!(p["policy"]["apps"]["userInstallsAllowed"], true);
    assert_eq!(
        check(&h, &org, member, app).await,
        json!({ "allowed": true })
    );
    assert_eq!(
        check(&h, &org, guest, app).await,
        json!({ "allowed": false, "reason": "ROLE_REQUIRED" })
    );
    assert_eq!(
        check(&h, &org, owner, other_app).await,
        json!({ "allowed": false, "reason": "NOT_ALLOWLISTED" })
    );
    // Clearing the allow list (null) admits any app again.
    set_policy(&h, &org, owner, json!({ "apps": { "allowList": null } })).await;
    assert_eq!(
        check(&h, &org, member, other_app).await,
        json!({ "allowed": true })
    );

    // A suspended organization is not ready; a suspended member is not a member.
    h.exec(&format!(
        "UPDATE organization_memberships SET status = 'SUSPENDED' WHERE organization_id = '{org}' AND user_id = '{member}'"
    ))
    .await
    .unwrap();
    assert_eq!(
        check(&h, &org, member, app).await,
        json!({ "allowed": false, "reason": "NOT_A_MEMBER" })
    );
    h.exec(&format!(
        "UPDATE organizations SET lifecycle_status = 'SUSPENDED' WHERE id = '{org}'"
    ))
    .await
    .unwrap();
    assert_eq!(
        check(&h, &org, owner, app).await,
        json!({ "allowed": false, "reason": "ORGANIZATION_NOT_READY" })
    );
}

#[tokio::test]
async fn the_check_needs_its_permission_and_a_strict_body() {
    let Some(h) = Harness::with_app_installs(true).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "apps-b").await["id"]);
    let body = json!({ "accountId": owner, "appId": Uuid::new_v4() });
    // Messaging holds organization.read only.
    h.by(&h.peers.messaging)
        .post(&check_path(&org), body.clone())
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.rogue_meeting)
        .post(&check_path(&org), body.clone())
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    h.by(&h.peers.meeting)
        .post(
            &check_path(&org),
            json!({ "accountId": owner, "appId": Uuid::new_v4(), "role": "OWNER" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    h.by(&h.peers.meeting)
        .post(&check_path(&org), json!({ "accountId": owner }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // A relayed acting user may only ask about themselves.
    h.user(Uuid::new_v4())
        .post(&check_path(&org), body.clone())
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(owner)
        .post(&check_path(&org), body)
        .await
        .expect(200);
}

#[tokio::test]
async fn the_apps_section_is_strict_and_owner_or_admin_managed() {
    let Some(h) = Harness::with_app_installs(true).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "apps-c").await["id"]);
    let path = format!("/internal/v1/organizations/{org}/policies");
    // Absent until set.
    let p = h.user(owner).get(&path).await.expect(200).clone();
    assert!(p["policy"].get("apps").is_none(), "{p}");
    for bad in [
        json!({ "apps": { "installTier": "EVERYONE" } }),
        json!({ "apps": { "allowList": ["nope"] } }),
        json!({ "apps": { "extra": 1 } }),
    ] {
        h.user(owner)
            .patch(&path, json!({ "expectedRevision": 0, "policy": bad }))
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    let member = Uuid::new_v4();
    h.add_member(&org, owner, member, "MEMBER").await;
    h.user(member)
        .patch(
            &path,
            json!({ "expectedRevision": 0, "policy": { "apps": { "installTier": "MEMBERS" } } }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    let p = set_policy(
        &h,
        &org,
        owner,
        json!({ "apps": { "userInstallsAllowed": false } }),
    )
    .await;
    assert_eq!(p["revision"], 1);
    assert_eq!(
        p["policy"]["apps"],
        json!({ "installTier": "ADMINS", "allowList": null, "userInstallsAllowed": false })
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{org}' AND action = 'policy.updated'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn with_app_installs_off_everything_is_unchanged() {
    let Some(h) = Harness::with_app_installs(false).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "apps-off").await["id"]);
    ready(&h, &org).await;
    h.by(&h.peers.meeting)
        .post(
            &check_path(&org),
            json!({ "accountId": owner, "appId": Uuid::new_v4() }),
        )
        .await
        .expect_error(404, "NOT_FOUND");
    let path = format!("/internal/v1/organizations/{org}/policies");
    h.user(owner)
        .patch(
            &path,
            json!({ "expectedRevision": 0, "policy": { "apps": { "installTier": "MEMBERS" } } }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let p = h.user(owner).get(&path).await.expect(200).clone();
    assert!(p["policy"].get("apps").is_none(), "{p}");
    assert_eq!(p["revision"], 0);
}
