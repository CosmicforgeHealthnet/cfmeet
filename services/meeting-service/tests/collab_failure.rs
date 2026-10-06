//! Phase 2 Part F — failure suite F1–F10.

mod common;

use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use axum::http::StatusCode;
use backend::{
    config::FeatureFlags,
    domain::OutboxChannel,
    services::outbox,
    workers::outbox_relay::{LogPublisher, OutboxRelay},
};
use common::{collab::*, livekit_mock, rtc::*, *};
use livekit_protocol as proto;
use sea_orm::{ConnectionTrait, Database};
use serde_json::json;

fn collab_flags() -> FeatureFlags {
    FeatureFlags {
        rtc_provider_control_v2: true,
        conference_realtime_enabled: true,
        collaboration_v2_enabled: true,
        ..FeatureFlags::all_v2_enabled()
    }
}

async fn app_with(opts: TestOptions) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: collab_flags(),
        ..opts
    })
    .await
}

macro_rules! app {
    () => {
        match TestApp::collab().await {
            Some(a) => a,
            None => return,
        }
    };
}

/// F1 — LiveKit mute failure: stable provider error, no fake event.
#[tokio::test]
async fn f01_livekit_mute_failure() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    lk(&app)
        .publish(&room(cid), &g[0].identity, proto::TrackSource::Microphone)
        .unwrap();
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("{}/participants/{}/mute", base(cid), g[0].participant_id),
            Auth::Bearer(&owner.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert!(!v.to_string().contains("twirp") && !v.to_string().contains("injected"));
    assert_eq!(events(&app, cid, "participant.muted").await, 0);
    assert!(lk(&app)
        .tracks_of(&room(cid), &g[0].identity, proto::TrackSource::Microphone)
        .iter()
        .all(|(_, m)| !m));
}

/// F2 — permission update failure: the lock stays authoritative, no violating
/// credential is issued, the reconciler converges the provider.
#[tokio::test]
async fn f02_permission_update_failure() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    lk(&app).faults.fail_update.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("{}/participants/{}/audio-lock", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["audioLocked"], true);
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL");
    let (_, r) = refresh(&app, cid, p, p.auth()).await;
    let c = livekit_api::access_token::TokenVerifier::with_api_key(
        livekit_mock::API_KEY,
        livekit_mock::API_SECRET,
    )
    .verify(r["rtc"]["credential"].as_str().unwrap())
    .unwrap();
    assert!(
        !c.video
            .can_publish_sources
            .iter()
            .any(|x| x == "microphone"),
        "no violating credential"
    );
    assert!(
        lk(&app)
            .participant(&room(cid), &p.identity)
            .unwrap()
            .permission
            .can_publish_sources
            .contains(&(proto::TrackSource::Microphone as i32)),
        "provider not yet converged"
    );
    lk(&app).faults.fail_update.store(false, Ordering::SeqCst);
    let rep = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(rep.kinds.iter().any(|k| k == "PERMISSION_DRIFT"), "{rep:?}");
    assert!(lk(&app)
        .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
        .is_err());
}

