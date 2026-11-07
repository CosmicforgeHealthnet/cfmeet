//! Cross-organization isolation, internal authentication (replay, forged actor, rogue
//! keys, permissions), the membership-resolution / capability-evaluation truth tables,
//! operator administration and append-only history.

mod common;

use axum::http::Method;
use common::{s, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
async fn a_member_of_one_organization_cannot_see_or_change_another() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let a = h.create_org(alice, "org-alpha").await;
    let b = h.create_org(bob, "org-bravo").await;
    let (a_id, b_id) = (s(&a["id"]), s(&b["id"]));
    let bob_members = h
        .user(bob)
        .get(&format!("/internal/v1/organizations/{b_id}/members"))
        .await;
    let bob_m = s(&bob_members.expect(200)["items"][0]["id"]);
    let (_, token_b) = h.invite(&b_id, bob, "x@example.com", "MEMBER").await;
    let inv_b = h
        .user(bob)
        .get(&format!("/internal/v1/organizations/{b_id}/invitations"))
        .await;
    let inv_b_id = s(&inv_b.expect(200)["items"][0]["id"]);
    let unknown = Uuid::new_v4();

    for org in [b_id.clone(), unknown.to_string()] {
        let base = format!("/internal/v1/organizations/{org}");
        let checks: Vec<(Method, String, Option<Value>)> = vec![
            (Method::GET, base.clone(), None),
            (
                Method::PATCH,
                base.clone(),
                Some(json!({ "expectedVersion": 1, "name": "Pwned" })),
            ),
            (Method::GET, format!("{base}/members"), None),
            (
                Method::PATCH,
                format!("{base}/members/{bob_m}"),
                Some(json!({ "expectedVersion": 1, "role": "GUEST" })),
            ),
            (
                Method::POST,
                format!("{base}/members/me/leave"),
                Some(json!({})),
            ),
            (
                Method::POST,
                format!("{base}/ownership-transfer"),
                Some(json!({ "toMembershipId": bob_m, "expectedVersion": 1 })),
            ),
            (Method::GET, format!("{base}/invitations"), None),
            (
                Method::POST,
                format!("{base}/invitations"),
                Some(json!({ "email": "y@example.com", "role": "MEMBER" })),
            ),
            (
                Method::POST,
                format!("{base}/invitations/{inv_b_id}/revoke"),
                Some(json!({})),
            ),
            (Method::POST, format!("{base}/suspend"), Some(json!({}))),
            (Method::POST, format!("{base}/reactivate"), Some(json!({}))),
            (Method::POST, format!("{base}/archive"), Some(json!({}))),
            (Method::GET, format!("{base}/audit-events"), None),
            (Method::GET, format!("{base}/provisioning"), None),
        ];
        for (method, path, body) in checks {
            let r = h.user(alice).send(method.clone(), &path, body).await;
            r.expect_error(404, "ORGANIZATION_NOT_FOUND");
        }
    }
    // Bob's membership is not reachable through Alice's organization either.
    h.user(alice)
        .patch(
            &format!("/internal/v1/organizations/{a_id}/members/{bob_m}"),
            json!({ "expectedVersion": 1, "role": "GUEST" }),
        )
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    // Another user's organization list is not readable for an actor.
    h.user(alice)
        .get(&format!("/internal/v1/users/{bob}/organizations"))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // Evaluation does not disclose the other organization's status.
    let e = h
        .by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": alice, "organizationId": b_id, "capability": "organization.read" }),
        )
        .await;
    assert_eq!(e.expect(200)["reason"], "NOT_A_MEMBER");
    assert_eq!(e.json["organizationStatus"], json!(null));
    assert_eq!(e.json["role"], json!(null));
    // Nothing changed in B.
    assert_eq!(h.count(&format!("SELECT count(*) FROM organizations WHERE id = '{b_id}' AND name = 'Org org-bravo' AND version = 1 AND lifecycle_status = 'ACTIVE'")).await, 1);
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_invitations WHERE organization_id = '{b_id}' AND status = 'PENDING'")).await, 1);
    // Bob's invitation token is still Bob's organization's.
    let joined = h
        .user(Uuid::new_v4())
        .email("x@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token_b }),
        )
        .await;
    assert_eq!(joined.expect(200)["organizationId"], json!(b_id));
}

