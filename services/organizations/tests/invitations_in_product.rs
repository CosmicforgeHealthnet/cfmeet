//! In-product invitations (backend gap closure, Organizations G1/G9/G2):
//! the invitee lists and answers invitations addressed to them without the emailed token;
//! an invitation is only as good as its inviter; an operator can appoint an owner.

mod common;

use common::{s, Harness};
use serde_json::json;
use uuid::Uuid;

async fn invite(h: &Harness, by: Uuid, org: &str, body: serde_json::Value) -> serde_json::Value {
    h.user(by)
        .post(
            &format!("/internal/v1/organizations/{org}/invitations"),
            body,
        )
        .await
        .expect(201)
        .clone()
}

#[tokio::test]
async fn an_invitee_known_by_account_lists_and_answers_without_the_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "in-product-org").await;
    let id = s(&org["id"]);
    let invitee = Uuid::new_v4();
    let inv = invite(
        &h,
        owner,
        &id,
        json!({ "email": "kim@example.com", "role": "MEMBER", "inviteeUserId": invitee }),
    )
    .await;
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_invitations WHERE id = '{}' AND invitee_user_id = '{invitee}'",
            s(&inv["id"])
        ))
        .await,
        1,
        "the resolved account is recorded at creation"
    );

    // The invitee sees it (no verified email needed: the account is recorded).
    let mine = h
        .user(invitee)
        .get("/internal/v1/organization-invitations/mine")
        .await
        .expect(200)
        .clone();
    let items = mine["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], inv["id"]);
    assert_eq!(items[0]["organization"]["name"], "Org in-product-org");
    assert_eq!(items[0]["status"], "PENDING");
    assert!(items[0].get("token").is_none());

    // Nobody else sees or answers it; an unknown id looks the same.
    let stranger = Uuid::new_v4();
    let theirs = h
        .user(stranger)
        .email("someone@example.com")
        .get("/internal/v1/organization-invitations/mine")
        .await
        .expect(200)
        .clone();
    assert_eq!(theirs["items"], json!([]));
    let accept = format!(
        "/internal/v1/organization-invitations/{}/accept",
        s(&inv["id"])
    );
    h.user(stranger)
        .email("someone@example.com")
        .post(&accept, json!({}))
        .await
        .expect_error(404, "INVITATION_INVALID");
    h.user(stranger)
        .post(
            &format!(
                "/internal/v1/organization-invitations/{}/accept",
                Uuid::new_v4()
            ),
            json!({}),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");

    // The invitee accepts by id.
    let membership = h
        .user(invitee)
        .post(&accept, json!({}))
        .await
        .expect(200)
        .clone();
    assert_eq!(membership["role"], "MEMBER");
    assert_eq!(membership["status"], "ACTIVE");
    assert_eq!(membership["userId"], json!(invitee));
    assert_eq!(
        h.user(invitee)
            .get("/internal/v1/organization-invitations/mine")
            .await
            .expect(200)["items"],
        json!([])
    );
    // Single use.
    h.user(invitee)
        .post(&accept, json!({}))
        .await
        .expect_error(404, "INVITATION_INVALID");
}

#[tokio::test]
async fn an_email_addressed_invitation_is_listed_for_the_verified_email_and_declined_by_id() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "email-addressed-org").await;
    let id = s(&org["id"]);
    let inv = invite(
        &h,
        owner,
        &id,
        json!({ "email": "lee@example.com", "role": "GUEST" }),
    )
    .await;
    let lee = Uuid::new_v4();
    // Without the verified email the account cannot be matched.
    assert_eq!(
        h.user(lee)
            .get("/internal/v1/organization-invitations/mine")
            .await
            .expect(200)["items"],
        json!([])
    );
    let mine = h
        .user(lee)
        .email("lee@example.com")
        .get("/internal/v1/organization-invitations/mine")
        .await
        .expect(200)
        .clone();
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let decline = format!(
        "/internal/v1/organization-invitations/{}/decline",
        s(&inv["id"])
    );
    h.user(lee)
        .email("other@example.com")
        .post(&decline, json!({}))
        .await
        .expect_error(404, "INVITATION_INVALID");
    let declined = h
        .user(lee)
        .email("lee@example.com")
        .post(&decline, json!({}))
        .await
        .expect(200)
        .clone();
    assert_eq!(declined["status"], "DECLINED");
    // An expired invitation is never listed.
    let later = invite(
        &h,
        owner,
        &id,
        json!({ "email": "lee@example.com", "role": "MEMBER" }),
    )
    .await;
    h.exec(&format!(
        "UPDATE organization_invitations SET created_at = now() - interval '8 days', expires_at = now() - interval '1 day' WHERE id = '{}'",
        s(&later["id"])
    ))
    .await
    .unwrap();
    assert_eq!(
        h.user(lee)
            .email("lee@example.com")
            .get("/internal/v1/organization-invitations/mine")
            .await
            .expect(200)["items"],
        json!([])
    );
}

