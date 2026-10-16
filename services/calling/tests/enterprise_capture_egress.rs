//! Part F §60 / §61 application (RTC) capture through the REAL LiveKit Egress adapter
//! (`media::livekit_egress`) against a scripted LiveKit server API with failure injection: the
//! targeted failure behaviour (unavailable Egress, start / stop timeouts, protected
//! storage outage, duplicate start / stop, Call end racing a stop, voicemail maximum
//! length). The real-media path (real LiveKit, real Egress, real WebRTC audio, an
//! S3-compatible store) is certified in `rtc_certification`.

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    extract::{Path, State},
    http::{Method, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use calling::{
    calls::{
        app::enterprise::{CallMediaCapture, CapturePurpose, CaptureRequest},
        CallState,
    },
    enterprise::{policy::AdminCapability, EnterpriseFlags},
    media::{
        livekit::{LiveKitAdapter, LiveKitSettings},
        livekit_egress::{CaptureSettings, LiveKitEgressCapture},
    },
    platform::scopes::names,
};
use cf_object_storage::MemoryObjectStore;
use common::{
    app::Device,
    enterprise::{credential, id_of, strings, EnterpriseWorld},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

fn tag() -> String {
    // Unique names without digit runs: enterprise names refuse anything that looks like a
    // telephone number (7+ consecutive digits), which 8 random hex characters sometimes are.
    Uuid::new_v4().simple().to_string()[..8]
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                (b'g' + (c as u8 - b'0')) as char
            } else {
                c
            }
        })
        .collect()
}

/// A scripted LiveKit server API: Egress + RoomService.ListParticipants.
#[derive(Default)]
struct Script {
    unavailable: bool,
    start_delay_ms: u64,
    stop_unavailable: bool,
    /// A stopped egress ends FAILED without media.
    fail_on_stop: bool,
    egresses: Vec<Value>,
    /// room -> [(identity, microphone track sid)]
    participants: HashMap<String, Vec<(String, String)>>,
    log: Vec<String>,
}

#[derive(Clone)]
struct Stub {
    script: Arc<Mutex<Script>>,
    /// The staging side of the protected bucket the egress writes to.
    store: Arc<MemoryObjectStore>,
}

fn ogg(len: usize) -> Vec<u8> {
    let mut b = b"OggS".to_vec();
    b.extend((0..len).map(|i| (i % 251) as u8));
    b
}

