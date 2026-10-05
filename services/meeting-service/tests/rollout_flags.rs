//! Part N §52–§55 — feature-flag combination certification and the rollback drill.
//!
//! Every combination runs the same probe against the real router: what is enabled
//! works, what is disabled answers a stable error, and no combination falls back to an
//! insecure path (no media credential without provider control, no admission without
//! the waiting room, no legacy token, never a 5xx). The rollback drill "redeploys" the
//! service with flags turned off and on again over the same database, provider and
//! object store, and proves records, provider state and the outbox stay consistent.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use backend::config::FeatureFlags;
use common::{artifacts as art, collab::*, rtc::*, *};
use serde_json::{json, Value};

fn base_flags() -> FeatureFlags {
    FeatureFlags {
        meeting_v2_enabled: true,
        ..FeatureFlags::default()
    }
}

async fn app_with(flags: FeatureFlags) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags,
        ..TestOptions::default()
    })
    .await
}

fn no_credential(v: &Value) -> bool {
    v.get("rtc").map(Value::is_null).unwrap_or(true) && !v.to_string().contains("\"credential\"")
}

/// The standard probe; returns a compact outcome line for the report.
async fn probe(app: &TestApp, flags: &FeatureFlags, label: &str) -> String {
    let owner = app.user("owner").await;
    let mut out = vec![label.to_string()];
    let (s, space) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&owner.token),
            json!({ "title": "p", "accessPolicy": "PUBLIC" }),
        )
        .await;
    assert!(!s.is_server_error(), "{label}: {s} {space}");
    if !flags.meeting_v2_enabled {
        assert_eq!(code(&space), "FEATURE_DISABLED", "{label}");
        out.push("v2=off".into());
    } else {
        assert_eq!(s, StatusCode::CREATED, "{label}: {space}");
        let sid = uuid(&space["id"]);
        let (_, c) = app
            .post(
                format!("/api/v2/meeting-spaces/{sid}/conferences"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        let cid = uuid(&c["id"]);
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/start"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{label} start: {v}");
        // Join: credential only under provider control; never a fallback token.
        let (s, j) = app
            .join(cid, Auth::None, json!({ "displayName": "g" }))
            .await;
        assert_eq!(s, StatusCode::OK, "{label} join: {j}");
        if flags.rtc_provider_control_v2 {
            assert_eq!(j["rtcStatus"], "ISSUED", "{label}");
        } else {
            assert_eq!(j["rtcStatus"], "PROVIDER_CONTROL_DISABLED", "{label}");
            assert!(
                no_credential(&j),
                "{label}: no credential without provider control"
            );
        }
        out.push(format!("join={}", j["rtcStatus"].as_str().unwrap()));
        // Waiting room configuration.
        let (s, v) = app
            .post(
                "/api/v2/meeting-spaces",
                Auth::Bearer(&owner.token),
                json!({ "title": "w", "waitingRoomPolicy": "EVERYONE" }),
            )
            .await;
        assert_eq!(
            s.is_success(),
            flags.waiting_room_v2_enabled,
            "{label} waiting: {s} {v}"
        );
        // Realtime.
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/realtime/ticket"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(
            s.is_success(),
            flags.conference_realtime_enabled,
            "{label} realtime: {s} {v}"
        );
        // Collaboration.
        let (s, v) = app
            .get(
                format!("/api/v2/conferences/{cid}/polls"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert_eq!(
            s.is_success(),
            flags.collaboration_v2_enabled,
            "{label} collab: {s} {v}"
        );
        // Recording (owner must be a participant for the consent / provider path).
        let _ = app.join(cid, Auth::Bearer(&owner.token), json!({})).await;
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/recordings"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        if flags.recording_v2_enabled {
            assert!(
                s.is_success() || code(&v) == "RECORDING_PROVIDER_UNAVAILABLE",
                "{label} recording: {s} {v}"
            );
        } else {
            assert_eq!(code(&v), "RECORDING_DISABLED", "{label}");
        }
        // Special modes.
        let (s, v) = app
            .post(
                "/api/v2/meeting-spaces",
                Auth::Bearer(&owner.token),
                json!({ "title": "wb", "meetingType": "WEBINAR" }),
            )
            .await;
        assert_eq!(
            s.is_success(),
            flags.webinar_mode_enabled,
            "{label} webinar: {s} {v}"
        );
        let (s, v) = app
            .post(
                "/api/v2/meeting-spaces",
                Auth::Bearer(&owner.token),
                json!({ "title": "hc", "meetingType": "HEALTHCARE", "accessPolicy": "AUTHENTICATED", "waitingRoomPolicy": "EVERYONE" }),
            )
            .await;
        assert_eq!(
            s.is_success(),
            flags.healthcare_profile_enabled && flags.waiting_room_v2_enabled,
            "{label} healthcare: {s} {v}"
        );
    }
    // Legacy writes.
    if std::env::var("TEST_REDIS_URL")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        let (s, v) = app
            .post(
                "/api/v1/meetings/instant",
                Auth::Bearer(&owner.token),
                json!({ "title": "legacy" }),
            )
            .await;
        if flags.legacy_meeting_write_disabled {
            assert_eq!(
                (s, code(&v)),
                (StatusCode::GONE, "LEGACY_WRITE_DISABLED"),
                "{label}"
            );
        } else {
            assert_eq!(s, StatusCode::CREATED, "{label} legacy: {v}");
            assert!(!v["data"]["join_token"].as_str().unwrap_or("").is_empty());
        }
        out.push(format!("legacyWrite={}", s.as_u16()));
    }
    out.join(" ")
}