#[tokio::test]
async fn invitations_are_revoked_when_their_inviter_can_no_longer_issue_them() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "inviter-backed-org").await;
    let id = s(&org["id"]);
    // An ADMIN joins and invites two people.
    let admin = Uuid::new_v4();
    let admin_inv = invite(
        &h,
        owner,
        &id,
        json!({ "email": "admin@example.com", "role": "ADMIN", "inviteeUserId": admin }),
    )
    .await;
    let admin_membership = h
        .user(admin)
        .post(
            &format!(
                "/internal/v1/organization-invitations/{}/accept",
                s(&admin_inv["id"])
            ),
            json!({}),
        )
        .await
        .expect(200)
        .clone();
    let as_manager = invite(
        &h,
        admin,
        &id,
        json!({ "email": "m@example.com", "role": "MANAGER" }),
    )
    .await;
    let as_member = invite(
        &h,
        admin,
        &id,
        json!({ "email": "n@example.com", "role": "MEMBER" }),
    )
    .await;

    // Demoted to MANAGER: the MANAGER invitation is revoked, the MEMBER one stays.
    let demoted = h
        .user(owner)
        .patch(
            &format!(
                "/internal/v1/organizations/{id}/members/{}",
                s(&admin_membership["id"])
            ),
            json!({ "role": "MANAGER", "expectedVersion": admin_membership["version"] }),
        )
        .await
        .expect(200)
        .clone();
    let status = |inv: &serde_json::Value| {
        format!(
            "SELECT count(*) FROM organization_invitations WHERE id = '{}' AND status = ",
            s(&inv["id"])
        )
    };
    assert_eq!(
        h.count(&format!("{}'REVOKED'", status(&as_manager))).await,
        1
    );
    assert_eq!(
        h.count(&format!("{}'PENDING'", status(&as_member))).await,
        1
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{id}' AND action = 'invitation.revoked'"
        ))
        .await,
        1
    );

    // Removed: every pending invitation they issued is revoked.
    h.user(owner)
        .patch(
            &format!(
                "/internal/v1/organizations/{id}/members/{}",
                s(&admin_membership["id"])
            ),
            json!({ "status": "REMOVED", "expectedVersion": demoted["version"] }),
        )
        .await
        .expect(200);
    assert_eq!(
        h.count(&format!("{}'REVOKED'", status(&as_member))).await,
        1
    );
    h.user(Uuid::new_v4())
        .email("n@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": as_member["token"] }),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
}

