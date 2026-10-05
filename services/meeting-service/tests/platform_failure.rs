//! Part L — global failure certification (L-F1..L-F12, §59–§67).
//!
//! Real dependency failures are injected (PostgreSQL connections refused, Redis
//! unreachable, mock LiveKit RoomService / Egress faults, crashed worker leases) and the
//! public responses are checked for false success and for leaked internals.

mod common;

use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use axum::http::{Method, StatusCode};
use backend::{
    services::{realtime::RedisPublisher, rtc_control::RtcControl},
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use chrono::Utc;
use common::{artifacts as art, contract::*, livekit_mock, rtc::*, *};
use futures_util::StreamExt;
use sea_orm::{ConnectionTrait, Database};
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

fn ghi(opts: TestOptions) -> TestOptions {
    TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        ..opts
    }
}

async fn set_db_connections(app: &TestApp, allow: bool) {
    let admin = Database::connect(std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let db_name = app.db.url.rsplit('/').next().unwrap().to_string();
    admin
        .execute_unprepared(&format!(
            "ALTER DATABASE {db_name} ALLOW_CONNECTIONS {allow}"
        ))
        .await
        .unwrap();
    if !allow {
        admin
            .execute_unprepared(&format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
            ))
            .await
            .unwrap();
    }
    admin.close().await.ok();
}