#[tokio::test]
async fn n_flag_combination_matrix() {
    let all = FeatureFlags {
        rtc_provider_control_v2: true,
        conference_realtime_enabled: true,
        collaboration_v2_enabled: true,
        recording_v2_enabled: true,
        ..FeatureFlags::all_v2_enabled()
    };
    let combos: Vec<(&str, FeatureFlags)> = vec![
        ("all-off", FeatureFlags::default()),
        ("v2-core-only", base_flags()),
        (
            "v2+realtime",
            FeatureFlags {
                conference_realtime_enabled: true,
                ..base_flags()
            },
        ),
        (
            "v2+waiting-room",
            FeatureFlags {
                waiting_room_v2_enabled: true,
                ..base_flags()
            },
        ),
        (
            "v2+rtc-control",
            FeatureFlags {
                rtc_provider_control_v2: true,
                ..base_flags()
            },
        ),
        (
            "v2+collaboration",
            FeatureFlags {
                collaboration_v2_enabled: true,
                rtc_provider_control_v2: true,
                ..base_flags()
            },
        ),
        (
            "v2+recording",
            FeatureFlags {
                recording_v2_enabled: true,
                rtc_provider_control_v2: true,
                ..base_flags()
            },
        ),
        (
            "v2+webinar",
            FeatureFlags {
                webinar_mode_enabled: true,
                ..base_flags()
            },
        ),
        (
            "v2+healthcare",
            FeatureFlags {
                healthcare_profile_enabled: true,
                waiting_room_v2_enabled: true,
                ..base_flags()
            },
        ),
        ("all-A-I", all.clone()),
        (
            "all-A-I+legacy-writes-disabled",
            FeatureFlags {
                legacy_meeting_write_disabled: true,
                ..all.clone()
            },
        ),
        ("v2-off+legacy-writes-enabled", FeatureFlags::default()),
    ];
    let mut report = vec![];
    for (label, flags) in combos {
        let Some(app) = app_with(flags.clone()).await else {
            return;
        };
        report.push(probe(&app, &flags, label).await);
    }
    println!("FLAG MATRIX\n{}", report.join("\n"));
    assert_eq!(report.len(), 12);
}