async fn twirp(
    State(stub): State<Stub>,
    Path((service, method)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let delay = {
        let mut s = stub.script.lock().unwrap();
        s.log.push(method.clone());
        if s.unavailable {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if method.starts_with("Start") {
            s.start_delay_ms
        } else {
            0
        }
    };
    if delay > 0 {
        // A slow media server still executes the request after the client gave up.
        let late = stub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(StdDuration::from_millis(delay)).await;
            respond(&mut late.script.lock().unwrap(), &service, &method, &body);
        });
        tokio::time::sleep(StdDuration::from_millis(delay + 200)).await;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let (status, reply, upload) =
        respond(&mut stub.script.lock().unwrap(), &service, &method, &body);
    if let Some(key) = upload {
        // The egress uploads its finished file to the private staging key.
        use cf_object_storage::ObjectStore;
        stub.store.put(&key, ogg(2048), "audio/ogg").await.unwrap();
    }
    (status, Json(reply)).into_response()
}

/// (status, body, staging key the egress uploads).
fn respond(
    s: &mut Script,
    service: &str,
    method: &str,
    body: &Value,
) -> (StatusCode, Value, Option<String>) {
    let room = body["room_name"]
        .as_str()
        .or(body["room"].as_str())
        .unwrap_or_default()
        .to_string();
    match (service, method) {
        ("livekit.RoomService", "ListParticipants") => {
            let participants: Vec<Value> = s
                .participants
                .get(&room)
                .into_iter()
                .flatten()
                .map(|(identity, sid)| {
                    json!({ "identity": identity, "state": "ACTIVE",
                            "tracks": [{ "sid": sid, "type": "AUDIO", "source": "MICROPHONE" }] })
                })
                .collect();
            (
                StatusCode::OK,
                json!({ "participants": participants }),
                None,
            )
        }
        ("livekit.Egress", "StartTrackEgress" | "StartRoomCompositeEgress") => {
            let filepath = body["file"]["filepath"]
                .as_str()
                .or(body["file_outputs"][0]["filepath"].as_str())
                .unwrap()
                .to_string();
            let item = json!({
                "egress_id": format!("EG_{}", s.egresses.len()), "room_name": room,
                "status": "EGRESS_ACTIVE", "request": body,
                "file_results": [{ "filename": filepath, "duration": "0" }],
            });
            s.egresses.push(item.clone());
            (StatusCode::OK, item, None)
        }
        ("livekit.Egress", "ListEgress") => {
            let items: Vec<Value> = s
                .egresses
                .iter()
                .filter(|e| e["room_name"] == room.as_str())
                .cloned()
                .collect();
            (StatusCode::OK, json!({ "items": items }), None)
        }
        ("livekit.Egress", "StopEgress") => {
            if s.stop_unavailable {
                return (StatusCode::SERVICE_UNAVAILABLE, Value::Null, None);
            }
            let fail = s.fail_on_stop;
            let id = body["egress_id"].as_str().unwrap_or_default();
            let Some(item) = s.egresses.iter_mut().find(|e| e["egress_id"] == id) else {
                return (StatusCode::NOT_FOUND, json!({ "code": "not_found" }), None);
            };
            if item["status"] != "EGRESS_ACTIVE" {
                return (
                    StatusCode::PRECONDITION_FAILED,
                    json!({ "code": "failed_precondition" }),
                    None,
                );
            }
            if fail {
                item["status"] = json!("EGRESS_FAILED");
                return (StatusCode::OK, item.clone(), None);
            }
            item["status"] = json!("EGRESS_COMPLETE");
            item["file_results"][0]["duration"] = json!("1500000000");
            let key = item["file_results"][0]["filename"]
                .as_str()
                .unwrap()
                .to_string();
            (StatusCode::OK, item.clone(), Some(key))
        }
        _ => (StatusCode::NOT_FOUND, Value::Null, None),
    }
}

impl Stub {
    async fn serve(store: Arc<MemoryObjectStore>) -> (Self, String) {
        let stub = Stub {
            script: Arc::default(),
            store,
        };
        let app = Router::new()
            .route("/twirp/:service/:method", post(twirp))
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (stub, format!("127.0.0.1:{}", addr.port()))
    }

    fn set(&self, f: impl FnOnce(&mut Script)) {
        f(&mut self.script.lock().unwrap());
    }

    fn calls(&self, method: &str) -> usize {
        self.script
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|m| *m == method)
            .count()
    }

    fn egresses(&self) -> Vec<Value> {
        self.script.lock().unwrap().egresses.clone()
    }
}

fn adapter(addr: &str) -> Arc<LiveKitAdapter> {
    let pairs = [
        ("CALLING_LIVEKIT_URL", format!("ws://{addr}")),
        ("CALLING_LIVEKIT_API_URL", format!("http://{addr}")),
        ("CALLING_LIVEKIT_API_KEY", "egress-test".to_string()),
        (
            "CALLING_LIVEKIT_API_SECRET",
            "egress-test-secret-0123456789abcdefghijkl".to_string(),
        ),
        (
            "CALLING_MEDIA_GATEWAY_URL",
            "ws://127.0.0.1:1/v1/calling/media".to_string(),
        ),
        (
            "CALLING_MEDIA_EDGE_SIGNING_KEY",
            "egress-test-edge-key-0123456789abcdefghij".to_string(),
        ),
        ("CALLING_LIVEKIT_API_TIMEOUT_MS", "400".to_string()),
    ];
    let settings = LiveKitSettings::from_lookup(&|k: &str| {
        pairs
            .iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
    })
    .unwrap();
    Arc::new(LiveKitAdapter::new(settings).unwrap())
}

