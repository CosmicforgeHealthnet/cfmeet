//! Section 6 teams: create / list / get / patch / archive, team members (TEAM_MANAGER /
//! TEAM_MEMBER), managers limited to their own teams, only ACTIVE organization members,
//! no self-escalation, bounded hierarchy without cycles, append-only history, audit and
//! outbox for every mutation, two-organization isolation and team context in the service
//! authorization reads.

mod common;

use common::{s, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

struct Org {
    id: String,
    owner: Uuid,
    admin: Uuid,
    manager: Uuid,
    manager2: Uuid,
    member: Uuid,
    guest: Uuid,
    memberships: std::collections::HashMap<Uuid, String>,
}

async fn org(h: &Harness, slug: &str) -> Org {
    let owner = Uuid::new_v4();
    let o = h.create_org(owner, slug).await;
    let id = s(&o["id"]);
    let mut memberships = std::collections::HashMap::new();
    let owner_m = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{id}"))
        .await
        .expect(200)["viewer"]["membershipId"]
        .clone();
    memberships.insert(owner, s(&owner_m));
    let person = |role: &'static str| (Uuid::new_v4(), role);
    let people = [
        person("ADMIN"),
        person("MANAGER"),
        person("MANAGER"),
        person("MEMBER"),
        person("GUEST"),
    ];
    for (user, role) in people {
        let m = h.add_member(&id, owner, user, role).await;
        memberships.insert(user, s(&m["id"]));
    }
    Org {
        id,
        owner,
        admin: people[0].0,
        manager: people[1].0,
        manager2: people[2].0,
        member: people[3].0,
        guest: people[4].0,
        memberships,
    }
}

fn teams(o: &Org) -> String {
    format!("/internal/v1/organizations/{}/teams", o.id)
}

async fn create_team(h: &Harness, o: &Org, actor: Uuid, name: &str) -> Value {
    h.user(actor)
        .post(&teams(o), json!({ "name": name }))
        .await
        .expect(201)
        .clone()
}

async fn add(
    h: &Harness,
    o: &Org,
    actor: Uuid,
    team: &str,
    user: Uuid,
    role: &str,
) -> common::Response {
    h.user(actor)
        .post(
            &format!("{}/{team}/members", teams(o)),
            json!({ "userId": user, "role": role }),
        )
        .await
}

async fn eval_team(h: &Harness, user: Uuid, org: &str, team: &str) -> Value {
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": user, "organizationId": org, "capability": "organization.teams.manage", "teamId": team }),
        )
        .await
        .expect(200)
        .clone()
}

