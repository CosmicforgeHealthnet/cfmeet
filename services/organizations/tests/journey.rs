//! The organization lifecycle journey through the real router with real service tokens:
//! create → detail → profile patch (version conflict) → invite → accept (wrong email,
//! expired, single use) → member role changes (escalation, last owner) → ownership
//! transfer (concurrent: exactly one wins) → leave → suspend → reactivate → archive.

mod common;

use common::{s, Harness};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn organization_lifecycle_journey() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let carol = Uuid::new_v4();
    let erin = Uuid::new_v4();

    // --- create
    let org = h
        .user(owner)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "  Acme Health  ", "slug": "acme-health", "profile": { "industry": "Healthcare", "countryCode": "ng" } }),
        )
        .await
        .expect(201)
        .clone();
    let id = s(&org["id"]);
    assert_eq!(org["name"], "Acme Health");
    assert_eq!(org["lifecycleStatus"], "ACTIVE");
    assert_eq!(org["provisioningState"], "PENDING");
    assert_eq!(org["creationSource"], "SELF_SERVICE");
    assert_eq!(org["createdBy"], json!(owner));
    assert_eq!(org["version"], 1);
    assert_eq!(org["memberCount"], 1);
    assert_eq!(org["archivedAt"], json!(null));
    assert_eq!(org["profile"]["countryCode"], "NG");
    assert_eq!(org["profile"]["legalRegistration"], json!(null));
    assert_eq!(org["viewer"]["role"], "OWNER");
    assert_eq!(org["viewer"]["capabilities"].as_array().unwrap().len(), 14);
    // Invalid slugs and body-supplied authority are refused.
    h.user(owner)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "X", "slug": "-bad" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    h.user(owner)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "X", "slug": "xyz-org", "userId": bob }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    h.user(owner)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "Dup", "slug": "acme-health" }),
        )
        .await
        .expect_error(409, "SLUG_TAKEN");

    // --- detail
    let detail = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{id}"))
        .await;
    assert_eq!(detail.expect(200)["id"], org["id"]);

    // --- profile patch with optimistic concurrency
    let patched = h
        .user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}"),
            json!({ "expectedVersion": 1, "name": "Acme Health Group",
                    "profile": { "legalRegistration": { "registeredName": "Acme Health Ltd", "incorporatedOn": "2019-05-01" } } }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(patched["version"], 2);
    assert_eq!(
        patched["profile"]["legalRegistration"]["verification"],
        "SELF_DECLARED"
    );
    assert_eq!(patched["profile"]["industry"], "Healthcare");
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}"),
            json!({ "expectedVersion": 1, "name": "Stale" }),
        )
        .await
        .expect_error(409, "VERSION_CONFLICT");
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}"),
            json!({ "expectedVersion": 2, "profile": { "legalRegistration": { "verification": "VERIFIED" } } }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let other = h.create_org(owner, "other-org").await;
    h.user(owner)
        .patch(
            &format!("/internal/v1/organizations/{id}"),
            json!({ "expectedVersion": 2, "slug": "other-org" }),
        )
        .await
        .expect_error(409, "SLUG_TAKEN");
    assert_eq!(other["slug"], "other-org");

    // --- invite (token returned once, only its digest stored)
    let (inv, token) = h.invite(&id, owner, "Bob@Example.com", "MEMBER").await;
    assert_eq!(inv["status"], "PENDING");
    assert_eq!(inv["inviteeEmailHint"], "b***@example.com");
    assert_eq!(inv["role"], "MEMBER");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_invitations WHERE token_hash = '{token}' OR email_hint LIKE '%{token}%'")).await,
        0
    );
    // OWNER is never granted by invitation.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{id}/invitations"),
            json!({ "email": "x@example.com", "role": "OWNER" }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");

    // --- accept: wrong email and missing email refused (invitation untouched)
    h.user(carol)
        .email("carol@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(bob)
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    let bob_m = h
        .user(bob)
        .email("bob@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(bob_m["role"], "MEMBER");
    assert_eq!(bob_m["joinedVia"], "INVITATION");
    assert_eq!(bob_m["createdBy"], json!(owner));
    // Single use.
    h.user(bob)
        .email("bob@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
    h.user(bob)
        .email("bob@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": "not-a-token" }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");

    // --- expired invitation refused (and recorded EXPIRED)
    let (dave_inv, dave_token) = h.invite(&id, owner, "dave@example.com", "GUEST").await;
    h.exec(&format!(
        "UPDATE organization_invitations SET created_at = now() - interval '9 days', expires_at = now() - interval '1 second' WHERE id = '{}'",
        s(&dave_inv["id"])
    ))
    .await
    .unwrap();
    h.user(Uuid::new_v4())
        .email("dave@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": dave_token }),
        )
        .await
        .expect_error(410, "INVITE_EXPIRED");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_invitations WHERE id = '{}' AND status = 'EXPIRED'",
            s(&dave_inv["id"])
        ))
        .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'invitation.expired'")
            .await,
        1
    );

    // --- member management
    let erin_m = h.add_member(&id, owner, erin, "ADMIN").await;
    let bob_path = format!(
        "/internal/v1/organizations/{id}/members/{}",
        s(&bob_m["id"])
    );
    let erin_path = format!(
        "/internal/v1/organizations/{id}/members/{}",
        s(&erin_m["id"])
    );
    let members = h
        .user(owner)
        .get(&format!("/internal/v1/organizations/{id}/members"))
        .await;
    assert_eq!(members.expect(200)["items"].as_array().unwrap().len(), 3);
    let owner_m_id = members.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["userId"] == json!(owner))
        .unwrap()["id"]
        .clone();
    let owner_path = format!("/internal/v1/organizations/{id}/members/{}", s(&owner_m_id));
    // A MEMBER cannot manage (not even itself).
    h.user(bob)
        .patch(&bob_path, json!({ "expectedVersion": 1, "role": "ADMIN" }))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    // An ADMIN cannot assign ADMIN / OWNER, manage OWNER targets, or escalate itself.
    h.user(erin)
        .patch(&bob_path, json!({ "expectedVersion": 1, "role": "ADMIN" }))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(erin)
        .patch(
            &owner_path,
            json!({ "expectedVersion": 1, "role": "MEMBER" }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(erin)
        .patch(&erin_path, json!({ "expectedVersion": 1, "role": "OWNER" }))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    let bob_mgr = h
        .user(erin)
        .patch(
            &bob_path,
            json!({ "expectedVersion": 1, "role": "MANAGER" }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(bob_mgr["role"], "MANAGER");
    assert_eq!(bob_mgr["version"], 2);
    h.user(erin)
        .patch(&bob_path, json!({ "expectedVersion": 1, "role": "GUEST" }))
        .await
        .expect_error(409, "VERSION_CONFLICT");
    // The last OWNER can neither demote itself nor leave; nor change its own status.
    h.user(owner)
        .patch(
            &owner_path,
            json!({ "expectedVersion": 1, "role": "ADMIN" }),
        )
        .await
        .expect_error(409, "LAST_OWNER");
    h.user(owner)
        .patch(
            &owner_path,
            json!({ "expectedVersion": 1, "status": "SUSPENDED" }),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{id}/members/me/leave"),
            json!({}),
        )
        .await
        .expect_error(409, "LAST_OWNER");
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'membership.role_changed'"
        )
        .await,
        1
    );

    // --- concurrent ownership transfers: exactly one wins
    let v = h.version(&id, owner).await;
    let transfer_path = format!("/internal/v1/organizations/{id}/ownership-transfer");
    let to_bob = h.user(owner).post(
        &transfer_path,
        json!({ "toMembershipId": bob_m["id"], "expectedVersion": v }),
    );
    let to_erin = h.user(owner).post(
        &transfer_path,
        json!({ "toMembershipId": erin_m["id"], "expectedVersion": v }),
    );
    let (a, b) = tokio::join!(to_bob, to_erin);
    let wins: Vec<_> = [&a, &b]
        .into_iter()
        .filter(|r| r.status.as_u16() == 200)
        .collect();
    assert_eq!(
        wins.len(),
        1,
        "exactly one transfer wins: {:#} / {:#}",
        a.json,
        b.json
    );
    let loser = if a.status.as_u16() == 200 { &b } else { &a };
    assert!(
        matches!(loser.code(), "VERSION_CONFLICT" | "PERMISSION_DENIED"),
        "{:#}",
        loser.json
    );
    assert_eq!(wins[0].json["viewer"]["role"], "ADMIN");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM organization_memberships WHERE organization_id = '{id}' AND role = 'OWNER' AND status = 'ACTIVE'")).await,
        1
    );
    let new_owner = if a.status.as_u16() == 200 { bob } else { erin };
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = 'ownership.transferred'"
        )
        .await,
        1
    );

    // --- the former owner (now ADMIN) leaves and loses access
    let left = h
        .user(owner)
        .post(
            &format!("/internal/v1/organizations/{id}/members/me/leave"),
            json!({}),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(left["status"], "REMOVED");
    assert!(left["removedAt"].is_string());
    h.user(owner)
        .get(&format!("/internal/v1/organizations/{id}"))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");

    // --- audit read (OWNER / ADMIN)
    let audit = h
        .user(new_owner)
        .get(&format!(
            "/internal/v1/organizations/{id}/audit-events?limit=200"
        ))
        .await;
    let actions: Vec<String> = audit.expect(200)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| s(&e["action"]))
        .collect();
    for expected in [
        "organization.created",
        "organization.profile_updated",
        "invitation.created",
        "invitation.accepted",
        "invitation.expired",
        "membership.updated",
        "ownership.transferred",
        "membership.left",
    ] {
        assert!(
            actions.iter().any(|a| a == expected),
            "{expected} in {actions:?}"
        );
    }
    let page = h
        .user(new_owner)
        .get(&format!(
            "/internal/v1/organizations/{id}/audit-events?limit=2"
        ))
        .await;
    let before = page.expect(200)["nextCursor"].as_i64().unwrap();
    let next = h
        .user(new_owner)
        .get(&format!(
            "/internal/v1/organizations/{id}/audit-events?limit=2&before={before}"
        ))
        .await;
    assert!(next.expect(200)["items"][0]["seq"].as_i64().unwrap() < before);
    let guest = Uuid::new_v4();
    h.add_member(&id, new_owner, guest, "GUEST").await;
    h.user(guest)
        .get(&format!("/internal/v1/organizations/{id}/audit-events"))
        .await
        .expect_error(403, "PERMISSION_DENIED");
    h.user(guest)
        .get(&format!("/internal/v1/organizations/{id}/members"))
        .await
        .expect_error(403, "PERMISSION_DENIED");

    // --- suspend: capabilities denied, the OWNER can still read
    let member = if new_owner == bob { erin } else { bob };
    let suspended = h
        .user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/suspend"),
            json!({}),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(suspended["lifecycleStatus"], "SUSPENDED");
    assert_eq!(
        suspended["viewer"]["capabilities"],
        json!(["organization.read"])
    );
    h.user(member)
        .get(&format!("/internal/v1/organizations/{id}"))
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    h.user(new_owner)
        .patch(
            &format!("/internal/v1/organizations/{id}"),
            json!({ "expectedVersion": suspended["version"], "name": "Nope" }),
        )
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    h.user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/invitations"),
            json!({ "email": "z@example.com", "role": "MEMBER" }),
        )
        .await
        .expect_error(409, "ORGANIZATION_SUSPENDED");
    let eval = h
        .by(&h.peers.messaging)
        .post(
            "/internal/v1/organizations/authorization/evaluate",
            json!({ "userId": member, "organizationId": id, "capability": "meetings.organization.create" }),
        )
        .await;
    assert_eq!(eval.expect(200)["allowed"], false);
    assert_eq!(eval.json["reason"], "ORGANIZATION_SUSPENDED");
    let mine = h
        .user(member)
        .get(&format!("/internal/v1/users/{member}/organizations"))
        .await;
    let item = mine.expect(200)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["organization"]["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(item["organization"]["lifecycleStatus"], "SUSPENDED");
    assert_eq!(item["capabilities"], json!([]));
    // Idempotent suspend.
    h.user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/suspend"),
            json!({}),
        )
        .await
        .expect(200);

    // --- reactivate (owner-requested suspension)
    let active = h
        .user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/reactivate"),
            json!({}),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(active["lifecycleStatus"], "ACTIVE");
    h.user(member)
        .get(&format!("/internal/v1/organizations/{id}"))
        .await
        .expect(200);

    // --- archive (terminal)
    h.user(member)
        .post(
            &format!("/internal/v1/organizations/{id}/archive"),
            json!({}),
        )
        .await
        .expect_error(403, "PERMISSION_DENIED");
    let archived = h
        .user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/archive"),
            json!({ "expectedVersion": active["version"] }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(archived["lifecycleStatus"], "ARCHIVED");
    assert!(archived["archivedAt"].is_string());
    h.user(new_owner)
        .post(
            &format!("/internal/v1/organizations/{id}/reactivate"),
            json!({}),
        )
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
    let mine = h
        .user(new_owner)
        .get(&format!("/internal/v1/users/{new_owner}/organizations"))
        .await;
    assert!(mine.expect(200)["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["organization"]["id"] != json!(id)));

    // --- events: one per change, never an email or a token
    for t in [
        "organization.created",
        "organization.suspended",
        "organization.reactivated",
        "organization.archived",
    ] {
        assert!(h.count(&format!("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND event_type = '{t}' AND organization_id = '{id}'")).await >= 1, "{t}");
    }
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_outbox WHERE payload::text ILIKE '%example.com%'"
        )
        .await,
        0
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_outbox WHERE payload::text LIKE '%{token}%'"
        ))
        .await,
        0
    );
    assert_eq!(h.count("SELECT count(*) FROM organization_audit_events WHERE summary::text ILIKE '%example.com%'").await, 0);
    // Nothing was sent (no dispatcher ran); every event is recorded for reconciliation
    // and fanned out only to its subscribed consumers.
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND state <> 'PENDING'").await, 0);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination NOT IN ('cfmeet.reconciliation', 'cfmeet.meeting-service')").await, 0);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox o WHERE destination = 'cfmeet.meeting-service' AND NOT EXISTS (SELECT 1 FROM organization_outbox r WHERE r.event_id = o.event_id AND r.destination = 'cfmeet.reconciliation')").await, 0);
    assert_eq!(h.count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.meeting-service' AND event_type LIKE 'invitation.%'").await, 0);
    // Envelope shape.
    let env = h
        .count("SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND payload ? 'eventId' AND payload ? 'aggregateVersion' AND payload->>'schemaVersion' = '1' AND payload->'actor'->>'serviceId' = 'cfmeet.meeting-service'")
        .await;
    assert_eq!(
        env,
        h.count(
            "SELECT count(*) FROM organization_outbox WHERE destination = 'cfmeet.reconciliation' AND payload->'actor'->>'kind' = 'USER'"
        )
        .await
    );
}

