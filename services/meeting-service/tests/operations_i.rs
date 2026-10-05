//! Phase 2 Part I — quality telemetry, join SLI, conference health, incidents and
//! operations visibility. Test IDs follow the Part I specification (I-S security, I-C
//! concurrency, I-F failure, I-E2E end-to-end).

mod common;

use std::sync::{atomic::Ordering, Arc};

use axum::http::{Method, StatusCode};
use backend::services::quality::{
    incidents::{self, Incident, IncidentAction},
    sink::FailingSink,
};
use common::{
    artifacts::{meeting, recording_active},
    code,
    rtc::{connect, exec, join_guest, lk, provider_event, room, Joined},
    uuid, Auth, RtcMode, TestApp, TestOptions,
};
use livekit_protocol as proto;
use serde_json::{json, Value};
use uuid::Uuid;

const OPS_TOKEN: &str = "ops-internal-token-0123456789";

macro_rules! app {
    () => {{
        std::env::set_var("INTERNAL_API_TOKEN", OPS_TOKEN);
        match TestApp::artifacts().await {
            Some(a) => a,
            None => return,
        }
    }};
}

async fn failing_sink_app() -> Option<(TestApp, Arc<FailingSink>)> {
    std::env::set_var("INTERNAL_API_TOKEN", OPS_TOKEN);
    let sink = Arc::new(FailingSink::default());
    let app = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        telemetry: Some(sink.clone()),
        ..Default::default()
    })
    .await?;
    Some((app, sink))
}

fn sample(rtt: f64) -> Value {
    json!({
        "timestamp": chrono::Utc::now(),
        "rttMs": rtt,
        "jitterMs": 8.0,
        "packetLossRatio": 0.005,
        "bitrateKbps": 900.0,
        "width": 1280, "height": 720, "fps": 30.0,
        "networkType": "WIFI",
    })
}

async fn submit(app: &TestApp, cid: Uuid, auth: Auth<'_>, body: Value) -> (StatusCode, Value) {
    app.post(
        format!("/api/v2/conferences/{cid}/quality/samples"),
        auth,
        body,
    )
    .await
}

async fn ops(app: &TestApp, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let headers: Vec<(&str, &str)> = token
        .map(|t| vec![("x-internal-token", t)])
        .unwrap_or_default();
    app_get_with_headers(app, path, &headers).await
}

async fn app_get_with_headers(
    app: &TestApp,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(Method::GET).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app
        .router
        .clone()
        .oneshot(req.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, v)
}

async fn health(app: &TestApp, cid: Uuid) -> Value {
    let (s, v) = ops(
        app,
        &format!("/internal/operations/conferences/{cid}/health"),
        Some(OPS_TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn metrics(app: &TestApp) -> String {
    let (s, v) = ops(app, "/internal/metrics", Some(OPS_TOKEN)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["raw"].as_str().unwrap().to_string()
}

async fn session_i64(app: &TestApp, sid: Uuid, col: &str) -> i64 {
    app.scalar(&format!(
        "SELECT coalesce({col}, 0)::bigint FROM participant_sessions WHERE id='{sid}'"
    ))
    .await
}

async fn reconcile(app: &TestApp) -> backend::workers::conference_reconciler::ReconcileReport {
    backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap()
}

// ===========================================================================
// Security I-S1 … I-S10
// ===========================================================================

#[tokio::test]
async fn i_s01_sample_cannot_impersonate_another_session() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 2).await;
    let (s, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "participantSessionId": g[1].session_id, "samples": [sample(100.0)] }),
    )
    .await;
    assert!(s.is_client_error(), "{v}");
    assert_eq!(
        session_i64(&app, g[1].session_id, "quality_samples").await,
        0
    );
    // Without a session id the caller's OWN session is used.
    let (s, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(uuid(&v["participantSessionId"]), g[0].session_id);
}

#[tokio::test]
async fn i_s02_foreign_conference_submission_rejected() {
    let app = app!();
    let (_o, _s, _cid_a, ga) = meeting(&app, 1).await;
    let (_o2, _s2, cid_b, _) = meeting(&app, 0).await;
    let (s, v) = submit(
        &app,
        cid_b,
        ga[0].auth(),
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    assert!(
        matches!(
            s,
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED
        ),
        "{s} {v}"
    );
    let (s, _) = submit(
        &app,
        cid_b,
        Auth::None,
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    assert!(s.is_client_error());
}

#[tokio::test]
async fn i_s03_invalid_or_extreme_telemetry_rejected() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let mut bad = vec![];
    for (k, v) in [
        ("rttMs", json!(1e308)),
        ("rttMs", json!(-5)),
        ("packetLossRatio", json!(1.5)),
        ("fps", json!(5000)),
        ("width", json!(100000)),
        ("jitterMs", json!("NaN")),
    ] {
        let mut s = sample(100.0);
        s[k] = v;
        bad.push(s);
    }
    let mut skewed = sample(100.0);
    skewed["timestamp"] = json!(chrono::Utc::now() - chrono::Duration::hours(2));
    bad.push(skewed);
    let mut free_text = sample(100.0);
    free_text["note"] = json!("john@example.com");
    bad.push(free_text);
    for s in bad {
        let (st, v) = submit(&app, cid, g[0].auth(), json!({ "samples": [s] })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    }
    let (st, _) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "providerNode": "node; DROP TABLE", "samples": [sample(10.0)] }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        0
    );
}

#[tokio::test]
async fn i_s04_telemetry_rate_limited() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let mut limited = 0;
    for i in 0..15 {
        let (s, v) = submit(
            &app,
            cid,
            g[0].auth(),
            json!({ "batchId": format!("b{i}"), "samples": [sample(100.0)] }),
        )
        .await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(code(&v), "QUALITY_RATE_LIMITED");
            limited += 1;
        }
    }
    assert!(
        limited >= 3,
        "12 batches/min per session: {limited} limited"
    );
}

