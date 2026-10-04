//! Phase 2 Part F — mandatory E2E scenarios F-E2E-01…25 (mock LiveKit provider over
//! the production `LiveKitProvider`).

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{collab::*, livekit_mock, rtc::*, *};
use futures_util::StreamExt;
use livekit_api::access_token::TokenVerifier;
use livekit_protocol as proto;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::collab().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn claims(cred: &str) -> livekit_api::access_token::Claims {
    TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
        .verify(cred)
        .unwrap()
}

fn can(app: &TestApp, room_name: &str, identity: &str, source: proto::TrackSource) -> bool {
    lk(app).publish(room_name, identity, source).is_ok()
}

/// F-E2E-01 — host mute reaches the provider; event + audit recorded.
#[tokio::test]
async fn f01_host_mute() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .unwrap();
    let (s, v) = app
        .post(
            &format!("{}/participants/{}/mute", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mutedTracks"], 1);
    assert!(lk(&app)
        .tracks_of(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .iter()
        .all(|(_, m)| *m));
    assert_eq!(events(&app, cid, "participant.muted").await, 1);
    assert_eq!(audits(&app, cid, "MEDIA_MUTE").await, 1);
    // MUTE NOW is not a lock: the participant may unmute / republish.
    assert!(can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Microphone
    ));
}

/// F-E2E-02 — audio lock survives credential refresh and reconnect; unlock restores.
#[tokio::test]
async fn f02_audio_lock() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .unwrap();
    let url = format!("{}/participants/{}/audio-lock", base(cid), p.participant_id);
    let (s, v) = app.post(&url, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (
            v["audioLocked"].as_bool(),
            v["permissions"]["audioPublish"].as_bool()
        ),
        (Some(true), Some(false))
    );
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    assert!(
        lk(&app)
            .tracks_of(&room(cid), &p.identity, proto::TrackSource::Microphone)
            .is_empty(),
        "existing audio stopped"
    );
    assert!(!can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Microphone
    ));
    // Refresh + reconnect: still forbidden.
    let (s, r) = refresh(&app, cid, p, p.auth()).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let c = claims(r["rtc"]["credential"].as_str().unwrap());
    assert!(!c
        .video
        .can_publish_sources
        .iter()
        .any(|x| x == "microphone"));
    assert!(
        c.video.can_publish_sources.iter().any(|x| x == "camera"),
        "other media unaffected"
    );
    lk(&app).disconnect(&room(cid), &p.identity);
    lk(&app)
        .connect(r["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
    assert!(!can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Microphone
    ));
    // Idempotent.
    let (_, again) = app.post(&url, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(again["changed"], false);
    assert_eq!(events(&app, cid, "participant.audio_locked").await, 1);
    // Unlock restores.
    let (s, v) = app.delete(&url, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["permissions"]["audioPublish"], true);
    assert!(can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Microphone
    ));
    assert_eq!(events(&app, cid, "participant.audio_unlocked").await, 1);
    assert_eq!(audits(&app, cid, "MEDIA_AUDIO_LOCK").await, 1);
}

/// F-E2E-03 — video lock.
#[tokio::test]
async fn f03_video_lock() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Camera)
        .unwrap();
    let url = format!("{}/participants/{}/video-lock", base(cid), p.participant_id);
    let (s, v) = app.post(&url, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(lk(&app)
        .tracks_of(&room(cid), &p.identity, proto::TrackSource::Camera)
        .is_empty());
    assert!(!can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Camera
    ));
    assert!(can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Microphone
    ));
    let (_, r) = refresh(&app, cid, p, p.auth()).await;
    assert!(!claims(r["rtc"]["credential"].as_str().unwrap())
        .video
        .can_publish_sources
        .iter()
        .any(|x| x == "camera"));
    app.delete(&url, Auth::Bearer(&owner.token)).await;
    assert!(can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::Camera
    ));
}

/// F-E2E-04 — screen-share policy tiers enforced by provider grants.
#[tokio::test]
async fn f04_screen_share_policy() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let host = owner_joined(&app, cid, &owner).await;
    policy(
        &app,
        cid,
        &owner,
        json!({ "screenSharePolicy": "HOST_ONLY" }),
    )
    .await;
    assert!(
        !can(
            &app,
            &room(cid),
            &g[0].identity,
            proto::TrackSource::ScreenShare
        ),
        "participant denied"
    );
    assert!(
        can(
            &app,
            &room(cid),
            &host.identity,
            proto::TrackSource::ScreenShare
        ),
        "host allowed"
    );
    let (s, _) = change_role(&app, cid, &owner, g[0].participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !can(
            &app,
            &room(cid),
            &g[0].identity,
            proto::TrackSource::ScreenShare
        ),
        "presenter still denied under HOST_ONLY"
    );
    policy(
        &app,
        cid,
        &owner,
        json!({ "screenSharePolicy": "PRESENTERS_ONLY" }),
    )
    .await;
    assert!(
        can(
            &app,
            &room(cid),
            &g[0].identity,
            proto::TrackSource::ScreenShare
        ),
        "presenter allowed"
    );
    assert!(
        !can(
            &app,
            &room(cid),
            &g[1].identity,
            proto::TrackSource::ScreenShare
        ),
        "participant denied"
    );
}

