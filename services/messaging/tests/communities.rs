//! ADR 0073 — Communities: role boundaries (owner, community admin, group admin, member),
//! `404` for everyone who is not a member, join requests (community and restricted groups),
//! linking a group without exposing its earlier messages, members or count, bans that reach
//! the community's groups (also through a group's own invite link), invite-link and
//! invitation revocation, the Announcements group (only admins post; its membership follows
//! the community), idempotent creation, ownership transfer, deletion and the chat-list
//! preference — against the real router and PostgreSQL.

mod common;

use axum::http::Method;
use common::{flows::ALL_SCOPES, *};
use serde_json::{json, Value};
use uuid::Uuid;

struct World {
    h: Harness,
    c: Customer,
}

async fn world() -> Option<World> {
    let h = Harness::new().await?;
    let c = h.customer(ALL_SCOPES);
    h.allow_managed(&c, "GROUP").await;
    Some(World { h, c })
}

impl World {
    fn person(&self) -> Uuid {
        self.h.principal(&self.c)
    }
    async fn call(&self, method: Method, who: Uuid, path: &str, body: Option<Value>) -> Response {
        self.h
            .send(method, Some(&self.c.token), Some(who), path, body, &[])
            .await
    }
    async fn get(&self, who: Uuid, path: &str) -> Response {
        self.call(Method::GET, who, path, None).await
    }
    async fn post(&self, who: Uuid, path: &str, body: Value) -> Response {
        self.call(Method::POST, who, path, Some(body)).await
    }
    async fn community(&self, owner: Uuid, privacy: &str) -> (Uuid, Uuid) {
        let r = self
            .post(
                owner,
                "/v1/messaging/communities",
                json!({ "name": "Neighbours", "description": "Our street", "rules": "Be kind", "privacy": privacy }),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        assert_eq!(r.json["myRole"], "OWNER");
        let id = Uuid::parse_str(r.json["communityId"].as_str().unwrap()).unwrap();
        let ann = Uuid::parse_str(r.json["announcementsConversationId"].as_str().unwrap()).unwrap();
        (id, ann)
    }
    async fn link_token(&self, who: Uuid, id: Uuid) -> String {
        let r = self
            .post(
                who,
                &format!("/v1/messaging/communities/{id}/invite-links"),
                json!({}),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        let token = r.json["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("mcl_"));
        token
    }
    async fn join(&self, who: Uuid, token: &str) -> Response {
        self.post(
            who,
            "/v1/messaging/communities/invite-links/join",
            json!({ "token": token }),
        )
        .await
    }
    /// A member of an OPEN community, joined by link.
    async fn member_of(&self, owner: Uuid, id: Uuid) -> Uuid {
        let token = self.link_token(owner, id).await;
        let p = self.person();
        let r = self.join(p, &token).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        assert_eq!(r.json["outcome"], "JOINED");
        p
    }
    async fn group(&self, owner: Uuid, title: &str) -> Uuid {
        let r = self
            .h
            .create_conversation(
                &self.c,
                owner,
                json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "title": title }),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        Uuid::parse_str(r.json["conversationId"].as_str().unwrap()).unwrap()
    }
    async fn send(&self, who: Uuid, conversation: Uuid, text: &str) -> Response {
        self.post(
            who,
            &format!("/v1/conversations/{conversation}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": text }),
        )
        .await
    }
    async fn is_conversation_member(&self, who: Uuid, conversation: Uuid) -> bool {
        self.get(who, &format!("/v1/conversations/{conversation}"))
            .await
            .status
            .as_u16()
            == 200
    }
    async fn role(&self, who: Uuid, id: Uuid, target: Uuid, role: &str) -> Response {
        self.call(
            Method::PUT,
            who,
            &format!("/v1/messaging/communities/{id}/members/{target}/role"),
            Some(json!({ "role": role })),
        )
        .await
    }
    async fn create_keyed(&self, who: Uuid, body: Value, key: &str) -> Response {
        self.h
            .send(
                Method::POST,
                Some(&self.c.token),
                Some(who),
                "/v1/messaging/communities",
                Some(body),
                &[("idempotency-key", key)],
            )
            .await
    }
    async fn groups(&self, who: Uuid, id: Uuid) -> Vec<Value> {
        let r = self
            .get(who, &format!("/v1/messaging/communities/{id}/groups"))
            .await;
        assert_eq!(r.status.as_u16(), 200, "{}", r.text);
        r.json["groups"].as_array().unwrap().clone()
    }
}

fn group_of(groups: &[Value], conversation: Uuid) -> Value {
    groups
        .iter()
        .find(|g| g["conversationId"] == json!(conversation))
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn creation_makes_an_announcements_group_only_admins_post_in() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, ann) = w.community(owner, "OPEN").await;
    let conv = w.get(owner, &format!("/v1/conversations/{ann}")).await;
    assert_eq!(conv.status.as_u16(), 200, "{}", conv.text);
    assert_eq!(conv.json["announcementOnly"], true);
    assert_eq!(conv.json["securityProfile"], "MANAGED");
    let member = w.member_of(owner, id).await;
    // The member is in the Announcements group, reads it, and cannot post.
    assert!(w.is_conversation_member(member, ann).await);
    let r = w.send(member, ann, "hello").await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert_eq!(r.reason(), "ANNOUNCEMENT_ONLY");
    assert_eq!(w.send(owner, ann, "Welcome").await.status.as_u16(), 201);
    // A promoted admin posts.
    let r = w
        .call(
            Method::PUT,
            owner,
            &format!("/v1/messaging/communities/{id}/members/{member}/role"),
            Some(json!({ "role": "ADMIN" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(
        w.send(member, ann, "From an admin").await.status.as_u16(),
        201
    );
    // Its membership follows the community: leaving it directly is refused.
    let r = w
        .call(
            Method::DELETE,
            member,
            &format!("/v1/conversations/{ann}/members/{member}"),
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    assert_eq!(r.reason(), "COMMUNITY_ANNOUNCEMENTS_MEMBERSHIP");
    // The view: counts, joined groups, the Announcements group first.
    let v = w
        .get(member, &format!("/v1/messaging/communities/{id}"))
        .await;
    assert_eq!(v.status.as_u16(), 200, "{}", v.text);
    assert_eq!(v.json["memberCount"], 2);
    assert_eq!(v.json["groupCount"], 1);
    assert_eq!(v.json["joinedConversationIds"], json!([ann]));
    let list = w.get(member, "/v1/messaging/communities").await;
    assert_eq!(list.json["communities"][0]["communityId"], json!(id));
}

#[tokio::test]
async fn creation_is_idempotent_and_never_needs_an_organization() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let body = json!({ "name": "Book club" });
    let a = w.create_keyed(owner, body.clone(), "community-key-1").await;
    assert_eq!(a.status.as_u16(), 201, "{}", a.text);
    let b = w.create_keyed(owner, body, "community-key-1").await;
    assert_eq!(b.status.as_u16(), 200, "{}", b.text);
    assert_eq!(a.json["communityId"], b.json["communityId"]);
    assert_eq!(
        a.json["announcementsConversationId"],
        b.json["announcementsConversationId"]
    );
    let list = w.get(owner, "/v1/messaging/communities").await;
    assert_eq!(list.json["communities"].as_array().unwrap().len(), 1);
    // Validation.
    let r = w
        .post(owner, "/v1/messaging/communities", json!({ "name": " " }))
        .await;
    assert_eq!(r.status.as_u16(), 400);
    let r = w
        .post(
            owner,
            "/v1/messaging/communities",
            json!({ "name": "x", "privacy": "SECRET" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400);
}

#[tokio::test]
async fn non_members_get_404_everywhere() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, ann) = w.community(owner, "OPEN").await;
    let outsider = w.person();
    let base = format!("/v1/messaging/communities/{id}");
    for (method, path, body) in [
        (Method::GET, base.clone(), None),
        (Method::PATCH, base.clone(), Some(json!({ "name": "Mine" }))),
        (Method::DELETE, base.clone(), None),
        (Method::GET, format!("{base}/members"), None),
        (Method::GET, format!("{base}/groups"), None),
        (Method::GET, format!("{base}/join-requests"), None),
        (Method::GET, format!("{base}/invitations"), None),
        (Method::GET, format!("{base}/invite-links"), None),
        (
            Method::POST,
            format!("{base}/invite-links"),
            Some(json!({})),
        ),
        (Method::GET, format!("{base}/bans"), None),
        (Method::POST, format!("{base}/leave"), None),
        (
            Method::POST,
            format!("{base}/groups/{ann}/join"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("{base}/invitations"),
            Some(json!({ "principalIds": [Uuid::new_v4()] })),
        ),
        (
            Method::POST,
            format!("{base}/owner"),
            Some(json!({ "principalId": outsider })),
        ),
    ] {
        let r = w.call(method.clone(), outsider, &path, body).await;
        assert_eq!(r.status.as_u16(), 404, "{method} {path}: {}", r.text);
        assert_eq!(r.code(), "NOT_FOUND");
        assert!(!r.text.contains("Neighbours"));
    }
    // The community does not exist for them, nor its Announcements group.
    assert!(!w.is_conversation_member(outsider, ann).await);
    // A random community id answers the same.
    let r = w
        .get(
            outsider,
            &format!("/v1/messaging/communities/{}", Uuid::new_v4()),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404);
    assert!(
        w.get(outsider, "/v1/messaging/communities").await.json["communities"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn roles_are_enforced_by_the_server() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, _) = w.community(owner, "OPEN").await;
    let a = w.member_of(owner, id).await;
    let b = w.member_of(owner, id).await;
    let c = w.member_of(owner, id).await;
    let base = format!("/v1/messaging/communities/{id}");
    // A member changes nothing.
    let r = w
        .call(Method::PATCH, a, &base, Some(json!({ "name": "Taken" })))
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert_eq!(r.reason(), "NOT_COMMUNITY_ADMIN");
    assert_eq!(w.role(a, id, b, "ADMIN").await.status.as_u16(), 403);
    let r = w
        .call(Method::DELETE, a, &format!("{base}/members/{b}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .post(a, &format!("{base}/bans"), json!({ "principalId": b }))
        .await;
    assert_eq!(r.status.as_u16(), 403);
    assert_eq!(w.get(a, &format!("{base}/bans")).await.status.as_u16(), 403);
    assert_eq!(
        w.call(Method::DELETE, a, &base, None).await.status.as_u16(),
        403
    );
    // The owner promotes a; an admin edits and promotes members ...
    assert_eq!(w.role(owner, id, a, "ADMIN").await.status.as_u16(), 200);
    let r = w
        .call(
            Method::PATCH,
            a,
            &base,
            Some(json!({ "rules": "No spam", "privacy": "APPROVAL" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["privacy"], "APPROVAL");
    assert_eq!(w.role(a, id, b, "ADMIN").await.status.as_u16(), 200);
    // ... but never demotes or removes another admin, the owner, or deletes.
    let r = w.role(a, id, b, "MEMBER").await;
    assert_eq!(r.status.as_u16(), 403);
    assert_eq!(r.reason(), "NOT_COMMUNITY_OWNER");
    let r = w
        .call(Method::DELETE, a, &format!("{base}/members/{b}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .call(Method::DELETE, a, &format!("{base}/members/{owner}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 409);
    assert_eq!(r.reason(), "OWNER_CANNOT_BE_REMOVED");
    assert_eq!(w.role(a, id, owner, "MEMBER").await.status.as_u16(), 409);
    assert_eq!(
        w.call(Method::DELETE, a, &base, None).await.status.as_u16(),
        403
    );
    // An admin removes a plain member (who loses the Announcements group too).
    let ann = Uuid::parse_str(
        w.get(owner, &base).await.json["announcementsConversationId"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(w.is_conversation_member(c, ann).await);
    let r = w
        .call(Method::DELETE, a, &format!("{base}/members/{c}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert!(!w.is_conversation_member(c, ann).await);
    assert_eq!(w.get(c, &base).await.status.as_u16(), 404);
    // An admin may step down; the owner demotes and removes admins.
    assert_eq!(w.role(b, id, b, "MEMBER").await.status.as_u16(), 200);
    assert_eq!(w.role(owner, id, a, "MEMBER").await.status.as_u16(), 200);
    // The owner cannot leave without handing over; after transfer the old owner is an admin.
    let r = w.post(owner, &format!("{base}/leave"), json!({})).await;
    assert_eq!(r.status.as_u16(), 409);
    assert_eq!(r.reason(), "OWNER_MUST_TRANSFER_OR_DELETE");
    let r = w
        .post(owner, &format!("{base}/owner"), json!({ "principalId": a }))
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["ownerPrincipalId"], json!(a));
    assert_eq!(r.json["myRole"], "ADMIN");
    let conv = w.get(a, &format!("/v1/conversations/{ann}")).await;
    assert_eq!(conv.json["ownerPrincipalId"], json!(a), "{}", conv.text);
    let r = w.post(owner, &format!("{base}/leave"), json!({})).await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(w.get(owner, &base).await.status.as_u16(), 404);
    // The new owner deletes: everyone loses it.
    let r = w.call(Method::DELETE, a, &base, None).await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(w.get(b, &base).await.status.as_u16(), 404);
    assert_eq!(w.get(a, &base).await.status.as_u16(), 404);
}

#[tokio::test]
async fn linking_a_group_exposes_no_earlier_messages_members_or_count() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, _) = w.community(owner, "OPEN").await;
    let base = format!("/v1/messaging/communities/{id}");
    // An existing group with history and a member who is not in the community.
    let g = w.group(owner, "Gardening").await;
    let old_friend = w.person();
    let r = w
        .post(
            owner,
            &format!("/v1/conversations/{g}/members"),
            json!({ "principalId": old_friend }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    assert_eq!(
        w.send(owner, g, "before-link-secret").await.status.as_u16(),
        201
    );
    let member = w.member_of(owner, id).await;
    // Only an admin of the group links it: a community member who is not one cannot.
    let r = w
        .post(
            member,
            &format!("{base}/groups/link"),
            json!({ "conversationId": g }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let r = w
        .post(
            owner,
            &format!("{base}/groups/link"),
            json!({ "conversationId": g }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    assert_eq!(r.json["joined"], true);
    // A group belongs to one community only.
    let (other, _) = w.community(owner, "OPEN").await;
    let r = w
        .post(
            owner,
            &format!("/v1/messaging/communities/{other}/groups/link"),
            json!({ "conversationId": g }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409);
    assert_eq!(r.reason(), "GROUP_ALREADY_IN_A_COMMUNITY");
    // A non-joined member sees the title, never the count or the members.
    let listed = group_of(&w.groups(member, id).await, g);
    assert_eq!(listed["title"], "Gardening");
    assert_eq!(listed["joined"], false);
    assert!(listed["memberCount"].is_null());
    assert!(!w.is_conversation_member(member, g).await);
    let r = w
        .get(member, &format!("/v1/conversations/{g}/members"))
        .await;
    assert_eq!(r.status.as_u16(), 404);
    assert_eq!(w.send(owner, g, "between").await.status.as_u16(), 201);
    // Joining opens a new history window: nothing sent before is visible.
    let r = w
        .post(member, &format!("{base}/groups/{g}/join"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    assert_eq!(r.json["outcome"], "JOINED");
    assert_eq!(w.send(owner, g, "after-join").await.status.as_u16(), 201);
    let msgs = w
        .get(member, &format!("/v1/conversations/{g}/messages"))
        .await;
    assert_eq!(msgs.status.as_u16(), 200, "{}", msgs.text);
    assert!(msgs.text.contains("after-join"), "{}", msgs.text);
    assert!(!msgs.text.contains("before-link-secret"));
    assert!(!msgs.text.contains("between"));
    // Joining again changes nothing.
    let r = w
        .post(member, &format!("{base}/groups/{g}/join"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(r.json["outcome"], "ALREADY_MEMBER");
    let listed = group_of(&w.groups(member, id).await, g);
    assert_eq!(listed["memberCount"], 3);
    // Linking changed nothing about the group: the old friend is still in it, not in the community.
    assert!(w.is_conversation_member(old_friend, g).await);
    assert_eq!(w.get(old_friend, &base).await.status.as_u16(), 404);
    // Unlinking keeps the group and its members.
    let r = w
        .call(Method::DELETE, member, &format!("{base}/groups/{g}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .call(Method::DELETE, owner, &format!("{base}/groups/{g}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert!(w.is_conversation_member(member, g).await);
    assert!(group_of(&w.groups(member, id).await, g).is_null());
    // The Announcements group cannot be unlinked.
    let ann = w.get(owner, &base).await.json["announcementsConversationId"].clone();
    let r = w
        .call(
            Method::DELETE,
            owner,
            &format!("{base}/groups/{}", ann.as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 409);
}

#[tokio::test]
async fn groups_are_created_reordered_and_restricted_groups_take_requests() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, _) = w.community(owner, "OPEN").await;
    let base = format!("/v1/messaging/communities/{id}");
    let member = w.member_of(owner, id).await;
    // Members add no groups unless allowed.
    let r = w
        .post(
            member,
            &format!("{base}/groups"),
            json!({ "title": "Mine" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let a = w
        .post(
            owner,
            &format!("{base}/groups"),
            json!({ "title": "Events", "description": "Plans" }),
        )
        .await;
    assert_eq!(a.status.as_u16(), 201, "{}", a.text);
    let a = Uuid::parse_str(a.json["conversationId"].as_str().unwrap()).unwrap();
    let b = w
        .post(
            owner,
            &format!("{base}/groups"),
            json!({ "title": "Committee", "access": "RESTRICTED" }),
        )
        .await;
    assert_eq!(b.status.as_u16(), 201, "{}", b.text);
    assert_eq!(b.json["access"], "RESTRICTED");
    let b = Uuid::parse_str(b.json["conversationId"].as_str().unwrap()).unwrap();
    // Reorder: Announcements first, then the listed order.
    let r = w
        .call(
            Method::PUT,
            owner,
            &format!("{base}/groups/order"),
            Some(json!({ "conversationIds": [b, a] })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let order: Vec<Value> = r.json["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["conversationId"].clone())
        .collect();
    assert_eq!(order[1..], [json!(b), json!(a)]);
    assert_eq!(r.json["groups"][0]["announcements"], true);
    let r = w
        .call(
            Method::PUT,
            member,
            &format!("{base}/groups/order"),
            Some(json!({ "conversationIds": [a, b] })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    // A restricted group takes a request; the group's admin (here the owner) approves.
    let r = w
        .post(
            member,
            &format!("{base}/groups/{b}/join"),
            json!({ "message": "Please" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 202, "{}", r.text);
    assert_eq!(r.json["outcome"], "REQUESTED");
    let request = r.json["request"]["requestId"].as_str().unwrap().to_string();
    assert!(!w.is_conversation_member(member, b).await);
    // Asking again returns the same open request.
    let again = w
        .post(member, &format!("{base}/groups/{b}/join"), json!({}))
        .await;
    assert_eq!(again.json["request"]["requestId"], json!(request));
    assert_eq!(
        group_of(&w.groups(member, id).await, b)["pendingRequest"],
        true
    );
    // The member cannot decide it.
    let r = w
        .post(
            member,
            &format!("{base}/join-requests/{request}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let queue = w.get(owner, &format!("{base}/join-requests")).await;
    assert_eq!(queue.status.as_u16(), 200, "{}", queue.text);
    assert_eq!(queue.json["requests"][0]["requestId"], json!(request));
    assert_eq!(queue.json["requests"][0]["groupTitle"], "Committee");
    let r = w
        .post(
            owner,
            &format!("{base}/join-requests/{request}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "APPROVED");
    assert!(w.is_conversation_member(member, b).await);
    let r = w
        .post(
            owner,
            &format!("{base}/join-requests/{request}/reject"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409);
    // A group admin who is not a community admin decides only their group's requests.
    let other = w.member_of(owner, id).await;
    let r = w
        .call(
            Method::PATCH,
            owner,
            &format!("/v1/conversations/{b}/members/{member}"),
            Some(json!({ "role": "ADMIN" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let r = w
        .post(other, &format!("{base}/groups/{b}/join"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 202);
    let request = r.json["request"]["requestId"].as_str().unwrap().to_string();
    let queue = w.get(member, &format!("{base}/join-requests")).await;
    assert_eq!(queue.status.as_u16(), 200, "{}", queue.text);
    assert_eq!(queue.json["requests"].as_array().unwrap().len(), 1);
    let r = w
        .post(
            member,
            &format!("{base}/join-requests/{request}/reject"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "REJECTED");
    assert!(!w.is_conversation_member(other, b).await);
    // The group admin manages the group's access, not the community.
    let r = w
        .call(
            Method::PATCH,
            member,
            &format!("{base}/groups/{b}"),
            Some(json!({ "access": "OPEN" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let r = w
        .call(Method::PATCH, member, &base, Some(json!({ "name": "x" })))
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .call(
            Method::PATCH,
            member,
            &format!("{base}/groups/{a}"),
            Some(json!({ "access": "RESTRICTED" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    // Members may add groups once allowed.
    let r = w
        .call(
            Method::PATCH,
            owner,
            &base,
            Some(json!({ "membersCanAddGroups": true })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let r = w
        .post(
            other,
            &format!("{base}/groups"),
            json!({ "title": "Walks" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
}

#[tokio::test]
async fn approval_communities_take_join_requests() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, ann) = w.community(owner, "APPROVAL").await;
    let base = format!("/v1/messaging/communities/{id}");
    let token = w.link_token(owner, id).await;
    let p = w.person();
    let preview = w
        .post(
            p,
            "/v1/messaging/communities/invite-links/preview",
            json!({ "token": token }),
        )
        .await;
    assert_eq!(preview.status.as_u16(), 200, "{}", preview.text);
    assert_eq!(preview.json["name"], "Neighbours");
    assert_eq!(preview.json["member"], false);
    let r = w
        .post(
            p,
            "/v1/messaging/communities/invite-links/join",
            json!({ "token": token, "message": "I live at 12" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 202, "{}", r.text);
    assert_eq!(r.json["outcome"], "REQUESTED");
    let request = r.json["request"]["requestId"].as_str().unwrap().to_string();
    // Not a member yet: still 404, nothing of the Announcements group.
    assert_eq!(w.get(p, &base).await.status.as_u16(), 404);
    assert!(!w.is_conversation_member(p, ann).await);
    let mine = w.get(p, "/v1/messaging/communities/join-requests").await;
    assert_eq!(mine.json["requests"][0]["state"], "PENDING");
    assert_eq!(mine.json["requests"][0]["communityName"], "Neighbours");
    let v = w.get(owner, &base).await;
    assert_eq!(v.json["pendingRequestCount"], 1);
    let r = w
        .post(
            owner,
            &format!("{base}/join-requests/{request}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(w.get(p, &base).await.status.as_u16(), 200);
    assert!(w.is_conversation_member(p, ann).await);
    // Reject and cancel.
    let q = w.person();
    let r = w.join(q, &token).await;
    let rq = r.json["request"]["requestId"].as_str().unwrap().to_string();
    let r = w
        .post(
            owner,
            &format!("{base}/join-requests/{rq}/reject"),
            json!({}),
        )
        .await;
    assert_eq!(r.json["state"], "REJECTED");
    assert_eq!(w.get(q, &base).await.status.as_u16(), 404);
    let r = w.join(q, &token).await;
    let rq = r.json["request"]["requestId"].as_str().unwrap().to_string();
    let other = w.person();
    let r = w
        .post(
            other,
            &format!("/v1/messaging/communities/join-requests/{rq}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404);
    let r = w
        .post(
            q,
            &format!("/v1/messaging/communities/join-requests/{rq}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "CANCELLED");
    // A member's invitation asks too; an admin's admits at once.
    let invitee = w.person();
    let r = w
        .post(
            p,
            &format!("{base}/invitations"),
            json!({ "principalIds": [invitee] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let inv = r.json["invitations"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .post(
            invitee,
            &format!("/v1/messaging/communities/invitations/{inv}/accept"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 202, "{}", r.text);
    let invitee2 = w.person();
    let r = w
        .post(
            owner,
            &format!("{base}/invitations"),
            json!({ "principalIds": [invitee2, owner] }),
        )
        .await;
    assert_eq!(r.json["skipped"][0]["reason"], "SELF", "{}", r.text);
    let inv = r.json["invitations"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    let mine = w
        .get(invitee2, "/v1/messaging/communities/invitations")
        .await;
    assert_eq!(
        mine.json["invitations"][0]["community"]["name"],
        "Neighbours"
    );
    let r = w
        .post(
            invitee2,
            &format!("/v1/messaging/communities/invitations/{inv}/accept"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["outcome"], "JOINED");
    assert_eq!(r.json["community"]["myRole"], "MEMBER");
}

#[tokio::test]
async fn invite_links_and_invitations_can_be_revoked() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, _) = w.community(owner, "INVITE_ONLY").await;
    let base = format!("/v1/messaging/communities/{id}");
    let token = w.link_token(owner, id).await;
    let links = w.get(owner, &format!("{base}/invite-links")).await;
    assert!(
        !links.text.contains(&token),
        "a token is never listed again"
    );
    let link = links.json["inviteLinks"][0]["inviteLinkId"]
        .as_str()
        .unwrap()
        .to_string();
    let first = w.person();
    assert_eq!(w.join(first, &token).await.status.as_u16(), 201);
    // Members of an INVITE_ONLY community neither invite nor create links.
    let r = w
        .post(first, &format!("{base}/invite-links"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .post(
            first,
            &format!("{base}/invitations"),
            json!({ "principalIds": [w.person()] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .post(
            first,
            &format!("{base}/invite-links/{link}/revoke"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .post(
            owner,
            &format!("{base}/invite-links/{link}/revoke"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["status"], "REVOKED");
    // A revoked, unknown or malformed link answers the same.
    for t in [
        token.clone(),
        format!("mcl_{}", "A".repeat(43)),
        "nope".into(),
    ] {
        let r = w.join(w.person(), &t).await;
        assert_eq!(r.status.as_u16(), 404, "{}", r.text);
        assert_eq!(r.reason(), "INVITE_LINK_UNAVAILABLE");
        let r = w
            .post(
                w.person(),
                "/v1/messaging/communities/invite-links/preview",
                json!({ "token": t }),
            )
            .await;
        assert_eq!(r.status.as_u16(), 404);
    }
    // A use-capped link runs out.
    let r = w
        .post(
            owner,
            &format!("{base}/invite-links"),
            json!({ "maxUses": 1 }),
        )
        .await;
    let capped = r.json["token"].as_str().unwrap().to_string();
    assert_eq!(w.join(w.person(), &capped).await.status.as_u16(), 201);
    assert_eq!(w.join(w.person(), &capped).await.status.as_u16(), 404);
    // A revoked invitation can no longer be accepted.
    let invitee = w.person();
    let r = w
        .post(
            owner,
            &format!("{base}/invitations"),
            json!({ "principalIds": [invitee] }),
        )
        .await;
    let inv = r.json["invitations"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    let open = w.get(owner, &format!("{base}/invitations")).await;
    assert_eq!(open.json["invitations"].as_array().unwrap().len(), 1);
    let r = w
        .call(
            Method::DELETE,
            owner,
            &format!("{base}/invitations/{inv}"),
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "REVOKED");
    let r = w
        .post(
            invitee,
            &format!("/v1/messaging/communities/invitations/{inv}/accept"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409);
    assert_eq!(w.get(invitee, &base).await.status.as_u16(), 404);
    // Someone else's invitation is not theirs to answer.
    let r = w
        .post(
            owner,
            &format!("{base}/invitations"),
            json!({ "principalIds": [invitee] }),
        )
        .await;
    let inv = r.json["invitations"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .post(
            w.person(),
            &format!("/v1/messaging/communities/invitations/{inv}/accept"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404);
    let r = w
        .post(
            invitee,
            &format!("/v1/messaging/communities/invitations/{inv}/decline"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "DECLINED");
}

#[tokio::test]
async fn bans_reach_the_community_and_its_groups() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, ann) = w.community(owner, "OPEN").await;
    let base = format!("/v1/messaging/communities/{id}");
    let g = w
        .post(owner, &format!("{base}/groups"), json!({ "title": "Chat" }))
        .await;
    let g = Uuid::parse_str(g.json["conversationId"].as_str().unwrap()).unwrap();
    let token = w.link_token(owner, id).await;
    let troll = w.person();
    assert_eq!(w.join(troll, &token).await.status.as_u16(), 201);
    let r = w
        .post(troll, &format!("{base}/groups/{g}/join"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let r = w
        .post(
            owner,
            &format!("{base}/bans"),
            json!({ "principalId": troll }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    // Out of the community, its Announcements group and its groups.
    assert_eq!(w.get(troll, &base).await.status.as_u16(), 404);
    assert!(!w.is_conversation_member(troll, ann).await);
    assert!(!w.is_conversation_member(troll, g).await);
    // Refused again by link, by invitation, and by the group's own admission paths.
    let r = w.join(troll, &token).await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert_eq!(r.reason(), "PRINCIPAL_BANNED");
    let r = w
        .post(
            owner,
            &format!("{base}/invitations"),
            json!({ "principalIds": [troll] }),
        )
        .await;
    assert_eq!(r.json["skipped"][0]["reason"], "BANNED", "{}", r.text);
    let r = w
        .post(
            owner,
            &format!("/v1/conversations/{g}/members"),
            json!({ "principalId": troll }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert_eq!(r.reason(), "PRINCIPAL_BANNED");
    let link = w
        .post(
            owner,
            &format!("/v1/conversations/{g}/invite-links"),
            json!({}),
        )
        .await;
    assert_eq!(link.status.as_u16(), 201, "{}", link.text);
    let r = w
        .post(
            troll,
            "/v1/messaging/invite-links/join",
            json!({ "token": link.json["token"] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert!(!w.is_conversation_member(troll, g).await);
    let bans = w.get(owner, &format!("{base}/bans")).await;
    assert_eq!(bans.json["bans"][0]["principalId"], json!(troll));
    // Lifting the ban restores nothing, but they may join again.
    let r = w
        .call(Method::DELETE, owner, &format!("{base}/bans/{troll}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(w.get(troll, &base).await.status.as_u16(), 404);
    assert_eq!(w.join(troll, &token).await.status.as_u16(), 201);
    assert!(!w.is_conversation_member(troll, g).await);
}

#[tokio::test]
async fn members_page_and_the_chat_list_preference_is_per_person() {
    let Some(w) = world().await else { return };
    let owner = w.person();
    let (id, _) = w.community(owner, "OPEN").await;
    for _ in 0..3 {
        w.member_of(owner, id).await;
    }
    let page = w
        .get(
            owner,
            &format!("/v1/messaging/communities/{id}/members?limit=2"),
        )
        .await;
    assert_eq!(page.status.as_u16(), 200, "{}", page.text);
    assert_eq!(page.json["members"].as_array().unwrap().len(), 2);
    assert_eq!(page.json["members"][0]["role"], "OWNER");
    let cursor = page.json["nextCursor"].as_str().unwrap().to_string();
    let next = w
        .get(
            owner,
            &format!("/v1/messaging/communities/{id}/members?limit=2&cursor={cursor}"),
        )
        .await;
    assert_eq!(next.json["members"].as_array().unwrap().len(), 2);
    assert!(next.json["nextCursor"].is_null());
    let r = w.get(owner, "/v1/messaging/communities/preferences").await;
    assert_eq!(r.json["chatListMode"], "TOGETHER");
    let r = w
        .call(
            Method::PUT,
            owner,
            "/v1/messaging/communities/preferences",
            Some(json!({ "chatListMode": "SEPARATE" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(
        w.get(owner, "/v1/messaging/communities/preferences")
            .await
            .json["chatListMode"],
        "SEPARATE"
    );
    let someone = w.person();
    assert_eq!(
        w.get(someone, "/v1/messaging/communities/preferences")
            .await
            .json["chatListMode"],
        "TOGETHER"
    );
    let r = w
        .call(
            Method::PUT,
            owner,
            "/v1/messaging/communities/preferences",
            Some(json!({ "chatListMode": "MIXED" })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400);
}