#[tokio::test]
async fn i_s05_operations_inaccessible_to_participants() {
    let app = app!();
    let (owner, _s, cid, g) = meeting(&app, 1).await;
    for path in [
        "/internal/operations/conferences/health".to_string(),
        format!("/internal/operations/conferences/{cid}/health"),
        "/internal/operations/incidents".into(),
        "/internal/metrics".into(),
    ] {
        let token = g[0].participant_token.clone().unwrap();
        let (s, _) =
            app_get_with_headers(&app, &path, &[("x-participant-token", token.as_str())]).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
        let bearer = format!("Bearer {}", owner.token);
        let (s, _) = app_get_with_headers(&app, &path, &[("authorization", bearer.as_str())]).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
        let (s, _) = ops(&app, &path, Some("wrong-token-wrong-token-wrong-")).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }
    let (s, _) = app
        .get(format!("/api/v2/conferences/{cid}/quality"), g[0].auth())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "participants lack quality.view");
}

#[tokio::test]
async fn i_s06_host_sees_only_own_scoped_quality() {
    let app = app!();
    let (owner, _s, cid, g) = meeting(&app, 1).await;
    let (other_owner, _, other_cid, _) = meeting(&app, 0).await;
    submit(&app, cid, g[0].auth(), json!({ "providerRegion": "eu-west-1", "providerNode": "node-7", "samples": [sample(120.0)] })).await;
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/quality"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["activeSessions"], 1);
    let (s, _) = app
        .get(
            format!("/api/v2/conferences/{cid}/quality"),
            Auth::Bearer(&other_owner.token),
        )
        .await;
    assert!(matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND));
    let _ = other_cid;
    // Infrastructure-wide views are internal only.
    let bearer = format!("Bearer {}", owner.token);
    let (s, _) = app_get_with_headers(
        &app,
        "/internal/operations/conferences/health",
        &[("authorization", bearer.as_str())],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn i_s07_telemetry_labels_contain_no_pii() {
    let app = app!();
    let owner = app
        .user_with_email("pii-owner", "pii.owner@example.com")
        .await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "Secret Board Meeting", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let g = join_guest(&app, cid, "Jane Patient Doe").await;
    connect(&app, cid, &g).await;
    submit(&app, cid, g.auth(), json!({ "samples": [sample(500.0)] })).await;
    health(&app, cid).await;
    let m = metrics(&app).await;
    assert!(m.contains("cfmeet_"), "{m}");
    for needle in [
        "pii.owner@example.com",
        "Jane Patient Doe",
        "Secret Board Meeting",
        &cid.to_string(),
        &g.identity,
        &g.participant_id.to_string(),
    ] {
        assert!(!m.contains(needle), "{needle} in metrics");
    }
    // Every label value comes from a bounded vocabulary.
    for line in m.lines().filter(|l| !l.starts_with('#') && l.contains('{')) {
        let labels = &line[line.find('{').unwrap() + 1..line.find('}').unwrap()];
        for pair in labels.split(',') {
            let value = pair.split('=').nth(1).unwrap_or("").trim_matches('"');
            assert!(
                value.len() <= 32
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c)),
                "unbounded label {pair}"
            );
        }
    }
}