struct Desk {
    principal: Principal,
    phone: Device,
}

/// One enterprise world whose application capture is the LiveKit Egress adapter.
struct World {
    e: EnterpriseWorld,
    store: Arc<MemoryObjectStore>,
    stub: Stub,
    capture: Arc<LiveKitEgressCapture>,
    manager: crate::common::IssuedCredential,
}

impl World {
    async fn new() -> Option<Self> {
        let store = Arc::new(MemoryObjectStore::new());
        let (stub, addr) = Stub::serve(store.clone()).await;
        let capture = Arc::new(LiveKitEgressCapture::new(
            adapter(&addr),
            store.clone(),
            &CaptureSettings {
                egress: true,
                staging_prefix: "capture-staging".into(),
            },
        ));
        let (s, c) = (store.clone(), capture.clone());
        let e = EnterpriseWorld::configured(move |runtime| {
            runtime.enterprise_flags = EnterpriseFlags::ALL_ON;
            runtime.artifacts = s;
            runtime.capture = c;
        })
        .await?;
        e.h()
            .state
            .enterprise
            .operator_set_capabilities(
                e.a.w.scope(),
                vec![
                    AdminCapability::ManageRouting,
                    AdminCapability::ManageVoicemail,
                    AdminCapability::ManageRecording,
                ],
            )
            .await
            .unwrap();
        let manager = credential(
            e.h(),
            &e.a.w.consumer,
            e.a.w.kind,
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_VOICEMAIL_READ,
                names::ENTERPRISE_VOICEMAIL_MANAGE,
                names::ENTERPRISE_RECORDING_READ,
                names::ENTERPRISE_RECORDING_MANAGE,
            ],
        )
        .await;
        Some(Self {
            e,
            store,
            stub,
            capture,
            manager,
        })
    }

    async fn manage(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.e
            .send(self.manager.secret.expose_once(), method, path, body)
            .await
    }

    async fn desk(&self, name: &str) -> Desk {
        let principal = self.e.member(&format!("{name}-{}", tag())).await;
        let phone = self.e.a.device(&principal, &format!("{name}-desk")).await;
        Desk { principal, phone }
    }

    async fn tick(&self) {
        self.e.h().state.app.expire_due(100).await.unwrap();
    }

    async fn one(&self, sql: String) -> String {
        strings(self.e.h().conn(), &sql)
            .await
            .pop()
            .unwrap_or_default()
    }

    /// The live media identities of `who` in `call`.
    async fn identities(&self, call: Uuid, who: &Desk) -> Vec<String> {
        strings(
            self.e.h().conn(),
            &format!(
                "SELECT s.media_identity FROM call_sessions s JOIN call_participants p \
                   ON p.id = s.participant_id WHERE s.call_id = '{call}' \
                    AND p.principal_id = '{}' AND s.ended_at IS NULL",
                who.principal.id()
            ),
        )
        .await
    }

    fn staging(&self) -> Vec<String> {
        self.store
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("capture-staging/"))
            .collect()
    }

    fn artifacts(&self) -> Vec<String> {
        self.store
            .keys()
            .into_iter()
            .filter(|k| !k.starts_with("capture-staging/"))
            .collect()
    }

    // ---- voicemail ----------------------------------------------------------------

    /// `caller` dials `number`, a member whose unanswered calls go to their mailbox; the
    /// stub knows the caller's (and the member's) published microphone. Returns
    /// (call, message) once the voicemail capture was attempted.
    async fn voicemail(&self, caller: &Desk, number: &str) -> (Uuid, Uuid) {
        let e = &self.e;
        let member = self.desk("member").await;
        let r = self
            .manage(
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(
                    json!({ "owner": { "type": "PRINCIPAL", "id": member.principal.id() },
                             "displayLabel": "Capture mailbox",
                             "maxMessageSeconds": 120, "retentionDays": 30 }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let mailbox = id_of(&r.json, "mailboxId");
        let r = e
            .admin(
                Method::POST,
                "/v1/enterprise/forwarding-rules",
                Some(
                    json!({ "principalId": member.principal.id(), "condition": "NO_ANSWER",
                             "destination": { "type": "VOICEMAIL", "id": mailbox } }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        e.extension(number, "ENTERPRISE_MEMBER", member.principal.id())
            .await;
        let r = e
            .call_extension(
                "VOICE",
                &caller.principal,
                &caller.phone,
                number,
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        // What the media server would report: the caller and the ringing member both
        // publish a microphone.
        let room = common::app::AppWorld::room(call);
        let mut published: Vec<(String, String)> = self
            .identities(call, caller)
            .await
            .into_iter()
            .map(|i| (i, "TR_CALLER".to_string()))
            .collect();
        published.extend(
            self.identities(call, &member)
                .await
                .into_iter()
                .map(|i| (i, "TR_MEMBER".to_string())),
        );
        self.stub.set(|s| {
            s.participants.insert(room, published);
        });
        e.h().clock.advance(chrono::Duration::seconds(46));
        self.tick().await;
        let message: Uuid = self
            .one(format!(
                "SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"
            ))
            .await
            .parse()
            .expect("a voicemail message");
        (call, message)
    }

    async fn message(&self, message: Uuid) -> String {
        self.one(format!(
            "SELECT state || ':' || COALESCE(failure_code, '') || ':' || COALESCE(content_type, '') \
               FROM voicemail_messages WHERE id = '{message}'"
        ))
        .await
    }

    // ---- recording ----------------------------------------------------------------

    /// A CONNECTED enterprise Call between two application participants.
    async fn connected(&self, caller: &Desk, callee: &Desk, number: &str) -> Uuid {
        callee_extension(self, callee, number).await;
        self.connected_inner(caller, callee, number).await
    }

    async fn connected_inner(&self, caller: &Desk, callee: &Desk, number: &str) -> Uuid {
        let e = &self.e;
        let r = e
            .call_extension(
                "VOICE",
                &caller.principal,
                &caller.phone,
                number,
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let inv =
            e.a.invitation_for(call, callee.phone.endpoint)
                .await
                .expect("callee rings");
        let (_, v) = e.a.state(call).await;
        let r = e.a.accept(call, inv, &callee.phone, v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        for d in [&caller.phone, &callee.phone] {
            let s = e.a.session_on(call, d.endpoint).await.unwrap();
            e.a.connect(call, s).await;
        }
        assert_eq!(e.a.state(call).await.0, CallState::Connected);
        call
    }

    /// As [`World::connected`] for an extension that already exists.
    async fn call_connected(&self, caller: &Desk, callee: &Desk, number: &str) -> Uuid {
        self.connected_inner(caller, callee, number).await
    }

    async fn policy(&self) {
        let r = self
            .manage(
                Method::POST,
                "/v1/enterprise/recording-policies",
                Some(
                    json!({ "name": format!("Recording {}", tag()), "mode": "MANUAL_ALLOWED",
                             "noticeRequired": false, "consentParticipants": "NONE",
                             "jurisdictionRef": "NG-NDPA-2023/v2", "retentionDays": 30,
                             "maxRecordingSeconds": 600, "isDefault": true }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }

    async fn start_recording(&self, call: Uuid) -> TestResponse {
        self.manage(
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({})),
        )
        .await
    }

    async fn recording(&self, id: Uuid) -> String {
        self.one(format!(
            "SELECT state || ':' || COALESCE(stop_reason, '') || ':' || COALESCE(failure_code, '') \
                    || ':' || COALESCE(content_type, '') FROM call_recordings WHERE id = '{id}'"
        ))
        .await
    }
}

async fn callee_extension(w: &World, callee: &Desk, number: &str) {
    w.e.extension(number, "ENTERPRISE_MEMBER", callee.principal.id())
        .await;
}

fn recording_id(r: &TestResponse) -> Uuid {
    id_of(&r.json["recording"], "recordingId")
}

/// The voicemail capture records ONLY the caller's microphone track, is started once,
/// stops when the caller hangs up, and is stored as a protected Ogg artifact; the staging
/// object is released and a repeated stop / start changes nothing.
#[tokio::test]
async fn voicemail_captures_only_the_caller_and_is_collected_once() {
    let Some(w) = World::new().await else {
        return;
    };
    let caller = w.desk("caller").await;
    let (call, message) = w.voicemail(&caller, "7100").await;
    assert_eq!(w.message(message).await, "RECORDING::");
    let egresses = w.stub.egresses();
    assert_eq!(egresses.len(), 1, "{egresses:?}");
    let request = &egresses[0]["request"];
    // Exactly the caller's microphone track: no room mix, no other participant.
    assert_eq!(request["track_id"], "TR_CALLER");
    assert!(request.get("audio_only").is_none() && request.get("file_outputs").is_none());
    let filepath = request["file"]["filepath"].as_str().unwrap();
    assert_eq!(
        filepath,
        format!("capture-staging/{}.ogg", message.simple())
    );
    assert_eq!(request["file"]["disable_manifest"], true);
    // A repeated start of the same capture is the same capture.
    let caller_ids = w.identities(call, &caller).await;
    w.capture
        .start(CaptureRequest {
            capture_id: message,
            call_id: call,
            purpose: CapturePurpose::Voicemail,
            max_duration_seconds: 120,
            sources: caller_ids,
        })
        .await
        .unwrap();
    assert_eq!(w.stub.egresses().len(), 1);
    // Still recording: nothing to collect.
    w.tick().await;
    assert_eq!(w.message(message).await, "RECORDING::");
    assert_eq!(w.stub.calls("StopEgress"), 0);
    // The caller hangs up: the capture is stopped, collected, stored, released.
    let (_, v) = w.e.a.state(call).await;
    let r = w.e.a.command(call, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    w.tick().await;
    assert_eq!(w.message(message).await, "READY::audio/ogg");
    assert_eq!(w.stub.calls("StopEgress"), 1);
    assert!(w.staging().is_empty(), "{:?}", w.staging());
    let artifacts = w.artifacts();
    assert_eq!(artifacts.len(), 1);
    assert!(
        artifacts[0].starts_with("voicemail/") && artifacts[0].ends_with(".ogg"),
        "{artifacts:?}"
    );
    assert_eq!(w.store.get_raw(&artifacts[0]).unwrap(), ogg(2048));
    assert_eq!(
        w.one(format!(
            "SELECT duration_ms::text || ':' || sha256 FROM voicemail_messages WHERE id = '{message}'"
        ))
        .await,
        format!("1500:{}", cf_object_storage::sha256_hex(&ogg(2048)))
    );
    // Further passes and a repeated stop change nothing.
    w.tick().await;
    w.capture.stop(call, message).await.unwrap();
    assert_eq!(w.stub.calls("StopEgress"), 1);
    assert_eq!(w.artifacts().len(), 1);
}

/// The maximum message length stops the capture exactly once, the message becomes READY
/// and the ringing Call ends NO_ANSWER.
#[tokio::test]
async fn voicemail_maximum_length_stops_the_capture_once() {
    let Some(w) = World::new().await else {
        return;
    };
    let caller = w.desk("caller").await;
    let (call, message) = w.voicemail(&caller, "7110").await;
    w.e.h().clock.advance(chrono::Duration::seconds(60));
    w.tick().await;
    assert_eq!(w.stub.calls("StopEgress"), 0, "not yet at the maximum");
    w.e.h().clock.advance(chrono::Duration::seconds(61));
    w.tick().await;
    w.tick().await;
    assert_eq!(w.stub.calls("StopEgress"), 1);
    assert_eq!(w.message(message).await, "READY::audio/ogg");
    assert_eq!(w.e.a.state(call).await.0, CallState::NoAnswer);
}

/// Egress unavailable: the voicemail is FAILED, the Call ends cleanly (NO_ANSWER) and an
/// incident is raised; a recording is FAILED while its Call stays CONNECTED.
#[tokio::test]
async fn egress_unavailable_fails_safely_without_corrupting_the_call() {
    let Some(w) = World::new().await else {
        return;
    };
    w.stub.set(|s| s.unavailable = true);
    let caller = w.desk("caller").await;
    let (call, message) = w.voicemail(&caller, "7120").await;
    assert_eq!(w.message(message).await, "FAILED:CAPTURE_UNAVAILABLE:");
    assert_eq!(w.e.a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(
        w.one(format!(
            "SELECT category FROM calling_incidents WHERE resource_id = '{message}'"
        ))
        .await,
        "VOICEMAIL_FINALIZATION_FAILURE"
    );
    w.policy().await;
    let callee = w.desk("callee").await;
    let call = w.connected(&caller, &callee, "7121").await;
    let r = w.start_recording(call).await;
    assert!(r.status.is_success(), "{}", r.text);
    let recording = recording_id(&r);
    assert_eq!(
        w.recording(recording).await,
        "FAILED:CAPTURE_FAILED:CAPTURE_UNAVAILABLE:"
    );
    assert_eq!(w.e.a.state(call).await.0, CallState::Connected);
    // The media server returns; the passes never produce an artifact for it.
    w.stub.set(|s| s.unavailable = false);
    w.tick().await;
    assert!(w.artifacts().is_empty());
}

/// A start that times out is FAILED and never becomes READY, even if the media server
/// executed it late: the orphan capture is stopped and its media released.
#[tokio::test]
async fn start_timeout_never_produces_a_ready_artifact() {
    let Some(w) = World::new().await else {
        return;
    };
    w.policy().await;
    let (caller, callee) = (w.desk("caller").await, w.desk("callee").await);
    let call = w.connected(&caller, &callee, "7150").await;
    w.stub.set(|s| s.start_delay_ms = 1200);
    let r = w.start_recording(call).await;
    assert!(r.status.is_success(), "{}", r.text);
    let recording = recording_id(&r);
    assert_eq!(
        w.recording(recording).await,
        "FAILED:CAPTURE_FAILED:CAPTURE_START_TIMEOUT:"
    );
    // The late egress appears; the collector stops it and releases its media.
    tokio::time::sleep(StdDuration::from_millis(1500)).await;
    assert_eq!(w.stub.egresses().len(), 1);
    w.tick().await;
    w.tick().await;
    assert_eq!(w.stub.egresses()[0]["status"], "EGRESS_COMPLETE");
    assert!(w.staging().is_empty(), "{:?}", w.staging());
    assert!(w.artifacts().is_empty());
    assert!(w.recording(recording).await.starts_with("FAILED:"));
    assert_eq!(w.e.a.state(call).await.0, CallState::Connected);
}

/// A stop the media server does not answer is retried by the collector until the capture
/// finalizes — once.
#[tokio::test]
async fn stop_timeout_is_reconciled_by_the_collector() {
    let Some(w) = World::new().await else {
        return;
    };
    w.policy().await;
    let (caller, callee) = (w.desk("caller").await, w.desk("callee").await);
    let call = w.connected(&caller, &callee, "7160").await;
    let r = w.start_recording(call).await;
    let recording = recording_id(&r);
    assert_eq!(w.recording(recording).await, "RECORDING:::");
    let request = &w.stub.egresses()[0]["request"];
    // The mixed call audio of the Call's own resource, audio only.
    assert_eq!(request["audio_only"], true);
    assert_eq!(request["room_name"], common::app::AppWorld::room(call));
    assert_eq!(request["file_outputs"][0]["file_type"], "OGG");
    w.stub.set(|s| s.stop_unavailable = true);
    let r = w
        .manage(
            Method::POST,
            &format!("/v1/calls/{call}/recordings/{recording}/stop"),
            Some(json!({})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    w.tick().await;
    assert_eq!(w.recording(recording).await, "STOPPING:EXPLICIT::");
    w.stub.set(|s| s.stop_unavailable = false);
    w.tick().await;
    assert_eq!(w.recording(recording).await, "READY:EXPLICIT::audio/ogg");
    w.tick().await;
    assert_eq!(w.artifacts().len(), 1);
    assert!(w.staging().is_empty());
    assert_eq!(
        w.stub
            .egresses()
            .iter()
            .filter(|e| e["status"] == "EGRESS_COMPLETE")
            .count(),
        1
    );
}

/// Protected storage unavailable: the finished media stays staged, nothing is READY; once
/// storage returns the artifact is stored exactly once.
#[tokio::test]
async fn storage_outage_keeps_the_artifact_recoverable() {
    let Some(w) = World::new().await else {
        return;
    };
    let caller = w.desk("caller").await;
    let (call, message) = w.voicemail(&caller, "7130").await;
    w.capture.stop(call, message).await.unwrap();
    w.store.set_available(false);
    w.tick().await;
    w.tick().await;
    assert!(w.message(message).await.starts_with("RECORDING:"));
    w.store.set_available(true);
    w.tick().await;
    assert_eq!(w.message(message).await, "READY::audio/ogg");
    assert_eq!(w.artifacts().len(), 1);
    assert!(w.staging().is_empty());
}

/// A capture that ends without media is FAILED (bounded code) — never READY.
#[tokio::test]
async fn failed_egress_is_failed_never_ready() {
    let Some(w) = World::new().await else {
        return;
    };
    w.stub.set(|s| s.fail_on_stop = true);
    let caller = w.desk("caller").await;
    let (call, message) = w.voicemail(&caller, "7140").await;
    let (_, v) = w.e.a.state(call).await;
    w.e.a.command(call, "cancel", v).await;
    w.tick().await;
    assert_eq!(w.message(message).await, "FAILED:CAPTURE_FAILED:");
    assert!(w.artifacts().is_empty());
}

/// Duplicate starts and a stop racing the Call end: one capture, one stop effect, one
/// artifact per recording.
#[tokio::test]
async fn duplicate_start_and_stop_racing_call_end_yield_one_artifact() {
    let Some(w) = World::new().await else {
        return;
    };
    w.policy().await;
    let (caller, callee) = (w.desk("caller").await, w.desk("callee").await);
    callee_extension(&w, &callee, "7170").await;
    for round in 0..3 {
        let call = w.call_connected(&caller, &callee, "7170").await;
        let (a, b) = tokio::join!(w.start_recording(call), w.start_recording(call));
        assert!(
            a.status.is_success() && b.status.is_success(),
            "{} {}",
            a.text,
            b.text
        );
        let recording = recording_id(&a);
        assert_eq!(recording, recording_id(&b), "round {round}: one recording");
        let room = common::app::AppWorld::room(call);
        let of_call = |w: &World| {
            w.stub
                .egresses()
                .into_iter()
                .filter(|e| e["room_name"] == room.as_str())
                .count()
        };
        assert_eq!(of_call(&w), 1, "round {round}: one capture");
        let (_, v) = w.e.a.state(call).await;
        let stop_path = format!("/v1/calls/{call}/recordings/{recording}/stop");
        let (stop, end) = tokio::join!(
            w.manage(Method::POST, &stop_path, Some(json!({}))),
            w.e.a.command(call, "end", v)
        );
        assert!(stop.status.is_success(), "round {round}: {}", stop.text);
        assert!(end.status.is_success(), "round {round}: {}", end.text);
        let (x, y) = tokio::join!(w.tick(), w.tick());
        let _ = (x, y);
        w.tick().await;
        assert!(
            w.recording(recording).await.starts_with("READY:"),
            "round {round}: {}",
            w.recording(recording).await
        );
        assert_eq!(of_call(&w), 1);
    }
    assert_eq!(w.artifacts().len(), 3, "one artifact per recording");
    assert!(w.staging().is_empty());
}
