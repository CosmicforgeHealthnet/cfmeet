//! ADR 0072 — Channels, complete: unique handles, private channels enforced on every read
//! (detail, posts, media, directory), invite links, admins and their permission
//! boundaries, ownership transfer, rich posts (documents, link previews, polls with one vote
//! per follower, scheduled posts published by the sweep, pins, edits), stars, reports,
//! discovery (search, filters, keyset pages, suggestions), analytics from recorded events
//! (views deduplicated per person, no follower identity anywhere an admin looks), and
//! verification / moderation decided ONLY through the admin bridge.
//!
//! Communications (contacts) is a scripted Hub answering `contacts/eligible` like the real
//! one (mutual, unblocked contacts only).

mod common;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use axum::{http::Method, routing::post, Json, Router};
use cf_object_storage::{sha256_hex, MemoryObjectStore, SignedWrite};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SharedClock,
    SystemClock, TokenTtl,
};
use chrono::{DateTime, Duration, Utc};
use common::{flows::ALL_SCOPES, *};
use serde_json::{json, Value};
use uuid::Uuid;

/// The scripted Hub: symmetric mutual pairs.
#[derive(Clone, Default)]
struct Hub {
    pairs: Arc<Mutex<HashSet<(Uuid, Uuid)>>>,
}

impl Hub {
    fn connect(&self, a: Uuid, b: Uuid) {
        let key = if a < b { (a, b) } else { (b, a) };
        self.pairs.lock().unwrap().insert(key);
    }
}

async fn eligible(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let actor: Uuid = serde_json::from_value(body["actorPrincipalId"].clone()).unwrap();
    let among: Option<Vec<Uuid>> = body
        .get("principalIds")
        .map(|v| serde_json::from_value(v.clone()).unwrap());
    let since = (Utc::now() - Duration::days(30)).to_rfc3339();
    let contacts: Vec<Value> = hub
        .pairs
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(a, b)| {
            let other = if *a == actor {
                *b
            } else if *b == actor {
                *a
            } else {
                return None;
            };
            if among.as_ref().is_some_and(|l| !l.contains(&other)) {
                return None;
            }
            Some(json!({ "principalId": other, "eligibleSince": since }))
        })
        .collect();
    Json(json!({ "schemaVersion": 1, "contacts": contacts }))
}

struct World {
    h: Harness,
    c: Customer,
    hub: Hub,
    store: Arc<MemoryObjectStore>,
}

async fn world() -> Option<World> {
    let hub = Hub::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/internal/v1/communications/contacts/eligible",
            post(eligible),
        )
        .with_state(hub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(MemoryObjectStore::new());
    let s = store.clone();
    let mut h = Harness::build(move |state| {
        state.storage = s;
    })
    .await?;
    let c = h.customer(ALL_SCOPES);
    let clock: SharedClock = Arc::new(SystemClock);
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("channels-test").unwrap()),
        TokenTtl::default(),
        clock,
    ));
    let mut targets = InternalTargets::new(RuntimeEnvironment::Test);
    targets.insert(ServiceId::communications(), &url).unwrap();
    let client = Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    );
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        client,
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
    Some(World { h, c, hub, store })
}

const OP: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVAL: [(&str, &str); 3] = [
    ("x-cf-approver", "hq:staff-0002"),
    ("x-cf-approval-ref", "appr-0001"),
    (
        "x-cf-intent-digest",
        "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
    ),
];