/// F3 — screen-share termination failure: never claims the share stopped.
#[tokio::test]
async fn f03_stop_share_failure() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    start_share(&app, cid, &g[0]).await;
    lk(&app).faults.fail_update.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("{}/screen-share/{}/stop", base(cid), g[0].participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    lk(&app).faults.fail_update.store(false, Ordering::SeqCst);
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert_eq!(events(&app, cid, "screenshare.ended").await, 0);
    let (_, st) = app
        .get(
            &format!("{}/collaboration", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(st["activeScreenShares"], json!([g[0].participant_id]));
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='SCREEN_SHARE_STOP' AND metadata->>'result'='PROVIDER_FAILED'")).await, 1);
    // Retry after recovery succeeds.
    assert_eq!(
        app.post(
            &format!("{}/screen-share/{}/stop", base(cid), g[0].participant_id),
            Auth::Bearer(&owner.token),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
}

/// F4 — Redis down: reactions degrade (dropped, transient), chat still commits.
#[tokio::test]
async fn f04_redis_down_reaction_and_chat() {
    let Some(app) = app_with(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let (_, cid, g) = meeting(&app, public(), 2).await;
    let (s, v) = app
        .post(
            &format!("{}/reactions", base(cid)),
            g[0].auth(),
            json!({ "reaction": "clap" }),
        )
        .await;
    assert_eq!(
        (s, v["delivered"].as_bool()),
        (StatusCode::OK, Some(false)),
        "{v}"
    );
    let (s, _) = send(&app, cid, g[0].auth(), json!({ "body": "still works" })).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='chat.message_created' AND status='PENDING'")).await, 1);
    let (_, page) = messages(&app, cid, g[1].auth(), "").await;
    assert_eq!(
        page["messages"][0]["body"], "still works",
        "REST recovery is authoritative"
    );
    // Rate limits still apply without Redis (in-process window).
    let mut limited = false;
    for _ in 0..12 {
        if app
            .post(
                &format!("{}/reactions", base(cid)),
                g[1].auth(),
                json!({ "reaction": "clap" }),
            )
            .await
            .0
            == StatusCode::TOO_MANY_REQUESTS
        {
            limited = true;
        }
    }
    assert!(limited, "no unlimited abuse when Redis is down");
}

/// F5 — Redis down: durable poll / breakout / Q&A state commits; events stay durable.
#[tokio::test]
async fn f05_redis_down_durable_state() {
    let Some(app) = app_with(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert_eq!(
        vote(&app, cid, poll, g[0].auth(), "o1").await.0,
        StatusCode::CREATED
    );
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await;
    open_room(&app, cid, &owner, rooms[0]).await;
    assert_eq!(
        app.post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q" })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    for t in ["poll.started", "breakout.opened", "qna.question_created"] {
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='{t}' AND status='PENDING'")).await, 1, "{t}");
    }
    let (_, r) = app
        .get(&format!("{}/events?after=0", base(cid)), g[0].auth())
        .await;
    assert!(
        r["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["eventType"] == "qna.question_created"),
        "replay works without Redis"
    );
}

/// F6 — breakout routing with the provider unavailable: assignment stays
/// authoritative, no stale room authorization, reconciliation repairs.
#[tokio::test]
async fn f06_breakout_routing_provider_down() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let bid = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
    app.post(
        &format!("{}/breakouts/assign", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "mode": "BALANCED" }),
    )
    .await;
    lk(&app).faults.fail_create.store(true, Ordering::SeqCst);
    let v = open_room(&app, cid, &owner, bid).await;
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL", "{v}");
    let (s, r) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(
        (s, code(&r)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE"),
        "no credential for a room that does not exist"
    );
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), g[0].auth())
        .await;
    assert_eq!(
        st["me"]["mediaContext"]["breakoutId"],
        json!(bid),
        "business assignment authoritative"
    );
    // A main-room credential cannot keep the participant in main either.
    lk(&app).connect(&g[0].credential).ok();
    let (_, w) = provider_event(&app, "participant_joined", cid, Some(&g[0].identity)).await;
    assert_eq!(w["outcome"], "STALE_SESSION_EVICTED");
    lk(&app).faults.fail_create.store(false, Ordering::SeqCst);
    let rep = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(
        rep.kinds.iter().any(|k| k == "BREAKOUT_ROOM_MISSING"),
        "{rep:?}"
    );
    assert!(lk(&app).has_room(&breakout_room(bid)));
    assert_eq!(
        refresh(&app, cid, &g[0], g[0].auth()).await.0,
        StatusCode::OK
    );
}

/// F7 — countdown worker crash / restart and concurrent workers.
#[tokio::test]
async fn f07_countdown_worker_restart() {
    let app = app!();
    let (owner, cid, _) = meeting(&app, public(), 0).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 3 })).await;
    for &r in &rooms {
        open_room(&app, cid, &owner, r).await;
    }
    app.post(
        &format!("{}/breakouts/close-all", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "countdownSeconds": 600 }),
    )
    .await;
    // "Crash": no worker ran while the deadline passed.
    exec(&app, &format!("UPDATE breakout_rooms SET closing_at = now() - interval '5 seconds' WHERE conference_id='{cid}'")).await;
    let w1 = backend::workers::BreakoutCloser::new(app.state.meeting.core.clone());
    let w2 = backend::workers::BreakoutCloser::new(app.state.meeting.core.clone());
    let (a, b) = tokio::join!(w1.tick(), w2.tick());
    assert_eq!(a.unwrap() + b.unwrap(), 3, "each room closed exactly once");
    assert_eq!(events(&app, cid, "breakout.closed").await, 3);
    // Manual immediate close racing the worker is idempotent.
    let r = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
    open_room(&app, cid, &owner, r).await;
    app.post(
        &format!("{}/breakouts/{r}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "countdownSeconds": 60 }),
    )
    .await;
    exec(
        &app,
        &format!("UPDATE breakout_rooms SET closing_at = now() WHERE id='{r}'"),
    )
    .await;
    let manual = app.post(
        format!("{}/breakouts/{r}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({ "countdownSeconds": 0 }),
    );
    let (m, wk) = tokio::join!(manual, w1.tick());
    assert_eq!(m.0, StatusCode::OK);
    wk.unwrap();
    assert_eq!(events(&app, cid, "breakout.closed").await, 4);
}

/// F8 — outbox relay crash: Part F events survive and are delivered once.
#[tokio::test]
async fn f08_outbox_relay_crash() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    app.post(&format!("{}/hand-raise", base(cid)), g[0].auth(), json!({}))
        .await;
    open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    let total = app
        .scalar("SELECT count(*) FROM outbox_events WHERE channel='REALTIME' AND status='PENDING'")
        .await;
    let crashed = outbox::claim(
        &app.db.conn,
        OutboxChannel::Realtime,
        None,
        1000,
        30,
        "crashed",
    )
    .await
    .unwrap();
    assert_eq!(crashed.len() as i64, total);
    let relay = OutboxRelay::new(app.db.conn.clone(), Arc::new(LogPublisher));
    assert_eq!(relay.tick(1000).await.unwrap().0, 0);
    exec(&app, "UPDATE outbox_events SET available_at = now() - interval '1 second' WHERE locked_by='crashed'").await;
    assert_eq!(relay.tick(1000).await.unwrap().0 as i64, total);
    outbox::mark_published(&app.db.conn, crashed[0].id, "crashed")
        .await
        .unwrap();
    assert_eq!(relay.tick(1000).await.unwrap(), (0, 0));
    assert!(events(&app, cid, "hand.raised").await == 1);
}

/// F9 — duplicate provider track webhooks are applied once.
#[tokio::test]
async fn f09_duplicate_track_webhook() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 1).await;
    let sid = lk(&app)
        .publish(&room(cid), &g[0].identity, proto::TrackSource::ScreenShare)
        .unwrap();
    let body = livekit_mock::track_webhook_body(
        "track_published",
        "EV_dup_track",
        &room(cid),
        &g[0].identity,
        &sid,
        "SCREEN_SHARE",
    );
    let (_, a) = app.signed_webhook(&body).await;
    let (_, b) = app.signed_webhook(&body).await;
    assert_eq!(
        (a["outcome"].as_str(), b["outcome"].as_str()),
        (Some("APPLIED"), Some("DUPLICATE"))
    );
    // Same share reported again under a new event id: still one start.
    track_event(
        &app,
        "track_published",
        &room(cid),
        &g[0].identity,
        &sid,
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(events(&app, cid, "screenshare.started").await, 1);
    // Screen-share audio never counts as a share.
    track_event(
        &app,
        "track_unpublished",
        &room(cid),
        &g[0].identity,
        "TR_audio",
        "SCREEN_SHARE_AUDIO",
    )
    .await;
    assert_eq!(events(&app, cid, "screenshare.ended").await, 0);
}

