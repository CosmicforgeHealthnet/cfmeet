//! Phase 2 Part F — permanent security suite S1–S18.

mod common;

use axum::http::StatusCode;
use common::{collab::*, livekit_mock, rtc::*, *};
use livekit_api::access_token::TokenVerifier;
use livekit_protocol as proto;
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::collab().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn can(app: &TestApp, room_name: &str, identity: &str, source: proto::TrackSource) -> bool {
    lk(app).publish(room_name, identity, source).is_ok()
}

fn sources(cred: &str) -> Vec<String> {
    TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
        .verify(cred)
        .unwrap()
        .video
        .can_publish_sources
}

async fn self_unlock_denied(kind: &str, source: proto::TrackSource, grant: &str) {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    let url = format!(
        "{}/participants/{}/{kind}-lock",
        base(cid),
        p.participant_id
    );
    assert_eq!(
        app.post(&url, Auth::Bearer(&owner.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let (s, v) = app.delete(&url, p.auth()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "{v}"
    );
    let (s, _) = app.post(&url, p.auth(), json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!can(&app, &room(cid), &p.identity, source));
    let (_, r) = refresh(&app, cid, p, p.auth()).await;
    assert!(!sources(r["rtc"]["credential"].as_str().unwrap())
        .iter()
        .any(|s| s == grant));
    // Rejoining through the join pipeline does not clear the lock either.
    let (_, j) = app.join(cid, p.auth(), json!({})).await;
    assert!(!sources(j["rtc"]["credential"].as_str().unwrap())
        .iter()
        .any(|s| s == grant));
    assert_eq!(
        j["permissions"][if kind == "audio" {
            "audioPublish"
        } else {
            "videoPublish"
        }],
        false
    );
}

/// S1 — a participant cannot unlock their own host-imposed audio lock.
#[tokio::test]
async fn s01_cannot_self_unlock_audio() {
    self_unlock_denied("audio", proto::TrackSource::Microphone, "microphone").await;
}

/// S2 — … nor a video lock.
#[tokio::test]
async fn s02_cannot_self_unlock_video() {
    self_unlock_denied("video", proto::TrackSource::Camera, "camera").await;
}

/// S3 — a modified client cannot screen-share when policy denies it.
#[tokio::test]
async fn s03_modified_client_cannot_share() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
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
            &p.identity,
            proto::TrackSource::ScreenShare
        ),
        "provider grant denies"
    );
    let (_, r) = refresh(&app, cid, p, p.auth()).await;
    assert!(!sources(r["rtc"]["credential"].as_str().unwrap())
        .iter()
        .any(|s| s == "screen_share"));
    // A share that slipped through anyway (raced the grant) is stopped, never recorded.
    lk(&app).insert_participant(
        &room(cid),
        &p.identity,
        proto::ParticipantPermission {
            can_publish: true,
            can_subscribe: true,
            ..Default::default()
        },
    );
    let sid = lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::ScreenShare)
        .unwrap();
    track_event(
        &app,
        "track_published",
        &room(cid),
        &p.identity,
        &sid,
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(events(&app, cid, "screenshare.started").await, 0);
    assert!(
        lk(&app)
            .tracks_of(&room(cid), &p.identity, proto::TrackSource::ScreenShare)
            .is_empty(),
        "stopped at the provider"
    );
}

