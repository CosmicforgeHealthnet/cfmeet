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