#[tokio::test]
async fn self_service_creation_is_limited_per_actor_and_idempotent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let user = Uuid::new_v4();
    // Idempotent create: replay returns the stored response; a different body conflicts.
    let first = h
        .user(user)
        .idem("create-1")
        .post(
            "/internal/v1/organizations",
            json!({ "name": "Idem", "slug": "idem-org" }),
        )
        .await
        .expect(201)
        .clone();
    let again = h
        .user(user)
        .idem("create-1")
        .post(
            "/internal/v1/organizations",
            json!({ "slug": "idem-org", "name": "Idem" }),
        )
        .await;
    assert_eq!(again.expect(201)["id"], first["id"]);
    h.user(user)
        .idem("create-1")
        .post(
            "/internal/v1/organizations",
            json!({ "name": "Other", "slug": "idem-org-2" }),
        )
        .await
        .expect_error(409, "IDEMPOTENCY_CONFLICT");
    assert_eq!(h.count("SELECT count(*) FROM organizations").await, 1);
    // The key is scoped to the actor.
    h.user(Uuid::new_v4())
        .idem("create-1")
        .post(
            "/internal/v1/organizations",
            json!({ "name": "Other", "slug": "idem-org-3" }),
        )
        .await
        .expect(201);
    for i in 2..=5 {
        h.create_org(user, &format!("limit-{i}")).await;
    }
    h.user(user)
        .post(
            "/internal/v1/organizations",
            json!({ "name": "Six", "slug": "limit-6" }),
        )
        .await
        .expect_error(429, "RATE_LIMITED");
    // Older creations no longer count.
    h.exec("UPDATE organizations SET created_at = now() - interval '25 hours'")
        .await
        .unwrap();
    h.create_org(user, "limit-7").await;
}

