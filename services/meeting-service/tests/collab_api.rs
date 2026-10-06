//! Phase 2 Part F — API matrix, database invariants, pre-join contract and realtime
//! audience tests.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{collab::*, rtc::*, *};
use futures_util::StreamExt;
use livekit_protocol as proto;
use sea_orm::ConnectionTrait;
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

// ------------------------------ API matrix --------------------------------

/// Every major endpoint: unauthenticated, wrong role, foreign conference, unknown
/// fields and malformed body.
#[tokio::test]
async fn api_matrix_auth_role_scope_shape() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let (_, other) = app
        .live_conference(&app.user("other").await, public())
        .await;
    let pid = g[1].participant_id;
    let bid = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[1].auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let qid = uuid(&q["id"]);
    // (path suffix, valid body, participant-forbidden?)
    let endpoints: Vec<(String, Value, bool)> = vec![
        (format!("participants/{pid}/audio-lock"), json!({}), true),
        (format!("participants/{pid}/video-lock"), json!({}), true),
        (
            format!("participants/{pid}/screen-share-lock"),
            json!({}),
            true,
        ),
        ("spotlights".into(), json!({ "participantId": pid }), true),
        (
            format!("participants/{pid}/chat-restriction"),
            json!({}),
            true,
        ),
        ("breakouts".into(), json!({ "count": 1 }), true),
        (
            "breakouts/assign".into(),
            json!({ "mode": "BALANCED" }),
            true,
        ),
        (format!("breakouts/{bid}/open"), json!({}), true),
        (
            "breakouts/move".into(),
            json!({ "participantId": pid, "toBreakoutId": bid }),
            true,
        ),
        ("polls".into(), simple_poll("AFTER_VOTE"), true),
        (format!("polls/{poll}/close"), json!({}), true),
        (format!("questions/{qid}/answer"), json!({}), true),
        (format!("questions/{qid}/pin"), json!({}), true),
        ("messages".into(), json!({ "body": "x" }), false),
        ("reactions".into(), json!({ "reaction": "clap" }), false),
        ("questions".into(), json!({ "body": "Q2" }), false),
    ];
    for (suffix, body, host_only) in &endpoints {
        let uri = format!("{}/{suffix}", base(cid));
        let (s, v) = app.post(&uri, Auth::None, body.clone()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED"),
            "unauth {suffix}: {v}"
        );
        if *host_only {
            let (s, v) = app.post(&uri, g[0].auth(), body.clone()).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "wrong role {suffix}: {v}");
        }
        // Endpoints with a request DTO reject unknown fields and malformed JSON
        // (body-less commands ignore any body).
        if body.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            let mut extra = body.clone();
            extra["unexpectedField"] = json!(1);
            let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), extra).await;
            assert_eq!(
                (s, code(&v)),
                (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
                "unknown field {suffix}: {v}"
            );
            let (s, v) = app
                .post_raw(&uri, Auth::Bearer(&owner.token), "{not json")
                .await;
            assert_eq!(
                (s, code(&v)),
                (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
                "malformed {suffix}: {v}"
            );
        }
        // A stranger to the conference.
        let stranger = app
            .user(&format!("stranger-{}", Uuid::new_v4().simple()))
            .await;
        let (s, _) = app
            .post(&uri, Auth::Bearer(&stranger.token), body.clone())
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "stranger {suffix}");
    }
    // Foreign conference scoping.
    for suffix in [
        format!("participants/{pid}/audio-lock"),
        format!("breakouts/{bid}/open"),
        format!("polls/{poll}/close"),
        format!("questions/{qid}/pin"),
    ] {
        let other_owner_token = &owner.token;
        let (s, _) = app
            .post(
                &format!("{}/{suffix}", base(other)),
                Auth::Bearer(other_owner_token),
                json!({}),
            )
            .await;
        assert!(
            s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
            "foreign {suffix}: {s}"
        );
    }
}

