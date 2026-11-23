//! ADR 0068, phase A4: the admin bridge's commands (`organization.administer`, verified
//! `hq:<staff>` attribution). For the bridge every command needs `Idempotency-Key` (a
//! UUID) and, where the subject is versioned, `expectedVersion`; R3 commands need HQ's
//! second person; the applied command is recorded in `organization_admin_operations` in
//! the change's transaction (`GET /internal/v1/operator/operations/{key}`); a repeated key
//! never applies twice; the answer is `{status, version, resource}`. The operator console
//! and the Meeting Service facade keep their routes' answers.

mod common;

use common::{s, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

const STAFF: &str = "staff-0001";
const APPROVER: &str = "staff-0002";

fn suspend(org: &str) -> String {
    format!("/internal/v1/organizations/{org}/suspend")
}
fn reactivate(org: &str) -> String {
    format!("/internal/v1/organizations/{org}/reactivate")
}
fn archive(org: &str) -> String {
    format!("/internal/v1/organizations/{org}/archive")
}
const CREATE: &str = "/internal/v1/admin/organizations";
fn owners(org: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/owners")
}
fn retry(org: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/provisioning/retry")
}
const REPLAY: &str = "/internal/v1/admin/event-delivery/replay";
fn member_change(org: &str, m: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/members/{m}/change")
}
fn team_change(org: &str, t: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/teams/{t}/change")
}
fn policies_change(org: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/policies/change")
}
fn binding_change(org: &str, service: &str) -> String {
    format!(
        "/internal/v1/admin/organizations/{org}/scopes/primary/service-bindings/{service}/change"
    )
}
fn operation(key: &str) -> String {
    format!("/internal/v1/operator/operations/{key}")
}

fn key() -> String {
    Uuid::new_v4().to_string()
}

/// No personal data in a bridge answer.
#[track_caller]
fn no_personal_data(v: &Value) {
    let text = v.to_string();
    for forbidden in [
        "\"name\"",
        "email",
        "\"slug\"",
        "credentialId",
        "serviceAccountId",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {v:#}");
    }
}

/// The bridge's answer shape: `{status, version: string|null, resource: object}`.
#[track_caller]
fn answer<'a>(v: &'a Value, status: &str, version: Option<&str>) -> &'a Value {
    assert_eq!(v.as_object().unwrap().len(), 3, "{v:#}");
    assert_eq!(v["status"], status, "{v:#}");
    match version {
        Some(n) => assert_eq!(v["version"], n, "{v:#}"),
        None => assert!(v["version"].is_null(), "{v:#}"),
    }
    assert!(v["resource"].is_object());
    no_personal_data(v);
    &v["resource"]
}

async fn record(h: &Harness, key: &str) -> Value {
    h.by(&h.peers.bridge)
        .operator(STAFF)
        .get(&operation(key))
        .await
        .expect(200)
        .clone()
}

async fn operations(h: &Harness) -> i64 {
    h.count("SELECT count(*) FROM organization_admin_operations")
        .await
}

