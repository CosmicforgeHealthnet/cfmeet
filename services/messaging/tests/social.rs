//! ADR 0069 — Status and Channels: the server-enforced status audience (mutual contacts,
//! snapshots, revocation on removal / block, no restoration on re-add / unblock), every read
//! path behind the same gate (feed, detail, rings, media, viewers, reactions, replies),
//! resumable media uploads, idempotent creation, group ordering, mutes, close friends,
//! sharing, deletion, 24-hour expiry and channels at parity.
//!
//! Communications (the source of truth for contacts and personal blocks) is a scripted Hub
//! answering `contacts/eligible` exactly as the real one does: mutual, unblocked contacts
//! only, with the time eligibility last (re)started.

mod common;

use std::{
    collections::HashMap,
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

type Pairs = HashMap<(Uuid, Uuid), DateTime<Utc>>;

/// The scripted Hub: symmetric mutual pairs with their `eligibleSince`.
#[derive(Clone, Default)]
struct Hub {
    pairs: Arc<Mutex<Pairs>>,
    down: Arc<Mutex<bool>>,
}

impl Hub {
    fn key(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
        if a < b {
            (a, b)
        } else {
            (b, a)
        }
    }
    fn connect(&self, a: Uuid, b: Uuid, since: DateTime<Utc>) {
        self.pairs.lock().unwrap().insert(Self::key(a, b), since);
    }
    fn disconnect(&self, a: Uuid, b: Uuid) {
        self.pairs.lock().unwrap().remove(&Self::key(a, b));
    }
}

/// The personal policy check (`policy/check`): allowed between mutual contacts, as the
/// real Hub allows a DIRECT chat and its messages (status replies arrive in one).
async fn policy_check(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    if *hub.down.lock().unwrap() {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let actor: Uuid = serde_json::from_value(body["actorPrincipalId"].clone()).unwrap();
    let target: Uuid = serde_json::from_value(body["targetPrincipalId"].clone()).unwrap();
    let allowed = actor == target
        || hub
            .pairs
            .lock()
            .unwrap()
            .contains_key(&Hub::key(actor, target));
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "schemaVersion": 1,
            "allowed": allowed,
            "reason": if allowed { "ALLOWED" } else { "NOT_PERMITTED" },
            "policyVersion": 1,
        })),
    )
}