/// L-F1: PostgreSQL unavailable → every state-changing operation is refused with a
/// stable, retryable error; no credential, role grant, recording start, artefact
/// mutation or invitation use happens; nothing falls back to Redis.
#[tokio::test]
async fn l_f01_postgres_unavailable() {
    let app = app!(TestApp::with(ghi(TestOptions {
        db_acquire_timeout: Some(Duration::from_secs(2)),
        ..TestOptions::default()
    })));
    let doc = openapi();
    let (owner, sid, cid, guests) = art::meeting(&app, 1).await;
    let (rid, _) = art::recording_ready(&app, cid, &owner).await;
    let (_, inv) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "role": "PARTICIPANT", "conferenceId": cid }),
        )
        .await;
    let token = inv["token"].as_str().unwrap().to_string();
    let provider_calls = lk(&app).calls.lock().unwrap().len();
    let sessions_before = app
        .scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}'"
        ))
        .await;
    set_db_connections(&app, false).await;
    let bearer = format!("Bearer {}", owner.token);
    let gtoken = guests[0].participant_token.clone().unwrap();
    let invited = format!(r#"{{"displayName":"i","invitationToken":"{token}"}}"#);
    type Case<'a> = (Method, String, Vec<(&'a str, &'a str)>, &'a str);
    let cases: Vec<Case<'_>> = vec![
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            vec![],
            r#"{"displayName":"x"}"#,
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            vec![("x-participant-token", gtoken.as_str())],
            r#"{"displayName":"g"}"#,
        ),
        (
            Method::PATCH,
            format!(
                "/api/v2/conferences/{cid}/participants/{}/role",
                guests[0].participant_id
            ),
            vec![("authorization", bearer.as_str())],
            r#"{"role":"HOST"}"#,
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/recordings"),
            vec![("authorization", bearer.as_str())],
            "{}",
        ),
        (
            Method::DELETE,
            format!("/api/v2/conferences/{cid}/artifacts/{rid}"),
            vec![("authorization", bearer.as_str())],
            "",
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            vec![("authorization", bearer.as_str())],
            "{}",
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            vec![],
            invited.as_str(),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/end"),
            vec![("authorization", bearer.as_str())],
            "{}",
        ),
    ];
    let mut unavailable = 0;
    for (m, uri, headers, body) in cases {
        let body = (!body.is_empty()).then_some(body);
        let (s, h, v) = raw(&app, m.clone(), &uri, &headers, body).await;
        assert!(
            s.is_server_error() || s == StatusCode::UNAUTHORIZED,
            "{m} {uri}: {s} {v}"
        );
        if s.is_server_error() {
            assert_error_contract(&doc, s, &h, &v);
            assert!(
                matches!(code(&v), "SERVICE_UNAVAILABLE" | "DATABASE_ERROR"),
                "{uri}: {v}"
            );
            if code(&v) == "SERVICE_UNAVAILABLE" {
                assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
                unavailable += 1;
            }
        }
        assert!(v.get("rtc").is_none() && v.get("url").is_none(), "{v}");
        assert_no_secrets(&app, &uri, &v);
    }
    assert!(
        unavailable > 0,
        "connectivity loss is reported as retryable 503"
    );
    assert_eq!(
        lk(&app).calls.lock().unwrap().len(),
        provider_calls,
        "no provider action without authority"
    );
    set_db_connections(&app, true).await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}'"
        ))
        .await,
        sessions_before,
        "no session / credential without authoritative state"
    );
    // Nothing was decided while the authority was down.
    let after = |sql: String| {
        let app = &app;
        async move { app.scalar(&sql).await }
    };
    assert_eq!(
        after(format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND role='PARTICIPANT'",
            guests[0].participant_id
        ))
        .await,
        1
    );
    assert_eq!(after(format!("SELECT count(*) FROM meeting_invitations WHERE meeting_space_id='{sid}' AND usage_count = 0")).await, 1);
    assert_eq!(after(format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND artifact_type='RECORDING'")).await, 1);
    assert_eq!(art::artifact_status(&app, rid).await, "READY");
    assert_eq!(conference_status(&app, cid).await, "LIVE");
}

/// L-F2: Redis / realtime unavailable → REST authority continues, committed state is
/// correct, durable events stay PENDING (never dropped), REST replay works, an alert
/// metric is raised; Redis never grants anything.
#[tokio::test]
async fn l_f02_redis_unavailable() {
    let app = app!(TestApp::with(ghi(TestOptions {
        redis: RedisMode::Unreachable,
        ..TestOptions::default()
    })));
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f2", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    assert_eq!(g.body["rtcStatus"], "ISSUED");
    let (s, v) = change_role(&app, cid, &owner, g.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let stranger = app.user("stranger").await;
    let (s, _) = change_role(&app, cid, &stranger, g.participant_id, "HOST").await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "no authority from a degraded cache"
    );
    let failures_before = backend::services::rtc_metrics::RTC_METRICS
        .realtime_publish_failures
        .load(Ordering::Relaxed)
        + backend::services::rtc_metrics::RTC_METRICS
            .outbox_retried
            .load(Ordering::Relaxed);
    let relay = OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(RedisPublisher::new(app.state.redis.clone())) as Arc<dyn EventPublisher>),
        Some(Arc::new(RtcControl::new(app.state.meeting.core.clone()))),
    );
    let pending = app
        .scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"
        ))
        .await;
    assert!(pending > 0);
    for _ in 0..3 {
        exec(
            &app,
            "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
        )
        .await;
        relay.tick(100).await.unwrap();
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"
        ))
        .await,
        pending,
        "durable events retained"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        0
    );
    let failures_after = backend::services::rtc_metrics::RTC_METRICS
        .realtime_publish_failures
        .load(Ordering::Relaxed)
        + backend::services::rtc_metrics::RTC_METRICS
            .outbox_retried
            .load(Ordering::Relaxed);
    assert!(failures_after > failures_before, "operations metric raised");
    let (s, r) = app
        .get(
            format!("/api/v2/conferences/{cid}/events?after=0"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let replayed = r["events"].as_array().unwrap();
    assert!(!replayed.is_empty(), "{r}");
    assert!(
        replayed
            .iter()
            .any(|e| e["eventType"].as_str().unwrap_or("").contains("role")),
        "committed role change recoverable over REST: {r}"
    );
    let (_, c) = app
        .get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(c["status"], "LIVE");
    // Recording state still advances only on provider evidence through PostgreSQL.
    let (rid, _) = art::recording_active(&app, cid, &owner).await;
    assert_eq!(art::artifact_status(&app, rid).await, "RECORDING");
}

/// L-F3: LiveKit unavailable during join → stable retryable error, no connected
/// session, no media claim, authorized state preserved, safe retry.
#[tokio::test]
async fn l_f03_livekit_unavailable_during_join() {
    let app = app!(TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        rtc_timeout: Duration::from_millis(500),
        flags: TestApp::ghi_flags(),
        ..TestOptions::default()
    }));
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f3", "accessPolicy": "PUBLIC" }))
        .await;
    let member = app.user("member").await;
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, "RTC_PROVIDER_UNAVAILABLE")
    );
    assert!(v.get("rtc").is_none());
    assert_no_secrets(&app, "join outage", &v);
    assert!(!v.to_string().to_lowercase().contains("livekit"), "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')"
        ))
        .await,
        0,
        "no session claims media"
    );
    let pid_before = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND user_id='{}' AND admission_state='ADMITTED'",
            member.id
        ))
        .await;
    assert_eq!(pid_before, 1, "authorized participant state preserved");
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    let ok = join_user(&app, cid, &member).await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND user_id='{}'",
            member.id
        ))
        .await,
        1,
        "retry reuses the same logical participant"
    );
    connect(&app, cid, &ok).await;
    assert_eq!(session_state(&app, ok.session_id).await, "CONNECTED");
}