#[tokio::test]
async fn lifecycle_commands_follow_the_owner_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-life").await["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);

    // Refusals change nothing: no key, no approval (R3), no or a stale expectedVersion.
    bridge()
        .approved(APPROVER)
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .approved(APPROVER)
        .idem("not-a-uuid")
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .idem(&key())
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await
        .expect_error(403, "APPROVAL_REQUIRED");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(&suspend(&org), json!({}))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(&suspend(&org), json!({ "expectedVersion": 7 }))
        .await
        .expect_error(409, "VERSION_CONFLICT");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &suspend(&Uuid::new_v4().to_string()),
            json!({ "expectedVersion": 1 }),
        )
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    assert_eq!(operations(&h).await, 0);
    assert_eq!(h.version(&org, owner).await, 1);

    // Applied once, at the expected version, recorded with both people.
    let k1 = key();
    let r = bridge()
        .approved(APPROVER)
        .idem(&k1)
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await;
    let resource = answer(r.expect(200), "SUSPENDED", Some("2")).clone();
    assert_eq!(resource["organizationId"], json!(org));
    assert_eq!(resource["lifecycleReason"], "OPERATOR_ACTION");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_admin_operations WHERE idempotency_key = '{k1}' \
             AND command = 'organization.suspend' AND subject_kind = 'ORGANIZATION' AND subject_id = '{org}' \
             AND operator_ref = 'hq:{STAFF}' AND approver_ref = 'hq:{APPROVER}' AND approval_ref = 'apr-0001' \
             AND resulting_state = 'SUSPENDED' AND resulting_version = 2"
        ))
        .await,
        1
    );
    assert_eq!(
        record(&h, &k1).await,
        json!({ "idempotency_key": k1, "outcome": "APPLIED", "source_status": "SUSPENDED", "source_version": "2" })
    );

    // A repeated key answers the current state and applies nothing.
    let audits = h
        .count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{org}'"
        ))
        .await;
    let again = bridge()
        .approved(APPROVER)
        .idem(&k1)
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await;
    answer(again.expect(200), "SUSPENDED", Some("2"));
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{org}'"
        ))
        .await,
        audits
    );
    // The same key for another command or subject is a conflict.
    bridge()
        .approved(APPROVER)
        .idem(&k1)
        .post(&reactivate(&org), json!({ "expectedVersion": 2 }))
        .await
        .expect_error(409, "IDEMPOTENCY_CONFLICT");
    let other = s(&h.create_org(owner, "bridge-life-2").await["id"]);
    bridge()
        .approved(APPROVER)
        .idem(&k1)
        .post(&suspend(&other), json!({ "expectedVersion": 1 }))
        .await
        .expect_error(409, "IDEMPOTENCY_CONFLICT");

    // Reactivate, then archive (terminal).
    let r = bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(&reactivate(&org), json!({ "expectedVersion": 2 }))
        .await;
    answer(r.expect(200), "ACTIVE", Some("3"));
    let k3 = key();
    let r = bridge()
        .approved(APPROVER)
        .idem(&k3)
        .post(&archive(&org), json!({ "expectedVersion": 3 }))
        .await;
    let resource = answer(r.expect(200), "ARCHIVED", Some("4")).clone();
    assert!(resource["archivedAt"].is_string());
    assert_eq!(record(&h, &k3).await["source_status"], "ARCHIVED");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(&reactivate(&org), json!({ "expectedVersion": 4 }))
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
    assert_eq!(operations(&h).await, 3);
}

#[tokio::test]
async fn the_console_and_the_facade_keep_their_answers_and_cannot_use_bridge_only_routes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-console").await["id"]);
    let membership = h.add_member(&org, owner, Uuid::new_v4(), "MEMBER").await;
    let m = s(&membership["id"]);

    // The operator console suspends as before (no key, no approval): the Organization.
    let doc = h
        .by(&h.peers.operator)
        .post(&suspend(&org), json!({}))
        .await
        .expect(200)
        .clone();
    assert_eq!(doc["id"], json!(org));
    assert_eq!(doc["lifecycleStatus"], "SUSPENDED");
    let doc = h
        .by(&h.peers.operator)
        .post(&reactivate(&org), json!({ "expectedVersion": 2 }))
        .await
        .expect(200)
        .clone();
    assert_eq!(doc["lifecycleStatus"], "ACTIVE");
    // Operator create keeps 201 + the Organization; recovery keeps the Membership.
    let created = h
        .by(&h.peers.operator)
        .post(
            CREATE,
            json!({ "name": "Console Org", "slug": "console-org" }),
        )
        .await;
    assert_eq!(created.expect(201)["slug"], "console-org");
    let appointed = h
        .by(&h.peers.operator)
        .post(
            &owners(&org),
            json!({ "userId": Uuid::new_v4(), "reason": "owner account deactivated" }),
        )
        .await;
    assert_eq!(appointed.expect(200)["role"], "OWNER");
    h.by(&h.peers.operator)
        .post(REPLAY, json!({}))
        .await
        .expect(200);
    assert_eq!(operations(&h).await, 0, "nothing recorded for the console");

    // The staff-change routes are the bridge's alone, even for administer holders.
    let bodies = [
        (
            member_change(&org, &m),
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        ),
        (
            policies_change(&org),
            json!({ "expectedVersion": 0, "policy": { "features": { "calling": false } } }),
        ),
        (
            binding_change(&org, "calling"),
            json!({ "expectedVersion": 0, "action": "CLEAR" }),
        ),
        (
            team_change(&org, &Uuid::new_v4().to_string()),
            json!({ "expectedVersion": 1, "name": "X" }),
        ),
    ];
    for (path, body) in &bodies {
        h.by(&h.peers.operator)
            .idem(&key())
            .post(path, body.clone())
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
        h.by(&h.peers.meeting)
            .post(path, body.clone())
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
        h.by(&h.peers.messaging)
            .post(path, body.clone())
            .await
            .expect_error(403, "INTERNAL_PERMISSION_DENIED");
        // Nobody else may claim an HQ operator.
        h.by(&h.peers.operator)
            .operator(STAFF)
            .post(path, body.clone())
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
    }
}