async fn eligible(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    if *hub.down.lock().unwrap() {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let actor: Uuid = serde_json::from_value(body["actorPrincipalId"].clone()).unwrap();
    let among: Option<Vec<Uuid>> = body
        .get("principalIds")
        .map(|v| serde_json::from_value(v.clone()).unwrap());
    let contacts: Vec<Value> = hub
        .pairs
        .lock()
        .unwrap()
        .iter()
        .filter_map(|((a, b), since)| {
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
            Some(json!({ "principalId": other, "eligibleSince": since.to_rfc3339() }))
        })
        .collect();
    (
        axum::http::StatusCode::OK,
        Json(json!({ "schemaVersion": 1, "contacts": contacts })),
    )
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
        .route(
            "/internal/v1/communications/policy/check",
            post(policy_check),
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
        ServiceSigningKey::generate(KeyId::parse("social-test").unwrap()),
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
    async fn text_status(&self, who: Uuid, extra: Value) -> Uuid {
        let mut body = json!({ "kind": "TEXT", "text": "hello\nworld" });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = self.post(who, "/v1/messaging/status", body).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        Uuid::parse_str(r.json["statusId"].as_str().unwrap()).unwrap()
    }
    async fn sees(&self, who: Uuid, status: Uuid) -> bool {
        let r = self
            .get(who, &format!("/v1/messaging/status/{status}"))
            .await;
        match r.status.as_u16() {
            200 => true,
            404 => {
                assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
                false
            }
            other => panic!("unexpected {other}: {}", r.text),
        }
    }
    async fn feed_has(&self, who: Uuid, status: Uuid) -> bool {
        let r = self.get(who, "/v1/messaging/status/feed").await;
        assert_eq!(r.status.as_u16(), 200, "{}", r.text);
        r.text.contains(&status.to_string())
    }
    /// Uploads `bytes` as status media of `kind` through the signed part flow.
    async fn upload(
        &self,
        who: Uuid,
        kind: &str,
        content_type: &str,
        bytes: &[u8],
        duration: Option<i32>,
    ) -> Uuid {
        let mut body =
            json!({ "kind": kind, "contentType": content_type, "sizeBytes": bytes.len() });
        if let Some(d) = duration {
            body["durationMs"] = json!(d);
        }
        let r = self.post(who, "/v1/messaging/status/media", body).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        let id = r.json["mediaId"].as_str().unwrap().to_string();
        let part_bytes = r.json["partBytes"].as_u64().unwrap() as usize;
        for (i, chunk) in bytes.chunks(part_bytes).enumerate() {
            let g = self
                .post(
                    who,
                    &format!("/v1/messaging/status/media/{id}/parts/{i}"),
                    json!({ "sha256": sha256_hex(chunk) }),
                )
                .await;
            assert_eq!(g.status.as_u16(), 200, "{}", g.text);
            self.put_part(&g.json, chunk);
        }
        let done = self
            .post(
                who,
                &format!("/v1/messaging/status/media/{id}/complete"),
                json!({}),
            )
            .await;
        assert_eq!(done.status.as_u16(), 200, "{}", done.text);
        assert_eq!(done.json["state"], "COMPLETE");
        Uuid::parse_str(&id).unwrap()
    }
    fn put_part(&self, grant: &Value, bytes: &[u8]) {
        let u = &grant["upload"];
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
            .upload_signed(&write, self.h.now(), bytes.to_vec())
            .unwrap();
    }
}

fn png(size: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.resize(size, 7);
    v
}

#[tokio::test]
async fn only_mutual_unblocked_contacts_in_the_snapshot_see_a_status() {
    let Some(w) = world().await else {
        return;
    };
    let now = w.h.now();
    let (alice, bob, carol, dave, frank, gina, erin) = (
        w.person(),
        w.person(),
        w.person(),
        w.person(),
        w.person(),
        w.person(),
        w.person(),
    );
    let early = now - Duration::days(30);
    for p in [bob, frank, gina, erin] {
        w.hub.connect(alice, p, early);
    }
    // carol: a one-way request (never mutual) — the Hub does not list her. dave: nothing.
    let s = w
        .text_status(
            alice,
            json!({ "mentions": [bob, dave], "allowSharing": false }),
        )
        .await;
    assert!(w.sees(alice, s).await);
    assert!(w.sees(bob, s).await);
    assert!(w.feed_has(bob, s).await);
    for outsider in [carol, dave] {
        assert!(!w.sees(outsider, s).await, "outsider sees");
        assert!(!w.feed_has(outsider, s).await);
        let rings = w
            .post(
                outsider,
                "/v1/messaging/status/rings",
                json!({ "principalIds": [alice] }),
            )
            .await;
        assert_eq!(rings.json["rings"], json!([]), "{}", rings.text);
        // Direct reads of every per-status path are 404 too.
        for (m, p) in [
            (Method::POST, format!("/v1/messaging/status/{s}/view")),
            (Method::GET, format!("/v1/messaging/status/{s}/viewers")),
            (Method::PUT, format!("/v1/messaging/status/{s}/reaction")),
        ] {
            let body = if m == Method::GET {
                None
            } else if p.ends_with("/reaction") {
                Some(json!({ "emoji": "\u{1F44D}" }))
            } else {
                Some(json!({}))
            };
            let r = w.call(m.clone(), outsider, &p, body).await;
            assert_eq!(r.status.as_u16(), 404, "{m} {p}: {}", r.text);
        }
    }
    // Mentions never widen the audience; only the mentioned viewer sees mentionedMe.
    let as_bob = w.get(bob, &format!("/v1/messaging/status/{s}")).await;
    assert_eq!(as_bob.json["mentionedMe"], true);
    assert!(
        as_bob.json.get("mentions").is_none(),
        "viewers never see the mention list"
    );
    let as_alice = w.get(alice, &format!("/v1/messaging/status/{s}")).await;
    assert_eq!(as_alice.json["mentions"], json!([bob]));
    let rings = w
        .post(
            bob,
            "/v1/messaging/status/rings",
            json!({ "principalIds": [alice, dave] }),
        )
        .await;
    assert_eq!(rings.json["rings"][0]["principalId"], json!(alice));
    assert_eq!(rings.json["rings"][0]["unseenCount"], 1);
    assert_eq!(rings.json["rings"].as_array().unwrap().len(), 1);

    // Views: unique per person, owner-only viewer list.
    let v = w
        .post(
            bob,
            &format!("/v1/messaging/status/{s}/view"),
            json!({ "liked": true }),
        )
        .await;
    assert_eq!(v.status.as_u16(), 200, "{}", v.text);
    let viewers = w
        .get(alice, &format!("/v1/messaging/status/{s}/viewers"))
        .await;
    assert_eq!(
        viewers.json["viewers"][0]["principalId"],
        json!(bob),
        "{}",
        viewers.text
    );
    assert_eq!(viewers.json["viewers"][0]["liked"], true);
    assert_eq!(
        w.get(bob, &format!("/v1/messaging/status/{s}/viewers"))
            .await
            .status
            .as_u16(),
        404
    );
    // Another device (credential) of the same person sees the same viewed state.
    let other_device = w.h.credential(&w.c, ALL_SCOPES);
    let r =
        w.h.send(
            Method::GET,
            Some(&other_device.token),
            Some(bob),
            &format!("/v1/messaging/status/{s}"),
            None,
            &[],
        )
        .await;
    assert_eq!(
        (r.json["viewed"].clone(), r.json["liked"].clone()),
        (json!(true), json!(true))
    );

    // Sharing refused when not allowed.
    let r = w
        .post(
            bob,
            &format!("/v1/messaging/status/{s}/forwards"),
            json!({ "principalIds": [gina] }),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), r.reason()),
        (409, "SHARING_NOT_ALLOWED"),
        "{}",
        r.text
    );
    let r = w
        .post(
            bob,
            &format!("/v1/messaging/status/{s}/reshares"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);

    // CONTACTS_EXCEPT and ONLY.
    let except = w
        .text_status(
            alice,
            json!({ "audience": { "mode": "CONTACTS_EXCEPT", "principalIds": [frank] } }),
        )
        .await;
    assert!(w.sees(bob, except).await);
    assert!(!w.sees(frank, except).await);
    let only = w
        .text_status(
            alice,
            json!({ "audience": { "mode": "ONLY", "principalIds": [bob, dave] } }),
        )
        .await;
    assert!(w.sees(bob, only).await);
    assert!(!w.sees(gina, only).await, "ONLY-not-selected");
    assert!(
        !w.sees(dave, only).await,
        "ONLY lists cannot name non-contacts"
    );
    let owner_view = w
        .get(alice, &format!("/v1/messaging/status/{except}"))
        .await;
    assert_eq!(owner_view.json["audience"]["mode"], "CONTACTS_EXCEPT");
    assert_eq!(owner_view.json["audience"]["excludedCount"], 1);

    // Removing a contact revokes; re-adding never restores earlier statuses.
    w.hub.disconnect(alice, gina);
    assert!(!w.sees(gina, s).await, "removed contact");
    w.h.clock.advance(Duration::seconds(5));
    w.hub.connect(alice, gina, w.h.now());
    assert!(
        !w.sees(gina, s).await,
        "re-added contact must not regain access"
    );
    let later = w.text_status(alice, json!({})).await;
    assert!(
        w.sees(gina, later).await,
        "a status after re-adding is visible"
    );

    // A Hub block (either direction) revokes; unblocking never restores.
    w.hub.disconnect(alice, erin);
    assert!(!w.sees(erin, s).await, "blocked");
    w.h.clock.advance(Duration::seconds(5));
    w.hub.connect(alice, erin, w.h.now());
    assert!(!w.sees(erin, s).await, "unblocked must not regain access");

    // A Messaging block (bob blocks alice) removes both from each other's snapshots for good.
    let b = w
        .post(bob, "/v1/messaging/blocks", json!({ "principalId": alice }))
        .await;
    assert_eq!(b.status.as_u16(), 201, "{}", b.text);
    assert!(!w.sees(bob, s).await);
    let u = w
        .call(
            Method::DELETE,
            bob,
            &format!("/v1/messaging/blocks/{alice}"),
            None,
        )
        .await;
    assert!(u.status.is_success(), "{}", u.text);
    assert!(!w.sees(bob, s).await, "unblocking must not restore access");

    // A newly added contact never sees statuses published before.
    let newbie = w.person();
    w.hub.connect(alice, newbie, w.h.now());
    assert!(!w.sees(newbie, s).await);

    // Communications unreachable: reads fail closed, never wider.
    *w.hub.down.lock().unwrap() = true;
    let r = w.get(frank, &format!("/v1/messaging/status/{s}")).await;
    assert_eq!(r.status.as_u16(), 503, "{}", r.text);
    *w.hub.down.lock().unwrap() = false;
}

#[tokio::test]
async fn media_reads_follow_the_status_gate_and_uploads_resume() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, dave) = (w.person(), w.person(), w.person());
    w.hub.connect(alice, bob, w.h.now() - Duration::days(1));
    let bytes = png(5 * 1024 * 1024);
    // Resumable: the server lists stored parts; a part is idempotent per digest.
    let r = w
        .post(
            alice,
            "/v1/messaging/status/media",
            json!({ "kind": "IMAGE", "contentType": "image/png", "sizeBytes": bytes.len() }),
        )
        .await;
    assert_eq!(r.json["partCount"], 2, "{}", r.text);
    let id = r.json["mediaId"].as_str().unwrap().to_string();
    let part_bytes = r.json["partBytes"].as_u64().unwrap() as usize;
    let chunks: Vec<&[u8]> = bytes.chunks(part_bytes).collect();
    let g1 = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{id}/parts/1"),
            json!({ "sha256": sha256_hex(chunks[1]) }),
        )
        .await;
    w.put_part(&g1.json, chunks[1]);
    let state = w
        .get(alice, &format!("/v1/messaging/status/media/{id}"))
        .await;
    assert_eq!(state.json["storedParts"], json!([1]), "{}", state.text);
    let again = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{id}/parts/1"),
            json!({ "sha256": sha256_hex(chunks[1]) }),
        )
        .await;
    assert_eq!(
        (again.json["stored"].clone(), again.json["upload"].clone()),
        (json!(true), Value::Null)
    );
    let other = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{id}/parts/1"),
            json!({ "sha256": sha256_hex(b"x") }),
        )
        .await;
    assert_eq!(other.status.as_u16(), 409, "{}", other.text);
    // Incomplete media cannot be posted, by anyone.
    let early = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "IMAGE", "mediaId": id }),
        )
        .await;
    assert_eq!(early.status.as_u16(), 409, "{}", early.text);
    let not_mine = w
        .post(
            bob,
            "/v1/messaging/status",
            json!({ "kind": "IMAGE", "mediaId": id }),
        )
        .await;
    assert_eq!(not_mine.status.as_u16(), 404, "{}", not_mine.text);
    let g0 = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{id}/parts/0"),
            json!({ "sha256": sha256_hex(chunks[0]) }),
        )
        .await;
    w.put_part(&g0.json, chunks[0]);
    let done = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{id}/complete"),
            json!({}),
        )
        .await;
    assert_eq!(done.json["complete"], true, "{}", done.text);
    let s = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "IMAGE", "mediaId": id, "caption": "line 1\nline 2" }),
        )
        .await;
    assert_eq!(s.status.as_u16(), 201, "{}", s.text);
    // Readers in the audience get short-lived part URLs; others 404 on every media path.
    let grants = w
        .post(
            bob,
            &format!("/v1/messaging/status/media/{id}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(grants.status.as_u16(), 200, "{}", grants.text);
    assert_eq!(grants.json["parts"].as_array().unwrap().len(), 2);
    for (m, p) in [
        (
            Method::POST,
            format!("/v1/messaging/status/media/{id}/download-grants"),
        ),
        (Method::GET, format!("/v1/messaging/status/media/{id}")),
        (Method::DELETE, format!("/v1/messaging/status/media/{id}")),
    ] {
        let r = w.call(m.clone(), dave, &p, Some(json!({}))).await;
        assert_eq!(r.status.as_u16(), 404, "{m} {p}: {}", r.text);
    }
    // In use: cannot be cancelled. Deleting the status removes the media and its bytes.
    let r = w
        .call(
            Method::DELETE,
            alice,
            &format!("/v1/messaging/status/media/{id}"),
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    let sid = s.json["statusId"].as_str().unwrap().to_string();
    let d = w
        .call(
            Method::DELETE,
            alice,
            &format!("/v1/messaging/status/{sid}"),
            None,
        )
        .await;
    assert_eq!(d.status.as_u16(), 200, "{}", d.text);
    assert!(w.store.is_empty(), "media bytes deleted with the status");
    assert_eq!(
        w.get(bob, &format!("/v1/messaging/status/{sid}"))
            .await
            .status
            .as_u16(),
        404
    );

    // Cancelling an unfinished upload removes it; a video needs its duration and ≤ 60 s shown.
    let r = w
        .post(
            alice,
            "/v1/messaging/status/media",
            json!({ "kind": "VIDEO", "contentType": "video/mp4", "sizeBytes": 10 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    let mut mp4 = b"\0\0\0\x18ftypmp42".to_vec();
    mp4.resize(4096, 0);
    let video = w
        .upload(alice, "VIDEO", "video/mp4", &mp4, Some(150_000))
        .await;
    let r = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "VIDEO", "mediaId": video }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    let r = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "VIDEO", "mediaId": video, "startMs": 0, "endMs": 61_000 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    let r = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "VIDEO", "mediaId": video, "startMs": 60_000, "endMs": 120_000 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let up = w.post(alice, "/v1/messaging/status/media", json!({ "kind": "AUDIO", "contentType": "audio/ogg", "sizeBytes": 100, "durationMs": 1000 })).await;
    let up_id = up.json["mediaId"].as_str().unwrap().to_string();
    let r = w
        .call(
            Method::DELETE,
            alice,
            &format!("/v1/messaging/status/media/{up_id}"),
            None,
        )
        .await;
    assert_eq!(r.json["deleted"], true, "{}", r.text);
    assert_eq!(
        w.get(alice, &format!("/v1/messaging/status/media/{up_id}"))
            .await
            .status
            .as_u16(),
        404
    );
    // A file that is not what it claims is refused and deleted at completion.
    let fake = w
        .post(
            alice,
            "/v1/messaging/status/media",
            json!({ "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 16 }),
        )
        .await;
    let fid = fake.json["mediaId"].as_str().unwrap().to_string();
    let g = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{fid}/parts/0"),
            json!({ "sha256": sha256_hex(b"<svg>not-an-imag") }),
        )
        .await;
    w.put_part(&g.json, b"<svg>not-an-imag");
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/status/media/{fid}/complete"),
            json!({}),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), r.code()),
        (422, "CONTENT_REJECTED"),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn creation_is_idempotent_groups_keep_order_and_statuses_expire() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob) = (w.person(), w.person());
    w.hub.connect(alice, bob, w.h.now() - Duration::days(1));
    let body = json!({ "kind": "TEXT", "text": "once", "textColor": "#ffffff", "textAlign": "center", "textSize": 2,
        "backgroundGradient": { "from": "#112233", "to": "#445566", "angle": 45 }, "font": 3 });
    let send = |key: &'static str, body: Value| {
        let w = &w;
        async move {
            w.h.send(
                Method::POST,
                Some(&w.c.token),
                Some(alice),
                "/v1/messaging/status",
                Some(body),
                &[("idempotency-key", key)],
            )
            .await
        }
    };
    let first = send("status-key-0001", body.clone()).await;
    assert_eq!(first.status.as_u16(), 201, "{}", first.text);
    assert_eq!(first.json["backgroundGradient"]["angle"], 45);
    w.h.clock.advance(Duration::hours(3));
    let again = send("status-key-0001", body.clone()).await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.text);
    assert_eq!(again.json["statusId"], first.json["statusId"]);
    let reused = send(
        "status-key-0001",
        json!({ "kind": "TEXT", "text": "different" }),
    )
    .await;
    assert_eq!(reused.code(), "IDEMPOTENCY_KEY_REUSED", "{}", reused.text);
    // Raw CSS and bad styles are refused.
    let bad = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "TEXT", "text": "x", "background": "red; url(x)" }),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 400);
    let bad = w
        .post(
            alice,
            "/v1/messaging/status",
            json!({ "kind": "TEXT", "text": "x", "css": "x" }),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 400);

    // A group arriving out of order shows in sequence order.
    let g = Uuid::new_v4();
    let s1 = w
        .text_status(alice, json!({ "groupId": g, "sequence": 1 }))
        .await;
    w.h.clock.advance(Duration::seconds(3));
    let s0 = w
        .text_status(alice, json!({ "groupId": g, "sequence": 0 }))
        .await;
    let feed = w.get(bob, "/v1/messaging/status/feed").await;
    let posts: Vec<String> = feed.json["contacts"][0]["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["statusId"].as_str().unwrap().to_string())
        .collect();
    let i0 = posts.iter().position(|p| *p == s0.to_string()).unwrap();
    let i1 = posts.iter().position(|p| *p == s1.to_string()).unwrap();
    assert!(i0 < i1, "{posts:?}");

    // Mute moves the author to the muted list (server-side, every device).
    let m = w
        .call(
            Method::PUT,
            bob,
            &format!("/v1/messaging/status/mutes/{alice}"),
            None,
        )
        .await;
    assert_eq!(m.json["muted"], true, "{}", m.text);
    let feed = w.get(bob, "/v1/messaging/status/feed").await;
    assert_eq!(feed.json["contacts"], json!([]));
    assert_eq!(feed.json["muted"][0]["authorPrincipalId"], json!(alice));

    // My statuses, bulk delete, and the 24-hour expiry.
    let mine = w.get(alice, "/v1/messaging/status/mine").await;
    assert!(
        mine.json["posts"].as_array().unwrap().len() >= 3,
        "{}",
        mine.text
    );
    let d = w
        .post(
            alice,
            "/v1/messaging/status/delete",
            json!({ "statusIds": [s0, s1] }),
        )
        .await;
    assert_eq!(d.json["deleted"], 2, "{}", d.text);
    assert!(!w.sees(bob, s0).await);
    w.h.clock.advance(Duration::hours(25));
    let id = Uuid::parse_str(first.json["statusId"].as_str().unwrap()).unwrap();
    assert!(!w.sees(bob, id).await);
    assert!(
        !w.sees(alice, id).await,
        "expired statuses are gone for the author too"
    );
    messaging::social::sweep(&w.h.state).await.unwrap();
    assert_eq!(
        w.h.scalar(&format!(
            "SELECT count(*) FROM messaging_statuses WHERE id = '{id}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn close_friends_lists_preferences_and_sharing() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol, dave) = (w.person(), w.person(), w.person(), w.person());
    for p in [bob, carol, dave] {
        w.hub.connect(alice, p, w.h.now() - Duration::days(1));
    }
    w.hub.connect(bob, carol, w.h.now() - Duration::days(1));
    let stranger = w.person();
    let r = w
        .call(
            Method::PUT,
            alice,
            "/v1/messaging/status/close-friends",
            Some(json!({ "memberPrincipalIds": [bob, stranger] })),
        )
        .await;
    assert_eq!(r.json["memberPrincipalIds"], json!([bob]), "{}", r.text);
    let cf = w
        .text_status(alice, json!({ "audience": { "mode": "CLOSE_FRIENDS" } }))
        .await;
    assert_eq!(
        w.get(bob, &format!("/v1/messaging/status/{cf}")).await.json["closeFriends"],
        true
    );
    assert!(!w.sees(carol, cf).await);
    // Later list edits never expose earlier statuses.
    w.call(
        Method::PUT,
        alice,
        "/v1/messaging/status/close-friends",
        Some(json!({ "memberPrincipalIds": [bob, carol] })),
    )
    .await;
    assert!(!w.sees(carol, cf).await);

    // A named list, and the saved default (future statuses only).
    let l = w
        .post(
            alice,
            "/v1/messaging/status/audience-lists",
            json!({ "name": "Family", "memberPrincipalIds": [dave] }),
        )
        .await;
    assert_eq!(l.status.as_u16(), 201, "{}", l.text);
    let list_id = l.json["listId"].as_str().unwrap().to_string();
    let p = w
        .call(
            Method::PUT,
            alice,
            "/v1/messaging/status/audience",
            Some(json!({ "mode": "CUSTOM", "listId": list_id, "allowSharing": true })),
        )
        .await;
    assert_eq!(p.json["mode"], "CUSTOM", "{}", p.text);
    let fam = w.text_status(alice, json!({})).await;
    assert!(w.sees(dave, fam).await);
    assert!(!w.sees(bob, fam).await);
    let owner = w.get(alice, &format!("/v1/messaging/status/{fam}")).await;
    assert_eq!(owner.json["audience"]["label"], "Family");

    // Sharing: a forward reaches only recipients inside the original audience.
    let open = w
        .text_status(
            alice,
            json!({ "audience": { "mode": "ONLY", "principalIds": [bob] }, "allowSharing": true }),
        )
        .await;
    let f = w
        .post(
            bob,
            &format!("/v1/messaging/status/{open}/forwards"),
            json!({ "principalIds": [carol] }),
        )
        .await;
    assert_eq!(f.status.as_u16(), 200, "{}", f.text);
    assert_eq!(
        (f.json["forwarded"].clone(), f.json["skipped"].clone()),
        (json!(0), json!(1))
    );
    // A reshare is never shown outside the original audience.
    let rs = w
        .post(
            bob,
            &format!("/v1/messaging/status/{open}/reshares"),
            json!({}),
        )
        .await;
    assert_eq!(rs.status.as_u16(), 201, "{}", rs.text);
    let reshared = Uuid::parse_str(rs.json["statusId"].as_str().unwrap()).unwrap();
    assert_eq!(rs.json["origin"]["authorPrincipalId"], json!(alice));
    assert!(
        !w.sees(carol, reshared).await,
        "carol is outside the original audience"
    );
    // Deleting the original removes the reshare too.
    w.call(
        Method::DELETE,
        alice,
        &format!("/v1/messaging/status/{open}"),
        None,
    )
    .await;
    assert!(!w.sees(bob, reshared).await);

    // "Allow sharing" per status: only its author changes it; forwards and reshares then refuse.
    let later = w
        .text_status(
            alice,
            json!({ "audience": { "mode": "ONLY", "principalIds": [bob] }, "allowSharing": true }),
        )
        .await;
    let path = format!("/v1/messaging/status/{later}");
    let other = w
        .call(
            Method::PATCH,
            bob,
            &path,
            Some(json!({ "allowSharing": false })),
        )
        .await;
    assert_eq!(other.status.as_u16(), 404, "{}", other.text);
    let off = w
        .call(
            Method::PATCH,
            alice,
            &path,
            Some(json!({ "allowSharing": false })),
        )
        .await;
    assert_eq!(off.status.as_u16(), 200, "{}", off.text);
    assert_eq!(off.json["allowSharing"], false);
    assert_eq!(w.get(bob, &path).await.json["allowSharing"], false);
    let refused = w
        .post(
            bob,
            &format!("/v1/messaging/status/{later}/reshares"),
            json!({}),
        )
        .await;
    assert_eq!(refused.status.as_u16(), 409, "{}", refused.text);
    let on = w
        .call(
            Method::PATCH,
            alice,
            &path,
            Some(json!({ "allowSharing": true })),
        )
        .await;
    assert_eq!(on.json["allowSharing"], true, "{}", on.text);
}

#[tokio::test]
async fn channels_keep_parity() {
    let Some(w) = world().await else {
        return;
    };
    let (owner, reader) = (w.person(), w.person());
    let c = w
        .post(
            owner,
            "/v1/messaging/channels",
            json!({ "name": "News", "description": "Daily" }),
        )
        .await;
    assert_eq!(c.status.as_u16(), 201, "{}", c.text);
    let id = c.json["channelId"].as_str().unwrap().to_string();
    let dir = w.get(reader, "/v1/messaging/channels?q=new").await;
    assert_eq!(
        dir.json["channels"][0]["channelId"],
        json!(id),
        "{}",
        dir.text
    );
    let f = w
        .call(
            Method::PUT,
            reader,
            &format!("/v1/messaging/channels/{id}/follow"),
            None,
        )
        .await;
    assert_eq!(f.json["following"], true, "{}", f.text);
    // Posts after following count as unread (the harness clock does not move by itself).
    w.h.clock.advance(Duration::seconds(1));
    let denied = w
        .post(
            reader,
            &format!("/v1/messaging/channels/{id}/posts"),
            json!({ "text": "hi" }),
        )
        .await;
    assert_eq!(denied.status.as_u16(), 403, "{}", denied.text);
    let p =
        w.h.send(
            Method::POST,
            Some(&w.c.token),
            Some(owner),
            &format!("/v1/messaging/channels/{id}/posts"),
            Some(json!({ "text": "first" })),
            &[("idempotency-key", "channel-post-1")],
        )
        .await;
    assert_eq!(p.status.as_u16(), 201, "{}", p.text);
    let again =
        w.h.send(
            Method::POST,
            Some(&w.c.token),
            Some(owner),
            &format!("/v1/messaging/channels/{id}/posts"),
            Some(json!({ "text": "first" })),
            &[("idempotency-key", "channel-post-1")],
        )
        .await;
    assert_eq!(
        (again.status.as_u16(), again.json["postId"].clone()),
        (200, p.json["postId"].clone())
    );
    let following = w.get(reader, "/v1/messaging/channels/following").await;
    assert_eq!(
        following.json["channels"][0]["unreadCount"], 1,
        "{}",
        following.text
    );
    let post_id = p.json["postId"].as_str().unwrap().to_string();
    let r = w
        .call(
            Method::PUT,
            reader,
            &format!("/v1/messaging/channels/{id}/posts/{post_id}/reaction"),
            Some(json!({ "emoji": "\u{1F44D}" })),
        )
        .await;
    assert_eq!(r.json["reactions"][0]["count"], 1, "{}", r.text);
    let posts = w
        .get(reader, &format!("/v1/messaging/channels/{id}/posts"))
        .await;
    assert_eq!(posts.json["posts"].as_array().unwrap().len(), 1);
    let following = w.get(reader, "/v1/messaging/channels/following").await;
    assert_eq!(following.json["channels"][0]["unreadCount"], 0);
    let d = w
        .call(
            Method::DELETE,
            reader,
            &format!("/v1/messaging/channels/{id}"),
            None,
        )
        .await;
    assert_eq!(d.status.as_u16(), 403);
    let d = w
        .call(
            Method::DELETE,
            owner,
            &format!("/v1/messaging/channels/{id}"),
            None,
        )
        .await;
    assert_eq!(d.json["deleted"], true, "{}", d.text);
    assert_eq!(
        w.get(reader, &format!("/v1/messaging/channels/{id}"))
            .await
            .status
            .as_u16(),
        404
    );
}

/// ADR 0069 rollout: the retired Meeting Service channels arrive through the one-time import
/// (`messaging-admin social import-channels`), checked like uploads, with ids kept; a second
/// run changes nothing; a changed document or another environment is refused untouched.
#[tokio::test]
async fn legacy_channels_import_once_and_idempotently() {
    use base64::Engine as _;
    use messaging::social::legacy_import::{checksum, import, Export, ImportError};
    let Some(w) = world().await else {
        return;
    };
    let (owner, reader) = (w.person(), w.person());
    let scope = &w.c.context.scope;
    let (channel, text_post, media_post, bad_post, media, bad_media) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let image = png(5 * 1024 * 1024); // two 4 MiB parts
    let at = w.h.state.now() - Duration::days(3);
    let mut doc = json!({
        "schemaVersion": 1,
        "source": "cfmeet.meeting-service.social-channels",
        "exportedAt": w.h.state.now(),
        "scope": { "tenantId": scope.tenant_id, "applicationId": scope.application_id, "environmentId": scope.environment_id },
        "channels": [{ "id": channel, "ownerPrincipalId": owner, "name": "News", "description": "Daily", "createdAt": at, "updatedAt": at }],
        "followers": [
            { "channelId": channel, "principalId": owner, "followedAt": at, "lastReadAt": at },
            { "channelId": channel, "principalId": reader, "followedAt": at, "lastReadAt": at }
        ],
        "posts": [
            { "id": text_post, "channelId": channel, "authorPrincipalId": owner, "kind": "TEXT", "text": "hello", "mediaId": null, "createdAt": at },
            { "id": media_post, "channelId": channel, "authorPrincipalId": owner, "kind": "IMAGE", "text": null, "mediaId": media, "createdAt": at + Duration::seconds(1) },
            { "id": bad_post, "channelId": channel, "authorPrincipalId": owner, "kind": "IMAGE", "text": null, "mediaId": bad_media, "createdAt": at + Duration::seconds(2) }
        ],
        "reactions": [{ "postId": text_post, "principalId": reader, "emoji": "\u{1F44D}", "reactedAt": at }],
        "media": [
            { "id": media, "ownerPrincipalId": owner, "kind": "IMAGE", "contentType": "image/png", "sizeBytes": image.len(),
              "durationMs": null, "createdAt": at, "sha256": sha256_hex(&image),
              "data": base64::engine::general_purpose::STANDARD.encode(&image) },
            { "id": bad_media, "ownerPrincipalId": owner, "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 4,
              "durationMs": null, "createdAt": at, "sha256": sha256_hex(b"text"),
              "data": base64::engine::general_purpose::STANDARD.encode(b"text") }
        ],
        "checksum": ""
    });
    let parsed: Export = serde_json::from_value(doc.clone()).unwrap();
    doc["checksum"] = json!(checksum(&parsed));
    let parsed: Export = serde_json::from_value(doc.clone()).unwrap();
    let env = scope.environment_id;
    let now = w.h.state.now();

    // A changed document or another environment is refused and writes nothing.
    let mut tampered = parsed.clone();
    tampered.channels[0].name = "Other".into();
    assert!(matches!(
        import(&w.h.state.db, w.store.as_ref(), env, &tampered, now).await,
        Err(ImportError::Refused(_))
    ));
    assert!(matches!(
        import(
            &w.h.state.db,
            w.store.as_ref(),
            Uuid::new_v4(),
            &parsed,
            now
        )
        .await,
        Err(ImportError::Refused(_))
    ));
    assert_eq!(
        w.get(reader, "/v1/messaging/channels/following").await.json["channels"],
        json!([])
    );

    let first = import(&w.h.state.db, w.store.as_ref(), env, &parsed, now)
        .await
        .unwrap();
    assert_eq!(
        (
            first.imported.channels,
            first.imported.followers,
            first.imported.posts,
            first.imported.reactions,
            first.imported.media
        ),
        (1, 2, 2, 1, 1),
        "{first:?}"
    );
    assert_eq!(
        first.refused.len(),
        2,
        "unrecognised media and its media-only post: {first:?}"
    );

    // The follower sees the channel, its posts (ids kept) and can read the media.
    let following = w.get(reader, "/v1/messaging/channels/following").await;
    assert_eq!(
        following.json["channels"][0]["channelId"],
        json!(channel),
        "{}",
        following.text
    );
    assert_eq!(following.json["channels"][0]["name"], "News");
    let posts = w
        .get(reader, &format!("/v1/messaging/channels/{channel}/posts"))
        .await;
    let ids: Vec<Value> = posts.json["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["postId"].clone())
        .collect();
    assert!(
        ids.contains(&json!(text_post)) && ids.contains(&json!(media_post)),
        "{}",
        posts.text
    );
    assert!(!ids.contains(&json!(bad_post)));
    let g = w
        .post(
            reader,
            &format!("/v1/messaging/channels/media/{media}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(g.status.as_u16(), 200, "{}", g.text);
    assert_eq!(g.json["parts"].as_array().unwrap().len(), 2);
    let mut stored = Vec::new();
    for part in 0..2 {
        stored.extend(
            cf_object_storage::ObjectStore::get(
                w.store.as_ref(),
                &messaging::social::media::object_key(env, media, part),
                8 * 1024 * 1024,
            )
            .await
            .unwrap()
            .unwrap(),
        );
    }
    assert_eq!(stored, image, "media stored whole in 4 MiB parts");

    // A second run changes nothing.
    let again = import(&w.h.state.db, w.store.as_ref(), env, &parsed, now)
        .await
        .unwrap();
    assert_eq!(again.imported, Default::default(), "{again:?}");
    assert_eq!(
        (
            again.unchanged.channels,
            again.unchanged.followers,
            again.unchanged.posts,
            again.unchanged.reactions,
            again.unchanged.media
        ),
        (1, 2, 2, 1, 1)
    );
}

/// ADR 0074: a private reply is recorded for the status's author ONLY. The author lists the
/// replies of a live status with the chat each arrived in (and what the replier wrote,
/// read through the author's own membership); the replier, anyone else and any status that
/// is not the caller's get `404`. A deleted reply message stops being listed, the record
/// goes with its status (deletion and expiry), and "My status" counts the replies. The
/// reaction time and the "mentioned you" ring flag come along.
#[tokio::test]
async fn status_replies_are_listed_only_for_the_author() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol) = (w.person(), w.person(), w.person());
    let early = w.h.now() - Duration::days(3);
    for p in [bob, carol] {
        w.hub.connect(alice, p, early);
    }
    w.hub.connect(bob, carol, early);
    let s = w.text_status(alice, json!({ "mentions": [bob] })).await;
    let path = format!("/v1/messaging/status/{s}/replies");

    let r = w
        .post(
            bob,
            &path,
            json!({ "text": "Lovely view", "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let conversation = r.json["conversationId"].as_str().unwrap().to_string();
    let message = r.json["messageId"].as_str().unwrap().to_string();
    // The author replying to their own status is refused (and records nothing).
    let own = w
        .post(
            alice,
            &path,
            json!({ "text": "me", "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(own.status.as_u16(), 409, "{}", own.text);

    let listed = w.get(alice, &path).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text);
    assert_eq!(listed.json["statusId"], json!(s));
    let replies = listed.json["replies"].as_array().unwrap();
    assert_eq!(replies.len(), 1, "{}", listed.text);
    assert_eq!(replies[0]["principalId"], json!(bob));
    assert_eq!(replies[0]["conversationId"], json!(conversation));
    assert_eq!(replies[0]["messageId"], json!(message));
    assert_eq!(replies[0]["text"], "Lovely view", "only what bob wrote");

    // Nobody but the author reads them: not the replier, not another viewer, not a stranger.
    let stranger = w.person();
    for who in [bob, carol, stranger] {
        let r = w.get(who, &path).await;
        assert_eq!(r.status.as_u16(), 404, "{}", r.text);
        assert_eq!(r.code(), "NOT_FOUND");
        assert!(!r.text.contains("Lovely view"));
    }
    // An unknown status is 404 for the author too.
    let r = w
        .get(
            alice,
            &format!("/v1/messaging/status/{}/replies", Uuid::new_v4()),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);

    // "My status" counts them; the viewer of a status never sees the count.
    let mine = w.get(alice, "/v1/messaging/status/mine").await;
    assert_eq!(mine.json["posts"][0]["replyCount"], 1, "{}", mine.text);
    let as_bob = w.get(bob, &format!("/v1/messaging/status/{s}")).await;
    assert!(as_bob.json.get("replyCount").is_none(), "{}", as_bob.text);

    // A reaction carries when it was set (the chat list orders it against messages).
    let react = w
        .call(
            Method::PUT,
            carol,
            &format!("/v1/messaging/status/{s}/reaction"),
            Some(json!({ "emoji": "\u{1F60D}" })),
        )
        .await;
    assert_eq!(react.status.as_u16(), 200, "{}", react.text);
    let viewers = w
        .get(alice, &format!("/v1/messaging/status/{s}/viewers"))
        .await;
    let carol_view = viewers.json["viewers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["principalId"] == json!(carol))
        .cloned()
        .unwrap_or_else(|| panic!("carol listed: {}", viewers.text));
    assert_eq!(carol_view["reaction"], "\u{1F60D}");
    assert!(carol_view["reactedAt"].is_string(), "{}", viewers.text);

    // The ring tells bob (mentioned) that the status mentions him, and nobody else.
    let rings = w
        .post(
            bob,
            "/v1/messaging/status/rings",
            json!({ "principalIds": [alice] }),
        )
        .await;
    assert_eq!(
        rings.json["rings"][0]["mentionedMe"], true,
        "{}",
        rings.text
    );
    let rings = w
        .post(
            carol,
            "/v1/messaging/status/rings",
            json!({ "principalIds": [alice] }),
        )
        .await;
    assert_eq!(
        rings.json["rings"][0]["mentionedMe"], false,
        "{}",
        rings.text
    );

    // Bob deletes his reply message for everyone: it is no longer listed.
    let del = w
        .call(
            Method::DELETE,
            bob,
            &format!("/v1/conversations/{conversation}/messages/{message}?scope=EVERYONE"),
            None,
        )
        .await;
    assert!(
        del.status.is_success(),
        "delete reply: {} {}",
        del.status,
        del.text
    );
    let after = w.get(alice, &path).await;
    assert_eq!(after.json["replies"], json!([]), "{}", after.text);

    // The record goes with its status: deletion ...
    let second = w.text_status(alice, json!({})).await;
    let r = w
        .post(
            carol,
            &format!("/v1/messaging/status/{second}/replies"),
            json!({ "text": "Nice", "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let count = |id: Uuid| {
        format!("SELECT count(*) FROM messaging_status_replies WHERE status_id = '{id}'")
    };
    assert_eq!(w.h.scalar(&count(second)).await, 1);
    w.call(
        Method::DELETE,
        alice,
        &format!("/v1/messaging/status/{second}"),
        None,
    )
    .await;
    assert_eq!(w.h.scalar(&count(second)).await, 0);
    // ... and the 24-hour expiry (the author's list ends with the status, then the sweep).
    assert!(w.h.scalar(&count(s)).await >= 1);
    w.h.clock.advance(Duration::hours(25));
    let gone = w.get(alice, &path).await;
    assert_eq!(gone.status.as_u16(), 404, "{}", gone.text);
    messaging::social::sweep(&w.h.state).await.unwrap();
    assert_eq!(w.h.scalar(&count(s)).await, 0);
}