#[tokio::test]
async fn internal_authentication_refuses_replays_forged_actors_and_unknown_keys() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let alice = Uuid::new_v4();
    let org = h.create_org(alice, "auth-org").await;
    let id = s(&org["id"]);
    let path = format!("/internal/v1/organizations/{id}");

    // A replayed token (same jti) is refused, even for a read.
    let headers = h.user(alice).signed_headers();
    h.raw(Method::GET, &path, headers.clone(), None)
        .await
        .expect(200);
    h.raw(Method::GET, &path, headers, None)
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    assert_eq!(
        h.count("SELECT count(*) FROM organization_service_nonces")
            .await,
        2,
        "create + first read; the replay recorded nothing"
    );

    // No token / an untrusted key.
    h.raw(Method::GET, &path, Default::default(), None)
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    h.by(&h.peers.rogue_meeting)
        .actor(alice)
        .get(&path)
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");

    // The acting-user header is honoured only from callers holding organization.act-for-user.
    h.by(&h.peers.messaging)
        .actor(alice)
        .get(&path)
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.by(&h.peers.operator)
        .actor(alice)
        .get("/internal/v1/admin/organizations")
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.by(&h.peers.messaging)
        .email("a@example.com")
        .get(&path)
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");

    // Route permissions: Messaging reads (service snapshot, no viewer) but never writes.
    let snapshot = h.by(&h.peers.messaging).get(&path).await;
    assert_eq!(snapshot.expect(200)["id"], json!(id));
    assert!(snapshot.json.get("viewer").is_none());
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "X", "slug": "xyz-org" }),
        )
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.messaging)
        .get("/internal/v1/admin/organizations")
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.messaging)
        .get("/internal/v1/migration/reconciliation")
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.operator)
        .get(&path)
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    // Writes need an acting user.
    h.by(&h.peers.meeting)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "X", "slug": "xyz-org" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Malformed actor header.
    let mut bad = h.by(&h.peers.meeting).signed_headers();
    bad.insert("x-cf-acting-user", "not-a-uuid".parse().unwrap());
    h.raw(Method::GET, &path, bad, None)
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Unknown routes are authenticated and enveloped.
    h.by(&h.peers.meeting)
        .get("/internal/v1/nope")
        .await
        .expect_error(404, "NOT_FOUND");
    // Oversized bodies are refused before parsing.
    let big = json!({ "name": "x".repeat(70 * 1024), "slug": "big-org" });
    h.user(alice)
        .post("/internal/v1/organizations", big)
        .await
        .expect_error(413, "PAYLOAD_TOO_LARGE");
    // Probes and the served contract.
    let health = h
        .raw(Method::GET, "/health", Default::default(), None)
        .await;
    assert_eq!(health.expect(200)["status"], "ok");
    let ready = h.raw(Method::GET, "/ready", Default::default(), None).await;
    assert_eq!(ready.expect(200)["migrations"], "current");
    let doc = h
        .raw(Method::GET, "/openapi.json", Default::default(), None)
        .await;
    assert_eq!(doc.expect(200)["openapi"], "3.1.0");
    assert_eq!(
        doc.json,
        serde_json::from_str::<Value>(organizations::http::OPENAPI).unwrap()
    );
}

/// (user, capability) → (allowed, reason) through `authorization/evaluate`.
async fn eval(
    h: &Harness,
    user: Uuid,
    org: &str,
    capability: &str,
) -> (bool, String, Value, Value) {
    let r = h
        .by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": user, "organizationId": org, "capability": capability }),
        )
        .await;
    let v = r.expect(200).clone();
    (
        v["allowed"].as_bool().unwrap(),
        s(&v["reason"]),
        v["organizationStatus"].clone(),
        v["role"].clone(),
    )
}