/// S4 — viewers can never publish microphone, camera or screen share.
#[tokio::test]
async fn s04_viewer_cannot_publish() {
    let app = app!();
    let (owner, cid, g) = meeting(
        &app,
        json!({ "title": "v", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        1,
    )
    .await;
    let v = &g[0];
    assert!(sources(&v.credential).is_empty());
    policy(
        &app,
        cid,
        &owner,
        json!({ "screenSharePolicy": "ALL_INTERACTIVE" }),
    )
    .await;
    for src in [
        proto::TrackSource::Microphone,
        proto::TrackSource::Camera,
        proto::TrackSource::ScreenShare,
    ] {
        assert!(!can(&app, &room(cid), &v.identity, src), "{src:?}");
    }
    let (_, r) = refresh(&app, cid, v, v.auth()).await;
    assert_eq!(r["permissions"]["audioPublish"], false);
    assert_eq!(r["permissions"]["screenSharePublish"], false);
}

/// S5 — Host A cannot mute / lock / spotlight a participant of conference B.
#[tokio::test]
async fn s05_cross_conference_media_controls() {
    let app = app!();
    let host_a = app.user("a").await;
    let (_, conf_a) = app.live_conference(&host_a, public()).await;
    let (_, conf_b, g) = meeting(&app, public(), 1).await;
    let pid = g[0].participant_id;
    let calls = lk(&app).calls.lock().unwrap().len();
    for (uri, body) in [
        (
            format!("{}/participants/{pid}/mute", base(conf_a)),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            format!("{}/participants/{pid}/audio-lock", base(conf_a)),
            json!({}),
        ),
        (
            format!("{}/participants/{pid}/video-lock", base(conf_a)),
            json!({}),
        ),
        (
            format!("{}/spotlights", base(conf_a)),
            json!({ "participantId": pid }),
        ),
    ] {
        let (s, v) = app.post(&uri, Auth::Bearer(&host_a.token), body).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "PARTICIPANT_NOT_FOUND"),
            "{uri}"
        );
    }
    for uri in [
        format!("{}/participants/{pid}/audio-lock", base(conf_b)),
        format!("{}/participants/{pid}/video-lock", base(conf_b)),
    ] {
        assert_eq!(
            app.post(&uri, Auth::Bearer(&host_a.token), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(lk(&app).calls.lock().unwrap().len(), calls);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND (audio_locked_at IS NOT NULL OR spotlighted_at IS NOT NULL)")).await, 0);
}

/// S6 — chat sender identity cannot be forged.
#[tokio::test]
async fn s06_chat_sender_not_forgeable() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 2).await;
    for field in [
        "senderParticipantId",
        "participantId",
        "userId",
        "role",
        "senderId",
    ] {
        let mut body = json!({ "body": "hi" });
        body[field] = json!(g[1].participant_id);
        let (s, v) = send(&app, cid, g[0].auth(), body).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{field}"
        );
    }
    let (_, m) = send(&app, cid, g[0].auth(), json!({ "body": "hi" })).await;
    assert_eq!(uuid(&m["senderParticipantId"]), g[0].participant_id);
    // Without a participant credential there is no sender.
    assert_eq!(
        send(&app, cid, Auth::None, json!({ "body": "x" })).await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// S7 — private chat is readable only by its two parties.
#[tokio::test]
async fn s07_private_chat_isolation() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let (_, m) = send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "secret", "recipientParticipantId": g[1].participant_id }),
    )
    .await;
    let mid = m["id"].as_str().unwrap();
    let (_, c) = messages(
        &app,
        cid,
        g[2].auth(),
        &format!("?scope=PRIVATE&peerId={}", g[0].participant_id),
    )
    .await;
    assert!(!c.to_string().contains("secret"));
    let (_, c) = messages(
        &app,
        cid,
        g[2].auth(),
        &format!("?scope=PRIVATE&peerId={}", g[1].participant_id),
    )
    .await;
    assert!(!c.to_string().contains("secret"));
    // The host cannot read it either (no moderator access to private chat).
    owner_joined(&app, cid, &owner).await;
    let (_, h) = messages(
        &app,
        cid,
        Auth::Bearer(&owner.token),
        &format!("?scope=PRIVATE&peerId={}", g[0].participant_id),
    )
    .await;
    assert!(!h.to_string().contains("secret"));
    let (s, v) = app
        .delete(&format!("{}/messages/{mid}", base(cid)), g[2].auth())
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "MESSAGE_NOT_FOUND"));
    // Replay/realtime never delivers it to third parties.
    let (_, r) = app
        .get(&format!("{}/events?after=0", base(cid)), g[2].auth())
        .await;
    assert!(!r.to_string().contains("secret"));
    let (_, r) = app
        .get(&format!("{}/events?after=0", base(cid)), g[1].auth())
        .await;
    assert!(r.to_string().contains("secret"));
}