/// L-F4: LiveKit termination failure → ENDING (never ENDED), joins blocked, reconciler
/// converges once the provider recovers.
#[tokio::test]
async fn l_f04_livekit_termination_failure() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f4", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    connect(&app, cid, &p).await;
    lk(&app).faults.fail_delete.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conference"]["status"], "ENDING", "{v}");
    assert_ne!(v["providerEnforcement"], "ENFORCED");
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    let r = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    let _ = r;
    assert_eq!(
        conference_status(&app, cid).await,
        "ENDING",
        "still unconfirmed"
    );
    lk(&app).faults.fail_delete.store(false, Ordering::SeqCst);
    exec(
        &app,
        "UPDATE worker_leases SET expires_at = now() - interval '1 second'",
    )
    .await;
    backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert!(!lk(&app).has_room(&room(cid)));
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
}

/// L-F5: outbox delivery delayed → committed state is authoritative, events stay
/// PENDING (no rollback), and a later relay delivers each exactly once.
#[tokio::test]
async fn l_f05_outbox_delivery_delayed() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f5", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // No relay has run: the state change is committed regardless.
    let (_, c) = app
        .get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(c["isLocked"], true);
    let pending = app
        .scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"
        ))
        .await;
    assert!(pending >= 2);
    let relay = OutboxRelay::new(
        app.db.conn.clone(),
        Arc::new(backend::workers::outbox_relay::LogPublisher),
    );
    let (published, _) = relay.tick(1000).await.unwrap();
    assert!(published as i64 >= pending);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"
        ))
        .await,
        0
    );
    let (again, _) = relay.tick(1000).await.unwrap();
    assert_eq!(again, 0, "no duplicate delivery");
}

/// L-F6: recording provider failure is isolated: the conference continues, the
/// recording is never faked READY, a provider rejection is FAILED, a stable status is
/// reported.
#[tokio::test]
async fn l_f06_recording_provider_failure_isolated() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, _) = art::meeting(&app, 1).await;
    lk(&app).faults.fail_egress.store(true, Ordering::SeqCst);
    let (s, v) = art::start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["providerDispatch"], "PENDING_RETRY");
    let rid = uuid(&v["recording"]["id"]);
    assert_eq!(art::artifact_status(&app, rid).await, "REQUESTED");
    assert_no_secrets(&app, "recording outage", &v);
    // The live conference is unaffected.
    assert_eq!(conference_status(&app, cid).await, "LIVE");
    let j = join_guest(&app, cid, "during-outage").await;
    assert_eq!(j.body["rtcStatus"], "ISSUED");
    lk(&app).faults.fail_egress.store(false, Ordering::SeqCst);
    // Provider rejection → FAILED (never READY).
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings/{rid}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_success() || s.is_client_error());
    lk(&app).faults.reject_egress.store(true, Ordering::SeqCst);
    let (_, v) = art::start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    if let Some(rid2) = v["recording"]["id"].as_str() {
        let rid2 = Uuid::parse_str(rid2).unwrap();
        assert_ne!(art::artifact_status(&app, rid2).await, "READY");
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND status='READY'"
        ))
        .await,
        0,
        "no fake READY"
    );
    assert_eq!(conference_status(&app, cid).await, "LIVE");
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
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame within 10s")
            .expect("stream open")
            .expect("frame");
        if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// L-F7: a WebSocket disconnect never changes business or media state; the client