/// Invalid state: nothing interactive after the conference ended; idempotent repeats.
#[tokio::test]
async fn api_invalid_state_and_idempotency() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let pid = g[0].participant_id;
    let bid = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
    open_room(&app, cid, &owner, bid).await;
    let (_, again) = app
        .post(
            &format!("{}/breakouts/{bid}/open", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(again["changed"], false);
    app.post(
        &format!("{}/breakouts/{bid}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "countdownSeconds": 0 }),
    )
    .await;
    let (s, v) = app
        .post(
            &format!("{}/breakouts/{bid}/close", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "countdownSeconds": 0 }),
        )
        .await;
    assert_eq!(
        (s, v["breakouts"][0]["status"].as_str()),
        (StatusCode::OK, Some("CLOSED")),
        "closing a closed breakout is a no-op"
    );
    let (s, v) = app
        .post(
            &format!("{}/breakouts/{bid}/open", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "BREAKOUT_CLOSED"),
        "never reopened"
    );
    let (_, l) = app
        .delete(
            &format!("{}/participants/{pid}/audio-lock", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(l["changed"], false, "unlocking an unlocked participant");
    let (_, h) = app
        .delete(&format!("{}/hand-raise", base(cid)), g[0].auth())
        .await;
    assert_eq!(h["changed"], false, "lowering a lowered hand");
    app.post(
        &format!("{}/end", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    for (uri, body) in [
        (format!("{}/messages", base(cid)), json!({ "body": "late" })),
        (format!("{}/hand-raise", base(cid)), json!({})),
        (
            format!("{}/reactions", base(cid)),
            json!({ "reaction": "clap" }),
        ),
        (
            format!("{}/questions", base(cid)),
            json!({ "body": "late" }),
        ),
    ] {
        let (s, v) = app.post(&uri, g[0].auth(), body).await;
        assert!(
            s == StatusCode::CONFLICT || s == StatusCode::FORBIDDEN,
            "{uri}: {s} {v}"
        );
    }
    let (s, v) = app
        .post(
            &format!("{}/breakouts", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "count": 1 }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    // Historical reads remain available to authorized users.
    assert_eq!(
        app.get(&format!("{}/polls", base(cid)), Auth::Bearer(&owner.token))
            .await
            .0,
        StatusCode::OK
    );
}

/// Provider unavailable on media endpoints → stable 503.
#[tokio::test]
async fn api_provider_unavailable() {
    use std::sync::atomic::Ordering;
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (s, v) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    // Chat / polls / Q&A are unaffected by a provider outage.
    assert_eq!(
        send(&app, cid, g[0].auth(), json!({ "body": "ok" }))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        app.post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "ok" })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
}

/// With FEATURE_COLLABORATION_V2_ENABLED off, Part F endpoints are unavailable and
/// A–E behaviour is unchanged.
#[tokio::test]
async fn api_feature_flag_off() {
    let Some(app) = TestApp::livekit().await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, public()).await;
    let j = join_guest(&app, cid, "g").await;
    connect(&app, cid, &j).await;
    for uri in [
        "messages",
        "collaboration",
        "breakouts",
        "polls",
        "questions",
        "pre-join",
        "hand-raise",
    ] {
        let (s, v) = app
            .get(&format!("{}/{uri}", base(cid)), Auth::Bearer(&owner.token))
            .await;
        assert!(
            (s == StatusCode::NOT_FOUND && code(&v) == "FEATURE_DISABLED")
                || s == StatusCode::METHOD_NOT_ALLOWED,
            "{uri}: {s} {v}"
        );
    }
    let (s, v) = app
        .post(
            &format!("{}/participants/{}/audio-lock", base(cid), j.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "FEATURE_DISABLED"));
    assert!(
        lk(&app)
            .publish(&room(cid), &j.identity, proto::TrackSource::ScreenShare)
            .is_ok(),
        "Part E grants unchanged"
    );
}

// ------------------------------ database ----------------------------------

async fn sql_err(app: &TestApp, sql: &str) -> bool {
    app.db.conn.execute_unprepared(sql).await.is_err()
}

/// Constraints, indexes and privacy-preserving storage.
#[tokio::test]
async fn db_constraints() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    app.post(&format!("{}/breakouts/assign", base(cid)), Auth::Bearer(&owner.token), json!({ "mode": "MANUAL", "assignments": [{ "participantId": g[0].participant_id, "breakoutId": rooms[0] }] })).await;
    let p = g[0].participant_id;
    assert!(sql_err(&app, &format!("INSERT INTO breakout_assignments (id, breakout_room_id, conference_id, participant_id) VALUES (gen_random_uuid(), '{}', '{cid}', '{p}')", rooms[1])).await, "one active assignment");
    assert!(
        sql_err(
            &app,
            &format!(
                "UPDATE breakout_rooms SET status='CLOSING' WHERE id='{}'",
                rooms[1]
            )
        )
        .await,
        "CLOSING needs a deadline"
    );
    assert!(sql_err(&app, &format!("INSERT INTO breakout_rooms (id, conference_id, name) VALUES (gen_random_uuid(), '{cid}', 'room 1')")).await, "active names unique (case-insensitive)");
    // Cross-conference assignment impossible (composite FK).
    let (_, other) = app.live_conference(&app.user("o2").await, public()).await;
    assert!(sql_err(&app, &format!("INSERT INTO breakout_assignments (id, breakout_room_id, conference_id, participant_id) VALUES (gen_random_uuid(), '{}', '{other}', '{p}')", rooms[1])).await);
    // Messages: private and breakout scope are exclusive; no self-DMs; body bounds.
    assert!(sql_err(&app, &format!("INSERT INTO conference_messages (id, conference_id, breakout_room_id, sender_participant_id, recipient_participant_id, body) VALUES (gen_random_uuid(), '{cid}', '{}', '{p}', '{}', 'x')", rooms[0], g[1].participant_id)).await);
    assert!(sql_err(&app, &format!("INSERT INTO conference_messages (id, conference_id, sender_participant_id, recipient_participant_id, body) VALUES (gen_random_uuid(), '{cid}', '{p}', '{p}', 'x')")).await);
    assert!(sql_err(&app, &format!("INSERT INTO conference_messages (id, conference_id, sender_participant_id, body) VALUES (gen_random_uuid(), '{cid}', '{p}', '')")).await);
    // Polls: status / visibility checks; one ballot per participant; vote uniqueness.
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert!(
        sql_err(
            &app,
            &format!("UPDATE meeting_polls SET status='REOPENED' WHERE id='{poll}'")
        )
        .await
    );
    assert!(
        sql_err(
            &app,
            &format!("UPDATE meeting_polls SET results_visibility='EVERYONE' WHERE id='{poll}'")
        )
        .await
    );
    vote(&app, cid, poll, g[0].auth(), "o1").await;
    assert!(sql_err(&app, &format!("INSERT INTO meeting_poll_ballots (poll_id, conference_id, participant_id) VALUES ('{poll}', '{cid}', '{p}')")).await);
    let (_, q) = app
        .post(
            &format!("{}/questions", base(cid)),
            g[1].auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let qid = q["id"].as_str().unwrap();
    app.post(
        &format!("{}/questions/{qid}/vote", base(cid)),
        g[0].auth(),
        json!({}),
    )
    .await;
    assert!(sql_err(&app, &format!("INSERT INTO meeting_question_votes (id, question_id, conference_id, participant_id) VALUES (gen_random_uuid(), '{qid}', '{cid}', '{p}')")).await);
    assert!(
        sql_err(
            &app,
            &format!("UPDATE meeting_questions SET status='DELETED' WHERE id='{qid}'")
        )
        .await
    );
    // Indexes for hot paths exist.
    for idx in [
        "idx_participants_raised_hands",
        "idx_messages_room_cursor",
        "idx_breakout_closing_due",
        "idx_polls_conference",
        "idx_questions_conference",
        "uq_breakout_name_active",
    ] {
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM pg_indexes WHERE indexname='{idx}'"
            ))
            .await,
            1,
            "{idx}"
        );
    }
}

/// Media locks, hand raise ordering and spotlight persist; outbox is atomic with state.
#[tokio::test]
async fn db_persistence_and_outbox_atomicity() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    app.post(
        &format!(
            "{}/participants/{}/video-lock",
            base(cid),
            g[0].participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    app.post(
        &format!("{}/spotlights", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "participantId": g[1].participant_id }),
    )
    .await;
    for p in g.iter().rev() {
        app.post(&format!("{}/hand-raise", base(cid)), p.auth(), json!({}))
            .await;
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND video_locked_at IS NOT NULL AND video_locked_by IS NOT NULL", g[0].participant_id)).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND spotlighted_at IS NOT NULL", g[1].participant_id)).await, 1);
    let first = app.db.conn.query_one(sea_orm::Statement::from_string(sea_orm::DbBackend::Postgres, format!(
        "SELECT id FROM conference_participants WHERE conference_id='{cid}' AND hand_raised_at IS NOT NULL ORDER BY hand_raised_at, id LIMIT 1"))).await.unwrap().unwrap();
    assert_eq!(
        first.try_get_by_index::<Uuid>(0).unwrap(),
        g[2].participant_id,
        "oldest raise first"
    );
    // A rejected action writes neither state nor events.
    let before = app.scalar("SELECT count(*) FROM outbox_events").await;
    let (s, _) = app
        .post(
            &format!(
                "{}/participants/{}/video-lock",
                base(cid),
                g[1].participant_id
            ),
            g[2].auth(),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events").await,
        before
    );
    // Each committed change has its event in the same transaction (seq is gap-free).
    let n = app
        .scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME'"
        ))
        .await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT max(conference_seq) FROM outbox_events WHERE conference_id='{cid}'"
        ))
        .await,
        n
    );
}

