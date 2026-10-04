//! Phase 2 Part E — failure-injection suite (spec §19).

mod common;

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use backend::{
    config::FeatureFlags,
    domain::OutboxChannel,
    services::{outbox, realtime::RedisPublisher, rtc_control::RtcControl},
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use common::{livekit_mock, rtc::*, *};
use futures_util::StreamExt;
use sea_orm::{ConnectionTrait, Database};
use serde_json::json;

fn lk_flags() -> FeatureFlags {
    FeatureFlags {
        rtc_provider_control_v2: true,
        conference_realtime_enabled: true,
        ..FeatureFlags::all_v2_enabled()
    }
}

async fn lk_app(opts: TestOptions) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: lk_flags(),
        ..opts
    })
    .await
}

/// F1. PostgreSQL down: authoritative writes fail, no credential is issued and no
/// provider action happens without authority.
#[tokio::test]
async fn f1_postgres_down() {
    let Some(app) = lk_app(TestOptions {
        db_acquire_timeout: Some(Duration::from_secs(2)),
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f1", "accessPolicy": "PUBLIC" }))
        .await;
    let admin = Database::connect(std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let db_name = app.db.url.rsplit('/').next().unwrap().to_string();
    admin
        .execute_unprepared(&format!("ALTER DATABASE {db_name} ALLOW_CONNECTIONS false"))
        .await
        .unwrap();
    admin.execute_unprepared(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{db_name}' AND pid <> pg_backend_pid()")).await.unwrap();
    let calls_before = lk(&app).calls.lock().unwrap().len();
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert!(s.is_server_error(), "{s} {v}");
    assert!(
        v.get("rtc").map(|r| r.is_null()).unwrap_or(true),
        "no credential without authority"
    );
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_server_error() || s == StatusCode::UNAUTHORIZED);
    assert_eq!(
        lk(&app).calls.lock().unwrap().len(),
        calls_before,
        "no provider call without authority"
    );
    assert!(
        lk(&app).has_room(&room(cid)),
        "room not terminated without a committed END"
    );
    admin
        .execute_unprepared(&format!("ALTER DATABASE {db_name} ALLOW_CONNECTIONS true"))
        .await
        .unwrap();
    // Authority restored: business state is exactly as committed before the outage.
    assert_eq!(conference_status(&app, cid).await, "LIVE");
}

/// F2. Redis down: business state stays correct, events stay in the outbox, realtime
/// degrades to PostgreSQL tailing, REST recovery keeps working.
#[tokio::test]
async fn f2_redis_down() {
    let Some(app) = lk_app(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f2", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    assert_eq!(g.body["rtcStatus"], "ISSUED");
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let relay = OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(RedisPublisher::new(app.state.redis.clone())) as Arc<dyn EventPublisher>),
        Some(Arc::new(RtcControl::new(app.state.meeting.core.clone()))),
    );
    let pending = app
        .scalar("SELECT count(*) FROM outbox_events WHERE channel='REALTIME' AND status='PENDING'")
        .await;
    for _ in 0..12 {
        exec(
            &app,
            "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
        )
        .await;
        relay.tick(100).await.unwrap();
    }
    assert_eq!(app.scalar("SELECT count(*) FROM outbox_events WHERE channel='REALTIME' AND status='PENDING' AND last_error IS NOT NULL").await, pending, "retained, never dropped");
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        0,
        "outage is transient, not poison"
    );
    // Realtime degrades but still delivers from PostgreSQL.
    let addr = app.serve().await;
    let (_, t) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut frames = vec![];
    let unlock_uri = format!("/api/v2/conferences/{cid}/unlock");
    let mut sent = false;
    while frames
        .iter()
        .all(|f: &serde_json::Value| f["type"] != "event")
    {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
            frames.push(serde_json::from_str(&t).unwrap());
        }
        if !sent {
            app.post(&unlock_uri, Auth::Bearer(&owner.token), json!({}))
                .await;
            sent = true;
        }
    }
    assert_eq!(frames[0]["type"], "subscribed");
    assert_eq!(frames[0]["degraded"], true);
    assert!(frames.iter().any(|f| f["type"] == "degraded"));
    assert_eq!(
        frames.last().unwrap()["event"]["eventType"],
        "conference.unlocked"
    );
    let (_, r) = app
        .get(
            &format!("/api/v2/conferences/{cid}/events?after=0"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(r["events"].as_array().unwrap().len() >= 3, "{r}");
}

/// F3. LiveKit down (or hanging) on join → stable RTC_PROVIDER_UNAVAILABLE, bounded.
#[tokio::test]
async fn f3_livekit_down_on_join() {
    let Some(app) = lk_app(TestOptions {
        rtc_timeout: Duration::from_millis(500),
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f3", "accessPolicy": "PUBLIC" }))
        .await;
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert!(v.get("rtc").is_none());
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    lk(&app).faults.delay_ms.store(3_000, Ordering::SeqCst);
    let started = Instant::now();
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "h" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded by the provider timeout"
    );
    lk(&app).faults.delay_ms.store(0, Ordering::SeqCst);
    // Business admission preserved; unusable sessions closed; never a legacy fallback token.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='ADMITTED'")).await, 2);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
    let ok = join_guest(&app, cid, "after-recovery").await;
    assert_eq!(ok.body["rtcStatus"], "ISSUED");
}

/// F4. Room creation failure keeps PREPARING (no fake readiness); recovery reconciles.
#[tokio::test]
async fn f4_room_creation_failure() {
    let Some(app) = lk_app(TestOptions::default()).await else {
        return;
    };
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "f4", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{}/conferences", uuid(&space["id"])),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid = uuid(&c["id"]);
    lk(&app).faults.fail_create.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/prepare"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["conference"]["status"], "PREPARING");
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL");
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert_eq!(conference_status(&app, cid).await, "PREPARING");
    assert_eq!(events(&app, cid, "conference.ready").await, 0);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_prepare_requested' AND status='PENDING' AND last_error IS NOT NULL")).await, 1);
    lk(&app).faults.fail_create.store(false, Ordering::SeqCst);
    let r = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(
        r.kinds.contains(&"PREPARING_NOT_READY".to_string()),
        "{r:?}"
    );
    assert_eq!(conference_status(&app, cid).await, "WAITING");
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["conference"]["status"].as_str()),
        (StatusCode::OK, Some("LIVE"))
    );
}

/// F5. Termination failure keeps ENDING, refuses joins, and is counted for alerting.
#[tokio::test]
async fn f5_termination_failure() {
    let Some(app) = lk_app(TestOptions::default()).await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f5", "accessPolicy": "PUBLIC" }))
        .await;
    let before = backend::services::rtc_metrics::RTC_METRICS
        .room_terminate_failures
        .load(Ordering::Relaxed);
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(v["conference"]["status"], "ENDING");
    assert!(
        backend::services::rtc_metrics::RTC_METRICS
            .room_terminate_failures
            .load(Ordering::Relaxed)
            > before
    );
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "x" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    // Repeated END retries termination; still honest while the provider is down.
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (
            v["conference"]["status"].as_str(),
            v["providerEnforcement"].as_str()
        ),
        (Some("ENDING"), Some("PENDING_PROVIDER_CONTROL"))
    );
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (
            v["conference"]["status"].as_str(),
            v["providerEnforcement"].as_str()
        ),
        (Some("ENDED"), Some("ENFORCED"))
    );
}

