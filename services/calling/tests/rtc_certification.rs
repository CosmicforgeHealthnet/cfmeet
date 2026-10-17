//! Calling RTC certification against a REAL self-hosted LiveKit server (Part C §49,
//! §105–§111): the LiveKit adapter, real WebRTC clients (LiveKit's Python RTC SDK driving
//! actual media) and real signed webhooks delivered to Calling's callback route on a live
//! listener.
//!
//! Configuration (the certification workflow sets it; normal development needs neither
//! Docker nor LiveKit):
//!
//! * `CALLING_RTC_CERT_LIVEKIT_URL` (e.g. `ws://127.0.0.1:7880`),
//!   `CALLING_RTC_CERT_LIVEKIT_API_URL` (e.g. `http://127.0.0.1:7880`),
//!   `CALLING_RTC_CERT_LIVEKIT_API_KEY`, `CALLING_RTC_CERT_LIVEKIT_API_SECRET` — the server
//!   started with `infra/livekit/livekit.calling-cert.yaml`;
//! * `CALLING_RTC_CERT_PYTHON` — a Python with the `livekit` RTC SDK;
//! * `CALLING_RTC_CERT_WEBHOOK_PORT` (default 18089) — where LiveKit delivers webhooks;
//! * `CALLING_RTC_CERT_REQUIRED=1` turns a missing configuration into a failure (CI);
//! * application capture through REAL LiveKit Egress (§60 RTC voicemail, §61 RTC call
//!   recording): `CALLING_RTC_CERT_EGRESS_S3_ENDPOINT`, `…_S3_BUCKET`, `…_S3_ACCESS_KEY`,
//!   `…_S3_SECRET_KEY` (the private test bucket the Egress worker started with
//!   `infra/livekit/egress/egress.calling-cert.yaml` uploads to; LiveKit started with
//!   `infra/livekit/egress/livekit.calling-egress-cert.yaml`), `ffprobe` / `ffmpeg` on the
//!   PATH to verify the captured audio; `CALLING_RTC_CERT_EGRESS_REQUIRED=1` makes them
//!   mandatory.
//!
//! Without configuration the suite prints SKIPPED (never a silent pass).

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use calling::{
    calls::{ids::CallId, CallState},
    media::{
        gateway::ConnectionClaims,
        livekit::{LiveKitAdapter, LiveKitSettings, EDGE_ISSUER},
        livekit_gateway::LiveKitGateway,
        provider::{
            CallMediaProvider, MediaCredentialRequest, MediaParticipantRef, MediaResourceName,
            MediaSessionRef, MediaSessionSpec,
        },
        MediaPermissions,
    },
    signaling::InMemorySignalBus,
};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    fake_media::FakeMediaProvider,
    test_runtime, Harness, TestDb,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout},
};
use uuid::Uuid;

struct CertConfig {
    url: String,
    api_url: String,
    key: String,
    secret: String,
    python: String,
    port: u16,
    /// Per-run media-edge key (never known to LiveKit).
    edge_key: String,
    /// The protected test bucket of the Egress capture scenarios.
    egress: Option<cf_object_storage::S3Config>,
}

impl CertConfig {
    /// Calling's media connection gateway on the certification listener.
    fn gateway_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v1/calling/media", self.port)
    }
}

fn config() -> Option<CertConfig> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let all = (
        get("CALLING_RTC_CERT_LIVEKIT_URL"),
        get("CALLING_RTC_CERT_LIVEKIT_API_URL"),
        get("CALLING_RTC_CERT_LIVEKIT_API_KEY"),
        get("CALLING_RTC_CERT_LIVEKIT_API_SECRET"),
        get("CALLING_RTC_CERT_PYTHON"),
    );
    match all {
        (Some(url), Some(api_url), Some(key), Some(secret), Some(python)) => Some(CertConfig {
            url,
            api_url,
            key,
            secret,
            python,
            port: get("CALLING_RTC_CERT_WEBHOOK_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(18089),
            edge_key: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            egress: egress_config(),
        }),
        _ => {
            assert!(
                std::env::var("CALLING_RTC_CERT_REQUIRED").as_deref() != Ok("1"),
                "CALLING_RTC_CERT_REQUIRED=1 but the LiveKit certification configuration is incomplete"
            );
            eprintln!("SKIPPED: RTC certification (CALLING_RTC_CERT_* not configured)");
            None
        }
    }
}

fn egress_config() -> Option<cf_object_storage::S3Config> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (
        get("CALLING_RTC_CERT_EGRESS_S3_ENDPOINT"),
        get("CALLING_RTC_CERT_EGRESS_S3_BUCKET"),
        get("CALLING_RTC_CERT_EGRESS_S3_ACCESS_KEY"),
        get("CALLING_RTC_CERT_EGRESS_S3_SECRET_KEY"),
    ) {
        (Some(endpoint), Some(bucket), Some(access_key), Some(secret_key)) => {
            Some(cf_object_storage::S3Config {
                endpoint,
                public_endpoint: None,
                region: get("CALLING_RTC_CERT_EGRESS_S3_REGION")
                    .unwrap_or_else(|| "us-east-1".to_string()),
                bucket,
                access_key,
                secret_key,
                path_style: true,
                server_side_encryption: None,
            })
        }
        _ => {
            assert!(
                std::env::var("CALLING_RTC_CERT_EGRESS_REQUIRED").as_deref() != Ok("1"),
                "CALLING_RTC_CERT_EGRESS_REQUIRED=1 but the Egress capture configuration is incomplete"
            );
            None
        }
    }
}

fn adapter(cfg: &CertConfig) -> LiveKitAdapter {
    let pairs = [
        ("CALLING_LIVEKIT_URL", cfg.url.clone()),
        ("CALLING_LIVEKIT_API_URL", cfg.api_url.clone()),
        ("CALLING_LIVEKIT_API_KEY", cfg.key.clone()),
        ("CALLING_LIVEKIT_API_SECRET", cfg.secret.clone()),
        ("CALLING_MEDIA_GATEWAY_URL", cfg.gateway_url()),
        ("CALLING_MEDIA_EDGE_SIGNING_KEY", cfg.edge_key.clone()),
    ];
    let settings = LiveKitSettings::from_lookup(&|k: &str| {
        pairs
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
    })
    .expect("certification LiveKit settings");
    LiveKitAdapter::new(settings).unwrap()
}

/// Each client's stderr (SDK warnings / errors) goes to its own log file under
/// `target/tmp/rtc-clients/` for diagnosis (printed by CI when the suite fails).
fn client_log() -> std::process::Stdio {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rtc-clients");
    let _ = std::fs::create_dir_all(&dir);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match std::fs::File::create(dir.join(format!("client-{}-{n:03}.log", std::process::id()))) {
        Ok(file) => file.into(),
        Err(_) => std::process::Stdio::null(),
    }
}

/// A real WebRTC client process.
struct RtcClient {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl RtcClient {
    async fn spawn(cfg: &CertConfig) -> Self {
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rtc/rtc_client.py");
        let mut child = tokio::process::Command::new(&cfg.python)
            .arg("-I")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(client_log())
            .kill_on_drop(true)
            .spawn()
            .expect("python RTC client");
        let stdin = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            stdin,
            lines,
        }
    }