impl World {
    fn person(&self) -> Uuid {
        self.h.principal_of_kind(&self.c, "NATIVE_CF_USER")
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
    async fn put(&self, who: Uuid, path: &str, body: Option<Value>) -> Response {
        self.call(Method::PUT, who, path, body).await
    }
    async fn channel(&self, owner: Uuid, body: Value) -> String {
        let r = self.post(owner, "/v1/messaging/channels", body).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        r.json["channelId"].as_str().unwrap().to_string()
    }
    async fn text_post(&self, who: Uuid, channel: &str, text: &str) -> String {
        let r = self
            .post(
                who,
                &format!("/v1/messaging/channels/{channel}/posts"),
                json!({ "text": text }),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        r.json["postId"].as_str().unwrap().to_string()
    }
    /// A bridge command (with operator, key and the second person).
    async fn bridge(&self, path: &str, body: Value) -> Response {
        let peers = peers();
        let key = Uuid::new_v4().to_string();
        let mut headers: Vec<(&str, &str)> = vec![OP, ("idempotency-key", key.as_str())];
        headers.extend(APPROVAL);
        self.h
            .call_internal(
                &peers.admin_bridge,
                Method::POST,
                path,
                Some(body),
                None,
                &headers,
            )
            .await
    }
    async fn bridge_read(&self, path: &str) -> Response {
        let peers = peers();
        self.h
            .call_internal(&peers.admin_bridge, Method::GET, path, None, None, &[OP])
            .await
    }
    fn tenant(&self) -> Uuid {
        self.c.context.scope.tenant_id
    }
    async fn upload(&self, who: Uuid, body: Value, bytes: &[u8]) -> Uuid {
        let r = self.post(who, "/v1/messaging/channels/media", body).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        let id = r.json["mediaId"].as_str().unwrap().to_string();
        let part_bytes = r.json["partBytes"].as_u64().unwrap() as usize;
        for (i, chunk) in bytes.chunks(part_bytes).enumerate() {
            let g = self
                .post(
                    who,
                    &format!("/v1/messaging/channels/media/{id}/parts/{i}"),
                    json!({ "sha256": sha256_hex(chunk) }),
                )
                .await;
            assert_eq!(g.status.as_u16(), 200, "{}", g.text);
            let u = &g.json["upload"];
            let write = SignedWrite {
                method: "PUT",
                url: u["url"].as_str().unwrap().to_string(),
                headers: u["headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|h| {
                        (
                            h["name"].as_str().unwrap().to_string(),
                            h["value"].as_str().unwrap().to_string(),
                        )
                    })
                    .collect(),
                expires_at: DateTime::parse_from_rfc3339(u["expiresAt"].as_str().unwrap())
                    .unwrap()
                    .with_timezone(&Utc),
            };
            self.store
                .upload_signed(&write, self.h.now(), chunk.to_vec())
                .unwrap();
        }
        let done = self
            .post(
                who,
                &format!("/v1/messaging/channels/media/{id}/complete"),
                json!({}),
            )
            .await;
        assert_eq!(done.status.as_u16(), 200, "{}", done.text);
        Uuid::parse_str(&id).unwrap()
    }
}

fn png(size: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.resize(size, 7);
    v
}

#[tokio::test]
async fn handles_are_unique_validated_and_reserved() {
    let Some(w) = world().await else {
        return;
    };
    let (a, b) = (w.person(), w.person());
    let id = w
        .channel(
            a,
            json!({ "name": "Daily News", "handle": "@Daily_News", "category": "NEWS", "language": "en", "country": "NG" }),
        )
        .await;
    let c = w.get(a, &format!("/v1/messaging/channels/{id}")).await;
    assert_eq!(c.json["handle"], "daily_news", "{}", c.text);
    assert_eq!(c.json["category"], "NEWS");
    assert_eq!(c.json["visibility"], "PUBLIC");
    assert_eq!(c.json["verified"], false);
    let taken = w
        .post(
            b,
            "/v1/messaging/channels",
            json!({ "name": "Copy", "handle": "daily_news" }),
        )
        .await;
    assert_eq!(
        (taken.status.as_u16(), taken.reason()),
        (409, "HANDLE_TAKEN"),
        "{}",
        taken.text
    );
    let reserved = w
        .post(
            b,
            "/v1/messaging/channels",
            json!({ "name": "Fake", "handle": "cfmeet_official" }),
        )
        .await;
    assert_eq!(reserved.status.as_u16(), 400, "{}", reserved.text);
    assert_eq!(reserved.reason(), "HANDLE_RESERVED");
    let bad = w
        .post(
            b,
            "/v1/messaging/channels",
            json!({ "name": "Bad", "category": "NOT_A_CATEGORY" }),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 400, "{}", bad.text);
    let l = w
        .get(b, "/v1/messaging/channels/lookup?handle=daily_news")
        .await;
    assert_eq!(l.json["available"], false, "{}", l.text);
    assert_eq!(l.json["channel"]["channelId"], json!(id));
    let free = w
        .get(b, "/v1/messaging/channels/lookup?handle=morning_brief")
        .await;
    assert_eq!(
        (free.json["valid"].clone(), free.json["available"].clone()),
        (json!(true), json!(true))
    );
    let r = w
        .get(b, "/v1/messaging/channels/lookup?handle=support")
        .await;
    assert_eq!(r.json["reason"], "RESERVED", "{}", r.text);
    // Only the owner changes the username; editors cannot.
    let edit = w
        .call(
            Method::PATCH,
            b,
            &format!("/v1/messaging/channels/{id}"),
            Some(json!({ "description": "x" })),
        )
        .await;
    assert_eq!(edit.status.as_u16(), 403, "{}", edit.text);
}

#[tokio::test]
async fn private_channels_are_for_members_only() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, outsider) = (w.person(), w.person());
    let id = w
        .channel(
            owner,
            json!({ "name": "Inner circle", "visibility": "PRIVATE", "handle": "inner_circle" }),
        )
        .await;
    let photo = w
        .upload(
            owner,
            json!({ "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 2048 }),
            &png(2048),
        )
        .await;
    let posted = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "mediaId": photo, "text": "members only" }),
        )
        .await;
    assert_eq!(posted.status.as_u16(), 201, "{}", posted.text);
    // Hidden from outsiders everywhere: detail, posts, media, directory, lookup.
    for path in [
        format!("/v1/messaging/channels/{id}"),
        format!("/v1/messaging/channels/{id}/posts"),
    ] {
        let r = w.get(outsider, &path).await;
        assert_eq!(r.status.as_u16(), 404, "{path}: {}", r.text);
    }
    let grants = w
        .post(
            outsider,
            &format!("/v1/messaging/channels/media/{photo}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(grants.status.as_u16(), 404, "{}", grants.text);
    let dir = w.get(outsider, "/v1/messaging/channels?q=inner").await;
    assert!(!dir.text.contains(&id), "{}", dir.text);
    let l = w
        .get(
            outsider,
            "/v1/messaging/channels/lookup?handle=inner_circle",
        )
        .await;
    assert_eq!(l.json["available"], false);
    assert_eq!(l.json["channel"], Value::Null, "{}", l.text);
    let f = w
        .put(
            outsider,
            &format!("/v1/messaging/channels/{id}/follow"),
            None,
        )
        .await;
    assert_eq!(f.status.as_u16(), 404, "{}", f.text);
    // An invite link lets them preview and join; only its digest is stored.
    let inv = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/invites"),
            json!({}),
        )
        .await;
    assert_eq!(inv.status.as_u16(), 201, "{}", inv.text);
    let token = inv.json["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("mci_"));
    let stored =
        w.h.scalar(&format!(
            "SELECT count(*) FROM messaging_channel_invites WHERE token_sha256 = '{}'",
            sha256_hex(token.as_bytes())
        ))
        .await;
    assert_eq!(stored, 1);
    let listed = w
        .get(owner, &format!("/v1/messaging/channels/{id}/invites"))
        .await;
    assert!(!listed.text.contains(&token), "{}", listed.text);
    let preview = w
        .post(
            outsider,
            "/v1/messaging/channels/invites/preview",
            json!({ "token": token }),
        )
        .await;
    assert_eq!(preview.json["name"], "Inner circle", "{}", preview.text);
    assert!(preview.json.get("lastPost").is_none());
    let joined = w
        .post(
            outsider,
            "/v1/messaging/channels/invites/join",
            json!({ "token": token }),
        )
        .await;
    assert_eq!(joined.json["following"], true, "{}", joined.text);
    let posts = w
        .get(outsider, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert_eq!(
        posts.json["posts"].as_array().unwrap().len(),
        1,
        "{}",
        posts.text
    );
    let grants = w
        .post(
            outsider,
            &format!("/v1/messaging/channels/media/{photo}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(grants.status.as_u16(), 200, "{}", grants.text);
    // A revoked link joins nobody.
    let invite_id = inv.json["inviteId"].as_str().unwrap();
    let rv = w
        .call(
            Method::DELETE,
            owner,
            &format!("/v1/messaging/channels/{id}/invites/{invite_id}"),
            None,
        )
        .await;
    assert_eq!(rv.json["revoked"], true, "{}", rv.text);
    let late = w.person();
    let refused = w
        .post(
            late,
            "/v1/messaging/channels/invites/join",
            json!({ "token": token }),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 404, "{}", refused.text);
    // Leaving a private channel hides it again.
    let left = w
        .call(
            Method::DELETE,
            outsider,
            &format!("/v1/messaging/channels/{id}/follow"),
            None,
        )
        .await;
    assert_eq!(left.json["following"], false, "{}", left.text);
    assert_eq!(
        w.get(outsider, &format!("/v1/messaging/channels/{id}"))
            .await
            .status
            .as_u16(),
        404
    );
}

#[tokio::test]
async fn admin_permissions_have_boundaries_and_ownership_moves_to_an_admin() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, poster, manager, stranger) = (w.person(), w.person(), w.person(), w.person());
    w.hub.connect(owner, poster);
    w.hub.connect(owner, manager);
    w.hub.connect(manager, stranger);
    let id = w.channel(owner, json!({ "name": "Team news" })).await;
    let admins = |who: Uuid| format!("/v1/messaging/channels/{id}/admins/{who}");
    // Only contacts become admins.
    let r = w
        .put(
            owner,
            &admins(stranger),
            Some(json!({ "canPost": true, "canEdit": false, "canManageAdmins": false })),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), r.reason()),
        (403, "NOT_A_CONTACT"),
        "{}",
        r.text
    );
    let r = w
        .put(
            owner,
            &admins(poster),
            Some(json!({ "canPost": true, "canEdit": false, "canManageAdmins": false })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let r = w
        .put(
            owner,
            &admins(manager),
            Some(json!({ "canPost": false, "canEdit": false, "canManageAdmins": true })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    // The poster posts, but cannot edit the channel, manage admins, delete it or delete
    // the owner's posts; they can delete their own.
    let own = w.text_post(poster, &id, "from the poster").await;
    let owners = w.text_post(owner, &id, "from the owner").await;
    let edit = w
        .call(
            Method::PATCH,
            poster,
            &format!("/v1/messaging/channels/{id}"),
            Some(json!({ "name": "Taken over" })),
        )
        .await;
    assert_eq!(edit.status.as_u16(), 403, "{}", edit.text);
    let add = w
        .put(
            poster,
            &admins(stranger),
            Some(json!({ "canPost": true, "canEdit": false, "canManageAdmins": false })),
        )
        .await;
    assert_eq!(add.status.as_u16(), 403, "{}", add.text);
    let del = w
        .call(
            Method::DELETE,
            poster,
            &format!("/v1/messaging/channels/{id}"),
            None,
        )
        .await;
    assert_eq!(del.status.as_u16(), 403, "{}", del.text);
    let theirs = w
        .call(
            Method::DELETE,
            poster,
            &format!("/v1/messaging/channels/{id}/posts/{owners}"),
            None,
        )
        .await;
    assert_eq!(theirs.status.as_u16(), 403, "{}", theirs.text);
    let mine = w
        .call(
            Method::DELETE,
            poster,
            &format!("/v1/messaging/channels/{id}/posts/{own}"),
            None,
        )
        .await;
    assert_eq!(mine.json["deleted"], true, "{}", mine.text);
    // The manager (no post / edit) cannot post, cannot grant what they lack or make another
    // manager; they can add their contact as a poster? No: they hold no post right.
    let denied = w
        .post(
            manager,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "hi" }),
        )
        .await;
    assert_eq!(denied.status.as_u16(), 403, "{}", denied.text);
    let grant = w
        .put(
            manager,
            &admins(stranger),
            Some(json!({ "canPost": true, "canEdit": false, "canManageAdmins": false })),
        )
        .await;
    assert_eq!(
        (grant.status.as_u16(), grant.reason()),
        (403, "PERMISSION_NOT_HELD"),
        "{}",
        grant.text
    );
    let list = w
        .get(manager, &format!("/v1/messaging/channels/{id}/admins"))
        .await;
    assert_eq!(
        list.json["admins"].as_array().unwrap().len(),
        2,
        "{}",
        list.text
    );
    // A follower sees neither the admin list nor the audit log.
    let follower = w.person();
    w.put(
        follower,
        &format!("/v1/messaging/channels/{id}/follow"),
        None,
    )
    .await;
    for path in [
        format!("/v1/messaging/channels/{id}/admins"),
        format!("/v1/messaging/channels/{id}/audit"),
        format!("/v1/messaging/channels/{id}/scheduled"),
    ] {
        let r = w.get(follower, &path).await;
        assert_eq!(r.status.as_u16(), 403, "{path}: {}", r.text);
    }
    // Ownership moves only to an admin; the old owner stays as a full admin.
    let to_follower = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/owner"),
            json!({ "principalId": follower }),
        )
        .await;
    assert_eq!(to_follower.status.as_u16(), 409, "{}", to_follower.text);
    let moved = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/owner"),
            json!({ "principalId": poster }),
        )
        .await;
    assert_eq!(moved.json["myRole"], "ADMIN", "{}", moved.text);
    let as_new = w.get(poster, &format!("/v1/messaging/channels/{id}")).await;
    assert_eq!(as_new.json["isOwner"], true, "{}", as_new.text);
    let audit = w
        .get(poster, &format!("/v1/messaging/channels/{id}/audit"))
        .await;
    assert!(
        audit.text.contains("OWNERSHIP_TRANSFERRED"),
        "{}",
        audit.text
    );
    assert!(audit.text.contains("ADMIN_ADDED"));
    // A demoted admin steps down by themselves.
    let down = w
        .call(Method::DELETE, manager, &admins(manager), None)
        .await;
    assert_eq!(down.status.as_u16(), 200, "{}", down.text);
}