/// F6. Delayed webhook: the reconciler repairs operational state; the late callback is
/// then a harmless no-op.
#[tokio::test]
async fn f6_webhook_delayed() {
    let Some(app) = lk_app(TestOptions::default()).await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f6", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap();
    assert_eq!(session_state(&app, p.session_id).await, "PENDING");
    backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert_eq!(session_state(&app, p.session_id).await, "CONNECTED");
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
    let (_, v) = provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
    assert_eq!(v["outcome"], "APPLIED");
    assert_eq!(
        events(&app, cid, "participant.joined").await,
        1,
        "no duplicate transition"
    );
}

/// F7. Outbox worker crash: leased rows survive and are delivered exactly once after
/// the lease expires; a zombie worker cannot complete rows it no longer holds.
#[tokio::test]
async fn f7_outbox_worker_crash() {
    let Some(app) = lk_app(TestOptions::default()).await else {
        return;
    };
    let owner = app.user("owner").await;
    app.live_conference(&owner, json!({ "title": "f7", "accessPolicy": "PUBLIC" }))
        .await;
    let total = app
        .scalar("SELECT count(*) FROM outbox_events WHERE channel='REALTIME' AND status='PENDING'")
        .await;
    // Worker A claims everything and "crashes".
    let crashed = outbox::claim(
        &app.db.conn,
        OutboxChannel::Realtime,
        None,
        1000,
        30,
        "crashed-worker",
    )
    .await
    .unwrap();
    assert_eq!(crashed.len() as i64, total);
    let relay = OutboxRelay::new(
        app.db.conn.clone(),
        Arc::new(backend::workers::outbox_relay::LogPublisher),
    );
    assert_eq!(
        relay.tick(1000).await.unwrap().0,
        0,
        "leased rows are invisible"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING'")
            .await,
        total,
        "events survive the crash"
    );
    exec(&app, "UPDATE outbox_events SET available_at = now() - interval '1 second' WHERE locked_by = 'crashed-worker'").await;
    assert_eq!(relay.tick(1000).await.unwrap().0 as i64, total);
    // The zombie finally tries to complete: no effect, nothing double-published.
    outbox::mark_published(&app.db.conn, crashed[0].id, "crashed-worker")
        .await
        .unwrap();
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM outbox_events WHERE channel='REALTIME' AND status='PUBLISHED'"
        )
        .await,
        total
    );
    assert_eq!(relay.tick(1000).await.unwrap(), (0, 0));
}