async fn resolve(h: &Harness, user: Uuid, filter: Option<Vec<&str>>) -> Vec<Value> {
    let mut body = json!({ "userId": user });
    if let Some(f) = filter {
        body["organizationIds"] = json!(f);
    }
    h.by(&h.peers.messaging)
        .post("/internal/v1/organizations/memberships/resolve", body)
        .await
        .expect(200)["memberships"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn resolve_and_evaluate_truth_tables() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "truth-org").await;
    let id = s(&org["id"]);
    let (admin, manager, member, guest, suspended, removed) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    h.add_member(&id, owner, admin, "ADMIN").await;
    h.add_member(&id, owner, manager, "MANAGER").await;
    h.add_member(&id, owner, member, "MEMBER").await;
    h.add_member(&id, owner, guest, "GUEST").await;
    let s_m = h.add_member(&id, owner, suspended, "MEMBER").await;
    let r_m = h.add_member(&id, owner, removed, "MEMBER").await;
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}/members/{}", s(&s_m["id"])),
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        )
        .await
        .expect(200);
    h.user(admin)
        .patch(
            &format!("/internal/v1/organizations/{id}/members/{}", s(&r_m["id"])),
            json!({ "expectedVersion": 1, "status": "REMOVED" }),
        )
        .await
        .expect(200);
    let stranger = Uuid::new_v4();

    let active_table: Vec<(Uuid, &str, bool, &str)> = vec![
        (owner, "organization.lifecycle.manage", true, "ALLOWED"),
        (owner, "calling.enterprise.manage", true, "ALLOWED"),
        (admin, "organization.members.manage", true, "ALLOWED"),
        (
            admin,
            "organization.ownership.transfer",
            false,
            "CAPABILITY_NOT_GRANTED",
        ),
        (manager, "organization.members.invite", true, "ALLOWED"),
        (
            manager,
            "organization.profile.write",
            false,
            "CAPABILITY_NOT_GRANTED",
        ),
        (member, "meetings.organization.create", true, "ALLOWED"),
        (
            member,
            "meetings.organization.manage",
            false,
            "CAPABILITY_NOT_GRANTED",
        ),
        (guest, "organization.read", true, "ALLOWED"),
        (
            guest,
            "organization.members.read",
            false,
            "CAPABILITY_NOT_GRANTED",
        ),
        (
            suspended,
            "organization.read",
            false,
            "MEMBERSHIP_SUSPENDED",
        ),
        (removed, "organization.read", false, "NOT_A_MEMBER"),
        (stranger, "organization.read", false, "NOT_A_MEMBER"),
    ];
    for (user, cap, allowed, reason) in &active_table {
        let (a, r, _, _) = eval(&h, *user, &id, cap).await;
        assert_eq!((a, r.as_str()), (*allowed, *reason), "{cap}");
    }
    let (_, _, status, role) = eval(&h, removed, &id, "organization.read").await;
    assert_eq!((status, role), (json!(null), json!(null)));
    let (_, _, status, role) = eval(&h, admin, &id, "organization.read").await;
    assert_eq!((status, role), (json!("ACTIVE"), json!("ADMIN")));
    h.by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": owner, "organizationId": id, "capability": "organization.*" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let unknown = Uuid::new_v4().to_string();
    assert_eq!(
        eval(&h, owner, &unknown, "organization.read").await.1,
        "NOT_A_MEMBER"
    );

    // Resolution returns ACTIVE memberships only.
    let m = resolve(&h, member, None).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0]["organizationId"], json!(id));
    assert_eq!(m[0]["role"], "MEMBER");
    assert_eq!(m[0]["organizationStatus"], "ACTIVE");
    assert_eq!(
        m[0]["capabilities"],
        json!([
            "organization.read",
            "organization.members.read",
            "meetings.organization.create"
        ])
    );
    assert!(resolve(&h, suspended, None).await.is_empty());
    assert!(resolve(&h, removed, None).await.is_empty());
    assert!(resolve(&h, stranger, None).await.is_empty());
    assert!(resolve(&h, member, Some(vec![&unknown])).await.is_empty());
    assert_eq!(
        resolve(&h, member, Some(vec![&id, &unknown])).await.len(),
        1
    );

    // Operator suspension: only OWNER read remains, owners cannot lift it.
    let suspended_org = h
        .by(&h.peers.operator)
        .post(
            &format!("/internal/v1/organizations/{id}/suspend"),
            json!({}),
        )
        .await;
    assert_eq!(suspended_org.expect(200)["lifecycleStatus"], "SUSPENDED");
    let suspended_table: Vec<(Uuid, &str, bool, &str)> = vec![
        (owner, "organization.read", true, "ALLOWED"),
        (
            owner,
            "organization.profile.write",
            false,
            "ORGANIZATION_SUSPENDED",
        ),
        (admin, "organization.read", false, "ORGANIZATION_SUSPENDED"),
        (
            member,
            "meetings.organization.create",
            false,
            "ORGANIZATION_SUSPENDED",
        ),
        (removed, "organization.read", false, "NOT_A_MEMBER"),
        (
            suspended,
            "organization.read",
            false,
            "MEMBERSHIP_SUSPENDED",
        ),
    ];
    for (user, cap, allowed, reason) in &suspended_table {
        let (a, r, _, _) = eval(&h, *user, &id, cap).await;
        assert_eq!((a, r.as_str()), (*allowed, *reason), "suspended {cap}");
    }
    let o = resolve(&h, owner, None).await;
    assert_eq!(o[0]["organizationStatus"], "SUSPENDED");
    assert_eq!(o[0]["capabilities"], json!(["organization.read"]));
    assert_eq!(
        resolve(&h, member, None).await[0]["capabilities"],
        json!([])
    );
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{id}/reactivate"),
            json!({}),
        )
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    let lifted = h
        .by(&h.peers.operator)
        .patch(
            &format!("/internal/v1/admin/organizations/{id}"),
            json!({ "status": "ACTIVE" }),
        )
        .await;
    assert_eq!(lifted.expect(200)["lifecycleStatus"], "ACTIVE");
    assert_eq!(
        eval(&h, member, &id, "meetings.organization.create")
            .await
            .1,
        "ALLOWED"
    );
    // The operator action is audited as OPERATOR from the operator console.
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE organization_id = '{id}' AND action = 'organization.suspended' AND actor_kind = 'OPERATOR' AND actor_service = 'cfmeet.operator-console'")).await,
        1
    );
}