#[tokio::test]
async fn i_s08_provider_internals_not_exposed_to_clients() {
    let app = app!();
    let (owner, _s, cid, g) = meeting(&app, 1).await;
    let (s, v) = submit(&app, cid, g[0].auth(), json!({ "providerRegion": "eu-west-1", "providerNode": "lk-node-7", "samples": [sample(100.0)] })).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert!(!v.to_string().contains("lk-node-7"));
    for (path, auth) in [
        (
            format!("/api/v2/conferences/{cid}/quality"),
            Auth::Bearer(&owner.token),
        ),
        (format!("/api/v2/conferences/{cid}"), g[0].auth()),
        (
            format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&owner.token),
        ),
        (
            format!("/api/v2/conferences/{cid}/participants/me"),
            g[0].auth(),
        ),
    ] {
        let (_, body) = app.get(path.clone(), auth).await;
        let t = body.to_string();
        assert!(
            !t.contains("lk-node-7") && !t.contains("eu-west-1"),
            "{path}: {t}"
        );
    }
    let h = health(&app, cid).await;
    assert_eq!(
        h["providerNodes"],
        json!(["lk-node-7"]),
        "operations-only visibility: {h}"
    );
}

#[tokio::test]
async fn i_s09_incidents_cannot_mutate_business_state() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let (s, _) = app
        .call_with_headers(
            Method::POST,
            "/internal/operations/incidents",
            &[("x-internal-token", OPS_TOKEN)],
            json!({ "conferenceId": cid, "status": "ENDED" }).to_string(),
        )
        .await;
    assert!(
        matches!(s, StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND),
        "{s}"
    );
    let (s, _) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "healthState": "UNHEALTHY", "samples": [sample(10.0)] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no client-controlled health");
    let (s, _) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [{ "timestamp": chrono::Utc::now(), "role": "HOST" }] }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(common::rtc::conference_status(&app, cid).await, "LIVE");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND role='PARTICIPANT'",
            g[0].participant_id
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn i_s10_sink_outage_does_not_bypass_authorization() {
    let Some((app, sink)) = failing_sink_app().await else {
        return;
    };
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let stranger = app.user("stranger").await;
    let (s, _) = submit(
        &app,
        cid,
        Auth::Bearer(&stranger.token),
        json!({ "samples": [sample(10.0)] }),
    )
    .await;
    assert!(s.is_client_error(), "authorization still enforced");
    let (s, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(10.0), sample(20.0)] }),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["sinkDropped"], 2);
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        2,
        "relational aggregate intact"
    );
    assert!(sink.calls.load(Ordering::SeqCst) >= 2);
}

// ===========================================================================
// Concurrency I-C1 … I-C6
// ===========================================================================

#[tokio::test]
async fn i_c01_concurrent_quality_batches() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let futs = (0..8).map(|i| {
        submit(
            &app,
            cid,
            g[0].auth(),
            json!({ "batchId": format!("c{i}"), "samples": [sample(100.0), sample(200.0)] }),
        )
    });
    let results = futures_util::future::join_all(futs).await;
    let ok = results
        .iter()
        .filter(|r| r.0 == StatusCode::ACCEPTED)
        .count() as i64;
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        ok * 2,
        "no lost update"
    );
    let hist: Value = {
        use sea_orm::ConnectionTrait;
        let row = app
            .db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT rtt_histogram FROM participant_sessions WHERE id='{}'",
                    g[0].session_id
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get_by_index(0).unwrap()
    };
    let total: i64 = hist
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .sum();
    assert_eq!(total, ok * 2);
}

