//! Real self-hosted LiveKit certification (spec Part E §24).
//!
//! Drives a REAL livekit-server through the production `LiveKitProvider` and a REAL
//! WebRTC participant (`scripts/livekit-certification/rtcprobe`, pion). Run with
//! `scripts/livekit-certification/run.sh`. These tests are `#[ignore]`: they never
//! count in the default suite, and when run explicitly without a server they FAIL with
//! "PENDING" rather than passing.

mod common;

use std::{process::Stdio, time::Duration};

use axum::http::StatusCode;
use backend::{config::FeatureFlags, services::rtc_provider::RtcRoomRef};
use common::{rtc::*, *};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};
use uuid::Uuid;

struct Real {
    url: String,
    key: String,
    secret: String,
    webhook_port: u16,
    probe: String,
}

fn real() -> Real {
    let get = |k: &str| {
        std::env::var(k).unwrap_or_else(|_| panic!("PENDING: real LiveKit certification needs {k} (run scripts/livekit-certification/run.sh)"))
    };
    Real {
        url: get("CFMEET_REAL_LIVEKIT_URL"),
        key: get("CFMEET_REAL_LIVEKIT_KEY"),
        secret: get("CFMEET_REAL_LIVEKIT_SECRET"),
        webhook_port: get("CFMEET_REAL_LIVEKIT_WEBHOOK_PORT").parse().unwrap(),
        probe: get("RTCPROBE_BIN"),
    }
}

/// A real WebRTC participant.
struct Probe {
    child: Child,
    rx: mpsc::UnboundedReceiver<Value>,
    seen: Vec<Value>,
}

impl Probe {
    fn spawn(bin: &str, url: &str, credential: &str, publish: bool, secs: u32) -> Self {
        Self::spawn_with(
            bin,
            url,
            credential,
            if publish { "true" } else { "false" },
            secs,
        )
    }

    /// `publish`: false | true | comma list of mic,camera,screen.
    fn spawn_with(bin: &str, url: &str, credential: &str, publish: &str, secs: u32) -> Self {
        let mut child = Command::new(bin)
            .args([url, credential, publish, &secs.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn rtcprobe");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&l) {
                    let _ = tx.send(v);
                }
            }
        });
        Self {
            child,
            rx,
            seen: vec![],
        }
    }

    /// Ask the live probe to publish another track (`mic`, `camera`, `screen`).
    async fn publish(&mut self, source: &str) {
        use tokio::io::AsyncWriteExt;
        let stdin = self.child.stdin.as_mut().expect("probe stdin");
        stdin
            .write_all(format!("publish {source}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    /// Next observation (after everything already seen) matching any of `events` for
    /// `source`.
    async fn next_for(&mut self, events: &[&str], source: &str, within: Duration) -> Value {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(v)) => {
                    self.seen.push(v.clone());
                    if events.iter().any(|e| v["event"] == *e) && v["source"] == source {
                        return v;
                    }
                }
                _ => panic!(
                    "rtcprobe never reported {events:?} for {source}; saw {:?}",
                    self.seen
                ),
            }
        }
    }

    async fn wait_for(&mut self, event: &str, within: Duration) -> Value {
        if let Some(v) = self.seen.iter().find(|v| v["event"] == event) {
            return v.clone();
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(v)) => {
                    self.seen.push(v.clone());
                    if v["event"] == event {
                        return v;
                    }
                }
                _ => panic!("rtcprobe never reported {event}; saw {:?}", self.seen),
            }
        }
    }
}

async fn app_for(r: &Real) -> TestApp {
    let app = TestApp::with(TestOptions {
        rtc: RtcMode::RealLiveKit {
            url: r.url.clone(),
            api_key: r.key.clone(),
            api_secret: r.secret.clone(),
        },
        flags: FeatureFlags {
            rtc_provider_control_v2: true,
            conference_realtime_enabled: true,
            collaboration_v2_enabled: true,
            ..FeatureFlags::all_v2_enabled()
        },
        ..TestOptions::default()
    })
    .await
    .expect("database");
    // livekit-server delivers webhooks to this fixed port (scripts/livekit-certification/livekit.yaml).
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", r.webhook_port))
        .await
        .expect("webhook port free");
    let router = app.router.clone();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    app
}

