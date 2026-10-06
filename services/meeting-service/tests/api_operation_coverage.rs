//! Part M §9 — every `/api/v2` operation has a successful-path integration test.
//!
//! `scripts/certification/route_coverage.py` measures (from the requests every suite
//! actually sends) which operations have success-path and error-path coverage. The
//! operations below had only error-path coverage before Part M; each test drives the
//! success path through the real router and pairs it with a relevant authorization /
//! scope failure.

mod common;

use axum::http::StatusCode;
use common::{collab::*, rtc::*, *};
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

async fn collaboration_version(app: &TestApp, cid: Uuid, owner: &User) -> i64 {
    let (s, st) = app
        .get(
            format!("{}/collaboration", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{st}");
    st["collaborationVersion"].as_i64().unwrap()
}

/// Breakouts: patch assignments, host visit (enter / leave), per-room announcement.
#[tokio::test]
async fn ops_breakout_assignments_visit_and_announcement() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let host = owner_joined(&app, cid, &owner).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    let uri = format!("{}/breakouts/{}/assignments", base(cid), rooms[0]);
    // Participant cannot reassign; host can.
    let (s, _) = app
        .patch(&uri, g[0].auth(), json!({ "add": [g[1].participant_id] }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .patch(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "add": [g[0].participant_id, g[1].participant_id] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .patch(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "remove": [g[1].participant_id] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM breakout_assignments WHERE breakout_room_id='{}' AND unassigned_at IS NULL",
            rooms[0]
        ))
        .await,
        1
    );
    open_room(&app, cid, &owner, rooms[0]).await;
    // Host visit: enter a room, then return to MAIN.
    let (s, v) = app
        .post(
            format!("{}/breakouts/{}/enter", base(cid), rooms[0]),
            g[1].auth(),
            json!({}),
        )
        .await;
    assert!(s.is_client_error(), "participants cannot visit: {s} {v}");
    let (s, v) = app
        .post(
            format!("{}/breakouts/{}/enter", base(cid), rooms[0]),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .post(
            format!("{}/breakouts/leave", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let _ = host;
    // Announcement to one room.
    let ann = format!("{}/breakouts/{}/announcement", base(cid), rooms[0]);
    let (s, _) = app
        .post(&ann, g[0].auth(), json!({ "message": "hi" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(
            &ann,
            Auth::Bearer(&owner.token),
            json!({ "message": "Five minutes left" }),
        )
        .await;
    assert!(s.is_success(), "{s} {v}");
    assert!(events(&app, cid, "breakout.announcement").await >= 1);
    // Foreign breakout id under this conference.
    let (other_owner, other_cid, _) = meeting(&app, public(), 0).await;
    let foreign = create_rooms(&app, other_cid, &other_owner, json!({ "count": 1 })).await[0];
    let (s, _) = app
        .post(
            format!("{}/breakouts/{foreign}/announcement", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "message": "x" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Chat policy update (optimistic) and chat-restriction lift.
#[tokio::test]
async fn ops_chat_policy_and_restriction_lift() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let version = collaboration_version(&app, cid, &owner).await;
    let uri = format!("{}/chat-policy", base(cid));
    let (s, _) = app
        .patch(
            &uri,
            g[0].auth(),
            json!({ "collaborationVersion": version, "chatEnabled": false }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .patch(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "collaborationVersion": version, "privateChatMode": "HOSTS_ONLY" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .patch(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "collaborationVersion": version, "chatEnabled": false }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "stale version: {v}");
    // Restrict, then lift the restriction.
    let r = format!(
        "{}/participants/{}/chat-restriction",
        base(cid),
        g[0].participant_id
    );
    let (s, _) = app.post(&r, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app.delete(&r, g[0].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "cannot lift own restriction");
    let (s, v) = app.delete(&r, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = send(&app, cid, g[0].auth(), json!({ "body": "back" })).await;
    assert!(s.is_success(), "chat restored: {s} {v}");
}

/// Screen-share lock and unlock.
#[tokio::test]
async fn ops_screen_share_lock_and_unlock() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let uri = format!(
        "{}/participants/{}/screen-share-lock",
        base(cid),
        g[0].participant_id
    );
    let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let p = lk(&app).participant(&room(cid), &g[0].identity).unwrap();
    assert!(
        !p.permission
            .can_publish_sources
            .contains(&(livekit_protocol::TrackSource::ScreenShare as i32)),
        "provider grant withdrawn"
    );
    let (s, _) = app.delete(&uri, g[0].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "cannot self-unlock");
    let (s, v) = app.delete(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let p = lk(&app).participant(&room(cid), &g[0].identity).unwrap();
    assert!(p
        .permission
        .can_publish_sources
        .contains(&(livekit_protocol::TrackSource::ScreenShare as i32)));
}

/// Poll archive; question archive, unpin, unvote.
#[tokio::test]
async fn ops_poll_archive_and_question_lifecycle() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_CLOSE")).await;
    let (s, _) = app
        .post(
            format!("{}/polls/{poll}/close", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let archive = format!("{}/polls/{poll}/archive", base(cid));
    let (s, _) = app.post(&archive, g[0].auth(), json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(&archive, Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_polls WHERE id='{poll}' AND status='ARCHIVED'"
        ))
        .await,
        1
    );

    let (s, q) = app
        .post(
            format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Why?" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let qid = uuid(&q["id"]);
    let vote = format!("{}/questions/{qid}/vote", base(cid));
    let (s, _) = app.post(&vote, g[1].auth(), json!({})).await;
    assert!(s.is_success());
    let (s, v) = app.delete(&vote, g[1].auth()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["upvoteCount"].as_i64().unwrap_or(0), 0, "{v}");
    let pin = format!("{}/questions/{qid}/pin", base(cid));
    let (s, _) = app.post(&pin, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app.delete(&pin, g[1].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app.delete(&pin, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let arch = format!("{}/questions/{qid}/archive", base(cid));
    let (s, _) = app.post(&arch, g[0].auth(), json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "author cannot archive");
    let (s, v) = app.post(&arch, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_questions WHERE id='{qid}' AND status='ARCHIVED'"
        ))
        .await,
        1
    );
}

/// Transcript listing (provider-neutral path enabled in the GHI test configuration).
#[tokio::test]
async fn ops_transcript_listing() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, g) = common::artifacts::meeting(&app, 1).await;
    let uri = format!("/api/v2/conferences/{cid}/transcripts");
    let (s, v) = app.get(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = app.get(&uri, Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let _ = g;
}

/// Webinar registration listing (organizer only).
#[tokio::test]
async fn ops_registration_listing() {
    let app = app!(TestApp::artifacts());
    let owner = app.user("organizer").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/registrations"),
            Auth::None,
            json!({ "displayName": "Ada" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let uri = format!("/api/v2/meeting-spaces/{sid}/registrations");
    let (s, v) = app.get(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        !v.to_string().contains("admissionToken"),
        "tokens never listed"
    );
    let stranger = app.user("stranger").await;
    let (s, _) = app.get(&uri, Auth::Bearer(&stranger.token)).await;
    assert!(matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND));
}

/// Compatibility administration (platform administrators only).
#[tokio::test]
async fn ops_compat_backfill_and_shadow() {
    let app = app!(TestApp::new());
    let admin = app.admin("platform").await;
    let user = app.user("user").await;
    let host = app.user("host").await;
    let mid = Uuid::new_v4();
    use sea_orm::ConnectionTrait;
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, status) \
             VALUES ('{mid}', 'CVQN2345', '{h}', '{h}', 'legacy', false, now(), 'ongoing')",
            h = host.id
        ))
        .await
        .unwrap();
    let (s, _) = app
        .post(
            "/api/v2/admin/compat/backfill",
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(
            "/api/v2/admin/compat/backfill",
            Auth::Bearer(&admin.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let uri = format!("/api/v2/admin/compat/legacy-meetings/{mid}/shadow");
    let (s, v) = app.get(&uri, Auth::Bearer(&admin.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["differences"].as_array().map(|d| d.len()).unwrap_or(0),
        0,
        "{v}"
    );
    let (s, _) = app.get(&uri, Auth::Bearer(&user.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