#[tokio::test]
async fn i_c02_provider_disconnect_vs_client_reconnect() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let rejoin = app.join(
        cid,
        g[0].auth(),
        json!({ "previousSessionId": g[0].session_id }),
    );
    let left = provider_event(&app, "participant_left", cid, Some(&g[0].identity));
    let (r, l) = tokio::join!(rejoin, left);
    assert_eq!(r.0, StatusCode::OK, "{r:?}");
    assert_eq!(l.0, StatusCode::OK);
    let active = app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{}' AND state IN ('PENDING','CONNECTED')", g[0].participant_id)).await;
    assert_eq!(active, 1, "exactly the new session remains");
    let new_sid = uuid(&r.1["participantSession"]["id"]);
    let new = Joined {
        session_id: new_sid,
        identity: format!("ps_{}", new_sid.simple()),
        credential: r.1["rtc"]["credential"].as_str().unwrap().into(),
        participant_id: g[0].participant_id,
        participant_token: g[0].participant_token.clone(),
        body: r.1.clone(),
    };
    connect(&app, cid, &new).await;
    assert_eq!(session_i64(&app, new_sid, "reconnect_count").await, 1);
}

#[tokio::test]
async fn i_c03_two_reconcilers_same_drift() {
    let app = app!();
    let (_o, _s, cid, _g) = meeting(&app, 1).await;
    lk(&app).insert_participant(
        &room(cid),
        "rogue-identity",
        proto::ParticipantPermission {
            can_publish: true,
            ..Default::default()
        },
    );
    let a = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let b = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let (ra, rb) = tokio::join!(a.tick(), b.tick());
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert!(ra.skipped_lease || rb.skipped_lease, "singleton lease");
    assert!(!lk(&app)
        .identities(&room(cid))
        .contains(&"rogue-identity".to_string()));
    assert_eq!(
        lk(&app).call_count("RemoveParticipant"),
        1,
        "one destructive repair"
    );
}

#[tokio::test]
async fn i_c04_health_aggregation_while_conference_ends() {
    let app = app!();
    let (owner, _s, cid, g) = meeting(&app, 2).await;
    submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    let end = app.post(
        format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let path = format!("/internal/operations/conferences/{cid}/health");
    let eval = ops(&app, &path, Some(OPS_TOKEN));
    let (e, h) = tokio::join!(end, eval);
    assert!(e.0.is_success(), "{e:?}");
    assert_eq!(h.0, StatusCode::OK, "{h:?}");
    let after = health(&app, cid).await;
    assert!(
        matches!(after["lifecycleState"].as_str(), Some("ENDING" | "ENDED")),
        "{after}"
    );
    assert_eq!(after["activeParticipants"], 0);
}

#[tokio::test]
async fn i_c05_incident_creation_dedupe() {
    let app = app!();
    let (_o, _s, cid, _) = meeting(&app, 0).await;
    let make = || Incident {
        kind: incidents::kind::RTC_ROOM_MISSING,
        category: "RTC",
        conference_id: Some(cid),
        room_name: None,
        subject: Some("room".into()),
        action: IncidentAction::Escalated,
        critical: true,
        detail: json!({}),
    };
    let (i1, i2, i3) = (make(), make(), make());
    let (a, b, c) = tokio::join!(
        incidents::record(&app.db.conn, &i1),
        incidents::record(&app.db.conn, &i2),
        incidents::record(&app.db.conn, &i3)
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}' AND status='OPEN'"
        ))
        .await,
        1
    );
    assert_eq!(app.scalar(&format!("SELECT occurrences::bigint FROM rtc_drift_events WHERE conference_id='{cid}' AND status='OPEN'")).await, 3);
}

#[tokio::test]
async fn i_c06_sink_retry_does_not_duplicate_aggregates() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let body = json!({ "batchId": "retry-1", "samples": [sample(100.0), sample(150.0)] });
    let (_, v1) = submit(&app, cid, g[0].auth(), body.clone()).await;
    let (s, v2) = submit(&app, cid, g[0].auth(), body).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(v1["duplicateBatch"], false);
    assert_eq!(v2["duplicateBatch"], true, "{v2}");
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        2
    );
}

// ===========================================================================
// Failure I-F1 … I-F8
// ===========================================================================

#[tokio::test]
async fn i_f01_metrics_sink_unavailable() {
    let Some((app, _sink)) = failing_sink_app().await else {
        return;
    };
    let (owner, _s, cid, g) = meeting(&app, 1).await;
    let (s, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["sinkDropped"], 1);
    // Joins, recording and health keep working.
    let late = join_guest(&app, cid, "late").await;
    connect(&app, cid, &late).await;
    recording_active(&app, cid, &owner).await;
    let h = health(&app, cid).await;
    assert_eq!(h["joinSuccesses"], 2, "{h}");
}