#[tokio::test]
async fn invitation_replay_reissues_the_token_and_reinvite_replaces_the_pending_one() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "invites-org").await;
    let id = s(&org["id"]);
    let path = format!("/internal/v1/organizations/{id}/invitations");
    let first = h
        .user(owner)
        .idem("inv-1")
        .post(
            &path,
            json!({ "email": "pat@example.com", "role": "MEMBER" }),
        )
        .await
        .expect(201)
        .clone();
    let replay = h
        .user(owner)
        .idem("inv-1")
        .post(
            &path,
            json!({ "email": "pat@example.com", "role": "MEMBER" }),
        )
        .await
        .expect(201)
        .clone();
    assert_eq!(replay["id"], first["id"]);
    assert_ne!(
        replay["token"], first["token"],
        "a replay re-issues a fresh token"
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM organization_idempotency_records WHERE response_body ? 'token'"
        )
        .await,
        0
    );
    let pat = Uuid::new_v4();
    h.user(pat)
        .email("pat@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": first["token"] }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
    // Re-invite: the pending invitation is revoked and replaced.
    let second = h
        .user(owner)
        .post(
            &path,
            json!({ "email": "pat@example.com", "role": "GUEST" }),
        )
        .await
        .expect(201)
        .clone();
    assert_eq!(h.count(&format!("SELECT count(*) FROM organization_invitations WHERE organization_id = '{id}' AND status = 'PENDING'")).await, 1);
    h.user(pat)
        .email("pat@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": replay["token"] }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
    // Revoke is idempotent; a revoked token is invalid; decline of another invitation works.
    let revoke = format!("{path}/{}/revoke", s(&second["id"]));
    assert_eq!(
        h.user(owner).post(&revoke, json!({})).await.expect(200)["status"],
        "REVOKED"
    );
    assert_eq!(
        h.user(owner).post(&revoke, json!({})).await.expect(200)["status"],
        "REVOKED"
    );
    h.user(pat)
        .email("pat@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": second["token"] }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
    let (_, tok) = h.invite(&id, owner, "quinn@example.com", "MEMBER").await;
    let declined = h
        .user(Uuid::new_v4())
        .email("quinn@example.com")
        .post(
            "/internal/v1/organization-invitations/decline",
            json!({ "token": tok }),
        )
        .await;
    assert_eq!(declined.expect(200)["status"], "DECLINED");
    let listed = h.user(owner).get(&format!("{path}?status=DECLINED")).await;
    assert_eq!(listed.expect(200)["items"].as_array().unwrap().len(), 1);
    // Idempotent accept: a replay with the same key returns the stored membership.
    let (_, tok) = h.invite(&id, owner, "rae@example.com", "MEMBER").await;
    let rae = Uuid::new_v4();
    let a1 = h
        .user(rae)
        .email("rae@example.com")
        .idem("acc-1")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": tok }),
        )
        .await
        .expect(200)
        .clone();
    let a2 = h
        .user(rae)
        .email("rae@example.com")
        .idem("acc-1")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": tok }),
        )
        .await;
    assert_eq!(a2.expect(200)["id"], a1["id"]);
    // A MANAGER may invite MEMBER / GUEST only.
    let mgr = Uuid::new_v4();
    h.add_member(&id, owner, mgr, "MANAGER").await;
    h.user(mgr)
        .post(
            &path,
            json!({ "email": "m1@example.com", "role": "MEMBER" }),
        )
        .await
        .expect(201);
    h.user(mgr)
        .post(&path, json!({ "email": "m2@example.com", "role": "ADMIN" }))
        .await
        .expect_error(403, "PERMISSION_DENIED");
}