async fn two_rooms(app: &TestApp, cid: Uuid, owner: &User, a: &Joined, b: &Joined) -> (Uuid, Uuid) {
    let rooms = create_rooms(app, cid, owner, json!({ "names": ["A", "B"] })).await;
    app.post(&format!("{}/breakouts/assign", base(cid)), Auth::Bearer(&owner.token), json!({ "mode": "MANUAL", "assignments": [
        { "participantId": a.participant_id, "breakoutId": rooms[0] }, { "participantId": b.participant_id, "breakoutId": rooms[1] } ] })).await;
    open_room(app, cid, owner, rooms[0]).await;
    open_room(app, cid, owner, rooms[1]).await;
    (rooms[0], rooms[1])
}

/// S8 — a participant cannot read another breakout's chat (REST or realtime).
#[tokio::test]
async fn s08_breakout_chat_isolation() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, _) = two_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "room-A-only", "breakoutId": ra }),
    )
    .await;
    let (s, _) = messages(
        &app,
        cid,
        g[1].auth(),
        &format!("?scope=BREAKOUT&breakoutId={ra}"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, r) = app
        .get(&format!("{}/events?after=0", base(cid)), g[1].auth())
        .await;
    assert!(!r.to_string().contains("room-A-only"));
    let (_, r) = app
        .get(&format!("{}/events?after=0", base(cid)), g[0].auth())
        .await;
    assert!(r.to_string().contains("room-A-only"));
    // Main chat is hidden from breakout participants by default.
    assert_eq!(
        messages(&app, cid, g[1].auth(), "").await.0,
        StatusCode::FORBIDDEN
    );
}

/// S9 — participants cannot move themselves into a breakout.
#[tokio::test]
async fn s09_no_self_routing() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, rb) = two_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    let me = &g[0];
    for (uri, body) in [
        (
            format!("{}/breakouts/move", base(cid)),
            json!({ "participantId": me.participant_id, "toBreakoutId": rb }),
        ),
        (format!("{}/breakouts/{rb}/enter", base(cid)), json!({})),
        (
            format!("{}/breakouts/assign", base(cid)),
            json!({ "mode": "MANUAL", "assignments": [{ "participantId": me.participant_id, "breakoutId": rb }] }),
        ),
    ] {
        assert_eq!(
            app.post(&uri, me.auth(), body).await.0,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    let (s, _) = app
        .patch(
            &format!("{}/breakouts/{rb}/assignments", base(cid)),
            me.auth(),
            json!({ "add": [me.participant_id] }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), me.auth())
        .await;
    assert_eq!(st["me"]["mediaContext"]["breakoutId"], json!(ra));
}

/// S10 — a stale breakout credential cannot keep the old context after reassignment.
#[tokio::test]
async fn s10_stale_breakout_credential() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (ra, rb) = two_rooms(&app, cid, &owner, &g[0], &g[1]).await;
    let (_, old) = refresh(&app, cid, &g[0], g[0].auth()).await;
    let old_cred = old["rtc"]["credential"].as_str().unwrap().to_string();
    app.post(
        &format!("{}/breakouts/move", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "participantId": g[0].participant_id, "toBreakoutId": rb }),
    )
    .await;
    // Still-valid token for room A reconnects at the SFU …
    lk(&app).connect(&old_cred).unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &breakout_room(ra),
        None,
        Some(&g[0].identity),
    );
    let (_, w) = app.signed_webhook(&body).await;
    assert_eq!(w["outcome"], "STALE_SESSION_EVICTED");
    assert!(lk(&app)
        .participant(&breakout_room(ra), &g[0].identity)
        .is_none());
    // … and the reconciler evicts it even without a webhook.
    lk(&app).connect(&old_cred).unwrap();
    let r = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(
        r.kinds.iter().any(|k| k == "BREAKOUT_CONTEXT_DRIFT"),
        "{r:?}"
    );
    assert!(lk(&app)
        .participant(&breakout_room(ra), &g[0].identity)
        .is_none());
    // And a stale main-room credential is evicted while the participant is in a breakout.
    lk(&app).connect(&g[0].credential).unwrap();
    let (_, w) = provider_event(&app, "participant_joined", cid, Some(&g[0].identity)).await;
    assert_eq!(w["outcome"], "STALE_SESSION_EVICTED");
}