#[tokio::test]
async fn i_f02_time_series_backend_unavailable() {
    let Some((app, _sink)) = failing_sink_app().await else {
        return;
    };
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(100.0)] }),
    )
    .await;
    // Exporter has nothing from the sink, but the endpoint and relational counters work.
    let m = metrics(&app).await;
    assert!(
        m.contains("cfmeet_rtc_") || m.contains("cfmeet_ops_"),
        "{m}"
    );
    let h = health(&app, cid).await;
    assert_eq!(h["activeParticipants"], 1);
}

#[tokio::test]
async fn i_f03_malformed_telemetry_payload() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let tok = g[0].participant_token.clone().unwrap();
    let (s, _) = app
        .post_raw(
            &format!("/api/v2/conferences/{cid}/quality/samples"),
            Auth::Participant(&tok),
            "{not json",
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = submit(&app, cid, g[0].auth(), json!({ "samples": [] })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "empty batch");
    let big = json!({ "samples": (0..400).map(|_| sample(1.0)).collect::<Vec<_>>() }).to_string();
    let (s, _) = app
        .post_raw(
            &format!("/api/v2/conferences/{cid}/quality/samples"),
            Auth::Participant(&tok),
            &big,
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "body limit");
    assert_eq!(
        app.scalar(&format!(
            "SELECT quality_samples::bigint FROM participant_sessions WHERE id='{}'",
            g[0].session_id
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn i_f04_operations_aggregate_query_failure() {
    let app = app!();
    let (_o, _s, cid, _g) = meeting(&app, 1).await;
    exec(
        &app,
        "ALTER TABLE rtc_drift_events RENAME TO rtc_drift_events_offline",
    )
    .await;
    let (s, _) = ops(&app, "/internal/operations/incidents", Some(OPS_TOKEN)).await;
    let (s2, _) = ops(
        &app,
        &format!("/internal/operations/conferences/{cid}/health"),
        Some(OPS_TOKEN),
    )
    .await;
    let late = join_guest(&app, cid, "unaffected").await;
    exec(
        &app,
        "ALTER TABLE rtc_drift_events_offline RENAME TO rtc_drift_events",
    )
    .await;
    assert!(s.is_server_error(), "{s}");
    assert!(s2.is_server_error() || s2 == StatusCode::OK);
    assert!(late.participant_token.is_some(), "meeting path unaffected");
}

#[tokio::test]
async fn i_f05_provider_metrics_temporarily_unavailable() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 2).await;
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let r = reconcile(&app).await;
    let _ = r;
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    // No destructive guesses while the provider cannot answer.
    assert_eq!(common::rtc::conference_status(&app, cid).await, "LIVE");
    for j in &g {
        assert_eq!(
            common::rtc::session_state(&app, j.session_id).await,
            "CONNECTED"
        );
    }
    assert_eq!(lk(&app).call_count("RemoveParticipant"), 0);
}

#[tokio::test]
async fn i_f06_reconciliation_worker_crash() {
    let app = app!();
    let (_o, _s, cid, _) = meeting(&app, 0).await;
    exec(&app, "INSERT INTO worker_leases (name, holder, expires_at, updated_at) VALUES ('conference_reconciler', 'crashed-instance', now() + interval '1 hour', now()) ON CONFLICT (name) DO UPDATE SET holder = EXCLUDED.holder, expires_at = EXCLUDED.expires_at").await;
    assert!(reconcile(&app).await.skipped_lease, "live lease respected");
    exec(&app, "UPDATE worker_leases SET expires_at = now() - interval '1 second' WHERE name='conference_reconciler'").await;
    lk(&app).drop_room(&room(cid));
    let r = reconcile(&app).await;
    assert!(!r.skipped_lease, "expired lease taken over");
    assert!(lk(&app).has_room(&room(cid)), "work resumed");
}

#[tokio::test]
async fn i_f07_duplicate_provider_callbacks() {
    let app = app!();
    let (_o, _s, cid, _) = meeting(&app, 0).await;
    let g = join_guest(&app, cid, "dup").await;
    lk(&app).connect(&g.credential).unwrap();
    let body = common::livekit_mock::webhook_body(
        "participant_joined",
        "EV_DUP_1",
        &room(cid),
        None,
        Some(&g.identity),
    );
    let (_, a) = app.signed_webhook(&body).await;
    let (_, b) = app.signed_webhook(&body).await;
    assert_eq!(a["outcome"], "APPLIED");
    assert_eq!(b["outcome"], "DUPLICATE");
    let h = health(&app, cid).await;
    assert_eq!(h["joinMediaConnected"], 1, "{h}");
    assert_eq!(h["reconnects"], 0);
}

#[tokio::test]
async fn i_f08_high_volume_spike_bounded() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let (s, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": (0..21).map(|_| sample(1.0)).collect::<Vec<_>>() }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "per-batch bound: {v}");
    let futs = (0..30).map(|i| {
        submit(
            &app,
            cid,
            g[0].auth(),
            json!({ "batchId": format!("s{i}"), "samples": [sample(10.0)] }),
        )
    });
    let results = futures_util::future::join_all(futs).await;
    let accepted = results
        .iter()
        .filter(|r| r.0 == StatusCode::ACCEPTED)
        .count();
    assert!(accepted <= 12, "rate bound: {accepted}");
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        accepted as i64
    );
}

// ===========================================================================
// End-to-end I-E2E-01 … I-E2E-15
// ===========================================================================

#[tokio::test]
async fn i_e2e_01_successful_join_recorded() {
    let app = app!();
    let (_o, _s, cid, _g) = meeting(&app, 1).await;
    let h = health(&app, cid).await;
    assert_eq!(h["joinAttempts"], 1, "{h}");
    assert_eq!(h["joinSuccesses"], 1);
    assert_eq!(h["joinMediaConnected"], 1);
    let m = metrics(&app).await;
    assert!(m.contains("meeting_join_success_total 1"), "{m}");
}

#[tokio::test]
async fn i_e2e_02_provider_failure_separate_from_denial() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "123456" }),
        )
        .await;
    let (s, _) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "wrong", "passcode": "000000" }),
        )
        .await;
    assert!(s.is_client_error());
    lk(&app).drop_room(&room(cid));
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    let (_, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "unlucky", "passcode": "123456" }),
        )
        .await;
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    assert_eq!(code(&v), "RTC_PROVIDER_UNAVAILABLE", "{v}");
    let h = health(&app, cid).await;
    assert_eq!(h["joinDenials"], 1, "{h}");
    assert_eq!(h["joinProviderFailures"], 1);
    assert!(
        h["joinFailureRate"].as_f64().unwrap() > 0.0,
        "denial excluded, provider failure counted"
    );
}