#[tokio::test]
async fn create_retry_and_replay_are_r2_and_never_apply_twice() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);
    let new_owner = Uuid::new_v4();
    let body =
        json!({ "name": "Bridge Created", "slug": "bridge-created", "ownerUserId": new_owner });

    bridge()
        .post(CREATE, body.clone())
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let k = key();
    let r = bridge().idem(&k).post(CREATE, body.clone()).await;
    let resource = answer(r.expect(200), "ACTIVE", Some("1")).clone();
    let org = s(&resource["organizationId"]);
    assert_eq!(resource["provisioningState"], "PENDING");
    // Replayed: the organization it created, not a second one.
    let again = bridge().idem(&k).post(CREATE, body.clone()).await;
    assert_eq!(
        answer(again.expect(200), "ACTIVE", Some("1"))["organizationId"],
        json!(org)
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organizations WHERE slug = 'bridge-created'")
            .await,
        1
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_memberships WHERE organization_id = '{org}' AND user_id = '{new_owner}' AND role = 'OWNER'"
        ))
        .await,
        1
    );
    assert_eq!(record(&h, &k).await["source_status"], "ACTIVE");
    // A taken slug is a refusal.
    bridge()
        .idem(&key())
        .post(CREATE, body.clone())
        .await
        .expect_error(409, "SLUG_TAKEN");

    // Provisioning retry: unversioned, strict empty body.
    bridge()
        .idem(&key())
        .post(&retry(&org), json!({ "expectedVersion": 1 }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let kr = key();
    let r = bridge().idem(&kr).post(&retry(&org), json!({})).await;
    let resource = answer(r.expect(200), "PENDING", None).clone();
    assert_eq!(resource["organizationId"], json!(org));
    assert_eq!(
        record(&h, &kr).await,
        json!({ "idempotency_key": kr, "outcome": "APPLIED", "source_status": "PENDING", "source_version": null })
    );

    // Event replay: unversioned; a repeated key does not replay again.
    let ke = key();
    let r = bridge().idem(&ke).post(REPLAY, json!({})).await;
    let resource = answer(r.expect(200), "REPLAYED", None).clone();
    assert_eq!(resource["replayedCount"], 0);
    let again = bridge().idem(&ke).post(REPLAY, json!({})).await;
    assert!(answer(again.expect(200), "REPLAYED", None)["replayedCount"].is_null());
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_admin_operations WHERE idempotency_key = '{ke}' AND subject_kind = 'EVENT_DELIVERY' AND approver_ref IS NULL"
        ))
        .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_audit_events WHERE action = 'event_delivery.replayed'")
            .await,
        1
    );
}