#[tokio::test]
async fn polls_take_one_vote_per_follower() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, a, b, visitor) = (w.person(), w.person(), w.person(), w.person());
    let id = w.channel(owner, json!({ "name": "Polls" })).await;
    for p in [a, b] {
        w.put(p, &format!("/v1/messaging/channels/{id}/follow"), None)
            .await;
    }
    let single = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "poll": { "question": "Best day?", "options": ["Mon", "Fri"] } }),
        )
        .await;
    assert_eq!(single.json["kind"], "POLL", "{}", single.text);
    let pid = single.json["postId"].as_str().unwrap();
    let vote = format!("/v1/messaging/channels/{id}/posts/{pid}/vote");
    let r = w
        .put(a, &vote, Some(json!({ "optionIndexes": [0, 1] })))
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    let r = w.put(a, &vote, Some(json!({ "optionIndexes": [0] }))).await;
    assert_eq!(r.json["poll"]["options"][0]["votes"], 1, "{}", r.text);
    // Voting again replaces the vote: still one voter.
    let r = w.put(a, &vote, Some(json!({ "optionIndexes": [1] }))).await;
    assert_eq!(r.json["poll"]["totalVoters"], 1, "{}", r.text);
    assert_eq!(r.json["poll"]["options"][0]["votes"], 0);
    assert_eq!(r.json["poll"]["options"][1]["votes"], 1);
    assert_eq!(r.json["poll"]["myVotes"], json!([1]));
    let r = w.put(b, &vote, Some(json!({ "optionIndexes": [1] }))).await;
    assert_eq!(r.json["poll"]["totalVoters"], 2, "{}", r.text);
    let rows =
        w.h.scalar(&format!(
            "SELECT count(*) FROM messaging_channel_poll_votes WHERE post_id = '{pid}'"
        ))
        .await;
    assert_eq!(rows, 2);
    // Non-followers cannot vote.
    let r = w
        .put(visitor, &vote, Some(json!({ "optionIndexes": [0] })))
        .await;
    assert_eq!(
        (r.status.as_u16(), r.reason()),
        (403, "FOLLOW_TO_VOTE"),
        "{}",
        r.text
    );
    // Retracting removes it.
    let r = w.call(Method::DELETE, a, &vote, None).await;
    assert_eq!(r.json["poll"]["totalVoters"], 1, "{}", r.text);
    let multi = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "poll": { "question": "Topics?", "options": ["A", "B", "C"], "multiple": true } }),
        )
        .await;
    let mid = multi.json["postId"].as_str().unwrap();
    let r = w
        .put(
            a,
            &format!("/v1/messaging/channels/{id}/posts/{mid}/vote"),
            Some(json!({ "optionIndexes": [0, 2] })),
        )
        .await;
    assert_eq!(r.json["poll"]["myVotes"], json!([0, 2]), "{}", r.text);
    assert_eq!(r.json["poll"]["totalVoters"], 1);
}