#[tokio::test]
async fn i_e2e_03_join_latency_to_media_connection() {
    let app = app!();
    let (_o, _s, cid, _) = meeting(&app, 0).await;
    let g = join_guest(&app, cid, "slow-network").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    connect(&app, cid, &g).await;
    let latency = session_i64(&app, g.session_id, "join_latency_ms").await;
    assert!(
        latency >= 250,
        "measured to provider media connect: {latency}ms"
    );
    let h = health(&app, cid).await;
    assert!(h["avgJoinLatencyMs"].as_f64().unwrap() >= 250.0, "{h}");
}

#[tokio::test]
async fn i_e2e_04_sample_correlated_to_session() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let (s, v) = submit(&app, cid, g[0].auth(), json!({ "participantSessionId": g[0].session_id, "samples": [sample(80.0), sample(120.0)] })).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(uuid(&v["participantSessionId"]), g[0].session_id);
    assert_eq!(v["qualityState"], "HEALTHY");
    assert_eq!(
        session_i64(&app, g[0].session_id, "quality_samples").await,
        2
    );
    assert_eq!(session_i64(&app, g[0].session_id, "avg_rtt_ms").await, 100);
}

#[tokio::test]
async fn i_e2e_05_poor_network_degrades_health() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 2).await;
    let (_, v) = submit(
        &app,
        cid,
        g[0].auth(),
        json!({ "samples": [sample(450.0)] }),
    )
    .await;
    assert_eq!(v["qualityState"], "DEGRADED", "{v}");
    let mut bad = sample(1500.0);
    bad["packetLossRatio"] = json!(0.3);
    let (_, v) = submit(&app, cid, g[1].auth(), json!({ "samples": [bad] })).await;
    assert_eq!(v["qualityState"], "UNHEALTHY", "{v}");
    let h = health(&app, cid).await;
    assert!(
        matches!(h["healthState"].as_str(), Some("DEGRADED" | "UNHEALTHY")),
        "{h}"
    );
    assert!(h["reasons"].to_string().contains("PARTICIPANT_QUALITY"));
    assert_eq!(h["unhealthySessions"], 1);
}

