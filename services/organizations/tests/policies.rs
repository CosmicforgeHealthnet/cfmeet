//! Section 6 organization policies and the role / resource matrix:
//!
//! * typed policy with compare-and-swap (`expectedRevision`), strict validation, OWNER-only
//!   delegated administration, audit + `policy.updated` for every change;
//! * a policy only removes capabilities (meeting host creation, features, directory
//!   visibility, guests, manager invitations, admin policy management) and is applied by
//!   `authorization/evaluate`, `memberships/resolve`, the organization `viewer` and the
//!   routes themselves; the legacy Meeting profile policy is carried verbatim;
//! * the owner / admin / manager / member / guest × active / suspended / removed × two
//!   organizations matrix of `authorization/evaluate`.

mod common;

use common::{s, Harness};
use organizations::domain::{role_grants, Capability, Role};
use serde_json::{json, Value};
use uuid::Uuid;

async fn evaluate(h: &Harness, user: Uuid, org: &str, capability: &str) -> Value {
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": user, "organizationId": org, "capability": capability }),
        )
        .await
        .expect(200)
        .clone()
}

fn policies(org: &str) -> String {
    format!("/internal/v1/organizations/{org}/policies")
}

#[tokio::test]
async fn the_role_resource_matrix_holds_for_every_status_and_organization() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let a = s(&h.create_org(owner, "matrix-a").await["id"]);
    let b_owner = Uuid::new_v4();
    let b = s(&h.create_org(b_owner, "matrix-b").await["id"]);
    let mut people: Vec<(Uuid, Role, String)> = vec![(owner, Role::Owner, String::new())];
    for role in [Role::Admin, Role::Manager, Role::Member, Role::Guest] {
        let u = Uuid::new_v4();
        let m = h.add_member(&a, owner, u, role.as_str()).await;
        people.push((u, role, s(&m["id"])));
    }
    // Active memberships: exactly the matrix (default policy).
    for (user, role, _) in &people {
        for cap in Capability::ALL {
            let e = evaluate(&h, *user, &a, cap.as_str()).await;
            assert_eq!(e["allowed"], role_grants(*role, *cap), "{role} {cap}");
            assert_eq!(e["role"], json!(role.as_str()));
            // Never anything in the other organization.
            let other = evaluate(&h, *user, &b, cap.as_str()).await;
            assert_eq!(other["allowed"], false);
            assert_eq!(other["reason"], "NOT_A_MEMBER");
            assert_eq!(other["organizationStatus"], json!(null));
        }
    }
    // Suspended and removed memberships grant nothing.
    for (i, status) in [(1usize, "SUSPENDED"), (3usize, "REMOVED")] {
        let (user, _, membership) = &people[i];
        h.user(owner)
            .patch(
                &format!("/internal/v1/organizations/{a}/members/{membership}"),
                json!({ "expectedVersion": 1, "status": status }),
            )
            .await
            .expect(200);
        for cap in Capability::ALL {
            let e = evaluate(&h, *user, &a, cap.as_str()).await;
            assert_eq!(e["allowed"], false, "{status} {cap}");
            assert_eq!(
                e["reason"],
                if status == "SUSPENDED" {
                    "MEMBERSHIP_SUSPENDED"
                } else {
                    "NOT_A_MEMBER"
                }
            );
        }
    }
    // A suspended organization grants nothing but organization.read to its OWNERs.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{a}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    for (user, role, _) in [&people[0], &people[2], &people[4]] {
        for cap in Capability::ALL {
            let e = evaluate(&h, *user, &a, cap.as_str()).await;
            let expected = *role == Role::Owner && *cap == Capability::OrganizationRead;
            assert_eq!(e["allowed"], expected, "{role} {cap} suspended org");
        }
    }
    // Organization B is unaffected.
    assert_eq!(
        evaluate(&h, b_owner, &b, "organization.policies.manage").await["allowed"],
        true
    );
}