#[tokio::test]
async fn scheduled_posts_wait_for_the_sweep_and_rich_posts_keep_their_shape() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, follower) = (w.person(), w.person());
    let id = w.channel(owner, json!({ "name": "Later" })).await;
    w.put(
        follower,
        &format!("/v1/messaging/channels/{id}/follow"),
        None,
    )
    .await;
    let at = w.h.now() + Duration::minutes(10);
    let s = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "at ten", "publishAt": at.to_rfc3339() }),
        )
        .await;
    assert_eq!(s.status.as_u16(), 201, "{}", s.text);
    assert_eq!(s.json["state"], "SCHEDULED");
    let sid = s.json["postId"].as_str().unwrap().to_string();
    let early = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "past", "publishAt": (w.h.now() - Duration::minutes(1)).to_rfc3339() }),
        )
        .await;
    assert_eq!(early.status.as_u16(), 400, "{}", early.text);
    let list = w
        .get(owner, &format!("/v1/messaging/channels/{id}/scheduled"))
        .await;
    assert_eq!(list.json["posts"][0]["postId"], json!(sid), "{}", list.text);
    let posts = w
        .get(follower, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert!(!posts.text.contains(&sid), "{}", posts.text);
    // Not due yet: the sweep leaves it.
    messaging::social::sweep(&w.h.state).await.unwrap();
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_channel_scheduled_posts")
            .await,
        1
    );
    w.h.clock.advance(Duration::minutes(11));
    messaging::social::sweep(&w.h.state).await.unwrap();
    let posts = w
        .get(follower, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert_eq!(
        posts.json["posts"][0]["postId"],
        json!(sid),
        "{}",
        posts.text
    );
    assert_eq!(posts.json["posts"][0]["state"], "PUBLISHED");
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_channel_scheduled_posts")
            .await,
        0
    );
    // A cancelled scheduled post never appears.
    let s2 = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "never", "publishAt": (w.h.now() + Duration::hours(2)).to_rfc3339() }),
        )
        .await;
    let s2id = s2.json["postId"].as_str().unwrap();
    let c = w
        .call(
            Method::DELETE,
            owner,
            &format!("/v1/messaging/channels/{id}/scheduled/{s2id}"),
            None,
        )
        .await;
    assert_eq!(c.json["deleted"], true, "{}", c.text);
    // Documents keep their name; link previews need their link; edits are marked; pins.
    let pdf = b"%PDF-1.7\n1 0 obj\n".repeat(20);
    let doc = w
        .upload(
            owner,
            json!({ "kind": "DOCUMENT", "contentType": "application/pdf", "sizeBytes": pdf.len(), "fileName": "../report.pdf" }),
            &pdf,
        )
        .await;
    let d = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "mediaId": doc }),
        )
        .await;
    assert_eq!(d.json["kind"], "DOCUMENT", "{}", d.text);
    assert_eq!(d.json["media"]["fileName"], "report.pdf");
    let bad = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "no link here", "linkPreview": { "url": "https://example.com" } }),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 400, "{}", bad.text);
    let l = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "read https://example.com/a", "linkPreview": { "url": "https://example.com/a", "title": "A", "siteName": "Example" } }),
        )
        .await;
    assert_eq!(l.json["linkPreview"]["title"], "A", "{}", l.text);
    let lid = l.json["postId"].as_str().unwrap();
    let e = w
        .call(
            Method::PATCH,
            owner,
            &format!("/v1/messaging/channels/{id}/posts/{lid}"),
            Some(json!({ "text": "changed, link gone" })),
        )
        .await;
    assert_eq!(e.json["edited"], true, "{}", e.text);
    assert_eq!(e.json["linkPreview"], Value::Null);
    let p = w
        .put(
            owner,
            &format!("/v1/messaging/channels/{id}/posts/{lid}/pin"),
            None,
        )
        .await;
    assert_eq!(p.json["pinned"], true, "{}", p.text);
    let ch = w
        .get(follower, &format!("/v1/messaging/channels/{id}"))
        .await;
    assert_eq!(
        ch.json["pinnedPosts"][0]["postId"],
        json!(lid),
        "{}",
        ch.text
    );
    let up = w
        .call(
            Method::DELETE,
            owner,
            &format!("/v1/messaging/channels/{id}/posts/{lid}/pin"),
            None,
        )
        .await;
    assert_eq!(up.json["pinned"], false, "{}", up.text);
    // Stars are a per-person list.
    let st = w
        .put(
            follower,
            &format!("/v1/messaging/channels/{id}/posts/{sid}/star"),
            None,
        )
        .await;
    assert_eq!(st.json["starred"], true, "{}", st.text);
    let mine = w.get(follower, "/v1/messaging/channels/starred").await;
    assert_eq!(
        mine.json["items"][0]["post"]["postId"],
        json!(sid),
        "{}",
        mine.text
    );
    let theirs = w.get(owner, "/v1/messaging/channels/starred").await;
    assert_eq!(theirs.json["items"].as_array().unwrap().len(), 0);
    let un = w
        .call(
            Method::DELETE,
            follower,
            &format!("/v1/messaging/channels/{id}/posts/{sid}/star"),
            None,
        )
        .await;
    assert_eq!(un.json["starred"], false, "{}", un.text);
    // Mute is the follower's own; forwards go only to mutual contacts.
    let m = w
        .put(follower, &format!("/v1/messaging/channels/{id}/mute"), None)
        .await;
    assert_eq!(m.json["muted"], true, "{}", m.text);
    let um = w
        .call(
            Method::DELETE,
            follower,
            &format!("/v1/messaging/channels/{id}/mute"),
            None,
        )
        .await;
    assert_eq!(um.json["muted"], false, "{}", um.text);
    let fw = w
        .post(
            follower,
            &format!("/v1/messaging/channels/{id}/posts/{sid}/forwards"),
            json!({ "principalIds": [w.person()] }),
        )
        .await;
    assert_eq!(
        (fw.json["forwarded"].clone(), fw.json["skipped"].clone()),
        (json!(0), json!(1)),
        "{}",
        fw.text
    );
}