/// reconnects with its cursor (replay) or refetches canonical state.
#[tokio::test]
async fn l_f07_websocket_disconnect_recovery() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f7", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    connect(&app, cid, &g).await;
    let addr = app.serve().await;
    let (_, t) = app
        .post(
            format!("/api/v2/conferences/{cid}/realtime/ticket"),
            g.auth(),
            json!({}),
        )
        .await;
    let mut ws = ws_connect(addr, cid, t["ticket"].as_str().unwrap(), None).await;
    let hello = next_frame(&mut ws).await;
    let cursor = hello["cursor"].as_i64().unwrap_or(0);
    drop(ws); // abrupt disconnect
    app.post(
        format!("/api/v2/conferences/{cid}/lock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        session_state(&app, g.session_id).await,
        "CONNECTED",
        "RTC continues"
    );
    assert!(lk(&app).participant(&room(cid), &g.identity).is_some());
    // Replay from the cursor over REST and over a new socket.
    let (s, r) = app
        .get(
            format!("/api/v2/conferences/{cid}/events?after={cursor}"),
            g.auth(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert!(r["events"].to_string().contains("conference.locked"), "{r}");
    let (_, t) = app
        .post(
            format!("/api/v2/conferences/{cid}/realtime/ticket"),
            g.auth(),
            json!({}),
        )
        .await;
    let mut ws = ws_connect(addr, cid, t["ticket"].as_str().unwrap(), Some(cursor)).await;
    let mut saw_lock = false;
    for _ in 0..10 {
        let f = next_frame(&mut ws).await;
        if f["type"] == "event" && f["event"]["eventType"] == "conference.locked" {
            saw_lock = true;
            break;
        }
        if f["type"] == "resync_required" {
            break;
        }
    }
    let (_, c) = app
        .get(format!("/api/v2/conferences/{cid}"), g.auth())
        .await;
    assert!(
        saw_lock || c["isLocked"] == true,
        "replay or canonical refetch recovers"
    );
    assert_eq!(c["isLocked"], true);
}

/// L-F8: scheduler crash/recovery — a crash after materializing (bookkeeping lost) or
/// after committing a scheduled END (provider termination pending) never duplicates
/// occurrences or terminal transitions on restart.
#[tokio::test]
async fn l_f08_scheduler_crash_recovery() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let space = app.create_space(&owner, json!({ "title": "f8" })).await;
    let sid = uuid(&space["id"]);
    let start = (Utc::now() + chrono::Duration::days(1))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let (s, _) = app
        .call(
            Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=DAILY;COUNT=4", "startLocal": start, "durationMinutes": 30 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let before = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    exec(
        &app,
        "UPDATE meeting_recurrence_rules SET materialized_until = NULL",
    )
    .await;
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    scheduler.tick(Utc::now()).await.unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        before,
        "restart after lost bookkeeping creates no duplicates"
    );
    // Scheduled END committed while the provider is down, then the process "crashes".
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "f8-live", "accessPolicy": "PUBLIC" }),
        )
        .await;
    exec(&app, &format!("UPDATE conference_sessions SET scheduled_start_at = now() - interval '3 hours', scheduled_end_at = now() - interval '2 hours' WHERE id='{cid}'")).await;
    lk(&app).faults.fail_delete.store(true, Ordering::SeqCst);
    assert_eq!(scheduler.tick(Utc::now()).await.unwrap().end_requested, 1);
    assert_eq!(conference_status(&app, cid).await, "ENDING");
    let restarted = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    assert_eq!(
        restarted.tick(Utc::now()).await.unwrap().end_requested,
        0,
        "no duplicate END"
    );
    lk(&app).faults.fail_delete.store(false, Ordering::SeqCst);
    exec(&app, "UPDATE outbox_events SET available_at = now() - interval '1 second' WHERE status='PENDING'").await;
    art::dispatch(&app, cid).await;
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_terminate_requested'"
        ))
        .await,
        1
    );
}

/// L-F9: reconciler crash → the lease blocks a second instance until expiry, then
/// recovery proceeds and repairs drift once.
#[tokio::test]
async fn l_f09_reconciler_crash_recovery() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f9", "accessPolicy": "PUBLIC" }))
        .await;
    let crashed = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    assert!(!crashed.tick().await.unwrap().skipped_lease);
    drop(crashed);
    lk(&app).drop_room(&room(cid));
    let survivor = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    assert!(
        survivor.tick().await.unwrap().skipped_lease,
        "live lease respected"
    );
    exec(
        &app,
        "UPDATE worker_leases SET expires_at = now() - interval '1 second'",
    )
    .await;
    let r = survivor.tick().await.unwrap();
    assert!(r.kinds.contains(&"LIVE_ROOM_MISSING".to_string()), "{r:?}");
    assert!(lk(&app).has_room(&room(cid)));
}