#[tokio::test]
async fn policies_are_typed_compare_and_swap_and_audited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "policy-org").await["id"]);
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    h.add_member(&org, owner, admin, "ADMIN").await;
    h.add_member(&org, owner, member, "MEMBER").await;
    // The legacy Meeting Service profile policy is carried verbatim.
    h.exec(&format!(
        "UPDATE organizations SET profile_policy = '{{\"requireProfile\":true,\"fields\":[\"title\"]}}' WHERE id = '{org}'"
    ))
    .await
    .unwrap();
    let p = h.user(member).get(&policies(&org)).await;
    let p = p.expect(200).clone();
    assert_eq!(p["revision"], 0);
    assert_eq!(p["policy"]["features"]["calling"], true);
    assert_eq!(
        p["policy"]["delegatedAdministration"]["managersMayCreateTeams"],
        false
    );
    assert_eq!(
        p["legacyMeetingProfilePolicy"],
        json!({ "requireProfile": true, "fields": ["title"] })
    );
    // Members never change policies.
    h.user(member)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 0, "policy": { "features": { "calling": false } } }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // ADMIN changes features (CAS from revision 0).
    let r = h
        .user(admin)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 0, "policy": { "features": { "calling": false } } }),
        )
        .await;
    assert_eq!(r.expect(200)["revision"], 1);
    assert_eq!(
        r.json["policy"]["features"]["meetings"], true,
        "partial merge"
    );
    // A stale revision conflicts; nothing changes.
    h.user(owner)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 0, "policy": { "features": { "messaging": false } } }),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
    // Strict validation.
    for bad in [
        json!({ "unknownSection": {} }),
        json!({ "features": { "video": true } }),
        json!({ "retention": { "hintDays": 0 } }),
        json!({ "meetings": { "hostCreation": "EVERYONE" } }),
    ] {
        h.user(owner)
            .patch(
                &policies(&org),
                json!({ "expectedRevision": 1, "policy": bad }),
            )
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    // delegatedAdministration is OWNER-only (no self-elevation by ADMINs).
    h.user(admin)
        .patch(&policies(&org), json!({ "expectedRevision": 1, "policy": { "delegatedAdministration": { "managersMayCreateTeams": true } } }))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // A no-op change writes nothing.
    h.user(owner)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 1, "policy": { "features": { "calling": false } } }),
        )
        .await
        .expect(200);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE event_type = 'policy.updated' AND organization_id = '{org}'")).await, 1);
    // The OWNER removes policy management from ADMINs (and cannot lose it themselves).
    let r = h
        .user(owner)
        .patch(&policies(&org), json!({ "expectedRevision": 1, "policy": { "delegatedAdministration": { "adminsMayManagePolicies": false } } }))
        .await;
    assert_eq!(r.expect(200)["revision"], 2);
    h.user(admin)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 2, "policy": { "retention": { "hintDays": 30 } } }),
        )
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    assert_eq!(
        evaluate(&h, admin, &org, "organization.policies.manage").await["reason"],
        "POLICY_RESTRICTED"
    );
    assert_eq!(
        evaluate(&h, owner, &org, "organization.policies.manage").await["allowed"],
        true
    );
    // Audit and outbox for every change, versioned by revision.
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE action = 'policy.updated' AND organization_id = '{org}'")).await, 2);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE event_type = 'policy.updated' AND aggregate_type = 'policy' AND aggregate_version IN (1, 2) AND organization_id = '{org}'")).await, 2);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE event_type = 'policy.updated' AND payload->'payload'->'changedKeys' ? 'features.calling'").await, 1);
    // A suspended organization's policy cannot change.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/suspend"),
            json!({}),
        )
        .await
        .expect(200);
    h.user(owner)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 2, "policy": { "retention": { "hintDays": 30 } } }),
        )
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
}

