//! Phase 2 Part E — mandatory E2E scenarios E1–E15 against the real `LiveKitProvider`
//! talking to the in-process mock LiveKit RoomService (mocked provider certification).

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{livekit_mock, rtc::*, *};
use futures_util::StreamExt;
use livekit_protocol as proto;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::livekit().await {
            Some(a) => a,
            None => return,
        }
    };
}

async fn created_conference(app: &TestApp, owner: &User, body: Value) -> (Uuid, Uuid) {
    let space = app.create_space(owner, body).await;
    let space_id = uuid(&space["id"]);
    let (s, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space_id}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    (space_id, uuid(&c["id"]))
}

/// E1 — PREPARE reaches WAITING only after the provider confirmed the room.
#[tokio::test]
async fn e1_room_prepare() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = created_conference(
        &app,
        &owner,
        json!({ "title": "E1", "accessPolicy": "PUBLIC" }),
    )
    .await;
    assert!(!lk(&app).has_room(&room(cid)), "no room before prepare");
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/prepare"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conference"]["status"], "WAITING", "{v}");
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    assert!(lk(&app).has_room(&room(cid)), "real provider room created");
    assert_eq!(events(&app, cid, "conference.preparing").await, 1);
    assert_eq!(events(&app, cid, "conference.ready").await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND rtc_room_ready_at IS NOT NULL AND rtc_room_id IS NOT NULL")).await, 1);
    // Idempotent: a second prepare is a no-op and never a second room.
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/prepare"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["changed"], false);
    assert_eq!(lk(&app).room_names().len(), 1);
    // The prepare command was executed exactly once and recorded.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_prepare_requested' AND status='PUBLISHED'")).await, 1);
    // Start goes LIVE on the confirmed room.
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conference"]["status"], "LIVE");
}

/// E2 — Admitted join: credential only after commit, bound to the room and a
/// server-generated identity, bounded TTL; admitted ≠ joined until the provider says so.
#[tokio::test]
async fn e2_admitted_join() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E2", "accessPolicy": "PUBLIC" }))
        .await;
    let j = join_guest(&app, cid, "Guest").await;
    assert_eq!(j.body["participant"]["admissionState"], "ADMITTED");
    let claims = livekit_api::access_token::TokenVerifier::with_api_key(
        livekit_mock::API_KEY,
        livekit_mock::API_SECRET,
    )
    .verify(&j.credential)
    .expect("credential signed with the provider secret");
    assert_eq!(
        claims.sub, j.identity,
        "server-generated opaque media identity"
    );
    assert_eq!(claims.video.room, room(cid));
    assert!(!claims.video.room_admin && !claims.video.room_create && !claims.video.room_record);
    let ttl = claims.exp as i64 - chrono::Utc::now().timestamp();
    assert!(ttl > 0 && ttl <= 10 * 60 + 5, "bounded TTL, got {ttl}s");
    // Admitted, not yet joined.
    assert_eq!(session_state(&app, j.session_id).await, "PENDING");
    assert_eq!(events(&app, cid, "participant.admitted").await, 1);
    assert_eq!(events(&app, cid, "participant.joined").await, 0);
    connect(&app, cid, &j).await;
    assert_eq!(session_state(&app, j.session_id).await, "CONNECTED");
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
    // Multi-device: a second device is a second session of the SAME participant.
    let (s, v) = app.join(cid, j.auth(), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(uuid(&v["participant"]["id"]), j.participant_id);
    assert_ne!(uuid(&v["participantSession"]["id"]), j.session_id);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        1
    );
}

/// E3 — VIEWER is subscribe-only, and a modified viewer client cannot publish.
#[tokio::test]
async fn e3_viewer_subscribe_only() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "E3", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    let v = join_guest(&app, cid, "viewer").await;
    assert_eq!(v.body["participant"]["role"], "VIEWER");
    let p = &v.body["permissions"];
    assert_eq!(
        (
            p["audioPublish"].as_bool(),
            p["videoPublish"].as_bool(),
            p["screenSharePublish"].as_bool(),
            p["subscribe"].as_bool()
        ),
        (Some(false), Some(false), Some(false), Some(true))
    );
    let claims = livekit_api::access_token::TokenVerifier::with_api_key(
        livekit_mock::API_KEY,
        livekit_mock::API_SECRET,
    )
    .verify(&v.credential)
    .unwrap();
    assert!(
        !claims.video.can_publish
            && claims.video.can_publish_sources.is_empty()
            && claims.video.can_subscribe
    );
    connect(&app, cid, &v).await;
    for source in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        let r = lk(&app).publish(&room(cid), &v.identity, source);
        assert!(
            r.is_err(),
            "modified viewer client must not publish {source:?}"
        );
    }
}