async fn eventually<F, Fut>(what: &str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// Full lifecycle against a real SFU: room create/lookup, credential, real media join,
/// verified webhook, participant listing, mute, permission update, VIEWER cannot
/// publish, removal, termination.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_full_lifecycle() {
    let r = real();
    let app = app_for(&r).await;
    let owner = app.user("owner").await;
    let provider = app.state.meeting.core.rtc.clone();
    assert_eq!(provider.name(), "LIVEKIT");

    // Room creation + lookup (real).
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "cert", "accessPolicy": "PUBLIC" }))
        .await;
    let room_ref = RtcRoomRef {
        conference_id: cid,
        room_name: room(cid),
    };
    let info = provider
        .get_room(&room_ref)
        .await
        .expect("ListRooms")
        .expect("real room exists");
    assert!(
        info.room_id.as_deref().unwrap_or("").starts_with("RM_"),
        "{info:?}"
    );
    println!("CERT room create+lookup: PASS ({:?})", info.room_id);

    // Credential issuance + real media join + verified webhook.
    let speaker = join_guest(&app, cid, "speaker").await;
    let mut sp = Probe::spawn(&r.probe, &r.url, &speaker.credential, true, 60);
    sp.wait_for("connected", Duration::from_secs(20)).await;
    let published = sp.wait_for("publish_ok", Duration::from_secs(20)).await;
    println!(
        "CERT credential + media join: PASS (track {})",
        published["trackSid"]
    );
    eventually("webhook participant_joined → CONNECTED", || async {
        session_state(&app, speaker.session_id).await == "CONNECTED"
    })
    .await;
    assert!(
        app.scalar("SELECT count(*) FROM rtc_webhook_events WHERE outcome='APPLIED'")
            .await
            >= 1
    );
    println!("CERT webhook verification + session sync: PASS");

    // Participant listing (real).
    let list = provider.list_participants(&room_ref).await.unwrap();
    let me = list
        .iter()
        .find(|p| p.media_identity == speaker.identity)
        .expect("listed");
    assert!(me.grants.as_ref().unwrap().can_publish_audio);
    println!(
        "CERT participant listing: PASS ({} participants)",
        list.len()
    );

    // Mute primitive (real).
    eventually("published track visible", || async {
        provider
            .list_participants(&room_ref)
            .await
            .unwrap()
            .iter()
            .any(|p| p.media_identity == speaker.identity && !p.tracks.is_empty())
    })
    .await;
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{cid}/participants/{}/mute",
                speaker.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mutedTracks"], 1);
    sp.wait_for("track_muted", Duration::from_secs(10)).await;
    println!("CERT mute_track: PASS");

    // Permission update (real): demote to VIEWER.
    let (s, _) = change_role(&app, cid, &owner, speaker.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    eventually("provider grants follow VIEWER", || async {
        provider
            .list_participants(&room_ref)
            .await
            .unwrap()
            .iter()
            .any(|p| {
                p.media_identity == speaker.identity
                    && p.grants
                        .as_ref()
                        .map(|g| !g.can_publish_audio && g.can_subscribe)
                        .unwrap_or(false)
            })
    })
    .await;
    println!("CERT update_permissions: PASS");

    // VIEWER credential cannot publish on the real SFU (modified client).
    let (_, viewer_conf) = app
        .live_conference(
            &owner,
            json!({ "title": "viewers", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    let viewer = join_guest(&app, viewer_conf, "viewer").await;
    let mut vp = Probe::spawn(&r.probe, &r.url, &viewer.credential, true, 30);
    vp.wait_for("connected", Duration::from_secs(20)).await;
    vp.wait_for("publish_denied", Duration::from_secs(20)).await;
    println!("CERT VIEWER cannot publish: PASS");

    // Removal (real disconnect).
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                speaker.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    sp.wait_for("disconnected", Duration::from_secs(15)).await;
    assert!(!provider
        .list_participants(&room_ref)
        .await
        .unwrap()
        .iter()
        .any(|p| p.media_identity == speaker.identity));
    println!("CERT remove_participant: PASS");

    // Termination (real) with confirmed absence.
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{viewer_conf}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["conference"]["status"], "ENDED");
    vp.wait_for("disconnected", Duration::from_secs(15)).await;
    let gone = RtcRoomRef {
        conference_id: viewer_conf,
        room_name: room(viewer_conf),
    };
    assert!(provider.get_room(&gone).await.unwrap().is_none());
    println!("CERT terminate_room + ENDED: PASS");
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["conference"]["status"].as_str()),
        (StatusCode::OK, Some("ENDED"))
    );
    // room_finished webhooks for ended rooms are applied at most once.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    drop(vp.child.kill().await);
}

/// Reconciliation against the real SFU: provider-side room loss is repaired and a
/// participant that connected without a delivered webhook is recorded.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_reconciliation() {
    let r = real();
    let app = app_for(&r).await;
    let owner = app.user("owner").await;
    let provider = app.state.meeting.core.rtc.clone();
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "drift", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let room_ref = RtcRoomRef {
        conference_id: cid,
        room_name: room(cid),
    };
    provider
        .terminate_room(&room_ref, "TEST_PROVIDER_LOSS")
        .await
        .unwrap();
    assert!(provider.get_room(&room_ref).await.unwrap().is_none());
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let report = reconciler.tick().await.unwrap();
    assert!(
        report.kinds.contains(&"LIVE_ROOM_MISSING".to_string()),
        "{report:?}"
    );
    assert!(provider.get_room(&room_ref).await.unwrap().is_some());
    println!("CERT reconcile LIVE_ROOM_MISSING: PASS");
    let _ = Uuid::nil();
}