/// S11 — a poll answer cannot be submitted for another participant.
#[tokio::test]
async fn s11_poll_answer_identity() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    for field in ["participantId", "voterId", "userId"] {
        let mut body = json!({ "answers": [{ "questionId": "q1", "optionIds": ["o1"] }] });
        body[field] = json!(g[1].participant_id);
        let (s, v) = app
            .post(
                &format!("{}/polls/{poll}/responses", base(cid)),
                g[0].auth(),
                body,
            )
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{field}"
        );
    }
    assert_eq!(
        vote(&app, cid, poll, g[0].auth(), "o1").await.0,
        StatusCode::CREATED
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_poll_ballots WHERE poll_id='{poll}' AND participant_id='{}'", g[1].participant_id)).await, 0);
    assert_eq!(
        vote(&app, cid, poll, g[1].auth(), "o2").await.0,
        StatusCode::CREATED,
        "B can still vote"
    );
}

/// S12 — anonymous responses cannot be correlated through ordinary host APIs.
#[tokio::test]
async fn s12_anonymous_poll_uncorrelatable() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let mut body = simple_poll("HOST_ONLY");
    body["anonymous"] = json!(true);
    body["questions"] = json!([{ "type": "SHORT_TEXT", "prompt": "Feedback?" }]);
    let (_, p) = app
        .post(
            &format!("{}/polls", base(cid)),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    let poll = uuid(&p["id"]);
    app.post(
        &format!("{}/polls/{poll}/open", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    for (i, x) in g.iter().enumerate() {
        app.post(
            &format!("{}/polls/{poll}/responses", base(cid)),
            x.auth(),
            json!({ "answers": [{ "questionId": "q1", "text": format!("answer {i}") }] }),
        )
        .await;
    }
    let (_, dup) = app
        .post(
            &format!("{}/polls/{poll}/responses", base(cid)),
            g[0].auth(),
            json!({ "answers": [{ "questionId": "q1", "text": "again" }] }),
        )
        .await;
    app.post(
        &format!("{}/polls/{poll}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    owner_joined(&app, cid, &owner).await;
    let mut outputs = vec![dup.to_string()];
    outputs.push(
        app.get(
            &format!("{}/polls/{poll}/results", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1
        .to_string(),
    );
    outputs.push(
        app.get(
            &format!("{}/polls/{poll}", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1
        .to_string(),
    );
    outputs.push(
        app.get(&format!("{}/polls", base(cid)), Auth::Bearer(&owner.token))
            .await
            .1
            .to_string(),
    );
    // Realtime: every poll event the host can see.
    let (_, evs) = app
        .get(
            &format!("{}/events?after=0", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    for e in evs["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["eventType"].as_str().unwrap().starts_with("poll."))
    {
        outputs.push(e.to_string());
    }
    for o in &outputs {
        for x in &g {
            assert!(
                !o.contains(&x.participant_id.to_string()),
                "identity leaked: {o}"
            );
            assert!(!o.contains(&x.identity));
        }
    }
    assert!(
        outputs[1].contains("answer 0"),
        "texts available, unattributed"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_poll_responses WHERE poll_id='{poll}' AND participant_id IS NOT NULL")).await, 0);
}

/// S13 — quiz correct answers are not leaked before the results state allows it.
#[tokio::test]
async fn s13_quiz_answers_protected() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let body = json!({ "title": "Q", "quizMode": true, "resultsVisibility": "AFTER_CLOSE",
        "questions": [{ "type": "SINGLE_CHOICE", "prompt": "?", "options": ["a", "b"], "correct": [1] }] });
    let poll = open_poll(&app, cid, &owner, body).await;
    let mut seen = vec![];
    seen.push(
        app.get(&format!("{}/polls/{poll}", base(cid)), g[0].auth())
            .await
            .1,
    );
    seen.push(
        app.get(&format!("{}/polls", base(cid)), g[0].auth())
            .await
            .1,
    );
    seen.push(
        app.get(&format!("{}/events?after=0", base(cid)), g[0].auth())
            .await
            .1,
    );
    seen.push(vote(&app, cid, poll, g[0].auth(), "o2").await.1);
    seen.push(
        app.get(&format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
            .await
            .1,
    );
    for v in &seen {
        assert!(!v.to_string().contains("\"correct\""), "leak: {v}");
        assert!(
            v.get("score").map(|s| s.is_null()).unwrap_or(true),
            "score leak: {v}"
        );
    }
    let (_, host) = app
        .get(
            &format!("{}/polls/{poll}", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(
        host.to_string().contains("\"correct\""),
        "managers see the key"
    );
}

/// S14 — anonymous Q&A submitter identity is not leaked.
#[tokio::test]
async fn s14_anonymous_qna_identity() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "anon?", "anonymous": true }),
        )
        .await;
    let qid = uuid(&q["id"]);
    owner_joined(&app, cid, &owner).await;
    let mut outputs = vec![];
    outputs.push(
        app.get(
            &format!("{}/questions", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    );
    outputs.push(
        app.get(&format!("{}/questions", base(cid)), g[1].auth())
            .await
            .1,
    );
    outputs.push(
        app.post(
            &format!("{}/questions/{qid}/vote", base(cid)),
            g[1].auth(),
            json!({}),
        )
        .await
        .1,
    );
    outputs.push(
        app.post(
            &format!("{}/questions/{qid}/pin", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await
        .1,
    );
    outputs.push(
        app.post(
            &format!("{}/questions/{qid}/answer", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "answer": "a" }),
        )
        .await
        .1,
    );
    for auth in [Auth::Bearer(&owner.token), g[1].auth()] {
        let (_, evs) = app
            .get(&format!("{}/events?after=0", base(cid)), auth)
            .await;
        for e in evs["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["eventType"].as_str().unwrap().starts_with("qna."))
        {
            outputs.push(e.clone());
        }
    }
    for o in outputs {
        let t = o.to_string();
        assert!(
            !t.contains(&g[0].participant_id.to_string()) && !t.contains("guest-0"),
            "leak: {t}"
        );
    }
}

/// S15 — question vote counts cannot be forged.
#[tokio::test]
async fn s15_vote_count_not_forgeable() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 2).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let qid = uuid(&q["id"]);
    let (s, v) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q2", "upvoteCount": 100 }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    for _ in 0..3 {
        app.post(
            &format!("{}/questions/{qid}/vote", base(cid)),
            g[1].auth(),
            json!({}),
        )
        .await;
    }
    let (s, v) = app
        .post(
            &format!("{}/questions/{qid}/vote", base(cid)),
            g[0].auth(),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        "no self-vote"
    );
    let (_, list) = app
        .get(&format!("{}/questions", base(cid)), g[0].auth())
        .await;
    assert_eq!(list["questions"][0]["upvoteCount"], 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_question_votes WHERE question_id='{qid}'"
        ))
        .await,
        1
    );
}

/// S16 — foreign-conference poll and question ids are rejected.
#[tokio::test]
async fn s16_foreign_poll_and_question_ids() {
    let app = app!();
    let (owner_a, conf_a, ga) = meeting(&app, public(), 1).await;
    let (owner_b, conf_b, gb) = meeting(&app, public(), 1).await;
    let poll_b = open_poll(&app, conf_b, &owner_b, simple_poll("AFTER_VOTE")).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(conf_b)),
            gb[0].auth(),
            json!({ "body": "B" }),
        )
        .await;
    let qid = uuid(&q["id"]);
    let (s, v) = vote(&app, conf_a, poll_b, ga[0].auth(), "o1").await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "POLL_NOT_FOUND"));
    for uri in [
        format!("{}/polls/{poll_b}", base(conf_a)),
        format!("{}/polls/{poll_b}/results", base(conf_a)),
    ] {
        let (s, v) = app.get(&uri, Auth::Bearer(&owner_a.token)).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "POLL_NOT_FOUND"),
            "{uri}"
        );
    }
    for uri in [
        format!("{}/questions/{qid}/vote", base(conf_a)),
        format!("{}/questions/{qid}/answer", base(conf_a)),
    ] {
        let (s, v) = app
            .post(&uri, Auth::Bearer(&owner_a.token), json!({}))
            .await;
        assert!(
            s == StatusCode::NOT_FOUND && code(&v) == "QUESTION_NOT_FOUND"
                || s == StatusCode::NOT_FOUND,
            "{uri}: {v}"
        );
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_poll_ballots WHERE poll_id='{poll_b}'"
        ))
        .await,
        0
    );
}

/// S17 — a provider webhook cannot create screen-share state for another conference.
#[tokio::test]
async fn s17_webhook_screen_share_scoping() {
    let app = app!();
    let (_, conf_a, _) = meeting(&app, public(), 0).await;
    let (owner_b, conf_b, gb) = meeting(&app, public(), 1).await;
    let _ = owner_b;
    // B's identity reported in A's room, and in an unknown room.
    let (_, w) = track_event(
        &app,
        "track_published",
        &room(conf_a),
        &gb[0].identity,
        "TR_x",
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(w["outcome"], "IGNORED");
    let (_, w) = track_event(
        &app,
        "track_published",
        "cf-unknown",
        &gb[0].identity,
        "TR_y",
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(w["outcome"], "UNKNOWN_ROOM");
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM participant_sessions WHERE screen_share_started_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        events(&app, conf_a, "screenshare.started").await
            + events(&app, conf_b, "screenshare.started").await,
        0
    );
    // Forged (unsigned) track webhooks are rejected outright.
    let body = livekit_mock::track_webhook_body(
        "track_published",
        "EV_forged",
        &room(conf_b),
        &gb[0].identity,
        "TR_z",
        "SCREEN_SHARE",
    );
    assert_eq!(app.webhook(&body, None).await.0, StatusCode::UNAUTHORIZED);
}

/// S18 — clients cannot inject actor / role / provider identity through DTOs.
#[tokio::test]
async fn s18_dto_injection() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let pid = g[0].participant_id;
    let cases = vec![
        (
            format!("{}/spotlights", base(cid)),
            json!({ "participantId": pid, "actorId": owner.id }),
        ),
        (
            format!("{}/reactions", base(cid)),
            json!({ "reaction": "clap", "participantId": pid }),
        ),
        (
            format!("{}/questions", base(cid)),
            json!({ "body": "q", "conferenceRole": "HOST" }),
        ),
        (
            format!("{}/breakouts", base(cid)),
            json!({ "count": 1, "hostId": owner.id }),
        ),
        (
            format!("{}/breakouts/move", base(cid)),
            json!({ "participantId": pid, "toBreakoutId": null, "providerIdentity": "ps_x" }),
        ),
        (
            format!("{}/media-credential", base(cid)),
            json!({ "participantSessionId": g[0].session_id, "permissionGrant": { "audioPublish": true } }),
        ),
        (
            format!("{}/polls", base(cid)),
            json!({ "title": "t", "questions": [], "ownerId": owner.id }),
        ),
        (
            format!("{}/messages", base(cid)),
            json!({ "body": "x", "participantRole": "HOST" }),
        ),
    ];
    for (uri, body) in cases {
        let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), body).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{uri}: {v}"
        );
    }
    let (s, v) = app
        .patch(
            &format!("{}/collaboration-policy", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "collaborationVersion": 1, "changes": { "godMode": true } }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, v) = app.join(cid, g[0].auth(), json!({ "role": "HOST" })).await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
}