#[tokio::test]
async fn analytics_count_recorded_events_and_dedupe_views() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, a, b) = (w.person(), w.person(), w.person());
    let id = w.channel(owner, json!({ "name": "Stats" })).await;
    for p in [a, b] {
        w.put(p, &format!("/v1/messaging/channels/{id}/follow"), None)
            .await;
    }
    let post = w.text_post(owner, &id, "count me").await;
    let views = format!("/v1/messaging/channels/{id}/posts/views");
    for _ in 0..3 {
        let r = w.post(a, &views, json!({ "postIds": [post] })).await;
        assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    }
    w.post(b, &views, json!({ "postIds": [post] })).await;
    // The owner's own views are not counted.
    let o = w.post(owner, &views, json!({ "postIds": [post] })).await;
    assert_eq!(o.json["recorded"], 0, "{}", o.text);
    w.put(
        a,
        &format!("/v1/messaging/channels/{id}/posts/{post}/reaction"),
        Some(json!({ "emoji": "\u{1F44D}" })),
    )
    .await;
    let un = w
        .call(
            Method::DELETE,
            b,
            &format!("/v1/messaging/channels/{id}/follow"),
            None,
        )
        .await;
    assert_eq!(un.json["following"], false, "{}", un.text);
    let r = w
        .get(owner, &format!("/v1/messaging/channels/{id}/analytics"))
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["posts"]["views"], 2, "{}", r.text);
    assert_eq!(r.json["posts"]["uniqueViewers"], 2);
    assert_eq!(r.json["posts"]["reactions"], 1);
    assert_eq!(r.json["followers"]["current"], 2);
    // Owner + a + b followed (3), b left (1).
    assert_eq!(r.json["followers"]["follows"], 3);
    assert_eq!(r.json["followers"]["unfollows"], 1);
    assert_eq!(r.json["engagementRate"], 0.5);
    assert_eq!(r.json["topPosts"][0]["postId"], json!(post));
    let today = w.h.now().date_naive().to_string();
    let ranged = w
        .get(
            owner,
            &format!("/v1/messaging/channels/{id}/analytics?from={today}&to={today}"),
        )
        .await;
    assert_eq!(
        ranged.json["series"].as_array().unwrap().len(),
        1,
        "{}",
        ranged.text
    );
    assert_eq!(ranged.json["series"][0]["followers"], 2);
    // Followers never see analytics, and nothing an admin reads names a follower.
    let denied = w
        .get(a, &format!("/v1/messaging/channels/{id}/analytics"))
        .await;
    assert_eq!(denied.status.as_u16(), 403, "{}", denied.text);
    for path in [
        format!("/v1/messaging/channels/{id}/analytics"),
        format!("/v1/messaging/channels/{id}"),
        format!("/v1/messaging/channels/{id}/admins"),
        format!("/v1/messaging/channels/{id}/audit"),
        format!("/v1/messaging/channels/{id}/posts"),
        format!("/v1/messaging/channels/{id}/moderation"),
        format!("/v1/messaging/channels/{id}/verification"),
        "/v1/messaging/channels/following".to_string(),
    ] {
        let r = w.get(owner, &path).await;
        assert_eq!(r.status.as_u16(), 200, "{path}: {}", r.text);
        for follower in [a, b] {
            assert!(
                !r.text.contains(&follower.to_string()),
                "{path} names a follower: {}",
                r.text
            );
        }
    }
}

