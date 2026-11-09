//! Blueprint v1.0 Section 16 (MTG-017 / DATA-008): legacy v1 meeting creation.
//!
//! `FEATURE_LEGACY_MEETING_CREATE_MODE`:
//! * `mirror` (default): the legacy create works as before, every answer carries
//!   `Deprecation` / `Sunset`, and the new meeting is mapped into v2 at once (no lazy
//!   resolve needed); legacy end refreshes the mirror.
//! * `refuse`: new legacy creates answer `410 LEGACY_WRITE_DISABLED` pointing to
//!   `/api/v2`, write nothing, and existing legacy meetings keep their reads and updates.
mod common;

use axum::{
    body::Body,
    http::{header, HeaderMap, Method, Request, StatusCode},
};
use backend::config::{FeatureFlags, LegacyMeetingCreateMode};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

async fn raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    bearer: &str,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

fn assert_deprecated(h: &HeaderMap, context: &str) {
    assert!(h.get("deprecation").is_some(), "{context}: Deprecation");
    assert!(h.get("sunset").is_some(), "{context}: Sunset");
    assert!(
        h.get("link")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("rel=\"deprecation\""),
        "{context}: Link rel=deprecation"
    );
}

async fn app_with(mode: LegacyMeetingCreateMode) -> Option<TestApp> {
    if !require_test_redis() {
        return None;
    }
    TestApp::with(TestOptions {
        flags: FeatureFlags {
            legacy_meeting_create_mode: mode,
            ..FeatureFlags::all_v2_enabled()
        },
        ..TestOptions::default()
    })
    .await
}

fn future_local() -> String {
    (chrono::Utc::now() + chrono::Duration::days(2))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

#[tokio::test]
async fn mirror_mode_maps_new_legacy_meetings_into_v2_at_once() {
    let Some(app) = app_with(LegacyMeetingCreateMode::Mirror).await else {
        return;
    };
    let host = app.user("mirrorhost").await;
    let mirrored_before = backend::services::ops_metrics::OPS_METRICS
        .legacy_creates_mirrored
        .load(std::sync::atomic::Ordering::Relaxed);

    // Instant meeting: created live, mapped before anyone resolves it.
    let (s, h, v) = raw(
        &app,
        Method::POST,
        "/api/v1/meetings/instant",
        &host.token,
        json!({ "title": "v1 instant" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_deprecated(&h, "instant");
    let instant: Uuid = v["data"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id = '{instant}' AND origin = 'LEGACY'"
        ))
        .await,
        1,
        "mirrored on create"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id = '{instant}' AND status = 'LIVE'"
        ))
        .await,
        1
    );

    // Scheduled meeting: mapped too.
    let (s, h, v) = raw(
        &app,
        Method::POST,
        "/api/v1/meetings",
        &host.token,
        json!({ "title": "v1 scheduled", "start_time": future_local(), "timezone": "Africa/Lagos" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_deprecated(&h, "create");
    let scheduled: Uuid = v["data"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id = '{scheduled}' AND status = 'CREATED'"
        ))
        .await,
        1
    );

    // A legacy update refreshes the mirrored space title.
    let (s, _, v) = raw(
        &app,
        Method::PUT,
        &format!("/api/v1/meetings/{scheduled}"),
        &host.token,
        json!({ "title": "renamed in v1" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id = '{scheduled}' AND title = 'renamed in v1'"
        ))
        .await,
        1,
        "mirror refreshed on update"
    );

    // A legacy end refreshes the mirrored conference.
    let (s, _, v) = raw(
        &app,
        Method::POST,
        &format!("/api/v1/meetings/{instant}/end"),
        &host.token,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE legacy_meeting_id = '{instant}' AND status = 'ENDED'"
        ))
        .await,
        1,
        "mirror refreshed on end"
    );

    // The v2 resolver answers from the mapping (still legacy-managed).
    let (s, r) = app
        .get(
            &format!("/api/v2/resolve/{scheduled}"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["matchedBy"], "LEGACY_ID");
    let mirrored_after = backend::services::ops_metrics::OPS_METRICS
        .legacy_creates_mirrored
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(mirrored_after >= mirrored_before + 4);
}

#[tokio::test]
async fn refuse_mode_blocks_only_new_legacy_creates() {
    let Some(app) = app_with(LegacyMeetingCreateMode::Refuse).await else {
        return;
    };
    let host = app.user("refusehost").await;
    let meetings_before = app.scalar("SELECT count(*) FROM meetings").await;
    for (uri, body) in [
        ("/api/v1/meetings/instant", json!({ "title": "x" })),
        (
            "/api/v1/meetings",
            json!({ "title": "x", "start_time": future_local(), "timezone": "UTC" }),
        ),
    ] {
        let (s, h, v) = raw(&app, Method::POST, uri, &host.token, body).await;
        assert_eq!(
            (s, v["error"]["code"].as_str().unwrap_or("")),
            (StatusCode::GONE, "LEGACY_WRITE_DISABLED"),
            "{uri}: {v}"
        );
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("/api/v2/meeting-spaces"),
            "{uri}: points to v2: {v}"
        );
        assert_deprecated(&h, uri);
    }
    assert_eq!(
        app.scalar("SELECT count(*) FROM meetings").await,
        meetings_before,
        "nothing written"
    );

    // Existing legacy meetings keep working (reads and updates are not creates).
    let id = Uuid::new_v4();
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', 'RFSQ2345', '{h}', '{h}', 'old', false, now() + interval '1 day', now() + interval '1 day 1 hour', 'scheduled')",
            h = host.id
        ))
        .await
        .unwrap();
    let (s, v) = app
        .get(&format!("/api/v1/meetings/{id}"), Auth::Bearer(&host.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _, v) = raw(
        &app,
        Method::PUT,
        &format!("/api/v1/meetings/{id}"),
        &host.token,
        json!({ "title": "still editable" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Refuse mode never mirrors (the lazy resolver still maps on demand).
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id = '{id}'"
        ))
        .await,
        0
    );
}