#[tokio::test]
async fn owners_and_admins_manage_teams_and_managers_only_their_own() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let o = org(&h, "teams-org").await;
    let eng = create_team(&h, &o, o.admin, "Engineering").await;
    let ops = create_team(&h, &o, o.owner, "Operations").await;
    let (eng_id, ops_id) = (s(&eng["id"]), s(&ops["id"]));
    assert_eq!(eng["status"], "ACTIVE");
    assert_eq!(eng["version"], 1);
    // Names are unique among ACTIVE teams (case-folded).
    h.user(o.owner)
        .post(&teams(&o), json!({ "name": " engineering " }))
        .await
        .expect_error(409, "TEAM_NAME_TAKEN");
    // Members and guests never create teams; managers only when the policy allows it.
    for actor in [o.member, o.guest] {
        h.user(actor)
            .post(&teams(&o), json!({ "name": "Nope" }))
            .await
            .expect_error(403, "PERMISSION_DENIED");
    }
    h.user(o.manager)
        .post(&teams(&o), json!({ "name": "Mine" }))
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    // ADMIN appoints a TEAM_MANAGER (an organization MANAGER).
    let mgr = add(&h, &o, o.admin, &eng_id, o.manager, "TEAM_MANAGER").await;
    let mgr = mgr.expect(201).clone();
    assert_eq!(mgr["role"], "TEAM_MANAGER");
    // TEAM_MANAGER is never held by a MEMBER / GUEST.
    add(&h, &o, o.admin, &eng_id, o.member, "TEAM_MANAGER")
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // The team manager adds TEAM_MEMBERs to their own team only …
    let member = add(&h, &o, o.manager, &eng_id, o.member, "TEAM_MEMBER").await;
    let member = member.expect(201).clone();
    add(&h, &o, o.manager, &eng_id, o.guest, "TEAM_MEMBER")
        .await
        .expect(201);
    // … never appoints managers, never touches another team.
    add(&h, &o, o.manager, &eng_id, o.manager2, "TEAM_MANAGER")
        .await
        .expect_error(403, "PERMISSION_DENIED");
    add(&h, &o, o.manager, &ops_id, o.member, "TEAM_MEMBER")
        .await
        .expect_error(403, "PERMISSION_DENIED");
    add(&h, &o, o.manager2, &eng_id, o.member, "TEAM_MEMBER")
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // Idempotent re-add; a different role needs PATCH.
    add(&h, &o, o.manager, &eng_id, o.member, "TEAM_MEMBER")
        .await
        .expect(200);
    // A user is in several teams.
    add(&h, &o, o.owner, &ops_id, o.member, "TEAM_MEMBER")
        .await
        .expect(201);
    let resolved = h
        .by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/memberships/resolve",
            json!({ "userId": o.member }),
        )
        .await;
    let teams_of_member = resolved.expect(200)["memberships"][0]["teams"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(teams_of_member.len(), 2);
    // Renaming: own team yes, another team no; re-parenting is an ADMIN decision.
    h.user(o.manager)
        .patch(
            &format!("{}/{eng_id}", teams(&o)),
            json!({ "expectedVersion": 1, "name": "Platform Engineering" }),
        )
        .await
        .expect(200);
    h.user(o.manager)
        .patch(
            &format!("{}/{ops_id}", teams(&o)),
            json!({ "expectedVersion": 1, "name": "Taken over" }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(o.manager)
        .patch(
            &format!("{}/{eng_id}", teams(&o)),
            json!({ "expectedVersion": 2, "parentTeamId": ops_id }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // Stale versions conflict.
    h.user(o.admin)
        .patch(
            &format!("{}/{eng_id}", teams(&o)),
            json!({ "expectedVersion": 1, "name": "Stale" }),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
    // The team manager removes TEAM_MEMBERs, not other managers; anyone leaves themselves.
    let guest_row = s(&h
        .user(o.owner)
        .get(&format!("{}/{eng_id}/members", teams(&o)))
        .await
        .expect(200)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["userId"] == json!(o.guest))
        .unwrap()["id"]);
    h.user(o.manager)
        .post(
            &format!("{}/{eng_id}/members/{guest_row}/remove", teams(&o)),
            json!({}),
        )
        .await
        .expect(200);
    h.user(o.member)
        .post(
            &format!("{}/{eng_id}/members/{}/remove", teams(&o), s(&mgr["id"])),
            json!({}),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    let left = h
        .user(o.member)
        .post(
            &format!("{}/{eng_id}/members/{}/remove", teams(&o), s(&member["id"])),
            json!({}),
        )
        .await;
    assert_eq!(left.expect(200)["status"], "REMOVED");
    // Removal is idempotent.
    h.user(o.member)
        .post(
            &format!("{}/{eng_id}/members/{}/remove", teams(&o), s(&member["id"])),
            json!({}),
        )
        .await
        .expect(200);
    // Evaluation with team context: the manager manages Engineering only.
    let e = eval_team(&h, o.manager, &o.id, &eng_id).await;
    assert_eq!(
        (e["allowed"].as_bool(), e["teamRole"].as_str()),
        (Some(true), Some("TEAM_MANAGER"))
    );
    let e = eval_team(&h, o.manager, &o.id, &ops_id).await;
    assert_eq!(e["allowed"], false);
    assert_eq!(e["reason"], "CAPABILITY_NOT_GRANTED");
    let e = eval_team(&h, o.admin, &o.id, &ops_id).await;
    assert_eq!(e["allowed"], true);
    assert_eq!(e["teamRole"], json!(null));
}

#[tokio::test]
async fn team_roles_never_self_escalate_and_follow_the_organization_role() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let o = org(&h, "escalation-org").await;
    let t = s(&create_team(&h, &o, o.owner, "Sales").await["id"]);
    let m = add(&h, &o, o.owner, &t, o.manager, "TEAM_MEMBER").await;
    let m = m.expect(201).clone();
    // A TEAM_MEMBER (even an organization MANAGER) cannot raise their own team role.
    h.user(o.manager)
        .patch(
            &format!("{}/{t}/members/{}", teams(&o), s(&m["id"])),
            json!({ "expectedVersion": 1, "role": "TEAM_MANAGER" }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // An ADMIN appoints, the manager steps down themselves.
    let up = h
        .user(o.admin)
        .patch(
            &format!("{}/{t}/members/{}", teams(&o), s(&m["id"])),
            json!({ "expectedVersion": 1, "role": "TEAM_MANAGER" }),
        )
        .await;
    assert_eq!(up.expect(200)["version"], 2);
    let down = h
        .user(o.manager)
        .patch(
            &format!("{}/{t}/members/{}", teams(&o), s(&m["id"])),
            json!({ "expectedVersion": 2, "role": "TEAM_MEMBER" }),
        )
        .await;
    assert_eq!(down.expect(200)["role"], "TEAM_MEMBER");
    // Re-appointed, then demoted to MEMBER in the organization: the TEAM_MANAGER row stays
    // but grants nothing (the organization role no longer holds teams.manage).
    h.user(o.admin)
        .patch(
            &format!("{}/{t}/members/{}", teams(&o), s(&m["id"])),
            json!({ "expectedVersion": 3, "role": "TEAM_MANAGER" }),
        )
        .await
        .expect(200);
    let ms = o.memberships[&o.manager].clone();
    let membership = h
        .user(o.owner)
        .get(&format!("/internal/v1/organizations/{}/members", o.id))
        .await
        .expect(200)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == json!(ms))
        .unwrap()
        .clone();
    h.user(o.owner)
        .patch(
            &format!("/internal/v1/organizations/{}/members/{ms}", o.id),
            json!({ "expectedVersion": membership["version"], "role": "MEMBER" }),
        )
        .await
        .expect(200);
    add(&h, &o, o.manager, &t, o.guest, "TEAM_MEMBER")
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // Suspended / removed organization members cannot join; removal ends team memberships.
    let gm = o.memberships[&o.guest].clone();
    h.user(o.owner)
        .patch(
            &format!("/internal/v1/organizations/{}/members/{gm}", o.id),
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        )
        .await
        .expect(200);
    add(&h, &o, o.owner, &t, o.guest, "TEAM_MEMBER")
        .await
        .expect_error(400, "VALIDATION_ERROR");
    add(&h, &o, o.owner, &t, o.member, "TEAM_MEMBER")
        .await
        .expect(201);
    let mm = o.memberships[&o.member].clone();
    h.user(o.owner)
        .patch(
            &format!("/internal/v1/organizations/{}/members/{mm}", o.id),
            json!({ "expectedVersion": 1, "status": "REMOVED" }),
        )
        .await
        .expect(200);
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_team_members WHERE membership_id = '{mm}' AND status = 'ACTIVE'")).await,
        0
    );
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_team_member_history WHERE change_kind = 'MEMBERSHIP_ENDED' AND user_id = '{}'", o.member)).await,
        1
    );
    add(&h, &o, o.owner, &t, Uuid::new_v4(), "TEAM_MEMBER")
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
}

#[tokio::test]
async fn the_hierarchy_is_bounded_acyclic_and_archival_is_guarded() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let o = org(&h, "hierarchy-org").await;
    let mut chain: Vec<String> = Vec::new();
    for i in 0..5 {
        let mut body = json!({ "name": format!("Level {i}") });
        if let Some(parent) = chain.last() {
            body["parentTeamId"] = json!(parent);
        }
        let t = h.user(o.owner).post(&teams(&o), body).await;
        chain.push(s(&t.expect(201)["id"]));
    }
    // A sixth level is too deep.
    h.user(o.owner)
        .post(
            &teams(&o),
            json!({ "name": "Too deep", "parentTeamId": chain[4] }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // A cycle is refused (root under its own descendant), as is a self-parent.
    let root = chain[0].clone();
    for parent in [chain[3].clone(), root.clone()] {
        h.user(o.owner)
            .patch(
                &format!("{}/{root}", teams(&o)),
                json!({ "expectedVersion": 1, "parentTeamId": parent }),
            )
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    // A parent of another organization is unknown here.
    let other = org(&h, "hierarchy-other").await;
    let foreign = s(&create_team(&h, &other, other.owner, "Foreign").await["id"]);
    h.user(o.owner)
        .post(
            &teams(&o),
            json!({ "name": "Cross", "parentTeamId": foreign }),
        )
        .await
        .expect_error(404, "TEAM_NOT_FOUND");
    // … and its teams are invisible / unmanageable from here.
    h.user(o.owner)
        .get(&format!("{}/{foreign}", teams(&o)))
        .await
        .expect_error(404, "TEAM_NOT_FOUND");
    h.user(o.owner)
        .get(&format!(
            "/internal/v1/organizations/{}/teams/{foreign}",
            other.id
        ))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    // Archival: not while children are ACTIVE; detaching (null parent) is allowed.
    h.user(o.owner)
        .post(&format!("{}/{}/archive", teams(&o), chain[3]), json!({}))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let leaf = chain[4].clone();
    h.user(o.owner)
        .patch(
            &format!("{}/{leaf}", teams(&o)),
            json!({ "expectedVersion": 1, "parentTeamId": null }),
        )
        .await
        .expect(200);
    add(&h, &o, o.owner, &chain[3], o.member, "TEAM_MEMBER")
        .await
        .expect(201);
    // Managers never archive; the owner does, which ends memberships; idempotent.
    let r = h
        .user(o.manager)
        .post(&format!("{}/{}/archive", teams(&o), chain[3]), json!({}))
        .await;
    r.expect_error(403, "PERMISSION_DENIED");
    let archived = h
        .user(o.owner)
        .post(&format!("{}/{}/archive", teams(&o), chain[3]), json!({}))
        .await;
    assert_eq!(archived.expect(200)["status"], "ARCHIVED");
    h.user(o.owner)
        .post(&format!("{}/{}/archive", teams(&o), chain[3]), json!({}))
        .await
        .expect(200);
    add(&h, &o, o.owner, &chain[3], o.guest, "TEAM_MEMBER")
        .await
        .expect_error(409, "TEAM_ARCHIVED");
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_team_members WHERE team_id = '{}' AND status = 'ACTIVE'", chain[3])).await, 0);
    let listed = h
        .user(o.member)
        .get(&format!("{}?status=ARCHIVED", teams(&o)))
        .await;
    assert_eq!(listed.expect(200)["items"].as_array().unwrap().len(), 1);
    // Guests do not read the team directory (no organization.members.read).
    h.user(o.guest)
        .get(&teams(&o))
        .await
        .expect_error(403, "PERMISSION_DENIED");
}

#[tokio::test]
async fn every_team_mutation_is_audited_with_an_outbox_event_and_history_is_append_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let o = org(&h, "audit-teams").await;
    let t = s(&create_team(&h, &o, o.owner, "Audited").await["id"]);
    let m = add(&h, &o, o.owner, &t, o.manager, "TEAM_MEMBER").await;
    let m = s(&m.expect(201)["id"]);
    h.user(o.owner)
        .patch(
            &format!("{}/{t}/members/{m}", teams(&o)),
            json!({ "expectedVersion": 1, "role": "TEAM_MANAGER" }),
        )
        .await
        .expect(200);
    h.user(o.owner)
        .patch(
            &format!("{}/{t}", teams(&o)),
            json!({ "expectedVersion": 1, "description": "d" }),
        )
        .await
        .expect(200);
    h.user(o.owner)
        .post(&format!("{}/{t}/members/{m}/remove", teams(&o)), json!({}))
        .await
        .expect(200);
    h.user(o.owner)
        .post(
            &format!("{}/{t}/archive", teams(&o)),
            json!({ "expectedVersion": 2 }),
        )
        .await
        .expect(200);
    for (event, action) in [
        ("team.created", "team.created"),
        ("team.member_added", "team.member_added"),
        ("team.member_role_changed", "team.member_role_changed"),
        ("team.updated", "team.updated"),
        ("team.member_removed", "team.member_removed"),
        ("team.archived", "team.archived"),
    ] {
        assert_eq!(
            h.count(&format!("SELECT count(*) FROM organization_outbox WHERE event_type = '{event}' AND organization_id = '{}'", o.id)).await,
            1,
            "{event}"
        );
        assert_eq!(
            h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = '{action}' AND organization_id = '{}' AND actor_user_id = '{}'", o.id, o.owner)).await,
            1,
            "{action}"
        );
    }
    // Aggregate versions are monotonic per aggregate.
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM (SELECT aggregate_id, aggregate_version FROM organization_outbox WHERE aggregate_type IN ('team', 'team_member') AND organization_id = '{}' GROUP BY 1, 2 HAVING count(*) > 1) d", o.id)).await,
        0
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_team_member_history WHERE team_member_id = '{m}'"
        ))
        .await,
        3
    );
    for sql in [
        "UPDATE organization_team_member_history SET occurred_at = now()",
        "DELETE FROM organization_team_member_history",
        "TRUNCATE organization_team_member_history",
    ] {
        let err = h.exec(sql).await.expect_err(sql);
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    // A suspended organization grants no team management to anyone.
    h.user(o.owner)
        .post(
            &format!("/internal/v1/organizations/{}/suspend", o.id),
            json!({}),
        )
        .await
        .expect(200);
    h.user(o.owner)
        .post(&teams(&o), json!({ "name": "While suspended" }))
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
}