/// F8. Reconciler crash: the lease blocks a second instance until it expires, then
/// recovery proceeds safely.
#[tokio::test]
async fn f8_reconciler_crash() {
    let Some(app) = lk_app(TestOptions::default()).await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f8", "accessPolicy": "PUBLIC" }))
        .await;
    let crashed = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    assert!(!crashed.tick().await.unwrap().skipped_lease);
    drop(crashed); // crash: lease never released
    lk(&app).drop_room(&room(cid));
    let survivor = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    assert!(survivor.tick().await.unwrap().skipped_lease);
    assert!(!lk(&app).has_room(&room(cid)));
    exec(
        &app,
        "UPDATE worker_leases SET expires_at = now() - interval '1 second'",
    )
    .await;
    let r = survivor.tick().await.unwrap();
    assert!(
        !r.skipped_lease && r.kinds.contains(&"LIVE_ROOM_MISSING".to_string()),
        "{r:?}"
    );
    assert!(lk(&app).has_room(&room(cid)));
}

/// F9. No insecure fallback: with provider control off a join gets no credential at
/// all (never a legacy all-powerful token), and a provider failure never degrades to one.
#[tokio::test]
async fn f9_no_insecure_fallback() {
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: FeatureFlags {
            rtc_provider_control_v2: false,
            ..FeatureFlags::all_v2_enabled()
        },
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f9", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["rtcStatus"], "PROVIDER_CONTROL_DISABLED");
    assert!(v["rtc"].is_null());
    assert!(
        lk(&app).room_names().is_empty(),
        "no provider control at all"
    );
    let body = livekit_mock::webhook_body("room_started", "EV_f9", &room(cid), None, None);
    assert_eq!(app.signed_webhook(&body).await.0, StatusCode::NOT_FOUND);
}