#[tokio::test]
async fn an_invitation_whose_inviter_lost_the_right_is_revoked_on_accept() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "accept-recheck-org").await;
    let id = s(&org["id"]);
    let manager = Uuid::new_v4();
    let manager_inv = invite(
        &h,
        owner,
        &id,
        json!({ "email": "mgr@example.com", "role": "MANAGER", "inviteeUserId": manager }),
    )
    .await;
    h.user(manager)
        .post(
            &format!(
                "/internal/v1/organization-invitations/{}/accept",
                s(&manager_inv["id"])
            ),
            json!({}),
        )
        .await
        .expect(200);
    let guest = Uuid::new_v4();
    let inv = invite(
        &h,
        manager,
        &id,
        json!({ "email": "g@example.com", "role": "MEMBER", "inviteeUserId": guest }),
    )
    .await;
    // The inviter loses the right without a membership change (simulated directly):
    // acceptance re-checks the inviter and revokes.
    h.exec(&format!(
        "UPDATE organization_memberships SET status = 'SUSPENDED' WHERE organization_id = '{id}' AND user_id = '{manager}'"
    ))
    .await
    .unwrap();
    h.user(guest)
        .post(
            &format!(
                "/internal/v1/organization-invitations/{}/accept",
                s(&inv["id"])
            ),
            json!({}),
        )
        .await
        .expect_error(404, "INVITATION_INVALID");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_invitations WHERE id = '{}' AND status = 'REVOKED'",
            s(&inv["id"])
        ))
        .await,
        1,
        "the refusal is committed"
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_memberships WHERE organization_id = '{id}' AND user_id = '{guest}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn an_operator_appoints_an_owner_with_an_audited_reason() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = h.create_org(owner, "owner-recovery-org").await;
    let id = s(&org["id"]);
    let path = format!("/internal/v1/admin/organizations/{id}/owners");
    let successor = Uuid::new_v4();

    // Users cannot; a reason is required.
    h.by(&h.peers.messaging)
        .post(
            &path,
            json!({ "userId": successor, "reason": "owner account deactivated" }),
        )
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    h.by(&h.peers.operator)
        .post(&path, json!({ "userId": successor, "reason": "  " }))
        .await
        .expect_error(400, "VALIDATION_ERROR");

    let appointed = h
        .by(&h.peers.operator)
        .post(
            &path,
            json!({ "userId": successor, "reason": "owner account deactivated" }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(appointed["role"], "OWNER");
    assert_eq!(appointed["status"], "ACTIVE");
    assert_eq!(appointed["joinedVia"], "OPERATOR");
    // Idempotent.
    let again = h
        .by(&h.peers.operator)
        .post(
            &path,
            json!({ "userId": successor, "reason": "owner account deactivated" }),
        )
        .await
        .expect(200)
        .clone();
    assert_eq!(again["id"], appointed["id"]);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{id}' AND action = 'membership.owner_appointed' AND summary->>'reason' = 'owner account deactivated'"
        ))
        .await,
        1
    );
    // The previous owner can now leave: the organization keeps an owner.
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{id}/members/me/leave"),
            json!({}),
        )
        .await
        .expect(200);

    // An existing member is promoted (the platform-admin facade relays as the Meeting Service).
    let member = Uuid::new_v4();
    let inv = invite(
        &h,
        successor,
        &id,
        json!({ "email": "mem@example.com", "role": "MEMBER", "inviteeUserId": member }),
    )
    .await;
    h.user(member)
        .post(
            &format!(
                "/internal/v1/organization-invitations/{}/accept",
                s(&inv["id"])
            ),
            json!({}),
        )
        .await
        .expect(200);
    let promoted = h
        .user(Uuid::new_v4())
        .post(&path, json!({ "userId": member, "reason": "second owner" }))
        .await
        .expect(200)
        .clone();
    assert_eq!(promoted["role"], "OWNER");
    assert_eq!(promoted["userId"], json!(member));

    // Archived organizations are refused.
    h.user(successor)
        .post(
            &format!("/internal/v1/organizations/{id}/archive"),
            json!({}),
        )
        .await
        .expect(200);
    h.by(&h.peers.operator)
        .post(
            &path,
            json!({ "userId": Uuid::new_v4(), "reason": "too late" }),
        )
        .await
        .expect_error(409, "ORGANIZATION_ARCHIVED");
}