/// E4 — VIEWER → PRESENTER promotion changes provider grants after commit.
#[tokio::test]
async fn e4_viewer_promoted_to_presenter() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "E4", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    let v = join_guest(&app, cid, "viewer").await;
    connect(&app, cid, &v).await;
    assert!(lk(&app)
        .publish(&room(cid), &v.identity, proto::TrackSource::Camera)
        .is_err());
    let (s, body) = change_role(&app, cid, &owner, v.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let perm = lk(&app)
        .participant(&room(cid), &v.identity)
        .unwrap()
        .permission;
    assert!(perm.can_publish, "provider grants updated after commit");
    assert!(lk(&app)
        .publish(&room(cid), &v.identity, proto::TrackSource::Camera)
        .is_ok());
    assert!(lk(&app)
        .publish(&room(cid), &v.identity, proto::TrackSource::ScreenShare)
        .is_ok());
    assert_eq!(
        lk(&app).call_count("UpdateParticipant"),
        2,
        "role sync + connect re-assert"
    );
    assert_eq!(
        events(&app, cid, "participant.media_permission_changed").await,
        1
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='PROVIDER_COMMAND' AND status='PENDING'")).await, 0);
}

/// E5 — Participant → VIEWER demotion revokes publishing at the provider.
#[tokio::test]
async fn e5_demotion_to_viewer() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E5", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "speaker").await;
    connect(&app, cid, &p).await;
    lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .expect("participant may publish");
    let (s, _) = change_role(&app, cid, &owner, p.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    let state = lk(&app).participant(&room(cid), &p.identity).unwrap();
    assert!(!state.permission.can_publish && state.permission.can_subscribe);
    assert!(
        state.tracks.is_empty(),
        "provider unpublished revoked tracks"
    );
    assert!(lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .is_err());
}

/// E6 — Remove: provider disconnect, session finalized, stale credential cannot bypass,
/// reconnect re-runs authorization.
#[tokio::test]
async fn e6_remove_participant() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E6", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "troll").await;
    connect(&app, cid, &p).await;
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                p.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    assert!(
        lk(&app).participant(&room(cid), &p.identity).is_none(),
        "provider participant disconnected"
    );
    assert_eq!(session_state(&app, p.session_id).await, "TERMINATED");
    // The still-valid provider credential reconnects directly to the SFU …
    lk(&app).connect(&p.credential).unwrap();
    let (_, w) = provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
    assert_eq!(w["outcome"], "STALE_SESSION_EVICTED");
    // … and is evicted because PostgreSQL authority says no.
    assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
    // A new join re-runs authorization and is refused.
    let (s, v) = app.join(cid, p.auth(), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
}

/// E7 — Ban: provider-enforced disconnect, no new credential, every reconnect checks it.
#[tokio::test]
async fn e7_ban_participant() {
    let app = app!();
    let owner = app.user("owner").await;
    let troll = app.user("troll").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "E7", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let p = join_user(&app, cid, &troll).await;
    connect(&app, cid, &p).await;
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{cid}/participants/{}/ban",
                p.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({ "scope": "SPACE" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
    let (s, v) = app.join(cid, Auth::Bearer(&troll.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
    assert!(v["rtc"].is_null());
    lk(&app).connect(&p.credential).unwrap();
    let (_, w) = provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
    assert_eq!(w["outcome"], "STALE_SESSION_EVICTED");
    assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
}

/// E8 — END: block → terminate room → confirm absence → finalize sessions → ENDED.
#[tokio::test]
async fn e8_end_conference() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E8", "accessPolicy": "PUBLIC" }))
        .await;
    let a = join_guest(&app, cid, "a").await;
    let b = join_guest(&app, cid, "b").await;
    connect(&app, cid, &a).await;
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conference"]["status"], "ENDED");
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    assert!(!lk(&app).has_room(&room(cid)), "room terminated");
    assert_eq!(lk(&app).call_count("DeleteRoom"), 1);
    for s in [a.session_id, b.session_id] {
        assert_eq!(session_state(&app, s).await, "TERMINATED");
    }
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND end_reason='CONFERENCE_ENDED'")).await, 2);
    assert_eq!(events(&app, cid, "conference.ending").await, 1);
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
}

/// E9 — Termination failure keeps ENDING (joins refused); reconciliation finishes it.
#[tokio::test]
async fn e9_termination_failure_and_reconciliation() {
    use std::sync::atomic::Ordering;
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E9", "accessPolicy": "PUBLIC" }))
        .await;
    let a = join_guest(&app, cid, "a").await;
    lk(&app).faults.fail_delete.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["conference"]["status"], "ENDING",
        "never ENDED without provider termination"
    );
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL");
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_terminate_requested' AND status='PENDING' AND last_error IS NOT NULL")).await, 1);
    // A delete that is acknowledged but leaves the room is not "terminated" either.
    lk(&app).faults.fail_delete.store(false, Ordering::SeqCst);
    lk(&app).faults.delete_noop.store(true, Ordering::SeqCst);
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let r = reconciler.tick().await.unwrap();
    assert!(r.failed >= 1, "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "ENDING");
    // Provider recovers → reconciler confirms absence → ENDED.
    lk(&app).faults.delete_noop.store(false, Ordering::SeqCst);
    let r = reconciler.tick().await.unwrap();
    assert!(
        r.kinds.contains(&"ENDING_NOT_FINALIZED".to_string()) && r.repaired >= 1,
        "{r:?}"
    );
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(session_state(&app, a.session_id).await, "TERMINATED");
    assert!(!lk(&app).has_room(&room(cid)));
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    // The relay later finds the command obsolete and completes it without side effects.
    let relay = backend::workers::outbox_relay::OutboxRelay::with_control(
        app.db.conn.clone(),
        None,
        Some(std::sync::Arc::new(
            backend::services::rtc_control::RtcControl::new(app.state.meeting.core.clone()),
        )),
    );
    common::rtc::exec(
        &app,
        &format!("UPDATE outbox_events SET available_at = now() WHERE conference_id='{cid}'"),
    )
    .await;
    relay.tick(100).await.unwrap();
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='PROVIDER_COMMAND' AND status='PENDING'")).await, 0);
}

/// E10 — Provider join/leave drive ParticipantSession state (never admission).
#[tokio::test]
async fn e10_webhook_join_leave() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E10", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    connect(&app, cid, &p).await;
    assert_eq!(session_state(&app, p.session_id).await, "CONNECTED");
    // Simulate a 2-minute media session (provider timestamps have 1 s resolution).
    exec(&app, &format!("UPDATE participant_sessions SET established_at = now() - interval '3 minutes', connected_at = now() - interval '2 minutes' WHERE id='{}'", p.session_id)).await;
    lk(&app).disconnect(&room(cid), &p.identity);
    let (s, w) = provider_event(&app, "participant_left", cid, Some(&p.identity)).await;
    assert_eq!(
        (s, w["outcome"].as_str()),
        (StatusCode::OK, Some("APPLIED"))
    );
    assert_eq!(session_state(&app, p.session_id).await, "DISCONNECTED");
    assert_eq!(events(&app, cid, "participant.left").await, 1);
    // Admission is untouched by provider callbacks.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='ADMITTED' AND role='PARTICIPANT'", p.participant_id)).await, 1);
    let attendance =
        backend::services::admission::attendance_seconds(&app.db.conn, cid, chrono::Utc::now())
            .await
            .unwrap();
    assert_eq!(attendance.len(), 1);
    assert!(
        (110..=130).contains(&attendance[0].1),
        "attendance from the connected interval: {attendance:?}"
    );
}

/// E11 — Webhook replay is deduplicated (no duplicate session, event or finalization).
#[tokio::test]
async fn e11_webhook_replay() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E11", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_replay_1",
        &room(cid),
        None,
        Some(&p.identity),
    );
    let (_, first) = app.signed_webhook(&body).await;
    let (_, second) = app.signed_webhook(&body).await;
    assert_eq!(first["outcome"], "APPLIED");
    assert_eq!(second["outcome"], "DUPLICATE");
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
    assert_eq!(
        app.scalar("SELECT count(*) FROM rtc_webhook_events WHERE event_id='EV_replay_1'")
            .await,
        1
    );
    // room_finished replayed while ENDING: finalized exactly once.
    use std::sync::atomic::Ordering;
    lk(&app).faults.fail_delete.store(true, Ordering::SeqCst);
    app.post(
        &format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    lk(&app).drop_room(&room(cid));
    let fin = livekit_mock::webhook_body("room_finished", "EV_finish_1", &room(cid), None, None);
    let (_, a) = app.signed_webhook(&fin).await;
    let (_, b) = app.signed_webhook(&fin).await;
    assert_eq!(
        (a["outcome"].as_str(), b["outcome"].as_str()),
        (Some("APPLIED"), Some("DUPLICATE"))
    );
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
}

async fn ws_connect(
    addr: std::net::SocketAddr,
    cid: Uuid,
    ticket: &str,
    cursor: Option<i64>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut url = format!("ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={ticket}");
    if let Some(c) = cursor {
        url.push_str(&format!("&cursor={c}"));
    }
    tokio_tungstenite::connect_async(url)
        .await
        .expect("ws upgrade")
        .0
}

async fn next_frame<S>(ws: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame within 10s")
            .expect("stream open")
            .expect("frame");
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn next_event<S>(ws: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let f = next_frame(ws).await;
        if f["type"] == "event" {
            return f["event"].clone();
        }
    }
}

async fn ticket(app: &TestApp, cid: Uuid, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(
        &format!("/api/v2/conferences/{cid}/realtime/ticket"),
        auth,
        json!({}),
    )
    .await
}

/// E12 — Realtime subscription is authorized server-side.
#[tokio::test]
async fn e12_realtime_authorization() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let owner = app.user("owner").await;
    let stranger = app.user("stranger").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "E12", "accessPolicy": "AUTHENTICATED", "waitingRoomPolicy": "OFF" }),
        )
        .await;
    let (_, other) = app
        .live_conference(
            &owner,
            json!({ "title": "other", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let addr = app.serve().await;
    // Knowing the id is not enough.
    let (s, v) = ticket(&app, cid, Auth::Bearer(&stranger.token)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "{v}"
    );
    let (s, _) = ticket(&app, cid, Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Forged / cross-conference tickets are rejected before the upgrade.
    let (s, v) = ticket(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["scope"], "FULL");
    let t = v["ticket"].as_str().unwrap().to_string();
    for (target, tk) in [
        (other, t.clone()),
        (cid, format!("{t}x")),
        (cid, "garbage".to_string()),
    ] {
        let url = format!("ws://{addr}/api/v2/conferences/{target}/realtime?ticket={tk}");
        assert!(
            tokio_tungstenite::connect_async(url).await.is_err(),
            "rejected before upgrade"
        );
    }
    // Authorized subscriber receives committed events with a gap-free sequence.
    let mut ws = ws_connect(addr, cid, &t, None).await;
    let hello = next_frame(&mut ws).await;
    assert_eq!(hello["type"], "subscribed", "{hello}");
    let start = hello["cursor"].as_i64().unwrap();
    app.post(
        &format!("/api/v2/conferences/{cid}/lock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let e = next_event(&mut ws).await;
    assert_eq!(e["eventType"], "conference.locked");
    assert_eq!(e["sequence"].as_i64().unwrap(), start + 1);
    assert_eq!(e["schemaVersion"], 1);
    assert!(
        e["eventId"].is_string() && e["occurredAt"].is_string() && e["conferenceId"] == json!(cid)
    );
    // A guest gets FULL scope only once admitted; their own credential header works too.
    let g = join_guest(&app, other, "guest").await;
    let (s, v) = ticket(&app, other, g.auth()).await;
    assert_eq!((s, v["scope"].as_str()), (StatusCode::OK, Some("FULL")));
    // …but never for a conference the credential does not belong to.
    let (s, _) = ticket(&app, cid, g.auth()).await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED,
        "{s}"
    );
    let _ = ws.close(None).await;
}

/// E13 — Reconnect with a cursor replays exactly the missed events, in order; an
/// unservable cursor yields an explicit canonical-refetch instruction.
#[tokio::test]
async fn e13_realtime_reconnect_recovery() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "E13", "accessPolicy": "PUBLIC" }))
        .await;
    let addr = app.serve().await;
    let (_, v) = ticket(&app, cid, Auth::Bearer(&owner.token)).await;
    let mut ws = ws_connect(addr, cid, v["ticket"].as_str().unwrap(), None).await;
    let start = next_frame(&mut ws).await["cursor"].as_i64().unwrap();
    app.post(
        &format!("/api/v2/conferences/{cid}/lock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let seen = next_event(&mut ws).await["sequence"].as_i64().unwrap();
    let _ = ws.close(None).await;
    drop(ws);
    // While disconnected: three more committed events.
    app.post(
        &format!("/api/v2/conferences/{cid}/unlock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    join_guest(&app, cid, "late-1").await;
    app.post(
        &format!("/api/v2/conferences/{cid}/lock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (_, v) = ticket(&app, cid, Auth::Bearer(&owner.token)).await;
    let mut ws = ws_connect(addr, cid, v["ticket"].as_str().unwrap(), Some(seen)).await;
    let hello = next_frame(&mut ws).await;
    assert_eq!(hello["cursor"].as_i64(), Some(seen), "{hello}");
    let mut types = vec![];
    let mut expected = seen + 1;
    while types.len() < 3 {
        let e = next_event(&mut ws).await;
        assert_eq!(e["sequence"].as_i64(), Some(expected), "in order, gap-free");
        expected += 1;
        types.push(e["eventType"].as_str().unwrap().to_string());
    }
    assert_eq!(
        types,
        [
            "conference.unlocked",
            "participant.admitted",
            "conference.locked"
        ]
    );
    assert!(start < seen);
    let _ = ws.close(None).await;
    // Unservable cursor (ahead of the authority) → explicit resync instruction.
    let (_, v) = ticket(&app, cid, Auth::Bearer(&owner.token)).await;
    let mut ws = ws_connect(addr, cid, v["ticket"].as_str().unwrap(), Some(9_999)).await;
    let f = next_frame(&mut ws).await;
    assert_eq!(f["type"], "resync_required");
    assert_eq!(f["canonical"], format!("/api/v2/conferences/{cid}"));
    // REST replay agrees, and tolerates duplicates (same cursor → same events).
    let (_, r1) = app
        .get(
            &format!("/api/v2/conferences/{cid}/events?after={seen}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let (_, r2) = app
        .get(
            &format!("/api/v2/conferences/{cid}/events?after={seen}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(r1["events"].as_array().unwrap().len(), 3);
    assert_eq!(r1["events"], r2["events"]);
    assert_eq!(r1["resyncRequired"], false);
    let (_, r3) = app
        .get(
            &format!("/api/v2/conferences/{cid}/events?after=9999"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(r3["resyncRequired"], true);
}

/// E14 — Provider drift is detected and repaired deterministically; ambiguity escalates.
#[tokio::test]
async fn e14_provider_drift_reconciliation() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "E14", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    let viewer = join_guest(&app, cid, "viewer").await;
    connect(&app, cid, &viewer).await;
    let pending = join_guest(&app, cid, "late-webhook").await;
    let gone = join_guest(&app, cid, "gone").await;
    connect(&app, cid, &gone).await;
    // Drift: permission tampering, rogue identity, connected-without-webhook, vanished session.
    lk(&app).insert_participant(
        &room(cid),
        &viewer.identity,
        proto::ParticipantPermission {
            can_publish: true,
            can_subscribe: true,
            ..Default::default()
        },
    );
    lk(&app).insert_participant(
        &room(cid),
        "rogue-identity",
        proto::ParticipantPermission {
            can_publish: true,
            ..Default::default()
        },
    );
    lk(&app).connect(&pending.credential).unwrap();
    lk(&app).disconnect(&room(cid), &gone.identity);
    exec(&app, &format!("UPDATE participant_sessions SET connected_at = now() - interval '5 minutes' WHERE id='{}'", gone.session_id)).await;
    // Orphan room of an ended conference, and an unmapped cf- room.
    let (_, ended) = app
        .live_conference(
            &owner,
            json!({ "title": "ended", "accessPolicy": "PUBLIC" }),
        )
        .await;
    app.post(
        &format!("/api/v2/conferences/{ended}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    lk(&app).insert_room(&room(ended));
    lk(&app).insert_room("cf-unmapped-room");

    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let r = reconciler.tick().await.unwrap();
    for kind in [
        "PERMISSION_DRIFT",
        "PROVIDER_PARTICIPANT_WITHOUT_SESSION",
        "SESSION_CONNECTED_NOT_RECORDED",
        "SESSION_CONNECTED_BUT_ABSENT",
        "ORPHAN_ROOM",
        "UNMAPPED_PROVIDER_ROOM",
    ] {
        assert!(r.kinds.iter().any(|k| k == kind), "{kind} detected: {r:?}");
    }
    assert!(
        !lk(&app)
            .participant(&room(cid), &viewer.identity)
            .unwrap()
            .permission
            .can_publish,
        "grants repaired"
    );
    assert!(
        lk(&app).participant(&room(cid), "rogue-identity").is_none(),
        "rogue evicted"
    );
    assert_eq!(session_state(&app, pending.session_id).await, "CONNECTED");
    assert_eq!(session_state(&app, gone.session_id).await, "DISCONNECTED");
    assert!(!lk(&app).has_room(&room(ended)), "orphan room terminated");
    assert!(
        lk(&app).has_room("cf-unmapped-room"),
        "ambiguous room is escalated, never deleted"
    );
    assert_eq!(app.scalar("SELECT count(*) FROM rtc_drift_events WHERE kind='UNMAPPED_PROVIDER_ROOM' AND action='ESCALATED'").await, 1);
    // LIVE room lost at the provider → recreated.
    lk(&app).drop_room(&room(cid));
    let r = reconciler.tick().await.unwrap();
    assert!(r.kinds.contains(&"LIVE_ROOM_MISSING".to_string()), "{r:?}");
    assert!(lk(&app).has_room(&room(cid)));
    // Idempotent: a converged system reports only the escalation.
    let r = reconciler.tick().await.unwrap();
    assert_eq!(r.repaired, 0, "{r:?}");
}

/// E15 — Host A cannot provider-control a participant of conference B; the attack
/// fails before any provider call.
#[tokio::test]
async fn e15_cross_conference_provider_control_attack() {
    let app = app!();
    let host_a = app.user("host-a").await;
    let host_b = app.user("host-b").await;
    let (_, conf_a) = app
        .live_conference(&host_a, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, conf_b) = app
        .live_conference(&host_b, json!({ "title": "B", "accessPolicy": "PUBLIC" }))
        .await;
    let victim = join_guest(&app, conf_b, "victim").await;
    connect(&app, conf_b, &victim).await;
    let before: Vec<_> = [
        "RemoveParticipant",
        "UpdateParticipant",
        "MutePublishedTrack",
        "GetParticipant",
    ]
    .iter()
    .map(|m| lk(&app).call_count(m))
    .collect();
    let pid = victim.participant_id;
    let attempts = [
        (
            format!("/api/v2/conferences/{conf_a}/participants/{pid}/mute"),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            format!("/api/v2/conferences/{conf_a}/participants/{pid}/remove"),
            json!({}),
        ),
        (
            format!("/api/v2/conferences/{conf_a}/participants/{pid}/ban"),
            json!({}),
        ),
        (
            format!("/api/v2/conferences/{conf_b}/participants/{pid}/mute"),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            format!("/api/v2/conferences/{conf_b}/participants/{pid}/remove"),
            json!({}),
        ),
    ];
    for (uri, body) in attempts {
        let (s, v) = app.post(&uri, Auth::Bearer(&host_a.token), body).await;
        assert!(
            matches!(s, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
            "{uri}: {s} {v}"
        );
    }
    let (s, _) = app
        .patch(
            &format!("/api/v2/conferences/{conf_a}/participants/{pid}/role"),
            Auth::Bearer(&host_a.token),
            json!({ "role": "VIEWER" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let after: Vec<_> = [
        "RemoveParticipant",
        "UpdateParticipant",
        "MutePublishedTrack",
        "GetParticipant",
    ]
    .iter()
    .map(|m| lk(&app).call_count(m))
    .collect();
    assert_eq!(before, after, "no provider call was made");
    assert!(
        lk(&app)
            .participant(&room(conf_b), &victim.identity)
            .is_some(),
        "victim untouched"
    );
    assert_eq!(session_state(&app, victim.session_id).await, "CONNECTED");
    // Host B (authorized) can mute the victim: the primitive is real.
    lk(&app)
        .publish(
            &room(conf_b),
            &victim.identity,
            proto::TrackSource::Microphone,
        )
        .unwrap();
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{conf_b}/participants/{pid}/mute"),
            Auth::Bearer(&host_b.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mutedTracks"], 1);
    assert!(lk(&app)
        .participant(&room(conf_b), &victim.identity)
        .unwrap()
        .tracks
        .iter()
        .all(|t| t.muted));
}