/// F10 — database failure before commit: no collaboration state claims success.
#[tokio::test]
async fn f10_database_failure() {
    let Some(app) = app_with(TestOptions {
        db_acquire_timeout: Some(Duration::from_secs(2)),
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let admin = Database::connect(std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let db_name = app.db.url.rsplit('/').next().unwrap().to_string();
    admin
        .execute_unprepared(&format!("ALTER DATABASE {db_name} ALLOW_CONNECTIONS false"))
        .await
        .unwrap();
    admin.execute_unprepared(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{db_name}' AND pid <> pg_backend_pid()")).await.unwrap();
    let calls = lk(&app).calls.lock().unwrap().len();
    let results = vec![
        send(&app, cid, g[0].auth(), json!({ "body": "x" })).await,
        app.post(&format!("{}/hand-raise", base(cid)), g[0].auth(), json!({}))
            .await,
        app.post(
            &format!(
                "{}/participants/{}/audio-lock",
                base(cid),
                g[0].participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await,
        app.post(
            &format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q" }),
        )
        .await,
    ];
    admin
        .execute_unprepared(&format!("ALTER DATABASE {db_name} ALLOW_CONNECTIONS true"))
        .await
        .unwrap();
    for (s, v) in results {
        assert!(
            s.is_server_error() || s == StatusCode::UNAUTHORIZED,
            "{s} {v}"
        );
        assert!(!v.to_string().to_lowercase().contains("sqlx"), "{v}");
    }
    assert_eq!(
        lk(&app).calls.lock().unwrap().len(),
        calls,
        "no provider action without authority"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_messages WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND (hand_raised_at IS NOT NULL OR audio_locked_at IS NOT NULL)")).await, 0);
}