#[tokio::test]
async fn operator_administration_and_the_platform_admin_facade() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let platform_admin = Uuid::new_v4();
    // Operator tooling (no actor).
    let created = h
        .by(&h.peers.operator)
        .post(
            "/internal/v1/admin/organizations",
            json!({ "name": "Operator Org", "slug": "operator-org", "ownerUserId": owner }),
        )
        .await
        .expect(201)
        .clone();
    assert_eq!(created["creationSource"], "PLATFORM_ADMIN");
    assert_eq!(created["createdBy"], json!(null));
    assert_eq!(created["memberCount"], 1);
    let owner_view = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{}", s(&created["id"])))
        .await;
    assert_eq!(owner_view.expect(200)["viewer"]["role"], "OWNER");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_memberships WHERE organization_id = '{}' AND joined_via = 'OPERATOR'", s(&created["id"]))).await,
        1
    );
    // The Meeting Service platform-admin facade relays the verified human admin.
    let relayed = h
        .user(platform_admin)
        .post(
            "/internal/v1/admin/organizations",
            json!({ "name": "Facade Org", "slug": "facade-org" }),
        )
        .await
        .expect(201)
        .clone();
    assert_eq!(relayed["createdBy"], json!(platform_admin));
    assert_eq!(relayed["memberCount"], 0);
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE target_id = '{}' AND action = 'organization.created' AND actor_kind = 'OPERATOR' AND actor_user_id = '{platform_admin}'", s(&relayed["id"]))).await,
        1
    );
    let patched = h
        .user(platform_admin)
        .patch(
            &format!("/internal/v1/admin/organizations/{}", s(&relayed["id"])),
            json!({ "status": "SUSPENDED" }),
        )
        .await;
    assert_eq!(patched.expect(200)["lifecycleStatus"], "SUSPENDED");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_audit_events WHERE target_id = '{}' AND action = 'organization.suspended' AND actor_user_id = '{platform_admin}'", s(&relayed["id"]))).await,
        1
    );
    // Listing with status filter and keyset pagination.
    let suspended = h
        .by(&h.peers.operator)
        .get("/internal/v1/admin/organizations?status=SUSPENDED")
        .await;
    assert_eq!(suspended.expect(200)["items"].as_array().unwrap().len(), 1);
    let page = h
        .by(&h.peers.operator)
        .get("/internal/v1/admin/organizations?limit=1")
        .await;
    let cursor = s(&page.expect(200)["nextCursor"]);
    let rest = h
        .by(&h.peers.operator)
        .get(&format!(
            "/internal/v1/admin/organizations?limit=1&cursor={cursor}"
        ))
        .await;
    assert_ne!(
        rest.expect(200)["items"][0]["id"],
        page.json["items"][0]["id"]
    );
    h.by(&h.peers.operator)
        .get("/internal/v1/admin/organizations?status=GONE")
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Provisioning is truthful (nothing provisioned in Section 5).
    let p = h
        .by(&h.peers.messaging)
        .get(&format!(
            "/internal/v1/organizations/{}/provisioning",
            s(&created["id"])
        ))
        .await;
    assert_eq!(p.expect(200)["provisioningState"], "PENDING");
    assert_eq!(p.json["step"], json!(null));
}

#[tokio::test]
async fn audit_and_membership_history_are_append_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    h.create_org(owner, "append-org").await;
    assert!(
        h.count("SELECT count(*) FROM organization_audit_events")
            .await
            >= 1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_membership_history")
            .await,
        1
    );
    for table in [
        "organization_audit_events",
        "organization_membership_history",
    ] {
        for sql in [
            format!("UPDATE {table} SET occurred_at = now()"),
            format!("DELETE FROM {table}"),
            format!("TRUNCATE {table}"),
        ] {
            let err = h.exec(&sql).await.expect_err(&sql);
            assert!(err.to_string().contains("append-only"), "{sql}: {err}");
        }
    }
    // Secret-looking summary keys are refused by the schema.
    let err = h
        .exec("INSERT INTO organization_audit_events (id, actor_kind, actor_service, action, target_type, summary, occurred_at) VALUES (gen_random_uuid(), 'SYSTEM', 'x', 'test.action', 't', '{\"email\":\"a@b.c\"}', now())")
        .await;
    assert!(err.is_err());
}
