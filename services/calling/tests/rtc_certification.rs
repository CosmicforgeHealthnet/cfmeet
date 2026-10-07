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
//! * `CALLING_RTC_CERT_REQUIRED=1` turns a missing configuration into a failure (CI).
//!
//! Without configuration the suite prints SKIPPED (never a silent pass).

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use calling::{
    calls::{ids::CallId, CallState},
    media::{
        livekit::{LiveKitAdapter, LiveKitSettings},
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
    test_runtime, Harness, TestDb, TEST_PEPPER,
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

fn adapter(cfg: &CertConfig) -> LiveKitAdapter {
    let pairs = [
        ("CALLING_LIVEKIT_URL", cfg.url.clone()),
        ("CALLING_LIVEKIT_API_URL", cfg.api_url.clone()),
        ("CALLING_LIVEKIT_API_KEY", cfg.key.clone()),
        ("CALLING_LIVEKIT_API_SECRET", cfg.secret.clone()),
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
            .stderr(std::process::Stdio::null())
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
}

impl Cert {
    async fn new(cfg: CertConfig) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        let lk = Arc::new(adapter(&cfg));
        let bus = Arc::new(InMemorySignalBus::new());
        let h = Harness::with_runtime(
            db,
            TEST_PEPPER,
            calling::domain::ManualClock::new(chrono::Utc::now()),
            Arc::new(FakeMediaProvider::default()),
            bus.clone(),
            test_runtime(Some(lk.clone()), bus),
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
    video_call_and_removal(&cert).await;
    stale_reentry(&cert).await;
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
    let mut client = RtcClient::spawn(&cert.cfg).await;
    let joined = client
        .connect(&cert.cfg.url, credential.token.expose())
        .await;
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
    let rejoin = again
        .connect(&cert.cfg.url, credential.token.expose())
        .await;
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
    a.quit().await;
    b.quit().await;
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
async fn stale_reentry(cert: &Cert) {
    let (call, _caller, _callee, s1, s2) = cert.answered("VOICE", "stale").await;
    let c1 = cert.credential(call, s1).await;
    let c2 = cert.credential(call, s2).await;
    let mut a = RtcClient::spawn(&cert.cfg).await;
    let mut b = RtcClient::spawn(&cert.cfg).await;
    a.connect(c1["url"].as_str().unwrap(), c1["token"].as_str().unwrap())
        .await;
    b.connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
        .await;
    eventually(15, "stale scenario CONNECTED", || async {
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
    // 1-5. accepted, credential issued, joined, removed, provider disconnect confirmed.
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
    // A new credential for the removed session is refused.
    let r = cert.a.media_credential(call, s2).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // 6. Re-entry with the STILL VALID original credential.
    let mut stale = RtcClient::spawn(&cert.cfg).await;
    let rejoin = stale
        .connect(c2["url"].as_str().unwrap(), c2["token"].as_str().unwrap())
        .await;
    eprintln!("stale re-entry: provider join result {rejoin}");
    if rejoin["ok"] == true {
        // The provider let the connection in (the token is cryptographically valid): it
        // holds no media right, is never admitted, and is evicted.
        let status = stale.status().await;
        assert_eq!(
            status["can_publish"], false,
            "stale connection must not publish: {status}"
        );
        assert_eq!(
            status["can_subscribe"], false,
            "stale connection must not subscribe: {status}"
        );
        let publish = stale
            .cmd(json!({ "cmd": "publish", "kind": "audio" }))
            .await;
        assert_eq!(
            publish["ok"], false,
            "stale connection published: {publish}"
        );
        let evicted = stale
            .cmd(json!({ "cmd": "wait_disconnected", "timeout": 15 }))
            .await;
        eprintln!("stale re-entry: eviction {evicted}");
        assert_eq!(
            evicted["ok"], true,
            "stale connection must be evicted: {evicted}"
        );
        let status = stale.status().await;
        assert!(
            status["subscribed"].as_array().unwrap().is_empty(),
            "stale connection received media: {status}"
        );
    }
    eventually(10, "stale identity absent", || async {
        cert.provider_identities(call).await.len() == 1
    })
    .await;
    // The database never re-admitted the removed session.
    let view = cert.a.call_view(call).await;
    let removed = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["sessionId"] == s2.to_string())
        .unwrap();
    assert_eq!(removed["mediaAccess"], "REVOKED");
    assert_eq!(removed["state"], "ENDED");
    let (_, version) = cert.a.state(call).await;
    cert.a.command(call, "end", version).await;
    a.quit().await;
    b.quit().await;
    stale.quit().await;
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
    let mut client = RtcClient::spawn(&cert.cfg).await;
    let joined = client.connect(&cert.cfg.url, rogue.token.expose()).await;
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