// ------------------------------ pre-join ----------------------------------

#[tokio::test]
async fn prejoin_contract() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, json!({ "title": "Town call", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "123456", "waitingRoomPolicy": "EVERYONE" })).await;
    let (s, v) = app
        .get(&format!("{}/pre-join", base(cid)), Auth::None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["title"], "Town call");
    assert_eq!(v["joinable"], true);
    assert_eq!(v["access"]["passcodeRequired"], true);
    assert_eq!(v["access"]["waitingRoomExpected"], true);
    assert_eq!(v["displayName"]["editable"], true);
    assert_eq!(v["displayName"]["identityBearing"], false);
    assert_eq!(v["devices"]["deviceIdentifiersCollected"], false);
    assert_eq!(v["media"]["role"], "PARTICIPANT");
    assert!(v["organizer"]["displayName"].is_string());
    assert!(!v.to_string().contains(&owner.email), "no organizer email");
    let (_, owner_view) = app
        .get(
            &format!("{}/pre-join", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(owner_view["access"]["alreadyAuthorized"], true);
    assert_eq!(owner_view["access"]["waitingRoomExpected"], false);
    // Sensitive meetings hide title and organizer from unauthorized callers.
    let (_, hc) = app.live_conference(&owner, json!({ "title": "Dr visit", "accessPolicy": "INVITE_ONLY", "securityProfile": "HEALTHCARE" })).await;
    let (_, v) = app.get(&format!("{}/pre-join", base(hc)), Auth::None).await;
    assert!(v["title"].is_null() && v["organizer"].is_null(), "{v}");
    assert_eq!(v["access"]["invitationRequired"], true);
    assert!(!v.to_string().contains("Dr visit"));
}

#[tokio::test]
async fn prejoin_consent_and_display_name() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, json!({
        "title": "Recorded", "accessPolicy": "PUBLIC",
        "collaborationPolicy": { "requiredConsents": ["RECORDING", "AI_PROCESSING"], "consentVersion": "2026-10", "aiNotice": true }
    })).await;
    let (_, v) = app
        .get(&format!("{}/pre-join", base(cid)), Auth::None)
        .await;
    assert_eq!(
        v["consents"]["required"],
        json!(["RECORDING", "AI_PROCESSING"])
    );
    assert_eq!(v["consents"]["version"], "2026-10");
    assert_eq!(v["notices"]["aiNotice"], true);
    let (s, e) = app
        .join(cid, Auth::None, json!({ "displayName": "Ann" }))
        .await;
    assert_eq!((s, code(&e)), (StatusCode::FORBIDDEN, "CONSENT_REQUIRED"));
    let (s, e) = app.join(cid, Auth::None, json!({ "displayName": "Ann", "consents": [{ "type": "RECORDING", "version": "old" }, { "type": "AI_PROCESSING", "version": "2026-10" }] })).await;
    assert_eq!(
        (s, code(&e)),
        (StatusCode::FORBIDDEN, "CONSENT_REQUIRED"),
        "stale version does not count"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        0,
        "no admission without consent"
    );
    let (s, j) = app.join(cid, Auth::None, json!({ "displayName": "Ann", "consents": [{ "type": "RECORDING", "version": "2026-10" }, { "type": "AI_PROCESSING", "version": "2026-10" }] })).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_consents WHERE conference_id='{cid}' AND decision='GRANTED' AND consent_version='2026-10'")).await, 2);
    // Rejoin needs no re-consent.
    let token = j["participantToken"].as_str().unwrap();
    let (s, _) = app.join(cid, Auth::Participant(token), json!({})).await;
    assert_eq!(s, StatusCode::OK);
    // Display name never becomes identity: sensitive meetings use the profile name.
    let user = app.user("realname").await;
    let (_, hc) = app.live_conference(&owner, json!({ "title": "HC", "accessPolicy": "AUTHENTICATED", "securityProfile": "HEALTHCARE" })).await;
    let (_, pre) = app
        .get(&format!("{}/pre-join", base(hc)), Auth::Bearer(&user.token))
        .await;
    assert_eq!(pre["displayName"]["editable"], false);
    let (s, j) = app
        .join(
            hc,
            Auth::Bearer(&user.token),
            json!({ "displayName": "Dr. Owner" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_ne!(j["participant"]["displayName"], "Dr. Owner");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{hc}' AND user_id='{}'", owner.id)).await, 0, "never another user's participant");
}

// ------------------------------ realtime ----------------------------------

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws(app: &TestApp, addr: std::net::SocketAddr, cid: Uuid, auth: Auth<'_>) -> Ws {
    let (s, t) = app
        .post(&format!("{}/realtime/ticket", base(cid)), auth, json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    let mut w = tokio_tungstenite::connect_async(url).await.unwrap().0;
    collect(&mut w, |f| f["type"] == "subscribed").await;
    w
}

/// Frames until `pred` matches (panics after 8 s).
async fn collect(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut out = vec![];
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(8), ws.next())
            .await
            .expect("expected frame")
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

fn types(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|f| f["event"]["eventType"].as_str().map(String::from))
        .collect()
}

/// Audience filtering on the live stream: private chat, breakout chat, manager-only
/// events, media lock, provider screen-share evidence, poll visibility, Q&A, closure.
#[tokio::test]
async fn realtime_audiences() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let addr = app.serve().await;
    let mut wa = ws(&app, addr, cid, g[0].auth()).await;
    let mut wb = ws(&app, addr, cid, g[1].auth()).await;
    let mut wc = ws(&app, addr, cid, g[2].auth()).await;
    let mut wh = ws(&app, addr, cid, Auth::Bearer(&owner.token)).await;
    // Private chat A → B; then a public marker so C's stream can be checked up to it.
    send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "psst", "recipientParticipantId": g[1].participant_id }),
    )
    .await;
    send(&app, cid, g[0].auth(), json!({ "body": "MARK1" })).await;
    let fb = collect(&mut wb, |f| f["event"]["payload"]["body"] == "MARK1").await;
    assert!(fb.iter().any(|f| f["event"]["payload"]["body"] == "psst"));
    let fc = collect(&mut wc, |f| f["event"]["payload"]["body"] == "MARK1").await;
    assert!(
        !fc.iter().any(|f| f["event"]["payload"]["body"] == "psst"),
        "C never receives the private message"
    );
    let fh = collect(&mut wh, |f| f["event"]["payload"]["body"] == "MARK1").await;
    assert!(
        !fh.iter().any(|f| f["event"]["payload"]["body"] == "psst"),
        "nor does the host"
    );
    collect(&mut wa, |f| f["event"]["payload"]["body"] == "MARK1").await;
    // Media lock + provider screen-share evidence reach everyone.
    app.post(
        &format!(
            "{}/participants/{}/audio-lock",
            base(cid),
            g[2].participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    start_share(&app, cid, &g[0]).await;
    let fb = collect(&mut wb, |f| {
        f["event"]["eventType"] == "screenshare.started"
    })
    .await;
    assert!(types(&fb).contains(&"participant.audio_locked".to_string()));
    assert!(
        !fb.last().unwrap().to_string().contains(&g[0].identity),
        "no provider identity"
    );
    // Poll: response events only to the voter; host-only results only to managers.
    let poll = open_poll(&app, cid, &owner, simple_poll("HOST_ONLY")).await;
    vote(&app, cid, poll, g[1].auth(), "o1").await;
    send(&app, cid, g[0].auth(), json!({ "body": "MARK2" })).await;
    let fb = collect(&mut wb, |f| f["event"]["payload"]["body"] == "MARK2").await;
    assert!(types(&fb).contains(&"poll.response_recorded".to_string()));
    assert!(
        !types(&fb).contains(&"poll.results_updated".to_string()),
        "participants never get host-only results"
    );
    let fc = collect(&mut wc, |f| f["event"]["payload"]["body"] == "MARK2").await;
    assert!(
        !types(&fc).contains(&"poll.response_recorded".to_string()),
        "not someone else's receipt"
    );
    let fh = collect(&mut wh, |f| f["event"]["payload"]["body"] == "MARK2").await;
    assert!(types(&fh).contains(&"poll.results_updated".to_string()));
    // Breakouts: A in room 1, B in room 2; breakout chat stays inside.
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    app.post(&format!("{}/breakouts/assign", base(cid)), Auth::Bearer(&owner.token), json!({ "mode": "MANUAL", "assignments": [
        { "participantId": g[0].participant_id, "breakoutId": rooms[0] }, { "participantId": g[1].participant_id, "breakoutId": rooms[1] } ] })).await;
    open_room(&app, cid, &owner, rooms[0]).await;
    open_room(&app, cid, &owner, rooms[1]).await;
    collect(&mut wa, |f| {
        f["event"]["eventType"] == "breakout.participant_moved"
    })
    .await;
    collect(&mut wb, |f| {
        f["event"]["eventType"] == "breakout.participant_moved"
    })
    .await;
    send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "room1-secret", "breakoutId": rooms[0] }),
    )
    .await;
    send(
        &app,
        cid,
        g[1].auth(),
        json!({ "body": "MARK3", "breakoutId": rooms[1] }),
    )
    .await;
    let fb = collect(&mut wb, |f| f["event"]["payload"]["body"] == "MARK3").await;
    assert!(
        !fb.iter()
            .any(|f| f["event"]["payload"]["body"] == "room1-secret"),
        "breakout chat isolated"
    );
    let fa = collect(&mut wa, |f| f["event"]["payload"]["body"] == "room1-secret").await;
    assert!(!fa.iter().any(|f| f["event"]["payload"]["body"] == "MARK3"));
    // Announcement + countdown + closure reach breakout members.
    app.post(
        &format!("{}/breakouts/announcement", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "message": "wrap up" }),
    )
    .await;
    collect(&mut wa, |f| {
        f["event"]["eventType"] == "breakout.announcement"
    })
    .await;
    collect(&mut wb, |f| {
        f["event"]["eventType"] == "breakout.announcement"
    })
    .await;
    app.post(
        &format!("{}/breakouts/close-all", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "countdownSeconds": 5 }),
    )
    .await;
    collect(&mut wa, |f| f["event"]["eventType"] == "breakout.closing").await;
    exec(
        &app,
        &format!("UPDATE breakout_rooms SET closing_at = now() WHERE conference_id='{cid}'"),
    )
    .await;
    backend::workers::BreakoutCloser::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    let fa = collect(&mut wa, |f| {
        f["event"]["eventType"] == "breakout.participant_moved"
            && f["event"]["payload"]["reason"] == "BREAKOUT_CLOSED"
    })
    .await;
    assert!(types(&fa).contains(&"breakout.closed".to_string()));
    // Back in main: A receives main-room chat again; Q&A events flow to all.
    app.post(
        &format!("{}/questions", base(cid)),
        g[2].auth(),
        json!({ "body": "Q?" }),
    )
    .await;
    collect(&mut wa, |f| {
        f["event"]["eventType"] == "qna.question_created"
    })
    .await;
    send(&app, cid, g[2].auth(), json!({ "body": "welcome back" })).await;
    collect(&mut wa, |f| f["event"]["payload"]["body"] == "welcome back").await;
}
