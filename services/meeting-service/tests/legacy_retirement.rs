//! Part K — legacy write retirement readiness (K-E2E-10, §37–§39, §88).
//!
//! `FEATURE_LEGACY_MEETING_WRITE_DISABLED` retires every state-changing legacy meeting
//! route with `410 LEGACY_WRITE_DISABLED` while legacy reads, the compatibility
//! resolver and `/api/v2` keep working. Off by default.

mod common;

use axum::http::{Method, StatusCode};
use backend::config::FeatureFlags;
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

fn legacy_redis_required() -> bool {
    require_test_redis()
}

async fn app_with(flags: FeatureFlags) -> Option<TestApp> {
    TestApp::with(TestOptions {
        flags,
        ..TestOptions::default()
    })
    .await
}

async fn legacy_meeting(app: &TestApp, host: &User) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let code = format!("R{}", &id.simple().to_string()[..7])
        .to_uppercase()
        .replace(['0', '1', 'O', 'I'], "Q");
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', '{code}', '{h}', '{h}', 'legacy', false, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing')",
            h = host.id
        ))
        .await
        .unwrap();
    (id, code)
}

/// K-E2E-10: with legacy writes disabled, every legacy write answers the stable
/// deprecation error before touching the database; required reads, the resolver and v2
/// keep working.
#[tokio::test]
async fn k_e2e_10_legacy_write_disable_keeps_reads_and_v2() {
    if !legacy_redis_required() {
        return;
    }
    let Some(app) = app_with(FeatureFlags {
        legacy_meeting_write_disabled: true,
        rtc_provider_control_v2: true,
        ..FeatureFlags::all_v2_enabled()
    })
    .await
    else {
        return;
    };
    let host = app.user("host").await;
    let (mid, code_str) = legacy_meeting(&app, &host).await;
    let pid = Uuid::new_v4();
    let meetings_before = app.scalar("SELECT count(*) FROM meetings").await;
    let participants_before = app.scalar("SELECT count(*) FROM participants").await;
    let logs_before = app.scalar("SELECT count(*) FROM session_logs").await;
    let rejected_before = backend::services::ops_metrics::OPS_METRICS
        .legacy_writes_rejected
        .load(std::sync::atomic::Ordering::Relaxed);
    let writes: Vec<(Method, String)> = vec![
        (Method::POST, "/api/v1/meetings".into()),
        (Method::POST, "/api/v1/meetings/instant".into()),
        (Method::PUT, format!("/api/v1/meetings/{mid}")),
        (Method::DELETE, format!("/api/v1/meetings/{mid}")),
        (Method::POST, format!("/api/v1/meetings/{mid}/end")),
        (Method::POST, format!("/api/v1/meetings/{mid}/join")),
        (Method::POST, format!("/api/v1/meetings/join/{code_str}")),
        (
            Method::POST,
            format!("/api/v1/meetings/{mid}/waiting/{pid}/admit"),
        ),
        (
            Method::POST,
            format!("/api/v1/meetings/{mid}/waiting/{pid}/deny"),
        ),
        (
            Method::POST,
            format!("/api/v1/meetings/{mid}/screen-share/start"),
        ),
        (
            Method::POST,
            format!("/api/v1/meetings/{mid}/screen-share/stop"),
        ),
        (Method::POST, format!("/api/v1/meetings/{mid}/chat")),
        (Method::POST, format!("/api/v1/participants/{pid}/kick")),
        (Method::PATCH, format!("/api/v1/participants/{pid}/audio")),
        (Method::PATCH, format!("/api/v1/participants/{pid}/video")),
        (Method::POST, "/api/v1/api/meetings".into()),
        (Method::POST, format!("/api/v1/api/meetings/{mid}/join")),
    ];
    for (m, uri) in &writes {
        for auth in [Auth::None, Auth::Bearer(&host.token)] {
            let (s, v) = app
                .call(
                    m.clone(),
                    uri,
                    auth,
                    Some(json!({ "title": "x", "display_name": "x" })),
                )
                .await;
            assert_eq!(
                (s, code(&v)),
                (StatusCode::GONE, "LEGACY_WRITE_DISABLED"),
                "{m} {uri}: {v}"
            );
            assert!(v["error"]["requestId"].is_string(), "{v}");
        }
    }
    let rejected_after = backend::services::ops_metrics::OPS_METRICS
        .legacy_writes_rejected
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(rejected_after >= rejected_before + 2 * writes.len() as u64);
    assert_eq!(
        app.scalar("SELECT count(*) FROM meetings").await,
        meetings_before
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM participants").await,
        participants_before
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM session_logs").await,
        logs_before
    );

    // Required compatibility reads keep working.
    for uri in [
        "/api/v1/meetings".to_string(),
        format!("/api/v1/meetings/{mid}"),
        format!("/api/v1/meetings/{mid}/participants"),
        format!("/api/v1/meetings/{mid}/waiting"),
    ] {
        let (s, v) = app.get(&uri, Auth::Bearer(&host.token)).await;
        assert_eq!(s, StatusCode::OK, "legacy read {uri}: {v}");
    }
    let (s, v) = app
        .get(&format!("/api/v1/meetings/public/{code_str}"), Auth::None)
        .await;
    assert_eq!(s, StatusCode::OK, "public legacy info: {v}");
    let (s, v) = app
        .get(
            &format!("/api/v2/resolve/{code_str}"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "resolver: {v}");
    assert_eq!(v["matchedBy"], "LEGACY_CODE");

    // v2 remains fully functional.
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "v2", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    // Non-meeting v1 surfaces (auth, users) are not part of the retirement.
    let (s, _) = app.get("/api/v1/users/me", Auth::Bearer(&host.token)).await;
    assert_eq!(s, StatusCode::OK);
}

/// Default rollout: the flag is off, legacy writes work and never touch v2-native
/// state (no hybrid authority): legacy handlers address only legacy rows.
#[tokio::test]
async fn k_legacy_writes_default_on_and_cannot_overwrite_v2_state() {
    if !legacy_redis_required() {
        return;
    }
    let Some(app) = app_with(FeatureFlags {
        rtc_provider_control_v2: true,
        ..FeatureFlags::all_v2_enabled()
    })
    .await
    else {
        return;
    };
    assert!(!FeatureFlags::default().legacy_meeting_write_disabled);
    assert!(!FeatureFlags::all_v2_enabled().legacy_meeting_write_disabled);
    let host = app.user("host").await;
    let (s, v) = app
        .post(
            "/api/v1/meetings/instant",
            Auth::Bearer(&host.token),
            json!({ "title": "still works" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // A v2-native space / conference id or code is unknown to every legacy handler.
    let (sid, cid) = app
        .live_conference(&host, json!({ "title": "v2", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, space) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&host.token),
        )
        .await;
    let v2_code = space["meetingCode"].as_str().unwrap().to_string();
    for (m, uri) in [
        (Method::PUT, format!("/api/v1/meetings/{sid}")),
        (Method::POST, format!("/api/v1/meetings/{sid}/end")),
        (Method::POST, format!("/api/v1/meetings/{cid}/end")),
        (Method::DELETE, format!("/api/v1/meetings/{cid}")),
        (Method::POST, format!("/api/v1/meetings/join/{v2_code}")),
    ] {
        let (s, v) = app
            .call(
                m.clone(),
                &uri,
                Auth::Bearer(&host.token),
                Some(json!({ "title": "x" })),
            )
            .await;
        assert!(s.is_client_error(), "{m} {uri}: {s} {v}");
    }
    let (_, c) = app
        .get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(
        c["status"], "LIVE",
        "v2 authority untouched by legacy handlers"
    );
    // Legacy host credentials never carry a recording grant (Part G authority).
    let token = v["data"]["join_token"].as_str().unwrap();
    let claims = livekit_api::access_token::TokenVerifier::with_api_key("k", "s")
        .verify(token)
        .expect("legacy credential minted by the legacy adapter");
    assert!(claims.video.room_join && claims.video.room_admin);
    assert!(
        !claims.video.room_record,
        "legacy host token must not grant room_record"
    );
}

/// §88 flag matrix: v2 disabled → stable FEATURE_DISABLED for every v2 route; the
/// write allowlist gates v2 writes but not v2 reads of authorized resources.
#[tokio::test]
async fn k_feature_flag_combinations_are_coherent() {
    // v2 off: every v2 route answers FEATURE_DISABLED (404) with the envelope.
    let Some(app) = app_with(FeatureFlags::default()).await else {
        return;
    };
    let u = app.user("u").await;
    for (m, uri) in [
        (Method::POST, "/api/v2/meeting-spaces".to_string()),
        (
            Method::GET,
            format!("/api/v2/conferences/{}", Uuid::new_v4()),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{}/join", Uuid::new_v4()),
        ),
    ] {
        let (s, v) = app
            .call(m, &uri, Auth::Bearer(&u.token), Some(json!({})))
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "FEATURE_DISABLED"),
            "{uri}"
        );
        assert!(v["error"]["requestId"].is_string());
    }
    drop(app);
    // Allowlist: only listed users write; others are refused without side effects.
    let writer_id = Uuid::new_v4();
    let Some(app) = app_with(FeatureFlags {
        v2_write_allowlist: Some([writer_id].into_iter().collect()),
        ..FeatureFlags::all_v2_enabled()
    })
    .await
    else {
        return;
    };
    let outsider = app.user("outsider").await;
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&outsider.token),
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!(code(&v), "FEATURE_DISABLED", "{s} {v}");
    assert_eq!(app.scalar("SELECT count(*) FROM meeting_spaces").await, 0);
}