#[tokio::test]
async fn ownership_recovery_is_at_the_users_membership_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-owner").await["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF).approved(APPROVER);
    let stranger = Uuid::new_v4();
    let body = |user: Uuid, v: i64| json!({ "userId": user, "reason": "owner left", "expectedVersion": v });

    h.by(&h.peers.bridge)
        .operator(STAFF)
        .idem(&key())
        .post(&owners(&org), body(stranger, 0))
        .await
        .expect_error(403, "APPROVAL_REQUIRED");
    bridge()
        .idem(&key())
        .post(
            &owners(&org),
            json!({ "userId": stranger, "reason": "owner left" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .idem(&key())
        .post(&owners(&org), body(stranger, 1))
        .await
        .expect_error(409, "VERSION_CONFLICT");

    // No membership yet: version 0; the new OWNER membership starts at 1.
    let k = key();
    let r = bridge()
        .idem(&k)
        .post(&owners(&org), body(stranger, 0))
        .await;
    let resource = answer(r.expect(200), "ACTIVE", Some("1")).clone();
    assert_eq!(resource["role"], "OWNER");
    assert_eq!(resource["userId"], json!(stranger));
    assert_eq!(record(&h, &k).await["source_version"], "1");

    // An existing member is promoted at its own version.
    let member = Uuid::new_v4();
    let m = h.add_member(&org, owner, member, "MEMBER").await;
    let v = m["version"].as_i64().unwrap();
    let r = bridge()
        .idem(&key())
        .post(&owners(&org), body(member, v))
        .await;
    let resource = answer(r.expect(200), "ACTIVE", Some(&(v + 1).to_string())).clone();
    assert_eq!(resource["role"], "OWNER");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{org}' AND action = 'membership.owner_appointed'"
        ))
        .await,
        2
    );
}

#[tokio::test]
async fn member_changes_keep_the_last_owner_and_end_removed_memberships() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-members").await["id"]);
    let owner_membership = h
        .by(&h.peers.meeting)
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect(200)["items"][0]
        .clone();
    let member = h.add_member(&org, owner, Uuid::new_v4(), "MEMBER").await;
    let m = s(&member["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);

    bridge()
        .idem(&key())
        .post(
            &member_change(&org, &m),
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        )
        .await
        .expect_error(403, "APPROVAL_REQUIRED");
    for bad in [
        json!({ "status": "SUSPENDED" }),
        json!({ "expectedVersion": 1 }),
        json!({ "expectedVersion": 1, "role": "KING" }),
        json!({ "expectedVersion": 1, "status": "SUSPENDED", "email": "x@example.com" }),
    ] {
        bridge()
            .approved(APPROVER)
            .idem(&key())
            .post(&member_change(&org, &m), bad)
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &member_change(&org, &m),
            json!({ "expectedVersion": 9, "status": "SUSPENDED" }),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &member_change(&org, &Uuid::new_v4().to_string()),
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        )
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");

    let k = key();
    let r = bridge()
        .approved(APPROVER)
        .idem(&k)
        .post(
            &member_change(&org, &m),
            json!({ "expectedVersion": 1, "status": "SUSPENDED", "role": "ADMIN" }),
        )
        .await;
    let resource = answer(r.expect(200), "SUSPENDED", Some("3")).clone();
    assert_eq!(resource["role"], "ADMIN");
    assert_eq!(resource["membershipId"], json!(m));
    assert_eq!(
        record(&h, &k).await,
        json!({ "idempotency_key": k, "outcome": "APPLIED", "source_status": "SUSPENDED", "source_version": "3" })
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_membership_history WHERE membership_id = '{m}' AND actor_kind = 'OPERATOR' AND actor_service = 'cfmeet.admin-bridge'"
        ))
        .await,
        1
    );

    // The last active OWNER stays.
    let om = s(&owner_membership["id"]);
    let ov = owner_membership["version"].as_i64().unwrap();
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &member_change(&org, &om),
            json!({ "expectedVersion": ov, "role": "ADMIN" }),
        )
        .await
        .expect_error(409, "LAST_OWNER");

    // Removal ends the membership; it is then not found.
    let r = bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &member_change(&org, &m),
            json!({ "expectedVersion": 3, "status": "REMOVED" }),
        )
        .await;
    let resource = answer(r.expect(200), "REMOVED", Some("4")).clone();
    assert!(resource["removedAt"].is_string());
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &member_change(&org, &m),
            json!({ "expectedVersion": 4, "status": "ACTIVE" }),
        )
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
}