#[tokio::test]
async fn policies_restrict_capabilities_everywhere_they_are_evaluated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "restrict-org").await["id"]);
    let manager = Uuid::new_v4();
    let member = Uuid::new_v4();
    h.add_member(&org, owner, manager, "MANAGER").await;
    h.add_member(&org, owner, member, "MEMBER").await;
    // A pending GUEST invitation created while guests are allowed.
    let (_, guest_token) = h.invite(&org, owner, "guest@example.com", "GUEST").await;
    let r = h
        .user(owner)
        .patch(
            &policies(&org),
            json!({ "expectedRevision": 0, "policy": {
                "memberDirectory": { "visibility": "MANAGERS", "searchable": false },
                "guests": { "invitationsAllowed": false },
                "meetings": { "hostCreation": "MANAGERS" },
                "features": { "messaging": false },
                "retention": { "hintDays": 365 },
                "delegatedAdministration": { "managersMayInvite": false },
            } }),
        )
        .await;
    r.expect(200);
    // Meeting host creation: MANAGERS and above.
    let e = evaluate(&h, member, &org, "meetings.organization.create").await;
    assert_eq!(
        (e["allowed"].as_bool(), e["reason"].as_str()),
        (Some(false), Some("POLICY_RESTRICTED"))
    );
    assert_eq!(
        evaluate(&h, manager, &org, "meetings.organization.create").await["allowed"],
        true
    );
    // A disabled feature is off for everyone, OWNER included.
    assert_eq!(
        evaluate(&h, owner, &org, "messaging.organization.moderate").await["reason"],
        "POLICY_RESTRICTED"
    );
    // The member directory is MANAGERS-only.
    h.user(member)
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    h.user(manager)
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect(200);
    // Guests: no new GUEST invitation and the pending one is not honoured.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/invitations"),
            json!({ "email": "g2@example.com", "role": "GUEST" }),
        )
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    h.user(Uuid::new_v4())
        .email("guest@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": guest_token }),
        )
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    // Managers may not invite.
    h.user(manager)
        .post(
            &format!("/internal/v1/organizations/{org}/invitations"),
            json!({ "email": "m2@example.com", "role": "MEMBER" }),
        )
        .await
        .expect_error(403, "POLICY_RESTRICTED");
    // The viewer, resolve and the roles endpoint reflect the policy.
    let viewer = h
        .user(member)
        .get(&format!("/internal/v1/organizations/{org}"))
        .await;
    let caps: Vec<String> = viewer.expect(200)["viewer"]["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .collect();
    assert!(!caps.contains(&"meetings.organization.create".to_string()));
    assert!(!caps.contains(&"organization.members.read".to_string()));
    assert!(caps.contains(&"organization.read".to_string()));
    let resolved = h
        .by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/memberships/resolve",
            json!({ "userId": manager }),
        )
        .await;
    let m = resolved.expect(200)["memberships"][0].clone();
    assert_eq!(m["policyRevision"], 1);
    assert!(!m["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("organization.members.invite")));
    let roles = h
        .user(member)
        .get(&format!(
            "/internal/v1/organizations/{org}/roles/capabilities"
        ))
        .await;
    let roles = roles.expect(200).clone();
    assert_eq!(roles["policyRevision"], 1);
    assert_eq!(roles["roles"].as_array().unwrap().len(), 5);
    let member_row = roles["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role"] == "MEMBER")
        .unwrap()
        .clone();
    assert!(
        member_row["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("meetings.organization.create")),
        "the matrix"
    );
    assert!(
        !member_row["effectiveCapabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("meetings.organization.create")),
        "under the policy"
    );
    assert_eq!(roles["teamRoles"].as_array().unwrap().len(), 2);
    // Owner services read the effective constraints (service read, no actor).
    let eff = h
        .by(&h.peers.messaging)
        .get(&format!(
            "/internal/v1/organizations/{org}/policies/effective"
        ))
        .await;
    let c = eff.expect(200)["constraints"].clone();
    assert_eq!(c["meetingHostCreation"], "MANAGERS");
    assert_eq!(c["externalGuestsAllowed"], false);
    assert_eq!(c["enabledFeatures"]["messaging"], false);
    assert_eq!(c["memberDirectory"]["searchable"], false);
    assert_eq!(c["retentionHintDays"], 365);
    assert_eq!(c["combination"], "STRICTER_WINS");
    assert_eq!(eff.json["revision"], 1);
    // Unknown organizations stay non-enumerating.
    h.by(&h.peers.messaging)
        .get(&format!(
            "/internal/v1/organizations/{}/policies/effective",
            Uuid::new_v4()
        ))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    // A corrupt stored policy fails closed (never falls back to the permissive defaults).
    h.exec(&format!("UPDATE organization_policies SET document = '{{\"bogus\": 1}}' WHERE organization_id = '{org}'"))
        .await
        .unwrap();
    h.user(member)
        .get(&format!("/internal/v1/organizations/{org}"))
        .await
        .expect_error(503, "SERVICE_UNAVAILABLE");
}
