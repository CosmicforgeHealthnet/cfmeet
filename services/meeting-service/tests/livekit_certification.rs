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