#[tokio::test]
async fn team_changes_rename_and_archive_at_the_team_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-teams").await["id"]);
    let t = h
        .user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/teams"),
            json!({ "name": "Support" }),
        )
        .await
        .expect(201)
        .clone();
    let team = s(&t["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF).approved(APPROVER);

    for bad in [
        json!({ "expectedVersion": 1 }),
        json!({ "expectedVersion": 1, "status": "ACTIVE" }),
        json!({ "expectedVersion": 1, "status": "ARCHIVED", "name": "X" }),
        json!({ "name": "X" }),
        json!({ "expectedVersion": 1, "name": "" }),
    ] {
        bridge()
            .idem(&key())
            .post(&team_change(&org, &team), bad)
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    bridge()
        .idem(&key())
        .post(
            &team_change(&org, &team),
            json!({ "expectedVersion": 2, "name": "Care" }),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
    bridge()
        .idem(&key())
        .post(
            &team_change(&org, &Uuid::new_v4().to_string()),
            json!({ "expectedVersion": 1, "name": "Care" }),
        )
        .await
        .expect_error(404, "TEAM_NOT_FOUND");

    let r = bridge()
        .idem(&key())
        .post(
            &team_change(&org, &team),
            json!({ "expectedVersion": 1, "name": "Care", "description": "Front line" }),
        )
        .await;
    let resource = answer(r.expect(200), "ACTIVE", Some("2")).clone();
    assert_eq!(resource["teamId"], json!(team));
    let detail = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{org}/teams/{team}"))
        .await
        .expect(200)
        .clone();
    assert_eq!(detail["name"], "Care");
    assert_eq!(detail["description"], "Front line");

    let k = key();
    let r = bridge()
        .idem(&k)
        .post(
            &team_change(&org, &team),
            json!({ "expectedVersion": 2, "status": "ARCHIVED" }),
        )
        .await;
    let resource = answer(r.expect(200), "ARCHIVED", Some("3")).clone();
    assert!(resource["archivedAt"].is_string());
    assert_eq!(record(&h, &k).await["source_status"], "ARCHIVED");
    // Archiving again is idempotent; renaming an archived team is refused.
    let r = bridge()
        .idem(&key())
        .post(
            &team_change(&org, &team),
            json!({ "expectedVersion": 3, "status": "ARCHIVED" }),
        )
        .await;
    answer(r.expect(200), "ARCHIVED", Some("3"));
    bridge()
        .idem(&key())
        .post(
            &team_change(&org, &team),
            json!({ "expectedVersion": 3, "name": "Again" }),
        )
        .await
        .expect_error(409, "TEAM_ARCHIVED");
}

#[tokio::test]
async fn policy_changes_are_compare_and_swap_on_the_revision() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-policy").await["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF).approved(APPROVER);
    let change = |v: i64, policy: Value| json!({ "expectedVersion": v, "policy": policy });

    for bad in [
        json!({ "policy": { "features": { "calling": false } } }),
        change(0, json!([])),
        change(0, json!({ "features": { "calling": "no" } })),
        change(0, json!({ "nope": {} })),
    ] {
        bridge()
            .idem(&key())
            .post(&policies_change(&org), bad)
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    bridge()
        .idem(&key())
        .post(
            &policies_change(&org),
            change(1, json!({ "features": { "calling": false } })),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");

    // Defaults are revision 0; staff may change owner-only sections too.
    let k = key();
    let r = bridge()
        .idem(&k)
        .post(
            &policies_change(&org),
            change(
                0,
                json!({ "features": { "calling": false }, "delegatedAdministration": { "managersMayCreateTeams": true } }),
            ),
        )
        .await;
    let resource = answer(r.expect(200), "UPDATED", Some("1")).clone();
    assert!(resource["changedKeys"]
        .as_array()
        .unwrap()
        .contains(&json!("features.calling")));
    assert_eq!(record(&h, &k).await["source_version"], "1");
    let p = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{org}/policies"))
        .await
        .expect(200)
        .clone();
    assert_eq!(p["revision"], 1);
    assert!(p["updatedBy"].is_null(), "no CF Meet user changed it");
    assert_eq!(p["policy"]["features"]["calling"], false);

    // The same document again writes nothing.
    let r = bridge()
        .idem(&key())
        .post(
            &policies_change(&org),
            change(1, json!({ "features": { "calling": false } })),
        )
        .await;
    answer(r.expect(200), "UNCHANGED", Some("1"));
    bridge()
        .idem(&key())
        .post(
            &policies_change(&org),
            change(0, json!({ "features": { "messaging": false } })),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
}

#[tokio::test]
async fn binding_changes_set_and_clear_at_the_binding_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-binding").await["id"]);
    let bridge = || h.by(&h.peers.bridge).operator(STAFF).approved(APPROVER);
    let (sa, cred) = (Uuid::new_v4(), Uuid::new_v4());
    let set = |v: i64| json!({ "expectedVersion": v, "action": "SET", "serviceAccountId": sa, "credentialId": cred });

    // No recorded triple yet.
    bridge()
        .idem(&key())
        .post(&binding_change(&org, "calling"), set(0))
        .await
        .expect_error(409, "PROVISIONING_PENDING");
    h.exec(&format!(
        "INSERT INTO organization_platform_scopes (id, organization_id, capability, tenant_id, application_id, environment_id, mapping_state, created_at, updated_at) \
         VALUES (gen_random_uuid(), '{org}', 'primary', '{}', '{}', '{}', 'VERIFYING', now(), now())",
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4()
    ))
    .await
    .unwrap();
    for bad in [
        json!({ "expectedVersion": 0, "action": "SET", "serviceAccountId": sa }),
        json!({ "expectedVersion": 0, "action": "CLEAR", "credentialId": cred }),
        json!({ "expectedVersion": 0, "action": "ROTATE" }),
        json!({ "action": "CLEAR" }),
    ] {
        bridge()
            .idem(&key())
            .post(&binding_change(&org, "calling"), bad)
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    bridge()
        .idem(&key())
        .post(&binding_change(&org, "telex"), set(0))
        .await
        .expect_error(404, "NOT_FOUND");
    bridge()
        .idem(&key())
        .post(&binding_change(&org, "calling"), set(1))
        .await
        .expect_error(409, "VERSION_CONFLICT");

    let k = key();
    let r = bridge()
        .idem(&k)
        .post(&binding_change(&org, "calling"), set(0))
        .await;
    let resource = answer(r.expect(200), "BOUND", Some("1")).clone();
    assert_eq!(resource["source"], "OPERATOR");
    assert_eq!(resource["service"], "calling");
    assert_eq!(
        record(&h, &k).await,
        json!({ "idempotency_key": k, "outcome": "APPLIED", "source_status": "BOUND", "source_version": "1" })
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_admin_operations WHERE idempotency_key = '{k}' AND subject_kind = 'CALLING_BINDING'"
        ))
        .await,
        1
    );

    // Clear at version 1; the next SET expects 0 again.
    let kc = key();
    let r = bridge()
        .idem(&kc)
        .post(
            &binding_change(&org, "calling"),
            json!({ "expectedVersion": 1, "action": "CLEAR" }),
        )
        .await;
    answer(r.expect(200), "REMOVED", None);
    assert_eq!(
        record(&h, &kc).await,
        json!({ "idempotency_key": kc, "outcome": "APPLIED", "source_status": "REMOVED", "source_version": null })
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_scope_service_bindings WHERE organization_id = '{org}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn the_operation_record_is_the_bridges_alone() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "bridge-record").await["id"]);
    let k = key();
    h.by(&h.peers.bridge)
        .operator(STAFF)
        .approved(APPROVER)
        .idem(&k)
        .post(&suspend(&org), json!({ "expectedVersion": 1 }))
        .await
        .expect(200);
    assert_eq!(record(&h, &k).await["outcome"], "APPLIED");
    // Unknown key, or anyone but the bridge: 404.
    h.by(&h.peers.bridge)
        .operator(STAFF)
        .get(&operation(&key()))
        .await
        .expect_error(404, "NOT_FOUND");
    h.by(&h.peers.operator)
        .get(&operation(&k))
        .await
        .expect_error(404, "NOT_FOUND");
    h.by(&h.peers.meeting)
        .get(&operation(&k))
        .await
        .expect_error(404, "NOT_FOUND");
    h.by(&h.peers.messaging)
        .get(&operation(&k))
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    // The record is append-only.
    assert!(h
        .exec(&format!(
            "UPDATE organization_admin_operations SET resulting_state = 'ACTIVE' WHERE idempotency_key = '{k}'"
        ))
        .await
        .is_err());
    assert!(h
        .exec(&format!(
            "DELETE FROM organization_admin_operations WHERE idempotency_key = '{k}'"
        ))
        .await
        .is_err());
}