/// L-F10: artefact worker crash → a live per-row lease is respected; after expiry the
/// deletion completes exactly once.
#[tokio::test]
async fn l_f10_artifact_worker_crash_recovery() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, _) = art::meeting(&app, 0).await;
    let (rid, _) = art::recording_ready(&app, cid, &owner).await;
    let key = art::storage_key(&app, rid).await;
    exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute', locked_by='crashed', locked_until = now() + interval '1 hour' WHERE id='{rid}'")).await;
    assert_eq!(art::retention_tick(&app).await.deleted, 0);
    assert!(app.storage.contains(&key));
    exec(&app, &format!("UPDATE meeting_artifacts SET locked_until = now() - interval '1 second' WHERE id='{rid}'")).await;
    assert_eq!(art::retention_tick(&app).await.deleted, 1);
    assert_eq!(
        art::retention_tick(&app).await.deleted,
        0,
        "no duplicate terminal op"
    );
    assert!(!app.storage.contains(&key));
    assert_eq!(art::audits(&app, cid, "ARTIFACT_DELETED").await, 1);
}

/// L-F11: delayed / replayed provider callbacks never resurrect authority: a join
/// callback for a removed participant is evicted, replays are deduplicated, a late
/// egress completion cannot regress a terminal artefact.
#[tokio::test]
async fn l_f11_provider_callback_delay_and_replay() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, _) = art::meeting(&app, 0).await;
    let p = join_guest(&app, cid, "p").await;
    let (s, _) = app
        .post(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                p.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // The provider join (stale credential) and its callback arrive after removal.
    if lk(&app).connect(&p.credential).is_ok() {
        let body = livekit_mock::webhook_body(
            "participant_joined",
            "EV_LF11",
            &room(cid),
            None,
            Some(&p.identity),
        );
        let (s, v) = app.signed_webhook(&body).await;
        assert_eq!(s, StatusCode::OK);
        assert_ne!(v["outcome"], "APPLIED", "{v}");
        assert!(
            lk(&app).participant(&room(cid), &p.identity).is_none(),
            "evicted"
        );
        let (_, v) = app.signed_webhook(&body).await;
        assert_eq!(v["outcome"], "DUPLICATE");
    }
    assert_ne!(session_state(&app, p.session_id).await, "CONNECTED");
    // Late egress callbacks after READY / delete.
    let (rid, _) = art::recording_ready(&app, cid, &owner).await;
    let eg = art::provider_ref(&app, rid).await;
    let (s, _) = app
        .delete(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let late = lk(&app).egress_activate(&eg);
    let (s, _) = art::egress_event(&app, &late).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        art::artifact_status(&app, rid).await,
        "DELETED",
        "terminal state kept"
    );
}

/// L-F12: mixed dependency failure (Redis + LiveKit down) never produces a false
/// success: no media claim, no ENDED without provider confirmation, no recording
/// evidence; recovery converges.
#[tokio::test]
async fn l_f12_mixed_dependency_failure_no_false_success() {
    let app = app!(TestApp::with(ghi(TestOptions {
        redis: RedisMode::Unreachable,
        rtc_timeout: Duration::from_millis(500),
        ..TestOptions::default()
    })));
    let doc = openapi();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "f12", "accessPolicy": "PUBLIC" }))
        .await;
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    lk(&app).faults.fail_egress.store(true, Ordering::SeqCst);
    let (s, h, v) = raw(
        &app,
        Method::POST,
        &format!("/api/v2/conferences/{cid}/join"),
        &[],
        Some(r#"{"displayName":"x"}"#),
    )
    .await;
    assert_eq!(code(&v), "RTC_PROVIDER_UNAVAILABLE");
    assert_error_contract(&doc, s, &h, &v);
    let (s, v) = art::start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    if s == StatusCode::ACCEPTED {
        let rid = uuid(&v["recording"]["id"]);
        assert_eq!(
            art::artifact_status(&app, rid).await,
            "REQUESTED",
            "no capture claimed"
        );
    }
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["conference"]["status"], "ENDING",
        "never ENDED without confirmation"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state='CONNECTED'"
        ))
        .await,
        0
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND status IN ('RECORDING','READY')"
        ))
        .await,
        0
    );
    assert_no_secrets(&app, "end", &v);
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    lk(&app).faults.fail_egress.store(false, Ordering::SeqCst);
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        v["conference"]["status"], "ENDED",
        "converges after recovery: {v}"
    );
}