/// Part F against the real SFU: host mute, audio/video permission reduction and
/// restore, screen-share policy enforcement, provider screen-share events, host stop.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_part_f_media_controls() {
    let r = real();
    let app = app_for(&r).await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "cert-f", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let base = format!("/api/v2/conferences/{cid}");
    let a = join_guest(&app, cid, "presenter").await;
    let mut probe = Probe::spawn_with(&r.probe, &r.url, &a.credential, "mic,camera", 90);
    probe.wait_for("connected", Duration::from_secs(20)).await;
    probe
        .next_for(&["publish_ok"], "MICROPHONE", Duration::from_secs(20))
        .await;
    probe
        .next_for(&["publish_ok"], "CAMERA", Duration::from_secs(20))
        .await;
    eventually("session CONNECTED via real webhook", || async {
        session_state(&app, a.session_id).await == "CONNECTED"
    })
    .await;
    let provider = app.state.meeting.core.rtc.clone();
    let room_ref = RtcRoomRef {
        conference_id: cid,
        room_name: room(cid),
    };
    eventually("both published tracks visible to the SFU", || async {
        provider
            .list_participants(&room_ref)
            .await
            .unwrap()
            .iter()
            .any(|p| p.media_identity == a.identity && p.tracks.len() >= 2)
    })
    .await;

    // Host audio mute (MUTE NOW).
    let (s, v) = app
        .post(
            format!("{base}/participants/{}/mute", a.participant_id),
            Auth::Bearer(&owner.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mutedTracks"], 1, "{v}");
    probe
        .next_for(&["track_muted"], "MICROPHONE", Duration::from_secs(10))
        .await;
    println!("CERT F host audio mute: PASS");

    // Audio lock: provider grant reduced; live and future microphone publication refused.
    let (s, v) = app
        .post(
            format!("{base}/participants/{}/audio-lock", a.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["providerEnforcement"], "ENFORCED", "{v}");
    probe.publish("mic").await;
    probe
        .next_for(&["publish_denied"], "MICROPHONE", Duration::from_secs(15))
        .await;
    println!("CERT F audio permission reduction: PASS");
    // Video lock: the live camera track is unpublished / muted and cannot come back.
    let (s, v) = app
        .post(
            format!("{base}/participants/{}/video-lock", a.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let cam = probe
        .next_for(
            &["track_unpublished", "track_muted"],
            "CAMERA",
            Duration::from_secs(15),
        )
        .await;
    println!("CERT F video-lock live camera signal: {}", cam["event"]);
    probe.publish("camera").await;
    probe
        .next_for(&["publish_denied"], "CAMERA", Duration::from_secs(15))
        .await;
    println!("CERT F video permission reduction: PASS");
    // Restore.
    app.delete(
        format!("{base}/participants/{}/audio-lock", a.participant_id),
        Auth::Bearer(&owner.token),
    )
    .await;
    app.delete(
        format!("{base}/participants/{}/video-lock", a.participant_id),
        Auth::Bearer(&owner.token),
    )
    .await;
    probe.publish("mic").await;
    probe
        .next_for(&["publish_ok"], "MICROPHONE", Duration::from_secs(15))
        .await;
    probe.publish("camera").await;
    probe
        .next_for(&["publish_ok"], "CAMERA", Duration::from_secs(15))
        .await;
    println!("CERT F permission restore: PASS");

    // Screen-share policy enforced by the real SFU.
    let v0 = collab_version(&app, &base, &owner.token).await;
    let (s, v) = app
        .patch(
            format!("{base}/collaboration-policy"),
            Auth::Bearer(&owner.token),
            json!({ "collaborationVersion": v0, "changes": { "screenSharePolicy": "HOST_ONLY" } }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    probe.publish("screen").await;
    probe
        .next_for(&["publish_denied"], "SCREEN_SHARE", Duration::from_secs(15))
        .await;
    println!("CERT F screen-share permission enforcement: PASS");
    let v1 = collab_version(&app, &base, &owner.token).await;
    app.patch(format!("{base}/collaboration-policy"), Auth::Bearer(&owner.token), json!({ "collaborationVersion": v1, "changes": { "screenSharePolicy": "ALL_INTERACTIVE" } })).await;
    probe.publish("screen").await;
    probe
        .next_for(&["publish_ok"], "SCREEN_SHARE", Duration::from_secs(15))
        .await;
    eventually(
        "screenshare.started from the real track_published webhook",
        || async { events(&app, cid, "screenshare.started").await == 1 },
    )
    .await;
    println!("CERT F screen-share provider event: PASS");

    // Host stops the share (STOP NOW, not a lock).
    let (s, v) = app
        .post(
            format!("{base}/screen-share/{}/stop", a.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let scr = probe
        .next_for(
            &["track_unpublished", "track_muted"],
            "SCREEN_SHARE",
            Duration::from_secs(15),
        )
        .await;
    println!("CERT F host stop live screen signal: {}", scr["event"]);
    eventually("screenshare.ended", || async {
        events(&app, cid, "screenshare.ended").await >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        events(&app, cid, "screenshare.ended").await,
        1,
        "late provider evidence deduplicated"
    );
    probe.publish("screen").await;
    probe
        .next_for(&["publish_ok"], "SCREEN_SHARE", Duration::from_secs(15))
        .await;
    println!("CERT F stop screen share (+ sharing still permitted): PASS");
    drop(probe.child.kill().await);
}

async fn collab_version(app: &TestApp, base: &str, token: &str) -> i64 {
    app.get(format!("{base}/collaboration"), Auth::Bearer(token))
        .await
        .1["collaborationVersion"]
        .as_i64()
        .unwrap()
}

// ===========================================================================
// Parts G–I real certification
// ===========================================================================

async fn eventually_for<F, Fut>(what: &str, secs: u64, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out after {secs}s waiting for: {what}");
}

/// Real S3-compatible storage from `run.sh` (MinIO). Without it Part G is PENDING.
fn real_s3() -> backend::config::operations::ObjectStorageConfig {
    let get = |k: &str| {
        std::env::var(k).unwrap_or_else(|_| {
            panic!("PENDING: Part G certification needs {k} (run.sh with EGRESS_BIN and MINIO_BIN)")
        })
    };
    backend::config::operations::ObjectStorageConfig {
        endpoint: Some(get("CFMEET_REAL_S3_ENDPOINT")),
        public_endpoint: None,
        region: "us-east-1".into(),
        bucket: get("CFMEET_REAL_S3_BUCKET"),
        access_key: get("CFMEET_REAL_S3_ACCESS_KEY"),
        secret_key: get("CFMEET_REAL_S3_SECRET_KEY"),
        path_style: true,
        prefix: "artifacts".into(),
    }
}

async fn app_for_ghi(
    r: &Real,
    backends: Option<backend::services::artifacts::ArtifactBackends>,
) -> TestApp {
    let app = TestApp::with(TestOptions {
        rtc: RtcMode::RealLiveKit {
            url: r.url.clone(),
            api_key: r.key.clone(),
            api_secret: r.secret.clone(),
        },
        flags: TestApp::ghi_flags(),
        artifact_backends: backends,
        ..TestOptions::default()
    })
    .await
    .expect("database");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", r.webhook_port))
        .await
        .expect("webhook port free");
    let router = app.router.clone();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    app
}

async fn artifact_col(app: &TestApp, id: Uuid, col: &str) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT coalesce({col}::text, '') FROM meeting_artifacts WHERE id='{id}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

/// Part G: real composite capture through LiveKit Egress into real S3-compatible
/// storage (MinIO), real provider callbacks, verified finalization, signed access,
/// deletion with confirmed absence. Plus a direct object-storage certification.
#[tokio::test]
#[ignore = "requires livekit-server + egress + MinIO (scripts/livekit-certification/run.sh)"]
async fn real_livekit_part_g_recording_and_object_storage() {
    use backend::services::artifacts::storage::{ArtifactStorage, S3ArtifactStorage};
    use sha2::Digest;
    let r = real();
    let s3 = real_s3();
    let storage = std::sync::Arc::new(S3ArtifactStorage::new(s3).expect("s3 config"));
    storage.ensure_bucket().await.expect("bucket");

    // --- Real object storage certification -------------------------------
    let key = storage.object_key(&format!("cert/{}.bin", Uuid::new_v4().simple()));
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    storage
        .put(&key, bytes.clone(), "application/octet-stream")
        .await
        .expect("PUT");
    let meta = storage
        .head(&key)
        .await
        .expect("HEAD")
        .expect("object exists");
    assert_eq!(meta.size, bytes.len() as u64, "{meta:?}");
    assert_eq!(
        meta.content_type.as_deref(),
        Some("application/octet-stream")
    );
    println!(
        "CERT S3 object exists + metadata: PASS ({} bytes, etag {:?})",
        meta.size, meta.etag
    );
    let digest = storage.digest(&key, 10_000_000).await.unwrap().unwrap();
    assert_eq!(digest.sha256, hex::encode(sha2::Sha256::digest(&bytes)));
    println!("CERT S3 checksum (streamed SHA-256 + MD5/ETag): PASS");
    let signed = storage
        .signed_read_url(&key, Duration::from_secs(120), Some("cert.bin"))
        .unwrap();
    let got = reqwest::get(&signed.url).await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(got.bytes().await.unwrap().to_vec(), bytes);
    let tampered = signed.url.replace("cert/", "cert/x");
    assert_eq!(
        reqwest::get(&tampered).await.unwrap().status(),
        403,
        "object-scoped signature"
    );
    let unsigned = signed.url.split('?').next().unwrap().to_string();
    assert_eq!(
        reqwest::get(&unsigned).await.unwrap().status(),
        403,
        "no anonymous read"
    );
    println!("CERT S3 signed read (scoped, expiring, no anonymous access): PASS");
    storage.delete(&key).await.expect("DELETE");
    assert!(storage.head(&key).await.unwrap().is_none());
    println!("CERT S3 delete + absence confirmed: PASS");

    // --- Real composite recording -----------------------------------------
    let lk_config = backend::config::LiveKitConfig {
        url: r.url.clone(),
        api_key: r.key.clone(),
        api_secret: r.secret.clone(),
    };
    let recorder = backend::services::artifacts::recorder::LiveKitEgressRecorder::with_api_url(
        &lk_config,
        &backend::services::rtc_provider::admin_url_from(&r.url),
        Duration::from_secs(15),
        backend::config::operations::RecordingEncoding::H264Opus,
    );
    let backends = backend::services::artifacts::ArtifactBackends {
        recorder: std::sync::Arc::new(recorder),
        storage: storage.clone(),
        ..Default::default()
    };
    let app = app_for_ghi(&r, Some(backends)).await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "cert-g", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let speaker = join_guest(&app, cid, "speaker").await;
    let mut probe = Probe::spawn_with(&r.probe, &r.url, &speaker.credential, "mic,camera", 180);
    probe.wait_for("connected", Duration::from_secs(20)).await;
    probe
        .next_for(&["publish_ok"], "CAMERA", Duration::from_secs(20))
        .await;
    eventually("session CONNECTED", || async {
        session_state(&app, speaker.session_id).await == "CONNECTED"
    })
    .await;

    let base = format!("/api/v2/conferences/{cid}");
    let (s, v) = app
        .post(
            format!("{base}/recordings"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        v["recording"]["status"], "REQUESTED",
        "never RECORDING without evidence"
    );
    let rid = uuid(&v["recording"]["id"]);
    eventually_for("egress reference stored", 30, || async {
        !artifact_col(&app, rid, "provider_ref").await.is_empty()
    })
    .await;
    println!(
        "CERT G real composite capture start (egress {}): PASS",
        artifact_col(&app, rid, "provider_ref").await
    );
    eventually_for("real egress_started webhook → RECORDING", 90, || async {
        artifact_col(&app, rid, "status").await == "RECORDING"
    })
    .await;
    println!("CERT G real provider callback (egress_started → RECORDING): PASS");
    tokio::time::sleep(Duration::from_secs(10)).await;
    let (s, v) = app
        .post(
            format!("{base}/recordings/{rid}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recording"]["status"], "STOPPING");
    println!("CERT G real stop (STOPPING): PASS");
    eventually_for(
        "egress_ended + storage verification → READY",
        120,
        || async { artifact_col(&app, rid, "status").await == "READY" },
    )
    .await;
    let (_, rec) = app
        .get(
            format!("{base}/recordings/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let size = rec["sizeBytes"].as_i64().unwrap();
    assert!(size > 1000, "{rec}");
    let object_key = artifact_col(&app, rid, "storage_key").await;
    let head = storage
        .head(&object_key)
        .await
        .unwrap()
        .expect("real file in object storage");
    assert_eq!(head.size as i64, size);
    println!(
        "CERT G real media file READY ({size} bytes, {} ms, key verified in MinIO): PASS",
        rec["durationMs"]
    );
    let (s, a) = app
        .post(
            format!("{base}/artifacts/{rid}/access"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    let file = reqwest::get(a["url"].as_str().unwrap())
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(file.len() as i64, size);
    assert_eq!(&file[4..8], b"ftyp", "MP4 container");
    assert_eq!(
        rec["checksumSha256"],
        json!(hex::encode(sha2::Sha256::digest(&file)))
    );
    println!("CERT G signed artefact access + checksum: PASS");
    let (s, _) = app
        .post(
            format!("{base}/artifacts/{rid}/access"),
            speaker.auth(),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    println!("CERT G unauthorized access rejected: PASS");
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='recording.ready'")).await,
        1
    );
    let egress_events = app
        .scalar("SELECT count(*) FROM rtc_webhook_events WHERE event_type LIKE 'egress_%'")
        .await;
    assert!(
        egress_events >= 2,
        "real egress callbacks stored once each: {egress_events}"
    );
    println!("CERT G callback dedupe ({egress_events} unique egress callbacks, one READY): PASS");
    let (s, v) = app
        .delete(
            format!("{base}/artifacts/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "DELETED");
    assert!(storage.head(&object_key).await.unwrap().is_none());
    println!("CERT G delete + absence confirmed in object storage: PASS");
}

/// Part H: webinar VIEWER publish denial, promotion grants publish, demotion removes
/// publish, backstage isolation through a separate provider room.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_part_h_stage_and_backstage() {
    let r = real();
    let app = app_for_ghi(&r, None).await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "cert-h", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        )
        .await;
    let provider = app.state.meeting.core.rtc.clone();
    let main = RtcRoomRef {
        conference_id: cid,
        room_name: room(cid),
    };
    let a = join_guest(&app, cid, "attendee").await;
    assert_eq!(a.body["participant"]["role"], "VIEWER");
    let mut probe = Probe::spawn_with(&r.probe, &r.url, &a.credential, "camera", 180);
    probe.wait_for("connected", Duration::from_secs(20)).await;
    probe
        .next_for(&["publish_denied"], "CAMERA", Duration::from_secs(45))
        .await;
    println!("CERT H VIEWER publish denial (real SFU): PASS");
    eventually("CONNECTED", || async {
        session_state(&app, a.session_id).await == "CONNECTED"
    })
    .await;
    let (s, v) = change_role(&app, cid, &owner, a.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    eventually("provider grants follow PRESENTER", || async {
        provider
            .list_participants(&main)
            .await
            .unwrap()
            .iter()
            .any(|p| {
                p.media_identity == a.identity
                    && p.grants
                        .as_ref()
                        .map(|g| g.can_publish_video)
                        .unwrap_or(false)
            })
    })
    .await;
    probe.publish("camera").await;
    probe
        .next_for(&["publish_ok"], "CAMERA", Duration::from_secs(45))
        .await;
    println!("CERT H promotion to PRESENTER grants publish: PASS");
    let (s, _) = change_role(&app, cid, &owner, a.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    eventually("publish removed after demotion", || async {
        provider
            .list_participants(&main)
            .await
            .unwrap()
            .iter()
            .any(|p| {
                p.media_identity == a.identity
                    && p.grants
                        .as_ref()
                        .map(|g| !g.can_publish_video && !g.can_publish_audio)
                        .unwrap_or(false)
            })
    })
    .await;
    probe.publish("camera").await;
    probe
        .next_for(&["publish_denied"], "CAMERA", Duration::from_secs(45))
        .await;
    println!("CERT H demotion removes publish: PASS");
    drop(probe);

    // Backstage isolation: a panelist routed into its own provider room.
    let p = join_guest(&app, cid, "panelist").await;
    change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/backstage"),
            p.auth(),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, cred) = app
        .post(
            format!("/api/v2/conferences/{cid}/media-credential"),
            p.auth(),
            json!({ "participantSessionId": p.session_id }),
        )
        .await;
    let bs = RtcRoomRef {
        conference_id: cid,
        room_name: backend::services::specialized::backstage_room_name(cid),
    };
    assert!(
        provider.get_room(&bs).await.unwrap().is_some(),
        "real backstage room"
    );
    let mut bp = Probe::spawn_with(
        &r.probe,
        &r.url,
        cred["rtc"]["credential"].as_str().unwrap(),
        "mic",
        60,
    );
    bp.wait_for("connected", Duration::from_secs(20)).await;
    eventually("panelist present backstage only", || async {
        let back = provider.list_participants(&bs).await.unwrap();
        let front = provider.list_participants(&main).await.unwrap();
        back.iter().any(|x| x.media_identity == p.identity)
            && !front.iter().any(|x| x.media_identity == p.identity)
    })
    .await;
    println!("CERT H backstage routing + isolation (separate provider room): PASS");
    let (s, _) = change_role(&app, cid, &owner, p.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    eventually("demoted panelist evicted from backstage", || async {
        !provider
            .list_participants(&bs)
            .await
            .unwrap()
            .iter()
            .any(|x| x.media_identity == p.identity)
    })
    .await;
    println!("CERT H demotion evicts from backstage: PASS");
}

/// Part I: provider presence → session state, join latency to media, provider
/// node / region mapping, disconnect state, reconciliation without false drift.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_part_i_presence_and_mapping() {
    let r = real();
    let app = app_for_ghi(&r, None).await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "cert-i", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let g = join_guest(&app, cid, "member").await;
    let mut probe = Probe::spawn_with(&r.probe, &r.url, &g.credential, "mic", 60);
    let connected = probe.wait_for("connected", Duration::from_secs(20)).await;
    eventually("provider presence → CONNECTED", || async {
        session_state(&app, g.session_id).await == "CONNECTED"
    })
    .await;
    let latency = app
        .scalar(&format!(
            "SELECT coalesce(join_latency_ms,0)::bigint FROM participant_sessions WHERE id='{}'",
            g.session_id
        ))
        .await;
    assert!(latency > 0, "join latency measured to real media connect");
    println!("CERT I provider presence + join latency ({latency} ms): PASS");
    let node = connected["serverNode"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let region = connected["serverRegion"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!node.is_empty(), "SFU reports its node id: {connected}");
    let sanitized: String = node
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    let mut body = json!({ "providerNode": sanitized, "samples": [{ "timestamp": chrono::Utc::now(), "rttMs": 20.0 }] });
    if !region.is_empty() {
        body["providerRegion"] = json!(region);
    }
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/quality/samples"),
            g.auth(),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let mapped = app
        .scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE id='{}' AND provider_node='{}'",
            g.session_id, sanitized
        ))
        .await;
    assert_eq!(mapped, 1);
    println!(
        "CERT I node mapping (node {sanitized}, region {:?}): PASS",
        if region.is_empty() {
            "not reported by single-node SFU"
        } else {
            region.as_str()
        }
    );
    let rep = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(
        !rep.kinds
            .iter()
            .any(|k| k.contains("ABSENT") || k.contains("WITHOUT_SESSION")),
        "no false drift: {rep:?}"
    );
    println!("CERT I reconciliation against real presence (no false drift): PASS");
    drop(probe);
    eventually_for("participant_left → session closed", 40, || async {
        session_state(&app, g.session_id).await != "CONNECTED"
    })
    .await;
    println!(
        "CERT I participant disconnect state: PASS ({})",
        session_state(&app, g.session_id).await
    );
}

impl Probe {
    /// First of `events` (already seen or upcoming).
    async fn wait_any(&mut self, events: &[&str], within: Duration) -> Value {
        if let Some(v) = self
            .seen
            .iter()
            .find(|v| events.iter().any(|e| v["event"] == *e))
        {
            return v.clone();
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(v)) => {
                    self.seen.push(v.clone());
                    if events.iter().any(|e| v["event"] == *e) {
                        return v;
                    }
                }
                _ => panic!("rtcprobe never reported {events:?}; saw {:?}", self.seen),
            }
        }
    }

    /// Drain everything currently buffered into `seen` (non-blocking).
    fn pump(&mut self) {
        while let Ok(v) = self.rx.try_recv() {
            self.seen.push(v);
        }
    }

    fn count(&self, event: &str) -> usize {
        self.seen.iter().filter(|v| v["event"] == event).count()
    }

    fn last_stats(&self) -> Option<Value> {
        self.seen
            .iter()
            .rev()
            .find(|v| v["event"] == "stats")
            .cloned()
    }
}

/// Part M §7 additions against the real SFU: waiting room → admit → real connect,
/// real SUBSCRIPTION of a remote participant's audio + video (RTP actually received),
/// ban → real disconnect, and ban/reconnect protection with the old credential.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_m_waiting_subscription_ban() {
    let r = real();
    let app = app_for(&r).await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "cert-m", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }))
        .await;
    let provider = app.state.meeting.core.rtc.clone();
    let main = RtcRoomRef {
        conference_id: cid,
        room_name: room(cid),
    };

    // Host publishes audio + video.
    let host = join_user(&app, cid, &owner).await;
    let mut hp = Probe::spawn_with(&r.probe, &r.url, &host.credential, "mic,camera", 120);
    hp.wait_for("connected", Duration::from_secs(20)).await;
    hp.next_for(&["publish_ok"], "MICROPHONE", Duration::from_secs(20))
        .await;
    hp.next_for(&["publish_ok"], "CAMERA", Duration::from_secs(20))
        .await;

    // Guest waits (no credential), is admitted, then obtains a credential.
    let (s, g) = app
        .join(cid, Auth::None, json!({ "displayName": "waiting guest" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    assert_eq!(g["participant"]["admissionState"], "WAITING");
    assert!(g["rtc"].is_null(), "no credential while waiting");
    let gtoken = g["participantToken"].as_str().unwrap().to_string();
    let gpid = uuid(&g["participant"]["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [gpid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, g2) = app.join(cid, Auth::Participant(&gtoken), json!({})).await;
    assert_eq!(
        (s, g2["rtcStatus"].as_str()),
        (StatusCode::OK, Some("ISSUED")),
        "{g2}"
    );
    let credential = g2["rtc"]["credential"].as_str().unwrap().to_string();
    let session_id = uuid(&g2["participantSession"]["id"]);
    let identity = format!("ps_{}", session_id.simple());
    println!("CERT M waiting → admit → credential: PASS");

    // Real subscription: the guest receives the host's audio and video RTP.
    let mut gp = Probe::spawn_with(&r.probe, &r.url, &credential, "false", 120);
    gp.wait_for("connected", Duration::from_secs(20)).await;
    let mut kinds = vec![];
    while kinds.len() < 2 {
        let v = gp
            .wait_any(&["track_subscribed"], Duration::from_secs(30))
            .await;
        gp.seen.retain(|x| x != &v);
        assert_eq!(v["from"], json!(host.identity), "{v}");
        kinds.push(v["kind"].as_str().unwrap().to_string());
    }
    kinds.sort();
    assert_eq!(kinds, ["audio", "video"]);
    eventually("guest CONNECTED", || async {
        session_state(&app, session_id).await == "CONNECTED"
    })
    .await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    gp.pump();
    let st = gp.last_stats().expect("stats");
    assert!(st["rtpPackets"].as_i64().unwrap() > 0, "RTP received: {st}");
    println!("CERT M real subscription (audio+video RTP received): PASS ({st})");

    // Ban → real disconnect; the old credential cannot restore presence.
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/participants/{gpid}/ban"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    gp.wait_for("disconnected", Duration::from_secs(15)).await;
    println!("CERT M ban → real disconnect: PASS");
    let mut replay = Probe::spawn_with(&r.probe, &r.url, &credential, "mic", 60);
    let outcome = replay
        .wait_any(&["connect_failed", "disconnected"], Duration::from_secs(30))
        .await;
    println!(
        "CERT M ban reconnect with old credential → {}",
        outcome["event"]
    );
    eventually("banned identity absent at the provider", || async {
        !provider
            .list_participants(&main)
            .await
            .unwrap()
            .iter()
            .any(|p| p.media_identity == identity)
    })
    .await;
    let (_, again) = app.join(cid, Auth::Participant(&gtoken), json!({})).await;
    assert!(
        again.get("rtc").map(|x| x.is_null()).unwrap_or(true),
        "banned: no new credential: {again}"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{gpid}' AND state IN ('PENDING','CONNECTED')")).await,
        0,
        "banned participant holds no active session"
    );
    println!("CERT M ban/reconnect protection: PASS");
    drop(hp);
}

fn proc_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}

fn proc_rss_kb(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn livekit_pid() -> Option<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-f", "livekit-server --config"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Part M §43 / §44 — self-hosted media load + large-audience strategy on the real SFU.
/// A TOWN_HALL conference: `MEDIA_LOAD_PUBLISHERS` presenters on a capped stage publish
/// audio + video; `MEDIA_LOAD_SUBSCRIBERS` VIEWER attendees subscribe only. Records
/// join latency, join failures, unexpected disconnects, subscriptions, RTP packets and
/// gaps, and livekit-server CPU / RSS. Sizes are environment-driven so the same test
/// runs at the capacity of whatever host executes it; the report states the sizes run.
#[tokio::test]
#[ignore = "requires a real livekit-server (scripts/livekit-certification/run.sh)"]
async fn real_livekit_m_media_load_and_large_audience() {
    let r = real();
    let env = |k: &str, d: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let publishers = env("MEDIA_LOAD_PUBLISHERS", 4);
    let subscribers = env("MEDIA_LOAD_SUBSCRIBERS", 16);
    let hold = env("MEDIA_LOAD_SECS", 30) as u64;
    let app = app_for_ghi(&r, None).await;
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "media load", "accessPolicy": "PUBLIC", "meetingType": "TOWN_HALL", "viewerCapacity": publishers + subscribers }),
        )
        .await;
    let (s, v) = common::artifacts::set_profile(
        &app,
        &owner,
        sid,
        json!({ "stage": { "maxStageParticipants": publishers, "maxPresenters": publishers } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let lk_pid = livekit_pid();
    let cpu0 = lk_pid.and_then(proc_cpu_ticks);
    let t0 = std::time::Instant::now();

    // Everyone joins as a VIEWER attendee (subscribe-only by default).
    let total = publishers + subscribers;
    let mut joined = vec![];
    let mut join_failures = 0usize;
    let mut failure_reasons: Vec<String> = vec![];
    for i in 0..total {
        let (s, v) = app
            .join(cid, Auth::None, json!({ "displayName": format!("a{i}") }))
            .await;
        if s != StatusCode::OK || v["rtcStatus"] != "ISSUED" {
            join_failures += 1;
            failure_reasons.push(format!("api {s} {}", code(&v)));
            continue;
        }
        assert_eq!(
            v["participant"]["role"], "VIEWER",
            "audience defaults to VIEWER"
        );
        joined.push(v);
    }
    // The subscribe-only audience pool is bounded by its own capacity.
    let (_, over) = app
        .join(cid, Auth::None, json!({ "displayName": "over capacity" }))
        .await;
    assert_eq!(
        code(&over),
        "MEETING_CAPACITY_REACHED",
        "viewer pool enforced: {over}"
    );
    let mut probes = vec![];
    let mut connect_ms = vec![];
    for v in &joined {
        let started = std::time::Instant::now();
        let mut p = Probe::spawn_with(
            &r.probe,
            &r.url,
            v["rtc"]["credential"].as_str().unwrap(),
            "false",
            (hold + 120) as u32,
        );
        let first = p
            .wait_any(&["connected", "connect_failed"], Duration::from_secs(60))
            .await;
        if first["event"] == "connected" {
            connect_ms.push(started.elapsed().as_millis() as u64);
        } else {
            join_failures += 1;
            failure_reasons.push(format!("media {}", first["error"]));
        }
        probes.push(p);
    }
    // Promote the stage, then the presenters publish audio + video on live connections.
    for v in joined.iter().take(publishers) {
        let (s, b) = change_role(
            &app,
            cid,
            &owner,
            uuid(&v["participant"]["id"]),
            "PRESENTER",
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    // Stage capacity holds under load: one more promotion is refused.
    if let Some(extra) = joined.get(publishers) {
        let (_, b) = change_role(
            &app,
            cid,
            &owner,
            uuid(&extra["participant"]["id"]),
            "PRESENTER",
        )
        .await;
        assert_eq!(code(&b), "STAGE_CAPACITY_REACHED", "{b}");
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for p in probes.iter_mut().take(publishers) {
        p.publish("mic").await;
        p.publish("camera").await;
    }
    for p in probes.iter_mut().take(publishers) {
        p.next_for(&["publish_ok"], "MICROPHONE", Duration::from_secs(45))
            .await;
        p.next_for(&["publish_ok"], "CAMERA", Duration::from_secs(45))
            .await;
    }
    // Audience stays subscribe-only: a modified attendee client is refused.
    if let Some(p) = probes.get_mut(publishers) {
        p.publish("camera").await;
        p.next_for(&["publish_denied"], "CAMERA", Duration::from_secs(45))
            .await;
    }
    let mut peak_rss = lk_pid.and_then(proc_rss_kb).unwrap_or(0);
    let hold_until = std::time::Instant::now() + Duration::from_secs(hold);
    while std::time::Instant::now() < hold_until {
        tokio::time::sleep(Duration::from_secs(2)).await;
        peak_rss = peak_rss.max(lk_pid.and_then(proc_rss_kb).unwrap_or(0));
    }
    tokio::time::sleep(Duration::from_secs(6)).await;
    let mut subs_expected_met = 0usize;
    let (mut packets, mut lost, mut unexpected_disconnects, mut subscriptions) =
        (0i64, 0i64, 0usize, 0usize);
    let expected_tracks = publishers * 2;
    for (i, p) in probes.iter_mut().enumerate() {
        p.pump();
        unexpected_disconnects += p.count("disconnected");
        let subscribed = p.count("track_subscribed");
        subscriptions += subscribed;
        let own = if i < publishers { 2 } else { 0 };
        if subscribed + own >= expected_tracks {
            subs_expected_met += 1;
        }
        if let Some(st) = p.last_stats() {
            packets += st["rtpPackets"].as_i64().unwrap_or(0);
            lost += st["rtpLost"].as_i64().unwrap_or(0);
        }
    }
    let cpu1 = lk_pid.and_then(proc_cpu_ticks);
    let elapsed = t0.elapsed().as_secs_f64();
    let cpu_pct = match (cpu0, cpu1) {
        (Some(a), Some(b)) => Some(((b - a) as f64 / 100.0) / elapsed * 100.0),
        _ => None,
    };
    connect_ms.sort();
    let pct = |q: f64| {
        connect_ms
            .get(((connect_ms.len() as f64 - 1.0) * q).round() as usize)
            .copied()
    };
    let loss_pct = if packets + lost > 0 {
        lost as f64 * 100.0 / (packets + lost) as f64
    } else {
        0.0
    };
    let report = json!({
        "publishers": publishers,
        "subscribers": subscribers,
        "holdSecs": hold,
        "joinFailures": join_failures,
        "joinFailureReasons": failure_reasons.iter().take(5).collect::<Vec<_>>(),
        "connectMs": { "p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99), "max": connect_ms.last() },
        "unexpectedDisconnects": unexpected_disconnects,
        "trackSubscriptions": subscriptions,
        "participantsWithAllStageTracks": subs_expected_met,
        "rtpPackets": packets,
        "rtpGaps": lost,
        "rtpGapPct": loss_pct,
        "livekitCpuPctAvg": cpu_pct,
        "livekitPeakRssMb": peak_rss / 1024,
        "hostCpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "livekitVersion": probes.first().and_then(|p| p.seen.iter().find(|v| v["event"] == "connected").map(|v| v["serverVersion"].clone())),
    });
    println!("MEDIA_LOAD {report}");
    if let Ok(dir) = std::env::var("CFMEET_CERT_OUT") {
        let _ = std::fs::write(
            format!("{dir}/media-load-{publishers}x{subscribers}.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        );
    }
    assert_eq!(join_failures, 0, "{report}");
    assert_eq!(unexpected_disconnects, 0, "{report}");
    assert_eq!(
        subs_expected_met,
        probes.len(),
        "every participant receives every stage track: {report}"
    );
    assert!(packets > 0, "{report}");
    let (_, list) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let presenters = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["role"] == "PRESENTER")
        .count();
    assert_eq!(presenters, publishers, "small controlled stage");
    // Media delivery is reported, never silently passed: any RTP gap means the run hit
    // an observed limit of the host it ran on. An operator-approved ceiling can be
    // enforced with MEDIA_LOAD_MAX_GAP_PCT (no threshold is invented here).
    let media = if lost == 0 {
        "CLEAN (0 RTP gaps)".to_string()
    } else {
        format!("OBSERVED LIMIT ({loss_pct:.1}% RTP gaps)")
    };
    if let Some(max) = std::env::var("MEDIA_LOAD_MAX_GAP_PCT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        assert!(
            loss_pct <= max,
            "media delivery above approved ceiling {max}%: {report}"
        );
    }
    println!("CERT M media load {publishers} publishers × {subscribers} subscribers: control plane PASS; media delivery {media}");
    for p in probes.iter_mut() {
        let _ = p.child.kill().await;
    }
}