/// F-E2E-05 — host stops an active share; provider confirms; canonical event once.
#[tokio::test]
async fn f05_stop_screen_share() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    let sid = start_share(&app, cid, p).await;
    assert_eq!(events(&app, cid, "screenshare.started").await, 1);
    let (_, st) = app
        .get(
            &format!("{}/collaboration", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(st["activeScreenShares"], json!([p.participant_id]));
    let (s, v) = app
        .post(
            &format!("{}/screen-share/{}/stop", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["stoppedSessions"], 1);
    assert!(
        lk(&app)
            .tracks_of(&room(cid), &p.identity, proto::TrackSource::ScreenShare)
            .is_empty(),
        "provider track ended"
    );
    assert_eq!(events(&app, cid, "screenshare.ended").await, 1);
    // Late provider evidence for the same track is a no-op.
    track_event(
        &app,
        "track_unpublished",
        &room(cid),
        &p.identity,
        &sid,
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(events(&app, cid, "screenshare.ended").await, 1);
    // STOP is not a lock: sharing is still permitted.
    assert!(can(
        &app,
        &room(cid),
        &p.identity,
        proto::TrackSource::ScreenShare
    ));
    let (s, v) = app
        .post(
            &format!("{}/screen-share/{}/stop", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "SCREEN_SHARE_NOT_ACTIVE")
    );
    assert_eq!(audits(&app, cid, "SCREEN_SHARE_STOP").await, 1);
}

/// F-E2E-06 — viewer collaborates per policy but never publishes.
#[tokio::test]
async fn f06_viewer() {
    let app = app!();
    let (owner, cid, g) = meeting(
        &app,
        json!({ "title": "v", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        1,
    )
    .await;
    let v = &g[0];
    for src in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        assert!(!can(&app, &room(cid), &v.identity, src), "{src:?}");
    }
    assert_eq!(
        send(&app, cid, v.auth(), json!({ "body": "hi" })).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        app.post(
            &format!("{}/reactions", base(cid)),
            v.auth(),
            json!({ "reaction": "clap" })
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        app.post(&format!("{}/hand-raise", base(cid)), v.auth(), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.post(
            &format!("{}/questions", base(cid)),
            v.auth(),
            json!({ "body": "Q?" })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert_eq!(
        vote(&app, cid, poll, v.auth(), "o1").await.0,
        StatusCode::CREATED
    );
}

/// F-E2E-07 — spotlight is shared state; personal pins are never stored.
#[tokio::test]
async fn f07_spotlight() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (s, v) = app
        .post(
            &format!("{}/spotlights", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for viewer in [&g[0], &g[1]] {
        let (_, st) = app
            .get(&format!("{}/collaboration", base(cid)), viewer.auth())
            .await;
        assert_eq!(
            st["spotlights"][0]["participantId"],
            json!(g[0].participant_id)
        );
    }
    assert_eq!(events(&app, cid, "participant.spotlighted").await, 1);
    // Single spotlight: a new one replaces the old.
    app.post(
        &format!("{}/spotlights", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "participantId": g[1].participant_id }),
    )
    .await;
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), g[0].auth())
        .await;
    assert_eq!(st["spotlights"].as_array().unwrap().len(), 1);
    assert_eq!(events(&app, cid, "participant.spotlight_removed").await, 1);
    // Spotlight never changes media rights.
    assert!(can(
        &app,
        &room(cid),
        &g[0].identity,
        proto::TrackSource::Microphone
    ));
    assert_eq!(app.scalar("SELECT count(*) FROM information_schema.columns WHERE table_name='conference_participants' AND column_name LIKE '%pin%'").await, 0, "no pin state");
    let (s, _) = app
        .delete(
            &format!("{}/spotlights/{}", base(cid), g[1].participant_id),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

/// F-E2E-08 — meeting chat: server-derived sender, database pagination.
#[tokio::test]
async fn f08_chat() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 2).await;
    for i in 0..5 {
        let who = &g[i % 2];
        let (s, v) = send(&app, cid, who.auth(), json!({ "body": format!("m{i}") })).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(uuid(&v["senderParticipantId"]), who.participant_id);
        assert_eq!(v["senderDisplayName"], format!("guest-{}", i % 2));
    }
    let mut seen = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("?limit=2&before={c}"),
            None => "?limit=2".into(),
        };
        let (s, v) = messages(&app, cid, g[1].auth(), &q).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        for m in v["messages"].as_array().unwrap() {
            seen.push(m["body"].as_str().unwrap().to_string());
        }
        match v["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(
        seen,
        ["m4", "m3", "m2", "m1", "m0"],
        "newest first, no duplicates, no gaps"
    );
}

/// F-E2E-09 — private chat reaches only the recipient.
#[tokio::test]
async fn f09_private_chat() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 3).await;
    let (s, m) = send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "secret", "recipientParticipantId": g[1].participant_id }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    let (_, b) = messages(
        &app,
        cid,
        g[1].auth(),
        &format!("?scope=PRIVATE&peerId={}", g[0].participant_id),
    )
    .await;
    assert_eq!(b["messages"][0]["body"], "secret");
    let (_, c) = messages(
        &app,
        cid,
        g[2].auth(),
        &format!("?scope=PRIVATE&peerId={}", g[0].participant_id),
    )
    .await;
    assert!(
        c["messages"].as_array().unwrap().is_empty(),
        "C sees nothing"
    );
    let (_, main) = messages(&app, cid, g[2].auth(), "").await;
    assert!(main["messages"].as_array().unwrap().is_empty());
    let audience = app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='chat.message_created' AND audience->>'kind'='PARTICIPANTS' AND audience->'moderatorPermission' IS NULL")).await;
    assert_eq!(audience, 1);
}

/// F-E2E-10 — host disables chat for one participant.
#[tokio::test]
async fn f10_chat_moderation() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (s, _) = app
        .post(
            &format!(
                "{}/participants/{}/chat-restriction",
                base(cid),
                g[0].participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = send(&app, cid, g[0].auth(), json!({ "body": "x" })).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "CHAT_PARTICIPANT_RESTRICTED")
    );
    assert_eq!(
        send(&app, cid, g[1].auth(), json!({ "body": "fine" }))
            .await
            .0,
        StatusCode::CREATED
    );
    // Moderator deletes a message.
    let (_, m) = send(&app, cid, g[1].auth(), json!({ "body": "bad" })).await;
    let (s, d) = app
        .delete(
            &format!("{}/messages/{}", base(cid), m["id"].as_str().unwrap()),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!((s, d["changed"].as_bool()), (StatusCode::OK, Some(true)));
    let (_, page) = messages(&app, cid, g[1].auth(), "").await;
    assert_eq!(page["messages"][0]["deleted"], true);
    assert!(page["messages"][0]["body"].is_null());
    assert_eq!(audits(&app, cid, "CHAT_PARTICIPANT_RESTRICT").await, 1);
    assert_eq!(audits(&app, cid, "CHAT_MESSAGE_DELETE").await, 1);
}

async fn ws(
    app: &TestApp,
    addr: std::net::SocketAddr,
    cid: Uuid,
    auth: Auth<'_>,
    cursor: Option<i64>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (s, t) = app
        .post(&format!("{}/realtime/ticket", base(cid)), auth, json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let mut url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    if let Some(c) = cursor {
        url.push_str(&format!("&cursor={c}"));
    }
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

async fn frames_until<S>(ws: &mut S, pred: impl Fn(&Value) -> bool) -> Vec<Value>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut out = vec![];
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            let done = pred(&v);
            out.push(v);
            if done {
                return out;
            }
        }
    }
}

/// F-E2E-11 — reactions are transient realtime events; nothing is stored.
#[tokio::test]
async fn f11_reaction() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let addr = app.serve().await;
    let mut s = ws(&app, addr, cid, Auth::Bearer(&owner.token), None).await;
    frames_until(&mut s, |f| f["type"] == "subscribed").await;
    let rows = app.scalar("SELECT count(*) FROM outbox_events").await;
    let (st, v) = app
        .post(
            &format!("{}/reactions", base(cid)),
            g[0].auth(),
            json!({ "reaction": "celebrate" }),
        )
        .await;
    assert_eq!(
        (st, v["delivered"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{v}"
    );
    let frames = frames_until(&mut s, |f| f["event"]["eventType"] == "reaction.sent").await;
    let e = &frames.last().unwrap()["event"];
    assert_eq!(e["payload"]["reaction"], "celebrate");
    assert_eq!(e["payload"]["participantId"], json!(g[0].participant_id));
    assert!(e["sequence"].is_null(), "transient: no sequence");
    assert!(
        !e.to_string().contains(&g[0].identity),
        "no provider identity"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events").await,
        rows,
        "no outbox row"
    );
    let (st, v) = app
        .post(
            &format!("{}/reactions", base(cid)),
            g[0].auth(),
            json!({ "reaction": "dynamite" }),
        )
        .await;
    assert_eq!(
        (st, code(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
}

/// F-E2E-12 — hand raise: ordering, persistence across reconnect, host lowers.
#[tokio::test]
async fn f12_hand_raise() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    for p in &g {
        let (s, v) = app
            .post(&format!("{}/hand-raise", base(cid)), p.auth(), json!({}))
            .await;
        assert_eq!(
            (s, v["raised"].as_bool()),
            (StatusCode::OK, Some(true)),
            "{v}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (_, q) = app
        .get(
            &format!("{}/hand-raise", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let order: Vec<Uuid> = q["hands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| uuid(&h["participantId"]))
        .collect();
    assert_eq!(
        order,
        g.iter().map(|p| p.participant_id).collect::<Vec<_>>(),
        "oldest first"
    );
    // Reconnect (new device session) keeps the raised hand.
    let (s, v) = app
        .join(
            cid,
            g[1].auth(),
            json!({ "previousSessionId": g[1].session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), g[1].auth())
        .await;
    assert!(st["me"]["handRaisedAt"].is_string());
    let (s, v) = app
        .delete(
            &format!(
                "{}/participants/{}/hand-raise",
                base(cid),
                g[1].participant_id
            ),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!((s, v["changed"].as_bool()), (StatusCode::OK, Some(true)));
    let (_, q) = app
        .get(
            &format!("{}/hand-raise", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(q["hands"].as_array().unwrap().len(), 2);
    assert_eq!(events(&app, cid, "hand.raised").await, 3);
    assert_eq!(events(&app, cid, "hand.lowered").await, 1);
}

/// F-E2E-13 — create rooms + balanced automatic assignment.
#[tokio::test]
async fn f13_breakout_create_and_balance() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 7).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 3 })).await;
    let named = create_rooms(&app, cid, &owner, json!({ "names": ["Design"] })).await;
    let (s, v) = app
        .post(
            &format!("{}/breakouts/assign", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "BALANCED", "breakoutIds": rooms }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["assigned"].as_array().unwrap().len(), 7);
    let (_, list) = app
        .get(
            &format!("{}/breakouts", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let mut counts: Vec<i64> = list["breakouts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["id"] != json!(named[0]))
        .map(|b| b["participantCount"].as_i64().unwrap())
        .collect();
    counts.sort();
    assert_eq!(counts, vec![2, 2, 3]);
    let names: Vec<&str> = list["breakouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Room 1", "Room 2", "Room 3", "Design"]);
    // Participants never see other rooms' member lists.
    let (_, mine) = app
        .get(&format!("{}/breakouts", base(cid)), g[0].auth())
        .await;
    assert!(mine["breakouts"][0].get("participantIds").is_none());
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        7,
        "no new participants"
    );
}

async fn into_rooms(
    app: &TestApp,
    cid: Uuid,
    owner: &User,
    a: &Joined,
    b: &Joined,
) -> (Uuid, Uuid) {
    let rooms = create_rooms(app, cid, owner, json!({ "names": ["A", "B"] })).await;
    app.post(
        &format!("{}/breakouts/assign", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "mode": "MANUAL", "assignments": [
        { "participantId": a.participant_id, "breakoutId": rooms[0] },
        { "participantId": b.participant_id, "breakoutId": rooms[1] },
    ] }),
    )
    .await;
    open_room(app, cid, owner, rooms[0]).await;
    open_room(app, cid, owner, rooms[1]).await;
    (rooms[0], rooms[1])
}

/// F-E2E-14 — breakout isolation (media + chat).
#[tokio::test]
async fn f14_breakout_isolation() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, rb) = into_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    assert!(lk(&app).has_room(&breakout_room(ra)) && lk(&app).has_room(&breakout_room(rb)));
    assert!(
        lk(&app).participant(&room(cid), &g[0].identity).is_none(),
        "rerouted out of main"
    );
    let (_, ca) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(
        claims(ca["rtc"]["credential"].as_str().unwrap()).video.room,
        breakout_room(ra)
    );
    assert_eq!(ca["mediaContext"]["breakoutId"], json!(ra));
    lk(&app)
        .connect(ca["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &breakout_room(ra),
        None,
        Some(&g[0].identity),
    );
    assert_eq!(app.signed_webhook(&body).await.1["outcome"], "APPLIED");
    // A's identity showing up in room B is evicted.
    lk(&app).insert_participant(
        &breakout_room(rb),
        &g[0].identity,
        proto::ParticipantPermission {
            can_subscribe: true,
            ..Default::default()
        },
    );
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &breakout_room(rb),
        None,
        Some(&g[0].identity),
    );
    assert_eq!(
        app.signed_webhook(&body).await.1["outcome"],
        "STALE_SESSION_EVICTED"
    );
    assert!(lk(&app)
        .participant(&breakout_room(rb), &g[0].identity)
        .is_none());
    // Chat isolation.
    assert_eq!(
        send(
            &app,
            cid,
            g[0].auth(),
            json!({ "body": "in A", "breakoutId": ra })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (s, _) = messages(
        &app,
        cid,
        g[1].auth(),
        &format!("?scope=BREAKOUT&breakoutId={ra}"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = send(
        &app,
        cid,
        g[1].auth(),
        json!({ "body": "sneak", "breakoutId": ra }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
}

/// F-E2E-15 — move A → B changes assignment, provider room and chat scope.
#[tokio::test]
async fn f15_move_participant() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, rb) = into_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    let (_, ca) = refresh(&app, cid, &g[0], g[0].auth()).await;
    lk(&app)
        .connect(ca["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    let (s, v) = app
        .post(
            &format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rb }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mediaContext"]["breakoutId"], json!(rb));
    assert!(
        lk(&app)
            .participant(&breakout_room(ra), &g[0].identity)
            .is_none(),
        "removed from old room"
    );
    let (_, cb) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(
        claims(cb["rtc"]["credential"].as_str().unwrap()).video.room,
        breakout_room(rb)
    );
    assert_eq!(
        messages(
            &app,
            cid,
            g[0].auth(),
            &format!("?scope=BREAKOUT&breakoutId={rb}")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        messages(
            &app,
            cid,
            g[0].auth(),
            &format!("?scope=BREAKOUT&breakoutId={ra}")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM breakout_assignments WHERE participant_id='{}' AND unassigned_at IS NULL AND breakout_room_id='{rb}'", g[0].participant_id)).await, 1);
    assert_eq!(
        events(&app, cid, "breakout.participant_moved").await,
        3,
        "2 on open + 1 move"
    );
    // Back to main.
    let (_, v) = app
        .post(
            &format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": null }),
        )
        .await;
    assert_eq!(v["mediaContext"]["kind"], "MAIN");
}

/// F-E2E-16 — host announcement reaches every open breakout.
#[tokio::test]
async fn f16_breakout_announcement() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    into_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    let (s, v) = app
        .post(
            &format!("{}/breakouts/announcement", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "message": "5 minutes left" }),
        )
        .await;
    assert_eq!((s, v["rooms"].as_i64()), (StatusCode::OK, Some(2)), "{v}");
    for p in &g {
        let (_, r) = app
            .get(&format!("{}/events?after=0", base(cid)), p.auth())
            .await;
        assert!(
            r["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["eventType"] == "breakout.announcement"
                    && e["payload"]["message"] == "5 minutes left"),
            "{r}"
        );
    }
}

/// F-E2E-17 — close with countdown, survives worker restart, everyone back to main.
#[tokio::test]
async fn f17_breakout_close_countdown() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, rb) = into_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    let (s, v) = app
        .post(
            &format!("{}/breakouts/close-all", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "countdownSeconds": 30 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["breakouts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|b| b["status"] == "CLOSING" && b["closingAt"].is_string()));
    // "Restart": a fresh worker instance; the persisted deadline is all it needs.
    let worker = backend::workers::BreakoutCloser::new(app.state.meeting.core.clone());
    assert_eq!(worker.tick().await.unwrap(), 0, "not due yet");
    exec(&app, &format!("UPDATE breakout_rooms SET closing_at = now() - interval '1 second' WHERE conference_id='{cid}'")).await;
    let worker = backend::workers::BreakoutCloser::new(app.state.meeting.core.clone());
    assert_eq!(worker.tick().await.unwrap(), 2);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM breakout_rooms WHERE conference_id='{cid}' AND status='CLOSED'"
        ))
        .await,
        2
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM breakout_assignments WHERE conference_id='{cid}' AND unassigned_at IS NULL")).await, 0);
    assert!(
        !lk(&app).has_room(&breakout_room(ra)) && !lk(&app).has_room(&breakout_room(rb)),
        "provider rooms terminated"
    );
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), g[0].auth())
        .await;
    assert_eq!(st["me"]["mediaContext"]["kind"], "MAIN");
    let (_, cred) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(
        claims(cred["rtc"]["credential"].as_str().unwrap())
            .video
            .room,
        room(cid)
    );
    assert_eq!(
        send(
            &app,
            cid,
            g[0].auth(),
            json!({ "body": "x", "breakoutId": ra })
        )
        .await
        .1["error"]["code"],
        "BREAKOUT_CLOSED"
    );
    assert_eq!(events(&app, cid, "breakout.closed").await, 2);
    assert_eq!(worker.tick().await.unwrap(), 0, "idempotent");
}

/// F-E2E-18 — poll lifecycle with AFTER_CLOSE visibility.
#[tokio::test]
async fn f18_poll() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_CLOSE")).await;
    assert_eq!(
        vote(&app, cid, poll, g[0].auth(), "o1").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        vote(&app, cid, poll, g[1].auth(), "o2").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        vote(&app, cid, poll, g[2].auth(), "o1").await.0,
        StatusCode::CREATED
    );
    let (s, v) = app
        .get(&format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "POLL_RESULTS_NOT_VISIBLE")
    );
    let (_, host) = app
        .get(
            &format!("{}/polls/{poll}/results", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        host["questions"][0]["optionCounts"],
        json!({ "o1": 2, "o2": 1 })
    );
    let (s, _) = app
        .post(
            &format!("{}/polls/{poll}/close", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .get(&format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["responseCount"], 3);
    assert_eq!(
        vote(&app, cid, poll, g[0].auth(), "o1").await.1["error"]["code"],
        "POLL_NOT_OPEN"
    );
    let (s, v) = app
        .post(
            &format!("{}/polls/{poll}/open", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "POLL_NOT_OPEN"),
        "never reopened"
    );
    assert_eq!(events(&app, cid, "poll.started").await, 1);
    assert_eq!(events(&app, cid, "poll.closed").await, 1);
}

fn quiz() -> Value {
    json!({
        "title": "Quiz",
        "quizMode": true,
        "resultsVisibility": "AFTER_CLOSE",
        "questions": [
            { "type": "SINGLE_CHOICE", "prompt": "2+2?", "options": ["3", "4"], "correct": [1], "points": 2 },
            { "type": "MULTIPLE_CHOICE", "prompt": "Primes?", "options": ["2", "4", "5"], "correct": [0, 2] },
        ],
    })
}

/// F-E2E-19 — quiz scored server-side; answers hidden until results state.
#[tokio::test]
async fn f19_quiz() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let poll = open_poll(&app, cid, &owner, quiz()).await;
    let (_, view) = app
        .get(&format!("{}/polls/{poll}", base(cid)), g[0].auth())
        .await;
    assert!(
        !view.to_string().contains("\"correct\""),
        "answers hidden: {view}"
    );
    let (_, list) = app.get(&format!("{}/polls", base(cid)), g[0].auth()).await;
    assert!(!list.to_string().contains("\"correct\""));
    let (s, r) = app.post(&format!("{}/polls/{poll}/responses", base(cid)), g[0].auth(), json!({ "answers": [
        { "questionId": "q1", "optionIds": ["o2"] }, { "questionId": "q2", "optionIds": ["o1", "o3"] } ] })).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert!(r["score"].is_null(), "no score leak before close");
    // A client-supplied score is rejected outright (unknown field)…
    let (s, forged) = app.post(&format!("{}/polls/{poll}/responses", base(cid)), g[1].auth(), json!({ "answers": [
        { "questionId": "q1", "optionIds": ["o1"] }, { "questionId": "q2", "optionIds": ["o1"] } ], "score": 99 })).await;
    assert_eq!(
        (s, code(&forged)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    let wrong = json!({ "answers": [
        { "questionId": "q1", "optionIds": ["o1"] }, { "questionId": "q2", "optionIds": ["o1"] } ] });
    assert_eq!(
        app.post(
            &format!("{}/polls/{poll}/responses", base(cid)),
            g[1].auth(),
            wrong.clone()
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (_, dup) = app
        .post(
            &format!("{}/polls/{poll}/responses", base(cid)),
            g[1].auth(),
            wrong,
        )
        .await;
    assert_eq!(code(&dup), "POLL_ALREADY_RESPONDED");
    app.post(
        &format!("{}/polls/{poll}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (_, res) = app
        .get(&format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(res["myScore"], 3.0, "{res}");
    assert_eq!(res["maxScore"], 3.0);
    assert_eq!(res["averageScore"], 1.5, "server-side scores: 3 and 0");
    assert_eq!(res["questions"][0]["correct"], json!(["o2"]));
}

/// F-E2E-20 — anonymous poll: aggregate only, voters unidentifiable.
#[tokio::test]
async fn f20_anonymous_poll() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let mut body = simple_poll("AFTER_CLOSE");
    body["anonymous"] = json!(true);
    let poll = open_poll(&app, cid, &owner, body).await;
    for (p, o) in g.iter().zip(["o1", "o2", "o1"]) {
        assert_eq!(
            vote(&app, cid, poll, p.auth(), o).await.0,
            StatusCode::CREATED
        );
    }
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_poll_responses WHERE poll_id='{poll}' AND participant_id IS NOT NULL")).await, 0);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(DISTINCT created_at) FROM meeting_poll_responses WHERE poll_id='{poll}'"
        ))
        .await,
        1,
        "no per-answer timing"
    );
    let (s, v) = app
        .get(
            &format!("{}/polls/{poll}/results", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "POLL_RESULTS_NOT_VISIBLE"),
        "no live diffing"
    );
    app.post(
        &format!("{}/polls/{poll}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (_, res) = app
        .get(
            &format!("{}/polls/{poll}/results", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        res["questions"][0]["optionCounts"],
        json!({ "o1": 2, "o2": 1 })
    );
    let text = res.to_string();
    for p in &g {
        assert!(!text.contains(&p.participant_id.to_string()));
    }
    // The host's realtime stream never attributes a poll event to a voter.
    let (_, evs) = app
        .get(
            &format!("{}/events?after=0", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let poll_events: Vec<&Value> = evs["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["eventType"].as_str().unwrap().starts_with("poll."))
        .collect();
    assert!(!poll_events.is_empty());
    for e in poll_events {
        for p in &g {
            assert!(
                !e.to_string().contains(&p.participant_id.to_string()),
                "{e}"
            );
        }
    }
    let poll_events = event_payloads(&app, cid, "poll.results_updated").await;
    for e in poll_events {
        for p in &g {
            assert!(
                !e.to_string().contains(&p.participant_id.to_string()),
                "{e}"
            );
        }
    }
}

/// F-E2E-21 — Q&A submit, upvote, pin, answer.
#[tokio::test]
async fn f21_qna() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let (s, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "When is lunch?" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{q}");
    let qid = uuid(&q["id"]);
    for p in &g[1..] {
        let (s, v) = app
            .post(
                &format!("{}/questions/{qid}/vote", base(cid)),
                p.auth(),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (_, again) = app
        .post(
            &format!("{}/questions/{qid}/vote", base(cid)),
            g[1].auth(),
            json!({}),
        )
        .await;
    assert_eq!(
        (
            again["changed"].as_bool(),
            again["question"]["upvoteCount"].as_i64()
        ),
        (Some(false), Some(2))
    );
    assert_eq!(
        app.post(
            &format!("{}/questions/{qid}/pin", base(cid)),
            Auth::Bearer(&owner.token),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (s, a) = app
        .post(
            &format!("{}/questions/{qid}/answer", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "answer": "At noon" }),
        )
        .await;
    assert_eq!(
        (s, a["question"]["status"].as_str()),
        (StatusCode::OK, Some("ANSWERED"))
    );
    let (_, list) = app
        .get(&format!("{}/questions", base(cid)), g[2].auth())
        .await;
    assert_eq!(list["questions"][0]["pinned"], true);
    assert_eq!(list["questions"][0]["answer"], "At noon");
    assert_eq!(list["questions"][0]["votedByMe"], true);
    for (t, n) in [
        ("qna.question_created", 1),
        ("qna.question_upvoted", 2),
        ("qna.question_pinned", 1),
        ("qna.question_answered", 1),
    ] {
        assert_eq!(events(&app, cid, t).await, n, "{t}");
    }
    assert_eq!(audits(&app, cid, "QNA_ANSWER").await, 1);
}

/// F-E2E-22 — anonymous question is not attributable.
#[tokio::test]
async fn f22_anonymous_qna() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Is this safe?", "anonymous": true }),
        )
        .await;
    let me = g[0].participant_id.to_string();
    let (_, host) = app
        .get(
            &format!("{}/questions", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let (_, peer) = app
        .get(&format!("{}/questions", base(cid)), g[1].auth())
        .await;
    for v in [&host, &peer] {
        assert!(
            !v.to_string().contains(&me) && !v.to_string().contains("guest-0"),
            "{v}"
        );
    }
    assert!(host["questions"][0]["participantId"].is_null());
    let (_, own) = app
        .get(&format!("{}/questions", base(cid)), g[0].auth())
        .await;
    assert_eq!(own["questions"][0]["isMine"], true);
    for e in event_payloads(&app, cid, "qna.question_created").await {
        assert!(!e.to_string().contains(&me));
    }
    let qid = uuid(&q["id"]);
    app.post(
        &format!("{}/questions/{qid}/answer", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    for e in event_payloads(&app, cid, "qna.question_answered").await {
        assert!(!e.to_string().contains(&me));
    }
}

/// F-E2E-23 — webinar attendee: collaboration yes, publication no.
#[tokio::test]
async fn f23_webinar_attendee() {
    let app = app!();
    let (owner, cid, g) = meeting(
        &app,
        json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        1,
    )
    .await;
    let a = &g[0];
    assert_eq!(a.body["participant"]["role"], "VIEWER");
    let c = claims(&a.credential);
    assert!(
        !c.video.can_publish && c.video.can_publish_sources.is_empty() && c.video.can_subscribe
    );
    for src in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        assert!(!can(&app, &room(cid), &a.identity, src));
    }
    assert_eq!(
        app.post(
            &format!("{}/questions", base(cid)),
            a.auth(),
            json!({ "body": "Q" })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        app.post(
            &format!("{}/reactions", base(cid)),
            a.auth(),
            json!({ "reaction": "heart" })
        )
        .await
        .0,
        StatusCode::OK
    );
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert_eq!(
        vote(&app, cid, poll, a.auth(), "o2").await.0,
        StatusCode::CREATED
    );
    let (_, pre) = app
        .get(&format!("{}/pre-join", base(cid)), Auth::None)
        .await;
    assert_eq!(pre["media"]["role"], "VIEWER");
    assert_eq!(pre["media"]["screenSharePolicy"], "PRESENTERS_ONLY");
    let (s, v) = send(&app, cid, a.auth(), json!({ "body": "dm", "recipientParticipantId": owner_joined(&app, cid, &owner).await.participant_id })).await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "attendee → host private allowed: {v}"
    );
}

/// F-E2E-24 — cross-conference attacks fail before state or provider mutation.
#[tokio::test]
async fn f24_cross_conference_attack() {
    let app = app!();
    let (_, conf_a, _) = meeting(&app, public(), 0).await;
    let host_a = app.user("host-a").await;
    let (_, conf_a) = {
        let _ = conf_a;
        app.live_conference(&host_a, public()).await
    };
    let (owner_b, conf_b, gb) = meeting(&app, public(), 1).await;
    let victim = &gb[0];
    lk(&app)
        .publish(
            &room(conf_b),
            &victim.identity,
            proto::TrackSource::Microphone,
        )
        .unwrap();
    let bid = create_rooms(&app, conf_b, &owner_b, json!({ "count": 1 })).await[0];
    let poll = open_poll(&app, conf_b, &owner_b, simple_poll("AFTER_VOTE")).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(conf_b)),
            victim.auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let qid = uuid(&q["id"]);
    let (_, m) = send(&app, conf_b, victim.auth(), json!({ "body": "hello" })).await;
    let mid = m["id"].as_str().unwrap().to_string();
    let before_calls = lk(&app).calls.lock().unwrap().len();
    let before_rows = app.scalar("SELECT count(*) FROM outbox_events").await;
    let a = base(conf_a);
    let pid = victim.participant_id;
    let attempts: Vec<(&str, String, Value)> = vec![
        (
            "POST",
            format!("{a}/participants/{pid}/mute"),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            "POST",
            format!("{a}/participants/{pid}/audio-lock"),
            json!({}),
        ),
        (
            "POST",
            format!("{a}/participants/{pid}/video-lock"),
            json!({}),
        ),
        ("POST", format!("{a}/screen-share/{pid}/stop"), json!({})),
        (
            "POST",
            format!("{a}/spotlights"),
            json!({ "participantId": pid }),
        ),
        (
            "POST",
            format!("{a}/participants/{pid}/chat-restriction"),
            json!({}),
        ),
        ("DELETE", format!("{a}/messages/{mid}"), json!(null)),
        (
            "DELETE",
            format!("{a}/participants/{pid}/hand-raise"),
            json!(null),
        ),
        ("POST", format!("{a}/breakouts/{bid}/open"), json!({})),
        (
            "POST",
            format!("{a}/breakouts/move"),
            json!({ "participantId": pid, "toBreakoutId": bid }),
        ),
        ("POST", format!("{a}/polls/{poll}/close"), json!({})),
        ("GET", format!("{a}/polls/{poll}/results"), json!(null)),
        ("POST", format!("{a}/questions/{qid}/dismiss"), json!({})),
        ("POST", format!("{a}/questions/{qid}/pin"), json!({})),
    ];
    for (method, uri, body) in attempts {
        let (s, v) = match method {
            "POST" => app.post(&uri, Auth::Bearer(&host_a.token), body).await,
            "DELETE" => app.delete(&uri, Auth::Bearer(&host_a.token)).await,
            _ => app.get(&uri, Auth::Bearer(&host_a.token)).await,
        };
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {uri}: {v}");
    }
    // Same targets on B by A's host: not authorized.
    let (s, _) = app
        .post(
            &format!("{}/participants/{pid}/audio-lock", base(conf_b)),
            Auth::Bearer(&host_a.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        lk(&app).calls.lock().unwrap().len(),
        before_calls,
        "no provider call"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events").await,
        before_rows,
        "no state change"
    );
    assert!(lk(&app)
        .tracks_of(
            &room(conf_b),
            &victim.identity,
            proto::TrackSource::Microphone
        )
        .iter()
        .all(|(_, m)| !m));
}

/// F-E2E-25 — disconnect during changes; replay + canonical refetch converge.
#[tokio::test]
async fn f25_realtime_recovery() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let addr = app.serve().await;
    let mut s = ws(&app, addr, cid, g[1].auth(), None).await;
    let hello = frames_until(&mut s, |f| f["type"] == "subscribed").await;
    let cursor = hello[0]["cursor"].as_i64().unwrap();
    let _ = s.close(None).await;
    drop(s);
    // Changes while disconnected.
    app.post(
        &format!(
            "{}/participants/{}/audio-lock",
            base(cid),
            g[0].participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    app.post(&format!("{}/hand-raise", base(cid)), g[1].auth(), json!({}))
        .await;
    app.post(
        &format!("{}/spotlights", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "participantId": g[0].participant_id }),
    )
    .await;
    let bid = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
    app.post(&format!("{}/breakouts/assign", base(cid)), Auth::Bearer(&owner.token), json!({ "mode": "MANUAL", "assignments": [{ "participantId": g[1].participant_id, "breakoutId": bid }] })).await;
    open_room(&app, cid, &owner, bid).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let mut s = ws(&app, addr, cid, g[1].auth(), Some(cursor)).await;
    let frames = frames_until(&mut s, |f| {
        f["event"]["eventType"] == "qna.question_created"
    })
    .await;
    let types: Vec<String> = frames
        .iter()
        .filter_map(|f| f["event"]["eventType"].as_str().map(String::from))
        .collect();
    for t in [
        "participant.audio_locked",
        "hand.raised",
        "participant.spotlighted",
        "breakout.opened",
        "breakout.participant_moved",
        "poll.started",
        "qna.question_created",
    ] {
        assert!(types.iter().any(|x| x == t), "{t} replayed: {types:?}");
    }
    assert!(
        !types
            .iter()
            .any(|x| x == "breakout.assignment_changed" || x == "poll.created"),
        "manager-only events filtered: {types:?}"
    );
    let seqs: Vec<i64> = frames
        .iter()
        .filter_map(|f| f["event"]["sequence"].as_i64())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "ordered");
    // Canonical refetch converges to the same state.
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), g[1].auth())
        .await;
    assert!(st["me"]["handRaisedAt"].is_string());
    assert_eq!(st["me"]["mediaContext"]["breakoutId"], json!(bid));
    assert_eq!(
        st["spotlights"][0]["participantId"],
        json!(g[0].participant_id)
    );
    let (_, polls) = app.get(&format!("{}/polls", base(cid)), g[1].auth()).await;
    assert_eq!(polls["polls"][0]["id"], json!(poll));
    let (_, qs) = app
        .get(&format!("{}/questions", base(cid)), g[1].auth())
        .await;
    assert_eq!(qs["questions"][0]["id"], q["id"]);
    let (_, st0) = app
        .get(&format!("{}/collaboration", base(cid)), g[0].auth())
        .await;
    assert_eq!(st0["me"]["audioLocked"], true);
}
