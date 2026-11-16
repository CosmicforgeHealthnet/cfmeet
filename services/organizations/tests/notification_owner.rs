//! X5 `resolveInvitationInvitee` (contract `organizations/notification-owner/v1`):
//! `POST /internal/v1/organization-invitations/{invitationId}/invitee/resolve` for the
//! Notification Service only (`organization-invitations.read`). Read-only; never the
//! invitee email or token.

mod common;

use axum::http::{HeaderMap, Method};
use common::{s, Harness};
use serde_json::{json, Value};
use uuid::Uuid;

fn path(invitation: &str) -> String {
    format!("/internal/v1/organization-invitations/{invitation}/invitee/resolve")
}

/// The response carries exactly `{status, inviteeAccountRef}` and nothing that could be an
/// email address or the token.
#[track_caller]
fn assert_no_email(doc: &Value, email: &str, token: &str) {
    let keys: Vec<&str> = doc
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.len(), 2, "{doc}");
    assert!(keys.contains(&"status") && keys.contains(&"inviteeAccountRef"));
    let text = doc.to_string();
    assert!(!text.contains('@'), "{text}");
    assert!(!text.contains(email), "{text}");
    assert!(!text.contains(token), "{text}");
    let local = email.split('@').next().unwrap();
    assert!(!text.contains(local), "{text}");
}

#[tokio::test]
async fn only_the_notification_service_may_resolve_an_invitee() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "x5-auth").await["id"]);
    let (inv, _) = h.invite(&org, owner, "auth@example.com", "MEMBER").await;
    let p = path(&s(&inv["id"]));

    // No service token → 401.
    h.raw(Method::POST, &p, HeaderMap::new(), None)
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // An untrusted key claiming a trusted identity → 401.
    h.by(&h.peers.rogue_meeting)
        .post(&p, json!({}))
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // Trusted peers without `organization-invitations.read` → 403, even the account relay.
    for peer in [&h.peers.messaging, &h.peers.meeting, &h.peers.operator] {
        h.by(peer)
            .post(&p, json!({}))
            .await
            .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    }
    // The Notification Service holding the permission → 200.
    let r = h.by(&h.peers.notifications).post(&p, json!({})).await;
    assert_eq!(r.expect(200)["status"], "PENDING");
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    // A replayed token is refused.
    let call = h.by(&h.peers.notifications);
    let headers = call.signed_headers();
    h.raw(Method::POST, &p, headers.clone(), None)
        .await
        .expect(200);
    h.raw(Method::POST, &p, headers, None)
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // The Notification Service never asserts an acting user here.
    h.by(&h.peers.notifications)
        .actor(owner)
        .post(&p, json!({}))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
}

#[tokio::test]
async fn each_status_resolves_without_the_email() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "x5-status").await["id"]);
    let resolve = |id: String| {
        let h = &h;
        async move {
            h.by(&h.peers.notifications)
                .post(&path(&id), json!({}))
                .await
                .expect(200)
                .clone()
        }
    };

    // PENDING: no account is known for an email-addressed invitation.
    let (pending, token) = h.invite(&org, owner, "pending@example.com", "MEMBER").await;
    let doc = resolve(s(&pending["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "PENDING", "inviteeAccountRef": null })
    );
    assert_no_email(&doc, "pending@example.com", &token);
    // An empty body is accepted too (the contract defines none).
    let empty = h
        .by(&h.peers.notifications)
        .send(Method::POST, &path(&s(&pending["id"])), None)
        .await;
    assert_eq!(empty.expect(200)["status"], "PENDING");

    // ACCEPTED: the accepting account.
    let accepter = Uuid::new_v4();
    let (accepted, token) = h.invite(&org, owner, "accept@example.com", "MEMBER").await;
    h.user(accepter)
        .email("accept@example.com")
        .post(
            "/internal/v1/organization-invitations/accept",
            json!({ "token": token }),
        )
        .await
        .expect(200);
    let doc = resolve(s(&accepted["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "ACCEPTED", "inviteeAccountRef": format!("cfmeet-user:{accepter}") })
    );
    assert_no_email(&doc, "accept@example.com", &token);

    // DECLINED: the declining account.
    let decliner = Uuid::new_v4();
    let (declined, token) = h.invite(&org, owner, "decline@example.com", "MEMBER").await;
    h.user(decliner)
        .email("decline@example.com")
        .post(
            "/internal/v1/organization-invitations/decline",
            json!({ "token": token }),
        )
        .await
        .expect(200);
    let doc = resolve(s(&declined["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "DECLINED", "inviteeAccountRef": format!("cfmeet-user:{decliner}") })
    );
    assert_no_email(&doc, "decline@example.com", &token);

    // REVOKED: never answered by an account.
    let (revoked, token) = h.invite(&org, owner, "revoke@example.com", "MEMBER").await;
    h.user(owner)
        .post(
            &format!(
                "/internal/v1/organizations/{org}/invitations/{}/revoke",
                s(&revoked["id"])
            ),
            json!({}),
        )
        .await
        .expect(200);
    let doc = resolve(s(&revoked["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "REVOKED", "inviteeAccountRef": null })
    );
    assert_no_email(&doc, "revoke@example.com", &token);

    // EXPIRED by time while still recorded PENDING (before the sweeper runs).
    let (lapsed, token) = h.invite(&org, owner, "lapsed@example.com", "MEMBER").await;
    h.exec(&format!(
        "UPDATE organization_invitations SET created_at = now() - interval '8 days', expires_at = now() - interval '1 day' WHERE id = '{}'",
        s(&lapsed["id"])
    ))
    .await
    .unwrap();
    let doc = resolve(s(&lapsed["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "EXPIRED", "inviteeAccountRef": null })
    );
    assert_no_email(&doc, "lapsed@example.com", &token);
    // … and once the sweeper has recorded it.
    organizations::worker::expire_invitations(&h.db.conn, 10)
        .await
        .unwrap();
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_invitations WHERE id = '{}' AND status = 'EXPIRED'",
            s(&lapsed["id"])
        ))
        .await,
        1
    );
    let doc = resolve(s(&lapsed["id"])).await;
    assert_eq!(
        doc,
        json!({ "status": "EXPIRED", "inviteeAccountRef": null })
    );

    // Resolution is read-only: nothing was emitted by the resolve calls.
    let before = h.count("SELECT count(*) FROM organization_outbox").await;
    resolve(s(&pending["id"])).await;
    assert_eq!(
        h.count("SELECT count(*) FROM organization_outbox").await,
        before
    );
}

#[tokio::test]
async fn unknown_and_malformed_invitations_are_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let by = || h.by(&h.peers.notifications);
    by().post(&path(&Uuid::new_v4().to_string()), json!({}))
        .await
        .expect_error(404, "NOT_FOUND");
    by().post(&path("not-a-uuid"), json!({}))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "x5-bad").await["id"]);
    let (inv, _) = h.invite(&org, owner, "bad@example.com", "MEMBER").await;
    // Unknown body fields are refused.
    by().post(&path(&s(&inv["id"])), json!({ "email": "bad@example.com" }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // Only POST is routed.
    by().get(&path(&s(&inv["id"])))
        .await
        .expect_error(405, "METHOD_NOT_ALLOWED");
}