#[tokio::test]
async fn discovery_searches_filters_and_pages() {
    let Some(w) = world().await else {
        return;
    };
    let me = w.person();
    let mut ids = Vec::new();
    for (i, (name, cat, country)) in [
        ("Lagos Football", "SPORTS", "NG"),
        ("Accra Football", "SPORTS", "GH"),
        ("Tech Weekly", "TECHNOLOGY", "NG"),
    ]
    .iter()
    .enumerate()
    {
        let owner = w.person();
        let id = w
            .channel(
                owner,
                json!({ "name": name, "category": cat, "country": country, "language": "en", "handle": format!("disc_{i}_chan") }),
            )
            .await;
        ids.push(id);
        w.h.clock.advance(Duration::seconds(1));
    }
    // Two followers make the first one the most popular.
    for _ in 0..2 {
        let p = w.person();
        w.put(
            p,
            &format!("/v1/messaging/channels/{}/follow", ids[0]),
            None,
        )
        .await;
    }
    let r = w
        .get(me, "/v1/messaging/channels?q=football&category=SPORTS")
        .await;
    let found: Vec<&str> = r.json["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["channelId"].as_str().unwrap())
        .collect();
    assert_eq!(found, vec![ids[0].as_str(), ids[1].as_str()], "{}", r.text);
    let ng = w
        .get(me, "/v1/messaging/channels?country=NG&sort=NEW")
        .await;
    assert_eq!(
        ng.json["channels"][0]["channelId"],
        json!(ids[2]),
        "{}",
        ng.text
    );
    // Keyset pages: NEW, one at a time.
    let first = w
        .get(me, "/v1/messaging/channels?sort=NEW&limit=1&language=en")
        .await;
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = w
        .get(
            me,
            &format!("/v1/messaging/channels?sort=NEW&limit=1&language=en&cursor={cursor}"),
        )
        .await;
    assert_ne!(
        first.json["channels"][0]["channelId"], second.json["channels"][0]["channelId"],
        "{}",
        second.text
    );
    let bad = w.get(me, "/v1/messaging/channels?cursor=nonsense").await;
    assert_eq!(bad.status.as_u16(), 400, "{}", bad.text);
    // Trending follows recorded follows.
    let t = w.get(me, "/v1/messaging/channels?sort=TRENDING").await;
    assert_eq!(
        t.json["channels"][0]["channelId"],
        json!(ids[0]),
        "{}",
        t.text
    );
    let v = w.get(me, "/v1/messaging/channels?sort=VERIFIED").await;
    assert_eq!(
        v.json["channels"].as_array().unwrap().len(),
        0,
        "{}",
        v.text
    );
    let s = w
        .get(me, "/v1/messaging/channels/suggestions?q=disc_2")
        .await;
    assert_eq!(
        s.json["suggestions"][0]["channelId"],
        json!(ids[2]),
        "{}",
        s.text
    );
    let none = w.get(me, "/v1/messaging/channels/suggestions").await;
    assert_eq!(none.status.as_u16(), 200, "{}", none.text);
    // Preview before following: a public channel's posts are readable.
    let preview = w
        .get(me, &format!("/v1/messaging/channels/{}/posts", ids[1]))
        .await;
    assert_eq!(preview.status.as_u16(), 200, "{}", preview.text);
}

#[tokio::test]
async fn verification_and_moderation_come_only_from_the_admin_bridge() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, follower) = (w.person(), w.person());
    let id = w
        .channel(owner, json!({ "name": "Brand", "handle": "brand_channel" }))
        .await;
    w.put(
        follower,
        &format!("/v1/messaging/channels/{id}/follow"),
        None,
    )
    .await;
    // No customer route verifies or moderates.
    for body in [
        json!({ "verified": true }),
        json!({ "moderationState": "ACTIVE" }),
    ] {
        let r = w
            .call(
                Method::PATCH,
                owner,
                &format!("/v1/messaging/channels/{id}"),
                Some(body),
            )
            .await;
        // An unknown field is refused (400 / 422) and changes nothing.
        assert!(matches!(r.status.as_u16(), 400 | 422), "{}", r.text);
    }
    let customer =
        w.h.send(
            Method::POST,
            Some(&w.c.token),
            Some(owner),
            &format!("/internal/v1/operator/channels/{id}/moderate"),
            Some(json!({})),
            &[],
        )
        .await;
    assert_eq!(customer.status.as_u16(), 401, "{}", customer.text);
    // Only the owner applies.
    let r = w
        .post(
            follower,
            &format!("/v1/messaging/channels/{id}/verification"),
            json!({ "basis": "BRAND" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let app = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/verification"),
            json!({ "basis": "BRAND" }),
        )
        .await;
    assert_eq!(app.status.as_u16(), 201, "{}", app.text);
    assert_eq!(app.json["state"], "PENDING");
    let again = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/verification"),
            json!({ "basis": "BRAND" }),
        )
        .await;
    assert_eq!(again.status.as_u16(), 409, "{}", again.text);
    let still = w
        .get(follower, &format!("/v1/messaging/channels/{id}"))
        .await;
    assert_eq!(still.json["verified"], false);
    // HQ's queue shows the application (handle and counts, no names).
    let queue = w
        .bridge_read("/internal/v1/operator/channel-verifications?status=PENDING")
        .await;
    assert_eq!(queue.status.as_u16(), 200, "{}", queue.text);
    let item = &queue.json["items"][0];
    assert_eq!(item["handle"], "brand_channel", "{}", queue.text);
    assert!(!queue.text.contains("\"Brand\""));
    let app_id = app.json["applicationId"].as_str().unwrap();
    // Without the second person nothing changes.
    let peers = peers();
    let key = Uuid::new_v4().to_string();
    let lone = w
        .h
        .call_internal(
            &peers.admin_bridge,
            Method::POST,
            &format!("/internal/v1/operator/channel-verifications/{app_id}/decide"),
            Some(json!({ "tenantId": w.tenant(), "expectedVersion": 1, "reasonCode": "MEETS_POLICY", "decision": "APPROVE" })),
            None,
            &[OP, ("idempotency-key", key.as_str())],
        )
        .await;
    assert_eq!(lone.status.as_u16(), 403, "{}", lone.text);
    let ok = w
        .bridge(
            &format!("/internal/v1/operator/channel-verifications/{app_id}/decide"),
            json!({ "tenantId": w.tenant(), "expectedVersion": 1, "reasonCode": "MEETS_POLICY", "decision": "APPROVE" }),
        )
        .await;
    assert_eq!(ok.json["status"], "APPROVED", "{}", ok.text);
    let verified = w
        .get(follower, &format!("/v1/messaging/channels/{id}"))
        .await;
    assert_eq!(verified.json["verified"], true, "{}", verified.text);
    // A verified channel keeps its username.
    let rename = w
        .call(
            Method::PATCH,
            owner,
            &format!("/v1/messaging/channels/{id}"),
            Some(json!({ "handle": "other_brand" })),
        )
        .await;
    assert_eq!(rename.status.as_u16(), 409, "{}", rename.text);
    // Revoke at the channel's version.
    let ch = w
        .bridge_read(&format!("/internal/v1/operator/channels/{id}"))
        .await;
    let version = ch.json["version"].as_i64().unwrap();
    assert_eq!(ch.json["verified"], true, "{}", ch.text);
    let stale = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/verification/revoke"),
            json!({ "tenantId": w.tenant(), "expectedVersion": version + 5, "reasonCode": "POLICY_BREACH" }),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409, "{}", stale.text);
    let revoked = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/verification/revoke"),
            json!({ "tenantId": w.tenant(), "expectedVersion": version, "reasonCode": "POLICY_BREACH" }),
        )
        .await;
    assert_eq!(revoked.json["status"], "REVOKED", "{}", revoked.text);
    let mine = w
        .get(owner, &format!("/v1/messaging/channels/{id}/verification"))
        .await;
    assert_eq!(mine.json["verified"], false, "{}", mine.text);
    assert_eq!(mine.json["applications"][0]["state"], "REVOKED");
    // Reports reach HQ without the reporter or the note.
    let post = w.text_post(owner, &id, "questionable").await;
    let rep = w
        .post(
            follower,
            &format!("/v1/messaging/channels/{id}/reports"),
            json!({ "category": "SPAM", "postId": post, "note": "my private words" }),
        )
        .await;
    assert_eq!(rep.status.as_u16(), 201, "{}", rep.text);
    let reports = w
        .bridge_read("/internal/v1/operator/channel-reports?status=OPEN")
        .await;
    assert!(reports.text.contains(&post), "{}", reports.text);
    assert!(!reports.text.contains("my private words"));
    assert!(!reports.text.contains(&follower.to_string()));
    // Takedown removes the post and actions its report.
    let v = w
        .bridge_read(&format!("/internal/v1/operator/channels/{id}"))
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let down = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/posts/{post}/takedown"),
            json!({ "tenantId": w.tenant(), "expectedVersion": v, "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(down.json["status"], "TAKEN_DOWN", "{}", down.text);
    let left = w
        .get(follower, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert!(!left.text.contains(&post), "{}", left.text);
    // Suspend: followers cannot read posts, the owner cannot post, discovery hides it.
    let v = w
        .bridge_read(&format!("/internal/v1/operator/channels/{id}"))
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let s = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/moderate"),
            json!({ "tenantId": w.tenant(), "expectedVersion": v, "reasonCode": "INVESTIGATION", "state": "SUSPENDED" }),
        )
        .await;
    assert_eq!(s.json["status"], "SUSPENDED", "{}", s.text);
    let r = w
        .get(follower, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let r = w
        .post(
            owner,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "still here?" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    let dir = w.get(w.person(), "/v1/messaging/channels?q=brand").await;
    assert!(!dir.text.contains(&id), "{}", dir.text);
    let history = w
        .get(owner, &format!("/v1/messaging/channels/{id}/moderation"))
        .await;
    assert_eq!(
        history.json["moderationState"], "SUSPENDED",
        "{}",
        history.text
    );
    assert!(history.text.contains("TAKEDOWN") && history.text.contains("VERIFY"));
    assert!(!history.text.contains("hq:staff"), "{}", history.text);
    let actions = w.bridge_read("/internal/v1/operator/channel-actions").await;
    assert!(actions.text.contains("SUSPEND"), "{}", actions.text);
    // Reinstate.
    let v = w
        .bridge_read(&format!("/internal/v1/operator/channels/{id}"))
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let back = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/moderate"),
            json!({ "tenantId": w.tenant(), "expectedVersion": v, "reasonCode": "CLEARED", "state": "ACTIVE" }),
        )
        .await;
    assert_eq!(back.json["status"], "ACTIVE", "{}", back.text);
    // A report on the channel itself, resolved by HQ.
    let rep2 = w
        .post(
            follower,
            &format!("/v1/messaging/channels/{id}/reports"),
            json!({ "category": "IMPERSONATION" }),
        )
        .await;
    let rid = rep2.json["reportId"].as_str().unwrap();
    let resolved = w
        .bridge(
            &format!("/internal/v1/operator/channel-reports/{rid}/resolve"),
            json!({ "tenantId": w.tenant(), "expectedVersion": 1, "reasonCode": "NOT_IMPERSONATION", "resolution": "DISMISSED" }),
        )
        .await;
    assert_eq!(resolved.json["status"], "DISMISSED", "{}", resolved.text);
    // Another tenant names nothing.
    let other = w
        .bridge(
            &format!("/internal/v1/operator/channels/{id}/moderate"),
            json!({ "tenantId": Uuid::new_v4(), "expectedVersion": 1, "reasonCode": "X_Y", "state": "BANNED" }),
        )
        .await;
    assert_eq!(other.status.as_u16(), 404, "{}", other.text);
}