#[tokio::test]
async fn i_e2e_06_reconnect_counted_once() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let (s, r) = app
        .join(
            cid,
            g[0].auth(),
            json!({ "previousSessionId": g[0].session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let sid = uuid(&r["participantSession"]["id"]);
    let identity = format!("ps_{}", sid.simple());
    lk(&app)
        .connect(r["rtc"]["credential"].as_str().unwrap())
        .unwrap();
    let body = common::livekit_mock::webhook_body(
        "participant_joined",
        "EV_RECONNECT",
        &room(cid),
        None,
        Some(&identity),
    );
    app.signed_webhook(&body).await;
    app.signed_webhook(&body).await;
    provider_event(&app, "participant_joined", cid, Some(&identity)).await;
    assert_eq!(session_i64(&app, sid, "reconnect_count").await, 1);
    assert_eq!(health(&app, cid).await["reconnects"], 1);
}

#[tokio::test]
async fn i_e2e_07_publish_failure_metric() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    let mut s1 = sample(100.0);
    s1["failure"] = json!({ "kind": "VIDEO_PUBLISH", "category": "PERMISSION_DENIED" });
    let (s, v) = submit(&app, cid, g[0].auth(), json!({ "samples": [s1] })).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        session_i64(&app, g[0].session_id, "publish_failures").await,
        1
    );
    assert_eq!(health(&app, cid).await["publishFailures"], 1);
    let m = metrics(&app).await;
    assert!(
        m.lines().any(|l| l.contains("publish_failures")
            && l.contains("kind=\"video\"")
            && l.contains("permission_denied")),
        "{m}"
    );
}

#[tokio::test]
async fn i_e2e_08_provider_room_disappearance_incident() {
    let app = app!();
    let (_o, _s, cid, _) = meeting(&app, 1).await;
    lk(&app).drop_room(&room(cid));
    reconcile(&app).await;
    let (_, v) = ops(&app, "/internal/operations/incidents", Some(OPS_TOKEN)).await;
    let mine: Vec<&Value> = v["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["conferenceId"] == json!(cid))
        .collect();
    assert!(mine.iter().any(|i| i["kind"] == "RTC_ROOM_MISSING"), "{v}");
}

#[tokio::test]
async fn i_e2e_09_orphan_provider_participant_removed() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    lk(&app).insert_participant(
        &room(cid),
        "rogue-identity",
        proto::ParticipantPermission {
            can_publish: true,
            ..Default::default()
        },
    );
    let r = reconcile(&app).await;
    assert!(
        r.kinds
            .iter()
            .any(|k| k == "PROVIDER_PARTICIPANT_WITHOUT_SESSION"),
        "{r:?}"
    );
    assert!(!lk(&app)
        .identities(&room(cid))
        .contains(&"rogue-identity".to_string()));
    assert!(
        lk(&app).identities(&room(cid)).contains(&g[0].identity),
        "legitimate participant untouched"
    );
}

#[tokio::test]
async fn i_e2e_10_stale_connected_session_reconciled() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    lk(&app).disconnect(&room(cid), &g[0].identity);
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    // Within grace: untouched.
    reconciler.tick().await.unwrap();
    assert_eq!(
        common::rtc::session_state(&app, g[0].session_id).await,
        "CONNECTED"
    );
    exec(&app, &format!("UPDATE participant_sessions SET connected_at = now() - interval '10 minutes' WHERE id='{}'", g[0].session_id)).await;
    let r = reconciler.tick().await.unwrap();
    assert!(
        r.kinds.iter().any(|k| k == "SESSION_CONNECTED_BUT_ABSENT"),
        "{r:?}"
    );
    assert_ne!(
        common::rtc::session_state(&app, g[0].session_id).await,
        "CONNECTED"
    );
}