    async fn cmd(&mut self, v: Value) -> Value {
        self.stdin
            .write_all(format!("{v}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("client answers")
            .unwrap()
            .expect("client alive");
        serde_json::from_str(&line).unwrap()
    }

    async fn connect(&mut self, url: &str, token: &str) -> Value {
        self.cmd(json!({ "cmd": "connect", "url": url, "token": token }))
            .await
    }

    async fn status(&mut self) -> Value {
        self.cmd(json!({ "cmd": "status" })).await
    }

    async fn quit(mut self) {
        let _ = self.cmd(json!({ "cmd": "quit" })).await;
        let _ = self.child.kill().await;
    }
}

/// Polls `f` until it holds (or fails after `secs`).
async fn eventually<F, Fut>(secs: u64, what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct Cert {
    cfg: CertConfig,
    a: AppWorld,
    lk: Arc<LiveKitAdapter>,
    /// The protected artifact store when the Egress capture scenarios run.
    store: Option<Arc<cf_object_storage::S3ObjectStore>>,
}

impl Cert {
    async fn new(cfg: CertConfig) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        let lk = Arc::new(adapter(&cfg));
        let bus = Arc::new(InMemorySignalBus::new());
        let mut runtime = test_runtime(Some(lk.clone()), bus.clone());
        // Every client media connection passes Calling's admission gateway.
        runtime.media_gateway = Some(Arc::new(LiveKitGateway::new(lk.clone())));
        // Application capture: the REAL LiveKit Egress adapter over the protected bucket.
        let store = match &cfg.egress {
            Some(config) => {
                let store = Arc::new(
                    cf_object_storage::S3ObjectStore::new(config.clone())
                        .expect("egress test bucket"),
                );
                store.ensure_bucket().await.expect("egress test bucket");
                runtime.artifacts = store.clone();
                runtime.capture =
                    Arc::new(calling::media::livekit_egress::LiveKitEgressCapture::new(
                        lk.clone(),
                        store.clone(),
                        &calling::media::livekit_egress::CaptureSettings {
                            egress: true,
                            staging_prefix: "capture-staging".to_string(),
                        },
                    ));
                Some(store)
            }
            None => None,
        };
        let h = Harness::with_runtime(
            db,
            calling::domain::ManualClock::new(chrono::Utc::now()),
            Arc::new(FakeMediaProvider::default()),
            bus.clone(),
            runtime,
        );
        // Real listener: LiveKit delivers its signed webhooks here.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.port))
            .await
            .expect("certification webhook port");
        let router = h.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let w =
            CallWorld::in_harness(h, "cert", calling::domain::EnvironmentKind::Production).await;
        Some(Self {
            cfg,
            a: AppWorld { w },
            lk,
            store,
        })
    }

    async fn credential(&self, call: Uuid, session: Uuid) -> Value {
        let r = self.a.media_credential(call, session).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        r.json
    }

    async fn state(&self, call: Uuid) -> CallState {
        self.a.state(call).await.0
    }

    async fn session_admitted(&self, call: Uuid, session: Uuid) -> bool {
        self.a.call_view(call).await["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| {
                s["sessionId"] == session.to_string()
                    && s["state"] == "CONNECTED"
                    && !s["mediaAdmittedAt"].is_null()
            })
    }

    async fn provider_identities(&self, call: Uuid) -> Vec<(String, bool, MediaPermissions)> {
        self.lk
            .list_participants(&room(call))
            .await
            .unwrap()
            .into_iter()
            .map(|p| (p.identity, p.active, p.permissions))
            .collect()
    }

    /// Ringing → answered call (one device each), returning sessions too.
    async fn answered(&self, kind: &str, tag: &str) -> (Uuid, Device, Device, Uuid, Uuid) {
        let caller = self
            .a
            .device(&self.a.w.initiator, &format!("{tag}-caller"))
            .await;
        let callee = self
            .a
            .device(&self.a.w.recipient, &format!("{tag}-callee"))
            .await;
        let r = self
            .a
            .create_call(
                kind,
                &self.a.w.initiator,
                &caller,
                &[&self.a.w.recipient],
                &format!("{tag}-create"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        assert_eq!(r.json["call"]["state"], "RINGING");
        let invitation = self.a.invitation_for(call, callee.endpoint).await.unwrap();
        let (_, version) = self.a.state(call).await;
        let r = self.a.accept(call, invitation, &callee, version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        let s1 = self.a.session_on(call, caller.endpoint).await.unwrap();
        let s2 = self.a.session_on(call, callee.endpoint).await.unwrap();
        (call, caller, callee, s1, s2)
    }
}

fn room(call: Uuid) -> MediaSessionRef {
    MediaSessionRef {
        name: MediaResourceName::for_call(CallId::from_uuid(call)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_livekit_certification() {
    let Some(cfg) = config() else {
        return;
    };
    let Some(cert) = Cert::new(cfg).await else {
        return;
    };
    common::provider_contract::run(cert.lk.as_ref()).await;
    adapter_contract(&cert).await;
    forged_and_duplicate_callbacks(&cert).await;
    voice_call(&cert).await;
    enterprise_extension_call(&cert).await;
    enterprise_ring_groups(&cert).await;
    enterprise_queue(&cert).await;
    enterprise_transfer(&cert).await;
    if cert.store.is_some() {
        rtc_voicemail_through_egress(&cert).await;
        rtc_recording_through_egress(&cert).await;
    } else {
        eprintln!("SKIPPED: RTC Egress capture (CALLING_RTC_CERT_EGRESS_* not configured)");
    }
    video_call_and_removal(&cert).await;
    for round in 0..3 {
        refreshed_privileged_token_cannot_reenter(&cert, round).await;
    }
    superseded_generation_and_terminal_call_tokens_are_refused(&cert).await;
    legitimate_reconnect_with_the_current_generation(&cert).await;
    group_call(&cert).await;
    reconciliation(&cert).await;
    unknown_participant_quarantine(&cert).await;
}

/// Adapter contract against the real server: deterministic resource, idempotent ensure,
/// join-only grants, listing, permission update, removal, termination.
async fn adapter_contract(cert: &Cert) {
    let call = Uuid::new_v4();
    let session = room(call);
    assert_eq!(session.name.as_str(), format!("cfc-{}", call.simple()));
    let spec = MediaSessionSpec {
        max_participants: 4,
        empty_timeout: chrono::Duration::minutes(2),
    };
    let first = cert
        .lk
        .create_or_ensure_session(&session, &spec)
        .await
        .unwrap();
    let again = cert
        .lk
        .create_or_ensure_session(&session, &spec)
        .await
        .unwrap();
    assert_eq!(
        first.resource_ref, again.resource_ref,
        "ensure is idempotent"
    );
    assert!(first.resource_ref.is_some());
    let snapshot = cert.lk.reconcile(&session).await.unwrap();
    assert!(snapshot.exists && snapshot.participants.is_empty());
    // A join-only credential: the client joins but holds no publish / subscribe right.
    let identity = format!("cfmid_{}", "a".repeat(64));
    let credential = cert
        .lk
        .issue_credentials(&MediaCredentialRequest {
            session: session.clone(),
            identity: identity.clone(),
            generation: 1,
            ttl: chrono::Duration::minutes(5),
        })
        .await
        .unwrap();
    // The client credential points at Calling's gateway and is sealed with the edge key:
    // LiveKit itself refuses it.
    assert_eq!(credential.url, cert.cfg.gateway_url());
    let mut refused = RtcClient::spawn(&cert.cfg).await;
    let direct = refused
        .connect(&cert.cfg.url, credential.token.expose())
        .await;
    assert_eq!(
        direct["ok"], false,
        "LiveKit must not accept a client credential: {direct}"
    );
    refused.quit().await;
    // The adapter itself is exercised with the gateway's per-connection upstream token.
    let upstream = |generation| {
        cert.lk
            .upstream_token(&ConnectionClaims {
                resource_name: session.name.as_str().to_string(),
                identity: identity.clone(),
                generation: Some(generation),
            })
            .unwrap()
    };
    let mut client = RtcClient::spawn(&cert.cfg).await;
    let joined = client.connect(&cert.cfg.url, &upstream(1)).await;
    assert_eq!(joined["ok"], true, "{joined}");
    let status = client.status().await;
    assert_eq!(status["can_publish"], false);
    assert_eq!(status["can_subscribe"], false);
    let denied = client
        .cmd(json!({ "cmd": "publish", "kind": "audio" }))
        .await;
    assert_eq!(
        denied["ok"], false,
        "join-only credential must not publish: {denied}"
    );
    // Server-side permission update, then removal and termination.
    let participant = MediaParticipantRef {
        session: session.clone(),
        identity: identity.clone(),
    };
    eventually(10, "participant listed", || async {
        cert.lk
            .list_participants(&session)
            .await
            .unwrap()
            .iter()
            .any(|p| p.identity == identity)
    })
    .await;
    let voice = MediaPermissions::default_for(calling::calls::kinds::CallKind::Voice);
    cert.lk
        .update_permissions(&participant, voice)
        .await
        .unwrap();
    let ok = client
        .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
        .await;
    assert_eq!(ok["ok"], true);
    let published = client
        .cmd(json!({ "cmd": "publish", "kind": "audio" }))
        .await;
    assert_eq!(published["ok"], true, "{published}");
    let camera = client
        .cmd(json!({ "cmd": "publish", "kind": "video" }))
        .await;
    assert_eq!(
        camera["ok"], false,
        "voice permissions must not allow camera: {camera}"
    );
    let listed = cert.lk.list_participants(&session).await.unwrap();
    let p = listed.iter().find(|p| p.identity == identity).unwrap();
    assert_eq!(p.generation, Some(1));
    assert!(p.permissions.can_publish_audio && !p.permissions.can_publish_video);
    cert.lk.remove_session(&participant).await.unwrap();
    let gone = client
        .cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
        .await;
    assert_eq!(
        gone["ok"], true,
        "removed participant is disconnected: {gone}"
    );
    cert.lk.terminate_session(&session).await.unwrap();
    assert!(!cert.lk.reconcile(&session).await.unwrap().exists);
    // Terminating again is idempotent; the still-valid credential cannot resurrect the
    // terminated resource (rooms are created by Calling only).
    cert.lk.terminate_session(&session).await.unwrap();
    let mut again = RtcClient::spawn(&cert.cfg).await;
    let rejoin = again.connect(&cert.cfg.url, &upstream(1)).await;
    assert_eq!(
        rejoin["ok"], false,
        "a terminated resource is not recreated by a join: {rejoin}"
    );
    assert!(!cert.lk.reconcile(&session).await.unwrap().exists);
    client.quit().await;
    again.quit().await;
}

async fn forged_and_duplicate_callbacks(cert: &Cert) {
    let body = json!({ "event": "room_started", "id": "EV_forged", "room": { "name": format!("cfc-{}", Uuid::new_v4().simple()) } });
    let r = cert
        .a
        .h()
        .request_with(
            Method::POST,
            "/internal/provider/media/events",
            None,
            Some(body.clone()),
            &[("authorization", "eyJhbGciOiJIUzI1NiJ9.e30.forged")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    let r = cert
        .a
        .h()
        .request_with(
            Method::POST,
            "/internal/provider/media/events",
            None,
            Some(body),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

async fn voice_call(cert: &Cert) {
    let (call, _caller, _callee, s1, s2) = cert.answered("VOICE", "voice").await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    assert_eq!(c1["capabilities"]["canPublishVideo"], false);
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    // One joined party is not a media path.
    eventually(15, "caller admitted", || cert.session_admitted(call, s1)).await;
    assert_eq!(cert.state(call).await, CallState::Answered);
    assert_eq!(
        b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    eventually(15, "CONNECTED on verified evidence", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert_eq!(
        b.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    let camera = a.cmd(json!({ "cmd": "publish", "kind": "video" })).await;
    assert_eq!(
        camera["ok"], false,
        "voice call camera publish must be denied: {camera}"
    );
    let heard = b
        .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
        .await;
    assert_eq!(heard["ok"], true, "real media flows: {heard}");
    assert_eq!(
        cert.provider_identities(call).await.len(),
        2,
        "provider lists both sessions"
    );

    // HOLD is enforced at the provider: publishing stops.
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": false, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert!(cert
        .provider_identities(call)
        .await
        .iter()
        .all(|(_, _, p)| !p.can_publish_audio && p.can_subscribe));
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "resume", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert!(cert
        .provider_identities(call)
        .await
        .iter()
        .all(|(_, _, p)| p.can_publish_audio && !p.can_publish_video));

    // END: cleanup confirmed by the provider, then ENDED with usage (the harness clock is
    // manual: let connected time pass).
    cert.a.h().clock.advance(chrono::Duration::seconds(5));
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Ended);
    assert!(
        !cert.lk.reconcile(&room(call)).await.unwrap().exists,
        "room terminated"
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
            .await["ok"],
        true
    );
    let usage = cert
        .a
        .w
        .calls()
        .usage_records(cert.a.w.scope(), CallId::from_uuid(call))
        .await
        .unwrap();
    assert!(usage
        .iter()
        .any(|u| u.usage_type.as_str() == "CALL_CONNECTED_TIME" && u.quantity > 0));
    // Part K §102: the end-of-call quality summary is recorded from the REAL media
    // evidence of this call — both sessions admitted by LiveKit, both parties connected.
    let quality = cert
        .a
        .w
        .calls()
        .quality_records(cert.a.w.scope(), CallId::from_uuid(call))
        .await
        .unwrap();
    let summary = quality
        .iter()
        .find(|q| q.metrics.get("media_sessions_admitted").is_some())
        .expect("an end-of-call quality summary");
    assert_eq!(summary.metrics.get("media_sessions_admitted"), Some(2.0));
    assert_eq!(summary.metrics.get("connected_participants"), Some(2.0));
    println!(
        "rtc certification: quality summary recorded (admitted sessions 2, connected participants 2, reconnects {})",
        summary.metrics.get("session_reconnects").unwrap_or_default()
    );
    a.quit().await;
    b.quit().await;
}

/// Part F §35 / §200 / §206: Extension 1001 → Extension 1002 over ENTERPRISE_INTERNAL
/// with REAL internal media (no public leg), held and resumed at the provider, ended.
async fn enterprise_extension_call(cert: &Cert) {
    use calling::platform::scopes::names;
    let h = cert.a.h();
    common::enterprise::allow_enterprise_scopes(h, &cert.a.w.consumer).await;
    let admin = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let caller_token = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let member = |reference: &'static str| async move {
        let (view, _) = h
            .platform
            .register_principal(
                cert.a.w.consumer.env(cert.a.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
        h.state
            .principals
            .resolve(
                &cert.a.w.ctx,
                &calling::platform::principals::PrincipalReference::Id(view.id),
            )
            .await
            .unwrap()
    };
    let alice = member("ent-alice").await;
    let bob = member("ent-bob").await;
    for (number, who) in [("1001", &alice), ("1002", &bob)] {
        let r = h
            .request(
                axum::http::Method::POST,
                "/v1/enterprise/extensions",
                Some(admin.secret.expose_once()),
                Some(json!({ "extensionNumber": number, "displayLabel": number,
                              "destination": { "type": "ENTERPRISE_MEMBER", "id": who.id() } })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let alice_desk = cert.a.device(&alice, "ent-alice-desk").await;
    let bob_desk = cert.a.device(&bob, "ent-bob-desk").await;
    let r = h
        .request_with(
            axum::http::Method::POST,
            "/v1/calls",
            Some(caller_token.secret.expose_once()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(),
                         "initiatorEndpointId": alice_desk.endpoint, "targetExtension": "1002" })),
            &[("idempotency-key", "ent-cert-1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "RINGING");
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let invitation = cert
        .a
        .invitation_for(call, bob_desk.endpoint)
        .await
        .unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.accept(call, invitation, &bob_desk, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s1 = cert.a.session_on(call, alice_desk.endpoint).await.unwrap();
    let s2 = cert.a.session_on(call, bob_desk.endpoint).await.unwrap();
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    eventually(
        15,
        "enterprise call CONNECTED on verified evidence",
        || async { cert.state(call).await == CallState::Connected },
    )
    .await;
    for client in [&mut a, &mut b] {
        assert_eq!(
            client
                .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                .await["ok"],
            true
        );
    }
    let heard = b
        .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
        .await;
    assert_eq!(heard["ok"], true, "real internal media flows: {heard}");
    // Hold / resume of the enterprise call, enforced at the provider (§206).
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Held);
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": false, "timeout": 10 }))
            .await["ok"],
        true
    );
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "resume", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Connected);
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    // No public network anywhere in this Call.
    let public: i64 = common::scalar_i64(
        h.conn(),
        &format!(
            "SELECT (SELECT count(*) FROM call_telephony_commands WHERE call_id = '{call}') + \
             (SELECT count(*) FROM call_public_parties WHERE call_id = '{call}') + \
             (SELECT count(*) FROM call_routes WHERE call_id = '{call}' AND routing_class <> 'ENTERPRISE_INTERNAL')"
        ),
    )
    .await;
    assert_eq!(public, 0);
    h.clock.advance(chrono::Duration::seconds(5));
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Ended);
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
            .await["ok"],
        true
    );
    // Correct history for both logical participants (§62, derived from Call evidence).
    for (who, direction) in [(alice.id(), "OUTBOUND"), (bob.id(), "INBOUND")] {
        let r = cert
            .a
            .get(&format!("/v1/calls?principalId={who}&limit=5"))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        let entry = r.json["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["callId"] == call.to_string())
            .cloned()
            .expect("the call is in the participant's history");
        assert_eq!(entry["participation"]["direction"], direction);
        assert_eq!(entry["participation"]["outcome"], "ANSWERED");
        assert_eq!(entry["participation"]["missed"], false);
        assert_eq!(entry["enterprise"]["entryExtension"], "1002");
        assert!(entry["durationMs"].as_i64().unwrap() >= 5_000, "{entry}");
    }
    println!("enterprise extension call certified: 1001 -> 1002 over ENTERPRISE_INTERNAL with real media and history");
    a.quit().await;
    b.quit().await;
}

/// Part F §201 / §202: a SIMULTANEOUS group (two members ring, one wins with real media,
/// the other is answered-elsewhere) and an ORDERED group (member 1 does not answer, member
/// 2 rings and answers) — each ONE logical Call.
async fn enterprise_ring_groups(cert: &Cert) {
    use calling::platform::scopes::names;
    let h = cert.a.h();
    let admin = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let caller_token = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let admin_post = |path: String, body: Value| {
        let token = admin.secret.expose_once().to_string();
        async move {
            let r = h
                .request(axum::http::Method::POST, &path, Some(&token), Some(body))
                .await;
            assert!(r.status.is_success(), "{path}: {}", r.text);
            r.json
        }
    };
    let member = |reference: String| async move {
        let (view, _) = h
            .platform
            .register_principal(
                cert.a.w.consumer.env(cert.a.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference,
                },
            )
            .await
            .unwrap();
        h.state
            .principals
            .resolve(
                &cert.a.w.ctx,
                &calling::platform::principals::PrincipalReference::Id(view.id),
            )
            .await
            .unwrap()
    };
    let caller = member("rg-caller".into()).await;
    let m1 = member("rg-m1".into()).await;
    let m2 = member("rg-m2".into()).await;
    let caller_desk = cert.a.device(&caller, "rg-caller-desk").await;
    let d1 = cert.a.device(&m1, "rg-m1-desk").await;
    let d2 = cert.a.device(&m2, "rg-m2-desk").await;
    for (strategy, number) in [("SIMULTANEOUS", "4000"), ("ORDERED", "4001")] {
        let group = admin_post(
            "/v1/enterprise/ring-groups".into(),
            json!({ "name": format!("Cert {strategy}"), "strategy": strategy,
                    "memberRingSeconds": 10, "groupTimeoutSeconds": 40 }),
        )
        .await;
        let gid = group["ringGroupId"].as_str().unwrap().to_string();
        let r = h
            .request(
                axum::http::Method::PUT,
                &format!("/v1/enterprise/ring-groups/{gid}/members"),
                Some(admin.secret.expose_once()),
                Some(json!({ "expectedVersion": 1, "members": [
                    { "type": "ENTERPRISE_MEMBER", "id": m1.id() },
                    { "type": "ENTERPRISE_MEMBER", "id": m2.id() } ] })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        admin_post(
            "/v1/enterprise/extensions".into(),
            json!({ "extensionNumber": number, "displayLabel": strategy,
                    "destination": { "type": "RING_GROUP", "id": gid } }),
        )
        .await;
        let r = h
            .request_with(
                axum::http::Method::POST,
                "/v1/calls",
                Some(caller_token.secret.expose_once()),
                Some(json!({ "kind": "VOICE", "initiatorPrincipalId": caller.id(),
                             "initiatorEndpointId": caller_desk.endpoint, "targetExtension": number })),
                &[("idempotency-key", &format!("rg-cert-{number}"))],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        if strategy == "SIMULTANEOUS" {
            assert_eq!(r.json["liveInvitationCount"], 2);
        } else {
            // Member 1 rings alone and does not answer; server time moves to member 2.
            assert_eq!(r.json["liveInvitationCount"], 1);
            assert!(cert.a.invitation_for(call, d1.endpoint).await.is_some());
            h.clock.advance(chrono::Duration::seconds(11));
            h.state.app.expire_due(100).await.unwrap();
        }
        let invitation = cert.a.invitation_for(call, d2.endpoint).await.unwrap();
        let (_, version) = cert.a.state(call).await;
        let r = cert.a.accept(call, invitation, &d2, version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        if strategy == "SIMULTANEOUS" {
            let view = cert.a.call_view(call).await;
            assert!(view["invitations"].as_array().unwrap().iter().any(|i| {
                i["endpointId"] == d1.endpoint.to_string() && i["status"] == "ANSWERED_ELSEWHERE"
            }));
        }
        let s1 = cert.a.session_on(call, caller_desk.endpoint).await.unwrap();
        let s2 = cert.a.session_on(call, d2.endpoint).await.unwrap();
        let c1 = cert.credential(call, s1).await;
        let c2 = cert.credential(call, s2).await;
        let mut a = RtcClient::spawn(&cert.cfg).await;
        let mut b = RtcClient::spawn(&cert.cfg).await;
        assert_eq!(
            a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
                .await["ok"],
            true
        );
        assert_eq!(
            b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
                .await["ok"],
            true
        );
        eventually(15, "ring-group call CONNECTED", || async {
            cert.state(call).await == CallState::Connected
        })
        .await;
        for client in [&mut a, &mut b] {
            assert_eq!(
                client
                    .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                    .await["ok"],
                true
            );
            assert_eq!(
                client
                    .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                    .await["ok"],
                true
            );
        }
        let heard = a
            .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
            .await;
        assert_eq!(
            heard["ok"], true,
            "real media with the winning member: {heard}"
        );
        assert_eq!(
            cert.provider_identities(call).await.len(),
            2,
            "only the winner and the caller are in the media resource"
        );
        let (_, version) = cert.a.state(call).await;
        let r = cert.a.command(call, "end", version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(cert.state(call).await, CallState::Ended);
        println!(
            "enterprise ring group certified: {strategy} with real media, one winner, one Call"
        );
        a.quit().await;
        b.quit().await;
    }
}

/// §56 with real media: a queued caller is offered to the first agent, the offer times
/// out (server time), the next agent is offered the SAME Call, answers and talks.
async fn enterprise_queue(cert: &Cert) {
    use calling::platform::scopes::names;
    let h = cert.a.h();
    let admin = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let caller_token = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let member = |reference: &'static str| async move {
        let (view, _) = h
            .platform
            .register_principal(
                cert.a.w.consumer.env(cert.a.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
        h.state
            .principals
            .resolve(
                &cert.a.w.ctx,
                &calling::platform::principals::PrincipalReference::Id(view.id),
            )
            .await
            .unwrap()
    };
    let caller = member("q-caller").await;
    let first = member("q-agent-1").await;
    let second = member("q-agent-2").await;
    let caller_desk = cert.a.device(&caller, "q-caller-desk").await;
    let d1 = cert.a.device(&first, "q-agent-1-desk").await;
    let d2 = cert.a.device(&second, "q-agent-2-desk").await;
    let r = h
        .request(
            axum::http::Method::POST,
            "/v1/enterprise/queues",
            Some(admin.secret.expose_once()),
            Some(
                json!({ "name": "Cert queue", "maxWaitSeconds": 120, "offerTimeoutSeconds": 10,
                         "capacity": 10 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let qid = r.json["queueId"].as_str().unwrap().to_string();
    let r = h
        .request(
            axum::http::Method::PUT,
            &format!("/v1/enterprise/queues/{qid}/members"),
            Some(admin.secret.expose_once()),
            Some(json!({ "expectedVersion": 1, "members": [
                { "principalId": first.id() }, { "principalId": second.id() } ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .request(
            axum::http::Method::POST,
            "/v1/enterprise/extensions",
            Some(admin.secret.expose_once()),
            Some(
                json!({ "extensionNumber": "4100", "displayLabel": "Cert queue",
                         "destination": { "type": "QUEUE", "id": qid } }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = h
        .request_with(
            axum::http::Method::POST,
            "/v1/calls",
            Some(caller_token.secret.expose_once()),
            Some(
                json!({ "kind": "VOICE", "initiatorPrincipalId": caller.id(),
                         "initiatorEndpointId": caller_desk.endpoint, "targetExtension": "4100" }),
            ),
            &[("idempotency-key", "queue-cert-4100")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["enterprise"]["phase"], "QUEUED");
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // The first agent is offered and lets it ring out; server time moves the offer on.
    assert!(cert.a.invitation_for(call, d1.endpoint).await.is_some());
    h.clock.advance(chrono::Duration::seconds(11));
    h.state.app.expire_due(100).await.unwrap();
    let invitation = cert.a.invitation_for(call, d2.endpoint).await.unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.accept(call, invitation, &d2, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s1 = cert.a.session_on(call, caller_desk.endpoint).await.unwrap();
    let s2 = cert.a.session_on(call, d2.endpoint).await.unwrap();
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    eventually(15, "queued call CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    for client in [&mut a, &mut b] {
        assert_eq!(
            client
                .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                .await["ok"],
            true
        );
    }
    let heard = a
        .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
        .await;
    assert_eq!(
        heard["ok"], true,
        "real media with the answering agent: {heard}"
    );
    assert_eq!(
        cert.provider_identities(call).await.len(),
        2,
        "only the answering agent and the caller are in the media resource"
    );
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Ended);
    println!(
        "enterprise queue certified: FIFO offer, agent rotation on timeout, real media, one Call"
    );
    a.quit().await;
    b.quit().await;
}

/// Part F §207 / §208: an enterprise transfer with real media — B hands the Call to
/// extension C (same Call id, B removed from the media resource, A now hears C), then a
/// failed transfer (D declines) returns the SAME Call to CONNECTED with media intact.
async fn enterprise_transfer(cert: &Cert) {
    use calling::platform::scopes::names;
    let h = cert.a.h();
    let admin = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let user = common::enterprise::credential(
        h,
        &cert.a.w.consumer,
        cert.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let member = |reference: &'static str| async move {
        let (view, _) = h
            .platform
            .register_principal(
                cert.a.w.consumer.env(cert.a.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
        h.state
            .principals
            .resolve(
                &cert.a.w.ctx,
                &calling::platform::principals::PrincipalReference::Id(view.id),
            )
            .await
            .unwrap()
    };
    let alice = member("xf-alice").await;
    let bob = member("xf-bob").await;
    let carol = member("xf-carol").await;
    let dave = member("xf-dave").await;
    for (number, who) in [("1502", &bob), ("1503", &carol), ("1504", &dave)] {
        let r = h
            .request(
                axum::http::Method::POST,
                "/v1/enterprise/extensions",
                Some(admin.secret.expose_once()),
                Some(json!({ "extensionNumber": number, "displayLabel": number,
                              "destination": { "type": "ENTERPRISE_MEMBER", "id": who.id() } })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let alice_desk = cert.a.device(&alice, "xf-alice-desk").await;
    let bob_desk = cert.a.device(&bob, "xf-bob-desk").await;
    let carol_desk = cert.a.device(&carol, "xf-carol-desk").await;
    let dave_desk = cert.a.device(&dave, "xf-dave-desk").await;
    let r = h
        .request_with(
            axum::http::Method::POST,
            "/v1/calls",
            Some(user.secret.expose_once()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": alice.id(),
                         "initiatorEndpointId": alice_desk.endpoint, "targetExtension": "1502" })),
            &[("idempotency-key", "xf-cert-1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let invitation = cert
        .a
        .invitation_for(call, bob_desk.endpoint)
        .await
        .unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.accept(call, invitation, &bob_desk, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let mut clients = Vec::new();
    for desk in [&alice_desk, &bob_desk] {
        let s = cert.a.session_on(call, desk.endpoint).await.unwrap();
        let c = cert.credential(call, s).await;
        let mut client = RtcClient::spawn(&cert.cfg).await;
        assert_eq!(
            client
                .connect(c["url"].as_str().unwrap(), c["token"].as_str().unwrap())
                .await["ok"],
            true
        );
        clients.push(client);
    }
    eventually(15, "transfer scenario CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    for client in clients.iter_mut() {
        assert_eq!(
            client
                .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                .await["ok"],
            true
        );
    }
    let mut b = clients.pop().unwrap();
    let mut a = clients.pop().unwrap();
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
            .await["ok"],
        true
    );

    // B transfers its place to extension 1503 (an enterprise target).
    let transfer = |participant: Uuid, extension: &'static str, key: &'static str| {
        let token = user.secret.expose_once().to_string();
        async move {
            let (_, version) = cert.a.state(call).await;
            h.request_with(
                axum::http::Method::POST,
                &format!("/v1/calls/{call}/transfer"),
                Some(&token),
                Some(json!({ "transferringParticipantId": participant,
                             "targetExtension": extension, "expectedVersion": version })),
                &[("idempotency-key", key)],
            )
            .await
        }
    };
    let bob_participant = cert.a.participant_of(call, bob.id().as_uuid()).await;
    let r = transfer(bob_participant, "1503", "xf-cert-2").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Transferring);
    let invitation = cert
        .a
        .invitation_for(call, carol_desk.endpoint)
        .await
        .unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.accept(call, invitation, &carol_desk, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s3 = cert.a.session_on(call, carol_desk.endpoint).await.unwrap();
    let c3 = cert.credential(call, s3).await;
    let mut c = RtcClient::spawn(&cert.cfg).await;
    let joined = c
        .connect(c3["url"].as_str().unwrap(), c3["token"].as_str().unwrap())
        .await;
    assert_eq!(joined["ok"], true);
    let carol_identity = joined["identity"].as_str().unwrap().to_string();
    eventually(20, "transfer completed on real media evidence", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    // B was removed from the media resource; A now hears C — the SAME Call.
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_disconnected", "timeout": 15 }))
            .await["ok"],
        true
    );
    assert_eq!(
        c.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert_eq!(
        c.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    let heard = a
        .cmd(json!({ "cmd": "wait_subscribed", "count": 2, "timeout": 20 }))
        .await;
    assert!(
        heard["subscribed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["from"] == carol_identity.as_str()),
        "A hears C after the transfer: {heard}"
    );
    assert_eq!(
        common::scalar_i64(
            h.conn(),
            &format!("SELECT count(*) FROM calls WHERE id = '{call}'")
        )
        .await,
        1
    );
    println!("enterprise transfer certified: 1502 -> 1503 on one Call with real media");

    // A failed transfer (D declines) returns the SAME Call to CONNECTED, media intact.
    let carol_participant = cert.a.participant_of(call, carol.id().as_uuid()).await;
    let r = transfer(carol_participant, "1504", "xf-cert-3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let invitation = cert
        .a
        .invitation_for(call, dave_desk.endpoint)
        .await
        .unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.decline(call, invitation, &dave_desk, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(cert.state(call).await, CallState::Connected);
    for client in [&mut a, &mut c] {
        assert_eq!(client.status().await["connected"], true);
    }
    println!("enterprise failed transfer certified: declined target, the Call stays CONNECTED");
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for client in [a, b, c] {
        client.quit().await;
    }
}

async fn video_call_and_removal(cert: &Cert) {
    let (call, _caller, callee, s1, s2) = cert.answered("VIDEO", "video").await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    assert_eq!(c1["capabilities"]["canPublishVideo"], true);
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
        .await;
    b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
        .await;
    eventually(15, "video CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    for client in [&mut a, &mut b] {
        assert_eq!(
            client
                .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "video" }))
                .await["ok"],
            true
        );
    }
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_subscribed", "count": 2, "timeout": 15 }))
            .await["ok"],
        true
    );
    // Remove the callee's session: provider disconnect is confirmed.
    let (_, version) = cert.a.state(call).await;
    let r = cert
        .a
        .post(
            &format!("/v1/calls/{call}/sessions/{s2}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
            .await["ok"],
        true
    );
    assert_eq!(cert.provider_identities(call).await.len(), 1);
    let _ = callee;
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    a.quit().await;
    b.quit().await;
}

/// The removed session's still-valid credential must not regain media (§44, §109).
/// The (unverified) payload of a JWT, for inspecting what the SDK holds.
fn jwt_payload(token: &str) -> Value {
    use base64::Engine;
    let payload = token.split('.').nth(1).expect("a JWT");
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .expect("base64url payload"),
    )
    .expect("JSON payload")
}

async fn session_media(cert: &Cert, session: Uuid) -> (String, i32, String) {
    use sea_orm::ConnectionTrait;
    let row = cert
        .a
        .h()
        .conn()
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT media_identity, media_generation, media_access FROM call_sessions WHERE id = '{session}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    (
        row.try_get_by_index(0).unwrap(),
        row.try_get_by_index(1).unwrap(),
        row.try_get_by_index(2).unwrap(),
    )
}

async fn joined_callbacks(cert: &Cert, session: Uuid) -> i64 {
    common::scalar_i64(
        cert.a.h().conn(),
        &format!(
            "SELECT count(*) FROM call_media_provider_events WHERE session_id = '{session}' AND event_kind = 'PARTICIPANT_JOINED'"
        ),
    )
    .await
}

/// Part C security closure: a REAL server-refreshed PRIVILEGED token, captured from the
/// real client SDK, cannot bring a removed session back — neither through the gateway
/// nor around it — and the refusal happens before any media participation.
async fn refreshed_privileged_token_cannot_reenter(cert: &Cert, round: u32) {
    // 1-2. An answered app Call; join-only credentials for both sessions.
    let (call, _caller, _callee, s1, s2) =
        cert.answered("VOICE", &format!("refresh-{round}")).await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let issued = c2["token"].as_str().unwrap().to_string();
    assert_eq!(c2["url"], cert.cfg.gateway_url());
    let issued_claims = jwt_payload(&issued);
    assert_eq!(issued_claims["iss"], EDGE_ISSUER);
    assert_eq!(issued_claims["video"]["canPublish"], false);
    assert_eq!(issued_claims["video"]["canSubscribe"], false);
    // 3. Both clients join (through the gateway).
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        b.connect(c2["url"].as_str().unwrap(), &issued).await["ok"],
        true
    );
    // 4-5. Calling admits them; the provider applies audio publish + subscription.
    eventually(20, "refresh scenario CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 15 }))
            .await["ok"],
        true
    );
    // 6-7. LiveKit refreshes the token after the permission update; the real SDK holds it.
    let captured = b
        .cmd(json!({ "cmd": "wait_refreshed", "publish": true, "timeout": 30 }))
        .await;
    assert_eq!(
        captured["ok"], true,
        "no privileged refreshed token was captured: {captured}"
    );
    let refreshed = captured["token"].as_str().unwrap().to_string();
    let latest = b.cmd(json!({ "cmd": "token" })).await;
    assert!(latest["refreshes"].as_u64().unwrap() >= 1);
    // 8. It is the admitted session's privileged token (refreshed by the server, then
    // sealed by the edge on its way to the client).
    let (identity, generation, access) = session_media(cert, s2).await;
    assert_eq!(access, "AUTHORIZED");
    assert_ne!(refreshed, issued);
    let p = jwt_payload(&refreshed);
    assert_eq!(p["sub"], identity);
    assert_eq!(p["video"]["room"], room(call).name.as_str());
    assert_eq!(p["video"]["canPublish"], true, "{p}");
    assert_eq!(p["video"]["canSubscribe"], true, "{p}");
    assert_eq!(p["attributes"]["cfm.gen"], generation.to_string());
    assert_eq!(
        p["iss"], EDGE_ISSUER,
        "the client never holds a LiveKit-signed token"
    );
    assert_eq!(
        cert.lk
            .verify_edge_credential(&refreshed, chrono::Utc::now()),
        Ok(ConnectionClaims {
            resource_name: room(call).name.as_str().to_string(),
            identity: identity.clone(),
            generation: Some(generation),
        })
    );
    // Media flows: b publishes, a receives it.
    assert_eq!(
        b.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
            .await["ok"],
        true
    );
    // 9. Remove the session.
    let (_, version) = cert.a.state(call).await;
    let r = cert
        .a
        .post(
            &format!("/v1/calls/{call}/sessions/{s2}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // 10. Authoritative state: REVOKED.
    assert_eq!(session_media(cert, s2).await.2, "REVOKED");
    // 11. The provider disconnected it.
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_disconnected", "timeout": 15 }))
            .await["ok"],
        true
    );
    eventually(10, "removed identity gone from the provider", || async {
        !cert
            .provider_identities(call)
            .await
            .iter()
            .any(|(i, _, _)| *i == identity)
    })
    .await;
    let joins_before = joined_callbacks(cert, s2).await;
    let received_before = a.status().await["subscribed"].as_array().unwrap().len();
    // 12-13. A NEW client presents the SAVED REFRESHED PRIVILEGED token through the
    // gateway, and immediately tries to publish and to receive media.
    let mut stale = RtcClient::spawn(&cert.cfg).await;
    let attempt = stale.connect(&cert.cfg.gateway_url(), &refreshed).await;
    assert_eq!(
        attempt["ok"], false,
        "the saved refreshed token re-entered: {attempt}"
    );
    assert!(
        attempt["error"]
            .as_str()
            .unwrap_or_default()
            .contains("401"),
        "refused at admission (401), before the media server: {attempt}"
    );
    let publish = stale
        .cmd(json!({ "cmd": "publish", "kind": "audio" }))
        .await;
    assert_eq!(publish["ok"], false, "{publish}");
    let status = stale.status().await;
    assert_eq!(status["connected"], false, "{status}");
    assert!(status["subscribed"].as_array().unwrap().is_empty());
    // …and directly at LiveKit, around the gateway: LiveKit does not know the edge key.
    let mut around = RtcClient::spawn(&cert.cfg).await;
    let direct = around.connect(&cert.cfg.url, &refreshed).await;
    assert_eq!(
        direct["ok"], false,
        "LiveKit accepted a client token: {direct}"
    );
    assert_eq!(
        around
            .cmd(json!({ "cmd": "publish", "kind": "audio" }))
            .await["ok"],
        false
    );
    // The original join-only credential is refused the same way.
    let mut original = RtcClient::spawn(&cert.cfg).await;
    let original_attempt = original.connect(&cert.cfg.gateway_url(), &issued).await;
    assert_eq!(original_attempt["ok"], false, "{original_attempt}");
    // No media window at all: LiveKit never saw the identity again, no join callback
    // arrived for the session, and the remaining party received nothing new.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!cert
        .provider_identities(call)
        .await
        .iter()
        .any(|(i, _, _)| *i == identity));
    assert_eq!(joined_callbacks(cert, s2).await, joins_before);
    assert_eq!(
        a.status().await["subscribed"].as_array().unwrap().len(),
        received_before
    );
    eprintln!(
        "refreshed-token re-entry (round {round}): gateway={} direct={} original={}",
        attempt["error"], direct["error"], original_attempt["error"]
    );
    for client in [stale, around, original, a, b] {
        client.quit().await;
    }
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
}

/// A superseded generation is refused while the current one is admitted; nothing is
/// admitted once the Call is terminal.
async fn superseded_generation_and_terminal_call_tokens_are_refused(cert: &Cert) {
    let (call, _caller, _callee, s1, s2) = cert.answered("VOICE", "generation").await;
    let c1 = cert.credential(call, s1).await;
    let old = cert.credential(call, s2).await;
    let current = cert.credential(call, s2).await;
    assert_ne!(old["token"], current["token"]);
    let mut stale = RtcClient::spawn(&cert.cfg).await;
    let refused = stale
        .connect(old["url"].as_str().unwrap(), old["token"].as_str().unwrap())
        .await;
    assert_eq!(
        refused["ok"], false,
        "generation N after N+1 must be refused: {refused}"
    );
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        b.connect(
            current["url"].as_str().unwrap(),
            current["token"].as_str().unwrap()
        )
        .await["ok"],
        true
    );
    eventually(20, "generation scenario CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    // The Call ends: the current generation is refused afterwards.
    let (_, version) = cert.a.state(call).await;
    assert_eq!(
        cert.a.command(call, "end", version).await.status,
        StatusCode::OK
    );
    eventually(20, "ENDED", || async {
        cert.state(call).await == CallState::Ended
    })
    .await;
    let mut late = RtcClient::spawn(&cert.cfg).await;
    let after = late
        .connect(
            current["url"].as_str().unwrap(),
            current["token"].as_str().unwrap(),
        )
        .await;
    assert_eq!(
        after["ok"], false,
        "a terminal Call admitted a connection: {after}"
    );
    for client in [stale, a, b, late] {
        client.quit().await;
    }
}

/// A legitimate session that loses its connection comes back with its current
/// (server-refreshed, edge-sealed) token and regains its media.
async fn legitimate_reconnect_with_the_current_generation(cert: &Cert) {
    let (call, _caller, _callee, s1, s2) = cert.answered("VOICE", "reconnect").await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    eventually(20, "reconnect scenario CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    let captured = b
        .cmd(json!({ "cmd": "wait_refreshed", "publish": true, "timeout": 30 }))
        .await;
    assert_eq!(captured["ok"], true, "{captured}");
    let token = captured["token"].as_str().unwrap().to_string();
    // The network drops: the client is gone at the provider.
    b.cmd(json!({ "cmd": "disconnect" })).await;
    b.quit().await;
    let (identity, _, _) = session_media(cert, s2).await;
    eventually(15, "connection gone at the provider", || async {
        !cert
            .provider_identities(call)
            .await
            .iter()
            .any(|(i, _, _)| *i == identity)
    })
    .await;
    // Reconnect with the token the SDK holds: admitted, media restored.
    let mut back = RtcClient::spawn(&cert.cfg).await;
    let rejoined = back.connect(&cert.cfg.gateway_url(), &token).await;
    assert_eq!(
        rejoined["ok"], true,
        "legitimate reconnect refused: {rejoined}"
    );
    assert_eq!(
        back.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 15 }))
            .await["ok"],
        true,
        "re-admitted server-side"
    );
    assert_eq!(
        back.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
            .await["ok"],
        true
    );
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    a.quit().await;
    back.quit().await;
}

async fn group_call(cert: &Cert) {
    let third = cert.a.principal("cert-third").await;
    let caller = cert.a.device(&cert.a.w.initiator, "group-caller").await;
    let d2 = cert.a.device(&cert.a.w.recipient, "group-2").await;
    let d3 = cert.a.device(&third, "group-3").await;
    let r = cert
        .a
        .create_call(
            "VIDEO",
            &cert.a.w.initiator,
            &caller,
            &[&cert.a.w.recipient, &third],
            "group-create",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    for device in [&d2, &d3] {
        let invitation = cert.a.invitation_for(call, device.endpoint).await.unwrap();
        let (_, version) = cert.a.state(call).await;
        let r = cert.a.accept(call, invitation, device, version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    let mut clients = Vec::new();
    let mut sessions = Vec::new();
    for device in [&caller, &d2, &d3] {
        let session = cert.a.session_on(call, device.endpoint).await.unwrap();
        let c = cert.credential(call, session).await;
        let mut client = RtcClient::spawn(&cert.cfg).await;
        assert_eq!(
            client
                .connect(c["url"].as_str().unwrap(), c["token"].as_str().unwrap())
                .await["ok"],
            true
        );
        clients.push(client);
        sessions.push(session);
    }
    eventually(15, "group CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    eventually(15, "three admitted", || async {
        cert.provider_identities(call)
            .await
            .iter()
            .filter(|(_, _, p)| p.can_subscribe)
            .count()
            == 3
    })
    .await;
    // Same Call id, same resource, individual identities.
    assert_eq!(cert.provider_identities(call).await.len(), 3);
    // Individual removal: the others stay connected.
    let (_, version) = cert.a.state(call).await;
    let r = cert
        .a
        .post(
            &format!("/v1/calls/{call}/sessions/{}/remove", sessions[2]),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        clients[2]
            .cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
            .await["ok"],
        true
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(clients[0].status().await["connected"], true);
    assert_eq!(clients[1].status().await["connected"], true);
    assert_eq!(cert.state(call).await, CallState::Connected);
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    for client in clients {
        client.quit().await;
    }
}

async fn reconciliation(cert: &Cert) {
    let (call, _caller, _callee, s1, s2) = cert.answered("VOICE", "recon").await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
        .await;
    b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
        .await;
    eventually(15, "recon CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    // Drift 1: a permission mismatch at the provider is repaired.
    let identities = cert.provider_identities(call).await;
    let (identity, _, _) = identities.first().unwrap().clone();
    cert.lk
        .update_permissions(
            &MediaParticipantRef {
                session: room(call),
                identity: identity.clone(),
            },
            MediaPermissions::default_for(calling::calls::kinds::CallKind::Video),
        )
        .await
        .unwrap();
    let repairs = cert.a.h().state.app.reconcile_call(call).await.unwrap();
    assert!(repairs >= 1);
    let fixed = cert.provider_identities(call).await;
    assert!(
        fixed.iter().all(|(_, _, p)| !p.can_publish_video),
        "voice permissions restored"
    );
    // Drift 2: a session ended in Calling while its connection stays at the provider.
    cert.a
        .w
        .calls()
        .end_session(
            &cert.a.w.cmd,
            CallId::from_uuid(call),
            calling::calls::ids::CallSessionId::from_uuid(s2),
        )
        .await
        .unwrap();
    assert_eq!(cert.provider_identities(call).await.len(), 2);
    cert.a.h().state.app.reconcile_call(call).await.unwrap();
    assert_eq!(
        b.cmd(json!({ "cmd": "wait_disconnected", "timeout": 10 }))
            .await["ok"],
        true
    );
    // Drift 3: a resource left behind for a terminal Call is terminated.
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    assert_eq!(cert.state(call).await, CallState::Ended);
    cert.lk
        .create_or_ensure_session(
            &room(call),
            &MediaSessionSpec {
                max_participants: 2,
                empty_timeout: chrono::Duration::minutes(1),
            },
        )
        .await
        .unwrap();
    cert.a.h().state.app.reconcile_call(call).await.unwrap();
    assert!(
        !cert.lk.reconcile(&room(call)).await.unwrap().exists,
        "terminal Call's resource terminated"
    );
    a.quit().await;
    b.quit().await;
}

/// A connection with an identity Calling never issued is quarantined and evicted (§45).
async fn unknown_participant_quarantine(cert: &Cert) {
    let (call, _caller, _callee, _s1, _s2) = cert.answered("VOICE", "unknown").await;
    let rogue = cert
        .lk
        .issue_credentials(&MediaCredentialRequest {
            session: room(call),
            identity: format!("cfmid_{}", "f".repeat(64)),
            generation: 1,
            ttl: chrono::Duration::minutes(5),
        })
        .await
        .unwrap();
    // Through the gateway: refused before LiveKit is contacted.
    let mut gated = RtcClient::spawn(&cert.cfg).await;
    let refused = gated.connect(&rogue.url, rogue.token.expose()).await;
    assert_eq!(
        refused["ok"], false,
        "unknown identity admitted by the gateway: {refused}"
    );
    gated.quit().await;
    // Defense in depth: a LiveKit-signed token for an unknown identity (as if one leaked
    // around the gateway) is quarantined and evicted by Calling.
    let leaked = cert
        .lk
        .upstream_token(&ConnectionClaims {
            resource_name: room(call).name.as_str().to_string(),
            identity: format!("cfmid_{}", "f".repeat(64)),
            generation: Some(1),
        })
        .unwrap();
    let mut client = RtcClient::spawn(&cert.cfg).await;
    let joined = client.connect(&cert.cfg.url, &leaked).await;
    if joined["ok"] == true {
        let evicted = client
            .cmd(json!({ "cmd": "wait_disconnected", "timeout": 15 }))
            .await;
        assert_eq!(
            evicted["ok"], true,
            "unknown identity must be evicted: {evicted}"
        );
    }
    // It never became a participant or a session.
    let view = cert.a.call_view(call).await;
    assert_eq!(view["participants"].as_array().unwrap().len(), 2);
    let quarantined = common::scalar_i64(
        cert.a.h().conn(),
        &format!(
            "SELECT count(*) FROM call_media_provider_events WHERE call_id = '{call}' AND outcome = 'QUARANTINED'"
        ),
    )
    .await;
    assert!(quarantined >= 1);
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "cancel", version).await;
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    client.quit().await;
}

// ---------------------------------------------------------------------------
// Part F §60 / §61: application (RTC) capture through REAL LiveKit Egress
// ---------------------------------------------------------------------------

async fn enterprise_member(
    cert: &Cert,
    reference: &str,
) -> calling::platform::principals::ResolvedPrincipal {
    let h = cert.a.h();
    let (view, _) = h
        .platform
        .register_principal(
            cert.a.w.consumer.env(cert.a.w.kind).id,
            crate::common::NewPrincipal::External {
                kind: calling::domain::PrincipalKind::EnterpriseMember,
                external_reference: reference.to_string(),
            },
        )
        .await
        .unwrap();
    h.state
        .principals
        .resolve(
            &cert.a.w.ctx,
            &calling::platform::principals::PrincipalReference::Id(view.id),
        )
        .await
        .unwrap()
}

async fn scoped(cert: &Cert, scopes: &[&'static str]) -> crate::common::IssuedCredential {
    common::enterprise::credential(cert.a.h(), &cert.a.w.consumer, cert.a.w.kind, scopes).await
}

async fn send(
    cert: &Cert,
    token: &crate::common::IssuedCredential,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> common::TestResponse {
    cert.a
        .h()
        .request(method, path, Some(token.secret.expose_once()), body)
        .await
}

async fn tick(cert: &Cert) {
    cert.a.h().state.app.expire_due(100).await.unwrap();
}

async fn one(cert: &Cert, sql: String) -> String {
    common::enterprise::strings(cert.a.h().conn(), &sql)
        .await
        .pop()
        .unwrap_or_default()
}

/// The captured artifact is real audio: an Ogg / Opus file with ONLY audio streams and a
/// signal (the clients publish a 440 Hz tone), verified by ffprobe / ffmpeg.
fn verify_captured_audio(bytes: &[u8], what: &str) {
    assert!(bytes.starts_with(b"OggS"), "{what}: an Ogg container");
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rtc-captures");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{what}-{}.ogg", Uuid::new_v4().simple()));
    std::fs::write(&file, bytes).unwrap();
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name",
        ])
        .args(["-show_entries", "format=duration", "-of", "default=nw=1"])
        .arg(&file)
        .output()
        .expect("ffprobe on the PATH");
    let out = String::from_utf8_lossy(&probe.stdout).to_string();
    assert!(out.contains("codec_name=opus"), "{what}: {out}");
    assert!(out.contains("codec_type=audio"), "{what}: {out}");
    assert!(
        !out.contains("codec_type=video"),
        "{what}: audio only: {out}"
    );
    let duration: f64 = out
        .lines()
        .find_map(|l| l.strip_prefix("duration="))
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    assert!(duration >= 2.0, "{what}: {duration}s captured");
    let vol = std::process::Command::new("ffmpeg")
        .args(["-nostats", "-i"])
        .arg(&file)
        .args(["-af", "volumedetect", "-f", "null", "-"])
        .output()
        .expect("ffmpeg on the PATH");
    let err = String::from_utf8_lossy(&vol.stderr);
    let max: f64 = err
        .lines()
        .find_map(|l| l.split("max_volume:").nth(1))
        .and_then(|v| v.trim().trim_end_matches("dB").trim().parse().ok())
        .expect("volumedetect output");
    assert!(
        max > -40.0,
        "{what}: captured audio is not silence ({max} dB)"
    );
    println!("{what}: opus/ogg, audio only, {duration:.1}s, max {max} dB");
}

/// §60 RTC voicemail: an enterprise application caller dials an extension, nobody
/// answers, the Call routes to the member's mailbox, LiveKit Egress captures ONLY the
/// caller's real WebRTC microphone track, the protected artifact is finalized through
/// Calling's storage path, the message is READY, linked to the Call and the caller's /
/// owner's history; retrieval is authorized, mark-read and deletion work.
async fn rtc_voicemail_through_egress(cert: &Cert) {
    use calling::{enterprise::policy::AdminCapability, platform::scopes::names};
    let h = cert.a.h();
    let store = cert.store.clone().unwrap();
    h.state
        .enterprise
        .operator_set_capabilities(
            cert.a.w.scope(),
            vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageVoicemail,
                AdminCapability::ManageRecording,
            ],
        )
        .await
        .unwrap();
    let admin = scoped(
        cert,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let vm_admin = scoped(
        cert,
        &[
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    let vm_reader = scoped(cert, &[names::ENTERPRISE_VOICEMAIL_READ]).await;
    let caller_token = scoped(
        cert,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let carol = enterprise_member(cert, "ent-vm-carol").await;
    let dave = enterprise_member(cert, "ent-vm-dave").await;
    let r = send(
        cert,
        &vm_admin,
        Method::POST,
        "/v1/enterprise/voicemail/mailboxes",
        Some(json!({ "owner": { "type": "PRINCIPAL", "id": dave.id() },
                     "displayLabel": "Dave", "maxMessageSeconds": 60, "retentionDays": 30 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mailbox = r.json["mailboxId"].as_str().unwrap().to_string();
    let r = send(
        cert,
        &admin,
        Method::POST,
        "/v1/enterprise/forwarding-rules",
        Some(json!({ "principalId": dave.id(), "condition": "NO_ANSWER",
                     "destination": { "type": "VOICEMAIL", "id": mailbox } })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = send(
        cert,
        &admin,
        Method::POST,
        "/v1/enterprise/extensions",
        Some(json!({ "extensionNumber": "1301", "displayLabel": "Dave",
                     "destination": { "type": "ENTERPRISE_MEMBER", "id": dave.id() } })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let carol_desk = cert.a.device(&carol, "ent-vm-carol-desk").await;
    let dave_desk = cert.a.device(&dave, "ent-vm-dave-desk").await;
    let r = h
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(caller_token.secret.expose_once()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": carol.id(),
                         "initiatorEndpointId": carol_desk.endpoint, "targetExtension": "1301" })),
            &[("idempotency-key", "ent-vm-cert-1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(cert
        .a
        .invitation_for(call, dave_desk.endpoint)
        .await
        .is_some());
    // The caller is in the Call's media with a real microphone (a 440 Hz tone).
    let s1 = cert.a.session_on(call, carol_desk.endpoint).await.unwrap();
    let c1 = cert.credential(call, s1).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    assert_eq!(
        a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
            .await["ok"],
        true
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
            .await["ok"],
        true
    );
    assert_eq!(
        a.cmd(json!({ "cmd": "publish", "kind": "audio" })).await["ok"],
        true
    );
    // Nobody answers: the offer expires and the router reaches the mailbox.
    h.clock.advance(chrono::Duration::seconds(46));
    tick(cert).await;
    let message: Uuid = one(
        cert,
        format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"),
    )
    .await
    .parse()
    .expect("the Call reached voicemail");
    assert_eq!(
        one(cert, format!("SELECT state || ':' || capture_source FROM voicemail_messages WHERE id = '{message}'")).await,
        "RECORDING:APPLICATION"
    );
    assert_eq!(cert.state(call).await, CallState::Ringing);
    // The caller leaves a message (real audio through the real Egress), then hangs up.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "cancel", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    eventually(90, "voicemail READY through Egress", || async {
        tick(cert).await;
        one(
            cert,
            format!("SELECT state FROM voicemail_messages WHERE id = '{message}'"),
        )
        .await
            == "READY"
    })
    .await;
    let row = one(
        cert,
        format!(
            "SELECT content_type || '|' || size_bytes || '|' || duration_ms || '|' || sha256 || '|' \
                    || object_key FROM voicemail_messages WHERE id = '{message}'"
        ),
    )
    .await;
    let parts: Vec<&str> = row.split('|').collect();
    assert_eq!(parts[0], "audio/ogg", "{row}");
    assert!(parts[2].parse::<i64>().unwrap() >= 3_000, "{row}");
    let (sha, key) = (parts[3].to_string(), parts[4].to_string());
    // The canonical protected object exists; the Egress staging object was released.
    use cf_object_storage::ObjectStore;
    assert!(store.head(&key).await.unwrap().is_some());
    let staging = format!("capture-staging/{}.ogg", message.simple());
    assert!(
        store.head(&staging).await.unwrap().is_none(),
        "staging released"
    );
    // History: the caller sees the voicemail outcome; only the mailbox owner the link.
    let r = cert
        .a
        .get(&format!("/v1/calls?principalId={}&limit=5", carol.id()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let entry = r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .cloned()
        .expect("caller history");
    assert_eq!(entry["enterprise"]["outcome"], "VOICEMAIL", "{entry}");
    assert!(entry["voicemail"].is_null(), "{entry}");
    let r = cert
        .a
        .get(&format!("/v1/calls?principalId={}&limit=5", dave.id()))
        .await;
    let entry = r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .cloned()
        .expect("owner history");
    assert_eq!(
        entry["voicemail"]["messageId"],
        message.to_string(),
        "{entry}"
    );
    assert_eq!(entry["participation"]["missed"], true, "{entry}");
    // Authorized retrieval by the owner: the audio is the caller's real tone.
    let path = format!("/v1/enterprise/voicemail/messages/{message}");
    let r = send(
        cert,
        &vm_reader,
        Method::POST,
        &format!("{path}/media-access"),
        Some(json!({ "principalId": dave.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let url = r.json["url"].as_str().unwrap();
    // A short-lived signed read of the canonical protected object, never the staging one.
    assert!(
        !url.contains("capture-staging") && url.contains(".ogg"),
        "{url}"
    );
    let bytes = reqwest::get(url)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    assert_eq!(cf_object_storage::sha256_hex(&bytes), sha);
    verify_captured_audio(&bytes, "rtc-voicemail");
    // Not the owner / another tenant: 404.
    let r = send(
        cert,
        &vm_reader,
        Method::POST,
        &format!("{path}/media-access"),
        Some(json!({ "principalId": carol.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let other = h
        .consumer(
            "egress-cert-other",
            calling::domain::TenantClassification::External,
        )
        .await;
    common::enterprise::allow_enterprise_scopes(h, &other).await;
    let other = common::enterprise::credential(
        h,
        &other,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    for (method, p, body) in [
        (Method::GET, path.clone(), None),
        (
            Method::POST,
            format!("{path}/media-access"),
            Some(json!({})),
        ),
    ] {
        let r = send(cert, &other, method, &p, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    // Mark READ, then delete: the protected object is removed.
    let version = send(cert, &vm_admin, Method::GET, &path, None).await.json["version"]
        .as_i64()
        .unwrap();
    let r = send(
        cert,
        &vm_admin,
        Method::PUT,
        &format!("{path}/read-state"),
        Some(json!({ "expectedVersion": version, "readState": "READ", "principalId": dave.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["readState"], "READ");
    let version = r.json["version"].as_i64().unwrap();
    let r = send(
        cert,
        &vm_admin,
        Method::DELETE,
        &format!("{path}?expectedVersion={version}&principalId={}", dave.id()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    tick(cert).await;
    assert_eq!(
        one(
            cert,
            format!("SELECT state FROM voicemail_messages WHERE id = '{message}'")
        )
        .await,
        "DELETED"
    );
    assert!(
        store.head(&key).await.unwrap().is_none(),
        "artifact deleted"
    );
    println!("RTC voicemail certified: real WebRTC caller audio -> LiveKit Egress (caller track only) -> protected store -> READY -> history -> authorized retrieval -> READ -> deleted");
    a.quit().await;
}

/// §61 RTC call recording: a CONNECTED enterprise Call with two real WebRTC participants;
/// recording is denied without consent, starts only once notice / consent evidence
/// exists, LiveKit Egress captures the mixed call audio (audio only), the artifact is
/// READY, retrievable by a participant (audited), 404 across tenants; a recording active
/// at Call end stops and finalizes exactly once; retention deletion removes the object.
async fn rtc_recording_through_egress(cert: &Cert) {
    use calling::platform::scopes::names;
    let h = cert.a.h();
    let store = cert.store.clone().unwrap();
    let admin = scoped(
        cert,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let rec_admin = scoped(
        cert,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    let rec_reader = scoped(cert, &[names::ENTERPRISE_RECORDING_READ]).await;
    let caller_token = scoped(
        cert,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await;
    let r = send(cert, &rec_admin, Method::POST, "/v1/enterprise/recording-policies",
        Some(json!({ "name": "RTC certification", "mode": "MANUAL_ALLOWED", "noticeRequired": true,
                     "consentParticipants": "ALL_PARTICIPANTS", "jurisdictionRef": "NG-NDPA-2023/v2",
                     "retentionDays": 30, "maxRecordingSeconds": 600, "isDefault": true }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let erin = enterprise_member(cert, "ent-rec-erin").await;
    let frank = enterprise_member(cert, "ent-rec-frank").await;
    let r = send(
        cert,
        &admin,
        Method::POST,
        "/v1/enterprise/extensions",
        Some(json!({ "extensionNumber": "1311", "displayLabel": "Frank",
                     "destination": { "type": "ENTERPRISE_MEMBER", "id": frank.id() } })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let erin_desk = cert.a.device(&erin, "ent-rec-erin-desk").await;
    let frank_desk = cert.a.device(&frank, "ent-rec-frank-desk").await;
    let r = h
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(caller_token.secret.expose_once()),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": erin.id(),
                         "initiatorEndpointId": erin_desk.endpoint, "targetExtension": "1311" })),
            &[("idempotency-key", "ent-rec-cert-1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let invitation = cert
        .a
        .invitation_for(call, frank_desk.endpoint)
        .await
        .unwrap();
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.accept(call, invitation, &frank_desk, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let s1 = cert.a.session_on(call, erin_desk.endpoint).await.unwrap();
    let s2 = cert.a.session_on(call, frank_desk.endpoint).await.unwrap();
    let (c1, c2) = (
        cert.credential(call, s1).await,
        cert.credential(call, s2).await,
    );
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    for (client, c) in [(&mut a, &c1), (&mut b, &c2)] {
        assert_eq!(
            client
                .connect(c["url"].as_str().unwrap(), c["token"].as_str().unwrap())
                .await["ok"],
            true
        );
    }
    eventually(15, "recording call CONNECTED", || async {
        cert.state(call).await == CallState::Connected
    })
    .await;
    for client in [&mut a, &mut b] {
        assert_eq!(
            client
                .cmd(json!({ "cmd": "wait_permission", "publish": true, "timeout": 10 }))
                .await["ok"],
            true
        );
        assert_eq!(
            client
                .cmd(json!({ "cmd": "publish", "kind": "audio" }))
                .await["ok"],
            true
        );
    }
    // The Call works without any recording.
    for client in [&mut a, &mut b] {
        let heard = client
            .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 15 }))
            .await;
        assert_eq!(heard["ok"], true, "{heard}");
    }
    let start = || async {
        send(
            cert,
            &rec_admin,
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({ "principalId": erin.id() })),
        )
        .await
    };
    // Without consent: DENIED, nothing captured.
    let r = start().await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(
        one(
            cert,
            format!("SELECT count(*)::text FROM call_recordings WHERE call_id = '{call}'")
        )
        .await,
        "0"
    );
    assert_eq!(
        one(
            cert,
            format!(
                "SELECT decision || ':' || reason FROM call_recording_authorizations \
                            WHERE call_id = '{call}' ORDER BY seq DESC LIMIT 1"
            )
        )
        .await,
        "DENIED:CONSENT_MISSING"
    );
    // Each participant consents through their own endpoint; recording starts.
    for desk in [&erin_desk, &frank_desk] {
        let r = send(
            cert,
            &caller_token,
            Method::POST,
            &format!("/v1/calls/{call}/recording-consent"),
            Some(json!({ "endpointId": desk.endpoint, "decision": "GRANTED" })),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    let r = start().await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording: Uuid = r.json["recording"]["recordingId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(one(cert, format!("SELECT state || ':' || capture_source FROM call_recordings WHERE id = '{recording}'")).await,
               "RECORDING:APPLICATION");
    let notice = one(
        cert,
        format!(
            "SELECT notice_evidence::text FROM call_recording_authorizations \
                                    WHERE call_id = '{call}' AND decision = 'ALLOWED'"
        ),
    )
    .await;
    assert!(notice.contains("true"), "notice evidence: {notice}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let r = send(
        cert,
        &rec_admin,
        Method::POST,
        &format!("/v1/calls/{call}/recordings/{recording}/stop"),
        Some(json!({ "principalId": erin.id() })),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text);
    eventually(90, "recording READY through Egress", || async {
        tick(cert).await;
        one(
            cert,
            format!("SELECT state FROM call_recordings WHERE id = '{recording}'"),
        )
        .await
            == "READY"
    })
    .await;
    let row = one(
        cert,
        format!(
            "SELECT content_type || '|' || sha256 || '|' || object_key \
                                 FROM call_recordings WHERE id = '{recording}'"
        ),
    )
    .await;
    let parts: Vec<&str> = row.split('|').collect();
    assert_eq!(parts[0], "audio/ogg", "{row}");
    let (sha, key) = (parts[1].to_string(), parts[2].to_string());
    use cf_object_storage::ObjectStore;
    assert!(store
        .head(&format!("capture-staging/{}.ogg", recording.simple()))
        .await
        .unwrap()
        .is_none());
    // A participant retrieves it (audited); the mixed audio is real.
    let path = format!("/v1/enterprise/recordings/{recording}");
    let r = send(
        cert,
        &rec_reader,
        Method::POST,
        &format!("{path}/media-access"),
        Some(json!({ "principalId": frank.id() })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let bytes = reqwest::get(r.json["url"].as_str().unwrap())
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    assert_eq!(cf_object_storage::sha256_hex(&bytes), sha);
    verify_captured_audio(&bytes, "rtc-recording");
    let other = h
        .consumer(
            "egress-cert-rec-other",
            calling::domain::TenantClassification::External,
        )
        .await;
    common::enterprise::allow_enterprise_scopes(h, &other).await;
    let other = common::enterprise::credential(
        h,
        &other,
        cert.a.w.kind,
        &[
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    for (method, p, body) in [
        (Method::GET, path.clone(), None),
        (
            Method::POST,
            format!("{path}/media-access"),
            Some(json!({})),
        ),
    ] {
        let r = send(cert, &other, method, &p, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}: {}", r.text);
    }
    assert_eq!(
        one(
            cert,
            format!(
                "SELECT string_agg(operation || ':' || outcome, ',' ORDER BY seq) \
                           FROM call_recording_access_audit WHERE recording_id = '{recording}' \
                            AND operation = 'MEDIA_ACCESS'"
            )
        )
        .await,
        "MEDIA_ACCESS:GRANTED"
    );
    // A second recording still running when the Call ends: stopped and finalized once.
    let r = start().await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let second: Uuid = r.json["recording"]["recordingId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(second, recording);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (_, version) = cert.a.state(call).await;
    let r = cert.a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    eventually(90, "recording active at Call end finalized", || async {
        tick(cert).await;
        one(
            cert,
            format!("SELECT state FROM call_recordings WHERE id = '{second}'"),
        )
        .await
            == "READY"
    })
    .await;
    tick(cert).await;
    assert_eq!(
        one(
            cert,
            format!("SELECT stop_reason FROM call_recordings WHERE id = '{second}'")
        )
        .await,
        "CALL_ENDED"
    );
    for kind in ["RECORDING_STOPPED", "RECORDING_READY"] {
        assert_eq!(
            one(
                cert,
                format!(
                    "SELECT count(*)::text FROM call_events WHERE call_id = '{call}' \
                               AND event_type = '{kind}' AND payload::text LIKE '%{second}%'"
                )
            )
            .await,
            "1",
            "{kind} once"
        );
    }
    // Retention: once the retention period passes the object is deleted.
    h.clock.advance(chrono::Duration::days(31));
    tick(cert).await;
    tick(cert).await;
    assert_eq!(
        one(
            cert,
            format!("SELECT state FROM call_recordings WHERE id = '{recording}'")
        )
        .await,
        "DELETED"
    );
    assert!(
        store.head(&key).await.unwrap().is_none(),
        "retention removed the artifact"
    );
    println!("RTC recording certified: consent-gated, LiveKit Egress mixed audio (audio only) -> protected store -> READY -> audited participant retrieval -> stop-at-call-end once -> retention");
    a.quit().await;
    b.quit().await;
}