/// The rollback drill (§55).
#[tokio::test]
async fn n_rollback_drill() {
    let all = TestApp::ghi_flags();
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: all.clone(),
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    // 1–3. v2 fully enabled: meetings, active sessions, waiting guest, collaboration,
    // recording.
    let (owner, sid, cid, guests) = art::meeting_with(
        &app,
        json!({ "title": "drill", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        0,
    )
    .await;
    let member = app.user("member").await;
    let m = join_user(&app, cid, &member).await;
    connect(&app, cid, &m).await;
    let host = owner_joined(&app, cid, &owner).await;
    let (_, w) = app
        .join(cid, Auth::None, json!({ "displayName": "waiting" }))
        .await;
    assert_eq!(w["participant"]["admissionState"], "WAITING");
    let waiting_pid = uuid(&w["participant"]["id"]);
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    let (s, msg) = send(
        &app,
        cid,
        m.auth_bearer(&member),
        json!({ "body": "before rollback" }),
    )
    .await;
    assert!(s.is_success(), "{msg}");
    let (rid, eg) = art::recording_active(&app, cid, &owner).await;
    let _ = guests;
    let before = snapshot(&app, cid).await;

    // 4–5. Unhealthy v2 feature set → disable collaboration, recording, realtime,
    // waiting-room management (provider control stays on).
    let reduced = FeatureFlags {
        collaboration_v2_enabled: false,
        recording_v2_enabled: false,
        transcription_v2_enabled: false,
        conference_realtime_enabled: false,
        waiting_room_v2_enabled: false,
        ..all.clone()
    };
    let app = app.reconfigure(TestOptions {
        flags: reduced,
        ..TestOptions::default()
    });
    // 6. Records remain valid; disabled surfaces answer stable errors.
    assert_eq!(snapshot(&app, cid).await, before, "no record lost");
    assert_eq!(conference_status(&app, cid).await, "LIVE");
    assert_eq!(session_state(&app, m.session_id).await, "CONNECTED");
    assert_eq!(session_state(&app, host.session_id).await, "CONNECTED");
    let (_, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/polls"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(code(&v), "FEATURE_DISABLED");
    // Capture is switched off; existing artefact records stay readable to authorised
    // actors (permission-checked) so a rollback never hides evidence.
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(code(&v), "RECORDING_DISABLED");
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings/{rid}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(code(&v), "RECORDING_DISABLED");
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recordings"].as_array().map(|a| a.len()), Some(1), "{v}");
    let outsider = app.user("rollback-outsider").await;
    let (s, _) = app
        .get(
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&outsider.token),
        )
        .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "outsider still denied: {s}"
    );
    // 7. No unsafe fallback: the waiting room still applies and cannot be bypassed.
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [waiting_pid] }),
        )
        .await;
    assert_eq!(code(&v), "FEATURE_DISABLED");
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "new guest" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["participant"]["admissionState"], "WAITING",
        "never admitted by fallback"
    );
    assert!(no_credential(&v));
    // Provider evidence keeps flowing while recording is disabled (never lost).
    let bytes = b"\x00\x00\x00\x18ftypmp42 rollback drill".repeat(32);
    let done = lk(&app).egress_complete(&eg, Some(&app.storage), &bytes, None);
    let (s, _) = art::egress_event(&app, &done).await;
    assert_eq!(s, StatusCode::OK);
    // 8. Provider reconciliation is non-destructive for valid state.
    let r = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(
        !r.kinds
            .iter()
            .any(|k| k.contains("ORPHAN") || k == "LIVE_ROOM_MISSING"),
        "{r:?}"
    );
    assert!(lk(&app).has_room(&room(cid)));
    assert!(lk(&app).participant(&room(cid), &m.identity).is_some());

    // Deeper rollback: provider control off → joins never get a fallback token.
    let app = app.reconfigure(TestOptions {
        flags: FeatureFlags {
            rtc_provider_control_v2: false,
            ..all.clone()
        },
        ..TestOptions::default()
    });
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["rtcStatus"], "PROVIDER_CONTROL_DISABLED");
    assert!(no_credential(&v));
    // Routing rollback: v2 off → stable FEATURE_DISABLED, service healthy (legacy v1
    // traffic with v2 off is certified by `n_flag_combination_matrix`).
    let app = app.reconfigure(TestOptions {
        flags: FeatureFlags {
            meeting_v2_enabled: false,
            ..all.clone()
        },
        ..TestOptions::default()
    });
    let (_, v) = app
        .get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(code(&v), "FEATURE_DISABLED");
    let (s, _) = app.get("/health", Auth::None).await;
    assert_eq!(s, StatusCode::OK);

    // 9–10. Re-enable: canonical state intact and convergent.
    let app = app.reconfigure(TestOptions {
        flags: all.clone(),
        ..TestOptions::default()
    });
    assert_eq!(conference_status(&app, cid).await, "LIVE");
    let (_, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/polls/{poll}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(v["status"], "OPEN", "{v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [waiting_pid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    if art::artifact_status(&app, rid).await != "READY" {
        // The provider re-delivers completion; the worker verifies storage.
        let (s, _) = art::egress_event(&app, &done).await;
        assert_eq!(s, StatusCode::OK);
        art::retention_tick(&app).await;
    }
    assert_eq!(
        art::artifact_status(&app, rid).await,
        "READY",
        "recording not orphaned"
    );
    let relay = backend::workers::outbox_relay::OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(backend::workers::outbox_relay::LogPublisher)
            as Arc<dyn backend::workers::outbox_relay::EventPublisher>),
        Some(Arc::new(backend::services::rtc_control::RtcControl::new(
            app.state.meeting.core.clone(),
        ))),
    );
    exec(
        &app,
        "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
    )
    .await;
    for _ in 0..5 {
        relay.tick(500).await.unwrap();
    }
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        0,
        "outbox consistent"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING'")
            .await,
        0,
        "outbox drained"
    );
    let _ = sid;
}

trait BearerAuth {
    fn auth_bearer<'a>(&self, u: &'a User) -> Auth<'a>;
}

impl BearerAuth for Joined {
    fn auth_bearer<'a>(&self, u: &'a User) -> Auth<'a> {
        Auth::Bearer(&u.token)
    }
}

async fn snapshot(app: &TestApp, cid: uuid::Uuid) -> (i64, i64, i64, i64) {
    (
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        app.scalar(&format!(
            "SELECT count(*) FROM conference_messages WHERE conference_id='{cid}'"
        ))
        .await,
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_polls WHERE conference_id='{cid}' AND status='OPEN'"
        ))
        .await,
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}'"
        ))
        .await,
    )
}