#[tokio::test]
async fn i_e2e_11_recording_failure_in_health() {
    let app = app!();
    let (owner, _s, cid, _) = meeting(&app, 1).await;
    let (_rid, eg) = recording_active(&app, cid, &owner).await;
    let failed = lk(&app).egress_fail(&eg, "encoder crashed");
    app.signed_webhook(&failed).await;
    let h = health(&app, cid).await;
    assert_eq!(h["recordingFailures"], 1, "{h}");
    assert!(
        h["reasons"].to_string().contains("RECORDING_FAILURE"),
        "{h}"
    );
    assert_eq!(h["healthState"], "DEGRADED");
}

#[tokio::test]
async fn i_e2e_12_transcription_failure_in_health() {
    let app = app!();
    let (owner, _s, cid, _) = common::artifacts::meeting_with(
        &app,
        json!({ "title": "x", "accessPolicy": "PUBLIC", "transcriptionPolicy": "HOST_OPTIONAL" }),
        1,
    )
    .await;
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/transcription"),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    let job = uuid(&v["id"]);
    let jr = common::artifacts::job_ref(&app, job).await;
    common::artifacts::transcription_callback(
        &app,
        &json!({ "type": "JOB_STATUS", "jobRef": jr, "status": "FAILED" }),
    )
    .await;
    let h = health(&app, cid).await;
    assert_eq!(h["transcriptionFailures"], 1, "{h}");
    assert!(h["reasons"].to_string().contains("TRANSCRIPTION_FAILURE"));
}

#[tokio::test]
async fn i_e2e_13_operations_lists_unhealthy_without_pii() {
    let app = app!();
    let owner = app
        .user_with_email("ops-owner", "ops.owner@example.com")
        .await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "Confidential Merger Call", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let g = join_guest(&app, cid, "Alice Sensitive").await;
    connect(&app, cid, &g).await;
    let mut bad = sample(2000.0);
    bad["packetLossRatio"] = json!(0.4);
    submit(&app, cid, g.auth(), json!({ "samples": [bad] })).await;
    let (s, v) = ops(
        &app,
        "/internal/operations/conferences/health?state=UNHEALTHY",
        Some(OPS_TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let list = v["conferences"].as_array().unwrap();
    assert!(list.iter().any(|c| c["conferenceId"] == json!(cid)), "{v}");
    let t = v.to_string();
    for needle in [
        "ops.owner@example.com",
        "Alice Sensitive",
        "Confidential Merger Call",
        &g.identity,
    ] {
        assert!(!t.contains(needle), "{needle} leaked");
    }
    let (s, _) = ops(
        &app,
        "/internal/operations/conferences/health?state=BOGUS",
        Some(OPS_TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn i_e2e_14_metrics_failure_does_not_break_live_conference() {
    let Some((app, _sink)) = failing_sink_app().await else {
        return;
    };
    let (owner, _s, cid, g) = meeting(&app, 2).await;
    for j in &g {
        let (s, _) = submit(&app, cid, j.auth(), json!({ "samples": [sample(90.0)] })).await;
        assert_eq!(s, StatusCode::ACCEPTED);
    }
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/messages"),
            g[0].auth(),
            json!({ "body": "still chatting" }),
        )
        .await;
    assert!(s.is_success());
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_success());
}

#[tokio::test]
async fn i_e2e_15_two_reconcilers_no_duplicate_destructive_repair() {
    let app = app!();
    let (_o, _s, cid, g) = meeting(&app, 1).await;
    lk(&app).disconnect(&room(cid), &g[0].identity);
    exec(&app, &format!("UPDATE participant_sessions SET connected_at = now() - interval '10 minutes' WHERE id='{}'", g[0].session_id)).await;
    lk(&app).insert_participant(
        &room(cid),
        "rogue-2",
        proto::ParticipantPermission::default(),
    );
    let a = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let b = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let (ra, rb) = tokio::join!(a.tick(), b.tick());
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert!(
        ra.skipped_lease ^ rb.skipped_lease,
        "exactly one active reconciler"
    );
    assert_eq!(
        lk(&app).call_count("RemoveParticipant"),
        1,
        "one destructive repair"
    );
    assert_ne!(
        common::rtc::session_state(&app, g[0].session_id).await,
        "CONNECTED"
    );
    // A second pass finds nothing left to repair.
    let again = a.tick().await.unwrap();
    assert!(
        !again
            .kinds
            .iter()
            .any(|k| k == "SESSION_CONNECTED_BUT_ABSENT"
                || k == "PROVIDER_PARTICIPANT_WITHOUT_SESSION"),
        "{again:?}"
    );
}
